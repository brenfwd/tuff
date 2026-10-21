//! Paintings: block-attached entities that cover a rectangle of wall.
//! Placing one picks, among the variants that fit the free wall, one of
//! the largest area. Any hit breaks it, and it falls off at the next
//! 100-tick check when its wall or space is gone.

use tuff_vanilla_data::block::{Axis, Facing};
use tuff_vanilla_data::damage_type::{self, DamageType};
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState, Item};

use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{
    AttackOutcome, Effect, EntityAttack, EntityBase, EntityHit, RegionEntity, Tick, TickContext,
    take, take_number,
};
use tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY;
use tuff_core::world::{damage_type_in_tag, framed, packets, physics};
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const CHECK_INTERVAL: u32 = 100;
const DEPTH: f64 = 0.0625;
const WALL_OFFSET: f64 = 0.46875;
const DROP_SIDE_OFFSET: f64 = 0.15;
const REGISTRY: &str = "minecraft:painting_variant";

const DATA_DIRECTION: u8 = 8;
const DATA_VARIANT: u8 = 9;

#[derive(Debug)]
pub struct Variant {
    pub name: &'static str,
    pub width: i32,
    pub height: i32,
    pub placeable: bool,
}

impl Variant {
    pub fn area(&self) -> i32 {
        self.width * self.height
    }
}

const fn v(name: &'static str, width: i32, height: i32) -> Variant {
    Variant {
        name,
        width,
        height,
        placeable: true,
    }
}

const fn hidden(name: &'static str, width: i32, height: i32) -> Variant {
    Variant {
        name,
        width,
        height,
        placeable: false,
    }
}

pub const VARIANTS: &[Variant] = &[
    v("kebab", 1, 1),
    v("aztec", 1, 1),
    v("alban", 1, 1),
    v("aztec2", 1, 1),
    v("bomb", 1, 1),
    v("plant", 1, 1),
    v("wasteland", 1, 1),
    v("pool", 2, 1),
    v("courbet", 2, 1),
    v("sea", 2, 1),
    v("sunset", 2, 1),
    v("creebet", 2, 1),
    v("wanderer", 1, 2),
    v("graham", 1, 2),
    v("match", 2, 2),
    v("bust", 2, 2),
    v("stage", 2, 2),
    v("void", 2, 2),
    v("skull_and_roses", 2, 2),
    hidden("wither", 2, 2),
    v("fighters", 4, 2),
    v("pointer", 4, 4),
    v("pigscene", 4, 4),
    v("burning_skull", 4, 4),
    v("skeleton", 4, 3),
    hidden("earth", 2, 2),
    hidden("wind", 2, 2),
    hidden("water", 2, 2),
    hidden("fire", 2, 2),
    v("donkey_kong", 4, 3),
    v("baroque", 2, 2),
    v("humble", 2, 2),
    v("meditative", 1, 1),
    v("prairie_ride", 1, 2),
    v("unpacked", 4, 4),
    v("backyard", 3, 4),
    v("bouquet", 3, 3),
    v("cavebird", 3, 3),
    v("changing", 4, 2),
    v("cotan", 3, 3),
    v("endboss", 3, 3),
    v("fern", 3, 3),
    v("finding", 4, 2),
    v("lowmist", 4, 2),
    v("orb", 4, 4),
    v("owlemons", 3, 3),
    v("passage", 4, 2),
    v("pond", 3, 4),
    v("sunflowers", 3, 3),
    v("tides", 3, 3),
    v("dennis", 3, 3),
];

fn find(name: &str) -> Option<&'static Variant> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    variants().iter().find(|variant| variant.name == name)
}

