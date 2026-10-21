//! The core side of the Players page: the roster and the online rows.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

use crate::protocol::pack::PackStatus;
use crate::resource_pack::PackState;
use crate::world::Shared;
use crate::world::messages::{CoordinatorMessage, Roster, RosterEntry};
use hyper::StatusCode;
use tuff_storage::admin::NameAndId;
use tuff_storage::admin::{format_uuid, parse_uuid_text};
use tuff_web_ui::host::{HostError, HostFuture};
use tuff_web_ui::players::{PlayerHost, SpawnPoint, valid_name};

/// The online player list, the known profiles and the stored lists.
pub async fn roster(shared: &Shared) -> Option<Roster> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::Roster { reply })
        .await
        .ok()?;
    rx.await.ok()
}

/// Finds the name of a player given by uuid among everyone the roster
/// knows; a name is returned as given.
pub fn resolve_name(roster: &Roster, player: &str) -> Option<String> {
    let Some(uuid) = parse_uuid_text(player) else {
        return Some(player.to_owned());
    };
    let lists = &roster.lists;
    roster
        .online
        .iter()
        .map(|p| (p.uuid, &p.name))
        .chain(roster.known.iter().map(|p| (p.uuid, &p.name)))
        .chain(lists.ops.iter().map(|o| (o.user.uuid, &o.user.name)))
        .chain(lists.whitelist.iter().map(|p| (p.uuid, &p.name)))
        .chain(lists.bans.iter().map(|b| (b.key.uuid, &b.key.name)))
        .find(|(id, name)| *id == uuid && valid_name(name))
        .map(|(_, name)| name.clone())
}

/// The status a player's packs add up to: the first failure, else
/// `loaded` once every pack is, else `pending`; `None` with no packs.
pub fn pack_summary(states: Option<&HashMap<u128, PackState>>) -> Option<&'static str> {
    let states = states.filter(|s| !s.is_empty())?;
    let mut ordered: Vec<(&u128, &PackState)> = states.iter().collect();
    ordered.sort_by_key(|(id, _)| **id);
    if let Some(failed) = ordered
        .iter()
        .find_map(|(_, s)| s.status.filter(|s| s.is_failure()))
    {
        return Some(match failed {
            PackStatus::Declined => "declined",
            PackStatus::InvalidUrl => "invalid_url",
            PackStatus::FailedReload => "failed_reload",
            PackStatus::Discarded => "discarded",
            _ => "failed_download",
        });
    }
    Some(if ordered.iter().all(|(_, s)| s.loaded) {
        "loaded"
    } else {
        "pending"
    })
}

