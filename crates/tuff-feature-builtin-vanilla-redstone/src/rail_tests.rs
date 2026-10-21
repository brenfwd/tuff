use std::collections::HashMap;
use std::time::Instant;

use super::*;
use crate::{Effect, TickPriority};

struct Grid {
    blocks: HashMap<BlockPos, BlockState>,
    nudges: u64,
    torches: Vec<(BlockPos, u64)>,
}

impl Reader for Grid {
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

impl RedstoneLevel for Grid {
    fn set_block(&mut self, pos: BlockPos, state: BlockState, _flags: u32) -> bool {
        self.blocks.insert(pos, state) != Some(state)
    }
    fn neighbor_changed(&mut self, pos: BlockPos, _source: Block) {
        self.nudges = self
            .nudges
            .wrapping_mul(31)
            .wrapping_add((pos.x as u64) ^ ((pos.y as u64) << 20) ^ ((pos.z as u64) << 40));
    }
    fn update_neighbors_except(&mut self, _: BlockPos, _: Block, _: Option<Facing>) {}
    fn shape_update(&mut self, _: Facing, _: BlockPos, _: BlockPos, _: BlockState, _: u32, _: u32) {
    }
    fn schedule_tick(&mut self, _: BlockPos, _: Block, _: u32, _: TickPriority) {}
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
        0.0
    }
    fn torch_toggles(&mut self) -> &mut Vec<(BlockPos, u64)> {
        &mut self.torches
    }
    fn entities_touching(&self, _: [f64; 6], _: bool) -> usize {
        0
    }
    fn arrow_touching(&self, _: [f64; 6]) -> bool {
        false
    }
    fn effect(&mut self, _: Effect) {}
}

struct Dice(u64);

impl Dice {
    fn roll(&mut self, sides: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % sides
    }
}

const KINDS: [Block; 4] = [
    Block::Rail,
    Block::Rail,
    Block::PoweredRail,
    Block::DetectorRail,
];

/// Lays rails one by one over a random field of steps and power blocks
/// and returns a digest of the final blocks and the neighbour updates.
fn lay_track(seed: u64, size: i32) -> u64 {
    let mut dice = Dice(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut grid = Grid {
        blocks: HashMap::new(),
        nudges: 0,
        torches: Vec::new(),
    };
    let stone = Block::Stone.default_state();
    for x in 0..size {
        for z in 0..size {
            grid.blocks.insert(BlockPos::new(x, 0, z), stone);
            match dice.roll(10) {
                0 | 1 => {
                    grid.blocks.insert(BlockPos::new(x, 1, z), stone);
                }
                2 => {
                    grid.blocks
                        .insert(BlockPos::new(x, 1, z), Block::RedstoneBlock.default_state());
                }
                _ => {}
            }
        }
    }
    for _ in 0..size * size {
        let (x, z) = (dice.roll(size as u64) as i32, dice.roll(size as u64) as i32);
        let floor = BlockPos::new(x, 1, z);
        let pos = if grid.block(floor).block() == Block::Stone {
            above(floor)
        } else if grid.block(floor).block() == Block::Air {
            floor
        } else {
            continue;
        };
        if grid.block(pos).block() != Block::Air {
            continue;
        }
        let state = KINDS[dice.roll(4) as usize].default_state();
        grid.blocks.insert(pos, state);
        update_placed(&mut grid, pos, state);
    }
    let mut cells: Vec<_> = grid.blocks.iter().map(|(p, s)| (*p, *s)).collect();
    cells.sort_by_key(|(p, _)| (p.x, p.y, p.z));
    let mut digest = 0xcbf2_9ce4_8422_2325_u64;
    let mut mix = |word: u64| digest = (digest ^ word).wrapping_mul(0x0100_0000_01b3);
    for (pos, state) in cells {
        mix(pos.x as u64);
        mix(pos.y as u64);
        mix(pos.z as u64);
        mix(u64::from(state.id()));
    }
    mix(grid.nudges);
    digest
}

#[tuff_core::watchdog]
#[test]
fn laid_track_matches_the_pinned_layouts() {
    let digests: Vec<u64> = (0..12).map(|seed| lay_track(seed, 12)).collect();
    assert_eq!(
        digests,
        [
            15182440541544815398,
            10127923366856628496,
            12355679453195067408,
            657882486713045804,
            6900695715725518121,
            6406476137805973341,
            3757705321238541660,
            18023055548255138133,
            1887773386073790111,
            7466270770902421062,
            16909393254068096116,
            11165900102955682706,
        ]
    );
}

#[tuff_core::watchdog]
#[test]
#[ignore = "timing run: cargo test --release -p tuff-feature-builtin-vanilla-redstone track_laying_timing -- --ignored --nocapture"]
fn track_laying_timing() {
    let start = Instant::now();
    let mut sink = 0u64;
    for seed in 0..400 {
        sink ^= lay_track(seed, 24);
    }
    println!("track_laying_timing: {:?} ({sink})", start.elapsed());
}
