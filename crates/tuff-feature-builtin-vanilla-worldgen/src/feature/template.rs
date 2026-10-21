//! Features that place structure templates: `template` (a weighted pick of
//! templates, centred on the origin) and `fossil` (a buried bone template
//! with an ore overlay).

use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::block::Facing;

use crate::feature::{Feature, PlaceContext, Registry};
use crate::json::{self, Result, obj};
use crate::proto::HeightmapKind;
use crate::structure::geometry::{BoundingBox, Mirror, Rotation};
use crate::structure::processor::ProcessorList;
use crate::structure::template::{self, PlaceRandom, PlaceSettings};
use tuff_item::random::RandomSource;
use tuff_world_model::BlockPos;

pub fn register(registry: &mut Registry) {
    registry.register("minecraft:template", |_, v| {
        Ok(Arc::new(TemplateFeature::parse(v)?))
    });
    registry.register("minecraft:fossil", |_, v| Ok(Arc::new(Fossil::parse(v)?)));
}

fn rotation(name: &str) -> Result<Rotation> {
    Ok(match name {
        "none" => Rotation::None,
        "clockwise_90" => Rotation::Clockwise90,
        "180" => Rotation::Clockwise180,
        "counterclockwise_90" => Rotation::CounterClockwise90,
        other => return Err(format!("unknown rotation {other}")),
    })
}

struct Entry {
    template: String,
    rotations: Vec<Rotation>,
    weight: i32,
}

struct TemplateFeature {
    templates: Vec<Entry>,
    processors: ProcessorList,
}

