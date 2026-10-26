//! Item frames and glow item frames: block-attached entities holding one
//! item. Hitting one pops its item out first, then breaks the frame; a
//! click puts an item in or turns the item an eighth of a turn. A frame
//! that loses its wall (or has something in its way) falls off at the next
//! 100-tick check.

use tuff_item::item::BurnReason;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::damage_type::{self, DamageType};
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState, Item};

use tuff_core::protocol::play::{self, MetadataValue, metadata};
use tuff_core::world::entity::{
    AttackOutcome, Effect, EntityAttack, EntityBase, EntityHit, EntityInteraction, InteractOutcome,
    RegionEntity, Tick, TickContext, take, take_bool, take_number,
};
use tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY;
use tuff_core::world::messages::HandEffect;
use tuff_core::world::region_api::SlotEdit;
use tuff_core::world::{damage_type_in_tag, framed, packets, physics};
use tuff_item::item::{ItemStack, MintReason};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const CHECK_INTERVAL: u32 = 100;
const MAP_REPORT_TICKS: u64 = 10;
const DEPTH: f64 = 0.0625;
const WIDTH: f64 = 0.75;
const MAP_WIDTH: f64 = 1.0;
const WALL_OFFSET: f64 = 0.46875;
const ROTATIONS: u8 = 8;
const DROP_SIDE_OFFSET: f64 = 0.15;

const DATA_DIRECTION: u8 = 8;
const DATA_ITEM: u8 = 9;
const DATA_ROTATION: u8 = 10;

pub fn is_frame(kind: EntityType) -> bool {
    matches!(kind, EntityType::ItemFrame | EntityType::GlowItemFrame)
}

pub fn frame_item(kind: EntityType) -> Item {
    if kind == EntityType::GlowItemFrame {
        Item::GlowItemFrame
    } else {
        Item::ItemFrame
    }
}

pub fn facing_of_id(id: i64) -> Option<Facing> {
    Some(match id {
        0 => Facing::Down,
        1 => Facing::Up,
        2 => Facing::North,
        3 => Facing::South,
        4 => Facing::West,
        5 => Facing::East,
        _ => return None,
    })
}

/// The yaw and pitch a frame facing `facing` has.
fn angles(facing: Facing) -> (f32, f32) {
    match facing {
        Facing::South => (0.0, 0.0),
        Facing::West => (90.0, 0.0),
        Facing::North => (180.0, 0.0),
        Facing::East => (270.0, 0.0),
        Facing::Up => (0.0, -90.0),
        Facing::Down => (0.0, 90.0),
    }
}

