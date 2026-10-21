//! Jigsaw structures: a start piece from a template pool, grown piece by
//! piece by joining jigsaw blocks to elements of the pools they name,
//! within a size budget and without overlaps.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;

use serde_json::Value;
use tuff_vanilla_data::registry::worldgen::StructurePiece as PieceType;
use tuff_vanilla_data::structure::{Structure, TerrainAdaptation};

use crate::beardifier::Junction;
use crate::feature::OwnedHeightProvider;
use crate::json::{self, Result, obj};
use crate::proto::HeightmapKind;
use crate::structure::geometry::{BoundingBox, Rotation};
use crate::structure::pool::{self, Element, LiquidSettings, Pool, Projection};
use crate::structure::template::JigsawBlock;
use crate::structure::{GenerationContext, GenerationStub, Piece, PieceContext, StructureKind};
use crate::surface::GenContext;
use tuff_item::random::{LegacyRandom, RandomSource};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

/// A pool alias binding: which pool a jigsaw's pool name stands for in
/// one structure.
#[derive(Debug, Clone)]
enum Alias {
    Direct(String, String),
    Random(String, Vec<(String, i32)>),
    Group(Vec<(Vec<Alias>, i32)>),
}

fn pick<'a, T>(list: &'a [(T, i32)], random: &mut impl RandomSource) -> &'a T {
    let total: i32 = list.iter().map(|(_, w)| *w).sum();
    let mut choice = random.next_int_bounded(total);
    for (value, weight) in list {
        choice -= weight;
        if choice < 0 {
            return value;
        }
    }
    &list[list.len() - 1].0
}

impl Alias {
    fn parse(value: &Value) -> Result<Self> {
        let what = "pool alias";
        Ok(match json::qualify(json::kind(value)?).as_str() {
            "minecraft:direct" => {
                let o = obj(value, &["alias", "target"], what)?;
                Alias::Direct(
                    json::qualify(o.str("alias")?),
                    json::qualify(o.str("target")?),
                )
            }
            "minecraft:random" => {
                let o = obj(value, &["alias", "targets"], what)?;
                let mut targets = Vec::new();
                for t in o.list("targets")? {
                    let t = obj(t, &["data", "weight"], what)?;
                    targets.push((json::qualify(t.str("data")?), t.i32("weight")?));
                }
                Alias::Random(json::qualify(o.str("alias")?), targets)
            }
            "minecraft:random_group" => {
                let o = obj(value, &["groups"], what)?;
                let mut groups = Vec::new();
                for g in o.list("groups")? {
                    let g = obj(g, &["data", "weight"], what)?;
                    let data = g
                        .list("data")?
                        .iter()
                        .map(Alias::parse)
                        .collect::<Result<Vec<_>>>()?;
                    groups.push((data, g.i32("weight")?));
                }
                Alias::Group(groups)
            }
            other => return Err(format!("unmodeled pool alias {other}")),
        })
    }

    fn resolve(&self, random: &mut impl RandomSource, out: &mut HashMap<String, String>) {
        match self {
            Alias::Direct(alias, target) => {
                out.insert(alias.clone(), target.clone());
            }
            Alias::Random(alias, targets) => {
                out.insert(alias.clone(), pick(targets, random).clone());
            }
            Alias::Group(groups) => {
                for alias in pick(groups, random) {
                    alias.resolve(random, out);
                }
            }
        }
    }
}

/// The pool aliases of one structure start.
#[derive(Debug, Default)]
pub struct Aliases(HashMap<String, String>);

impl Aliases {
    fn resolve(bindings: &[Alias], pos: BlockPos, seed: i64) -> Self {
        let mut map = HashMap::new();
        if bindings.is_empty() {
            return Self(map);
        }
        let mut random = LegacyRandom::new(seed)
            .fork_positional()
            .at(pos.x, pos.y, pos.z);
        for binding in bindings {
            binding.resolve(&mut random, &mut map);
        }
        Self(map)
    }

    pub fn lookup<'a>(&'a self, name: &'a str) -> &'a str {
        self.0.get(name).map_or(name, String::as_str)
    }
}

