//! Dropped item entities and their vanilla movement rules.

use tuff_item::item::DupUnchecked;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::registry::EntityType;

use crate::protocol::play::{MetadataValue, metadata};
use crate::world::contact::{Immersion, immersion};
use crate::world::entity::{
    Effect, EntityBase, RegionEntity, Tick, TickContext, take, take_number,
};
use crate::world::messages::PlayerId;
use crate::world::physics;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

const GRAVITY: f64 = 0.04;
const AIR_DRAG: f64 = 0.98;
const DRAG_F32: f32 = 0.98;
const DEFAULT_BLOCK_FRICTION: f32 = 0.6;
const FRICTION_PROBE: f32 = 0.500_001;
/// How deep water or lava must be around an item to float it.
const FLOAT_DEPTH: f64 = 0.10000000149011612;
const WATER_DRAG: f64 = 0.9900000095367432;
const LAVA_DRAG: f64 = 0.949999988079071;
const RISE: f32 = 5.0e-4;
const MAX_RISE: f64 = 0.05999999865889549;
pub const DESPAWN_AGE: i32 = 6000;
pub const INFINITE_LIFETIME: i32 = -32768;
pub const EXTENDED_LIFETIME_AGE: i32 = -6000;
pub const BLOCK_DROP_PICKUP_DELAY: u32 = 10;
const HALF_WIDTH: f64 = 0.125;
const DEFAULT_HEALTH: i16 = 5;
pub const NEVER_PICKUP: u32 = 32767;
const MERGE_PERIOD_MOVING: u32 = 2;
const MERGE_PERIOD_RESTING: u32 = 40;
const MERGE_REACH: f64 = 0.5;

#[derive(Debug)]
pub struct ItemEntity {
    pub base: EntityBase,
    pub stack: ItemStack,
    pub pickup_delay: u32,
    pub age: i32,
    pub health: i16,
    /// The offer outstanding for it. Not saved.
    pub reserved: Option<crate::world::entity::Reservation>,
    /// Players who turned the item down since it was last offered to
    /// everyone in range, so a full inventory does not keep it from the
    /// others. Not saved.
    pub refused_by: Vec<PlayerId>,
    /// Ticks since it was loaded or dropped. Not saved.
    pub ticks: u32,
}

/// One step of two dropped items merging across a region border. The
/// region giving items always takes them out of its own item before it
/// sends them, and the receiver adds them or drops them as a new item,
/// so no step can duplicate or lose a count.
#[derive(Debug)]
pub enum BorderMerge {
    /// An item due to merge whose `reach` meets the receiving region.
    Probe {
        from: tuff_world_model::pos::RegionPos,
        item: i32,
        stack: tuff_item::item::ItemSnapshot,
        owner: Option<tuff_nbt::Tag>,
        reach: [f64; 6],
    },
    /// The probing item is the smaller one: give up to `room` of it to
    /// `target` in region `to`, if it still holds `count`.
    Give {
        to: tuff_world_model::pos::RegionPos,
        item: i32,
        count: u8,
        target: i32,
        room: u8,
    },
    /// Items taken out of a neighbour's item, for `item`.
    Take {
        item: i32,
        stack: ItemStack,
        pickup_delay: u32,
        age: i32,
        at: [f64; 3],
    },
}

/// The saved key naming who threw or dropped an item.
pub const THROWER: &str = "Thrower";

pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let stack = take(nbt, "Item", |tag| {
        ItemStack::from_storage(tag.as_compound()?)
    })?;
    let short = |value: f64| (value as i16).max(0) as u32;
    let signed = |value: f64| i32::from(value as i16);
    Some(Box::new(ItemEntity {
        base,
        stack,
        pickup_delay: take_number(nbt, "PickupDelay").map_or(0, short),
        age: take_number(nbt, "Age").map_or(0, signed),
        health: take_number(nbt, "Health").map_or(DEFAULT_HEALTH, |v| v as i16),
        reserved: None,
        refused_by: Vec::new(),
        ticks: 0,
    }))
}

/// Whether an item or orb can not pass a block: anything with a
/// collision box (taken as a full cube), and unloaded blocks.
pub fn blocks_items(state: Option<BlockState>) -> bool {
    state.is_none_or(|state| !state.collision_boxes().is_empty())
}