/// The box of a frame in the block `pos` facing `facing`.
pub fn frame_box(pos: BlockPos, facing: Facing, map: bool) -> [f64; 6] {
    let (sx, sy, sz) = facing.step();
    let center = [
        f64::from(pos.x) + 0.5 - f64::from(sx) * WALL_OFFSET,
        f64::from(pos.y) + 0.5 - f64::from(sy) * WALL_OFFSET,
        f64::from(pos.z) + 0.5 - f64::from(sz) * WALL_OFFSET,
    ];
    let width = if map { MAP_WIDTH } else { WIDTH };
    let half = |axis_thin: bool| if axis_thin { DEPTH / 2.0 } else { width / 2.0 };
    let thin = [
        facing.axis() == tuff_vanilla_data::block::Axis::X,
        facing.axis() == tuff_vanilla_data::block::Axis::Y,
        facing.axis() == tuff_vanilla_data::block::Axis::Z,
    ];
    let h = thin.map(half);
    [
        center[0] - h[0],
        center[1] - h[1],
        center[2] - h[2],
        center[0] + h[0],
        center[1] + h[1],
        center[2] + h[2],
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

fn is_diode(state: BlockState) -> bool {
    matches!(state.block(), Block::Repeater | Block::Comparator)
}

/// Whether a frame in `pos` facing `facing` would stay: nothing solid in
/// its way, a solid (or, on a wall, diode) block behind it, and no other
/// frame facing the same way in the same space (`others` are the boxes of
/// every other frame nearby).
pub fn survives(
    pos: BlockPos,
    facing: Facing,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
    mut others: impl Iterator<Item = [f64; 6]>,
) -> bool {
    let pop = frame_box(pos, facing, false);
    if !physics::no_collision(pop, block) {
        return false;
    }
    let (sx, sy, sz) = facing.step();
    let behind = BlockPos::new(pos.x - sx, pos.y - sy, pos.z - sz);
    let Some(state) = block(behind) else {
        return false;
    };
    if !state.is_solid() && !(facing.is_horizontal() && is_diode(state)) {
        return false;
    }
    let axis = thin_axis(pop);
    !others.any(|other| physics::intersects(other, pop) && thin_axis(other) == axis)
}

#[derive(Debug, Clone)]
struct Blow {
    kind: &'static DamageType,
    attacker: Option<(i32, EntityType)>,
}

#[derive(Debug)]
pub struct ItemFrame {
    pub base: EntityBase,
    pub block_pos: BlockPos,
    pub facing: Facing,
    item: Option<ItemStack>,
    rotation: u8,
    drop_chance: f32,
    fixed: bool,
    since_check: u32,
    blows: Vec<Blow>,
    changed: Vec<u8>,
}

impl ItemFrame {
    pub fn new(kind: EntityType, id: i32, uuid: u128, block_pos: BlockPos, facing: Facing) -> Self {
        let mut frame = Self {
            base: EntityBase::new(kind, id, uuid, [0.0; 3], [0.0; 3]),
            block_pos,
            facing,
            item: None,
            rotation: 0,
            drop_chance: 1.0,
            fixed: false,
            since_check: 0,
            blows: Vec::new(),
            changed: Vec::new(),
        };
        frame.place();
        frame.base.no_gravity = true;
        frame
    }

    pub fn item(&self) -> Option<&ItemStack> {
        self.item.as_ref()
    }

    pub fn rotation(&self) -> u8 {
        self.rotation
    }

    pub fn is_fixed(&self) -> bool {
        self.fixed
    }

    pub fn set_item(&mut self, item: Option<ItemStack>) {
        if let Some(old) = self.replace_item(item) {
            old.burn(BurnReason::Destroyed);
        }
    }

    /// Puts `item` (one of it) in the frame and hands back what it held.
    pub fn replace_item(&mut self, item: Option<ItemStack>) -> Option<ItemStack> {
        let item = item.map(|stack| stack.keep(1, BurnReason::Destroyed));
        let had_map = self.has_map();
        let old = std::mem::replace(&mut self.item, item);
        self.changed.push(DATA_ITEM);
        if had_map != self.has_map() {
            self.place();
        }
        old
    }

    pub fn set_rotation(&mut self, rotation: u8) {
        self.rotation = rotation % ROTATIONS;
        self.changed.push(DATA_ROTATION);
    }

    fn has_map(&self) -> bool {
        self.item
            .as_ref()
            .is_some_and(|stack| stack.components.map_id.is_some())
    }

    fn map_id(&self) -> Option<u32> {
        self.item
            .as_ref()
            .filter(|stack| {
                tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::item::key::CLONABLE_MAPS,
                    stack.item(),
                )
            })
            .and_then(|stack| stack.components.map_id)
    }

    fn map_rotation(&self) -> i32 {
        match self.facing {
            Facing::South => 0,
            Facing::West => 90,
            Facing::North => 180,
            Facing::East => 270,
            _ => -90,
        }
    }

    fn report_map(&self, cx: &TickContext) {
        if let Some(id) = self.map_id()
            && let Some(frames) = cx.shared.services.map_frames()
        {
            frames.framed(
                id,
                cx.dimension,
                self.block_pos,
                self.map_rotation(),
                self.base.id,
            );
        }
    }

    fn release_map(&self, item: Option<&ItemStack>, cx: &TickContext) {
        if let Some(id) = item.and_then(|stack| stack.components.map_id)
            && let Some(frames) = cx.shared.services.map_frames()
        {
            frames.unframed(id, self.block_pos, self.base.id);
        }
    }

    fn place(&mut self) {
        let bounds = frame_box(self.block_pos, self.facing, self.has_map());
        self.base.position = [
            (bounds[0] + bounds[3]) / 2.0,
            (bounds[1] + bounds[4]) / 2.0,
            (bounds[2] + bounds[5]) / 2.0,
        ];
        (self.base.yaw, self.base.pitch) = angles(self.facing);
    }

    fn signal(&self) -> u8 {
        if self.item.is_some() {
            self.rotation % ROTATIONS + 1
        } else {
            0
        }
    }

    fn frame_stack(&self) -> ItemStack {
        #[expect(
            clippy::disallowed_methods,
            reason = "entity drops or fires an item outside loot tables (frame_stack)"
        )]
        let mut stack = ItemStack::mint(frame_item(self.base.kind), 1, MintReason::EntityDrop);
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

    fn drop_stack(&self, stack: ItemStack, cx: &mut TickContext) {
        let (sx, _, sz) = self.facing.step();
        let [x, y, z] = self.base.position;
        let velocity = [
            rand::Rng::r#gen::<f64>(cx.rng) * 0.2 - 0.1,
            0.2,
            rand::Rng::r#gen::<f64>(cx.rng) * 0.2 - 0.1,
        ];
        cx.effects.push(Effect::DropItem {
            stack,
            position: [
                x + f64::from(sx) * DROP_SIDE_OFFSET,
                y,
                z + f64::from(sz) * DROP_SIDE_OFFSET,
            ],
            velocity,
            pickup_delay: BLOCK_DROP_PICKUP_DELAY,
        });
    }

    /// Drops what it holds (by chance) and, with `with_frame`, itself.
    fn drop_contents(
        &mut self,
        with_frame: bool,
        creative: bool,
        by: Option<i32>,
        cx: &mut TickContext,
    ) {
        if self.fixed {
            return;
        }
        let item = self.item.take();
        if item.is_some() {
            self.changed.push(DATA_ITEM);
            report_item(item.as_ref(), None, by, cx);
        }
        self.release_map(item.as_ref(), cx);
        if !cx.shared.rule_bool("entity_drops") || creative {
            return;
        }
        if with_frame {
            let stack = self.frame_stack();
            self.drop_stack(stack, cx);
        }
        if let Some(item) = item
            && rand::Rng::r#gen::<f32>(cx.rng) < self.drop_chance
        {
            self.drop_stack(item, cx);
        }
    }

    fn invulnerable_to(&self, blow: &Blow, creative: bool) -> bool {
        self.base.invulnerable && !bypasses(blow.kind) && !creative
    }

    /// Applies one hit; true when the frame is gone.
    fn resolve(&mut self, blow: &Blow, cx: &mut TickContext) -> bool {
        let creative = blow
            .attacker
            .and_then(|(id, _)| cx.players.iter().find(|p| p.id == id))
            .is_some_and(|p| p.creative);
        if self.fixed && !(bypasses(blow.kind) || creative) {
            return false;
        }
        if self.invulnerable_to(blow, creative) {
            return false;
        }
        let by = blow
            .attacker
            .filter(|(_, kind)| *kind == EntityType::Player)
            .map(|(id, _)| id);
        let explosion = damage_type_in_tag(blow.kind, "minecraft:is_explosion");
        if !self.fixed && !explosion && self.item.is_some() {
            self.drop_contents(false, creative, by, cx);
            self.sound(SoundEvent::EntityItemFrameRemoveItem, cx);
            return false;
        }
        if let Some((_, kind)) = blow.attacker
            && kind != EntityType::Player
            && is_mob(kind)
            && !cx.shared.rule_bool("mob_griefing")
        {
            return false;
        }
        self.sound(SoundEvent::EntityItemFrameBreak, cx);
        self.drop_contents(true, creative, by, cx);
        true
    }
}

