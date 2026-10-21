//! Power sources: torches, levers, buttons, pressure plates, observers
//! and target blocks.

use tuff_vanilla_data::block::{Face, Facing};
use tuff_vanilla_data::redstone::Sensitivity;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState};

use super::{
    BlockBehavior, DIRECTIONS, Effect, RedstoneLevel, SignalBehavior, Signals, TickPriority,
    UPDATE_ALL, UPDATE_CLIENTS, UPDATE_KNOWN_SHAPE,
};
use tuff_vanilla_common::placement::step;
use tuff_world_model::BlockPos;

pub struct Torch;
pub struct Lever;
pub struct Button;
pub struct PressurePlate;
pub struct Observer;
pub struct Target;

/// Ticks between a torch's input changing and the torch following.
const TORCH_DELAY: u32 = 2;
/// A torch toggled this often within [`TORCH_TOGGLE_WINDOW`] burns out.
const TORCH_MAX_TOGGLES: usize = 8;
const TORCH_TOGGLE_WINDOW: u64 = 60;
/// How long a burnt-out torch stays off.
const TORCH_RESTART_DELAY: u32 = 160;
const LEVEL_EVENT_TORCH_BURNOUT: i32 = 1502;
const OBSERVER_DELAY: u32 = 2;
const TARGET_ARROW_TICKS: u32 = 20;
const TARGET_OTHER_TICKS: u32 = 8;
/// The box a pressure plate senses entities in, relative to its block.
const PLATE_TOUCH: [f64; 6] = [
    1.0 / 16.0,
    0.0,
    1.0 / 16.0,
    15.0 / 16.0,
    4.0 / 16.0,
    15.0 / 16.0,
];

fn powered(state: BlockState) -> bool {
    state.powered() == Some(true)
}

fn with_powered(state: BlockState, powered: bool) -> BlockState {
    state.with_powered(powered).unwrap_or(state)
}

fn offset(pos: BlockPos, [x0, y0, z0, x1, y1, z1]: [f64; 6]) -> [f64; 6] {
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    [x + x0, y + y0, z + z0, x + x1, y + y1, z + z1]
}

fn torch_has_signal(s: &Signals, pos: BlockPos, state: BlockState) -> bool {
    let toward = match state.facing() {
        Some(facing) if state.block() == Block::RedstoneWallTorch => facing.opposite(),
        _ => Facing::Down,
    };
    s.has_signal(step(pos, toward), toward)
}

fn notify_around(level: &mut dyn RedstoneLevel, pos: BlockPos, block: Block) {
    for direction in DIRECTIONS {
        level.update_neighbors_at(step(pos, direction), block);
    }
}

/// Records a toggle of the torch at `pos` if `add`, and says whether it
/// has toggled too often lately.
fn toggled_too_often(level: &mut dyn RedstoneLevel, pos: BlockPos, add: bool) -> bool {
    let now = level.game_time();
    let toggles = level.torch_toggles();
    if add {
        toggles.push((pos, now));
    }
    toggles.iter().filter(|(at, _)| *at == pos).count() >= TORCH_MAX_TOGGLES
}

impl BlockBehavior for Torch {
    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _old: BlockState,
        _moved: bool,
    ) {
        notify_around(level, pos, state.block());
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        moved: bool,
    ) {
        if !moved {
            notify_around(level, pos, state.block());
        }
    }

    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        let lit = state.lit() == Some(true);
        if lit == torch_has_signal(&Signals::new(level), pos, state)
            && !level.will_tick_this_tick(pos, state.block())
        {
            level.schedule_tick(pos, state.block(), TORCH_DELAY, TickPriority::Normal);
        }
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        let signal = torch_has_signal(&Signals::new(level), pos, state);
        let now = level.game_time();
        let toggles = level.torch_toggles();
        let expired = toggles
            .iter()
            .take_while(|(_, when)| now.saturating_sub(*when) > TORCH_TOGGLE_WINDOW)
            .count();
        toggles.drain(..expired);
        if state.lit() == Some(true) {
            if signal {
                level.set_block(pos, state.with_lit(false).unwrap_or(state), UPDATE_ALL);
                if toggled_too_often(level, pos, true) {
                    level.effect(Effect::LevelEvent {
                        event: LEVEL_EVENT_TORCH_BURNOUT,
                        pos,
                        data: 0,
                    });
                    let block = level.block(pos).block();
                    level.schedule_tick(pos, block, TORCH_RESTART_DELAY, TickPriority::Normal);
                }
            }
        } else if !signal && !toggled_too_often(level, pos, false) {
            level.set_block(pos, state.with_lit(true).unwrap_or(state), UPDATE_ALL);
        }
    }
}

impl SignalBehavior for Torch {}