struct Jigsaw {
    start_pool: String,
    start_jigsaw_name: Option<String>,
    max_depth: i32,
    start_height: OwnedHeightProvider,
    use_expansion_hack: bool,
    project_start_to_heightmap: Option<HeightmapKind>,
    max_distance: (i32, i32),
    pool_aliases: Vec<Alias>,
    dimension_padding: Option<(i32, i32)>,
    liquid_settings: LiquidSettings,
}

const MAX_VERTICAL_DISTANCE: i32 = 4064;

pub fn factory(structure: &'static Structure, config: &Value) -> Result<Arc<dyn StructureKind>> {
    let o = obj(
        config,
        &[
            "start_pool",
            "start_jigsaw_name",
            "size",
            "start_height",
            "use_expansion_hack",
            "project_start_to_heightmap",
            "max_distance_from_center",
            "pool_aliases",
            "dimension_padding",
            "liquid_settings",
        ],
        "jigsaw structure",
    )?;
    let max_distance = match o.req("max_distance_from_center")? {
        Value::Object(_) => {
            let d = obj(
                o.req("max_distance_from_center")?,
                &["horizontal", "vertical"],
                "max distance",
            )?;
            (
                d.i32("horizontal")?,
                d.i32_or("vertical", MAX_VERTICAL_DISTANCE)?,
            )
        }
        _ => {
            let h = o.i32("max_distance_from_center")?;
            (h, h)
        }
    };
    let max_depth = o.i32("size")?;
    validate_range(max_depth, max_distance.0, structure.terrain_adaptation)?;
    let dimension_padding = match o.get("dimension_padding") {
        None => None,
        Some(Value::Object(_)) => {
            let d = obj(
                o.req("dimension_padding")?,
                &["bottom", "top"],
                "dimension padding",
            )?;
            Some((d.i32_or("bottom", 0)?, d.i32_or("top", 0)?))
        }
        Some(_) => {
            let v = o.i32("dimension_padding")?;
            Some((v, v))
        }
    };
    let project_start_to_heightmap = match o.get("project_start_to_heightmap") {
        Some(v) => Some(
            v.as_str()
                .and_then(HeightmapKind::from_name)
                .ok_or_else(|| format!("unknown heightmap {v}"))?,
        ),
        None => None,
    };
    let pool_aliases = match o.get("pool_aliases") {
        Some(_) => o
            .list("pool_aliases")?
            .iter()
            .map(Alias::parse)
            .collect::<Result<Vec<_>>>()?,
        None => Vec::new(),
    };
    let liquid_settings = match o.get("liquid_settings") {
        Some(_) => LiquidSettings::parse(o.str("liquid_settings")?)?,
        None => LiquidSettings::ApplyWaterlogging,
    };
    let start_pool = json::qualify(o.str("start_pool")?);
    if pool::get(&start_pool).is_none() {
        return Err(format!("unknown start pool {start_pool}"));
    }
    Ok(Arc::new(Jigsaw {
        start_pool,
        start_jigsaw_name: match o.get("start_jigsaw_name") {
            Some(_) => Some(json::qualify(o.str("start_jigsaw_name")?)),
            None => None,
        },
        max_depth,
        start_height: OwnedHeightProvider::parse(o.req("start_height")?)?,
        use_expansion_hack: o.bool("use_expansion_hack")?,
        project_start_to_heightmap,
        max_distance,
        pool_aliases,
        dimension_padding,
        liquid_settings,
    }))
}

fn validate_range(size: i32, horizontal: i32, adaptation: TerrainAdaptation) -> Result<()> {
    if !(0..=20).contains(&size) {
        return Err(format!("jigsaw size {size} outside 0..20"));
    }
    let edge = match adaptation {
        TerrainAdaptation::None => 0,
        _ => 12,
    };
    if i64::from(horizontal) + edge > 128 {
        return Err(
            "Horizontal structure size including terrain adaptation must not exceed 128".into(),
        );
    }
    Ok(())
}

