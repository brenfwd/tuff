//! The structure block editor: the edit lands in the block's saved data,
//! then the block saves, loads or scans as asked.

use super::*;

const STRUCTURE_MODES: [&str; 4] = ["SAVE", "LOAD", "CORNER", "DATA"];
const STRUCTURE_ROTATIONS: [&str; 4] = [
    "NONE",
    "CLOCKWISE_90",
    "CLOCKWISE_180",
    "COUNTERCLOCKWISE_90",
];
const STRUCTURE_MIRRORS: [&str; 3] = ["NONE", "LEFT_RIGHT", "FRONT_BACK"];

/// A structure name as the game stores it: a valid identifier with its
/// namespace, `None` for an empty or invalid one.
fn structure_name(name: &str) -> Option<String> {
    let (namespace, path) = name.split_once(':').unwrap_or(("minecraft", name));
    let plain = |c: char| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-' | '.');
    let valid = !namespace.is_empty()
        && namespace.chars().all(plain)
        && !path.is_empty()
        && path.chars().all(|c| plain(c) || c == '/');
    valid.then(|| format!("{namespace}:{path}"))
}

pub enum StructureOutcome {
    Gone,
    Say(TextComponent),
    Place {
        name: String,
        request: TemplateRequest,
        extent: TemplateExtent,
    },
}

use crate::world::features::{CapturedArea, TemplateExtent, TemplateRequest};
use crate::world::region_api::RegionWorld;

/// Reads the area `size` large at `origin` for a template: `None` when part
/// of it is not on this region.
fn capture_area(
    world: &dyn RegionWorld,
    origin: BlockPos,
    size: [i32; 3],
    include_entities: bool,
) -> Option<CapturedArea> {
    use crate::world::growth::GrowthLevel;
    let mut area = CapturedArea {
        size,
        ..CapturedArea::default()
    };
    for dy in 0..size[1] {
        for dz in 0..size[2] {
            for dx in 0..size[0] {
                let at = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                let block = GrowthLevel::block(world, at)?;
                area.blocks.push((
                    BlockPos::new(dx, dy, dz),
                    block,
                    world.saved_block_entity(at),
                ));
            }
        }
    }
    if include_entities && size.iter().all(|s| *s > 0) {
        let min = [origin.x, origin.y, origin.z].map(f64::from);
        for id in world.entity_ids() {
            let Some(tracked) = world.entity(id) else {
                continue;
            };
            let base = tracked.entity.base();
            if base.kind == tuff_vanilla_data::registry::EntityType::Player {
                continue;
            }
            let rel: [f64; 3] = std::array::from_fn(|axis| base.position[axis] - min[axis]);
            if (0..3).any(|axis| rel[axis] < 0.0 || rel[axis] >= f64::from(size[axis])) {
                continue;
            }
            let block_pos = BlockPos::new(
                rel[0].floor() as i32,
                rel[1].floor() as i32,
                rel[2].floor() as i32,
            );
            area.entities
                .push((rel, block_pos, crate::world::entity::save(&*tracked.entity)));
        }
    }
    Some(area)
}

/// The block's settings as stored in its data.
struct Settings {
    name: Option<String>,
    mode: &'static str,
    offset: [i32; 3],
    size: [i32; 3],
    rotation: u8,
    mirror: u8,
    ignore_entities: bool,
    strict: bool,
    integrity: f32,
    seed: i64,
}

impl Settings {
    fn read(data: &tuff_nbt::Compound) -> Self {
        let int = |key: &str, default: i32, low: i32, high: i32| {
            data.get_int(key).unwrap_or(default).clamp(low, high)
        };
        let index = |key: &str, names: &[&str]| {
            data.get_str(key)
                .and_then(|value| names.iter().position(|name| *name == value))
                .unwrap_or(0) as u8
        };
        Self {
            name: data.get_str("name").and_then(structure_name),
            mode: data
                .get_str("mode")
                .and_then(|mode| STRUCTURE_MODES.iter().copied().find(|m| *m == mode))
                .unwrap_or("DATA"),
            offset: [
                int("posX", 0, -48, 48),
                int("posY", 1, -48, 48),
                int("posZ", 0, -48, 48),
            ],
            size: [
                int("sizeX", 0, 0, 48),
                int("sizeY", 0, 0, 48),
                int("sizeZ", 0, 0, 48),
            ],
            rotation: index("rotation", &STRUCTURE_ROTATIONS),
            mirror: index("mirror", &STRUCTURE_MIRRORS),
            ignore_entities: data.get_bool("ignoreEntities").unwrap_or(true),
            strict: data.get_bool("strict").unwrap_or(false),
            integrity: data.get_float("integrity").unwrap_or(1.0),
            seed: data.get_long("seed").unwrap_or(0),
        }
    }

    fn origin(&self, pos: BlockPos) -> BlockPos {
        BlockPos::new(
            pos.x + self.offset[0],
            pos.y + self.offset[1],
            pos.z + self.offset[2],
        )
    }

