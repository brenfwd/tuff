//! A region's sculk shriekers (see [`crate::world::sculk`]): they hear a
//! sensor's tendrils click as a vibration, or feel a player step on them,
//! warn the player behind it, shriek for 90 ticks and then answer with a
//! Warden's distant call and Darkness, up to a Warden emerging.

use std::collections::HashMap;

use rand::Rng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, tag};

use crate::world::sculk;
use tuff_core::protocol::play;
use tuff_core::storage::player_data::WardenTracker;
use tuff_core::world::game_event::{self, Candidate, Context, Emission, GameEvent, Listeners};
use tuff_core::world::messages::{PlayerEvent, PlayerId, PlayerUpdate};
use tuff_core::world::region_api::{RegionWorld, SeenPlayer, TickPhase};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const SHRIEK_TICKS: u64 = 90;
const MAX_WARNING: i32 = WardenTracker::MAX_WARNING_LEVEL;
const NEARBY_PLAYERS: f64 = 16.0;
const WARDEN_CHECK_RADIUS: f64 = 24.0;
const DARKNESS_RADIUS: f64 = 40.0;
const REPLY_SPREAD: i32 = 10;
const SUMMON_ATTEMPTS: usize = 20;
const SUMMON_XZ: i32 = 5;
const SUMMON_Y: i32 = 6;
/// How often shriekers left shrieking (a reload, a lost tick) are found.
const RESCAN_INTERVAL: u64 = 20;

#[derive(Default)]
struct Ears(Listeners);

/// A shriek under way: when it ends, and what the shrieker will answer
/// with, kept here so a shrieker broken mid-shriek still answers.
#[derive(Clone, Copy)]
struct Shriek {
    until: u64,
    warning: i32,
    can_summon: bool,
}

#[derive(Default)]
struct Shrieking(HashMap<BlockPos, Shriek>);

fn shrieking(world: &mut dyn RegionWorld) -> &mut HashMap<BlockPos, Shriek> {
    &mut world.state().get::<Shrieking>().0
}

fn blamed(emission: &Emission) -> Option<PlayerId> {
    emission.context.source.and_then(|s| s.player)
}

pub fn hear(world: &mut dyn RegionWorld, e: &Emission) {
    if !tuff_datapack::tags::is(tag::game_event::key::SHRIEKER_CAN_LISTEN, e.event)
        || blamed(e).is_none()
    {
        return;
    }
    let shriekers = world.raw_block_entities_near(
        e.block_pos(),
        sculk::WARNING_RANGE as i32,
        &sculk::is_shrieker,
    );
    let tick = world.tick();
    let origin = e.block_pos();
    for pos in shriekers {
        let Some(state) = world.block(pos) else {
            continue;
        };
        if state.get("shrieking") == Some("true") {
            continue;
        }
        let (dx, dy, dz) = (origin.x - pos.x, origin.y - pos.y, origin.z - pos.z);
        let dist = dx * dx + dy * dy + dz * dz;
        let range = sculk::WARNING_RANGE as i32;
        if dist > range * range {
            continue;
        }
        let ears = &mut world.state().get::<Ears>().0;
        if ears.get(&pos).is_some_and(|l| l.is_busy()) {
            continue;
        }
        if !game_event::audible(tag::game_event::key::SHRIEKER_CAN_LISTEN, e) {
            continue;
        }
        let blocks = |p: BlockPos| world.block(p);
        if game_event::occluded(&blocks, e.pos, pos.center()) {
            continue;
        }
        let candidate = Candidate {
            event: e.event,
            distance: (dist as f32).sqrt(),
            origin: e.pos,
            source: e.context.source,
        };
        world
            .state()
            .get::<Ears>()
            .0
            .entry(pos)
            .or_default()
            .offer(candidate, tick);
    }
}

pub fn tick(world: &mut dyn RegionWorld, phase: TickPhase) {
    if phase != TickPhase::Late {
        return;
    }
    let tick = world.tick();
    if tick.is_multiple_of(RESCAN_INTERVAL) {
        game_event::adopt_saved(
            world,
            |w| &mut w.state().get::<Ears>().0,
            sculk::is_shrieker,
        );
    }
    let mut positions: Vec<BlockPos> = world.state().get::<Ears>().0.keys().copied().collect();
    positions.sort_unstable_by_key(|p| (p.y, p.z, p.x));
    let loaded = if positions.is_empty() {
        Vec::new()
    } else {
        world.lit_ready_chunks()
    };
    for pos in positions {
        let here = world.raw_block_entity(pos).is_some_and(sculk::is_shrieker);
        let arrived = if here {
            let ready = loaded.contains(&ChunkPos::of_block(pos));
            world
                .state()
                .get::<Ears>()
                .0
                .get_mut(&pos)
                .and_then(|l| l.advance(tick, |d| d.floor() as i32, || ready))
        } else {
            None
        };
        let idle = !here
            || world
                .state()
                .get::<Ears>()
                .0
                .get(&pos)
                .is_none_or(|l| l.is_idle());
        if here {
            let listener = world.state().get::<Ears>().0.get(&pos).cloned();
            game_event::store(world, pos, listener.as_ref());
        }
        if idle {
            world.state().get::<Ears>().0.remove(&pos);
        }
        if let Some(vibration) = arrived
            && let Some(player) = vibration.source.and_then(|s| s.player)
        {
            try_shriek(world, pos, player, None);
        }
    }

    if tick.is_multiple_of(RESCAN_INTERVAL) {
        adopt_stuck(world, tick);
    }

    let mut done: Vec<(BlockPos, Shriek)> = shrieking(world)
        .iter()
        .map(|(pos, shriek)| (*pos, *shriek))
        .collect();
    done.sort_unstable_by_key(|(p, _)| (p.y, p.z, p.x));
    for (pos, shriek) in done {
        let present = world
            .block(pos)
            .is_some_and(|s| s.block() == Block::SculkShrieker);
        if present && shriek.until > tick {
            continue;
        }
        shrieking(world).remove(&pos);
        finish(world, pos, shriek);
    }
}

