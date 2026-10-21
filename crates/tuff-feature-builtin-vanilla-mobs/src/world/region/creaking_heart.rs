//! Creaking hearts: a heart between two matching pale oak logs wakes at
//! night, spawns a creaking protector when a player comes near, keeps
//! track of it by UUID and tears it down at daybreak, when it strays too
//! far, when a player is stuck in it or when the heart goes. A player's hit
//! on the protector makes the heart call out (trail particles, a sound
//! walking from the protector back to the heart) and, while awake, grow
//! resin on its logs. Comparators read how close the protector is.

use std::collections::{HashMap, HashSet, VecDeque};

use rand::Rng;
use tuff_core::world::player::Difficulty;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::{EntityType, ParticleType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState, damage_type, tag};

use crate::Mob;
use crate::creaking::Creaking;
use crate::{Damage, physics, spawning};
use tuff_core::world::block_entity::analog::RuntimeSignals;
use tuff_core::world::game_event::{Context, GameEvent, Source};
use tuff_core::world::region_api::{BlockAction, PlayerBlockAction, RegionState, RegionWorld};
use tuff_core::world::{framed, packets};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const RESCAN_TICKS: u64 = 20;
const PLAYER_DETECTION_RANGE: f64 = 32.0;
const ROAMING_RADIUS: f64 = 32.0;
const TOO_FAR: f64 = 34.0;
const SPAWN_RANGE_XZ: i32 = 16;
const SPAWN_RANGE_Y: i32 = 8;
const SPAWN_ATTEMPTS: usize = 5;
const UPDATE_TICKS: i32 = 20;
const UPDATE_TICKS_VARIANCE: i32 = 5;
const GRACE_TICKS: u64 = 30;
const HURT_CALL_TICKS: i32 = 100;
const HURT_CALL_INTERVAL: i32 = 10;
const HURT_CALL_PARTICLE_TICKS: i32 = 50;
const HURT_CALL_PARTICLES: usize = 20;
const RESIN_DEPTH: u32 = 2;
const RESIN_COUNT: u32 = 64;
const ORANGE: i32 = 16_545_810;
const GRAY: i32 = 6_250_335;
const BREAK_EXPERIENCE: std::ops::RangeInclusive<i32> = 20..=24;
const CREAKING_KEY: &str = "creaking";
const STATE: &str = "creaking_heart_state";

/// The game's direction order, which its shuffles start from.
const DIRECTIONS: [Facing; 6] = [
    Facing::Down,
    Facing::Up,
    Facing::North,
    Facing::South,
    Facing::West,
    Facing::East,
];

#[derive(Default)]
struct Heart {
    ticks_existed: u64,
    ticker: i32,
    signal: u8,
    protector: Option<u128>,
    emitter: i32,
    emitter_target: Option<[f64; 3]>,
}

#[derive(Default)]
struct Hearts {
    positions: Vec<BlockPos>,
    runtime: HashMap<BlockPos, Heart>,
    /// Player hits on protectors: the heart they belong to and who was hit.
    calls: Vec<(BlockPos, u128)>,
}

pub fn is_heart(nbt: &Compound) -> bool {
    nbt.get_str("id") == Some("minecraft:creaking_heart")
}

/// Whether the heart at `home` names creaking `uuid` its protector;
/// `None` while this region has not looked at that heart yet.
/// A heart in another region answers through it, asked at tick `now`.
pub fn protects(state: &mut RegionState, home: BlockPos, uuid: u128, now: u64) -> Option<bool> {
    if let Some(heart) = state.get::<Hearts>().runtime.get(&home) {
        return Some(heart.protector == Some(uuid));
    }
    foreign_protects(state, home, uuid, now)
}

/// A player hit protector `uuid` of the heart at `home`; the heart
/// answers when it next ticks, wherever it is.
pub fn call(state: &mut RegionState, home: BlockPos, uuid: u128) {
    if state.get::<Hearts>().runtime.contains_key(&home) {
        state.get::<Hearts>().calls.push((home, uuid));
    } else {
        state.get::<Remote>().calls.push((home, uuid));
    }
}

fn axis_offsets(axis: &str) -> [BlockPos; 2] {
    match axis {
        "x" => [BlockPos::new(1, 0, 0), BlockPos::new(-1, 0, 0)],
        "y" => [BlockPos::new(0, 1, 0), BlockPos::new(0, -1, 0)],
        _ => [BlockPos::new(0, 0, 1), BlockPos::new(0, 0, -1)],
    }
}

