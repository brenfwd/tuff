//! Cushions: dyed pads set on top of a block that one player can sit on.
//! Hitting one breaks it into its item; it also falls apart when its
//! support is gone, in fire, or when lightning strikes it.

use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::item::DyeColor;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::tag;
use tuff_vanilla_data::{BlockState, Item};

use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{
    AttackOutcome, Effect, EntityAttack, EntityBase, EntityHit, Notice, RegionEntity, Tick,
    TickContext, take,
};
use tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY;
use tuff_core::world::physics::{self, Aabb};
use tuff_core::world::{damage_type_in_tag, framed, packets};
use tuff_item::item::{ItemStack, MintReason};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const CHECK_INTERVAL: u32 = 100;
const ANCHOR_REACH: f64 = 0.015_625;
const ANCHOR_DROP: f64 = 0.125;
const SLICE: f64 = 0.015_625;
const DEFLATE: f64 = 1.0e-7;
const NEXT_DOWN: f64 = 1.0e-9;
const DATA_COLOR: u8 = 8;
const COLOR: &str = "color";
const BLOCK_POS: &str = "block_pos";

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Cushion, load);
}

pub fn color_of(item: Item) -> Option<DyeColor> {
    let name = item
        .name()
        .strip_prefix("minecraft:")?
        .strip_suffix("_cushion")?;
    DyeColor::from_name(name)
}

pub fn item_of(color: DyeColor) -> Item {
    Item::from_name(&format!("minecraft:{}_cushion", color.name())).unwrap_or(Item::WhiteCushion)
}

/// The box of a cushion standing at `position`.
pub fn cushion_box(position: [f64; 3]) -> Aabb {
    let size = EntityType::Cushion.size();
    let half = f64::from(size.width) / 2.0;
    [
        position[0] - half,
        position[1],
        position[2] - half,
        position[0] + half,
        position[1] + f64::from(size.height),
        position[2] + half,
    ]
}

fn deflate(b: Aabb, by: f64) -> Aabb {
    [
        b[0] + by,
        b[1] + by,
        b[2] + by,
        b[3] - by,
        b[4] - by,
        b[5] - by,
    ]
}

fn cells(b: Aabb) -> impl Iterator<Item = BlockPos> {
    let (x0, x1) = (b[0].floor() as i32, b[3].floor() as i32);
    let (y0, y1) = (b[1].floor() as i32, b[4].floor() as i32);
    let (z0, z1) = (b[2].floor() as i32, b[5].floor() as i32);
    (x0..=x1).flat_map(move |x| {
        (y0..=y1).flat_map(move |y| (z0..=z1).map(move |z| BlockPos::new(x, y, z)))
    })
}

type Blocks<'a> = &'a dyn Fn(BlockPos) -> Option<BlockState>;

fn has_anchor_below(bounds: Aabb, block: Blocks) -> bool {
    let anchor = [
        bounds[0],
        bounds[1] - ANCHOR_REACH,
        bounds[2],
        bounds[3] - NEXT_DOWN,
        bounds[1],
        bounds[5] - NEXT_DOWN,
    ];
    let area = [
        anchor[0],
        anchor[1] - ANCHOR_DROP,
        anchor[2],
        anchor[3],
        anchor[4],
        anchor[5],
    ];
    cells(area).any(|pos| {
        let mut shape: Option<Aabb> = None;
        for b in physics::block_boxes(block(pos), pos) {
            shape = Some(match shape {
                None => b,
                Some(s) => [
                    s[0].min(b[0]),
                    s[1].min(b[1]),
                    s[2].min(b[2]),
                    s[3].max(b[3]),
                    s[4].max(b[4]),
                    s[5].max(b[5]),
                ],
            });
        }
        shape.is_some_and(|s| physics::intersects(s, anchor))
    })
}

fn subtract(from: Aabb, cut: Aabb) -> Vec<Aabb> {
    if !physics::intersects(from, cut) {
        return vec![from];
    }
    let mut rest = Vec::new();
    let mut core = from;
    for axis in 0..3 {
        if cut[axis] > core[axis] {
            let mut piece = core;
            piece[axis + 3] = cut[axis];
            rest.push(piece);
            core[axis] = cut[axis];
        }
        if cut[axis + 3] < core[axis + 3] {
            let mut piece = core;
            piece[axis] = cut[axis + 3];
            rest.push(piece);
            core[axis + 3] = cut[axis + 3];
        }
    }
    rest
}