impl ItemEntity {
    pub fn new(
        id: i32,
        uuid: u128,
        stack: ItemStack,
        position: [f64; 3],
        velocity: [f64; 3],
        pickup_delay: u32,
    ) -> Self {
        Self {
            base: EntityBase::new(EntityType::Item, id, uuid, position, velocity),
            stack,
            pickup_delay,
            age: 0,
            health: DEFAULT_HEALTH,
            reserved: None,
            refused_by: Vec::new(),
            ticks: 0,
        }
    }

    /// Whether it may take part in a merge at all. An item with an offer
    /// outstanding sits out so the offered count stays what it was.
    pub fn mergeable(&self) -> bool {
        self.pickup_delay != NEVER_PICKUP
            && self.age != INFINITE_LIFETIME
            && self.age < DESPAWN_AGE
            && self.reserved.is_none()
            && self.stack.count() < self.stack.max_stack_size()
    }

    /// Makes it never age and never despawn.
    pub fn set_unlimited_lifetime(&mut self) {
        self.age = INFINITE_LIFETIME;
    }

    /// Gives it a second life span on top of the usual one.
    pub fn set_extended_lifetime(&mut self) {
        self.age = EXTENDED_LIFETIME_AGE;
    }

    /// Whether this tick is one it looks for neighbours to merge with:
    /// every other tick after changing block, else every 40th.
    pub fn merge_due(&self, moved: bool) -> bool {
        let period = if moved {
            MERGE_PERIOD_MOVING
        } else {
            MERGE_PERIOD_RESTING
        };
        self.ticks.is_multiple_of(period) && self.mergeable()
    }

    /// The box other items must meet to merge with this one.
    pub fn merge_reach(&self) -> [f64; 6] {
        let mut reach = self.bounds();
        for axis in [0, 2] {
            reach[axis] -= MERGE_REACH;
            reach[axis + 3] += MERGE_REACH;
        }
        reach
    }

    /// Whether the two stacks fit in one, with the same owner.
    pub fn merges_with(&self, other: &ItemEntity) -> bool {
        self.base.extra.get("Owner") == other.base.extra.get("Owner")
            && u32::from(self.stack.count()) + u32::from(other.stack.count())
                <= u32::from(other.stack.max_stack_size())
            && self.stack.stacks_with(&other.stack)
    }

    /// Whether this one absorbs `other` rather than the reverse: the
    /// bigger stack wins, and ties go to `other`.
    pub fn absorbs(&self, other: &ItemEntity) -> bool {
        other.stack.count() < self.stack.count()
    }

    /// Takes what fits of `from`'s items (never past 64), keeping the
    /// longer pickup delay and the younger age; `from` is left with the
    /// rest.
    pub fn absorb(&mut self, from: &mut ItemEntity) {
        let cap = self.stack.max_stack_size().min(64);
        let room = cap.saturating_sub(self.stack.count());
        self.stack.transfer(&mut from.stack, room);
        self.pickup_delay = self.pickup_delay.max(from.pickup_delay);
        self.age = self.age.min(from.age);
    }

    /// Advances one tick out of any fluid, against the blocks `block` gives.
    pub fn step_tick(&mut self, block: &dyn Fn(BlockPos) -> Option<BlockState>) {
        self.step_in_fluids(block, &Immersion::default(), &mut StdRng::seed_from_u64(0));
    }

    /// Sets it moving towards the nearest side (never down) whose block is
    /// not a full cube, at a random 0.1 to 0.3 blocks per tick.
    fn push_towards_closest_space(
        &mut self,
        block: &dyn Fn(BlockPos) -> Option<BlockState>,
        rng: &mut StdRng,
    ) {
        let bounds = self.base.bounds();
        let at = [
            self.base.position[0],
            (bounds[1] + bounds[4]) / 2.0,
            self.base.position[2],
        ];
        let cell = BlockPos::new(
            at[0].floor() as i32,
            at[1].floor() as i32,
            at[2].floor() as i32,
        );
        let delta = [
            at[0] - f64::from(cell.x),
            at[1] - f64::from(cell.y),
            at[2] - f64::from(cell.z),
        ];
        let mut closest = (1, 1.0, f64::MAX);
        for (axis, step) in [(2, -1), (2, 1), (0, -1), (0, 1), (1, 1)] {
            let mut neighbour = [cell.x, cell.y, cell.z];
            neighbour[axis] += step;
            let full = block(BlockPos::new(neighbour[0], neighbour[1], neighbour[2]))
                .is_some_and(|state| state.is_collision_full_block());
            if full {
                continue;
            }
            let distance = if step > 0 {
                1.0 - delta[axis]
            } else {
                delta[axis]
            };
            if distance < closest.2 {
                closest = (axis, f64::from(step), distance);
            }
        }
        let speed = f64::from(rng.gen_range(0.0f32..1.0) * 0.2 + 0.1);
        let velocity = &mut self.base.velocity;
        for (axis, value) in velocity.iter_mut().enumerate() {
            if axis == closest.0 {
                *value = closest.1 * speed;
            } else {
                *value *= 0.75;
            }
        }
    }