    fn request(&self, name: String, pos: BlockPos) -> TemplateRequest {
        TemplateRequest {
            name,
            pos: self.origin(pos),
            rotation: self.rotation,
            mirror: self.mirror,
            integrity: self.integrity.clamp(0.0, 1.0),
            seed: self.seed,
            strict: self.strict,
            ignore_entities: self.ignore_entities,
        }
    }
}

/// Saves the block's area as its template, on disk or only in memory.
fn save_area(world: &dyn RegionWorld, pos: BlockPos, settings: &Settings, to_disk: bool) -> bool {
    let Some(name) = settings.name.as_deref() else {
        return false;
    };
    let Some(host) = world.shared().services.feature_host() else {
        return false;
    };
    capture_area(
        world,
        settings.origin(pos),
        settings.size,
        !settings.ignore_entities,
    )
    .is_some_and(|area| host.save_template(name, area, to_disk))
}

/// Takes the template's size into the block, then readies its placement.
fn prepare_load(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    settings: &Settings,
    name: &str,
    size: [i32; 3],
) -> Option<(TemplateRequest, TemplateExtent)> {
    if let Some(data) = world.raw_block_entity_mut(pos) {
        for (key, value) in ["sizeX", "sizeY", "sizeZ"].into_iter().zip(size) {
            data.insert(key, tuff_nbt::Tag::Int(value));
        }
    }
    let host = world.shared().services.feature_host()?;
    let request = settings.request(name.to_owned(), pos);
    let extent = host.template_extent(&request)?;
    Some((request, extent))
}

/// The redstone signal beside a structure block changed: a rising edge
/// saves (in memory), loads or forgets its template by mode. A load comes
/// back as the placement to run.
pub fn redstone_power(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    signal: bool,
) -> Option<(TemplateRequest, TemplateExtent)> {
    use crate::world::growth::GrowthLevel;
    GrowthLevel::block(&*world, pos)
        .filter(|s| s.block() == tuff_vanilla_data::Block::StructureBlock)?;
    let data = world.raw_block_entity_mut(pos)?;
    let powered = data.get_bool("powered").unwrap_or(false);
    if signal == powered {
        return None;
    }
    data.insert("powered", signal);
    if !signal {
        return None;
    }
    let settings = Settings::read(data);
    let name = settings.name.clone()?;
    let host = world.shared().services.feature_host()?;
    match settings.mode {
        "SAVE" => {
            save_area(&*world, pos, &settings, false);
            None
        }
        "LOAD" => {
            let size = host.template_size(&name)?;
            prepare_load(world, pos, &settings, &name, size)
        }
        "CORNER" => {
            host.forget_template(&name);
            None
        }
        _ => None,
    }
}

/// Places a structure block's template across the regions it reaches.
pub async fn place(
    shared: std::sync::Arc<crate::world::Shared>,
    dimension: crate::world::dimension::DimensionId,
    request: TemplateRequest,
    extent: TemplateExtent,
) {
    use crate::world::messages::ManagerMessage;
    let flags = if request.strict {
        crate::world::redstone::UPDATE_CLIENTS
            | crate::world::redstone::UPDATE_SKIP_BLOCK_ENTITY_SIDE_EFFECTS
    } else {
        crate::world::redstone::UPDATE_CLIENTS | crate::world::redstone::UPDATE_NEIGHBORS
    };
    let (min, max) = extent.placed;
    let mut targets = Vec::new();
    for z in min.z..=max.z {
        for x in min.x..=max.x {
            let chunk = ChunkPos::new(x, z);
            let (reply, answer) = tokio::sync::oneshot::channel();
            let acquire = ManagerMessage::Acquire {
                dimension,
                region: chunk.region(),
                reply,
            };
            if shared.manager.send(acquire).await.is_err() {
                continue;
            }
            if let Ok(region) = answer.await {
                targets.push((chunk, region));
            }
        }
    }
    let work = std::sync::Arc::new(crate::world::place_work::Work::Request(
        crate::world::features::PlaceRequest::Template(request),
    ));
    crate::world::place_work::place_chunks(&targets, &work, flags).await;
    for (chunk, _) in targets {
        let _ = shared
            .manager
            .send(ManagerMessage::Release {
                dimension,
                region: chunk.region(),
            })
            .await;
    }
}

