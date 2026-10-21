//! The Players page's API and the `players` event topic:
//!
//! - `GET /players` (`players.read`): online players, known offline
//!   players, and the operator, whitelist and ban lists. Addresses and IP
//!   bans only with `players.moderate`.
//! - `POST /players/actions` (`players.moderate`) with one action, e.g.
//!   `{"action":"ban","player":"Alex","reason":"griefing"}`. Each action
//!   runs the matching command at the panel user's own level, so the
//!   command's own permission, feedback and operator broadcast apply.
//!
//! The topic bridge diffs the online rows once a second while the panel
//! is enabled and publishes `join`, `update` and `leave` events.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, Weak};
use std::time::Duration;

use hyper::{Method, StatusCode};
use serde_json::{Value, json};

use super::api::{json_body, method_not_allowed, require};
use super::audit::Entry;
use super::auth::{AuthContext, Scope};
use super::events::{PLAYERS, PanelEvents};
use super::guard::{self, error};
use super::host::{HostError, HostFuture, PanelHost};
use tuff_http::{Request, Response};
use tuff_storage::admin::parse_uuid_text;

/// Longest kick or ban reason and private message accepted.
pub const MAX_TEXT: usize = 256;
/// How often the topic bridge compares the online players.
pub const BRIDGE_PERIOD: Duration = Duration::from_secs(1);

/// Whether `name` is a player name the panel passes to a command.
const GAME_MODES: [&str; 4] = ["survival", "creative", "adventure", "spectator"];

/// What the Players page needs from the server.
pub trait PlayerHost: Send + Sync {
    /// The page's state; `moderate` adds addresses and IP bans. `None`
    /// while the world is down.
    fn state(&self, moderate: bool) -> HostFuture<'_, Option<Value>>;
    /// The online players' rows by uuid, as the topic sends them.
    fn rows(&self) -> HostFuture<'_, Option<BTreeMap<String, Value>>>;
    /// The name of a player given by uuid or name.
    fn resolve<'a>(&'a self, player: &'a str) -> HostFuture<'a, Result<String, HostError>>;
    fn spawn_point(&self) -> SpawnPoint;
}

pub fn valid_name(name: &str) -> bool {
    (1..=16).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Whether `text` is a single line of at most [`MAX_TEXT`] characters
/// with no control characters.
pub fn valid_text(text: &str) -> bool {
    !text.trim().is_empty()
        && text.chars().count() <= MAX_TEXT
        && !text.chars().any(char::is_control)
}

/// A moderation action as the panel sends it, before its player is
/// resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Kick {
        player: String,
        reason: Option<String>,
    },
    Ban {
        player: String,
        reason: Option<String>,
    },
    BanIp {
        target: String,
        reason: Option<String>,
    },
    Pardon {
        player: String,
    },
    PardonIp {
        ip: IpAddr,
    },
    WhitelistAdd {
        player: String,
    },
    WhitelistRemove {
        player: String,
    },
    WhitelistOn,
    WhitelistOff,
    Op {
        player: String,
    },
    Deop {
        player: String,
    },
    GameMode {
        player: String,
        mode: &'static str,
    },
    Spawn {
        player: String,
    },
    Message {
        player: String,
        text: String,
    },
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Kick { .. } => "kick",
            Self::Ban { .. } => "ban",
            Self::BanIp { .. } => "ban-ip",
            Self::Pardon { .. } => "pardon",
            Self::PardonIp { .. } => "pardon-ip",
            Self::WhitelistAdd { .. } => "whitelist-add",
            Self::WhitelistRemove { .. } => "whitelist-remove",
            Self::WhitelistOn => "whitelist-on",
            Self::WhitelistOff => "whitelist-off",
            Self::Op { .. } => "op",
            Self::Deop { .. } => "deop",
            Self::GameMode { .. } => "gamemode",
            Self::Spawn { .. } => "spawn",
            Self::Message { .. } => "message",
        }
    }

    /// The player the action names, if any.
    pub fn player(&self) -> Option<&str> {
        match self {
            Self::Kick { player, .. }
            | Self::Ban { player, .. }
            | Self::Pardon { player }
            | Self::WhitelistAdd { player }
            | Self::WhitelistRemove { player }
            | Self::Op { player }
            | Self::Deop { player }
            | Self::GameMode { player, .. }
            | Self::Spawn { player }
            | Self::Message { player, .. } => Some(player),
            Self::BanIp { .. } | Self::PardonIp { .. } | Self::WhitelistOn | Self::WhitelistOff => {
                None
            }
        }
    }

    fn with_player(mut self, name: String) -> Self {
        match &mut self {
            Self::Kick { player, .. }
            | Self::Ban { player, .. }
            | Self::Pardon { player }
            | Self::WhitelistAdd { player }
            | Self::WhitelistRemove { player }
            | Self::Op { player }
            | Self::Deop { player }
            | Self::GameMode { player, .. }
            | Self::Spawn { player }
            | Self::Message { player, .. } => *player = name,
            _ => {}
        }
        self
    }

    /// What the audit entry's detail says, if anything.
    fn detail(&self) -> Option<String> {
        match self {
            Self::Kick { reason, .. } | Self::Ban { reason, .. } | Self::BanIp { reason, .. } => {
                reason.clone()
            }
            Self::GameMode { mode, .. } => Some((*mode).to_owned()),
            _ => None,
        }
    }

    fn target(&self) -> String {
        match self {
            Self::BanIp { target, .. } => target.clone(),
            Self::PardonIp { ip } => ip.to_string(),
            _ => self.player().unwrap_or_default().to_owned(),
        }
    }
}

