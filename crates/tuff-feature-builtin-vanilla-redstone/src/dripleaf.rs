//! Big dripleaves: an entity standing on one tips it over step by step, a
//! projectile tips it fully at once, and it springs back after a while or
//! as soon as it is powered.

use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{Block, BlockState};

use super::{
    BlockBehavior, Effect, RedstoneLevel, SignalBehavior, Signals, TickPriority, UPDATE_CLIENTS,
};
use tuff_core::world::game_event::GameEvent;
use tuff_world_model::BlockPos;

pub struct BigDripleaf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tilt {
    None,
    Unstable,
    Partial,
    Full,
}

impl Tilt {
    fn of(state: BlockState) -> Self {
        match state.get("tilt") {
            Some("unstable") => Self::Unstable,
            Some("partial") => Self::Partial,
            Some("full") => Self::Full,
            _ => Self::None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Unstable => "unstable",
            Self::Partial => "partial",
            Self::Full => "full",
        }
    }

    fn delay(self) -> Option<u32> {
        match self {
            Self::None => None,
            Self::Unstable | Self::Partial => Some(10),
            Self::Full => Some(100),
        }
    }

    fn vibrates(self) -> bool {
        self != Self::Unstable
    }
}

fn play(level: &mut dyn RedstoneLevel, pos: BlockPos, sound: SoundEvent) {
    let pitch = level.random_float() * 0.4 + 0.8;
    level.effect(Effect::Sound {
        sound,
        pos,
        volume: 1.0,
        pitch,
    });
}

fn set_tilt(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState, tilt: Tilt) {
    let Some(tilted) = state.with("tilt", tilt.name()) else {
        return;
    };
    level.set_block(pos, tilted, UPDATE_CLIENTS);
    if tilt.vibrates() && tilt != Tilt::of(state) {
        level.effect(Effect::GameEvent(GameEvent::BlockChange, pos));
    }
}

fn tilt_and_schedule(
    level: &mut dyn RedstoneLevel,
    pos: BlockPos,
    state: BlockState,
    tilt: Tilt,
    sound: Option<SoundEvent>,
) {
    set_tilt(level, pos, state, tilt);
    if let Some(sound) = sound {
        play(level, pos, sound);
    }
    if let Some(delay) = tilt.delay() {
        level.schedule_tick(pos, Block::BigDripleaf, delay, TickPriority::Normal);
    }
}

fn reset(level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
    set_tilt(level, pos, state, Tilt::None);
    if Tilt::of(state) != Tilt::None {
        play(level, pos, SoundEvent::BlockBigDripleafTiltUp);
    }
}

fn powered(level: &dyn RedstoneLevel, pos: BlockPos) -> bool {
    Signals::new(level).has_neighbor_signal(pos)
}

impl BlockBehavior for BigDripleaf {
    fn neighbor_changed(
        &self,
        level: &mut dyn RedstoneLevel,
        pos: BlockPos,
        state: BlockState,
        _source: Block,
    ) {
        if powered(level, pos) {
            reset(level, pos, state);
        }
    }

    fn tick(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if powered(level, pos) {
            reset(level, pos, state);
            return;
        }
        let down = Some(SoundEvent::BlockBigDripleafTiltDown);
        match Tilt::of(state) {
            Tilt::Unstable => tilt_and_schedule(level, pos, state, Tilt::Partial, down),
            Tilt::Partial => tilt_and_schedule(level, pos, state, Tilt::Full, down),
            Tilt::Full => reset(level, pos, state),
            Tilt::None => {}
        }
    }

    fn projectile_hit(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        let down = Some(SoundEvent::BlockBigDripleafTiltDown);
        tilt_and_schedule(level, pos, state, Tilt::Full, down);
    }

    fn senses_entities(&self) -> bool {
        true
    }

    fn entity_inside(&self, level: &mut dyn RedstoneLevel, pos: BlockPos, state: BlockState) {
        if Tilt::of(state) != Tilt::None || powered(level, pos) {
            return;
        }
        let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        let top = y + 15.0 / 16.0;
        let standing = [x, top, z, x + 1.0, top + 1.0e-3, z + 1.0];
        if level.entities_touching(standing, false) > 0 {
            tilt_and_schedule(level, pos, state, Tilt::Unstable, None);
        }
    }

    fn is_active(&self) -> bool {
        true
    }
}

impl SignalBehavior for BigDripleaf {}