/// Shriekers found shrieking with no shriek under way (the region was
/// reloaded mid-shriek) finish now.
fn adopt_stuck(world: &mut dyn RegionWorld, tick: u64) {
    let stuck: Vec<BlockPos> = world
        .raw_block_entities_where(&sculk::is_shrieker)
        .into_iter()
        .filter(|pos| {
            world
                .block(*pos)
                .is_some_and(|s| s.get("shrieking") == Some("true"))
        })
        .collect();
    for pos in stuck {
        if shrieking(world).contains_key(&pos) {
            continue;
        }
        let warning = world.raw_block_entity(pos).map_or(0, sculk::warning_level);
        let can_summon = world
            .block(pos)
            .is_some_and(|s| s.get("can_summon") == Some("true"));
        shrieking(world).insert(
            pos,
            Shriek {
                until: tick,
                warning,
                can_summon,
            },
        );
    }
}

/// A player standing on a shrieker sets it off.
pub fn stepped(world: &mut dyn RegionWorld, update: &PlayerUpdate) {
    if !update.alive || !update.grounded || update.spectator {
        return;
    }
    let [x, y, z] = update.position;
    let pos = BlockPos::new(x.floor() as i32, (y - 0.2).floor() as i32, z.floor() as i32);
    if world
        .block(pos)
        .is_some_and(|s| s.block() == Block::SculkShrieker)
        && world.raw_block_entity(pos).is_some_and(sculk::is_shrieker)
    {
        let trigger = SeenPlayer {
            handle: update.handle.clone(),
            uuid: update.uuid,
            name: update.name.clone(),
            position: update.position,
            looks: update.looks,
        };
        try_shriek(world, pos, update.handle.id, Some(trigger));
    }
}

fn can_respond(world: &dyn RegionWorld, can_summon: bool) -> bool {
    can_summon && world.difficulty() != 0 && world.rule_bool("spawn_wardens")
}

fn can_summon(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    world
        .block(pos)
        .is_some_and(|s| s.get("can_summon") == Some("true"))
}

fn has_nearby_warden(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    let centre = pos.center();
    world
        .entity_ids_of(&[EntityType::Warden])
        .into_iter()
        .any(|id| {
            world.entity(id).is_some_and(|tracked| {
                let base = tracked.entity.base();
                base.kind == EntityType::Warden
                    && (base.position[0] - centre[0]).abs() <= WARDEN_CHECK_RADIUS
                    && (base.position[1] - centre[1]).abs() <= WARDEN_CHECK_RADIUS
                    && (base.position[2] - centre[2]).abs() <= WARDEN_CHECK_RADIUS
            })
        })
}

fn distance_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]) * (a[i] - b[i])).sum()
}

fn try_warn(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    player: PlayerId,
    trigger: Option<SeenPlayer>,
) -> Option<i32> {
    if has_nearby_warden(world, pos) {
        return None;
    }
    let centre = pos.center();
    let details = world.seen_player_details();
    let mut players: Vec<&SeenPlayer> = details
        .iter()
        .filter(|p| distance_sq(p.position, centre) < NEARBY_PLAYERS * NEARBY_PLAYERS)
        .collect();
    if !players.iter().any(|p| p.handle.id == player) {
        players.extend(
            details
                .iter()
                .find(|p| p.handle.id == player)
                .or(trigger.as_ref()),
        );
    }
    if players.iter().any(|p| p.looks.warden_cooldown) {
        return None;
    }
    let highest = players.iter().map(|p| p.looks.warden_warning).max()?;
    let warned = WardenTracker {
        warning_level: i32::from(highest),
        ..Default::default()
    }
    .warned();
    for p in players {
        p.handle.send(PlayerEvent::WardenTracker(warned));
    }
    Some(warned.warning_level)
}

