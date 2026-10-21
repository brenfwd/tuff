//! The game's heightmap kinds and which blocks each counts.

use tuff_vanilla_data::tag::block::key;
use tuff_vanilla_data::{Block, BlockState};

use tuff_world_model::chunk::is_air;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeightmapKind {
    WorldSurfaceWg,
    WorldSurface,
    OceanFloorWg,
    OceanFloor,
    MotionBlocking,
    MotionBlockingNoLeaves,
}

impl HeightmapKind {
    pub const WORLDGEN: [HeightmapKind; 2] =
        [HeightmapKind::WorldSurfaceWg, HeightmapKind::OceanFloorWg];
    pub const FINAL: [HeightmapKind; 4] = [
        HeightmapKind::OceanFloor,
        HeightmapKind::WorldSurface,
        HeightmapKind::MotionBlocking,
        HeightmapKind::MotionBlockingNoLeaves,
    ];

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "WORLD_SURFACE_WG" => HeightmapKind::WorldSurfaceWg,
            "WORLD_SURFACE" => HeightmapKind::WorldSurface,
            "OCEAN_FLOOR_WG" => HeightmapKind::OceanFloorWg,
            "OCEAN_FLOOR" => HeightmapKind::OceanFloor,
            "MOTION_BLOCKING" => HeightmapKind::MotionBlocking,
            "MOTION_BLOCKING_NO_LEAVES" => HeightmapKind::MotionBlockingNoLeaves,
            _ => return None,
        })
    }

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn is_opaque(self, state: BlockState) -> bool {
        let motion = |state: BlockState| {
            tuff_datapack::tags::is(key::BLOCKS_MOTION_IN_HEIGHTMAP, state.block())
        };
        let no_leaves = |state: BlockState| {
            tuff_datapack::tags::is(key::BLOCKS_MOTION_IN_HEIGHTMAP_NO_LEAVES, state.block())
        };
        match self {
            HeightmapKind::WorldSurfaceWg | HeightmapKind::WorldSurface => !is_air(state),
            HeightmapKind::OceanFloorWg | HeightmapKind::OceanFloor => motion(state),
            HeightmapKind::MotionBlocking => motion(state) || has_fluid(state),
            HeightmapKind::MotionBlockingNoLeaves => no_leaves(state) || has_fluid(state),
        }
    }
}

pub fn has_fluid(state: BlockState) -> bool {
    matches!(
        state.block(),
        Block::Water | Block::Lava | Block::BubbleColumn
    ) || state.get("waterlogged") == Some("true")
}
