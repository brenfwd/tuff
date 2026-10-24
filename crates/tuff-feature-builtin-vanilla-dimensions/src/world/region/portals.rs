//! Nether portal searches, portal building and the end platform, run in
//! the region that holds the blocks once they are loaded.

use tokio::sync::oneshot;
use tuff_core::world::block_ledger::ClaimReason;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use crate::world::portal::{self, Axis, PortalLevel, PortalRectangle};
use tuff_core::protocol::play;
use tuff_core::world::messages::RegionRequest;
use tuff_core::world::messages::{ActionOutcome, HandEffect, Place};
use tuff_core::world::region_api::{RegionHooks, RegionWorld};
use tuff_vanilla_common::placement;
use tuff_vanilla_data::Item;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{Chunk, is_air};
use tuff_world_model::pos::ChunkPos;

/// A portal question for one region.
pub enum PortalQuery {
    /// The portal rectangle around the portal block at `pos`.
    Shape {
        pos: BlockPos,
        reply: oneshot::Sender<Option<PortalRectangle>>,
    },
    /// Nether portal blocks within `radius` of `origin` in this region.
    Find {
        origin: BlockPos,
        radius: i32,
        reply: oneshot::Sender<Vec<BlockPos>>,
    },
    /// Builds an exit portal near `origin`.
    Create {
        origin: BlockPos,
        axis: Axis,
        reply: oneshot::Sender<Option<PortalRectangle>>,
    },
    /// Copies of this region's loaded chunks between `min` and `max`, for
    /// a portal built from every region's blocks.
    Chunks {
        min: BlockPos,
        max: BlockPos,
        reply: oneshot::Sender<Vec<(ChunkPos, Chunk)>>,
    },
    /// Writes this region's part of a built portal: frames with updates,
    /// portal blocks without.
    Build {
        writes: Vec<(BlockPos, BlockState)>,
        reply: oneshot::Sender<()>,
    },
    /// Where a body of `size` (width, height) arriving with its feet at
    /// `bottom` stands clear of blocks.
    FreePosition {
        bottom: [f64; 3],
        size: [f64; 2],
        reply: oneshot::Sender<[f64; 3]>,
    },
    /// Rebuilds this region's part of the end's obsidian platform.
    EndPlatform {
        origin: BlockPos,
        reply: oneshot::Sender<()>,
    },
}

impl PortalQuery {
    fn area(&self) -> (BlockPos, BlockPos) {
        match self {
            Self::Shape { pos, .. } => (*pos, *pos),
            Self::Find { origin, radius, .. } => portal::search_bounds(*origin, *radius),
            Self::Create { origin, .. } => portal::create_bounds(*origin),
            Self::Chunks { min, max, .. } => (*min, *max),
            Self::Build { writes, .. } => {
                writes
                    .iter()
                    .fold((writes[0].0, writes[0].0), |(lo, hi), (p, _)| {
                        (
                            BlockPos::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)),
                            BlockPos::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)),
                        )
                    })
            }
            Self::FreePosition { bottom, size, .. } => {
                portal::collision_free_bounds(*bottom, *size)
            }
            Self::EndPlatform { origin, .. } => (
                BlockPos::new(origin.x - 2, origin.y, origin.z - 2),
                BlockPos::new(origin.x + 2, origin.y, origin.z + 2),
            ),
        }
    }

    /// The request that answers this once its blocks are loaded.
    pub fn request(self) -> RegionRequest {
        let (min, max) = self.area();
        RegionRequest::WhenLoaded {
            min,
            max,
            run: tuff_core::world::region_api::RegionTask::new(move |world| answer(world, self)),
        }
    }
}

