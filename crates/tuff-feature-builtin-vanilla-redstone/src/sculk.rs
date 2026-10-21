//! Sculk sensors and calibrated sculk sensors: they hear game events as
//! delayed vibrations, switch on for a while with a redstone signal that
//! fades with distance, and click for sculk shriekers nearby.

use rand::Rng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState, tag};

use super::{
    BlockBehavior, Reader, RedstoneLevel, SignalBehavior, Signals, TickPriority, UPDATE_ALL,
};
use tuff_core::world::game_event::{self, Candidate, Context, Emission, GameEvent, Listeners};
use tuff_core::world::region_api::{RegionHooks, RegionWorld, TickPhase};
use tuff_nbt::Compound;
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const SENSOR_RANGE: i32 = 8;
const CALIBRATED_RANGE: i32 = 16;
const ACTIVE_TICKS: u32 = 30;
const CALIBRATED_ACTIVE_TICKS: u32 = 10;
const COOLDOWN_TICKS: u32 = 10;
const RESCAN_INTERVAL: u64 = 20;
const NOTES: [u8; 16] = [0, 0, 2, 4, 6, 7, 9, 10, 12, 14, 15, 18, 19, 21, 22, 24];

pub const BLOCKS: [Block; 2] = [Block::SculkSensor, Block::CalibratedSculkSensor];

#[derive(Default)]
struct Pending(Listeners);

pub fn is_sensor(nbt: &Compound) -> bool {
    matches!(
        nbt.get_str("id"),
        Some("minecraft:sculk_sensor" | "minecraft:calibrated_sculk_sensor")
    )
}

fn phase(state: BlockState) -> &'static str {
    state.get("sculk_sensor_phase").unwrap_or("inactive")
}

fn can_activate(state: BlockState) -> bool {
    phase(state) == "inactive"
}

fn with_phase(state: BlockState, phase: &str, power: u8) -> BlockState {
    let state = state.with("sculk_sensor_phase", phase).unwrap_or(state);
    state.with("power", &power.to_string()).unwrap_or(state)
}

fn range(block: Block) -> i32 {
    if block == Block::CalibratedSculkSensor {
        CALIBRATED_RANGE
    } else {
        SENSOR_RANGE
    }
}

fn active_ticks(block: Block) -> u32 {
    if block == Block::CalibratedSculkSensor {
        CALIBRATED_ACTIVE_TICKS
    } else {
        ACTIVE_TICKS
    }
}

fn update_neighbours(level: &mut dyn RedstoneLevel, pos: BlockPos, block: Block) {
    level.update_neighbors_at(pos, block);
    level.update_neighbors_at(BlockPos::new(pos.x, pos.y - 1, pos.z), block);
}

pub struct Sensor;

impl BlockBehavior for Sensor {
    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        match phase(state) {
            "active" => {
                level.set_block(pos, with_phase(state, "cooldown", 0), UPDATE_ALL);
                level.schedule_tick(pos, state.block(), COOLDOWN_TICKS, TickPriority::Normal);
                update_neighbours(level, pos, state.block());
            }
            "cooldown" => {
                level.set_block(pos, with_phase(state, "inactive", 0), UPDATE_ALL);
                if state.get("waterlogged") != Some("true") {
                    level.effect(super::Effect::Sound {
                        sound: SoundEvent::BlockSculkSensorClickingStop,
                        pos,
                        volume: 1.0,
                        pitch: 0.8,
                    });
                }
            }
            _ => {}
        }
    }

    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() != state.block()
            && state.power().unwrap_or(0) > 0
            && !level.has_scheduled_tick(pos, state.block())
        {
            level.set_block(pos, with_phase(state, phase(state), 0), 18);
        }
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _moved: bool,
    ) {
        if phase(state) == "active" {
            update_neighbours(level, pos, state.block());
        }
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for Sensor {
    fn signal(&self, _s: &Signals, _pos: BlockPos, state: BlockState, direction: Facing) -> u8 {
        if state.block() == Block::CalibratedSculkSensor && state.facing() == Some(direction) {
            return 0;
        }
        state.power().unwrap_or(0)
    }

    fn direct_signal(
        &self,
        s: &Signals,
        pos: BlockPos,
        state: BlockState,
        direction: Facing,
    ) -> u8 {
        if direction == Facing::Up {
            self.signal(s, pos, state, direction)
        } else {
            0
        }
    }

    fn wire_connects(&self, _state: BlockState, direction: Option<Facing>) -> bool {
        direction.is_some()
    }
}

struct Adapter<'a>(&'a dyn RegionWorld);