fn bypasses(kind: &DamageType) -> bool {
    damage_type_in_tag(kind, "minecraft:bypasses_invulnerability")
}

fn is_mob(kind: EntityType) -> bool {
    kind.mob().category != tuff_vanilla_data::entity::MobCategory::Misc
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
    let facing = take_number(nbt, "Facing")
        .and_then(|id| facing_of_id(id as i64))
        .unwrap_or(Facing::Down);
    let kind = base.kind;
    let (id, uuid) = (base.id, base.uuid);
    let mut frame = ItemFrame::new(kind, id, uuid, block_pos, facing);
    let rest = base;
    frame.base = EntityBase {
        position: frame.base.position,
        yaw: frame.base.yaw,
        pitch: frame.base.pitch,
        no_gravity: true,
        ..rest
    };
    #[expect(clippy::disallowed_methods, reason = "decoding a saved stack (load)")]
    let minted = take(nbt, "Item", |tag| {
        ItemStack::from_storage(tag.as_compound()?)
    })
    .filter(|stack| stack.count() > 0);
    frame.item = minted;
    frame.rotation = take_number(nbt, "ItemRotation").map_or(0, |v| (v as i64).rem_euclid(8) as u8);
    frame.drop_chance = take_number(nbt, "ItemDropChance").map_or(1.0, |v| v as f32);
    frame.fixed = take_bool(nbt, "Fixed").unwrap_or(false);
    frame.place();
    Some(Box::new(frame))
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::ItemFrame, load);
    registry.register(EntityType::GlowItemFrame, load);
}

