//! Template pools: the weighted pieces jigsaw structures choose from, and
//! the elements they hold (templates, features, lists of templates).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde_json::Value;
use tuff_vanilla_data::Block;

use crate::feature::PlaceContext;
use crate::feature::level::Region;
use crate::json::{self, Result, obj};
use crate::proto::HeightmapKind;
use crate::structure::geometry::{BoundingBox, Rotation};
use crate::structure::processor::{BlockIgnore, Gravity, JigsawReplacement, ProcessorList};
use crate::structure::template::{self, JigsawBlock, PlaceRandom, PlaceSettings, Template};
use tuff_item::random::{RandomSource, WorldgenRandom};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

pub const EMPTY_POOL: &str = "minecraft:empty";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    Rigid,
    TerrainMatching,
}

impl Projection {
    fn parse(name: &str) -> Result<Self> {
        match name {
            "rigid" => Ok(Self::Rigid),
            "terrain_matching" => Ok(Self::TerrainMatching),
            other => Err(format!("unknown projection {other}")),
        }
    }
}

/// How templates treat water already where they are placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiquidSettings {
    ApplyWaterlogging,
    IgnoreWaterlogging,
}

impl LiquidSettings {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            "apply_waterlogging" => Ok(Self::ApplyWaterlogging),
            "ignore_waterlogging" => Ok(Self::IgnoreWaterlogging),
            other => Err(format!("unknown liquid settings {other}")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::ApplyWaterlogging => "apply_waterlogging",
            Self::IgnoreWaterlogging => "ignore_waterlogging",
        }
    }
}

pub enum ElementKind {
    Empty,
    Single {
        location: String,
        template: OnceLock<Arc<Template>>,
        processors: ProcessorList,
        legacy: bool,
        liquid_override: Option<LiquidSettings>,
    },
    Feature(Value),
    List(Vec<Element>),
}

/// A pool element with its projection and its saved form.
pub struct Element {
    pub kind: ElementKind,
    pub projection: Projection,
    /// The element as structure starts save it.
    pub saved: Tag,
}

fn json_to_nbt(value: &Value) -> Tag {
    match value {
        Value::String(s) => Tag::String(s.clone()),
        Value::Bool(b) => Tag::Byte(*b as i8),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Tag::Int(i as i32),
            None => Tag::Double(n.as_f64().unwrap_or(0.0)),
        },
        Value::Array(list) => Tag::List(list.iter().map(json_to_nbt).collect()),
        Value::Object(map) => {
            let mut c = Compound::new();
            for (k, v) in map {
                c.insert(k.clone(), json_to_nbt(v));
            }
            Tag::Compound(c)
        }
        Value::Null => Tag::Compound(Compound::new()),
    }
}

impl Element {
    fn parse(value: &Value, projection_override: Option<Projection>) -> Result<Self> {
        let kind = value
            .get("element_type")
            .and_then(Value::as_str)
            .ok_or("pool element without element_type")?;
        let what = "pool element";
        let projection = |o: &json::Obj| -> Result<Projection> {
            match projection_override {
                Some(p) => Ok(p),
                None => Projection::parse(o.str("projection")?),
            }
        };
        let fields_single = [
            "element_type",
            "location",
            "processors",
            "projection",
            "override_liquid_settings",
        ];
        let (kind, projection) = match json::qualify(kind).as_str() {
            "minecraft:empty_pool_element" => {
                obj(value, &["element_type"], what)?;
                (ElementKind::Empty, Projection::TerrainMatching)
            }
            name @ ("minecraft:single_pool_element" | "minecraft:legacy_single_pool_element") => {
                let o = obj(value, &fields_single, what)?;
                let liquid_override = match o.get("override_liquid_settings") {
                    Some(v) => Some(LiquidSettings::parse(
                        v.as_str()
                            .ok_or("override_liquid_settings is not a string")?,
                    )?),
                    None => None,
                };
                (
                    ElementKind::Single {
                        location: json::qualify(o.str("location")?),
                        template: OnceLock::new(),
                        processors: ProcessorList::parse(o.req("processors")?)?,
                        legacy: name == "minecraft:legacy_single_pool_element",
                        liquid_override,
                    },
                    projection(&o)?,
                )
            }
            "minecraft:feature_pool_element" => {
                let o = obj(value, &["element_type", "feature", "projection"], what)?;
                let feature = match o.req("feature")? {
                    Value::String(name) => Value::String(json::qualify(name)),
                    inline => inline.clone(),
                };
                (ElementKind::Feature(feature), projection(&o)?)
            }
            "minecraft:list_pool_element" => {
                let o = obj(value, &["element_type", "elements", "projection"], what)?;
                let projection = projection(&o)?;
                let elements = o
                    .list("elements")?
                    .iter()
                    .map(|e| Element::parse(e, Some(projection)))
                    .collect::<Result<Vec<_>>>()?;
                if elements.is_empty() {
                    return Err("list pool element without elements".into());
                }
                (ElementKind::List(elements), projection)
            }
            other => return Err(format!("unmodeled pool element kind {other}")),
        };
        Ok(Self {
            kind,
            projection,
            saved: json_to_nbt(value),
        })
    }