/// One piece of a jigsaw structure.
pub struct JigsawPiece {
    pub element: Arc<Element>,
    pub position: BlockPos,
    pub ground_level_delta: i32,
    pub rotation: Rotation,
    pub bounding_box: BoundingBox,
    pub junctions: Vec<Junction>,
    pub liquid_settings: LiquidSettings,
    pub keep_jigsaws: bool,
}

impl JigsawPiece {
    fn moved(&mut self, dy: i32) {
        self.bounding_box = self.bounding_box.moved(0, dy, 0);
        self.position = BlockPos::new(self.position.x, self.position.y + dy, self.position.z);
    }
}

fn projection_name(rigid: bool) -> &'static str {
    if rigid { "rigid" } else { "terrain_matching" }
}

impl Piece for JigsawPiece {
    fn kind(&self) -> PieceType {
        PieceType::Jigsaw
    }

    fn bounding_box(&self) -> BoundingBox {
        self.bounding_box
    }

    fn rigid_ground_delta(&self) -> Option<i32> {
        (self.element.projection == Projection::Rigid).then_some(self.ground_level_delta)
    }

    fn junctions(&self) -> &[Junction] {
        &self.junctions
    }

    fn save_extra(&self, tag: &mut Compound) {
        tag.insert("PosX", self.position.x);
        tag.insert("PosY", self.position.y);
        tag.insert("PosZ", self.position.z);
        tag.insert("ground_level_delta", self.ground_level_delta);
        tag.insert("pool_element", self.element.saved.clone());
        tag.insert("rotation", self.rotation.name());
        let junctions = self
            .junctions
            .iter()
            .map(|j| {
                Tag::Compound(
                    Compound::new()
                        .with("source_x", j.source_x)
                        .with("source_ground_y", j.source_ground_y)
                        .with("source_z", j.source_z)
                        .with("delta_y", j.delta_y)
                        .with("dest_proj", projection_name(j.rigid_target)),
                )
            })
            .collect();
        tag.insert("junctions", Tag::List(junctions));
        if self.liquid_settings != LiquidSettings::ApplyWaterlogging {
            tag.insert("liquid_settings", self.liquid_settings.name());
        }
    }

    fn place(
        &self,
        ctx: &mut PieceContext,
        chunk_box: &mut BoundingBox,
        _: ChunkPos,
        reference: BlockPos,
    ) {
        self.element.place(
            ctx.level,
            self.position,
            reference,
            self.rotation,
            *chunk_box,
            ctx.random,
            self.liquid_settings,
            self.keep_jigsaws,
        );
    }

    fn keep_jigsaws(&mut self) {
        self.keep_jigsaws = true;
    }
}

/// The space pieces may still take: a box minus the boxes placed in it.
struct Free {
    min: [i32; 3],
    /// Exclusive.
    max: [i32; 3],
    taken: Vec<BoundingBox>,
}

impl Free {
    fn of(b: BoundingBox) -> Self {
        Self {
            min: [b.min_x, b.min_y, b.min_z],
            max: [b.max_x + 1, b.max_y + 1, b.max_z + 1],
            taken: Vec::new(),
        }
    }

    fn fits(&self, b: &BoundingBox) -> bool {
        b.min_x >= self.min[0]
            && b.min_y >= self.min[1]
            && b.min_z >= self.min[2]
            && b.max_x < self.max[0]
            && b.max_y < self.max[1]
            && b.max_z < self.max[2]
            && !self.taken.iter().any(|t| t.intersects(b))
    }
}

/// Pieces waiting to grow children: highest placement priority first,
/// first come first served within a priority.
#[derive(Default)]
struct Queue(BTreeMap<i32, VecDeque<(usize, usize, i32)>>);

impl Queue {
    fn push(&mut self, entry: (usize, usize, i32), priority: i32) {
        self.0.entry(priority).or_default().push_back(entry);
    }

    fn pop(&mut self) -> Option<(usize, usize, i32)> {
        loop {
            let mut last = self.0.last_entry()?;
            if let Some(entry) = last.get_mut().pop_front() {
                return Some(entry);
            }
            last.remove();
        }
    }
}

