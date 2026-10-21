//! Growing features in a live world on behalf of block behaviours.

use std::sync::Arc;

use tuff_core::world::features::{
    CapturedArea, FeatureChanges, FeatureHost, FeatureRef, FeatureRegion, FeatureSpan,
    PlaceRequest, SculkCursor, SculkEvent, TemplateExtent, TemplateRequest,
};
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_item::random::WorldgenRandom;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::{Chunk, SECTION_VOLUME};
use tuff_world_model::pos::ChunkPos;

use super::level::Region;
use super::multiface::Spreader;
use super::sculk::{Params, Spreaders, update_cursor};
use super::{PlaceContext, live_registry};
use crate::proto::ProtoChunk;
use crate::structure::geometry::{Mirror, Rotation};
use crate::structure::processor::{BlockRot, ProcessorList};
use crate::structure::template::{self, PlaceRandom, PlaceSettings};
use crate::surface::GenContext;
use tuff_item::random::{LegacyRandom, RandomSource};

pub struct Features;

impl FeatureRegion for Region<'_> {
    fn block(&self, pos: BlockPos) -> BlockState {
        Region::block(self, pos)
    }

    fn set_block(&mut self, pos: BlockPos, state: BlockState) -> bool {
        Region::set_block(self, pos, state)
    }

    fn biome(&mut self, pos: BlockPos) -> u16 {
        Region::biome(self, pos)
    }

    fn is_outside_build_height(&self, y: i32) -> bool {
        Region::is_outside_build_height(self, y)
    }

    fn place_configured(&mut self, name: &str, random: &mut WorldgenRandom, pos: BlockPos) -> bool {
        let feature = live_registry()
            .lock()
            .unwrap()
            .feature(&serde_json::Value::String(name.to_string()));
        let Ok(feature) = feature else {
            return false;
        };
        let mut ctx = PlaceContext {
            level: self,
            random,
        };
        feature.place(&mut ctx, pos)
    }

    fn place_placed(&mut self, name: &str, random: &mut WorldgenRandom, pos: BlockPos) -> bool {
        let Ok(feature) = self.placed_feature(name) else {
            return false;
        };
        let mut ctx = PlaceContext {
            level: self,
            random,
        };
        feature.place(&mut ctx, pos, None)
    }

    fn multiface_can_spread(&self, state: BlockState, pos: BlockPos, face: Facing) -> bool {
        Spreader::of(state.block()).can_spread_in_any_direction(self, state, pos, face)
    }

    fn multiface_spread(&mut self, random: &mut WorldgenRandom, state: BlockState, pos: BlockPos) {
        let mut ctx = PlaceContext {
            level: self,
            random,
        };
        Spreader::of(state.block()).spread_from_random_face(&mut ctx, state, pos);
    }

    fn sculk_update(
        &mut self,
        cursors: &mut [SculkCursor],
        origin: BlockPos,
        random: &mut WorldgenRandom,
        events: &mut Vec<SculkEvent>,
    ) {
        let spreaders = Spreaders::new();
        for cursor in cursors {
            update_cursor(
                cursor,
                self,
                &spreaders,
                Params::LEVEL,
                origin,
                random,
                true,
                events,
            );
        }
    }
}

/// Runs `grow` on a copy of the chunk window around `center` and reports
/// what it changed. With `center_only`, changes outside the centre chunk
/// are dropped (they belong to the windows of the chunks around it).
pub(crate) fn window_changes(
    center: ChunkPos,
    window: &[(ChunkPos, Chunk)],
    span: FeatureSpan,
    random: &mut WorldgenRandom,
    center_only: bool,
    grow: &mut dyn FnMut(&mut Region, &mut WorldgenRandom) -> bool,
) -> Option<FeatureChanges> {
    let shape = window.first()?.1.shape();
    let protos = window
        .iter()
        .map(|(pos, chunk)| ProtoChunk::from_chunk(*pos, chunk))
        .collect();
    let gen_span = GenContext {
        min_y: span.min_y,
        height: span.height,
        sea_level: span.sea_level,
    };
    let mut region = Region::live(center, protos, gen_span, span.seed, span.seed);
    if !grow(&mut region, random) {
        return None;
    }
    let mut changes = FeatureChanges::default();
    for (proto, (_, original)) in region.into_chunks().iter().zip(window) {
        if center_only && proto.pos != center {
            continue;
        }
        for (&(x, y, z), data) in &proto.block_entities {
            let mut data = data.clone();
            for key in ["id", "x", "y", "z"] {
                data.remove(key);
            }
            changes.block_entities.push((BlockPos::new(x, y, z), data));
        }
        changes.entities.extend(proto.entities.iter().cloned());
        let (cx, cz) = (proto.pos.x << 4, proto.pos.z << 4);
        for (index, section) in original.sections().enumerate() {
            let base = shape.section_base(index);
            for i in 0..SECTION_VOLUME {
                let (x, y, z) = (
                    (i & 15) as i32,
                    base + (i >> 8) as i32,
                    (i >> 4 & 15) as i32,
                );
                let state = proto.get(x, y, z);
                if section.get(i) != state {
                    changes
                        .blocks
                        .push((BlockPos::new(cx + x, y, cz + z), state));
                }
            }
        }
    }
    changes.blocks.sort_by_key(|(pos, _)| (pos.y, pos.x, pos.z));
    Some(changes)
}

fn rotation_of(turns: u8) -> Rotation {
    Rotation::ALL[usize::from(turns) % 4]
}

fn mirror_of(mirror: u8) -> Mirror {
    match mirror {
        1 => Mirror::LeftRight,
        2 => Mirror::FrontBack,
        _ => Mirror::None,
    }
}