    pub fn is_empty(&self) -> bool {
        matches!(self.kind, ElementKind::Empty)
    }

    fn template(&self) -> Option<&Arc<Template>> {
        match &self.kind {
            ElementKind::Single {
                location, template, ..
            } => Some(template.get_or_init(|| template::get(location))),
            _ => None,
        }
    }

    /// The element's jigsaw blocks at `position`, shuffled, then stably
    /// sorted by selection priority, highest first.
    pub fn shuffled_jigsaws(
        &self,
        position: BlockPos,
        rotation: Rotation,
        random: &mut impl RandomSource,
    ) -> Vec<JigsawBlock> {
        match &self.kind {
            ElementKind::Empty => Vec::new(),
            ElementKind::Single { .. } => {
                let mut jigsaws = self.template().unwrap().jigsaws(position, rotation);
                crate::feature::tree::decorator::shuffle(&mut jigsaws, random);
                jigsaws.sort_by_key(|j| std::cmp::Reverse(j.selection_priority));
                jigsaws
            }
            ElementKind::Feature(_) => {
                let state = Block::Jigsaw
                    .default_state()
                    .with("orientation", "down_south")
                    .unwrap_or(Block::Jigsaw.default_state());
                vec![JigsawBlock {
                    pos: position,
                    state,
                    rollable: true,
                    name: None,
                    pool: Arc::from(EMPTY_POOL),
                    target: Arc::from(EMPTY_POOL),
                    placement_priority: 0,
                    selection_priority: 0,
                }]
            }
            ElementKind::List(elements) => elements[0].shuffled_jigsaws(position, rotation, random),
        }
    }

    pub fn bounding_box(&self, position: BlockPos, rotation: Rotation) -> BoundingBox {
        match &self.kind {
            ElementKind::Empty => BoundingBox::at(position),
            ElementKind::Single { .. } => self.template().unwrap().bounding_box(
                position,
                rotation,
                BlockPos::new(0, 0, 0),
                crate::structure::geometry::Mirror::None,
            ),
            ElementKind::Feature(_) => BoundingBox::at(position),
            ElementKind::List(elements) => {
                let boxes: Vec<BoundingBox> = elements
                    .iter()
                    .filter(|e| !e.is_empty())
                    .map(|e| e.bounding_box(position, rotation))
                    .collect();
                crate::structure::geometry::encapsulating_boxes(&boxes)
                    .unwrap_or(BoundingBox::at(position))
            }
        }
    }

    pub fn ground_level_delta(&self) -> i32 {
        1
    }

    fn settings(
        &self,
        rotation: Rotation,
        chunk_box: BoundingBox,
        liquid: LiquidSettings,
        keep_jigsaws: bool,
    ) -> PlaceSettings {
        let ElementKind::Single {
            processors,
            legacy,
            liquid_override,
            ..
        } = &self.kind
        else {
            unreachable!("only single elements place templates");
        };
        let liquid = liquid_override.unwrap_or(liquid);
        let mut list = ProcessorList::default();
        if !legacy {
            list.push(BlockIgnore::structure_block());
        }
        if !keep_jigsaws {
            list.push(JigsawReplacement);
        }
        list.0.extend(processors.0.iter().cloned());
        if self.projection == Projection::TerrainMatching {
            list.push(Gravity::new(HeightmapKind::WorldSurfaceWg, -1));
        }
        if *legacy {
            list.push(BlockIgnore::structure_and_air());
        }
        PlaceSettings {
            rotation,
            bounding_box: Some(chunk_box),
            processors: list,
            known_shape: true,
            apply_waterlogging: liquid == LiquidSettings::ApplyWaterlogging,
            post_process: false,
            finalize_entities: true,
            ..PlaceSettings::default()
        }
    }