    /// Advances one tick in the fluids `immersion` describes: currents push
    /// the item, and water or lava deep enough to hold it replaces gravity
    /// with buoyancy and its own drag.
    pub fn step_in_fluids(
        &mut self,
        block: &dyn Fn(BlockPos) -> Option<BlockState>,
        immersion: &Immersion,
        rng: &mut StdRng,
    ) {
        if self.age != INFINITE_LIFETIME {
            self.age += 1;
        }
        self.ticks += 1;
        if self.pickup_delay != NEVER_PICKUP {
            self.pickup_delay = self.pickup_delay.saturating_sub(1);
        }
        immersion.push(&mut self.base.velocity);
        let depth = &immersion.submersion;
        let fluid_drag = if depth.water > FLOAT_DEPTH {
            Some(WATER_DRAG)
        } else if depth.lava > FLOAT_DEPTH {
            Some(LAVA_DRAG)
        } else {
            None
        };
        let gravity = if self.base.no_gravity || fluid_drag.is_some() {
            0.0
        } else {
            GRAVITY
        };
        if let Some(drag) = fluid_drag {
            let velocity = &mut self.base.velocity;
            velocity[0] *= drag;
            velocity[2] *= drag;
            if velocity[1] < MAX_RISE {
                velocity[1] += f64::from(RISE);
            }
        }
        self.base.velocity[1] -= gravity;
        let inside = physics::colliders(physics::inflate(self.base.bounds(), [-1.0e-7; 3]), block);
        let no_physics = !inside.is_empty();
        if no_physics {
            self.push_towards_closest_space(block, rng);
        }
        let movement = self.base.velocity;
        let moved = if no_physics {
            movement
        } else {
            physics::collide(
                self.base.bounds(),
                movement,
                0.0,
                self.base.on_ground,
                block,
            )
        };
        for axis in 0..3 {
            self.base.position[axis] += moved[axis];
            if moved[axis] != movement[axis] {
                self.base.velocity[axis] = 0.0;
            }
        }
        self.base.on_ground = !no_physics && movement[1] < 0.0 && moved[1] != movement[1];

        let friction = if self.base.on_ground {
            let below = crate::world::block_contact::on_pos(
                self.base.position,
                None,
                FRICTION_PROBE,
                block,
            );
            let slip = block(below).map_or(DEFAULT_BLOCK_FRICTION, |state| {
                state.block().movement().friction
            });
            f64::from(DRAG_F32 * slip)
        } else {
            AIR_DRAG
        };
        self.base.velocity[0] *= friction;
        self.base.velocity[1] *= AIR_DRAG;
        self.base.velocity[2] *= friction;
        if self.base.on_ground && self.base.velocity[1] < 0.0 {
            self.base.velocity[1] *= -0.5;
        }
    }

    pub fn bounds(&self) -> [f64; 6] {
        self.base.bounds()
    }

    /// Whether a player standing at `feet` is close enough to pick this up:
    /// the player's box inflated by 1 block horizontally and half a block
    /// vertically, against the item's box.
    /// The only player who may pick this up, if one is named.
    pub fn owner(&self) -> Option<u128> {
        self.base.extra.get("Owner").and_then(|tag| tag.as_uuid())
    }

    /// Who threw or dropped it, if anyone.
    pub fn thrower(&self) -> Option<u128> {
        self.base.extra.get(THROWER).and_then(|tag| tag.as_uuid())
    }