fn qualified(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

/// The painting variants of the world's registries, in network order:
/// the built-in ones, then pack-added ones (sized from their JSON, and
/// placed at random when in `#minecraft:placeable`). Built once per world.
pub fn variants() -> &'static [Variant] {
    type Cache = Vec<(
        std::sync::Weak<tuff_item::registries::Registries>,
        &'static [Variant],
    )>;
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Cache>> = std::sync::OnceLock::new();
    let registries = tuff_item::registries::current();
    let Some(frozen) = registries.get(REGISTRY).filter(|frozen| frozen.modified()) else {
        return VARIANTS;
    };
    let mut cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.retain(|(owner, _)| owner.strong_count() > 0);
    if let Some((_, list)) = cache
        .iter()
        .find(|(owner, _)| std::ptr::eq(owner.as_ptr(), std::sync::Arc::as_ptr(&registries)))
    {
        return list;
    }
    let placeable = |id: &str| {
        tuff_datapack::tags::with(|tags| tags.contains(REGISTRY, "minecraft:placeable", id))
    };
    let list: Vec<Variant> = frozen
        .network_iter()
        .map(|(id, entry, hidden)| {
            let short = id.strip_prefix("minecraft:").unwrap_or(id);
            let builtin = VARIANTS.iter().find(|v| v.name == short);
            let json = entry.value().json.as_deref();
            let size = |key: &str, fallback: i32| {
                json.and_then(|json| json[key].as_i64())
                    .map_or(fallback, |v| v as i32)
            };
            Variant {
                name: tuff_item::registries::intern(short),
                width: size("width", builtin.map_or(1, |v| v.width)),
                height: size("height", builtin.map_or(1, |v| v.height)),
                placeable: !hidden
                    && match (builtin, json) {
                        (Some(builtin), None) => builtin.placeable,
                        _ => placeable(id),
                    },
            }
        })
        .collect();
    let list: &'static [Variant] = Box::leak(list.into_boxed_slice());
    cache.push((std::sync::Arc::downgrade(&registries), list));
    list
}

/// The network id of `variant`: its position in the frozen registry the
/// client was sent (the generated order when no pack touched it).
fn network_id(variant: &Variant) -> i32 {
    let registries = tuff_item::registries::current();
    if let Some(frozen) = registries.get(REGISTRY).filter(|frozen| frozen.modified()) {
        return frozen
            .index_of(&qualified(variant.name))
            .map_or(0, |index| index as i32);
    }
    tuff_vanilla_data::data_registry::DataRegistry::get(REGISTRY)
        .and_then(|registry| {
            registry
                .entries
                .iter()
                .position(|entry| entry.strip_prefix("minecraft:") == Some(variant.name))
        })
        .map_or(0, |index| index as i32)
}

pub fn is_hanging(kind: EntityType) -> bool {
    matches!(
        kind,
        EntityType::Painting | EntityType::ItemFrame | EntityType::GlowItemFrame
    )
}

fn legacy_facing(id: i64) -> Facing {
    match id {
        1 => Facing::West,
        2 => Facing::North,
        3 => Facing::East,
        _ => Facing::South,
    }
}

fn legacy_id(facing: Facing) -> i8 {
    match facing {
        Facing::West => 1,
        Facing::North => 2,
        Facing::East => 3,
        _ => 0,
    }
}

fn yaw_of(facing: Facing) -> f32 {
    match facing {
        Facing::West => 90.0,
        Facing::North => 180.0,
        Facing::East => 270.0,
        _ => 0.0,
    }
}

fn size_offset(size: i32) -> f64 {
    if size % 2 == 0 { 0.5 } else { 0.0 }
}

/// The box of a painting of `variant` attached to the block `pos`
/// (the block it hangs in), facing `facing`.
pub fn painting_box(pos: BlockPos, facing: Facing, variant: &Variant) -> [f64; 6] {
    let (sx, _, sz) = facing.step();
    let (lx, _, lz) = facing.counter_clockwise().step();
    let horizontal = size_offset(variant.width);
    let vertical = size_offset(variant.height);
    let center = [
        f64::from(pos.x) + 0.5 - f64::from(sx) * WALL_OFFSET + f64::from(lx) * horizontal,
        f64::from(pos.y) + 0.5 + vertical,
        f64::from(pos.z) + 0.5 - f64::from(sz) * WALL_OFFSET + f64::from(lz) * horizontal,
    ];
    let width = f64::from(variant.width);
    let half = [
        if facing.axis() == Axis::X {
            DEPTH
        } else {
            width
        } / 2.0,
        f64::from(variant.height) / 2.0,
        if facing.axis() == Axis::Z {
            DEPTH
        } else {
            width
        } / 2.0,
    ];
    [
        center[0] - half[0],
        center[1] - half[1],
        center[2] - half[2],
        center[0] + half[0],
        center[1] + half[1],
        center[2] + half[2],
    ]
}