    /// Places the element's part inside `chunk_box`.
    #[expect(
        clippy::too_many_arguments,
        reason = "independent inputs of one operation; a parameter struct would be built once"
    )]
    pub fn place(
        &self,
        level: &mut Region,
        position: BlockPos,
        reference: BlockPos,
        rotation: Rotation,
        chunk_box: BoundingBox,
        random: &mut WorldgenRandom,
        liquid: LiquidSettings,
        keep_jigsaws: bool,
    ) -> bool {
        match &self.kind {
            ElementKind::Empty => true,
            ElementKind::Single { .. } => {
                let template = self.template().unwrap();
                let settings = self.settings(rotation, chunk_box, liquid, keep_jigsaws);
                let mut place_random = PlaceRandom::positional(random);
                template.place(level, position, reference, &settings, &mut place_random)
            }
            ElementKind::Feature(feature) => {
                let feature = level.placed_feature_value(feature);
                let Ok(feature) = feature else {
                    return false;
                };
                let mut ctx = PlaceContext { level, random };
                feature.place(&mut ctx, position, None)
            }
            ElementKind::List(elements) => elements.iter().all(|e| {
                e.place(
                    level,
                    position,
                    reference,
                    rotation,
                    chunk_box,
                    random,
                    liquid,
                    keep_jigsaws,
                )
            }),
        }
    }
}

/// A template pool: its elements repeated by weight, and the pool to fall
/// back on.
pub struct Pool {
    pub name: String,
    pub templates: Vec<Arc<Element>>,
    pub fallback: String,
    max_size: OnceLock<i32>,
}

impl Pool {
    pub(crate) fn parse_named(name: &str, value: &Value) -> Result<Self> {
        Self::parse(name, value)
    }

    /// A pool with no elements, for one that failed to parse.
    pub(crate) fn empty(name: &str) -> Self {
        Self {
            name: name.to_string(),
            templates: Vec::new(),
            fallback: EMPTY_POOL.to_owned(),
            max_size: OnceLock::new(),
        }
    }

    fn parse(name: &str, value: &Value) -> Result<Self> {
        let o = obj(value, &["elements", "fallback"], "template pool")?;
        let mut templates = Vec::new();
        for entry in o.list("elements")? {
            let e = obj(entry, &["element", "weight"], "template pool entry")?;
            let element = Arc::new(Element::parse(e.req("element")?, None)?);
            for _ in 0..e.i32("weight")? {
                templates.push(element.clone());
            }
        }
        Ok(Self {
            name: name.to_string(),
            templates,
            fallback: json::qualify(o.str("fallback")?),
            max_size: OnceLock::new(),
        })
    }

    pub fn is_empty_pool(&self) -> bool {
        self.name == EMPTY_POOL
    }

    pub fn random_template(&self, random: &mut impl RandomSource) -> Option<Arc<Element>> {
        if self.templates.is_empty() {
            return None;
        }
        Some(self.templates[random.next_int_bounded(self.templates.len() as i32) as usize].clone())
    }

    pub fn shuffled_templates(&self, random: &mut impl RandomSource) -> Vec<Arc<Element>> {
        let mut list = self.templates.clone();
        crate::feature::tree::decorator::shuffle(&mut list, random);
        list
    }

    /// The tallest element, unrotated.
    pub fn max_size(&self) -> i32 {
        *self.max_size.get_or_init(|| {
            self.templates
                .iter()
                .filter(|e| !e.is_empty())
                .map(|e| {
                    e.bounding_box(BlockPos::new(0, 0, 0), Rotation::None)
                        .y_span()
                })
                .max()
                .unwrap_or(0)
        })
    }
}

/// A template pool by name, parsed on first use. `None` for a name the
/// data does not define.
pub fn get(name: &str) -> Option<Arc<Pool>> {
    let name = json::qualify(name);
    if let Some(pool) = crate::loaded::structures::pool(&name) {
        return pool;
    }
    let cache = cache();
    if let Some(pool) = cache.lock().unwrap().get(&name) {
        return pool.clone();
    }
    let pool = tuff_vanilla_data::structure::template_pool(&name).map(|text| {
        let value: Value = serde_json::from_str(text).expect("template pool is JSON");
        Arc::new(Pool::parse(&name, &value).unwrap_or_else(|e| panic!("template pool {name}: {e}")))
    });
    cache.lock().unwrap().insert(name, pool.clone());
    pool
}

