//! Player state events (respawn, sneak, game mode, hunger, dimension) and
//! the state filters (`intercept-state`) that refuse game mode changes and
//! food losses without running guest code.

use std::collections::HashSet;

use tuff_core::protocol::play::GameMode;
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{HookFuture, SessionHooks, SessionTick};

use crate::bridge::player;
use crate::host::WasmHost;
use crate::sandbox::uuid_of;
use crate::wit::bindings::types::{
    Event, EventKinds, GameModeChanged, HungerChanged, RegionCrossed, RegionWatch, Respawned,
    SneakChange, StateChange, StateFilter, StateKind, TeleportAttempt, Vec3, WorldChanged,
};

pub fn matches(
    filter: &StateFilter,
    kind: StateKind,
    who: u128,
    dimension: &str,
    mode: Option<&str>,
) -> bool {
    filter.kind == kind
        && filter.player.is_none_or(|only| uuid_of(only) == who)
        && filter.dimension.as_deref().is_none_or(|d| d == dimension)
        && filter
            .game_mode
            .as_deref()
            .is_none_or(|wanted| Some(wanted) == mode)
}

fn refused(session: &PlayerSession, kind: StateKind, mode: Option<&str>) -> bool {
    let Some(host) = session.shared.services.get::<WasmHost>() else {
        return false;
    };
    let dimension = &session.dimension().key;
    host.snapshot()
        .state
        .iter()
        .any(|filter| matches(filter, kind, session.profile.id, dimension, mode))
}

fn announce(
    session: &PlayerSession,
    flag: EventKinds,
    event: impl FnOnce(crate::sandbox::Player) -> StateChange,
) {
    let Some(host) = session.shared.services.get::<WasmHost>() else {
        return;
    };
    if !host.listens(flag) {
        return;
    }
    let who = player(session);
    host.event(Event::PlayerState(event(who.clone())), Some(who));
}

#[derive(Default)]
struct InRegions(HashSet<String>);

pub fn inside(region: &RegionWatch, dimension: &str, pos: [f64; 3]) -> bool {
    let at = pos.map(|axis| axis.floor() as i32);
    let (lo, hi) = (
        [region.min.x, region.min.y, region.min.z],
        [region.max.x, region.max.y, region.max.z],
    );
    region.dimension == dimension
        && (0..3)
            .all(|axis| at[axis] >= lo[axis].min(hi[axis]) && at[axis] <= lo[axis].max(hi[axis]))
}

fn track_regions(session: &mut PlayerSession, leaving: bool) {
    let Some(host) = session.shared.services.get::<WasmHost>() else {
        return;
    };
    if !host.listens(EventKinds::PLAYER_STATE) {
        return;
    }
    let snapshot = host.snapshot();
    if snapshot.regions.is_empty() && session.ext.get::<InRegions>().is_none() {
        return;
    }
    let dimension = session.dimension().key.clone();
    let pos = session.pose.position;
    let mut crossings = Vec::new();
    let held = &mut session.ext.entry::<InRegions>().0;
    for region in &snapshot.regions {
        let now = !leaving && inside(region, &dimension, pos);
        if now != held.contains(&region.name) {
            if now {
                held.insert(region.name.clone());
            } else {
                held.remove(&region.name);
            }
            crossings.push((region.name.clone(), now));
        }
    }
    for (region, entered) in crossings {
        announce(session, EventKinds::PLAYER_STATE, |who| {
            StateChange::Region(RegionCrossed {
                player: who.to_wit(),
                region,
                entered,
            })
        });
    }
}

pub struct States;

impl SessionHooks for States {
    fn tick<'a>(&'a self, session: &'a mut PlayerSession, phase: SessionTick) -> HookFuture<'a> {
        if phase == SessionTick::Late {
            track_regions(session, false);
        }
        Box::pin(async {})
    }

    fn leaving<'a>(&'a self, session: &'a mut PlayerSession) -> HookFuture<'a> {
        track_regions(session, true);
        Box::pin(async {})
    }

    fn allows_teleport(&self, session: &PlayerSession, dimension: &str, _pos: [f64; 3]) -> bool {
        let Some(host) = session.shared.services.get::<WasmHost>() else {
            return true;
        };
        !host
            .snapshot()
            .state
            .iter()
            .any(|f| matches(f, StateKind::Teleport, session.profile.id, dimension, None))
    }