struct Placer<'c, 'g> {
    ctx: &'c mut GenerationContext<'g>,
    max_depth: i32,
    expansion_hack: bool,
    aliases: &'c Aliases,
    liquid_settings: LiquidSettings,
    pieces: Vec<JigsawPiece>,
    free: Vec<Free>,
    queue: Queue,
}

impl Placer<'_, '_> {
    fn first_free_height(&self, x: i32, z: i32) -> i32 {
        self.ctx.base_height(x, z, HeightmapKind::WorldSurfaceWg)
    }

    fn expansion(&self, element: &Element, rotation: Rotation, jigsaws: &[JigsawBlock]) -> i32 {
        let hack_box = element.bounding_box(BlockPos::new(0, 0, 0), rotation);
        if !self.expansion_hack || hack_box.y_span() > 16 {
            return 0;
        }
        jigsaws
            .iter()
            .map(|jigsaw| {
                let (dx, dy, dz) = jigsaw.front().step();
                let front = BlockPos::new(jigsaw.pos.x + dx, jigsaw.pos.y + dy, jigsaw.pos.z + dz);
                if !hack_box.contains(front) {
                    return 0;
                }
                let Some(pool) = pool::get(self.aliases.lookup(&jigsaw.pool)) else {
                    return 0;
                };
                let fallback = pool::get(&pool.fallback).map_or(0, |f| f.max_size());
                pool.max_size().max(fallback)
            })
            .max()
            .unwrap_or(0)
    }

    /// Pools reachable through `socket`, in the order their elements are
    /// tried, or `None` when either pool is missing or unusable.
    fn candidates(&mut self, socket: &JigsawBlock, depth: i32) -> Option<Vec<Arc<Element>>> {
        let usable = |pool: &Pool| pool.is_empty_pool() || !pool.templates.is_empty();
        let main = pool::get(self.aliases.lookup(&socket.pool)).filter(|p| usable(p))?;
        let fallback = pool::get(&main.fallback).filter(|p| usable(p))?;
        let mut out = if depth == self.max_depth {
            Vec::new()
        } else {
            main.shuffled_templates(&mut self.ctx.random)
        };
        out.extend(fallback.shuffled_templates(&mut self.ctx.random));
        Some(out)
    }

    /// Searches `elements` for the first child that attaches to `socket`
    /// and fits in `space`.
    fn search(
        &mut self,
        parent: &Parent,
        socket: &JigsawBlock,
        elements: Vec<Arc<Element>>,
        space: usize,
    ) -> Option<Child> {
        let step = socket.front().step();
        let front = BlockPos::new(
            socket.pos.x + step.0,
            socket.pos.y + step.1,
            socket.pos.z + step.2,
        );
        let mut column: Option<i32> = None;
        for element in elements {
            if element.is_empty() {
                return None;
            }
            let rigid = element.projection == Projection::Rigid;
            for rotation in crate::feature::tree::shuffled(&Rotation::ALL, &mut self.ctx.random) {
                let plugs = element.shuffled_jigsaws(
                    BlockPos::new(0, 0, 0),
                    rotation,
                    &mut self.ctx.random,
                );
                let reach = self.expansion(&element, rotation, &plugs);
                for plug in plugs.iter().filter(|plug| socket.can_attach(plug)) {
                    let lift = socket.pos.y - parent.floor + step.1 - plug.pos.y;
                    let anchor = if parent.rigid && rigid {
                        front.y
                    } else {
                        *column.get_or_insert_with(|| {
                            self.first_free_height(socket.pos.x, socket.pos.z)
                        })
                    };
                    let origin = BlockPos::new(
                        front.x - plug.pos.x,
                        anchor - plug.pos.y,
                        front.z - plug.pos.z,
                    );
                    let mut bounds = element.bounding_box(origin, rotation);
                    if reach > 0 {
                        let top = bounds.min_y + (reach + 1).max(bounds.max_y - bounds.min_y);
                        bounds.encapsulate(&BoundingBox::at(BlockPos::new(
                            bounds.min_x,
                            top,
                            bounds.min_z,
                        )));
                    }
                    if !self.free[space].fits(&bounds) {
                        continue;
                    }
                    let seam = match (parent.rigid, rigid) {
                        (true, _) => socket.pos.y,
                        (false, true) => anchor,
                        (false, false) => anchor + lift / 2,
                    };
                    return Some(Child {
                        element,
                        origin,
                        rotation,
                        bounds,
                        rigid,
                        plug_y: plug.pos.y,
                        lift,
                        front,
                        seam,
                    });
                }
            }
        }
        None
    }