/// The direction from a lever or button toward the block it hangs on.
fn attached_toward(state: BlockState) -> Facing {
    match state.face() {
        Some(Face::Floor) => Facing::Down,
        Some(Face::Ceiling) => Facing::Up,
        _ => state.facing().unwrap_or(Facing::North).opposite(),
    }
}

fn update_attached(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let block = state.block();
    level.update_neighbors_at(pos, block);
    level.update_neighbors_at(step(pos, attached_toward(state)), block);
}

impl BlockBehavior for Lever {
    fn exploded(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        self.used(level, pos, state);
    }

    fn used(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) -> bool {
        let next = with_powered(state, !powered(state));
        level.set_block(pos, next, UPDATE_ALL);
        update_attached(level, pos, next);
        level.effect(Effect::Sound {
            sound: SoundEvent::BlockLeverClick,
            pos,
            volume: 0.3,
            pitch: if powered(next) { 0.6 } else { 0.5 },
        });
        true
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        moved: bool,
    ) {
        if !moved && powered(state) {
            update_attached(level, pos, state);
        }
    }
}

impl SignalBehavior for Lever {}

fn button_sound(level: &mut dyn RedstoneLevel, pos: BlockPos, block: Block, pressed: bool) {
    if let Some(button) = block.button() {
        let sound = if pressed {
            button.press_sound
        } else {
            button.release_sound
        };
        level.effect(Effect::Sound {
            sound,
            pos,
            volume: 1.0,
            pitch: 1.0,
        });
    }
}

fn button_ticks(block: Block) -> u32 {
    block.button().map_or(20, |button| button.press_ticks)
}

fn check_button(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let block = state.block();
    let arrow = block.pressed_by_arrows() && {
        let bounds = state.outline_boxes().iter().fold(
            [f64::MAX, f64::MAX, f64::MAX, f64::MIN, f64::MIN, f64::MIN],
            |acc, b| {
                [
                    acc[0].min(f64::from(b[0])),
                    acc[1].min(f64::from(b[1])),
                    acc[2].min(f64::from(b[2])),
                    acc[3].max(f64::from(b[3])),
                    acc[4].max(f64::from(b[4])),
                    acc[5].max(f64::from(b[5])),
                ]
            },
        );
        level.arrow_touching(offset(pos, bounds))
    };
    if arrow != powered(state) {
        level.set_block(pos, with_powered(state, arrow), UPDATE_ALL);
        update_attached(level, pos, state);
        button_sound(level, pos, block, arrow);
    }
    if arrow {
        level.schedule_tick(pos, block, button_ticks(block), TickPriority::Normal);
    }
}

impl BlockBehavior for Button {
    fn senses_entities(&self) -> bool {
        true
    }

    fn exploded(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        self.used(level, pos, state);
    }

    fn used(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) -> bool {
        if powered(state) {
            return true;
        }
        let block = state.block();
        level.set_block(pos, with_powered(state, true), UPDATE_ALL);
        update_attached(level, pos, state);
        level.schedule_tick(pos, block, button_ticks(block), TickPriority::Normal);
        button_sound(level, pos, block, true);
        true
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if powered(state) {
            check_button(level, pos, state);
        }
    }

    fn entity_inside(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if state.block().pressed_by_arrows() && !powered(state) {
            check_button(level, pos, state);
        }
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        moved: bool,
    ) {
        if !moved && powered(state) {
            update_attached(level, pos, state);
        }
    }
}

impl SignalBehavior for Button {}

fn plate_signal(state: BlockState) -> u8 {
    match state.power() {
        Some(power) => power,
        None if powered(state) => 15,
        None => 0,
    }
}

fn with_plate_signal(state: BlockState, signal: u8) -> BlockState {
    match state.power() {
        Some(_) => state.with_power(signal),
        None => state.with_powered(signal > 0),
    }
    .unwrap_or(state)
}

fn plate_strength(level: &dyn RedstoneLevel, pos: BlockPos, state: BlockState) -> u8 {
    let Some(plate) = state.block().pressure_plate() else {
        return 0;
    };
    let area = offset(pos, PLATE_TOUCH);
    match plate.sensitivity {
        Sensitivity::Everything | Sensitivity::Mobs => {
            let living = plate.sensitivity == Sensitivity::Mobs;
            if level.entities_touching(area, living) > 0 {
                15
            } else {
                0
            }
        }
        Sensitivity::Weighted(max) => {
            let count = level.entities_touching(area, false).min(max as usize) as u32;
            if count == 0 {
                0
            } else {
                let share = count.min(max) as f32 / max as f32;
                (share * 15.0).ceil() as u8
            }
        }
    }
}