fn place_template(
    region: &mut Region,
    request: &TemplateRequest,
    random: &mut WorldgenRandom,
) -> bool {
    let template = template::get(&request.name);
    let mut settings = PlaceSettings {
        mirror: mirror_of(request.mirror),
        rotation: rotation_of(request.rotation),
        known_shape: request.strict,
        ignore_entities: request.ignore_entities,
        ..PlaceSettings::default()
    };
    if request.integrity < 1.0 {
        settings.processors = ProcessorList::default();
        settings.processors.push(BlockRot::new(request.integrity));
    }
    let mut seeded;
    let source: &mut dyn RandomSource = if request.seed == 0 {
        random
    } else {
        seeded = LegacyRandom::new(request.seed);
        &mut seeded
    };
    let mut place_random = PlaceRandom::shared(source);
    template.place(
        region,
        request.pos,
        request.pos,
        &settings,
        &mut place_random,
    )
}

impl FeatureHost for Features {
    fn with_window(
        &self,
        center: ChunkPos,
        window: &[(ChunkPos, Chunk)],
        span: FeatureSpan,
        random: &mut WorldgenRandom,
        grow: &mut dyn FnMut(&mut dyn FeatureRegion, &mut WorldgenRandom) -> bool,
    ) -> Option<FeatureChanges> {
        window_changes(
            center,
            window,
            span,
            random,
            false,
            &mut |region, random| grow(region, random),
        )
    }

    fn place(
        &self,
        request: &PlaceRequest,
        center: ChunkPos,
        window: &[(ChunkPos, Chunk)],
        span: FeatureSpan,
        random: &mut WorldgenRandom,
    ) -> Option<FeatureChanges> {
        match request {
            PlaceRequest::Feature { feature, pos } => {
                let value = match feature {
                    FeatureRef::Named(name) => serde_json::Value::String(name.clone()),
                    FeatureRef::Inline(value) => value.clone(),
                };
                let feature = live_registry().lock().unwrap().feature(&value).ok()?;
                window_changes(
                    center,
                    window,
                    span,
                    random,
                    false,
                    &mut |region, random| {
                        let mut ctx = PlaceContext {
                            level: region,
                            random,
                        };
                        feature.place(&mut ctx, *pos)
                    },
                )
            }
            PlaceRequest::Template(template) => {
                window_changes(center, window, span, random, true, &mut |region, random| {
                    place_template(region, template, random)
                })
            }
        }
    }

    fn template_extent(&self, request: &TemplateRequest) -> Option<TemplateExtent> {
        let qualified = crate::json::qualify(&request.name);
        let template = template::find(&qualified)?;
        let settings = PlaceSettings {
            mirror: mirror_of(request.mirror),
            rotation: rotation_of(request.rotation),
            ..PlaceSettings::default()
        };
        let pos = request.pos;
        let [x, y, z] = template.size(Rotation::None);
        let far = BlockPos::new(pos.x + x, pos.y + y, pos.z + z);
        let bounds = template.settings_box(&settings, pos);
        Some(TemplateExtent {
            declared: (ChunkPos::of_block(pos), ChunkPos::of_block(far)),
            placed: (
                ChunkPos::new(bounds.min_x >> 4, bounds.min_z >> 4),
                ChunkPos::new(bounds.max_x >> 4, bounds.max_z >> 4),
            ),
        })
    }

    fn template_size(&self, name: &str) -> Option<[i32; 3]> {
        template::find(name).map(|template| template.size)
    }

    fn save_template(&self, name: &str, area: CapturedArea, to_disk: bool) -> bool {
        let root = template::Template::capture(area);
        if to_disk {
            template::save(name, root).is_ok()
        } else {
            template::keep(name, &root)
        }
    }

    fn forget_template(&self, name: &str) {
        template::forget(name);
    }

    fn obfuscate_seed(&self, seed: i64) -> i64 {
        crate::biome_zoom::obfuscate_seed(seed)
    }

    fn zoom(&self, obfuscated_seed: i64, x: i32, y: i32, z: i32) -> [i32; 3] {
        crate::biome_zoom::zoom(obfuscated_seed, x, y, z)
    }

    fn temperature(&self, base: f32, frozen: bool, x: i32, y: i32, z: i32, sea_level: i32) -> f32 {
        crate::temperature::at_climate(base, frozen, x, y, z, sea_level)
    }
}

/// Offers the feature host to block behaviours.
pub struct FeatureService(Arc<Features>);

impl WorldService for FeatureService {
    fn start(&self, shared: &Arc<tuff_core::world::Shared>) {
        use crate::structure::template;
        let generated = shared
            .world_path
            .as_ref()
            .filter(|path| path.is_dir())
            .map(|path| path.join("generated"));
        let mut contents = shared.datapacks.watch_contents();
        let packs = contents.borrow_and_update().structures.clone();
        template::set_world_sources(shared, generated.clone(), packs);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let owner = Arc::downgrade(shared);
            runtime.spawn(async move {
                while contents.changed().await.is_ok() {
                    let Some(shared) = owner.upgrade() else {
                        break;
                    };
                    let packs = contents.borrow_and_update().structures.clone();
                    template::set_world_sources(&shared, generated.clone(), packs);
                }
            });
        }
    }

    fn feature_host(&self) -> Option<Arc<dyn FeatureHost>> {
        Some(self.0.clone())
    }
}

pub struct FeatureServiceFactory;

impl ServiceFactory for FeatureServiceFactory {
    fn create(
        &self,
        _dimensions: &tuff_core::world::dimension::Dimensions,
    ) -> Arc<dyn WorldService> {
        Arc::new(FeatureService(Arc::new(Features)))
    }
}