fn anchor_buried(bounds: Aabb, block: Blocks) -> bool {
    let slice = deflate(
        [
            bounds[0],
            bounds[1],
            bounds[2],
            bounds[3],
            bounds[1] + SLICE,
            bounds[5],
        ],
        DEFLATE,
    );
    let mut exposed = vec![slice];
    for pos in cells(slice) {
        for collider in physics::block_boxes(block(pos), pos) {
            exposed = exposed
                .into_iter()
                .flat_map(|piece| subtract(piece, collider))
                .collect();
            if exposed.is_empty() {
                return true;
            }
        }
    }
    false
}

fn covered_by_suffocating(bounds: Aabb, block: Blocks) -> bool {
    cells(deflate(bounds, DEFLATE)).all(|pos| block(pos).is_some_and(BlockState::is_suffocating))
}

/// Whether a cushion at `bounds` has something to rest on and room to
/// stay.
pub fn would_survive_at(bounds: Aabb, block: Blocks) -> bool {
    has_anchor_below(bounds, block) && !covered_by_suffocating(bounds, block)
}

/// Whether a cushion can be set down at `bounds`.
pub fn can_be_placed_at(bounds: Aabb, block: Blocks) -> bool {
    would_survive_at(bounds, block) && !anchor_buried(bounds, block)
}

#[derive(Debug, Clone)]
struct Blow {
    kind: &'static DamageType,
    attacker: Option<(i32, EntityType)>,
}

#[derive(Debug)]
pub struct Cushion {
    pub base: EntityBase,
    pub block_pos: BlockPos,
    color: DyeColor,
    since_check: u32,
    fresh: bool,
    seated: bool,
    struck: bool,
    blows: Vec<Blow>,
    changed: Vec<u8>,
}

impl Cushion {
    pub fn new(id: i32, uuid: u128, position: [f64; 3], yaw: f32, color: DyeColor) -> Self {
        let mut base = EntityBase::new(EntityType::Cushion, id, uuid, position, [0.0; 3]);
        base.yaw = yaw;
        base.no_gravity = true;
        Self::from_base(base, color)
    }