    pub fn in_pickup_range(&self, feet: [f64; 3]) -> bool {
        let reach = 0.3 + 1.0 + HALF_WIDTH;
        let [x, y, z] = self.base.position;
        (x - feet[0]).abs() <= reach
            && (z - feet[2]).abs() <= reach
            && y + 0.25 >= feet[1] - 0.5
            && y <= feet[1] + 1.8 + 0.5
    }
}

impl RegionEntity for ItemEntity {
    fn take_items(&mut self) -> Vec<ItemStack> {
        let stack = std::mem::replace(&mut self.stack, ItemStack::empty());
        if stack.is_empty() {
            Vec::new()
        } else {
            vec![stack]
        }
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        out.insert("Health", self.health);
        out.insert(
            "Age",
            self.age.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16,
        );
        out.insert("PickupDelay", self.pickup_delay.min(i16::MAX as u32) as i16);
        out.insert("Item", self.stack.to_nbt());
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![(
            metadata::ITEM_STACK,
            MetadataValue::Slot(Some(tuff_item::item::ItemSnapshot::of(&self.stack))),
        )]
    }

    fn display_name(&self) -> String {
        self.stack.item.name().to_owned()
    }

    fn resists(&self, damage_type: &'static DamageType) -> bool {
        self.stack
            .item
            .damage_resistant()
            .is_some_and(|tag| crate::world::damage_type_in_tag(damage_type, tag))
    }

    fn spilled_contents(&mut self) -> Vec<ItemStack> {
        let mut spilled: Vec<ItemStack> = self
            .stack
            .components
            .container
            .take()
            .into_iter()
            .flatten()
            .flatten()
            .collect();
        if let Some(bundle) = self.stack.components.bundle_contents.take() {
            spilled.extend(bundle.items().iter().map(DupUnchecked::dup_unchecked));
        }
        spilled
    }

    fn hurt(&mut self, amount: f32, damage_type: &'static DamageType) -> bool {
        if self.resists(damage_type) {
            return false;
        }
        self.health = (f32::from(self.health) - amount) as i16;
        self.health <= 0
    }

    fn tick(&mut self, context: &mut TickContext) -> Tick {
        let bounds = self.bounds();
        let eye = self.base.eye_position();
        let immersion = immersion(context, bounds, eye);
        let start = self.base.position;
        let webbed = in_cobweb(self.bounds(), context.block);
        self.step_in_fluids(context.block, &immersion, context.rng);
        if webbed {
            hold_in_cobweb(&mut self.base, start);
        }
        crate::world::physics::stop_at_moving_blocks(&mut self.base, start, context.pistons);
        let bounds = self.bounds();
        if crate::world::block_contact::bubble_column_push(
            bounds,
            &mut self.base.velocity,
            context.block,
        ) {
            self.base.fall_distance = 0.0;
        }
        if crate::world::block_contact::pricked_by_cactus(self, context.block) {
            let position = self.base.position;
            for stack in self.spilled_contents() {
                context.effects.push(Effect::DropItem {
                    stack,
                    position,
                    velocity: [0.0; 3],
                    pickup_delay: 10,
                });
            }
            return Tick::Discard;
        }
        if self.age >= DESPAWN_AGE {
            Tick::Discard
        } else {
            Tick::Alive
        }
    }
}

/// Whether a cobweb sits inside `bounds` (shrunk a hair like block contact).
pub fn in_cobweb(bounds: [f64; 6], block: &dyn Fn(BlockPos) -> Option<BlockState>) -> bool {
    const INSET: f64 = 1.0e-7;
    let lo = [bounds[0] + INSET, bounds[1] + INSET, bounds[2] + INSET];
    let hi = [bounds[3] - INSET, bounds[4] - INSET, bounds[5] - INSET];
    (lo[0].floor() as i32..=hi[0].floor() as i32).any(|x| {
        (lo[1].floor() as i32..=hi[1].floor() as i32).any(|y| {
            (lo[2].floor() as i32..=hi[2].floor() as i32).any(|z| {
                block(BlockPos::new(x, y, z))
                    .is_some_and(|s| s.block() == tuff_vanilla_data::Block::Cobweb)
            })
        })
    })
}