fn answer(world: &mut dyn RegionWorld, query: PortalQuery) {
    let air = Block::Air.default_state();
    match query {
        PortalQuery::Shape { pos, reply } => {
            let level = |p: BlockPos| world.block(p).unwrap_or(air);
            let _ = reply.send(portal::portal_rectangle(&level, pos));
        }
        PortalQuery::Find {
            origin,
            radius,
            reply,
        } => {
            let _ = reply.send(find_portal_blocks(world, origin, radius));
        }
        PortalQuery::Create {
            origin,
            axis,
            reply,
        } => {
            let mut level = WorldLevel {
                world: &*world,
                writes: Vec::new(),
            };
            let rectangle = portal::create_portal(&mut level, origin, axis);
            for (pos, state) in level.writes {
                if state.block() == Block::NetherPortal {
                    world.set_block_anywhere(pos, state);
                } else {
                    world.put_block(pos, state);
                }
            }
            let _ = reply.send(rectangle);
        }
        PortalQuery::Chunks { min, max, reply } => {
            let chunks = world
                .loaded_chunks()
                .into_iter()
                .filter(|c| {
                    (min.x >> 4..=max.x >> 4).contains(&c.x)
                        && (min.z >> 4..=max.z >> 4).contains(&c.z)
                })
                .filter_map(|c| Some((c, world.chunk(c)?.clone())))
                .collect();
            let _ = reply.send(chunks);
        }
        PortalQuery::Build { writes, reply } => {
            for (pos, state) in writes {
                if state.block() == Block::NetherPortal {
                    world.set_block_anywhere(pos, state);
                } else {
                    world.put_block(pos, state);
                }
            }
            let _ = reply.send(());
        }
        PortalQuery::FreePosition {
            bottom,
            size,
            reply,
        } => {
            let _ = reply.send(portal::collision_free(bottom, size, &|p| world.block(p)));
        }
        PortalQuery::EndPlatform { origin, reply } => {
            for (pos, state) in portal::end_platform(origin) {
                let Some(old) = world.block(pos) else {
                    continue;
                };
                if old == state || !world.owns(pos) {
                    continue;
                }
                if let Some(broken) = world.claim_block(pos, ClaimReason::Converted) {
                    let loot = world.shared().block_loot();
                    let drops = loot.drops(broken, None, world.rng());
                    world.drop_items(pos, drops);
                }
                world.put_block(pos, state);
            }
            let _ = reply.send(());
        }
    }
}

fn find_portal_blocks(world: &dyn RegionWorld, origin: BlockPos, radius: i32) -> Vec<BlockPos> {
    let dimension = world.dimension_info();
    let mut found = Vec::new();
    for pos in world.loaded_chunks() {
        if (pos.x * 16 + 15) < origin.x - radius
            || pos.x * 16 > origin.x + radius
            || (pos.z * 16 + 15) < origin.z - radius
            || pos.z * 16 > origin.z + radius
        {
            continue;
        }
        let Some(chunk) = world.chunk(pos) else {
            continue;
        };
        let shape = chunk.shape();
        for (index, section) in chunk.sections().enumerate() {
            if !section
                .palette()
                .iter()
                .any(|s| s.block() == Block::NetherPortal)
            {
                continue;
            }
            let base = shape.section_base(index);
            for i in 0..tuff_world_model::chunk::SECTION_VOLUME {
                if section.get(i).block() != Block::NetherPortal {
                    continue;
                }
                let at = BlockPos::new(
                    pos.x * 16 + (i & 15) as i32,
                    base + (i >> 8) as i32,
                    pos.z * 16 + ((i >> 4) & 15) as i32,
                );
                if (at.x - origin.x).abs() <= radius
                    && (at.z - origin.z).abs() <= radius
                    && dimension.contains_block(at)
                {
                    found.push(at);
                }
            }
        }
    }
    found
}

/// A region as [`portal::create_portal`] sees it: reads come from the
/// world, writes are collected and applied afterwards.
struct WorldLevel<'a> {
    world: &'a dyn RegionWorld,
    writes: Vec<(BlockPos, BlockState)>,
}

impl PortalLevel for WorldLevel<'_> {
    fn get(&self, pos: BlockPos) -> Option<BlockState> {
        self.writes
            .iter()
            .rev()
            .find(|(at, _)| *at == pos)
            .map(|(_, state)| *state)
            .or_else(|| self.world.block(pos))
    }

    fn set(&mut self, pos: BlockPos, state: BlockState) {
        self.writes.push((pos, state));
    }

    fn motion_blocking_height(&self, x: i32, z: i32) -> Option<i32> {
        self.world.motion_blocking_height(x, z)
    }

    fn min_y(&self) -> i32 {
        self.world.dimension_info().shape.min_y
    }

    fn logical_max_y(&self) -> i32 {
        self.world.dimension_info().logical_max_y()
    }

    fn inside_border(&self, pos: BlockPos) -> bool {
        self.world.dimension_info().contains_block(pos)
    }

    fn clamp_to_border(&self, pos: BlockPos) -> BlockPos {
        self.world
            .dimension_info()
            .clamp_to_border(pos.x as f64, pos.y, pos.z as f64)
    }
}