    fn from_base(base: EntityBase, color: DyeColor) -> Self {
        let [x, y, z] = base.position;
        Self {
            block_pos: BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32),
            base,
            color,
            since_check: 0,
            fresh: true,
            seated: false,
            struck: false,
            blows: Vec::new(),
            changed: Vec::new(),
        }
    }

    pub fn color(&self) -> DyeColor {
        self.color
    }

    fn stack(&self) -> ItemStack {
        let mut stack = ItemStack::mint(item_of(self.color), 1, MintReason::EntityDrop);
        if let Some(name) = self
            .base
            .custom_name
            .as_ref()
            .and_then(TextComponent::from_nbt)
        {
            stack.components.custom_name = Some(Box::new(name));
        }
        stack
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
        self.sound(SoundEvent::EntityCushionBreak, cx);
        if !cx.shared.rule_bool("entity_drops") || creative {
            return;
        }
        let velocity = [
            rand::Rng::r#gen::<f64>(cx.rng) * 0.2 - 0.1,
            0.2,
            rand::Rng::r#gen::<f64>(cx.rng) * 0.2 - 0.1,
        ];
        cx.effects.push(Effect::DropItem {
            stack: self.stack(),
            position: self.base.position,
            velocity,
            pickup_delay: BLOCK_DROP_PICKUP_DELAY,
        });
    }

    fn in_fire(&self, cx: &TickContext) -> bool {
        cells(deflate(self.bounds(), DEFLATE)).any(|pos| {
            (cx.block)(pos)
                .is_some_and(|state| tuff_datapack::tags::is(tag::block::key::FIRE, state.block()))
        })
    }

    fn resolve(&self, blow: &Blow, cx: &mut TickContext) -> bool {
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

impl RegionEntity for Cushion {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        out.insert(
            BLOCK_POS,
            Tag::IntArray(vec![self.block_pos.x, self.block_pos.y, self.block_pos.z]),
        );
        out.insert(COLOR, self.color.name());
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![(DATA_COLOR, self.color_value())]
    }

    fn take_changed_data(&mut self) -> Vec<(u8, MetadataValue)> {
        std::mem::take(&mut self.changed)
            .into_iter()
            .map(|index| (index, self.color_value()))
            .collect()
    }

    fn display_name(&self) -> String {
        "Cushion".to_owned()
    }

    fn pickable(&self) -> bool {
        true
    }

    fn seats(&self) -> usize {
        1
    }

    fn attacked(&mut self, attack: &EntityAttack, _cx: &mut TickContext) -> Option<AttackOutcome> {
        self.blows.push(Blow {
            kind: &tuff_vanilla_data::damage_type::PLAYER_ATTACK,
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

    fn notice(&mut self, notice: Notice) {
        if notice == Notice::Struck {
            self.struck = true;
        }
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        for blow in std::mem::take(&mut self.blows) {
            if self.resolve(&blow, cx) {
                return Tick::Discard;
            }
        }
        if self.struck {
            self.drop_item(false, cx);
            return Tick::Discard;
        }
        let seated = !self.base.riders.is_empty();
        if seated != self.seated {
            self.seated = seated;
            self.sound(
                if seated {
                    SoundEvent::EntityCushionSit
                } else {
                    SoundEvent::EntityCushionGetUp
                },
                cx,
            );
        }
        let due = self.since_check >= CHECK_INTERVAL;
        self.since_check = if due { 0 } else { self.since_check + 1 };
        if (self.fresh || due) && self.in_fire(cx) {
            self.drop_item(false, cx);
            return Tick::Discard;
        }
        self.fresh = false;
        if due && !would_survive_at(self.bounds(), cx.block) {
            self.drop_item(false, cx);
            return Tick::Discard;
        }
        Tick::Alive
    }
}

impl Cushion {
    fn color_value(&self) -> MetadataValue {
        MetadataValue::Holder {
            serializer: tuff_vanilla_data::entity::data_serializer::DYE_COLOR,
            id: self.color.id() as i32,
        }
    }
}

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let color =
        take(nbt, COLOR, |tag| DyeColor::from_name(tag.as_str()?)).unwrap_or(DyeColor::White);
    let block_pos = take(nbt, BLOCK_POS, |tag| match tag {
        Tag::IntArray(v) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
        _ => None,
    });
    let mut cushion = Cushion::from_base(base, color);
    cushion.base.no_gravity = true;
    if let Some(block_pos) = block_pos {
        cushion.block_pos = block_pos;
    }
    Some(Box::new(cushion))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;

    fn floor_world(pos: BlockPos) -> Option<BlockState> {
        Some(if pos.y < 64 {
            Block::Stone.default_state()
        } else {
            Block::Air.default_state()
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn cushions_rest_on_solid_ground_and_not_in_the_air() {
        let on_floor = cushion_box([0.5, 64.0, 0.5]);
        assert!(can_be_placed_at(on_floor, &floor_world));
        let floating = cushion_box([0.5, 70.0, 0.5]);
        assert!(!can_be_placed_at(floating, &floor_world));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cushion_inside_solid_blocks_does_not_survive() {
        let buried = cushion_box([0.5, 60.0, 0.5]);
        assert!(!would_survive_at(buried, &floor_world));
    }

    #[tuff_core::watchdog]
    #[test]
    fn cushion_items_map_to_their_dye_color() {
        assert_eq!(color_of(Item::LimeCushion), Some(DyeColor::Lime));
        assert_eq!(item_of(DyeColor::Red), Item::RedCushion);
        assert_eq!(color_of(Item::Stone), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn cushions_save_and_load_their_color() {
        let cushion = Cushion::new(1, 1, [0.5, 64.0, 0.5], 90.0, DyeColor::Purple);
        let mut saved = Compound::new();
        cushion.save(&mut saved);
        assert_eq!(saved.get_str(COLOR), Some("purple"));
        let base = EntityBase::new(EntityType::Cushion, 2, 2, [0.5, 64.0, 0.5], [0.0; 3]);
        let loaded = load(base, &mut saved).unwrap();
        assert_eq!(
            loaded.metadata(),
            vec![(
                DATA_COLOR,
                MetadataValue::Holder {
                    serializer: tuff_vanilla_data::entity::data_serializer::DYE_COLOR,
                    id: DyeColor::Purple.id() as i32
                }
            )]
        );
        assert_eq!(loaded.seats(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_struck_cushion_drops_itself_and_goes() {
        tuff_test_support::with_tick_context(|cx| {
            let mut cushion = Cushion::new(1, 1, [0.5, 64.0, 0.5], 0.0, DyeColor::Blue);
            cushion.notice(Notice::Struck);
            assert!(matches!(cushion.tick(cx), Tick::Discard));
            assert!(cx.effects.iter().any(|e| matches!(
                e,
                Effect::DropItem { stack, .. } if stack.item == Item::BlueCushion
            )));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_resting_cushion_stays() {
        tuff_test_support::with_tick_context(|cx| {
            let mut cushion = Cushion::new(1, 1, [0.5, 64.0, 0.5], 0.0, DyeColor::Blue);
            for _ in 0..(CHECK_INTERVAL + 2) {
                assert!(matches!(cushion.tick(cx), Tick::Alive));
            }
        });
    }
}