/// Scales this tick's displacement from `start` by the cobweb multiplier
/// and drops the velocity, as a thing stuck in a web.
pub fn hold_in_cobweb(base: &mut EntityBase, start: [f64; 3]) {
    let scale = crate::world::block_contact::Stuck::Cobweb.multiplier(false);
    for axis in 0..3 {
        base.position[axis] = start[axis] + (base.position[axis] - start[axis]) * scale[axis];
    }
    base.velocity = [0.0; 3];
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::BurnReason;
    use tuff_item::item::MintReason;
    use tuff_nbt::Tag;
    use tuff_vanilla_data::{Block, Item};

    #[tuff_core::watchdog]
    #[test]
    fn cobweb_holds_back_an_item_move() {
        let mut base = EntityBase::new(EntityType::Item, 1, 1, [0.0; 3], [0.0; 3]);
        let start = [0.5, 64.0, 0.5];
        base.position = [1.5, 63.0, 0.5];
        base.velocity = [1.0, -1.0, 0.0];
        hold_in_cobweb(&mut base, start);
        assert_eq!(base.velocity, [0.0; 3]);
        assert!((base.position[0] - 0.75).abs() < 1e-9);
        assert!((base.position[1] - 63.95).abs() < 1e-6);
        let web = |_: BlockPos| Some(tuff_vanilla_data::Block::Cobweb.default_state());
        assert!(in_cobweb([0.2, 64.0, 0.2, 0.8, 64.25, 0.8], &web));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_destroyed_shulker_box_gives_up_its_contents() {
        let mut shulker = ItemStack::mint(Item::ShulkerBox, 1, MintReason::Test);
        shulker.components.container = Some(vec![
            None,
            Some(ItemStack::mint(Item::Stone, 3, MintReason::Test)),
            Some(ItemStack::mint(Item::Dirt, 1, MintReason::Test)),
        ]);
        let mut entity = ItemEntity::new(1, 1, shulker, [0.5, 65.0, 0.5], [0.0; 3], 0);
        let spilled = entity.spilled_contents();
        assert_eq!(spilled.len(), 2);
        assert_eq!(spilled[0].item, Item::Stone);
        assert!(entity.spilled_contents().is_empty());
    }

    fn solid_where(
        solid: impl Fn(i32, i32, i32) -> bool,
    ) -> impl Fn(BlockPos) -> Option<BlockState> {
        move |pos| {
            Some(if solid(pos.x, pos.y, pos.z) {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        }
    }

    fn item(position: [f64; 3], velocity: [f64; 3]) -> ItemEntity {
        ItemEntity::new(
            1,
            1,
            ItemStack::mint(Item::Dirt, 1, MintReason::Test),
            position,
            velocity,
            10,
        )
    }

    fn in_water(current: [f64; 3]) -> Immersion {
        Immersion {
            submersion: crate::world::contact::Submersion {
                water: 1.0,
                water_current: crate::world::contact::Current {
                    sum: current,
                    count: 1,
                },
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn items_rise_in_deep_water_with_its_drag() {
        let mut entity = item([0.5, 60.0, 0.5], [0.1, -0.2, 0.0]);
        entity.step_in_fluids(
            &|_| Some(Block::Air.default_state()),
            &in_water([0.0; 3]),
            &mut StdRng::seed_from_u64(0),
        );
        assert!((entity.base.velocity[0] - 0.1 * WATER_DRAG * AIR_DRAG).abs() < 1e-12);
        assert!((entity.base.velocity[1] - (-0.2 + f64::from(RISE)) * AIR_DRAG).abs() < 1e-12);
        for _ in 0..400 {
            entity.step_in_fluids(
                &|_| Some(Block::Air.default_state()),
                &in_water([0.0; 3]),
                &mut StdRng::seed_from_u64(0),
            );
        }
        assert!(entity.base.velocity[1] > 0.0, "the item floats upward");
        assert!(entity.base.velocity[1] < MAX_RISE);
    }

    #[tuff_core::watchdog]
    #[test]
    fn shallow_water_does_not_float_items() {
        let mut shallow = in_water([0.0; 3]);
        shallow.submersion.water = 0.05;
        let mut entity = item([0.5, 60.0, 0.5], [0.0; 3]);
        entity.step_in_fluids(
            &|_| Some(Block::Air.default_state()),
            &shallow,
            &mut StdRng::seed_from_u64(0),
        );
        assert!((entity.base.velocity[1] + GRAVITY * AIR_DRAG).abs() < 1e-12);
    }

    #[tuff_core::watchdog]
    #[test]
    fn lava_floats_items_with_heavier_drag() {
        let lava = Immersion {
            submersion: crate::world::contact::Submersion {
                lava: 0.5,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut entity = item([0.5, 60.0, 0.5], [0.1, 0.0, 0.0]);
        entity.step_in_fluids(
            &|_| Some(Block::Air.default_state()),
            &lava,
            &mut StdRng::seed_from_u64(0),
        );
        assert!((entity.base.velocity[0] - 0.1 * LAVA_DRAG * AIR_DRAG).abs() < 1e-12);
        assert!(entity.base.velocity[1] > 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn currents_carry_items_along_the_flow() {
        let mut entity = item([0.5, 60.0, 0.5], [0.0; 3]);
        entity.step_in_fluids(
            &|_| Some(Block::Air.default_state()),
            &in_water([1.0, 0.0, 0.0]),
            &mut StdRng::seed_from_u64(0),
        );
        let expected = crate::world::contact::WATER_CURRENT_SCALE * WATER_DRAG * AIR_DRAG;
        assert!((entity.base.velocity[0] - expected).abs() < 1e-12);
        for _ in 0..20 {
            entity.step_in_fluids(
                &|_| Some(Block::Air.default_state()),
                &in_water([1.0, 0.0, 0.0]),
                &mut StdRng::seed_from_u64(0),
            );
        }
        assert!(entity.base.position[0] > 0.6);
        assert_eq!(entity.base.velocity[2], 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_weak_current_still_nudges_a_resting_item() {
        let mut entity = item([0.5, 60.0, 0.5], [0.0; 3]);
        let mut slow = in_water([0.1, 0.0, 0.0]);
        slow.submersion.water = 1.0;
        let mut velocity = [0.0; 3];
        slow.submersion.water_current.push(&mut velocity, 0.001);
        assert!((velocity[0] - 0.0045).abs() < 1e-9);
        entity.step_in_fluids(
            &|_| Some(Block::Air.default_state()),
            &slow,
            &mut StdRng::seed_from_u64(0),
        );
        assert!(entity.base.velocity[0] > 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn unlimited_lifetime_never_ages_and_extended_starts_before_zero() {
        let mut entity = item([0.5, 80.0, 0.5], [0.0; 3]);
        entity.set_unlimited_lifetime();
        for _ in 0..5 {
            entity.step_tick(&|_| Some(Block::Air.default_state()));
        }
        assert_eq!(entity.age, INFINITE_LIFETIME);
        assert!(!entity.mergeable());
        entity.set_extended_lifetime();
        entity.step_tick(&|_| Some(Block::Air.default_state()));
        assert_eq!(entity.age, -5999);
        assert!(entity.mergeable());
    }

    #[tuff_core::watchdog]
    #[test]
    fn falls_and_settles_on_ground() {
        let floor = |_x, y, _z| y <= 71;
        let mut entity = item([0.5, 75.0, 0.5], [0.0, 0.0, 0.0]);
        for _ in 0..100 {
            entity.step_tick(&solid_where(floor));
        }
        assert!(entity.base.on_ground);
        assert!((entity.base.position[1] - 72.0).abs() < 1e-9);
        assert_eq!(entity.pickup_delay, 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn items_rest_on_the_real_height_of_a_carpet() {
        let level = |pos: BlockPos| {
            Some(match pos.y {
                ..=71 => Block::Stone.default_state(),
                72 => Block::WhiteCarpet.default_state(),
                _ => Block::Air.default_state(),
            })
        };
        let mut entity = item([0.5, 75.0, 0.5], [0.0; 3]);
        for _ in 0..100 {
            entity.step_tick(&level);
        }
        assert!(entity.base.on_ground);
        assert!((entity.base.position[1] - 72.0625).abs() < 1e-6);
    }

    #[tuff_core::watchdog]
    #[test]
    fn items_slide_further_on_ice_than_on_stone() {
        let slide = |floor: Block| {
            let level = move |pos: BlockPos| {
                Some(if pos.y <= 71 {
                    floor.default_state()
                } else {
                    Block::Air.default_state()
                })
            };
            let mut entity = item([0.5, 72.0, 0.5], [0.3, 0.0, 0.0]);
            entity.base.on_ground = true;
            for _ in 0..20 {
                entity.step_tick(&level);
            }
            entity.base.position[0]
        };
        assert!(slide(Block::Ice) > slide(Block::Stone) + 1.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn walls_stop_horizontal_motion() {
        let wall = |x, y, _z| y <= 71 || x >= 2;
        let mut entity = item([0.5, 72.0, 0.5], [0.5, 0.0, 0.0]);
        for _ in 0..40 {
            entity.step_tick(&solid_where(wall));
        }
        assert!(entity.base.position[0] < 2.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn item_inside_block_is_pushed_out_sideways_towards_the_nearest_free_side() {
        let level = solid_where(|x, y, z| y <= 71 || (x, y, z) == (0, 72, 0));
        let mut entity = item([0.2, 72.0, 0.5], [0.0; 3]);
        entity.step_tick(&level);
        assert!(entity.base.velocity[0] < 0.0);
        assert!(entity.base.velocity[1] <= 0.0);
        for _ in 0..20 {
            entity.step_tick(&level);
        }
        let cell = [0.0, 72.0, 0.0, 1.0, 73.0, 1.0];
        assert!(!physics::intersects(entity.base.bounds(), cell));
    }

    #[tuff_core::watchdog]
    #[test]
    fn cauldrons_put_out_burning_items_or_set_them_alight() {
        let cauldron = |block: tuff_vanilla_data::Block| {
            move |pos: BlockPos| {
                Some(if pos.y == 64 {
                    block.default_state()
                } else {
                    tuff_vanilla_data::Block::Air.default_state()
                })
            }
        };
        let mut changed = false;
        let mut burning = item([0.5, 64.1, 0.5], [0.0; 3]);
        burning.base.fire = 100;
        let water = cauldron(tuff_vanilla_data::Block::WaterCauldron);
        let tick = crate::world::entity::base_tick(&mut burning, &mut changed, false, &water);
        assert_eq!(tick.lower_cauldron, Some(BlockPos::new(0, 64, 0)));
        assert_eq!(burning.base.fire, 0);

        let mut dry = item([0.5, 64.1, 0.5], [0.0; 3]);
        let tick = crate::world::entity::base_tick(&mut dry, &mut changed, false, &water);
        assert_eq!(tick.lower_cauldron, None);

        let mut above = item([0.5, 64.9, 0.5], [0.0; 3]);
        above.base.fire = 100;
        let tick = crate::world::entity::base_tick(&mut above, &mut changed, false, &water);
        assert_eq!(
            tick.lower_cauldron, None,
            "above the contents is not inside"
        );

        let lava = cauldron(tuff_vanilla_data::Block::LavaCauldron);
        let mut item = item([0.5, 64.5, 0.5], [0.0; 3]);
        let tick = crate::world::entity::base_tick(&mut item, &mut changed, false, &lava);
        assert!(tick.burned);
        assert_eq!(item.base.fire, 300);
    }

    #[tuff_core::watchdog]
    #[test]
    fn items_burn_away_in_lava_unless_fire_resistant() {
        let lava = |_: BlockPos| Some(tuff_vanilla_data::Block::Lava.default_state());
        let mut changed = false;
        let mut dirt = item([0.5, 64.2, 0.5], [0.0; 3]);
        let first = crate::world::entity::base_tick(&mut dirt, &mut changed, false, &lava);
        assert!(!first.destroyed && first.burned);
        assert!(changed, "the fire flag is sent");
        assert_eq!(dirt.base.fire, 300);
        assert!(crate::world::entity::base_tick(&mut dirt, &mut changed, false, &lava).destroyed);

        let mut ingot = ItemEntity::new(
            2,
            2,
            ItemStack::mint(Item::NetheriteIngot, 1, MintReason::Test),
            [0.5, 64.2, 0.5],
            [0.0; 3],
            10,
        );
        for _ in 0..10 {
            let tick = crate::world::entity::base_tick(&mut ingot, &mut changed, false, &lava);
            assert!(
                !tick.destroyed && !tick.burned,
                "fire resistant items do not sizzle"
            );
        }
        let air = |_: BlockPos| Some(tuff_vanilla_data::Block::Air.default_state());
        let mut dry = item([0.5, 64.2, 0.5], [0.0; 3]);
        assert!(!crate::world::entity::base_tick(&mut dry, &mut changed, false, &air).destroyed);
        assert_eq!(dry.base.fire, 0);

        dry.base.fire = 100;
        let rained = crate::world::entity::base_tick(&mut dry, &mut changed, true, &air);
        assert!(rained.extinguished);
        assert_eq!(dry.base.fire, 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn water_puts_out_a_burning_item() {
        let water = |pos: BlockPos| {
            Some(if pos.y == 64 {
                tuff_vanilla_data::Block::Water.default_state()
            } else {
                tuff_vanilla_data::Block::Air.default_state()
            })
        };
        let mut changed = false;
        let mut wet = item([0.5, 64.2, 0.5], [0.0; 3]);
        wet.base.fire = 100;
        let tick = crate::world::entity::base_tick(&mut wet, &mut changed, false, &water);
        assert!(tick.extinguished, "with the extinguish sound");
        assert_eq!(wet.base.fire, 0);
        assert!(changed, "the fire flag is cleared for viewers");

        let mut above = item([0.5, 66.0, 0.5], [0.0; 3]);
        above.base.fire = 100;
        let tick = crate::world::entity::base_tick(&mut above, &mut changed, false, &water);
        assert!(!tick.extinguished);
        assert_eq!(above.base.fire, 99, "out of the water it keeps burning");
    }

    #[tuff_core::watchdog]
    #[test]
    fn pickup_range_matches_inflated_player_box() {
        let entity = item([1.0, 72.0, 0.0], [0.0; 3]);
        assert!(entity.in_pickup_range([0.0, 72.0, 0.0]));
        assert!(!entity.in_pickup_range([3.0, 72.0, 0.0]));
        assert!(!entity.in_pickup_range([0.0, 76.0, 0.0]));
    }

    fn stack_of(item: Item, count: u8) -> ItemEntity {
        let mut entity = ItemEntity::new(
            1,
            1,
            ItemStack::mint(item, count, MintReason::Test),
            [0.5; 3],
            [0.0; 3],
            0,
        );
        entity
            .stack
            .resize(count, MintReason::Test, BurnReason::Test);
        entity
    }

    #[tuff_core::watchdog]
    #[test]
    fn merging_needs_same_item_room_and_owner() {
        let dirt = stack_of(Item::Dirt, 30);
        assert!(dirt.merges_with(&stack_of(Item::Dirt, 34)));
        assert!(!dirt.merges_with(&stack_of(Item::Dirt, 35)));
        assert!(!dirt.merges_with(&stack_of(Item::Stone, 1)));
        let mut owned = stack_of(Item::Dirt, 1);
        owned
            .base
            .extra
            .insert("Owner", Tag::IntArray(vec![1, 2, 3, 4]));
        assert!(!dirt.merges_with(&owned));
        assert!(!stack_of(Item::Dirt, 64).mergeable());
        let mut never = stack_of(Item::Dirt, 1);
        never.pickup_delay = NEVER_PICKUP;
        assert!(!never.mergeable());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_bigger_stack_absorbs_keeping_the_longer_delay_and_younger_age() {
        let mut big = stack_of(Item::Dirt, 10);
        let mut small = stack_of(Item::Dirt, 4);
        big.age = 500;
        small.age = 20;
        small.pickup_delay = 30;
        assert!(big.absorbs(&small));
        assert!(!small.absorbs(&big));
        assert!(
            !stack_of(Item::Dirt, 4).absorbs(&small),
            "ties go to the other"
        );
        big.absorb(&mut small);
        assert_eq!((big.stack.count(), small.stack.count()), (14, 0));
        assert_eq!((big.age, big.pickup_delay), (20, 30));
    }

    #[tuff_core::watchdog]
    #[test]
    fn merges_are_scanned_every_other_tick_while_moving_else_every_fortieth() {
        let mut entity = stack_of(Item::Dirt, 1);
        let due = |entity: &ItemEntity, moved| entity.merge_due(moved);
        entity.ticks = 2;
        assert!(due(&entity, true));
        assert!(!due(&entity, false));
        entity.ticks = 40;
        assert!(due(&entity, false));
        entity.ticks = 41;
        assert!(!due(&entity, true));
        let reach = entity.merge_reach();
        assert_eq!(reach[1], entity.bounds()[1]);
        assert_eq!(reach[0], entity.bounds()[0] - 0.5);
    }
}