fn player_field(body: &Value) -> Result<String, &'static str> {
    let player = body
        .get("player")
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or("player is required")?;
    if valid_name(player) || parse_uuid_text(player).is_some() {
        Ok(player.to_owned())
    } else {
        Err("player must be a name of 1 to 16 letters, digits or _, or a uuid")
    }
}

fn reason_field(body: &Value) -> Result<Option<String>, &'static str> {
    match body.get("reason") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) if valid_text(s) => Ok(Some(s.trim().to_owned())),
        Some(_) => Err("reason must be one line of at most 256 characters"),
    }
}

/// Reads an action body.
pub fn parse_action(body: &Value) -> Result<Action, &'static str> {
    let action = body
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(match action {
        "kick" => Action::Kick {
            player: player_field(body)?,
            reason: reason_field(body)?,
        },
        "ban" => Action::Ban {
            player: player_field(body)?,
            reason: reason_field(body)?,
        },
        "ban-ip" => {
            let target = body
                .get("target")
                .and_then(Value::as_str)
                .map(str::trim)
                .ok_or("target is required")?;
            if target.parse::<IpAddr>().is_err() && !valid_name(target) {
                return Err("target must be an IP address or a player name");
            }
            Action::BanIp {
                target: target.to_owned(),
                reason: reason_field(body)?,
            }
        }
        "pardon" => Action::Pardon {
            player: player_field(body)?,
        },
        "pardon-ip" => Action::PardonIp {
            ip: body
                .get("ip")
                .and_then(Value::as_str)
                .and_then(|ip| ip.trim().parse().ok())
                .ok_or("ip must be an IP address")?,
        },
        "whitelist-add" => Action::WhitelistAdd {
            player: player_field(body)?,
        },
        "whitelist-remove" => Action::WhitelistRemove {
            player: player_field(body)?,
        },
        "whitelist-on" => Action::WhitelistOn,
        "whitelist-off" => Action::WhitelistOff,
        "op" => Action::Op {
            player: player_field(body)?,
        },
        "deop" => Action::Deop {
            player: player_field(body)?,
        },
        "gamemode" => Action::GameMode {
            player: player_field(body)?,
            mode: body
                .get("mode")
                .and_then(Value::as_str)
                .and_then(|mode| GAME_MODES.into_iter().find(|m| *m == mode))
                .ok_or("mode is survival, creative, adventure or spectator")?,
        },
        "spawn" => Action::Spawn {
            player: player_field(body)?,
        },
        "message" => Action::Message {
            player: player_field(body)?,
            text: body
                .get("text")
                .and_then(Value::as_str)
                .filter(|t| valid_text(t))
                .map(|t| t.trim().to_owned())
                .ok_or("text must be one line of at most 256 characters")?,
        },
        _ => return Err("unknown action"),
    })
}

/// Where `/spawn`-like teleports go: the world spawn's dimension key and
/// block centre.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnPoint {
    pub dimension: String,
    pub position: [f64; 3],
}

fn with_reason(line: String, reason: &Option<String>) -> String {
    match reason {
        Some(reason) => format!("{line} {reason}"),
        None => line,
    }
}