fn update_plate_neighbours(level: &mut dyn RedstoneLevel, pos: BlockPos, block: Block) {
    level.update_neighbors_at(pos, block);
    level.update_neighbors_at(step(pos, Facing::Down), block);
}

fn check_plate(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState, old: u8) {
    let block = state.block();
    let Some(plate) = block.pressure_plate() else {
        return;
    };
    let signal = plate_strength(level, pos, state);
    let (was, is) = (old > 0, signal > 0);
    if old != signal {
        level.set_block(pos, with_plate_signal(state, signal), UPDATE_CLIENTS);
        update_plate_neighbours(level, pos, block);
    }
    if was != is {
        level.effect(Effect::Sound {
            sound: if is { plate.click_on } else { plate.click_off },
            pos,
            volume: 1.0,
            pitch: 1.0,
        });
    }
    if is {
        level.schedule_tick(pos, block, plate.pressed_ticks, TickPriority::Normal);
    }
}

impl BlockBehavior for PressurePlate {
    fn senses_entities(&self) -> bool {
        true
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        let signal = plate_signal(state);
        if signal > 0 {
            check_plate(level, pos, state, signal);
        }
    }

    fn entity_inside(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        let signal = plate_signal(state);
        if signal == 0 {
            check_plate(level, pos, state, signal);
        }
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        moved: bool,
    ) {
        if !moved && plate_signal(state) > 0 {
            update_plate_neighbours(level, pos, state.block());
        }
    }
}

impl SignalBehavior for PressurePlate {}

fn observer_facing(state: BlockState) -> Facing {
    state.facing().unwrap_or(Facing::South)
}

fn observer_front(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    let facing = observer_facing(state);
    let front = step(pos, facing.opposite());
    level.neighbor_changed(front, state.block());
    level.update_neighbors_except(front, state.block(), Some(facing));
}

impl BlockBehavior for Observer {
    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if powered(state) {
            level.set_block(pos, with_powered(state, false), UPDATE_CLIENTS);
        } else {
            level.set_block(pos, with_powered(state, true), UPDATE_CLIENTS);
            level.schedule_tick(pos, state.block(), OBSERVER_DELAY, TickPriority::Normal);
        }
        observer_front(level, pos, state);
    }

    fn update_shape(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        direction: Facing,
        _neighbour: BlockState,
    ) -> Option<BlockState> {
        if observer_facing(state) == direction
            && !powered(state)
            && !level.has_scheduled_tick(pos, state.block())
        {
            level.schedule_tick(pos, state.block(), OBSERVER_DELAY, TickPriority::Normal);
        }
        None
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
            && powered(state)
            && !level.has_scheduled_tick(pos, state.block())
        {
            let off = with_powered(state, false);
            level.set_block(pos, off, UPDATE_CLIENTS | UPDATE_KNOWN_SHAPE);
            observer_front(level, pos, off);
        }
    }

    fn after_removal(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _moved: bool,
    ) {
        if powered(state) && level.has_scheduled_tick(pos, state.block()) {
            observer_front(level, pos, with_powered(state, false));
        }
    }
}

impl SignalBehavior for Observer {
    fn wire_connects(&self, state: BlockState, direction: Option<Facing>) -> bool {
        direction == Some(observer_facing(state))
    }
}

impl BlockBehavior for Target {
    fn on_place(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        old: BlockState,
        _moved: bool,
    ) {
        if old.block() != state.block()
            && state.power().is_some_and(|power| power > 0)
            && !level.has_scheduled_tick(pos, state.block())
        {
            let off = state.with_power(0).unwrap_or(state);
            level.set_block(pos, off, UPDATE_CLIENTS | UPDATE_KNOWN_SHAPE);
        }
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if state.power().is_some_and(|power| power != 0) {
            level.set_block(pos, state.with_power(0).unwrap_or(state), UPDATE_ALL);
        }
    }

    fn hit_signal(&self, face: Facing, location: [f64; 3], arrow: bool) -> Option<(u8, u32)> {
        let off_centre = |c: f64| (c - c.floor() - 0.5).abs();
        let [dx, dy, dz] = location.map(off_centre);
        let distance = match face.axis() {
            tuff_vanilla_data::block::Axis::Y => dx.max(dz),
            tuff_vanilla_data::block::Axis::Z => dx.max(dy),
            tuff_vanilla_data::block::Axis::X => dy.max(dz),
        };
        let strength = ((15.0 * ((0.5 - distance) / 0.5).clamp(0.0, 1.0)).ceil() as u8).max(1);
        Some((
            strength,
            if arrow {
                TARGET_ARROW_TICKS
            } else {
                TARGET_OTHER_TICKS
            },
        ))
    }
}

impl SignalBehavior for Target {}