impl RegionEntity for ItemFrame {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn take_items(&mut self) -> Vec<ItemStack> {
        self.item.take().into_iter().collect()
    }

    fn held_items(&self) -> Option<Vec<Option<tuff_item::item::ItemView<'_>>>> {
        Some(vec![self.item.as_ref().map(ItemStack::view)])
    }

    fn set_held_items(&mut self, slots: Vec<(usize, Option<ItemStack>)>) -> bool {
        let mut found = false;
        for (slot, item) in slots {
            if slot == 0 && !found {
                found = true;
                if let Some(old) = self.replace_item(item) {
                    old.burn(BurnReason::Plugin);
                }
            } else if let Some(unused) = item {
                unused.burn(BurnReason::Plugin);
            }
        }
        found
    }

    fn save(&self, out: &mut Compound) {
        out.insert(
            "block_pos",
            Tag::IntArray(vec![self.block_pos.x, self.block_pos.y, self.block_pos.z]),
        );
        if let Some(item) = &self.item {
            out.insert("Item", item.to_storage());
        }
        out.insert("ItemRotation", self.rotation as i8);
        out.insert("ItemDropChance", self.drop_chance);
        out.insert("Facing", self.facing.id() as i8);
        out.insert("Fixed", self.fixed);
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        let mut values = Vec::new();
        if self.base.invisible {
            values.push((
                metadata::ENTITY_FLAGS,
                MetadataValue::Byte(self.base.flags() | metadata::FLAG_INVISIBLE),
            ));
        }
        values.push((
            DATA_DIRECTION,
            MetadataValue::Direction(self.facing.id() as i32),
        ));
        if self.item.is_some() {
            values.push((
                DATA_ITEM,
                MetadataValue::Slot(self.item.as_ref().map(tuff_item::item::ItemSnapshot::of)),
            ));
        }
        if self.rotation != 0 {
            values.push((DATA_ROTATION, MetadataValue::VarInt(self.rotation.into())));
        }
        values
    }

    fn take_changed_data(&mut self) -> Vec<(u8, MetadataValue)> {
        let mut changed = std::mem::take(&mut self.changed);
        changed.dedup();
        changed
            .into_iter()
            .map(|index| {
                let value = match index {
                    DATA_ITEM => MetadataValue::Slot(
                        self.item.as_ref().map(tuff_item::item::ItemSnapshot::of),
                    ),
                    _ => MetadataValue::VarInt(self.rotation.into()),
                };
                (index, value)
            })
            .collect()
    }

    fn resend_data(&mut self, index: u8) {
        if matches!(index, DATA_ITEM | DATA_ROTATION) {
            self.changed.push(index);
        }
    }

    fn spawn_data(&self) -> i32 {
        self.facing.id() as i32
    }

    fn bounds(&self) -> [f64; 6] {
        frame_box(self.block_pos, self.facing, self.has_map())
    }

    fn pickable(&self) -> bool {
        true
    }

    fn comparator_frame(&self) -> Option<(Facing, u8)> {
        Some((self.facing, self.signal()))
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

    fn kill(&mut self, cx: &mut TickContext) -> bool {
        self.release_map(self.item.as_ref(), cx);
        true
    }

    fn interact(
        &mut self,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        if self.fixed {
            return None;
        }
        if self.item.is_none() {
            let one = interaction.hand.take(1)?;
            self.set_item(Some(one));
            report_item(None, self.item.as_ref(), Some(interaction.player), cx);
            self.sound(SoundEvent::EntityItemFrameAddItem, cx);
            return Some(HandEffect::None);
        }
        self.sound(SoundEvent::EntityItemFrameRotateItem, cx);
        self.set_rotation(self.rotation + 1);
        Some(HandEffect::None)
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        for blow in std::mem::take(&mut self.blows) {
            if self.resolve(&blow, cx) {
                return Tick::Discard;
            }
        }
        if cx.tick.is_multiple_of(MAP_REPORT_TICKS) {
            self.report_map(cx);
        }
        self.since_check += 1;
        if self.since_check > CHECK_INTERVAL {
            self.since_check = 0;
            if !self.fixed {
                let others = cx
                    .entities
                    .iter()
                    .filter(|e| crate::world::painting::is_hanging(e.kind) && e.id != self.base.id)
                    .map(|e| e.bounds);
                let (sx, sy, sz) = self.facing.step();
                let behind = BlockPos::new(
                    self.block_pos.x - sx,
                    self.block_pos.y - sy,
                    self.block_pos.z - sz,
                );
                let known = (cx.block)(behind).is_some();
                if known && !survives(self.block_pos, self.facing, cx.block, others) {
                    self.drop_contents(true, false, None, cx);
                    return Tick::Discard;
                }
            }
        }
        Tick::Alive
    }
}