fn thin_axis(bounds: [f64; 6]) -> usize {
    (0..3)
        .min_by(|a, b| {
            (bounds[a + 3] - bounds[*a])
                .partial_cmp(&(bounds[b + 3] - bounds[*b]))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(0)
}

fn supporting(state: BlockState) -> bool {
    state.is_solid() || matches!(state.block(), Block::Repeater | Block::Comparator)
}

/// What is hanging nearby: its kind and box.
pub type Hanging = (EntityType, [f64; 6]);

/// Whether a painting of `variant` in `pos` would stay: nothing in its
/// space, a solid (or diode) block behind every cell it covers, and no
/// painting overlapping it nor another hanging thing on the same wall
/// plane. An unloaded block counts as unsupported.
pub fn survives(
    pos: BlockPos,
    facing: Facing,
    variant: &Variant,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
    mut others: impl Iterator<Item = Hanging>,
) -> bool {
    let bounds = painting_box(pos, facing, variant);
    if !physics::no_collision(bounds, block) {
        return false;
    }
    let (sx, sy, sz) = facing.step();
    let shift = [sx, sy, sz].map(|s| f64::from(s) * -0.5);
    let eps = 1.0e-7;
    let range = |axis: usize| {
        let low = bounds[axis] + shift[axis] + eps;
        let high = bounds[axis + 3] + shift[axis] - eps;
        (low.floor() as i32)..=(high.floor() as i32)
    };
    for x in range(0) {
        for y in range(1) {
            for z in range(2) {
                if !block(BlockPos::new(x, y, z)).is_some_and(supporting) {
                    return false;
                }
            }
        }
    }
    let axis = thin_axis(bounds);
    !others.any(|(kind, other)| {
        physics::intersects(other, bounds)
            && (kind == EntityType::Painting || thin_axis(other) == axis)
    })
}

/// The variants that fit at `pos` facing `facing`, narrowed to the
/// largest area.
pub fn fitting(
    pos: BlockPos,
    facing: Facing,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
    others: &[Hanging],
) -> Vec<&'static Variant> {
    let mut fits: Vec<&'static Variant> = variants()
        .iter()
        .filter(|variant| variant.placeable)
        .filter(|variant| survives(pos, facing, variant, block, others.iter().copied()))
        .collect();
    let largest = fits.iter().map(|variant| variant.area()).max().unwrap_or(0);
    fits.retain(|variant| variant.area() >= largest);
    fits
}

#[derive(Debug, Clone)]
struct Blow {
    kind: &'static DamageType,
    attacker: Option<(i32, EntityType)>,
}

#[derive(Debug)]
pub struct Painting {
    pub base: EntityBase,
    pub block_pos: BlockPos,
    pub facing: Facing,
    variant: &'static Variant,
    since_check: u32,
    blows: Vec<Blow>,
}

impl Painting {
    pub fn new(
        id: i32,
        uuid: u128,
        block_pos: BlockPos,
        facing: Facing,
        variant: &'static Variant,
    ) -> Self {
        let mut painting = Self {
            base: EntityBase::new(EntityType::Painting, id, uuid, [0.0; 3], [0.0; 3]),
            block_pos,
            facing,
            variant,
            since_check: 0,
            blows: Vec::new(),
        };
        painting.place();
        painting.base.no_gravity = true;
        painting
    }

    pub fn variant(&self) -> &'static Variant {
        self.variant
    }

    fn place(&mut self) {
        let bounds = painting_box(self.block_pos, self.facing, self.variant);
        self.base.position = [
            (bounds[0] + bounds[3]) / 2.0,
            (bounds[1] + bounds[4]) / 2.0,
            (bounds[2] + bounds[5]) / 2.0,
        ];
        self.base.yaw = yaw_of(self.facing);
        self.base.pitch = 0.0;
    }

    fn sound(&self, sound: SoundEvent, cx: &mut TickContext) {
        if self.base.silent {
            return;
        }
        cx.effects
            .push(Effect::ChunkPackets(packets([framed(&play::Sound {
                sound_event: sound.id(),
                category: play::sound_category::NEUTRAL,
                position: self.base.position,
                volume: 1.0,
                pitch: 1.0,
                seed: rand::Rng::r#gen(cx.rng),
            })])));
    }

    fn drop_item(&self, creative: bool, cx: &mut TickContext) {
        if !cx.shared.rule_bool("entity_drops") {
            return;
        }
        self.sound(SoundEvent::EntityPaintingBreak, cx);
        if creative {
            return;
        }
        let mut stack = ItemStack::new(Item::Painting, 1);
        if let Some(name) = self
            .base
            .custom_name
            .as_ref()
            .and_then(TextComponent::from_nbt)
        {
            stack.components.custom_name = Some(Box::new(name));
        }
        let (sx, _, sz) = self.facing.step();
        let [x, y, z] = self.base.position;
        cx.effects.push(Effect::DropItem {
            stack,
            position: [
                x + f64::from(sx) * DROP_SIDE_OFFSET,
                y,
                z + f64::from(sz) * DROP_SIDE_OFFSET,
            ],
            velocity: [
                rand::Rng::r#gen::<f64>(cx.rng) * 0.2 - 0.1,
                0.2,
                rand::Rng::r#gen::<f64>(cx.rng) * 0.2 - 0.1,
            ],
            pickup_delay: BLOCK_DROP_PICKUP_DELAY,
        });
    }

    /// Applies one hit; true when the painting is gone.
    fn resolve(&mut self, blow: &Blow, cx: &mut TickContext) -> bool {
        let creative = blow
            .attacker
            .and_then(|(id, _)| cx.players.iter().find(|p| p.id == id))
            .is_some_and(|p| p.creative);
        if self.base.invulnerable && !bypasses(blow.kind) && !creative {
            return false;
        }
        if let Some((_, kind)) = blow.attacker
            && kind != EntityType::Player
            && kind.mob().category != tuff_vanilla_data::entity::MobCategory::Misc
            && !cx.shared.rule_bool("mob_griefing")
        {
            return false;
        }
        self.drop_item(creative, cx);
        true
    }
}

