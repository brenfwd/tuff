//! Structures and jigsaw assemblies planned for `/place`, placed one chunk
//! window at a time.

use tuff_core::world::features::{FeatureChanges, FeatureSpan, PlannedStructure};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::pos::ChunkPos;

use crate::feature::host::window_changes;
use crate::structure::geometry::BoundingBox;
use crate::structure::{Piece, PieceContext, StructureStart};
use tuff_item::random::WorldgenRandom;

fn chunk_range(bounds: BoundingBox) -> (ChunkPos, ChunkPos) {
    (
        ChunkPos::new(bounds.min_x >> 4, bounds.min_z >> 4),
        ChunkPos::new(bounds.max_x >> 4, bounds.max_z >> 4),
    )
}

fn chunk_box(center: ChunkPos, span: FeatureSpan) -> BoundingBox {
    BoundingBox::new(
        center.x << 4,
        span.min_y,
        center.z << 4,
        (center.x << 4) + 15,
        span.min_y + span.height,
        (center.z << 4) + 15,
    )
}

pub struct StartPlan(pub StructureStart);

impl PlannedStructure for StartPlan {
    fn chunks(&self) -> (ChunkPos, ChunkPos) {
        chunk_range(self.0.bounding_box())
    }

    fn place_in_window(
        &self,
        center: ChunkPos,
        window: &[(ChunkPos, Chunk)],
        span: FeatureSpan,
        random: &mut WorldgenRandom,
    ) -> Option<FeatureChanges> {
        window_changes(center, window, span, random, true, &mut |level, random| {
            self.0
                .place_in_chunk(level, random, chunk_box(center, span), center);
            true
        })
    }
}

pub struct PiecesPlan {
    pub pieces: Vec<Box<dyn Piece>>,
    pub bounds: BoundingBox,
    pub reference: BlockPos,
}

impl PlannedStructure for PiecesPlan {
    fn chunks(&self) -> (ChunkPos, ChunkPos) {
        chunk_range(self.bounds)
    }

    fn place_in_window(
        &self,
        center: ChunkPos,
        window: &[(ChunkPos, Chunk)],
        span: FeatureSpan,
        random: &mut WorldgenRandom,
    ) -> Option<FeatureChanges> {
        window_changes(center, window, span, random, true, &mut |level, random| {
            let mut bounds = chunk_box(center, span);
            let mut ctx = PieceContext { level, random };
            for piece in &self.pieces {
                if piece.bounding_box().intersects(&bounds) {
                    piece.place(&mut ctx, &mut bounds, center, self.reference);
                }
            }
            true
        })
    }
}