/// Whether both logs along the heart's axis are pale oak on that axis.
pub fn has_required_logs(
    state: BlockState,
    pos: BlockPos,
    block: impl Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let Some(axis) = state.get("axis") else {
        return false;
    };
    axis_offsets(axis).iter().all(|o| {
        block(BlockPos::new(pos.x + o.x, pos.y + o.y, pos.z + o.z)).is_some_and(|n| {
            tuff_datapack::tags::is(tag::block::key::PALE_OAK_LOGS, n.block())
                && n.get("axis") == Some(axis)
        })
    })
}

/// The comparator reading for a protector `distance` blocks away.
pub fn signal_for_distance(distance: f64) -> u8 {
    let scaled = distance.clamp(0.0, ROAMING_RADIUS) / ROAMING_RADIUS;
    15 - (scaled * 15.0).floor() as u8
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3)
        .map(|i| (a[i] - b[i]) * (a[i] - b[i]))
        .sum::<f64>()
        .sqrt()
}

fn bottom_center(pos: BlockPos) -> [f64; 3] {
    [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    ]
}

fn block_of(at: [f64; 3]) -> BlockPos {
    BlockPos::new(
        at[0].floor() as i32,
        at[1].floor() as i32,
        at[2].floor() as i32,
    )
}

fn center_of(bounds: [f64; 6]) -> [f64; 3] {
    [0, 1, 2].map(|i| (bounds[i] + bounds[i + 3]) / 2.0)
}

fn find_creaking(world: &dyn RegionWorld, uuid: u128) -> Option<(i32, [f64; 3])> {
    world.entity_ids().into_iter().find_map(|id| {
        let base = world.entity(id)?.entity.base();
        (base.uuid == uuid && base.kind == EntityType::Creaking).then_some((id, base.position))
    })
}

fn creaking_mut(world: &mut dyn RegionWorld, id: i32) -> Option<&mut Mob<Creaking>> {
    world.entity_mut(id)?.entity.downcast_mut::<Mob<Creaking>>()
}

fn stored_uuid(world: &dyn RegionWorld, pos: BlockPos) -> Option<u128> {
    world
        .raw_block_entity(pos)?
        .get(CREAKING_KEY)
        .and_then(Tag::as_uuid)
}

fn set_stored_uuid(world: &mut dyn RegionWorld, pos: BlockPos, uuid: Option<u128>) {
    if let Some(nbt) = world.raw_block_entity_mut(pos) {
        match uuid {
            Some(uuid) => {
                nbt.insert(CREAKING_KEY, Tag::uuid(uuid));
            }
            None => {
                nbt.remove(CREAKING_KEY);
            }
        }
    }
    world.mark_blocks_dirty(pos);
}

fn hearts(world: &mut dyn RegionWorld) -> &mut Hearts {
    world.state().get::<Hearts>()
}

fn heart(world: &mut dyn RegionWorld, pos: BlockPos) -> &mut Heart {
    hearts(world).runtime.entry(pos).or_default()
}

fn set_signal(world: &mut dyn RegionWorld, pos: BlockPos, signal: Option<u8>) {
    let signals = &mut world.state().get::<RuntimeSignals>().0;
    let changed = match signal {
        Some(signal) => signals.insert(pos, signal) != Some(signal),
        None => signals.remove(&pos).is_some_and(|old| old != 0),
    };
    if changed {
        world.update_output_signal(pos, Block::CreakingHeart);
    }
}

/// Crumbles creaking `id` away: splinters, its death sound, gone.
fn tear_down(world: &mut dyn RegionWorld, id: i32) {
    let Some(tracked) = world.entity(id) else {
        return;
    };
    let bounds = tracked.entity.bounds();
    let at = tracked.entity.base().position;
    world.send_to_chunk(
        ChunkPos::of_position(at),
        None,
        crate::creaking::tear_down_particles(bounds),
    );
    world.play_sound(
        SoundEvent::EntityCreakingDeath,
        block_of(at),
        1.0,
        1.0,
        None,
    );
    world.remove_entity(id);
}

fn tear_down_protector(world: &mut dyn RegionWorld, uuid: u128) {
    match find_creaking(world, uuid) {
        Some((id, _)) => tear_down(world, id),
        None => on_remote_protector(world, uuid, tear_down),
    }
}

