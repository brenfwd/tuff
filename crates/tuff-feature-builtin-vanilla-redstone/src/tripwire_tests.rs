use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Instant;

use super::*;
use crate::Effect;

struct Trace {
    blocks: HashMap<BlockPos, BlockState>,
    log: String,
    seed: u32,
    touching: bool,
}

impl Reader for Trace {
    fn block(&self, pos: BlockPos) -> BlockState {
        self.blocks
            .get(&pos)
            .copied()
            .unwrap_or_else(|| Block::Air.default_state())
    }
    fn comparator_output(&self, _pos: BlockPos) -> u8 {
        0
    }
    fn analog_output(&self, _pos: BlockPos, _state: BlockState, _direction: Facing) -> u8 {
        0
    }
}

impl RedstoneLevel for Trace {
    fn set_block(&mut self, pos: BlockPos, state: BlockState, flags: u32) -> bool {
        let _ = write!(self.log, "S{pos:?}{state:?}{flags};");
        self.blocks.insert(pos, state) != Some(state)
    }
    fn neighbor_changed(&mut self, _: BlockPos, _: Block) {}
    fn update_neighbors_except(&mut self, pos: BlockPos, source: Block, skip: Option<Facing>) {
        let _ = write!(self.log, "N{pos:?}{source:?}{skip:?};");
    }
    fn shape_update(&mut self, _: Facing, _: BlockPos, _: BlockPos, _: BlockState, _: u32, _: u32) {
    }
    fn schedule_tick(&mut self, pos: BlockPos, block: Block, delay: u32, _: TickPriority) {
        let _ = write!(self.log, "T{pos:?}{block:?}{delay};");
    }
    fn has_scheduled_tick(&self, _: BlockPos, _: Block) -> bool {
        false
    }
    fn will_tick_this_tick(&self, _: BlockPos, _: Block) -> bool {
        false
    }
    fn set_comparator_output(&mut self, _: BlockPos, _: u8) {}
    fn game_time(&self) -> u64 {
        0
    }
    fn random_float(&mut self) -> f32 {
        self.seed = self.seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        (self.seed >> 8) as f32 / (1u32 << 24) as f32
    }
    fn torch_toggles(&mut self) -> &mut Vec<(BlockPos, u64)> {
        unreachable!()
    }
    fn entities_touching(&self, _: [f64; 6], _: bool) -> usize {
        usize::from(self.touching)
    }
    fn arrow_touching(&self, _: [f64; 6]) -> bool {
        false
    }
    fn effect(&mut self, effect: Effect) {
        let _ = write!(self.log, "E{effect:?};");
    }
}

struct Dice(u64);

impl Dice {
    fn roll(&mut self, sides: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % sides
    }
    fn coin(&mut self) -> bool {
        self.roll(2) == 1
    }
}

fn hook(dice: &mut Dice, facing: Facing) -> BlockState {
    let s = Block::TripwireHook.default_state();
    let s = s.with_facing(facing).unwrap_or(s);
    let s = s.with_attached(dice.coin()).unwrap_or(s);
    s.with_powered(dice.coin()).unwrap_or(s)
}

fn wire(dice: &mut Dice) -> BlockState {
    let s = Block::Tripwire.default_state();
    let s = s.with_attached(dice.coin()).unwrap_or(s);
    let s = s.with_powered(dice.roll(4) == 0).unwrap_or(s);
    s.with("disarmed", if dice.roll(5) == 0 { "true" } else { "false" })
        .unwrap_or(s)
}

fn build(dice: &mut Dice, facing: Facing) -> (Trace, BlockPos, i32) {
    let origin = BlockPos::new(0, 64, 0);
    let mut level = Trace {
        blocks: HashMap::new(),
        log: String::new(),
        seed: dice.roll(1 << 30) as u32,
        touching: dice.coin(),
    };
    let len = 2 + dice.roll(45) as i32;
    level.blocks.insert(origin, hook(dice, facing));
    for i in 1..len {
        let state = match dice.roll(24) {
            0 => Block::Stone.default_state(),
            1 => Block::Air.default_state(),
            2 => hook(dice, facing),
            _ => wire(dice),
        };
        level.blocks.insert(offset(origin, facing, i), state);
    }
    let far_facing = if dice.roll(6) == 0 {
        facing
    } else {
        facing.opposite()
    };
    level
        .blocks
        .insert(offset(origin, facing, len), hook(dice, far_facing));
    (level, origin, len)
}

fn scenario(dice: &mut Dice) -> String {
    let facing = HORIZONTAL[dice.roll(4) as usize];
    let (mut level, origin, len) = build(dice, facing);
    let state = level.block(origin);
    match dice.roll(6) {
        0 => Hook.placed(&mut level, origin, state),
        1 => Hook.tick(&mut level, origin, state),
        2 => {
            level.blocks.insert(origin, Block::Air.default_state());
            Hook.after_removal(&mut level, origin, state, false);
        }
        3 => {
            let at = offset(origin, facing, 1 + dice.roll(len as u64) as i32);
            let s = level.block(at);
            level.blocks.insert(at, Block::Air.default_state());
            Wire.after_removal(&mut level, at, s, false);
        }
        4 => {
            let at = offset(origin, facing, 1 + dice.roll(len as u64) as i32);
            let s = level.block(at);
            Wire.tick(&mut level, at, s);
            Wire.entity_inside(&mut level, at, s);
        }
        _ => {
            let at = offset(origin, facing, 1 + dice.roll(len as u64) as i32);
            let w = wire(dice);
            let old = level.block(at);
            level.blocks.insert(at, w);
            Wire.on_place(&mut level, at, w, old, false);
        }
    }
    let mut finals: Vec<_> = level.blocks.into_iter().collect();
    finals.sort_by_key(|(p, _)| (p.x, p.y, p.z));
    let _ = write!(level.log, "{finals:?}");
    level.log
}

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[tuff_core::watchdog]
#[test]
fn hook_spans_trace_matches_the_pinned_fingerprint() {
    let mut dice = Dice(0x9e37_79b9_7f4a_7c15);
    let digest = (0..4000).fold(0u64, |acc, _| {
        acc.wrapping_mul(31).wrapping_add(fnv(&scenario(&mut dice)))
    });
    assert_eq!(digest, PINNED);
}

const PINNED: u64 = 604_660_622_737_787_486;

#[tuff_core::watchdog]
#[test]
#[ignore = "timing run"]
fn hook_span_timing() {
    let mut dice = Dice(0x1234_5678_9abc_def1);
    let mut levels: Vec<_> = (0..512)
        .map(|_| {
            let facing = HORIZONTAL[dice.roll(4) as usize];
            build(&mut dice, facing)
        })
        .collect();
    let start = Instant::now();
    for _ in 0..200 {
        for (level, origin, _) in &mut levels {
            let state = level.block(*origin);
            calculate(level, *origin, state, false, true, None);
            level.log.clear();
        }
    }
    eprintln!("TIMING_NS {}", start.elapsed().as_nanos());
}