impl Reader for Adapter<'_> {
    fn block(&self, pos: BlockPos) -> BlockState {
        self.0.block(pos).unwrap_or(Block::Air.default_state())
    }

    fn comparator_output(&self, pos: BlockPos) -> u8 {
        self.0.comparator_output(pos)
    }

    fn analog_output(&self, _pos: BlockPos, _state: BlockState, _direction: Facing) -> u8 {
        0
    }
}

fn back_signal(world: &dyn RegionWorld, pos: BlockPos, state: BlockState) -> u8 {
    let Some(facing) = state.facing() else {
        return 0;
    };
    let back = facing.opposite();
    let adapter = Adapter(world);
    Signals::new(&adapter).signal(step(pos, back), back)
}

fn hears(world: &dyn RegionWorld, pos: BlockPos, state: BlockState, e: &Emission) -> bool {
    let origin = e.block_pos();
    if (origin == pos && matches!(e.event, GameEvent::BlockDestroy | GameEvent::BlockPlace))
        || game_event::frequency(e.event) == 0
        || !can_activate(state)
    {
        return false;
    }
    if state.block() == Block::CalibratedSculkSensor {
        let wanted = back_signal(world, pos, state);
        if wanted != 0 && game_event::frequency(e.event) != wanted {
            return false;
        }
    }
    true
}

fn distance_sq(a: BlockPos, b: BlockPos) -> i64 {
    let (dx, dy, dz) = (
        i64::from(a.x - b.x),
        i64::from(a.y - b.y),
        i64::from(a.z - b.z),
    );
    dx * dx + dy * dy + dz * dz
}

fn pitch(frequency: u8) -> f32 {
    let note = f32::from(NOTES[usize::from(frequency.min(15))]);
    2f32.powf((note - 12.0) / 12.0)
}

fn activate(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    state: BlockState,
    power: u8,
    frequency: u8,
    source: Option<game_event::Source>,
) {
    let block = state.block();
    world.set_block_flags(pos, with_phase(state, "active", power), UPDATE_ALL);
    world.schedule_tick(pos, block, active_ticks(block));
    world.update_neighbors_at(pos, block);
    world.update_neighbors_at(BlockPos::new(pos.x, pos.y - 1, pos.z), block);
    for direction in super::DIRECTIONS {
        let at = step(pos, direction);
        let Some(neighbour) = world.block(at) else {
            continue;
        };
        if !tuff_datapack::tags::is(tag::block::key::VIBRATION_RESONATORS, neighbour.block()) {
            continue;
        }
        if let Some(event) = game_event::resonance(frequency) {
            let context = Context {
                source,
                affected: Some(neighbour),
            };
            world.game_event(event, at.center(), context);
        }
        world.play_sound(
            SoundEvent::BlockAmethystBlockResonate,
            at,
            1.0,
            pitch(frequency),
            None,
        );
    }
    world.game_event(
        GameEvent::SculkSensorTendrilsClicking,
        pos.center(),
        Context {
            source,
            affected: None,
        },
    );
    if state.get("waterlogged") != Some("true") {
        let jitter = 0.8 + world.rng().r#gen::<f32>() * 0.2;
        world.play_sound(SoundEvent::BlockSculkSensorClicking, pos, 1.0, jitter, None);
    }
}

fn receive(world: &mut dyn RegionWorld, pos: BlockPos, vibration: &Candidate) {
    let Some(state) = world.block(pos) else {
        return;
    };
    if !BLOCKS.contains(&state.block()) || !can_activate(state) {
        return;
    }
    let frequency = game_event::frequency(vibration.event);
    if let Some(nbt) = world.raw_block_entity_mut(pos) {
        nbt.insert("last_vibration_frequency", i32::from(frequency));
    }
    world.block_entity_changed(pos);
    let origin = game_event::floor_pos(vibration.origin);
    let distance = (distance_sq(origin, pos) as f32).sqrt();
    let power = game_event::redstone_strength(distance, range(state.block()));
    activate(world, pos, state, power, frequency, vibration.source);
}

