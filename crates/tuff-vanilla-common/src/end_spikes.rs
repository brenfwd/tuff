//! The end's fixed structures: the obsidian spikes around the main island
//! and the floating islands, as plain block lists.

use tuff_vanilla_data::{Block, BlockState};

use tuff_item::random::{LegacyRandom, Random, RandomSource};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

pub type SpikeBlocks = (Vec<(BlockPos, BlockState)>, [(BlockPos, BlockState); 2]);

/// One obsidian spike of the dragon fight: where it stands, how wide and
/// tall it is, and whether iron bars cage its crystal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndSpike {
    pub center_x: i32,
    pub center_z: i32,
    pub radius: i32,
    pub height: i32,
    pub guarded: bool,
}

impl EndSpike {
    /// The block the spike's end crystal occupies (on bedrock, in fire).
    pub fn crystal(&self) -> BlockPos {
        BlockPos::new(self.center_x, self.height + 1, self.center_z)
    }
}

const SPIKE_COUNT: usize = 10;
const SPIKE_RING_RADIUS: f64 = 42.0;

/// The ten spikes a world seed places around the main island.
pub fn spikes_for_seed(seed: i64) -> Vec<EndSpike> {
    let key = LegacyRandom::new(seed).next_long() & 0xFFFF;
    let mut random = Random::Legacy(LegacyRandom::new(key));
    let mut order: [i32; SPIKE_COUNT] = std::array::from_fn(|n| n as i32);
    for len in (2..=SPIKE_COUNT).rev() {
        let pick = random.next_int_bounded(len as i32) as usize;
        order.swap(len - 1, pick);
    }
    order
        .iter()
        .enumerate()
        .map(|(slot, &rank)| {
            let theta = 2.0 * (0.3141592653589793 * slot as f64 - std::f64::consts::PI);
            EndSpike {
                center_x: (SPIKE_RING_RADIUS * theta.cos()).floor() as i32,
                center_z: (SPIKE_RING_RADIUS * theta.sin()).floor() as i32,
                radius: 2 + rank / 3,
                height: 76 + 3 * rank,
                guarded: matches!(rank, 1 | 2),
            }
        })
        .collect()
}

/// The saved form of the end crystal a spike carries.
pub fn spike_crystal(
    spike: &EndSpike,
    yaw: f32,
    invulnerable: bool,
    beam_target: Option<[i32; 3]>,
) -> Compound {
    let position = [
        f64::from(spike.center_x) + 0.5,
        f64::from(spike.height + 1),
        f64::from(spike.center_z) + 0.5,
    ];
    let mut crystal = Compound::new()
        .with("id", "minecraft:end_crystal")
        .with("Pos", Tag::List(position.map(Tag::Double).to_vec()))
        .with(
            "Rotation",
            Tag::List(vec![Tag::Float(yaw), Tag::Float(0.0)]),
        )
        .with("ShowBottom", true);
    if invulnerable {
        crystal.insert("Invulnerable", true);
    }
    if let Some(target) = beam_target {
        crystal.insert("beam_target", Tag::IntArray(target.to_vec()));
    }
    crystal
}

const CLEAR_ABOVE_Y: i32 = 65;
const CAGE_HALF: i32 = 2;
const CAGE_HEIGHT: i32 = 3;

fn bars(connections: [bool; 4]) -> BlockState {
    ["north", "south", "west", "east"]
        .into_iter()
        .zip(connections)
        .fold(Block::IronBars.default_state(), |state, (side, on)| {
            state
                .with(side, if on { "true" } else { "false" })
                .unwrap_or(state)
        })
}

/// Iron bars on the shell of a 5x4x5 box over the spike top: walls connect
/// along their own face, the roof connects every way except off its rim.
fn cage(spike: &EndSpike, out: &mut Vec<(BlockPos, BlockState)>) {
    for ox in -CAGE_HALF..=CAGE_HALF {
        let on_x_wall = ox.abs() == CAGE_HALF;
        for oz in -CAGE_HALF..=CAGE_HALF {
            let on_z_wall = oz.abs() == CAGE_HALF;
            for level in 0..=CAGE_HEIGHT {
                let roof = level == CAGE_HEIGHT;
                if !(on_x_wall || on_z_wall || roof) {
                    continue;
                }
                let runs_z = on_x_wall || roof;
                let runs_x = on_z_wall || roof;
                let state = bars([
                    runs_z && oz != -CAGE_HALF,
                    runs_z && oz != CAGE_HALF,
                    runs_x && ox != -CAGE_HALF,
                    runs_x && ox != CAGE_HALF,
                ]);
                out.push((
                    BlockPos::new(
                        spike.center_x + ox,
                        spike.height + level,
                        spike.center_z + oz,
                    ),
                    state,
                ));
            }
        }
    }
}