fn try_shriek(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    player: PlayerId,
    trigger: Option<SeenPlayer>,
) {
    let Some(state) = world.block(pos) else {
        return;
    };
    if state.block() != Block::SculkShrieker || state.get("shrieking") == Some("true") {
        return;
    }
    let mut warning = 0;
    let summons = can_summon(world, pos);
    if can_respond(world, summons) {
        match try_warn(world, pos, player, trigger) {
            Some(level) => warning = level,
            None => return,
        }
    }
    if let Some(nbt) = world.raw_block_entity_mut(pos) {
        sculk::set_warning_level(nbt, warning);
    }
    if let Some(next) = state.with("shrieking", "true") {
        world.set_block_flags(pos, next, 2);
    }
    let until = world.tick() + SHRIEK_TICKS;
    shrieking(world).insert(
        pos,
        Shriek {
            until,
            warning,
            can_summon: summons,
        },
    );
    world.level_event(play::level_event::ANIMATION_SCULK_SHRIEK, pos, 0);
    world.game_event(
        GameEvent::Shriek,
        pos.center(),
        Context::from(game_event::Source::player(player)),
    );
}

fn finish(world: &mut dyn RegionWorld, pos: BlockPos, shriek: Shriek) {
    if let Some(state) = world.block(pos)
        && state.block() == Block::SculkShrieker
        && state.get("shrieking") == Some("true")
        && let Some(next) = state.with("shrieking", "false")
    {
        world.set_block_flags(pos, next, 3);
    }
    if !can_respond(world, shriek.can_summon) || shriek.warning <= 0 {
        return;
    }
    if shriek.warning < MAX_WARNING || !summon_warden(world, pos) {
        reply(world, pos, shriek.warning);
    }
    let centre = pos.center();
    for player in world.seen_player_details() {
        if !player.looks.creative
            && distance_sq(player.position, centre) < DARKNESS_RADIUS * DARKNESS_RADIUS
        {
            player
                .handle
                .send(PlayerEvent::AddEffect(crate::warden::darkness()));
        }
    }
}

/// A Warden's call from somewhere around the shrieker, closer with each
/// warning.
fn reply(world: &mut dyn RegionWorld, pos: BlockPos, warning: i32) {
    let sound = match warning {
        1 => SoundEvent::EntityWardenNearbyClose,
        2 => SoundEvent::EntityWardenNearbyCloser,
        3 => SoundEvent::EntityWardenNearbyClosest,
        4 => SoundEvent::EntityWardenListeningAngry,
        _ => return,
    };
    let mut offset = || world.rng().gen_range(-REPLY_SPREAD..=REPLY_SPREAD);
    let at = BlockPos::new(pos.x + offset(), pos.y + offset(), pos.z + offset());
    let seed = world.rng().r#gen();
    let packet = tuff_core::world::framed(&play::Sound {
        sound_event: sound.id(),
        category: play::sound_category::HOSTILE,
        position: [f64::from(at.x), f64::from(at.y), f64::from(at.z)],
        volume: 5.0,
        pitch: 1.0,
        seed,
    });
    world.send_to_chunk(
        ChunkPos::of_block(pos),
        None,
        tuff_core::world::packets([packet]),
    );
}

/// Where around `shrieker` a Warden can come up: random columns, each
/// searched down for the top of a solid block with nothing above it.
fn summon_spot(world: &mut dyn RegionWorld, shrieker: BlockPos) -> Option<[f64; 3]> {
    for _ in 0..SUMMON_ATTEMPTS {
        let dx = world.rng().gen_range(-SUMMON_XZ..=SUMMON_XZ);
        let dz = world.rng().gen_range(-SUMMON_XZ..=SUMMON_XZ);
        let (x, z) = (shrieker.x + dx, shrieker.z + dz);
        let mut above = world.block(BlockPos::new(x, shrieker.y + SUMMON_Y, z));
        for y in (shrieker.y - SUMMON_Y - 1..shrieker.y + SUMMON_Y).rev() {
            let here = world.block(BlockPos::new(x, y, z));
            let fits = above.is_some_and(|a| a.collision_boxes().is_empty())
                && here.is_some_and(|h| h.is_collision_face_full(Facing::Up));
            if fits {
                let at = [f64::from(x) + 0.5, f64::from(y + 1), f64::from(z) + 0.5];
                let bounds = crate::spawning::spawn_box(EntityType::Warden, at);
                if crate::spawning::unobstructed(bounds, &|p| world.block(p)) {
                    return Some(at);
                }
                break;
            }
            above = here;
        }
    }
    None
}

fn summon_warden(world: &mut dyn RegionWorld, shrieker: BlockPos) -> bool {
    let Some(at) = summon_spot(world, shrieker) else {
        return false;
    };
    if !world.owns_chunk(ChunkPos::of_position(at)) {
        return false;
    }
    let id = world.shared().next_entity_id();
    let Some(mut entity) = world
        .shared()
        .entity_types
        .create(EntityType::Warden, id, at, 0.0)
    else {
        return false;
    };
    world.finalize_spawn(&mut entity, 0);
    if let Some(warden) = entity.downcast_mut::<crate::Mob<crate::warden::Warden>>() {
        warden.species.emerge(&mut warden.core);
    }
    world.add_entity(entity);
    true
}