pub fn tick(world: &mut dyn RegionWorld) {
    if world.tick().is_multiple_of(RESCAN_TICKS) {
        let mut found = world.raw_block_entities_where(&is_heart);
        found.sort_unstable_by_key(|p| (p.y, p.z, p.x));
        hearts(world).positions = found;
    }
    let positions = std::mem::take(&mut hearts(world).positions);
    let calls = std::mem::take(&mut hearts(world).calls);
    let mut gone: Vec<BlockPos> = hearts(world)
        .runtime
        .keys()
        .copied()
        .filter(|p| !positions.contains(p))
        .collect();
    for &pos in &positions {
        let present = world
            .block(pos)
            .is_some_and(|s| s.block() == Block::CreakingHeart)
            && world.raw_block_entity(pos).is_some_and(is_heart);
        if present {
            tick_heart(world, pos, &calls);
        } else {
            gone.push(pos);
        }
    }
    gone.sort_unstable_by_key(|p| (p.y, p.z, p.x));
    gone.dedup();
    for pos in gone {
        if let Some(heart) = hearts(world).runtime.remove(&pos) {
            let still_here = world.raw_block_entity(pos).is_some_and(is_heart);
            if !still_here && let Some(uuid) = heart.protector {
                tear_down_protector(world, uuid);
            }
        }
        set_signal(world, pos, None);
    }
    let still: Vec<BlockPos> = positions
        .into_iter()
        .filter(|p| world.raw_block_entity(*p).is_some_and(is_heart))
        .collect();
    hearts(world).positions = still;
    exchange(world);
}

fn protector(world: &mut dyn RegionWorld, pos: BlockPos) -> Option<(i32, [f64; 3])> {
    let uuid = stored_uuid(world, pos);
    let ticks = {
        let heart = heart(world, pos);
        heart.protector = uuid;
        heart.ticks_existed
    };
    let uuid = uuid?;
    let found = find_creaking(world, uuid);
    let elsewhere = found.is_none() && remote_protector(world, uuid).is_some();
    if found.is_none() && !elsewhere && ticks >= GRACE_TICKS {
        set_stored_uuid(world, pos, None);
        heart(world, pos).protector = None;
    }
    found
}

fn protector_bounds(world: &dyn RegionWorld, found: Option<(i32, [f64; 3])>) -> Option<[f64; 6]> {
    let (id, _) = found?;
    Some(world.entity(id)?.entity.bounds())
}

fn tick_heart(world: &mut dyn RegionWorld, pos: BlockPos, calls: &[(BlockPos, u128)]) {
    heart(world, pos).ticks_existed += 1;
    let found = protector(world, pos);
    let remote_at = match (found, heart(world, pos).protector) {
        (None, Some(uuid)) => remote_protector(world, uuid),
        _ => None,
    };
    let state = world
        .block(pos)
        .unwrap_or(Block::CreakingHeart.default_state());
    let signal = if state.get(STATE) == Some("uprooted") {
        0
    } else {
        found.map(|(_, at)| at).or(remote_at).map_or(0, |at| {
            signal_for_distance(distance(at, bottom_center(pos)))
        })
    };
    let previous = std::mem::replace(&mut heart(world, pos).signal, signal);
    if previous != signal || !world.state().get::<RuntimeSignals>().0.contains_key(&pos) {
        set_signal(world, pos, Some(signal));
    }

    for &(_, uuid) in calls.iter().filter(|(home, _)| *home == pos) {
        if heart(world, pos).protector == Some(uuid) {
            creaking_hurt(world, pos, state, found);
        }
    }
    tick_emitter(world, pos, found);

    let ticker = {
        let heart = heart(world, pos);
        let ticker = heart.ticker;
        heart.ticker -= 1;
        ticker
    };
    if ticker >= 0 {
        return;
    }
    let next = UPDATE_TICKS + world.rng().gen_range(0..UPDATE_TICKS_VARIANCE);
    heart(world, pos).ticker = next;

    let shared = world.shared().clone();
    let active = tuff_core::world::creaking_active(&shared, world.dimension_info());
    let updated = updated_state(world, state, pos, active);
    if updated != state {
        world.set_block_flags(pos, updated, 3);
        if updated.get(STATE) == Some("uprooted") {
            return;
        }
    }
    if let Some(uuid) = stored_uuid(world, pos) {
        if found.is_none()
            && let Some(at) = remote_at
            && (!active || distance(at, bottom_center(pos)) > TOO_FAR)
        {
            on_remote_protector(world, uuid, tear_down);
            set_stored_uuid(world, pos, None);
            heart(world, pos).protector = None;
            return;
        }
        if let Some((id, at)) = found
            && should_remove(world, id, at, pos, active)
        {
            tear_down(world, id);
            set_stored_uuid(world, pos, None);
            heart(world, pos).protector = None;
        }
        return;
    }
    if updated.get(STATE) != Some("awake")
        || !shared.game_rules.borrow().bool("spawn_monsters")
        || *shared.difficulty.borrow() == Difficulty::Peaceful
    {
        return;
    }
    let corner = [f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)];
    let near = world
        .seen_player_positions()
        .into_iter()
        .any(|at| distance(at, corner) < PLAYER_DETECTION_RANGE);
    if near {
        spawn_protector(world, pos);
    }
}

