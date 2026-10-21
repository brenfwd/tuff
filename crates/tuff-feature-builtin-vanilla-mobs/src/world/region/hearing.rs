//! Wardens and allays hearing game events: each is a listener with a 16
//! block range. A vibration that reaches a warden is handed to it as a
//! [`Notice::Heard`]; allays learn of note blocks.

use std::collections::HashMap;

use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::tag::{self, Key};

use tuff_core::world::entity::Notice;
use tuff_core::world::game_event::{self, Candidate, Emission, GameEvent, Listener};
use tuff_core::world::region_api::RegionWorld;
use tuff_world_model::BlockPos;

const RANGE: i32 = 16;

#[derive(Default)]
struct Ears(HashMap<i32, Listener>);

fn listens(kind: EntityType) -> Option<Key<GameEvent>> {
    match kind {
        EntityType::Warden => Some(tag::game_event::key::WARDEN_CAN_LISTEN),
        EntityType::Allay => Some(tag::game_event::key::ALLAY_CAN_LISTEN),
        _ => None,
    }
}

fn listeners(world: &dyn RegionWorld) -> Vec<(i32, EntityType, [f64; 3])> {
    let found: Vec<(i32, EntityType, [f64; 3])> = world
        .entity_ids_of(&[EntityType::Warden, EntityType::Allay])
        .into_iter()
        .filter_map(|id| {
            let tracked = world.entity(id)?;
            let base = tracked.entity.base();
            let eye = match base.kind {
                EntityType::Warden => 2.0,
                EntityType::Allay => f64::from(base.kind.size().eye_height),
                _ => return None,
            };
            Some((
                id,
                base.kind,
                [base.position[0], base.position[1] + eye, base.position[2]],
            ))
        })
        .collect();
    found
}

pub fn hear(world: &mut dyn RegionWorld, e: &Emission) {
    crate::allay::hear_jukebox(world, e);
    let tick = world.tick();
    let origin = e.block_pos();
    for (id, kind, eye) in listeners(world) {
        let Some(listens) = listens(kind).filter(|key| tuff_datapack::tags::is(*key, e.event))
        else {
            continue;
        };
        if e.context.source.is_some_and(|s| s.entity == id) {
            continue;
        }
        let at = game_event::floor_pos(eye);
        let (dx, dy, dz) = (origin.x - at.x, origin.y - at.y, origin.z - at.z);
        let dist = dx * dx + dy * dy + dz * dz;
        if dist > RANGE * RANGE {
            continue;
        }
        if world
            .state()
            .get::<Ears>()
            .0
            .get(&id)
            .is_some_and(|l| l.is_busy())
        {
            continue;
        }
        let mut avoided = None;
        if kind == EntityType::Allay && !crate::allay::can_hear(world, id, origin) {
            continue;
        }
        if !game_event::audible_checking(listens, e, |source| {
            avoided = Some(*source);
        }) {
            if let Some(source) = avoided {
                game_event::report_avoided(world, &source);
            }
            continue;
        }
        let blocks = |p: BlockPos| world.block(p);
        if game_event::occluded(&blocks, e.pos, eye) {
            continue;
        }
        world.state().get::<Ears>().0.entry(id).or_default().offer(
            Candidate {
                event: e.event,
                distance: (dist as f32).sqrt(),
                origin: e.pos,
                source: e.context.source,
            },
            tick,
        );
    }
}

pub fn tick(world: &mut dyn RegionWorld) {
    let tick = world.tick();
    let mut ids: Vec<i32> = world.state().get::<Ears>().0.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let kind = world
            .entity(id)
            .map(|t| t.entity.base().kind)
            .filter(|kind| listens(*kind).is_some());
        let present = kind.is_some();
        let arrived = if present {
            world
                .state()
                .get::<Ears>()
                .0
                .get_mut(&id)
                .and_then(|l| l.advance(tick, |d| d.floor() as i32, || true))
        } else {
            None
        };
        let idle = !present
            || world
                .state()
                .get::<Ears>()
                .0
                .get(&id)
                .is_none_or(|l| l.is_idle());
        if idle {
            world.state().get::<Ears>().0.remove(&id);
        }
        let Some(vibration) = arrived else {
            continue;
        };
        let at = game_event::floor_pos(vibration.origin);
        if kind == Some(EntityType::Allay) {
            crate::allay::heard(world, id, vibration.event, at);
        } else if let Some(tracked) = world.entity_mut(id) {
            tracked.entity.notice(Notice::Heard {
                at: [at.x, at.y, at.z],
                source: vibration.source.map_or(-1, |s| s.entity),
            });
        }
    }
}