pub fn apply_edit(
    world: &mut dyn RegionWorld,
    edit: &crate::protocol::play::StructureBlockEdit,
) -> StructureOutcome {
    use crate::world::growth::GrowthLevel;
    use crate::world::redstone::UPDATE_CLIENTS;
    use tuff_vanilla_data::Block;

    let pos = edit.position;
    let say = |key: &str, name: &str| {
        StructureOutcome::Say(TextComponent::translate(key, [TextComponent::new(name)]))
    };
    let Some(state) =
        GrowthLevel::block(&*world, pos).filter(|s| s.block() == Block::StructureBlock)
    else {
        return StructureOutcome::Gone;
    };
    let mode = usize::try_from(edit.mode).unwrap_or(3).min(3);
    let name = structure_name(&edit.name);
    let Some(data) = world.raw_block_entity_mut(pos) else {
        return StructureOutcome::Gone;
    };
    data.insert("name", name.clone().unwrap_or_default());
    data.insert("metadata", edit.data.as_str());
    for (key, value) in ["posX", "posY", "posZ"].into_iter().zip(edit.offset) {
        data.insert(key, value);
    }
    for (key, value) in ["sizeX", "sizeY", "sizeZ"].into_iter().zip(edit.size) {
        data.insert(key, value);
    }
    data.insert(
        "rotation",
        STRUCTURE_ROTATIONS[usize::try_from(edit.rotation).unwrap_or(0).min(3)],
    );
    data.insert(
        "mirror",
        STRUCTURE_MIRRORS[usize::try_from(edit.mirror).unwrap_or(0).min(2)],
    );
    data.insert("mode", STRUCTURE_MODES[mode]);
    data.insert("ignoreEntities", edit.ignore_entities);
    data.insert("strict", edit.strict);
    data.insert("showair", edit.show_air);
    data.insert("showboundingbox", edit.show_bounding_box);
    data.insert("integrity", edit.integrity);
    data.insert("seed", edit.seed);
    let settings = Settings::read(data);
    if let Some(next) = state.with("mode", &STRUCTURE_MODES[mode].to_lowercase())
        && next != state
    {
        world.set(pos, next, UPDATE_CLIENTS);
    }
    let outcome = match name {
        None => say("structure_block.invalid_structure_name", &edit.name),
        Some(name) => match edit.update {
            1 => say(
                if mode == 0 && save_area(&*world, pos, &settings, true) {
                    "structure_block.save_success"
                } else {
                    "structure_block.save_failure"
                },
                &name,
            ),
            2 => {
                let host = world.shared().services.feature_host();
                match host.and_then(|host| host.template_size(&name)) {
                    Some(size) if mode == 1 => {
                        if size == edit.size {
                            match prepare_load(world, pos, &settings, &name, size) {
                                Some((request, extent)) => StructureOutcome::Place {
                                    name,
                                    request,
                                    extent,
                                },
                                None => say("structure_block.load_not_found", &name),
                            }
                        } else {
                            if let Some(data) = world.raw_block_entity_mut(pos) {
                                for (key, value) in
                                    ["sizeX", "sizeY", "sizeZ"].into_iter().zip(size)
                                {
                                    data.insert(key, tuff_nbt::Tag::Int(value));
                                }
                            }
                            say("structure_block.load_prepare", &name)
                        }
                    }
                    _ => say("structure_block.load_not_found", &name),
                }
            }
            3 => {
                if mode == 0 && detect_structure_size(world, pos, &name) {
                    say("structure_block.size_success", &name)
                } else {
                    StructureOutcome::Say(TextComponent::translate(
                        "structure_block.size_failure",
                        [],
                    ))
                }
            }
            _ => return finish_structure_edit(world, pos, StructureOutcome::Gone),
        },
    };
    finish_structure_edit(world, pos, outcome)
}

fn finish_structure_edit(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    outcome: StructureOutcome,
) -> StructureOutcome {
    world.block_entity_changed(pos);
    outcome
}

/// The scan button: boxes in the corner blocks of the same name around the
/// structure block at `pos` and stores the box as its area.
fn detect_structure_size(world: &mut dyn RegionWorld, pos: BlockPos, name: &str) -> bool {
    use tuff_nbt::Tag;
    let corners: Vec<BlockPos> = world
        .raw_block_entities_where(&|data| {
            data.get_str("mode") == Some("CORNER") && data.get_str("name") == Some(name)
        })
        .into_iter()
        .filter(|at| (at.x - pos.x).abs() <= 80 && (at.z - pos.z).abs() <= 80)
        .filter(|at| {
            crate::world::growth::GrowthLevel::block(&*world, *at)
                .is_some_and(|s| s.block() == tuff_vanilla_data::Block::StructureBlock)
        })
        .collect();
    if corners.is_empty() {
        return false;
    }
    let mut min = [i32::MAX; 3];
    let mut max = [i32::MIN; 3];
    let all = corners.iter().chain((corners.len() == 1).then_some(&pos));
    for at in all {
        for (axis, value) in [at.x, at.y, at.z].into_iter().enumerate() {
            min[axis] = min[axis].min(value);
            max[axis] = max[axis].max(value);
        }
    }
    let delta = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    if delta.iter().any(|d| *d <= 1) {
        return false;
    }
    let Some(data) = world.raw_block_entity_mut(pos) else {
        return false;
    };
    let origin = [pos.x, pos.y, pos.z];
    for (axis, (pos_key, size_key)) in [("posX", "sizeX"), ("posY", "sizeY"), ("posZ", "sizeZ")]
        .into_iter()
        .enumerate()
    {
        data.insert(pos_key, Tag::Int(min[axis] - origin[axis] + 1));
        data.insert(size_key, Tag::Int(delta[axis] - 1));
    }
    true
}