    fn try_placing_children(&mut self, source: usize, outer_space: usize, depth: i32) {
        let parent = {
            let p = &self.pieces[source];
            Parent {
                element: p.element.clone(),
                rigid: p.element.projection == Projection::Rigid,
                floor: p.bounding_box.min_y,
                bounds: p.bounding_box,
                ground: p.ground_level_delta,
            }
        };
        let sockets = parent.element.shuffled_jigsaws(
            self.pieces[source].position,
            self.pieces[source].rotation,
            &mut self.ctx.random,
        );
        let mut inner_space: Option<usize> = None;
        for socket in &sockets {
            let Some(elements) = self.candidates(socket, depth) else {
                continue;
            };
            let (dx, dy, dz) = socket.front().step();
            let space = if parent.bounds.contains(BlockPos::new(
                socket.pos.x + dx,
                socket.pos.y + dy,
                socket.pos.z + dz,
            )) {
                *inner_space.get_or_insert_with(|| {
                    self.free.push(Free::of(parent.bounds));
                    self.free.len() - 1
                })
            } else {
                outer_space
            };
            let Some(child) = self.search(&parent, socket, elements, space) else {
                continue;
            };
            self.attach(source, &parent, socket, child, space, depth);
        }
    }

    fn attach(
        &mut self,
        source: usize,
        parent: &Parent,
        socket: &JigsawBlock,
        child: Child,
        space: usize,
        depth: i32,
    ) {
        self.free[space].taken.push(child.bounds);
        let ground = if child.rigid {
            parent.ground - child.lift
        } else {
            child.element.ground_level_delta()
        };
        self.pieces[source].junctions.push(Junction {
            source_x: child.front.x,
            source_ground_y: child.seam - (socket.pos.y - parent.floor) + parent.ground,
            source_z: child.front.z,
            delta_y: child.lift,
            rigid_target: child.rigid,
        });
        self.pieces.push(JigsawPiece {
            element: child.element,
            position: child.origin,
            ground_level_delta: ground,
            rotation: child.rotation,
            bounding_box: child.bounds,
            junctions: vec![Junction {
                source_x: socket.pos.x,
                source_ground_y: child.seam - child.plug_y + ground,
                source_z: socket.pos.z,
                delta_y: -child.lift,
                rigid_target: parent.rigid,
            }],
            liquid_settings: self.liquid_settings,
            keep_jigsaws: false,
        });
        if depth < self.max_depth {
            self.queue.push(
                (self.pieces.len() - 1, space, depth + 1),
                socket.placement_priority,
            );
        }
    }
}

/// The piece whose jigsaws are being filled.
struct Parent {
    element: Arc<Element>,
    rigid: bool,
    floor: i32,
    bounds: BoundingBox,
    ground: i32,
}

/// A child that attaches to a socket and fits its space.
struct Child {
    element: Arc<Element>,
    origin: BlockPos,
    rotation: Rotation,
    bounds: BoundingBox,
    rigid: bool,
    plug_y: i32,
    /// Vertical step from the parent to the child at the seam.
    lift: i32,
    front: BlockPos,
    /// Y of the seam between parent and child.
    seam: i32,
}

impl Jigsaw {
    fn could_exist_in_column(ctx: &mut GenerationContext, x: i32, z: i32) -> bool {
        let (min_y, max_y) = (ctx.shape.min_y, ctx.shape.min_y + ctx.shape.height - 1);
        (min_y >> 2..=max_y >> 2).any(|qy| {
            let biome = ctx.biome(x >> 2, qy, z >> 2);
            ctx.entry.allows(biome)
        })
    }
}