/// The command line an action runs, its player already a valid name.
pub fn command_line(action: &Action, spawn: &SpawnPoint) -> String {
    match action {
        Action::Kick { player, reason } => with_reason(format!("kick {player}"), reason),
        Action::Ban { player, reason } => with_reason(format!("ban {player}"), reason),
        Action::BanIp { target, reason } => with_reason(format!("ban-ip {target}"), reason),
        Action::Pardon { player } => format!("pardon {player}"),
        Action::PardonIp { ip } => format!("pardon-ip {ip}"),
        Action::WhitelistAdd { player } => format!("whitelist add {player}"),
        Action::WhitelistRemove { player } => format!("whitelist remove {player}"),
        Action::WhitelistOn => "whitelist on".to_owned(),
        Action::WhitelistOff => "whitelist off".to_owned(),
        Action::Op { player } => format!("op {player}"),
        Action::Deop { player } => format!("deop {player}"),
        Action::GameMode { player, mode } => format!("gamemode {mode} {player}"),
        Action::Spawn { player } => {
            let [x, y, z] = spawn.position;
            format!("execute in {} run tp {player} {x} {y} {z}", spawn.dimension)
        }
        Action::Message { player, text } => format!("tell {player} {text}"),
    }
}

async fn state(host: &dyn PanelHost, ctx: &AuthContext) -> Option<Value> {
    let moderate = ctx.allows(Scope::PlayersModerate);
    let mut state = host.players().state(moderate).await?;
    state["moderate"] = json!(moderate);
    state["level"] = json!(ctx.level.id());
    Some(state)
}

pub async fn handle(host: Arc<dyn PanelHost>, req: Request) -> Response {
    let ctx = match require(&req, Some(Scope::PlayersRead)) {
        Ok(ctx) => ctx,
        Err(resp) => return resp,
    };
    match (req.rest.trim_end_matches('/'), &req.method) {
        ("", &Method::GET) => match state(&*host, &ctx).await {
            Some(state) => Response::json(StatusCode::OK, &state),
            None => error(StatusCode::SERVICE_UNAVAILABLE, "the world is not running"),
        },
        ("/actions", &Method::POST) => act(&host, &req, &ctx).await,
        ("" | "/actions", _) => method_not_allowed(),
        _ => error(StatusCode::NOT_FOUND, "no such endpoint"),
    }
}

async fn act(host: &Arc<dyn PanelHost>, req: &Request, ctx: &AuthContext) -> Response {
    if !ctx.allows(Scope::PlayersModerate) {
        return error(StatusCode::FORBIDDEN, "not allowed");
    }
    let body = match json_body(req) {
        Ok(body) => body,
        Err(resp) => return resp,
    };
    let action = match parse_action(&body) {
        Ok(action) => action,
        Err(text) => return error(StatusCode::BAD_REQUEST, text),
    };
    let action = match action.player() {
        Some(player) => match host.players().resolve(player).await {
            Ok(name) => action.clone().with_player(name),
            Err(err) => return error(err.status, &err.message),
        },
        None => action,
    };
    if !host.web().auth.command_allowed(&ctx.session).await {
        return error(StatusCode::TOO_MANY_REQUESTS, "slow down");
    }
    let line = command_line(&action, &host.players().spawn_point());
    let who = ctx.principal.name();
    log::info!(target: "tuff::web", "{who} issued server command: /{line}");
    let (feedback, succeeded) =
        super::run_command(host.clone(), ctx.principal.clone(), ctx.level, &line).await;
    for text in &feedback {
        log::info!(target: "tuff::web", "[{who}] {text}");
    }
    let mut entry = Entry::new(who, format!("players.{}", action.name()))
        .remote(guard::client_ip(&**host, req))
        .target(action.target());
    if let Some(detail) = action.detail() {
        entry = entry.detail(detail);
    }
    if !succeeded {
        entry = entry.outcome(
            feedback
                .first()
                .cloned()
                .unwrap_or_else(|| "failed".to_owned()),
        );
    }
    host.web().audit.record(entry);
    Response::json(
        StatusCode::OK,
        &json!({
            "ok": succeeded,
            "command": line,
            "feedback": feedback,
            "state": state(&**host, ctx).await,
        }),
    )
}

/// The events that turn `old` rows into `new` ones: `join` and `update`
/// carry the row, `leave` the uuid and name.
pub fn diff(
    old: &BTreeMap<String, Value>,
    new: &BTreeMap<String, Value>,
) -> Vec<(&'static str, Value)> {
    let mut events = Vec::new();
    for (uuid, row) in old {
        if !new.contains_key(uuid) {
            events.push(("leave", json!({ "uuid": uuid, "name": row["name"] })));
        }
    }
    for (uuid, row) in new {
        match old.get(uuid) {
            None => events.push(("join", row.clone())),
            Some(before) if before != row => events.push(("update", row.clone())),
            Some(_) => {}
        }
    }
    events
}