/// Lighting nether portals and filling end portal frames.
pub struct PortalBlockHooks;

impl RegionHooks for PortalBlockHooks {
    fn light_portal(
        &self,
        world: &mut dyn RegionWorld,
        pos: BlockPos,
        from: Option<Facing>,
    ) -> bool {
        if world.dimension_info().portal_link.is_none() {
            return false;
        }
        let min_y = world.dimension_info().shape.min_y;
        let air = Block::Air.default_state();
        let shape = {
            let level = |p: BlockPos| world.block(p).unwrap_or(air);
            portal::find_empty_shape(&level, pos, min_y, Axis::X).filter(|_| {
                from.is_none_or(|facing| portal::lights_portal(&level, pos, min_y, facing))
            })
        };
        let Some(shape) = shape else {
            return false;
        };
        for (pos, state) in shape.portal_blocks() {
            world.set_block_anywhere(pos, state);
        }
        true
    }

    fn use_item_on(
        &self,
        world: &mut dyn RegionWorld,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        let item = place.held?;
        let pos = place.target;
        let outcome = ActionOutcome::Placed {
            off_hand: place.off_hand,
            item,
            placement: None,
        };
        let air = Block::Air.default_state();
        match item {
            crate::dragon::crystal::ITEM => {
                crate::dragon::crystal::place(world, pos, state)?;
                Some(ActionOutcome::Used {
                    off_hand: place.off_hand,
                    item,
                    effect: HandEffect::Consume(1),
                })
            }
            Item::FlintAndSteel | Item::FireCharge => {
                let at = pos.offset(place.face);
                if !world.owns(at) || !world.block(at).is_some_and(is_air) {
                    return None;
                }
                let facing = placement::from_yaw(place.yaw);
                let min_y = world.dimension_info().shape.min_y;
                let level = |p: BlockPos| world.block(p).unwrap_or(air);
                let portal = world
                    .dimension_info()
                    .portal_link
                    .is_some()
                    .then(|| portal::find_empty_shape(&level, at, min_y, Axis::X))
                    .flatten()
                    .filter(|_| portal::lights_portal(&level, at, min_y, facing));
                let fire = placement::fire_state(at, &level);
                if portal.is_none() && !placement::can_survive(fire, at, &level) {
                    return None;
                }
                let (sound, pitch) = world.fire_sound(item);
                let except = (item == Item::FlintAndSteel).then_some(place.player.id);
                world.play_sound(sound, at, 1.0, pitch, except);
                match portal {
                    Some(shape) => {
                        for (pos, state) in shape.portal_blocks() {
                            world.set_block_anywhere(pos, state);
                        }
                    }
                    None => world.put_block(at, fire),
                }
                Some(outcome)
            }
            Item::EnderEye => {
                if state.block() != Block::EndPortalFrame || state.get("eye") == Some("true") {
                    return None;
                }
                let filled = state.with("eye", "true")?;
                world.put_block(pos, filled);
                world.level_event(play::level_event::END_PORTAL_FRAME_FILL, pos, 0);
                let level = |p: BlockPos| world.block(p).unwrap_or(air);
                if let Some(center) = portal::end_portal_center(&level, pos) {
                    for at in portal::end_portal_blocks(center) {
                        if let Some(broken) = world.claim_block(at, ClaimReason::Converted) {
                            let drops = {
                                let loot = world.shared().block_loot();
                                loot.drops(broken, None, world.rng())
                            };
                            world.drop_items(at, drops);
                        }
                        world.set_block_anywhere(at, Block::EndPortal.default_state());
                    }
                    world.broadcast_global_level_event(
                        play::level_event::SOUND_END_PORTAL_SPAWN,
                        center,
                        0,
                    );
                }
                Some(outcome)
            }
            _ => None,
        }
    }
}