fn bypasses(kind: &DamageType) -> bool {
    damage_type_in_tag(kind, "minecraft:bypasses_invulnerability")
}

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let block_pos = take(nbt, "block_pos", |tag| match tag {
        Tag::IntArray(v) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
        _ => None,
    })
    .unwrap_or_else(|| {
        let [x, y, z] = base.position;
        BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
    });
    let facing = take_number(nbt, "facing").map_or(Facing::South, |id| legacy_facing(id as i64));
    let variant = take(nbt, "variant", |tag| match tag {
        Tag::String(name) => find(name),
        _ => None,
    })
    .unwrap_or(&VARIANTS[0]);
    let rest = base;
    let mut painting = Painting::new(rest.id, rest.uuid, block_pos, facing, variant);
    painting.base = EntityBase {
        position: painting.base.position,
        yaw: painting.base.yaw,
        pitch: 0.0,
        no_gravity: true,
        ..rest
    };
    painting.place();
    Some(Box::new(painting))
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Painting, load);
}

impl RegionEntity for Painting {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        out.insert("facing", legacy_id(self.facing));
        out.insert(
            "block_pos",
            Tag::IntArray(vec![self.block_pos.x, self.block_pos.y, self.block_pos.z]),
        );
        out.insert("variant", qualified(self.variant.name));
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![
            (
                DATA_DIRECTION,
                MetadataValue::Direction(self.facing.id() as i32),
            ),
            (
                DATA_VARIANT,
                MetadataValue::Holder {
                    serializer: self.base.kind.data_serializer(DATA_VARIANT).unwrap_or(0),
                    id: network_id(self.variant),
                },
            ),
        ]
    }

    fn spawn_data(&self) -> i32 {
        self.facing.id() as i32
    }

    fn spawn_position(&self) -> [f64; 3] {
        [
            f64::from(self.block_pos.x),
            f64::from(self.block_pos.y),
            f64::from(self.block_pos.z),
        ]
    }

    fn bounds(&self) -> [f64; 6] {
        painting_box(self.block_pos, self.facing, self.variant)
    }

    fn pickable(&self) -> bool {
        true
    }

    fn hurt(&mut self, _amount: f32, damage_type: &'static DamageType) -> bool {
        self.blows.push(Blow {
            kind: damage_type,
            attacker: None,
        });
        false
    }

    fn hurt_blamed(
        &mut self,
        _amount: f32,
        damage_type: &'static DamageType,
        culprit: Option<&tuff_core::world::entity::Culprit>,
    ) -> bool {
        self.blows.push(Blow {
            kind: damage_type,
            attacker: culprit.map(|c| (c.id, c.kind)),
        });
        false
    }

    fn attacked(&mut self, attack: &EntityAttack, _cx: &mut TickContext) -> Option<AttackOutcome> {
        self.blows.push(Blow {
            kind: &damage_type::PLAYER_ATTACK,
            attacker: Some((attack.attacker, EntityType::Player)),
        });
        Some(AttackOutcome {
            hurt: true,
            health_lost: 0.0,
        })
    }

    fn hit(&mut self, hit: &EntityHit, _cx: &mut TickContext) -> Option<AttackOutcome> {
        self.blows.push(Blow {
            kind: hit.kind,
            attacker: hit.attacker.as_ref().map(|a| (a.id, a.kind)),
        });
        Some(AttackOutcome {
            hurt: true,
            health_lost: 0.0,
        })
    }

    fn kill(&mut self, _cx: &mut TickContext) -> bool {
        true
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        for blow in std::mem::take(&mut self.blows) {
            if self.resolve(&blow, cx) {
                return Tick::Discard;
            }
        }
        self.since_check += 1;
        if self.since_check > CHECK_INTERVAL {
            self.since_check = 0;
            let (sx, sy, sz) = self.facing.step();
            let behind = BlockPos::new(
                self.block_pos.x - sx,
                self.block_pos.y - sy,
                self.block_pos.z - sz,
            );
            let known = (cx.block)(behind).is_some();
            let others = cx
                .entities
                .iter()
                .filter(|e| is_hanging(e.kind) && e.id != self.base.id)
                .map(|e| (e.kind, e.bounds));
            if known && !survives(self.block_pos, self.facing, self.variant, cx.block, others) {
                self.drop_item(false, cx);
                return Tick::Discard;
            }
        }
        Tick::Alive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wall_at(z: i32) -> impl Fn(BlockPos) -> Option<BlockState> {
        move |pos| {
            Some(if pos.z == z {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        }
    }

    fn by_name(name: &str) -> &'static Variant {
        find(name).unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn pack_paintings_follow_the_built_in_ones_and_may_be_placed() {
        let registry = tuff_vanilla_data::data_registry::DataRegistry::get(REGISTRY).unwrap();
        let mut loader = tuff_item::registries::Loader::default();
        loader.insert(
            registry,
            "a:mural".into(),
            "p",
            br#"{"width":1,"height":1,"asset_id":"a:mural"}"#.to_vec(),
        );
        let frozen = loader.freeze().unwrap();
        let variant = by_name("wasteland");
        let vanilla = network_id(variant);
        tuff_item::registries::sync_scope(frozen, || {
            assert_eq!(network_id(variant), vanilla);
            let mural = find("a:mural").expect("a pack painting");
            assert_eq!((mural.width, mural.height), (1, 1));
            assert_eq!(network_id(mural) as usize, registry.entries.len());
            assert!(!mural.placeable, "not in #minecraft:placeable");
            assert_eq!(variants().len(), registry.entries.len() + 1);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn variants_match_the_registry() {
        let registry = tuff_vanilla_data::data_registry::DataRegistry::get(REGISTRY).unwrap();
        assert_eq!(registry.entries.len(), VARIANTS.len());
        for variant in VARIANTS {
            assert!(
                registry
                    .entries
                    .iter()
                    .any(|entry| entry.ends_with(variant.name)),
                "{}",
                variant.name
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_wide_painting_extends_to_the_left_of_the_wall() {
        let bounds = painting_box(
            BlockPos::new(10, 64, 10),
            Facing::South,
            by_name("fighters"),
        );
        assert_eq!(bounds[5] - bounds[2], 0.0625);
        assert_eq!(bounds[3] - bounds[0], 4.0);
        assert_eq!(bounds[4] - bounds[1], 2.0);
        assert_eq!(bounds[0], 10.5 + 0.5 - 2.0);
        let odd = painting_box(BlockPos::new(0, 0, 0), Facing::East, by_name("bouquet"));
        assert_eq!(odd[4] - odd[1], 3.0);
        assert_eq!(odd[1], -1.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn placement_picks_the_largest_variant_that_fits() {
        let none = |_: BlockPos| Some(Block::Air.default_state());
        let fits = fitting(BlockPos::new(10, 64, 10), Facing::South, &none, &[]);
        assert!(fits.is_empty());
        let wall = |pos: BlockPos| {
            Some(
                if pos.z == 9 && (63..=66).contains(&pos.y) && (10..=14).contains(&pos.x) {
                    Block::Stone.default_state()
                } else {
                    Block::Air.default_state()
                },
            )
        };
        let big = fitting(BlockPos::new(10, 64, 10), Facing::South, &wall, &[]);
        assert!(!big.is_empty());
        assert!(big.iter().all(|variant| variant.area() == big[0].area()));
        let wall = |pos: BlockPos| {
            Some(if pos == BlockPos::new(10, 64, 9) {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let small = fitting(BlockPos::new(10, 64, 10), Facing::South, &wall, &[]);
        assert!(!small.is_empty() && small.iter().all(|variant| variant.area() == 1));
        assert!(small.iter().all(|variant| variant.placeable));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_painting_needs_every_cell_behind_it_and_free_space() {
        let wall = wall_at(9);
        let variant = by_name("wasteland");
        let pos = BlockPos::new(10, 64, 10);
        assert!(survives(
            pos,
            Facing::South,
            variant,
            &wall,
            std::iter::empty()
        ));
        assert!(!survives(
            pos,
            Facing::South,
            by_name("pool"),
            &|p| {
                Some(if p.z == 9 && p.x == 10 {
                    Block::Stone.default_state()
                } else {
                    Block::Air.default_state()
                })
            },
            std::iter::empty()
        ));
        let neighbour = painting_box(pos, Facing::South, variant);
        assert!(!survives(
            pos,
            Facing::South,
            variant,
            &wall,
            std::iter::once((EntityType::Painting, neighbour))
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn saved_paintings_load_back_the_same() {
        let mut registry = tuff_core::world::entity::Registry::default();
        register(&mut registry);
        let painting = Painting::new(1, 5, BlockPos::new(3, 70, -4), Facing::East, by_name("sea"));
        let nbt = tuff_core::world::entity::save(&painting).with("id", "minecraft:painting");
        let loaded = registry.load(&nbt, 9).expect("loads");
        let loaded = loaded.downcast_ref::<Painting>().expect("a painting");
        assert_eq!(loaded.block_pos, BlockPos::new(3, 70, -4));
        assert_eq!(loaded.facing, Facing::East);
        assert_eq!(loaded.variant().name, "sea");
        assert_eq!(loaded.base.position, painting.base.position);
        assert_eq!(painting.spawn_position(), [3.0, 70.0, -4.0]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn data_indices_match_the_dumped_layout() {
        let painting = Painting::new(1, 1, BlockPos::new(0, 64, 0), Facing::North, by_name("sea"));
        let layout = EntityType::Painting.data_layout();
        for (index, value) in painting.metadata() {
            assert_eq!(layout[usize::from(index)], value.type_id() as u8);
        }
    }
}