fn round(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

/// One online player's row, as the API lists it and the topic sends it
/// (without the address).
pub fn row_json(
    shared: &Shared,
    player: &RosterEntry,
    op_level: u8,
    pack: Option<&'static str>,
) -> Value {
    json!({
        "uuid": format_uuid(player.uuid),
        "name": player.name,
        "ping": player.latency,
        "gameMode": player.game_mode.name(),
        "dimension": player.dimension.map(|d| shared.dimension(d).key.clone()),
        "position": player.position.map(|p| [round(p[0]), round(p[1]), round(p[2])]),
        "opLevel": op_level,
        "brand": player.brand.as_deref().map(printable),
        "pack": pack,
    })
}

/// A client-supplied string with control characters dropped.
fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

fn op_level(roster: &Roster, uuid: u128) -> u8 {
    roster.lists.op(uuid).map_or(0, |op| op.level.id() as u8)
}

async fn online_rows(shared: &Shared, roster: &Roster) -> BTreeMap<String, Value> {
    let packs = shared.resource_packs.player_states().await;
    roster
        .online
        .iter()
        .map(|p| {
            let row = row_json(
                shared,
                p,
                op_level(roster, p.uuid),
                pack_summary(packs.get(&p.uuid)),
            );
            (format_uuid(p.uuid), row)
        })
        .collect()
}

fn user_json(user: &NameAndId) -> Value {
    json!({ "uuid": format_uuid(user.uuid), "name": user.name })
}

/// The page's whole state; `moderate` adds addresses and IP bans.
pub async fn state(shared: &Shared, moderate: bool) -> Option<Value> {
    let roster = roster(shared).await?;
    let mut rows = online_rows(shared, &roster).await;
    let mut online = Vec::with_capacity(roster.online.len());
    for player in &roster.online {
        let Some(mut row) = rows.remove(&format_uuid(player.uuid)) else {
            continue;
        };
        if moderate {
            row["address"] = json!(player.address.to_string());
        }
        online.push(row);
    }
    let lists = &roster.lists;
    let mut offline: BTreeMap<u128, NameAndId> = BTreeMap::new();
    let users = roster
        .known
        .iter()
        .chain(lists.ops.iter().map(|o| &o.user))
        .chain(lists.whitelist.iter())
        .chain(lists.bans.iter().map(|b| &b.key));
    for user in users {
        if !roster.online.iter().any(|p| p.uuid == user.uuid) {
            offline.entry(user.uuid).or_insert_with(|| user.clone());
        }
    }
    let mut offline: Vec<NameAndId> = offline.into_values().collect();
    offline.sort_by_key(|a| a.name.to_lowercase());
    let offline: Vec<Value> = offline
        .iter()
        .map(|user| {
            json!({
                "uuid": format_uuid(user.uuid),
                "name": user.name,
                "opLevel": op_level(&roster, user.uuid),
                "whitelisted": lists.whitelist.iter().any(|w| w.uuid == user.uuid),
                "banned": lists.bans.iter().any(|b| b.key.uuid == user.uuid),
            })
        })
        .collect();
    let ops: Vec<Value> = lists
        .ops
        .iter()
        .map(|op| {
            json!({
                "uuid": format_uuid(op.user.uuid),
                "name": op.user.name,
                "level": op.level.id(),
                "bypassesPlayerLimit": op.bypasses_player_limit,
            })
        })
        .collect();
    let bans: Vec<Value> = lists
        .bans
        .iter()
        .map(|ban| {
            json!({
                "uuid": format_uuid(ban.key.uuid),
                "name": ban.key.name,
                "source": ban.source,
                "created": ban.created,
                "expires": ban.expires,
                "reason": ban.reason,
            })
        })
        .collect();
    let ip_bans = moderate.then(|| {
        lists
            .ip_bans
            .iter()
            .map(|ban| {
                json!({
                    "ip": ban.key,
                    "source": ban.source,
                    "created": ban.created,
                    "expires": ban.expires,
                    "reason": ban.reason,
                })
            })
            .collect::<Vec<Value>>()
    });
    Some(json!({
        "online": online,
        "offline": offline,
        "ops": ops,
        "whitelist": {
            "enabled": roster.whitelist,
            "enforced": roster.enforce_whitelist,
            "entries": lists.whitelist.iter().map(user_json).collect::<Vec<_>>(),
        },
        "bans": bans,
        "ipBans": ip_bans,
    }))
}

fn not_running() -> HostError {
    HostError::new(StatusCode::SERVICE_UNAVAILABLE, "the world is not running")
}

impl PlayerHost for Shared {
    fn state(&self, moderate: bool) -> HostFuture<'_, Option<Value>> {
        Box::pin(state(self, moderate))
    }

    fn rows(&self) -> HostFuture<'_, Option<BTreeMap<String, Value>>> {
        Box::pin(async move {
            let roster = roster(self).await?;
            Some(online_rows(self, &roster).await)
        })
    }

    fn resolve<'a>(&'a self, player: &'a str) -> HostFuture<'a, Result<String, HostError>> {
        Box::pin(async move {
            let roster = roster(self).await.ok_or_else(not_running)?;
            resolve_name(&roster, player).ok_or_else(|| {
                HostError::new(StatusCode::NOT_FOUND, "no known player has that uuid")
            })
        })
    }

    fn spawn_point(&self) -> SpawnPoint {
        let spawn = self.spawn();
        SpawnPoint {
            dimension: self.dimension(self.spawn_dimension()).key.clone(),
            position: [
                f64::from(spawn.pos.x) + 0.5,
                f64::from(spawn.pos.y),
                f64::from(spawn.pos.z) + 0.5,
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn uuids_resolve_through_every_list() {
        let mut roster = Roster::default();
        roster.lists.bans.push(tuff_storage::admin::BanEntry::new(
            NameAndId {
                uuid: 0xab,
                name: "Banned".into(),
            },
            "Server".into(),
            None,
        ));
        assert_eq!(
            resolve_name(&roster, &format_uuid(0xab)).as_deref(),
            Some("Banned")
        );
        assert_eq!(resolve_name(&roster, &format_uuid(0xcd)), None);
        assert_eq!(resolve_name(&roster, "Steve").as_deref(), Some("Steve"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn pack_states_sum_up() {
        let state = |status, loaded| PackState {
            hash: String::new(),
            status,
            loaded,
        };
        assert_eq!(pack_summary(None), None);
        assert_eq!(pack_summary(Some(&HashMap::new())), None);
        let loaded = HashMap::from([(1, state(Some(PackStatus::Loaded), true))]);
        assert_eq!(pack_summary(Some(&loaded)), Some("loaded"));
        let pending = HashMap::from([
            (1, state(Some(PackStatus::Loaded), true)),
            (2, state(Some(PackStatus::Accepted), false)),
        ]);
        assert_eq!(pack_summary(Some(&pending)), Some("pending"));
        let declined = HashMap::from([
            (1, state(Some(PackStatus::Loaded), true)),
            (2, state(Some(PackStatus::Declined), false)),
        ]);
        assert_eq!(pack_summary(Some(&declined)), Some("declined"));
    }
}
