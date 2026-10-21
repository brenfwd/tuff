//! A creative-mode miner: it settles on the ground under it and digs a
//! square pit one layer at a time, breaking a fixed number of blocks per
//! tick and sinking as each layer clears. At the pit's floor limit it
//! moves over to start a fresh pit next to the last one.

use tuff_core::protocol::play::{PlayerActionStatus, Serverbound};
use tuff_world_model::BlockPos;

use crate::BotState;
use crate::goal::{Goal, Step, look_at};

const EYE_HEIGHT: f64 = 1.62;
const FACE_UP: i8 = 1;

pub struct Miner {
    pub blocks_per_tick: u32,
    /// The pit is `2 * half_width + 1` blocks square.
    pub half_width: i32,
    /// The lowest layer dug before moving on.
    pub bottom: i32,
    site: Option<Site>,
    teleports: u64,
    sequence: i32,
    swing: bool,
}

#[derive(Debug, Clone, Copy)]
struct Site {
    x: i32,
    z: i32,
    layer: i32,
    next: i32,
}

impl Miner {
    pub fn new(blocks_per_tick: u32, half_width: i32, bottom: i32) -> Self {
        Self {
            blocks_per_tick,
            half_width,
            bottom,
            site: None,
            teleports: 0,
            sequence: 0,
            swing: false,
        }
    }

    fn width(&self) -> i32 {
        2 * self.half_width + 1
    }

    fn settle(&self, state: &BotState, x: i32, z: i32) -> Option<Site> {
        let floor = state.terrain.as_ref()?.column(x, z)?.floor;
        (floor > f32::MIN).then(|| Site {
            x,
            z,
            layer: floor.ceil() as i32 - 1,
            next: 0,
        })
    }
}

impl Goal for Miner {
    fn tick(&mut self, state: &BotState) -> Step {
        if state.teleports != self.teleports {
            self.teleports = state.teleports;
            self.site = None;
        }
        let site = match self.site {
            Some(site) => site,
            None => {
                let [x, _, z] = state.position;
                match self.settle(state, x.floor() as i32, z.floor() as i32) {
                    Some(site) => site,
                    None => return Step::default(),
                }
            }
        };
        let mut site = site;
        let feet = [
            f64::from(site.x) + 0.5,
            f64::from(site.layer + 1),
            f64::from(site.z) + 0.5,
        ];
        let eye = [feet[0], feet[1] + EYE_HEIGHT, feet[2]];
        let mut step = Step {
            position: Some(feet),
            ..Step::default()
        };
        let width = self.width();
        for _ in 0..self.blocks_per_tick {
            let (dx, dz) = (
                site.next % width - self.half_width,
                site.next / width - self.half_width,
            );
            let target = BlockPos::new(site.x + dx, site.layer, site.z + dz);
            step.rotation = Some(look_at(
                eye,
                [
                    f64::from(target.x) + 0.5,
                    f64::from(target.y) + 1.0,
                    f64::from(target.z) + 0.5,
                ],
            ));
            self.sequence += 1;
            step.actions.push(Serverbound::PlayerAction {
                status: PlayerActionStatus::StartDestroyBlock,
                position: target,
                face: FACE_UP,
                sequence: self.sequence,
            });
            site.next += 1;
            if site.next == width * width {
                site.next = 0;
                site.layer -= 1;
                if site.layer < self.bottom {
                    let moved = site.x + width + 1;
                    self.site = None;
                    step.position = Some([f64::from(moved) + 0.5, feet[1], feet[2]]);
                    return step;
                }
            }
        }
        self.swing = !self.swing;
        if self.swing {
            step.actions.push(Serverbound::Punch);
        }
        self.site = Some(site);
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Terrain;
    use tuff_vanilla_data::Block;
    use tuff_wire::PacketReader;
    use tuff_world_model::chunk::{Chunk, ChunkShape};
    use tuff_world_model::pos::ChunkPos;

    fn ground(floor_y: i32) -> BotState {
        let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
        for x in 0..16 {
            for z in 0..16 {
                chunk.set(x, floor_y - 1, z, Block::Stone.default_state());
            }
        }
        let mut r = PacketReader::new(chunk.encode(0, 0, &[]));
        r.read_int().unwrap();
        r.read_int().unwrap();
        let mut terrain = Terrain::new(-64);
        terrain.read_chunk(ChunkPos::new(0, 0), &mut r).unwrap();
        BotState {
            position: [8.5, 120.0, 8.5],
            teleports: 1,
            terrain: Some(terrain),
            ..BotState::default()
        }
    }

    fn breaks(step: &Step) -> Vec<BlockPos> {
        step.actions
            .iter()
            .filter_map(|action| match action {
                Serverbound::PlayerAction { position, .. } => Some(*position),
                _ => None,
            })
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn digs_the_whole_layer_then_sinks() {
        let state = ground(64);
        let mut miner = Miner::new(3, 1, -60);
        let first = miner.tick(&state);
        assert_eq!(first.position, Some([8.5, 64.0, 8.5]));
        assert_eq!(
            breaks(&first),
            [
                BlockPos::new(7, 63, 7),
                BlockPos::new(8, 63, 7),
                BlockPos::new(9, 63, 7)
            ]
        );
        miner.tick(&state);
        let third = miner.tick(&state);
        assert_eq!(breaks(&third).last(), Some(&BlockPos::new(9, 63, 9)));
        let fourth = miner.tick(&state);
        assert_eq!(fourth.position, Some([8.5, 63.0, 8.5]));
        assert_eq!(breaks(&fourth)[0], BlockPos::new(7, 62, 7));
    }

    #[tuff_core::watchdog]
    #[test]
    fn waits_for_ground_and_moves_on_at_the_bottom() {
        let mut waiting = BotState {
            terrain: Some(Terrain::new(-64)),
            ..BotState::default()
        };
        let mut miner = Miner::new(9, 1, 63);
        assert_eq!(miner.tick(&waiting), Step::default());
        waiting = ground(64);
        let moved = miner.tick(&waiting);
        assert_eq!(breaks(&moved).len(), 9);
        assert_eq!(moved.position, Some([12.5, 64.0, 8.5]));
    }
}