pub struct SculkHooks;

impl RegionHooks for SculkHooks {
    fn game_event(&self, world: &mut dyn RegionWorld, e: &Emission) {
        if !tuff_datapack::tags::is(tag::game_event::key::VIBRATIONS, e.event) {
            return;
        }
        let sensors =
            world.raw_block_entities_near(e.block_pos(), game_event::radius(e.event), &is_sensor);
        if sensors.is_empty() {
            return;
        }
        let tick = world.tick();
        for pos in sensors {
            let Some(state) = world.block(pos) else {
                continue;
            };
            let radius = range(state.block());
            if !e.reaches(pos, radius) {
                continue;
            }
            if world
                .state()
                .get::<Pending>()
                .0
                .get(&pos)
                .is_some_and(|l| l.is_busy())
            {
                continue;
            }
            let mut avoided = None;
            if !game_event::audible_checking(tag::game_event::key::VIBRATIONS, e, |source| {
                avoided = Some(*source);
            }) {
                if let Some(source) = avoided {
                    game_event::report_avoided(world, &source);
                }
                continue;
            }
            if !hears(world, pos, state, e) {
                continue;
            }
            let blocks = |p: BlockPos| world.block(p);
            if game_event::occluded(&blocks, e.pos, pos.center()) {
                continue;
            }
            let candidate = Candidate {
                event: e.event,
                distance: (distance_sq(e.block_pos(), pos) as f32).sqrt(),
                origin: e.pos,
                source: e.context.source,
            };
            world
                .state()
                .get::<Pending>()
                .0
                .entry(pos)
                .or_default()
                .offer(candidate, tick);
        }
    }

    fn tick(&self, world: &mut dyn RegionWorld, phase: TickPhase) {
        if phase != TickPhase::Late {
            return;
        }
        let tick = world.tick();
        if tick.is_multiple_of(RESCAN_INTERVAL) {
            game_event::adopt_saved(world, |w| &mut w.state().get::<Pending>().0, is_sensor);
        }
        let mut positions: Vec<BlockPos> =
            world.state().get::<Pending>().0.keys().copied().collect();
        if positions.is_empty() {
            return;
        }
        positions.sort_unstable_by_key(|p| (p.y, p.z, p.x));
        let loaded = world.lit_ready_chunks();
        for pos in positions {
            let is_sensor_here = world.raw_block_entity(pos).is_some_and(is_sensor);
            let arrived = if is_sensor_here {
                let ready = loaded.contains(&ChunkPos::of_block(pos));
                world
                    .state()
                    .get::<Pending>()
                    .0
                    .get_mut(&pos)
                    .and_then(|l| l.advance(tick, |d| d.floor() as i32, || ready))
            } else {
                None
            };
            let idle = !is_sensor_here
                || world
                    .state()
                    .get::<Pending>()
                    .0
                    .get(&pos)
                    .is_none_or(|l| l.is_idle());
            if is_sensor_here {
                let listener = world.state().get::<Pending>().0.get(&pos).cloned();
                game_event::store(world, pos, listener.as_ref());
            }
            if idle {
                world.state().get::<Pending>().0.remove(&pos);
            }
            if let Some(vibration) = arrived {
                receive(world, pos, &vibration);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn resonance_pitch_spans_two_octaves() {
        assert!((pitch(0) - 0.5).abs() < 1e-6);
        assert!((pitch(8) - 1.0).abs() < 1e-6);
        assert!((pitch(15) - 2.0).abs() < 1e-6);
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_inactive_sensors_can_activate() {
        let sensor = Block::SculkSensor.default_state();
        assert!(can_activate(sensor));
        assert!(!can_activate(with_phase(sensor, "active", 7)));
        assert!(!can_activate(with_phase(sensor, "cooldown", 0)));
        assert_eq!(with_phase(sensor, "active", 7).power(), Some(7));
    }
}