fn cache() -> &'static Mutex<HashMap<String, Option<Arc<Pool>>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<Pool>>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Drops every parsed pool and the templates its elements hold; each is
/// parsed again when next used.
pub fn clear_cache() {
    let dropped = std::mem::take(&mut *cache().lock().unwrap_or_else(PoisonError::into_inner));
    drop(dropped);
}

/// Parses every template pool of the data, for tests.
pub fn parse_all() -> Vec<String> {
    let mut errors = Vec::new();
    for (name, text) in tuff_vanilla_data::structure::TEMPLATE_POOLS {
        let value: Value = serde_json::from_str(text).expect("template pool is JSON");
        if let Err(e) = Pool::parse(name, &value) {
            errors.push(format!("{name}: {e}"));
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn every_template_pool_parses() {
        let errors = parse_all();
        assert!(errors.is_empty(), "{errors:#?}");
        let empty = get(EMPTY_POOL).unwrap();
        assert!(empty.templates.is_empty());
        assert_eq!(empty.fallback, EMPTY_POOL);
    }

    #[tuff_core::watchdog]
    #[test]
    fn cleared_pools_are_parsed_again() {
        let name = "minecraft:village/plains/town_centers";
        let first = get(name).unwrap();
        assert!(Arc::ptr_eq(&first, &get(name).unwrap()));
        clear_cache();
        let again = get(name).unwrap();
        assert!(!Arc::ptr_eq(&first, &again));
        assert_eq!(first.templates.len(), again.templates.len());
    }

    #[tuff_core::watchdog]
    #[test]
    fn list_elements_share_their_projection() {
        let pool = get("minecraft:pillager_outpost/base_plates").unwrap();
        assert!(!pool.templates.is_empty());
        let watchtowers = get("minecraft:pillager_outpost/towers").unwrap();
        let list = watchtowers
            .templates
            .iter()
            .find_map(|e| match &e.kind {
                ElementKind::List(list) => Some((e.projection, list)),
                _ => None,
            })
            .expect("a list element");
        assert!(list.1.iter().all(|e| e.projection == list.0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn keeping_jigsaws_drops_the_replacement_processor() {
        let pool = get("minecraft:village/plains/town_centers").unwrap();
        let element = pool
            .templates
            .iter()
            .find(|e| matches!(e.kind, ElementKind::Single { .. }))
            .expect("a single element");
        let chunk_box = BoundingBox::new(0, 0, 0, 15, 15, 15);
        let replaced = element.settings(
            Rotation::None,
            chunk_box,
            LiquidSettings::ApplyWaterlogging,
            false,
        );
        let kept = element.settings(
            Rotation::None,
            chunk_box,
            LiquidSettings::ApplyWaterlogging,
            true,
        );
        assert_eq!(
            replaced.processors.iter().count(),
            kept.processors.iter().count() + 1
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn single_elements_can_override_the_liquid_settings() {
        let value = serde_json::json!({
            "element_type": "minecraft:single_pool_element",
            "location": "minecraft:village/plains/town_centers/plains_fountain_01",
            "processors": "minecraft:empty",
            "projection": "rigid",
            "override_liquid_settings": "ignore_waterlogging"
        });
        let element = Element::parse(&value, None).unwrap();
        let chunk_box = BoundingBox::new(0, 0, 0, 15, 15, 15);
        let settings = element.settings(
            Rotation::None,
            chunk_box,
            LiquidSettings::ApplyWaterlogging,
            false,
        );
        assert!(!settings.apply_waterlogging);
        let plain = Element::parse(
            &serde_json::json!({
                "element_type": "minecraft:single_pool_element",
                "location": "minecraft:village/plains/town_centers/plains_fountain_01",
                "processors": "minecraft:empty",
                "projection": "rigid"
            }),
            None,
        )
        .unwrap();
        assert!(
            plain
                .settings(
                    Rotation::None,
                    chunk_box,
                    LiquidSettings::ApplyWaterlogging,
                    false
                )
                .apply_waterlogging
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn feature_elements_accept_an_inline_placed_feature() {
        let value = serde_json::json!({
            "element_type": "minecraft:feature_pool_element",
            "projection": "rigid",
            "feature": {
                "feature": {"type": "minecraft:no_op", "config": {}},
                "placement": []
            }
        });
        let element = Element::parse(&value, None).unwrap();
        assert!(matches!(
            &element.kind,
            ElementKind::Feature(Value::Object(_))
        ));
    }
}