/// Tells container observers the frame's item went from `old` to `new`.
fn report_item(
    old: Option<&ItemStack>,
    new: Option<&ItemStack>,
    player: Option<i32>,
    cx: &mut TickContext,
) {
    cx.effects.push(Effect::HeldItems {
        player,
        slots: vec![SlotEdit::of(0, old, new)],
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;

    fn stone_wall_at(x: i32) -> impl Fn(BlockPos) -> Option<BlockState> {
        move |pos| {
            Some(if pos.x == x {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn data_indices_match_the_dumped_layout() {
        for kind in [EntityType::ItemFrame, EntityType::GlowItemFrame] {
            let mut frame = ItemFrame::new(kind, 1, 1, BlockPos::new(0, 64, 0), Facing::North);
            frame.set_item(Some(ItemStack::mint(Item::Diamond, 1, MintReason::Test)));
            frame.set_rotation(3);
            let layout = kind.data_layout();
            for (index, value) in frame.metadata() {
                if index == metadata::ENTITY_FLAGS {
                    continue;
                }
                assert_eq!(
                    layout[usize::from(index)],
                    value.type_id() as u8,
                    "index {index}"
                );
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_box_hugs_the_wall_and_grows_around_a_map() {
        let pos = BlockPos::new(10, 64, 10);
        let flat = frame_box(pos, Facing::East, false);
        assert_eq!(flat, [10.0, 64.125, 10.125, 10.0625, 64.875, 10.875]);
        let ceiling = frame_box(pos, Facing::Down, false);
        assert_eq!(ceiling[4], 65.0);
        assert_eq!(ceiling[4] - ceiling[1], 0.0625);
        let mapped = frame_box(pos, Facing::East, true);
        assert_eq!(mapped[5] - mapped[2], 1.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn it_needs_a_solid_wall_and_free_space() {
        let wall = stone_wall_at(9);
        let none = std::iter::empty();
        assert!(survives(
            BlockPos::new(10, 64, 10),
            Facing::East,
            &wall,
            none
        ));
        assert!(!survives(
            BlockPos::new(12, 64, 10),
            Facing::East,
            &wall,
            std::iter::empty()
        ));
        let neighbour = frame_box(BlockPos::new(10, 64, 10), Facing::East, false);
        assert!(!survives(
            BlockPos::new(10, 64, 10),
            Facing::East,
            &wall,
            std::iter::once(neighbour)
        ));
        let other_wall = frame_box(BlockPos::new(10, 64, 10), Facing::South, false);
        assert!(survives(
            BlockPos::new(10, 64, 10),
            Facing::East,
            &wall,
            std::iter::once(other_wall)
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_comparator_reads_the_rotation_of_a_held_item() {
        let mut frame = ItemFrame::new(
            EntityType::ItemFrame,
            1,
            1,
            BlockPos::new(0, 64, 0),
            Facing::South,
        );
        assert_eq!(frame.comparator_frame(), Some((Facing::South, 0)));
        frame.set_item(Some(ItemStack::mint(Item::Stick, 4, MintReason::Test)));
        assert_eq!(frame.item().map(|s| s.count()), Some(1));
        assert_eq!(frame.comparator_frame(), Some((Facing::South, 1)));
        frame.set_rotation(7);
        assert_eq!(frame.comparator_frame(), Some((Facing::South, 8)));
        frame.set_rotation(8);
        assert_eq!(frame.rotation(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn saved_frames_load_back_the_same() {
        let mut registry = tuff_core::world::entity::Registry::default();
        register(&mut registry);
        let mut frame = ItemFrame::new(
            EntityType::GlowItemFrame,
            1,
            7,
            BlockPos::new(3, 70, -4),
            Facing::Up,
        );
        frame.set_item(Some(ItemStack::mint(Item::Compass, 1, MintReason::Test)));
        frame.set_rotation(5);
        let nbt = tuff_core::world::entity::save(&frame).with("id", "minecraft:glow_item_frame");
        let loaded = registry.load(&nbt, 9).expect("loads");
        let loaded = loaded.downcast_ref::<ItemFrame>().expect("a frame");
        assert_eq!(loaded.block_pos, BlockPos::new(3, 70, -4));
        assert_eq!(loaded.facing, Facing::Up);
        assert_eq!(loaded.rotation(), 5);
        assert_eq!(loaded.item().map(|s| s.item()), Some(Item::Compass));
        assert_eq!(loaded.base.position, frame.base.position);
        assert_eq!(loaded.base.pitch, -90.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_fixed_frame_shrugs_off_ordinary_hits() {
        tuff_test_support::with_tick_context(|cx| {
            let mut frame = ItemFrame::new(
                EntityType::ItemFrame,
                1,
                1,
                BlockPos::new(0, 64, 0),
                Facing::North,
            );
            frame.fixed = true;
            frame.blows.push(Blow {
                kind: &damage_type::PLAYER_ATTACK,
                attacker: None,
            });
            assert!(matches!(frame.tick(cx), Tick::Alive));
            frame.blows.push(Blow {
                kind: &damage_type::GENERIC_KILL,
                attacker: None,
            });
            assert!(matches!(frame.tick(cx), Tick::Discard));
        });
    }
}