impl StructureKind for Jigsaw {
    fn find_generation_point(&self, ctx: &mut GenerationContext) -> Option<GenerationStub> {
        let settings = ctx.generator.settings;
        let span = GenContext {
            min_y: settings.min_y.max(ctx.shape.min_y),
            height: settings.height.min(ctx.shape.height),
            sea_level: ctx.sea_level(),
        };
        let height = self.start_height.sample(&mut ctx.random, span);
        let start = BlockPos::new(ctx.chunk.x << 4, height, ctx.chunk.z << 4);
        self.stub_at(ctx, start)
    }
}

impl Jigsaw {
    fn stub_at(&self, ctx: &mut GenerationContext, start: BlockPos) -> Option<GenerationStub> {
        let aliases = Aliases::resolve(&self.pool_aliases, start, ctx.seed);
        let rotation = Rotation::ALL[ctx.random.next_int_bounded(4) as usize];
        let center_pool: Arc<Pool> =
            pool::get(aliases.lookup(&self.start_pool)).or_else(|| pool::get(&self.start_pool))?;
        let element = center_pool.random_template(&mut ctx.random)?;
        if element.is_empty() {
            return None;
        }
        let anchored = match &self.start_jigsaw_name {
            Some(name) => {
                element
                    .shuffled_jigsaws(start, rotation, &mut ctx.random)
                    .into_iter()
                    .find(|j| j.name.as_deref() == Some(name.as_str()))?
                    .pos
            }
            None => start,
        };
        let local_anchor = BlockPos::new(
            anchored.x - start.x,
            anchored.y - start.y,
            anchored.z - start.z,
        );
        let adjusted = BlockPos::new(
            start.x - local_anchor.x,
            start.y - local_anchor.y,
            start.z - local_anchor.z,
        );
        let mut center = JigsawPiece {
            element: element.clone(),
            position: adjusted,
            ground_level_delta: element.ground_level_delta(),
            rotation,
            bounding_box: element.bounding_box(adjusted, rotation),
            junctions: Vec::new(),
            liquid_settings: self.liquid_settings,
            keep_jigsaws: false,
        };
        let b = center.bounding_box;
        let center_x = (b.max_x + b.min_x) / 2;
        let center_z = (b.max_z + b.min_z) / 2;
        let bottom_y = match self.project_start_to_heightmap {
            None => adjusted.y,
            Some(kind) => {
                if !Self::could_exist_in_column(ctx, center_x, center_z) {
                    return None;
                }
                start.y + ctx.base_height(center_x, center_z, kind)
            }
        };
        center.moved(bottom_y - (b.min_y + center.ground_level_delta));
        let (world_min, world_max) = (ctx.shape.min_y, ctx.shape.min_y + ctx.shape.height - 1);
        let (pad_bottom, pad_top) = self.dimension_padding.unwrap_or((0, 0));
        if self.dimension_padding.is_some() {
            let b = center.bounding_box;
            if b.min_y < world_min + pad_bottom || b.max_y > world_max - pad_top {
                return None;
            }
        }
        let center_y = bottom_y + local_anchor.y;
        let position = BlockPos::new(center_x, center_y, center_z);
        let max_depth = self.max_depth;
        let expansion_hack = self.use_expansion_hack;
        let (horizontal, vertical) = self.max_distance;
        let liquid_settings = self.liquid_settings;
        Some(GenerationStub {
            position,
            pieces: Box::new(move |ctx: &mut GenerationContext| {
                if max_depth <= 0 {
                    return Vec::new();
                }
                let center_box = center.bounding_box;
                let root = Free {
                    min: [
                        center_x - horizontal,
                        (center_y - vertical).max(world_min + pad_bottom),
                        center_z - horizontal,
                    ],
                    max: [
                        center_x + horizontal + 1,
                        (center_y + vertical + 1).min(world_max + 1 - pad_top),
                        center_z + horizontal + 1,
                    ],
                    taken: vec![center_box],
                };
                let mut placer = Placer {
                    ctx,
                    max_depth,
                    expansion_hack,
                    aliases: &aliases,
                    liquid_settings,
                    pieces: vec![center],
                    free: vec![root],
                    queue: Queue::default(),
                };
                placer.try_placing_children(0, 0, 0);
                while let Some((piece, free, depth)) = placer.queue.pop() {
                    placer.try_placing_children(piece, free, depth);
                }
                placer
                    .pieces
                    .into_iter()
                    .map(|p| Box::new(p) as Box<dyn Piece>)
                    .collect()
            }),
        })
    }
}