/// The heart lets its protector go: night is over (unless it must stay),
/// it strayed too far, or a player is stuck inside it.
fn should_remove(
    world: &mut dyn RegionWorld,
    id: i32,
    at: [f64; 3],
    pos: BlockPos,
    active: bool,
) -> bool {
    let persistent = world
        .entity(id)
        .and_then(|t| t.entity.downcast_ref::<Mob<Creaking>>())
        .is_some_and(|mob| mob.core.persistence_required);
    if (!active && !persistent) || distance(at, bottom_center(pos)) > TOO_FAR {
        return true;
    }
    let eyes: Vec<[f64; 3]> = world
        .seen_player_details()
        .into_iter()
        .map(|p| {
            let [x, y, z] = p.position;
            [x, y + p.looks.eye_height, z]
        })
        .collect();
    creaking_mut(world, id).is_some_and(|mob| mob.species.player_stuck_in(&mob.core, &eyes))
}

/// A player hit the protector: the heart calls out once at a time, and
/// while awake grows two or three resin clumps on its logs.
fn creaking_hurt(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    state: BlockState,
    found: Option<(i32, [f64; 3])>,
) {
    let Some(bounds) = protector_bounds(world, found) else {
        return;
    };
    if heart(world, pos).emitter > 0 {
        return;
    }
    for _ in 0..HURT_CALL_PARTICLES {
        emit_particle(world, pos, bounds, false);
    }
    if state.get(STATE) == Some("awake") {
        let clumps = world.rng().gen_range(2..=3);
        for _ in 0..clumps {
            if let Some(at) = spread_resin(world, pos) {
                world.play_sound(SoundEvent::BlockResinPlace, at, 1.0, 1.0, None);
                world.game_event(
                    GameEvent::BlockPlace,
                    at.center(),
                    Context::default().with_block(state),
                );
            }
        }
    }
    let heart = heart(world, pos);
    heart.emitter = HURT_CALL_TICKS;
    heart.emitter_target = Some(center_of(bounds));
}

/// The call's trail: orange from the heart to the protector for its
/// first half, grey back the whole time, and a hurt sound every ten ticks
/// that walks from the protector to the heart.
fn tick_emitter(world: &mut dyn RegionWorld, pos: BlockPos, found: Option<(i32, [f64; 3])>) {
    let emitter = heart(world, pos).emitter;
    if emitter <= 0 {
        return;
    }
    let bounds = protector_bounds(world, found);
    if emitter > HURT_CALL_PARTICLE_TICKS
        && let Some(bounds) = bounds
    {
        emit_particle(world, pos, bounds, true);
        emit_particle(world, pos, bounds, false);
    }
    if emitter % HURT_CALL_INTERVAL == 0
        && let Some(mut target) = heart(world, pos).emitter_target
    {
        if let Some(bounds) = bounds {
            target = center_of(bounds);
            heart(world, pos).emitter_target = Some(target);
        }
        let heart_center = pos.center();
        let progress = 0.2 + 0.8 * (HURT_CALL_TICKS - emitter) as f32 / HURT_CALL_TICKS as f32;
        let at = [0, 1, 2].map(|i| (heart_center[i] - target[i]) * f64::from(progress) + target[i]);
        let volume = emitter as f32 / 2.0 / HURT_CALL_TICKS as f32 + 0.5;
        world.play_sound(
            SoundEvent::BlockCreakingHeartHurt,
            block_of(at),
            volume,
            1.0,
            None,
        );
    }
    heart(world, pos).emitter -= 1;
}

