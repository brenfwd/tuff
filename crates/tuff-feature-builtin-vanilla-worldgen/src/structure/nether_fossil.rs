//! Nether fossils: a bone template on the first floor below a random
//! height, with a dried ghast beside it half the time.

use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::worldgen::StructurePiece as PieceType;
use tuff_vanilla_data::structure::Structure;
use tuff_vanilla_data::{Block, Support};

use crate::feature::OwnedHeightProvider;
use crate::json::{Result, obj};
use crate::structure::geometry::{BoundingBox, Rotation};
use crate::structure::processor::{BlockIgnore, ProcessorList};
use crate::structure::ruined_portal::place_template;
use crate::structure::template::{self, PlaceSettings, Template};
use crate::structure::{GenerationContext, GenerationStub, Piece, PieceContext, StructureKind};
use crate::surface::GenContext;
use tuff_item::random::{LegacyRandom, Random, RandomSource};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;
use tuff_world_model::pos::ChunkPos;

struct NetherFossil {
    height: OwnedHeightProvider,
}

pub fn factory(_: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    let o = obj(config, &["height"], "nether fossil")?;
    Ok(Arc::new(NetherFossil {
        height: OwnedHeightProvider::parse(o.req("height")?)?,
    }))
}

impl StructureKind for NetherFossil {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let x = (ctx.chunk.x << 4) + ctx.random.next_int_bounded(16);
        let z = (ctx.chunk.z << 4) + ctx.random.next_int_bounded(16);
        let sea_level = ctx.sea_level();
        let settings = ctx.generator.settings;
        let span = GenContext {
            min_y: settings.min_y.max(ctx.shape.min_y),
            height: settings.height.min(ctx.shape.height),
            sea_level,
        };
        let mut y = self.height.sample(&mut ctx.random, span);
        let (bottom, column) = ctx.base_column(x, z);
        let at = |y: i32| {
            usize::try_from(y - bottom)
                .ok()
                .and_then(|i| column.get(i))
                .copied()
                .unwrap_or(Block::Air.default_state())
        };
        while y > sea_level {
            let current = at(y);
            y -= 1;
            let below = at(y);
            if is_air(current)
                && (below.block() == Block::SoulSand
                    || below.is_face_sturdy(Facing::Up, Support::Full))
            {
                break;
            }
        }
        if y <= sea_level {
            return None;
        }
        let position = BlockPos::new(x, y, z);
        Some(GenerationStub {
            position,
            pieces: Box::new(move |ctx: &mut GenerationContext| {
                let rotation = Rotation::ALL[ctx.random.next_int_bounded(4) as usize];
                let list = tuff_vanilla_data::world::NETHER_FOSSILS;
                let name = list[ctx.random.next_int_bounded(list.len() as i32) as usize];
                vec![Box::new(NetherFossilPiece::new(name, position, rotation)) as Box<dyn Piece>]
            }),
        })
    }
}

pub struct NetherFossilPiece {
    template_name: &'static str,
    template: Arc<Template>,
    position: BlockPos,
    settings: PlaceSettings,
    bounding_box: BoundingBox,
}

impl NetherFossilPiece {
    fn new(template_name: &'static str, position: BlockPos, rotation: Rotation) -> Self {
        let template = template::get(template_name);
        let mut processors = ProcessorList::default();
        processors.push(BlockIgnore::structure_and_air());
        let settings = PlaceSettings {
            rotation,
            processors,
            ..PlaceSettings::default()
        };
        let bounding_box = template.settings_box(&settings, position);
        Self {
            template_name,
            template,
            position,
            settings,
            bounding_box,
        }
    }
}

impl Piece for NetherFossilPiece {
    fn kind(&self) -> PieceType {
        PieceType::Nefos
    }

    fn bounding_box(&self) -> BoundingBox {
        self.bounding_box
    }

    fn orientation(&self) -> Option<Facing> {
        Some(Facing::North)
    }

    fn save_extra(&self, tag: &mut Compound) {
        tag.insert("TPX", self.position.x);
        tag.insert("TPY", self.position.y);
        tag.insert("TPZ", self.position.z);
        tag.insert("Template", self.template_name);
        tag.insert("Rot", self.settings.rotation.name());
    }

    fn place(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &mut BoundingBox,
        _chunk: ChunkPos,
        reference: BlockPos,
    ) {
        let fossil = self.bounding_box;
        chunk_box.encapsulate(&fossil);
        place_template(
            ctx,
            &self.template,
            self.position,
            reference,
            &self.settings,
            *chunk_box,
        );
        let center = fossil.center();
        let mut random = Random::Legacy(LegacyRandom::new(ctx.level.seed()))
            .fork_positional()
            .at(center.x, center.y, center.z);
        if random.next_float() < 0.5 {
            let x = fossil.min_x + random.next_int_bounded(fossil.x_span());
            let z = fossil.min_z + random.next_int_bounded(fossil.z_span());
            let pos = BlockPos::new(x, fossil.min_y, z);
            if is_air(ctx.level.block(pos)) && chunk_box.contains(pos) {
                let rotation = Rotation::ALL[random.next_int_bounded(4) as usize];
                let state = rotation.rotate_state(Block::DriedGhast.default_state());
                ctx.level.set_block(pos, state);
            }
        }
    }
}