impl TemplateFeature {
    fn parse(value: &Value) -> Result<Self> {
        let o = obj(
            value,
            &["type", "templates", "processors"],
            "template feature",
        )?;
        let templates = o
            .list("templates")?
            .iter()
            .map(|entry| {
                let e = obj(entry, &["data", "weight"], "template entry")?;
                let d = obj(e.req("data")?, &["id", "rotations"], "template entry")?;
                let rotations = match d.get("rotations") {
                    Some(list) => list
                        .as_array()
                        .ok_or("rotations is a list")?
                        .iter()
                        .map(|r| rotation(r.as_str().unwrap_or("")))
                        .collect::<Result<_>>()?,
                    None => Rotation::ALL.to_vec(),
                };
                Ok(Entry {
                    template: json::qualify(d.str("id")?),
                    rotations,
                    weight: e.i32("weight")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if templates.is_empty() {
            return Err("template features need a template".into());
        }
        let processors = match o.get("processors") {
            Some(v) => ProcessorList::parse(v)?,
            None => ProcessorList::default(),
        };
        Ok(Self {
            templates,
            processors,
        })
    }
}

fn unit(facing: Facing) -> (i32, i32, i32) {
    facing.step()
}

impl Feature for TemplateFeature {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let total: i32 = self.templates.iter().map(|e| e.weight).sum();
        let mut selection = ctx.random.next_int_bounded(total);
        let entry = self
            .templates
            .iter()
            .find(|e| {
                selection -= e.weight;
                selection < 0
            })
            .expect("selection within the total weight");
        let rotation =
            entry.rotations[ctx.random.next_int_bounded(entry.rotations.len() as i32) as usize];
        let template = template::get(&entry.template);
        let (xx, _, xz) = unit(rotation.rotate_facing(Facing::West));
        let (zx, _, zz) = unit(rotation.rotate_facing(Facing::North));
        let (half_x, half_z) = (template.size[0] / 2, template.size[2] / 2);
        let pos = BlockPos::new(
            origin.x + xx * half_x + zx * half_z,
            origin.y,
            origin.z + xz * half_x + zz * half_z,
        );
        let settings = PlaceSettings {
            rotation,
            processors: self.processors.clone(),
            ..PlaceSettings::default()
        };
        let mut random = PlaceRandom::shared(&mut *ctx.random);
        template.place(ctx.level, pos, pos, &settings, &mut random)
    }
}

struct Fossil {
    fossils: Vec<String>,
    overlays: Vec<String>,
    fossil_processors: ProcessorList,
    overlay_processors: ProcessorList,
    max_empty_corners: i32,
}

impl Fossil {
    fn parse(value: &Value) -> Result<Self> {
        let o = obj(
            value,
            &[
                "type",
                "fossil_structures",
                "overlay_structures",
                "fossil_processors",
                "overlay_processors",
                "max_empty_corners_allowed",
            ],
            "fossil",
        )?;
        let names = |key: &str| -> Result<Vec<String>> {
            o.list(key)?
                .iter()
                .map(|v| v.as_str().map(json::qualify).ok_or("template names".into()))
                .collect()
        };
        let fossils = names("fossil_structures")?;
        let overlays = names("overlay_structures")?;
        if fossils.is_empty() || fossils.len() != overlays.len() {
            return Err("fossils need one overlay each".into());
        }
        Ok(Self {
            fossils,
            overlays,
            fossil_processors: ProcessorList::parse(o.req("fossil_processors")?)?,
            overlay_processors: ProcessorList::parse(o.req("overlay_processors")?)?,
            max_empty_corners: o.i32("max_empty_corners_allowed")?,
        })
    }
}

fn corners(b: &BoundingBox) -> [BlockPos; 8] {
    let (x0, y0, z0, x1, y1, z1) = (b.min_x, b.min_y, b.min_z, b.max_x, b.max_y, b.max_z);
    [
        BlockPos::new(x1, y1, z1),
        BlockPos::new(x0, y1, z1),
        BlockPos::new(x1, y0, z1),
        BlockPos::new(x0, y0, z1),
        BlockPos::new(x1, y1, z0),
        BlockPos::new(x0, y1, z0),
        BlockPos::new(x1, y0, z0),
        BlockPos::new(x0, y0, z0),
    ]
}

/// The corner a template's zero position moves to when it is rotated
/// about its own origin and mirrored, as the fossil feature anchors it.
pub fn zero_position(
    zero: BlockPos,
    mirror: Mirror,
    rotation: Rotation,
    size_x: i32,
    size_z: i32,
) -> BlockPos {
    let (sx, sz) = (size_x - 1, size_z - 1);
    let mx = if mirror == Mirror::FrontBack { sx } else { 0 };
    let mz = if mirror == Mirror::LeftRight { sz } else { 0 };
    let (dx, dz) = match rotation {
        Rotation::CounterClockwise90 => (mz, sx - mx),
        Rotation::Clockwise90 => (sz - mz, mx),
        Rotation::Clockwise180 => (sx - mx, sz - mz),
        Rotation::None => (mx, mz),
    };
    BlockPos::new(zero.x + dx, zero.y, zero.z + dz)
}

impl Feature for Fossil {
    fn place(&self, ctx: &mut PlaceContext, origin: BlockPos) -> bool {
        let rotation = Rotation::ALL[ctx.random.next_int_bounded(4) as usize];
        let index = ctx.random.next_int_bounded(self.fossils.len() as i32) as usize;
        let base = template::get(&self.fossils[index]);
        let overlay = template::get(&self.overlays[index]);
        let (cx, cz) = (origin.x >> 4, origin.z >> 4);
        let level = &mut *ctx.level;
        let bounding_box = BoundingBox::new(
            (cx << 4) - 16,
            level.min_y(),
            (cz << 4) - 16,
            (cx << 4) + 15 + 16,
            level.max_y(),
            (cz << 4) + 15 + 16,
        );
        let [size_x, _, size_z] = base.size(rotation);
        let low = BlockPos::new(origin.x + (-size_x) / 2, origin.y, origin.z + (-size_z) / 2);
        let mut lowest = origin.y;
        for dx in 0..size_x {
            for dz in 0..size_z {
                lowest =
                    lowest.min(level.height(HeightmapKind::OceanFloorWg, low.x + dx, low.z + dz));
            }
        }
        let target_y = (lowest - 15 - ctx.random.next_int_bounded(10)).max(level.min_y() + 10);
        let target = zero_position(
            BlockPos::new(low.x, target_y, low.z),
            Mirror::None,
            rotation,
            base.size[0],
            base.size[2],
        );
        let mut settings = PlaceSettings {
            rotation,
            bounding_box: Some(bounding_box),
            ..PlaceSettings::default()
        };
        let empty = corners(&base.settings_box(&settings, target))
            .iter()
            .filter(|&&pos| {
                let state = level.block(pos);
                tuff_world_model::chunk::is_air(state)
                    || matches!(state.block(), Block::Lava | Block::Water)
            })
            .count() as i32;
        if empty > self.max_empty_corners {
            return false;
        }
        settings.processors = self.fossil_processors.clone();
        let mut random = PlaceRandom::shared(&mut *ctx.random);
        base.place(level, target, target, &settings, &mut random);
        settings.processors = self.overlay_processors.clone();
        overlay.place(level, target, target, &settings, &mut random);
        true
    }
}