/// Level Particles for one trail particle: it flies from `position` to
/// `target` over `duration` ticks in `color`, shown at any distance.
struct TrailParticle {
    position: [f64; 3],
    target: [f64; 3],
    color: i32,
    duration: i32,
}

impl tuff_core::protocol::Encode for TrailParticle {
    const ID: i32 = tuff_vanilla_data::packet::play::Clientbound::LevelParticles.id();

    fn encode(&self, w: &mut tuff_wire::PacketWriter) {
        w.write_var_int(ParticleType::Trail.id() as i32);
        for c in self.target {
            w.write_double(c);
        }
        w.write_int(self.color);
        w.write_var_int(self.duration);
        w.write_bool(true);
        w.write_bool(true);
        for c in self.position {
            w.write_double(c);
        }
        for _ in 0..6 {
            w.write_float(0.0);
        }
        w.write_var_int(1);
        w.write_var_int(0);
    }
}

fn emit_particle(world: &mut dyn RegionWorld, pos: BlockPos, bounds: [f64; 6], towards: bool) {
    let rng = world.rng();
    let mut source =
        [0, 1, 2].map(|i| bounds[i] + rng.r#gen::<f64>() * (bounds[i + 3] - bounds[i]));
    let corner = [f64::from(pos.x), f64::from(pos.y), f64::from(pos.z)];
    let mut destination = corner.map(|c| c + rng.r#gen::<f64>());
    if towards {
        std::mem::swap(&mut source, &mut destination);
    }
    let duration = rng.gen_range(0..40) + 10;
    let packet = framed(&TrailParticle {
        position: source,
        target: destination,
        color: if towards { ORANGE } else { GRAY },
        duration,
    });
    world.send_to_chunk(ChunkPos::of_block(pos), None, packets([packet]));
}

fn shuffled(world: &mut dyn RegionWorld) -> [Facing; 6] {
    let mut order = DIRECTIONS;
    for i in (2..=order.len()).rev() {
        let to = world.rng().gen_range(0..i);
        order.swap(i - 1, to);
    }
    order
}

fn face_property(face: Facing) -> &'static str {
    match face {
        Facing::Down => "down",
        Facing::Up => "up",
        Facing::North => "north",
        Facing::South => "south",
        Facing::West => "west",
        Facing::East => "east",
    }
}

fn offset(pos: BlockPos, face: Facing) -> BlockPos {
    let (dx, dy, dz) = face.step();
    BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
}

fn is_pale_oak_log(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    world
        .block(pos)
        .is_some_and(|s| tuff_datapack::tags::is(tag::block::key::PALE_OAK_LOGS, s.block()))
}

/// Grows one resin clump face on a pale oak log within two logs of the
/// heart, searching outwards; where it grew, if anywhere.
fn spread_resin(world: &mut dyn RegionWorld, start: BlockPos) -> Option<BlockPos> {
    let mut queue = VecDeque::from([(start, 0u32)]);
    let mut seen = HashSet::new();
    let mut count = 0;
    while let Some((pos, depth)) = queue.pop_front() {
        if !seen.insert(pos) {
            continue;
        }
        if is_pale_oak_log(world, pos) {
            for face in shuffled(world) {
                let at = offset(pos, face);
                let Some(mut state) = world.block(at) else {
                    continue;
                };
                if tuff_world_model::chunk::is_air(state) {
                    state = Block::ResinClump.default_state();
                } else if state.block() == Block::Water && state.fluid_state().is_source() {
                    state = Block::ResinClump
                        .default_state()
                        .with("waterlogged", "true")
                        .unwrap_or(Block::ResinClump.default_state());
                }
                let attach = face_property(face.opposite());
                if state.block() == Block::ResinClump
                    && state.get(attach) != Some("true")
                    && world.owns(at)
                    && let Some(grown) = state.with(attach, "true")
                {
                    world.set_block_flags(at, grown, 3);
                    return Some(at);
                }
            }
        }
        count += 1;
        if count >= RESIN_COUNT {
            return None;
        }
        if depth < RESIN_DEPTH {
            for face in shuffled(world) {
                let next = offset(pos, face);
                if is_pale_oak_log(world, next) {
                    queue.push_back((next, depth + 1));
                }
            }
        }
    }
    None
}

/// A player broke a heart: its protector dies of the player's hand (and
/// twitches before it crumbles), and a natural heart drops experience
/// unless the player is in creative.
pub fn broken(world: &mut dyn RegionWorld, action: &PlayerBlockAction) {
    if action.action != BlockAction::Broke || action.state.block() != Block::CreakingHeart {
        return;
    }
    let player = world
        .seen_player_details()
        .into_iter()
        .find(|p| p.handle.id == action.player);
    kill_protector(
        world,
        action.pos,
        Damage {
            player: Some((action.player, player.as_ref().map_or(0, |p| p.uuid))),
            attacker: Some((action.player, EntityType::Player)),
            position: player.as_ref().map(|p| p.position),
            ..Damage::of(&damage_type::PLAYER_ATTACK)
        },
    );
    let creative = player.as_ref().is_none_or(|p| p.looks.creative);
    if !creative {
        award_experience(world, action.pos, action.state);
    }
}

/// An explosion destroys a heart: its protector dies of the blast, and a
/// player who set it off gets the heart's experience.
pub fn exploded(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    state: BlockState,
    cause: &tuff_core::world::explosion::ExplosionCause,
) {
    if state.block() != Block::CreakingHeart {
        return;
    }
    let igniter = cause.igniter.as_ref().map(|i| (i.id, i.uuid));
    kill_protector(
        world,
        pos,
        Damage {
            player: igniter,
            attacker: cause
                .culprit
                .as_ref()
                .map(|c| (c.id, c.kind))
                .or(igniter.map(|(id, _)| (id, EntityType::Player))),
            position: Some(cause.center),
            ..Damage::of(cause.damage_type)
        },
    );
    let creative = cause.igniter.as_ref().is_some_and(|i| {
        world
            .seen_player_details()
            .into_iter()
            .any(|p| p.handle.id == i.id && p.looks.creative)
    });
    if igniter.is_some() && !creative {
        award_experience(world, pos, state);
    }
}

fn kill_protector(world: &mut dyn RegionWorld, pos: BlockPos, damage: Damage) {
    let protector = hearts(world)
        .runtime
        .get_mut(&pos)
        .and_then(|heart| heart.protector.take());
    let Some(uuid) = protector else {
        return;
    };
    if let Some((id, _)) = find_creaking(world, uuid) {
        if let Some(mob) = creaking_mut(world, id) {
            mob.species.owe_death(damage);
        }
        return;
    }
    on_remote_protector(world, uuid, move |other, id| {
        if let Some(mob) = creaking_mut(other, id) {
            mob.species.owe_death(damage.clone());
        }
    });
}

fn award_experience(world: &mut dyn RegionWorld, pos: BlockPos, state: BlockState) {
    if state.get("natural") != Some("true") {
        return;
    }
    let amount = world.rng().gen_range(BREAK_EXPERIENCE);
    let at = pos.center();
    for value in tuff_core::world::experience::split(amount) {
        let id = world.shared().next_entity_id();
        let nbt = tuff_core::world::experience::spawn_nbt(at, value, world.rng());
        if let Some(orb) = world.shared().entity_types.load(&nbt, id) {
            world.add_entity(orb);
        }
    }
}

fn updated_state(
    world: &dyn RegionWorld,
    state: BlockState,
    pos: BlockPos,
    active: bool,
) -> BlockState {
    let tracking = stored_uuid(world, pos).is_some();
    let value = if !tracking && !has_required_logs(state, pos, |p| world.block(p)) {
        "uprooted"
    } else if active {
        "awake"
    } else {
        "dormant"
    };
    state.with(STATE, value).unwrap_or(state)
}

fn can_spawn_on(below: Option<BlockState>, above: Option<BlockState>) -> bool {
    let (Some(below), Some(above)) = (below, above) else {
        return false;
    };
    above.collision_boxes().is_empty()
        && !tuff_datapack::tags::is(tag::block::key::LEAVES, below.block())
        && below.is_collision_face_full(tuff_vanilla_data::block::Facing::Up)
}

/// Where a protector may stand, searched like the game's mob spawn helper:
/// a random column within 16 blocks, scanning down from 8 above the heart.
fn spawn_spot(world: &mut dyn RegionWorld, heart: BlockPos) -> Option<[f64; 3]> {
    for _ in 0..SPAWN_ATTEMPTS {
        let dx = world.rng().gen_range(-SPAWN_RANGE_XZ..=SPAWN_RANGE_XZ);
        let dz = world.rng().gen_range(-SPAWN_RANGE_XZ..=SPAWN_RANGE_XZ);
        let (x, z) = (heart.x + dx, heart.z + dz);
        let top = heart.y + SPAWN_RANGE_Y;
        let mut above = world.block(BlockPos::new(x, top, z));
        let mut found = None;
        for y in (heart.y - SPAWN_RANGE_Y - 1..top).rev() {
            let current = world.block(BlockPos::new(x, y, z));
            if can_spawn_on(current, above) {
                found = Some(y + 1);
                break;
            }
            above = current;
        }
        let Some(y) = found else {
            continue;
        };
        let at = bottom_center(BlockPos::new(x, y, z));
        if physics::no_collision(spawning::spawn_box(EntityType::Creaking, at), &|p| {
            world.block(p)
        }) {
            return Some(at);
        }
    }
    None
}

fn spawn_protector(world: &mut dyn RegionWorld, heart: BlockPos) {
    let Some(at) = spawn_spot(world, heart) else {
        return;
    };
    let shared = world.shared().clone();
    let mut saved = Compound::new();
    saved.insert("id", Tag::String("minecraft:creaking".into()));
    saved.insert("Pos", Tag::List(at.map(Tag::Double).to_vec()));
    let id = shared.next_entity_id();
    let Some(mut entity) = shared.entity_types.load(&saved, id) else {
        return;
    };
    if !spawning::unobstructed_for(EntityType::Creaking, entity.bounds(), &|p| world.block(p)) {
        return;
    }
    world.finalize_spawn(&mut entity, 0);
    if let Some(mob) = entity.downcast_mut::<Mob<Creaking>>() {
        mob.species.bind(&mut mob.core, heart);
    }
    let uuid = entity.base().uuid;
    world.add_entity(entity);
    set_stored_uuid(world, heart, Some(uuid));
    hearts(world).runtime.entry(heart).or_default().protector = Some(uuid);
    world.game_event(
        GameEvent::EntityPlace,
        at,
        Context::from(Source::entity(id)),
    );
    world.play_sound(
        SoundEvent::EntityCreakingSpawn,
        block_of(at),
        1.0,
        1.0,
        None,
    );
    world.play_sound(SoundEvent::BlockCreakingHeartSpawn, heart, 1.0, 1.0, None);
}

/// What a region knows of hearts and protectors across its border: the
/// heart side mirrors where its protector stands in a neighbour, and the
/// creaking side mirrors whether a neighbour's heart still names it.
#[derive(Default)]
struct Remote {
    /// Protector UUID to its last reported position and the tick of the
    /// report.
    protectors: HashMap<u128, ([f64; 3], u64)>,
    /// A foreign heart to the protector it named, and when it answered.
    homes: HashMap<BlockPos, (Option<u128>, u64)>,
    /// Foreign hearts a creaking here needs an answer about.
    wanted: HashSet<BlockPos>,
    /// Player hits on protectors whose heart is in another region.
    calls: Vec<(BlockPos, u128)>,
    asked: HashMap<u128, u64>,
}

const REMOTE_QUERY_TICKS: u64 = 10;
const REMOTE_KEEP_TICKS: u64 = 40;

fn remote(world: &mut dyn RegionWorld) -> &mut Remote {
    world.state().get::<Remote>()
}

fn neighbours(world: &dyn RegionWorld) -> Vec<tuff_core::world::messages::RegionHandle> {
    let here = world.region_pos();
    let mut found = Vec::new();
    for dx in -1..=1 {
        for dz in -1..=1 {
            if (dx, dz) != (0, 0)
                && let Some(handle) = world.neighbor(tuff_world_model::pos::RegionPos::new(
                    here.x + dx,
                    here.z + dz,
                ))
            {
                found.push(handle);
            }
        }
    }
    found
}

fn owner_of(
    world: &dyn RegionWorld,
    pos: BlockPos,
) -> Option<tuff_core::world::messages::RegionHandle> {
    world.neighbor(ChunkPos::of_block(pos).region())
}

fn run_on(
    handle: &tuff_core::world::messages::RegionHandle,
    task: impl FnOnce(&mut dyn RegionWorld) + Send + 'static,
) {
    handle.internal(tuff_core::world::messages::RegionInternal::Run(
        tuff_core::world::region_api::RegionTask::new(task),
    ));
}

/// Whether the heart at `home`, owned by another region, names `uuid`;
/// `None` until it answers. Asks it again when the answer is stale.
fn foreign_protects(state: &mut RegionState, home: BlockPos, uuid: u128, now: u64) -> Option<bool> {
    let remote = state.get::<Remote>();
    let answer = remote.homes.get(&home).copied();
    if answer.is_none_or(|(_, at)| now.saturating_sub(at) >= REMOTE_QUERY_TICKS) {
        remote.wanted.insert(home);
    }
    answer.map(|(protector, _)| protector == Some(uuid))
}

/// The protector of the heart at `pos` stands in a neighbouring region:
/// where it was last seen there, asking the neighbours again now and
/// then.
fn remote_protector(world: &mut dyn RegionWorld, uuid: u128) -> Option<[f64; 3]> {
    let now = world.tick();
    let seen = remote(world).protectors.get(&uuid).copied();
    let asked = remote(world).asked.get(&uuid).copied();
    if asked.is_none_or(|at| now.saturating_sub(at) >= REMOTE_QUERY_TICKS) {
        remote(world).asked.insert(uuid, now);
        let home = world.handle();
        for handle in neighbours(world) {
            let home = home.clone();
            run_on(&handle, move |other| {
                let Some((_, at)) = find_creaking(other, uuid) else {
                    return;
                };
                let tick = other.tick();
                run_on(&home, move |here| {
                    remote(here).protectors.insert(uuid, (at, tick));
                });
            });
        }
    }
    seen.filter(|(_, at)| now.saturating_sub(*at) <= REMOTE_KEEP_TICKS)
        .map(|(position, _)| position)
}

/// Has a neighbour holding protector `uuid` act on it.
fn on_remote_protector(
    world: &mut dyn RegionWorld,
    uuid: u128,
    act: impl Fn(&mut dyn RegionWorld, i32) + Clone + Send + 'static,
) {
    remote(world).protectors.remove(&uuid);
    for handle in neighbours(world) {
        let act = act.clone();
        run_on(&handle, move |other| {
            if let Some((id, _)) = find_creaking(other, uuid) {
                act(other, id);
            }
        });
    }
}

/// Sends the creakings' questions about foreign hearts and their hits on
/// protectors to the regions owning those hearts.
fn exchange(world: &mut dyn RegionWorld) {
    let wanted: Vec<BlockPos> = remote(world).wanted.drain().collect();
    let calls = std::mem::take(&mut remote(world).calls);
    let home = world.handle();
    for pos in wanted {
        let Some(owner) = owner_of(world, pos) else {
            continue;
        };
        let home = home.clone();
        run_on(&owner, move |other| {
            let named = other
                .block(pos)
                .is_some_and(|s| s.block() == Block::CreakingHeart)
                .then(|| stored_uuid(other, pos))
                .flatten();
            let tick = other.tick();
            run_on(&home, move |here| {
                remote(here).homes.insert(pos, (named, tick));
            });
        });
    }
    for (pos, uuid) in calls {
        if let Some(owner) = owner_of(world, pos) {
            run_on(&owner, move |other| {
                hearts(other).calls.push((pos, uuid));
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn signal_falls_off_over_the_roaming_radius() {
        assert_eq!(signal_for_distance(0.0), 15);
        assert_eq!(signal_for_distance(2.0), 15);
        assert_eq!(signal_for_distance(16.0), 8);
        assert_eq!(signal_for_distance(32.0), 0);
        assert_eq!(signal_for_distance(40.0), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn comparators_read_hearts() {
        for state in Block::CreakingHeart.states() {
            assert!(state.has_analog_output(), "{state:?}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn logs_must_run_along_the_heart_axis() {
        let heart = Block::CreakingHeart
            .default_state()
            .with("axis", "y")
            .unwrap();
        let log_y = Block::PaleOakLog.default_state().with("axis", "y").unwrap();
        let log_x = Block::PaleOakLog.default_state().with("axis", "x").unwrap();
        let origin = BlockPos::new(0, 64, 0);
        assert!(has_required_logs(heart, origin, |_| Some(log_y)));
        assert!(!has_required_logs(heart, origin, |_| Some(log_x)));
        assert!(!has_required_logs(heart, origin, |p| (p.y > 64).then_some(log_y)));
    }
}