/// Publishes the online players' changes on the `players` topic every
/// `period` while the panel is enabled; ends once the server is gone.
pub async fn forward(host: Weak<dyn PanelHost>, events: PanelEvents, period: Duration) {
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = BTreeMap::new();
    loop {
        ticker.tick().await;
        let Some(host) = host.upgrade() else {
            return;
        };
        if !super::enabled(&*host) {
            last.clear();
            continue;
        }
        let Some(rows) = host.players().rows().await else {
            return;
        };
        drop(host);
        for (name, data) in diff(&last, &rows) {
            events.publish(PLAYERS, name, data.to_string());
        }
        last = rows;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn names_and_texts_refuse_injection() {
        assert!(valid_name("Alex_01"));
        assert!(valid_name("a"));
        assert!(!valid_name(""));
        assert!(!valid_name("seventeen_letters"));
        assert!(!valid_name("Alex op"));
        assert!(!valid_name("@a"));
        assert!(!valid_name("Al\u{e9}x"));
        assert!(valid_text("griefing the spawn"));
        assert!(!valid_text("line\nop Steve"));
        assert!(!valid_text("tab\there"));
        assert!(!valid_text("   "));
        assert!(!valid_text(&"x".repeat(MAX_TEXT + 1)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn actions_parse_and_map_onto_commands() {
        let spawn = SpawnPoint {
            dimension: "minecraft:overworld".into(),
            position: [0.5, 64.0, -7.5],
        };
        let line = |body: Value| command_line(&parse_action(&body).unwrap(), &spawn);
        assert_eq!(
            line(json!({"action":"ban","player":"Alex","reason":" grief "})),
            "ban Alex grief"
        );
        assert_eq!(line(json!({"action":"kick","player":"Alex"})), "kick Alex");
        assert_eq!(
            line(json!({"action":"kick","player":"Alex","reason":""})),
            "kick Alex"
        );
        assert_eq!(
            line(json!({"action":"ban-ip","target":"10.0.0.1","reason":"alts"})),
            "ban-ip 10.0.0.1 alts"
        );
        assert_eq!(
            line(json!({"action":"pardon-ip","ip":"::1"})),
            "pardon-ip ::1"
        );
        assert_eq!(
            line(json!({"action":"whitelist-add","player":"Alex"})),
            "whitelist add Alex"
        );
        assert_eq!(line(json!({"action":"whitelist-off"})), "whitelist off");
        assert_eq!(
            line(json!({"action":"gamemode","player":"Alex","mode":"creative"})),
            "gamemode creative Alex"
        );
        assert_eq!(
            line(json!({"action":"spawn","player":"Alex"})),
            "execute in minecraft:overworld run tp Alex 0.5 64 -7.5"
        );
        assert_eq!(
            line(json!({"action":"message","player":"Alex","text":"hi there"})),
            "tell Alex hi there"
        );
        for bad in [
            json!({}),
            json!({"action":"op"}),
            json!({"action":"op","player":"Alex @a"}),
            json!({"action":"op","player":"Alex\nstop"}),
            json!({"action":"ban","player":"Alex","reason":"x\nop Steve"}),
            json!({"action":"ban","player":"Alex","reason":5}),
            json!({"action":"ban-ip","target":"not an ip"}),
            json!({"action":"pardon-ip","ip":"Alex"}),
            json!({"action":"gamemode","player":"Alex","mode":"god"}),
            json!({"action":"message","player":"Alex","text":"\u{7}"}),
            json!({"action":"stop"}),
        ] {
            assert!(parse_action(&bad).is_err(), "{bad}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn rows_diff_into_join_update_and_leave() {
        let row = |name: &str, ping: i32| json!({ "name": name, "ping": ping });
        let old = BTreeMap::from([("a".to_owned(), row("A", 1)), ("b".to_owned(), row("B", 1))]);
        let new = BTreeMap::from([("b".to_owned(), row("B", 9)), ("c".to_owned(), row("C", 1))]);
        let events = diff(&old, &new);
        assert_eq!(
            events,
            vec![
                ("leave", json!({"uuid":"a","name":"A"})),
                ("update", row("B", 9)),
                ("join", row("C", 1)),
            ]
        );
        assert!(diff(&new, &new).is_empty());
    }
}