/// The blocks of a spike in the order the game sets them: the obsidian
/// column clearing the air around it, the cage of a guarded one, and then
/// (after its crystal is added) the bedrock and fire under the crystal.
pub fn spike_blocks(spike: &EndSpike, min_y: i32) -> SpikeBlocks {
    let reach = spike.radius;
    let limit = i64::from(reach * reach + 1);
    let obsidian = Block::Obsidian.default_state();
    let air = Block::Air.default_state();
    let top = spike.height + 10;
    let mut out = Vec::new();
    for oz in -reach..=reach {
        let z = spike.center_z + oz;
        for y in min_y..=top {
            let solid_layer = y < spike.height;
            let clear_layer = y > CLEAR_ABOVE_Y;
            if !solid_layer && !clear_layer {
                continue;
            }
            for ox in -reach..=reach {
                let filled = solid_layer && i64::from(ox * ox + oz * oz) <= limit;
                if filled || clear_layer {
                    let pos = BlockPos::new(spike.center_x + ox, y, z);
                    out.push((pos, if filled { obsidian } else { air }));
                }
            }
        }
    }
    if spike.guarded {
        cage(spike, &mut out);
    }
    let crystal = spike.crystal();
    let base = BlockPos::new(crystal.x, crystal.y - 1, crystal.z);
    (
        out,
        [
            (base, Block::Bedrock.default_state()),
            (crystal, Block::Fire.default_state()),
        ],
    )
}

/// The end stone of a small floating island hanging below `origin`, in the
/// order the game places it: shrinking discs, one per layer downwards.
pub fn island_blocks(random: &mut impl RandomSource, origin: BlockPos) -> Vec<BlockPos> {
    let mut out = Vec::new();
    let mut radius = 4.0 + random.next_int_bounded(3) as f32;
    let mut layer = origin.y;
    while radius > 0.5 {
        let low = (-radius).floor() as i32;
        let high = radius.ceil() as i32;
        let bound = (radius + 1.0) * (radius + 1.0);
        for x in low..=high {
            for z in low..=high {
                if ((x * x + z * z) as f32) <= bound {
                    out.push(BlockPos::new(origin.x + x, layer, origin.z + z));
                }
            }
        }
        radius -= 0.5 + random.next_int_bounded(2) as f32;
        layer -= 1;
    }
    out
}

/// The exit portal around `origin`: a bedrock rim and floor under end
/// portal blocks (air while inactive), air above, and the bedrock pillar
/// with its four torches. The flag marks blocks an active portal breaks
/// (with drops) what was there before placing.
pub fn podium_blocks(origin: BlockPos, active: bool) -> Vec<(BlockPos, BlockState, bool)> {
    let bedrock = Block::Bedrock.default_state();
    let mut blocks = Vec::new();
    for z in origin.z - 4..=origin.z + 4 {
        for y in origin.y - 1..=origin.y + 32 {
            for x in origin.x - 4..=origin.x + 4 {
                let pos = BlockPos::new(x, y, z);
                let inside_rim = pos.closer_than(origin, 2.5);
                if !inside_rim && !pos.closer_than(origin, 3.5) {
                    continue;
                }
                let (state, replaces) = if y < origin.y {
                    if inside_rim {
                        (bedrock, false)
                    } else {
                        (Block::EndStone.default_state(), active)
                    }
                } else if y > origin.y {
                    (Block::Air.default_state(), active)
                } else if !inside_rim {
                    (bedrock, false)
                } else if active {
                    (Block::EndPortal.default_state(), true)
                } else {
                    (Block::Air.default_state(), false)
                };
                blocks.push((pos, state, replaces));
            }
        }
    }
    for dy in 0..4 {
        blocks.push((
            BlockPos::new(origin.x, origin.y + dy, origin.z),
            bedrock,
            false,
        ));
    }
    for (facing, dx, dz) in [
        ("north", 0, -1),
        ("south", 0, 1),
        ("west", -1, 0),
        ("east", 1, 0),
    ] {
        let torch = Block::WallTorch
            .default_state()
            .with("facing", facing)
            .expect("wall torches face every way");
        blocks.push((
            BlockPos::new(origin.x + dx, origin.y + 2, origin.z + dz),
            torch,
            false,
        ));
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv(text: &str) -> u64 {
        text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn spike_and_island_output_is_pinned() {
        let mut text = String::new();
        for seed in [0_i64, 1, -7, 123_456_789, i64::MIN] {
            for spike in spikes_for_seed(seed) {
                text.push_str(&format!("{spike:?}{:?}", spike_blocks(&spike, 0)));
            }
            let mut random = Random::Legacy(LegacyRandom::new(seed));
            for _ in 0..4 {
                text.push_str(&format!(
                    "{:?}",
                    island_blocks(&mut random, BlockPos::new(3, 60, -9))
                ));
            }
        }
        assert_eq!(fnv(&text), 8_919_453_450_260_993_114);
    }
}