    fn teleport_decided(
        &self,
        session: &PlayerSession,
        dimension: &str,
        pos: [f64; 3],
        cancelled: bool,
    ) {
        announce(session, EventKinds::PLAYER_STATE, |who| {
            let at = session.pose.position;
            StateChange::Teleport(TeleportAttempt {
                player: who.to_wit(),
                before_dimension: session.dimension().key.clone(),
                before: Vec3 {
                    x: at[0],
                    y: at[1],
                    z: at[2],
                },
                after_dimension: dimension.to_owned(),
                after: Vec3 {
                    x: pos[0],
                    y: pos[1],
                    z: pos[2],
                },
                cancelled,
            })
        });
    }

    fn respawned(&self, session: &PlayerSession) {
        announce(session, EventKinds::PLAYER_STATE, |who| {
            StateChange::Respawn(Respawned {
                player: who.to_wit(),
                pos: Vec3 {
                    x: session.pose.position[0],
                    y: session.pose.position[1],
                    z: session.pose.position[2],
                },
                dimension: session.dimension().key.clone(),
            })
        });
    }

    fn sneak_changed(&self, session: &PlayerSession, sneaking: bool) {
        announce(session, EventKinds::PLAYER_STATE, |who| {
            StateChange::Sneak(SneakChange {
                player: who.to_wit(),
                sneaking,
            })
        });
    }

    fn allows_game_mode(&self, session: &PlayerSession, mode: GameMode) -> bool {
        !refused(session, StateKind::GameMode, Some(mode.name()))
    }

    fn game_mode_changed(
        &self,
        session: &PlayerSession,
        from: GameMode,
        to: GameMode,
        cancelled: bool,
    ) {
        announce(session, EventKinds::PLAYER_STATE, |who| {
            StateChange::GameMode(GameModeChanged {
                player: who.to_wit(),
                before: from.name().to_owned(),
                after: to.name().to_owned(),
                cancelled,
            })
        });
    }

    fn allows_food_loss(&self, session: &PlayerSession) -> bool {
        !refused(session, StateKind::HungerLoss, None)
    }

    fn food_changed(&self, session: &PlayerSession, from: i32, to: i32, locked: bool) {
        announce(session, EventKinds::PLAYER_STATE, |who| {
            StateChange::Hunger(HungerChanged {
                player: who.to_wit(),
                before: from,
                after: to,
                locked,
            })
        });
    }

    fn dimension_changed(&self, session: &PlayerSession, from: &str) {
        announce(session, EventKinds::PLAYER_STATE, |who| {
            StateChange::Dimension(WorldChanged {
                player: who.to_wit(),
                before: from.to_owned(),
                after: session.dimension().key.clone(),
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn region_box_is_inclusive_in_any_corner_order() {
        let region = RegionWatch {
            name: "a".into(),
            dimension: "minecraft:overworld".into(),
            min: crate::wit::bindings::types::BlockPos { x: 5, y: 70, z: 5 },
            max: crate::wit::bindings::types::BlockPos { x: 0, y: 60, z: 0 },
        };
        assert!(inside(&region, "minecraft:overworld", [5.9, 70.0, 0.0]));
        assert!(inside(&region, "minecraft:overworld", [0.0, 60.0, 0.5]));
        assert!(!inside(&region, "minecraft:overworld", [6.0, 65.0, 1.0]));
        assert!(!inside(&region, "minecraft:overworld", [-0.1, 65.0, 1.0]));
        assert!(!inside(&region, "minecraft:the_end", [1.0, 65.0, 1.0]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_field_narrows_the_match() {
        let filter = StateFilter {
            kind: StateKind::GameMode,
            player: None,
            dimension: Some("minecraft:overworld".into()),
            game_mode: Some("creative".into()),
        };
        let kind = StateKind::GameMode;
        assert!(matches(
            &filter,
            kind,
            1,
            "minecraft:overworld",
            Some("creative")
        ));
        assert!(!matches(
            &filter,
            kind,
            1,
            "minecraft:the_end",
            Some("creative")
        ));
        assert!(!matches(
            &filter,
            kind,
            1,
            "minecraft:overworld",
            Some("survival")
        ));
        assert!(!matches(
            &filter,
            StateKind::HungerLoss,
            1,
            "minecraft:overworld",
            None
        ));
    }
}