/// The pieces `/place jigsaw` assembles: the start element of `pool`
/// anchored by its jigsaw named `target` at `at`, grown `max_depth` deep
/// within 128 blocks.
pub fn assemble(
    ctx: &mut GenerationContext,
    pool: &str,
    target: &str,
    max_depth: i32,
    at: BlockPos,
    keep_jigsaws: bool,
) -> Option<Vec<Box<dyn Piece>>> {
    let jigsaw = Jigsaw {
        start_pool: json::qualify(pool),
        start_jigsaw_name: Some(json::qualify(target)),
        max_depth,
        start_height: OwnedHeightProvider::Constant(
            tuff_vanilla_data::worldgen::VerticalAnchor::Absolute(0),
        ),
        use_expansion_hack: false,
        project_start_to_heightmap: None,
        max_distance: (128, 128),
        pool_aliases: Vec::new(),
        dimension_padding: None,
        liquid_settings: LiquidSettings::ApplyWaterlogging,
    };
    let stub = jigsaw.stub_at(ctx, at)?;
    let mut pieces = (stub.pieces)(ctx);
    if keep_jigsaws {
        pieces.iter_mut().for_each(|piece| piece.keep_jigsaws());
    }
    Some(pieces)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn config_range_validation_matches_the_codec() {
        assert!(validate_range(20, 116, TerrainAdaptation::Bury).is_ok());
        assert!(validate_range(21, 80, TerrainAdaptation::None).is_err());
        assert!(validate_range(-1, 80, TerrainAdaptation::None).is_err());
        assert!(validate_range(6, 117, TerrainAdaptation::BeardThin).is_err());
        assert!(validate_range(6, 128, TerrainAdaptation::None).is_ok());
        assert!(validate_range(6, 129, TerrainAdaptation::None).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_queue_serves_the_highest_priority_first_in_arrival_order() {
        let mut queue = Queue::default();
        queue.push((1, 0, 0), 0);
        queue.push((2, 0, 0), 5);
        queue.push((3, 0, 0), 0);
        queue.push((4, 0, 0), 5);
        assert_eq!(queue.pop(), Some((2, 0, 0)));
        queue.push((5, 0, 0), 1);
        let order: Vec<usize> = std::iter::from_fn(|| queue.pop()).map(|e| e.0).collect();
        assert_eq!(order, [4, 5, 1, 3]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn free_space_refuses_boxes_that_leave_it_or_overlap_taken_ones() {
        let mut free = Free::of(BoundingBox::new(0, 0, 0, 9, 9, 9));
        assert!(free.fits(&BoundingBox::new(0, 0, 0, 9, 9, 9)));
        assert!(!free.fits(&BoundingBox::new(0, 0, 0, 10, 9, 9)));
        free.taken.push(BoundingBox::new(2, 2, 2, 4, 4, 4));
        assert!(!free.fits(&BoundingBox::new(4, 4, 4, 5, 5, 5)));
        assert!(free.fits(&BoundingBox::new(5, 5, 5, 6, 6, 6)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn aliases_resolve_per_start_position() {
        let bindings = [Alias::Random(
            "minecraft:a".into(),
            vec![("minecraft:b".into(), 1), ("minecraft:c".into(), 1)],
        )];
        let pick = |x| {
            Aliases::resolve(&bindings, BlockPos::new(x, 0, 0), 1)
                .lookup("minecraft:a")
                .to_string()
        };
        let picks: std::collections::BTreeSet<String> = (0..64).map(|x| pick(x * 16)).collect();
        assert_eq!(picks.len(), 2);
        assert_eq!(pick(32), pick(32));
        assert_eq!(
            Aliases::resolve(&bindings, BlockPos::new(0, 0, 0), 1).lookup("minecraft:z"),
            "minecraft:z"
        );
    }
}
