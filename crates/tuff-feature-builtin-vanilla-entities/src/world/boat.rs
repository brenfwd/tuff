//! Boats, chest boats and rafts: vehicles a player rides and steers from
//! its own client. The region simulates a boat nobody drives (floating,
//! drifting, sliding on ice, falling) and keeps its damage, which breaks
//! it into its item.

use rand::Rng;
use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState, Item, tag};

use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::block_contact;
use tuff_core::world::block_entity::{self, decode_items, encode_items};
use tuff_core::world::contact as fluid;
use tuff_core::world::entity::{
    AttackOutcome, Effect, EntityAttack, EntityBase, EntityHit, RegionEntity, Tick, TickContext,
};
use tuff_core::world::physics::{self, Aabb, BlockLookup};
use tuff_core::world::{framed, packets};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_common::dismount;
use tuff_vanilla_common::shadow::{self, SHADOW_RANGE, distance};
use tuff_world_model::BlockPos;

const DATA_HURT: u8 = 8;
const DATA_HURT_DIR: u8 = 9;
const DATA_DAMAGE: u8 = 10;
const DATA_PADDLE_LEFT: u8 = 11;
const DATA_PADDLE_RIGHT: u8 = 12;
const DATA_BUBBLE_TIME: u8 = 13;
/// Ticks atop a bubble column before it is tossed up or pulled under.
const BUBBLE_TIME: i32 = 60;
const GRAVITY: f64 = 0.04;
/// Ticks under water before it throws its riders off.
const TIME_TO_EJECT: f32 = 60.0;
const PADDLE_SPEED: f32 = std::f32::consts::FRAC_PI_8;
const PADDLE_SOUND_TIME: f64 = 0.785_398_185_253_143_3;
/// Damage past which a hit breaks it.
const BREAK_DAMAGE: f32 = 40.0;
const WATER: tuff_vanilla_data::tag::Key<tuff_vanilla_data::fluid::Fluid> = tag::fluid::key::WATER;
const CHEST_SLOTS: usize = 27;
const ITEMS: &str = "Items";
const LOOT_TABLE: &str = "LootTable";
const LOOT_TABLE_SEED: &str = "LootTableSeed";

/// The server's own run of a boat its driver's client moves: reports are
/// held within range of it.
#[derive(Debug, Clone, Copy)]
struct Shadow {
    position: [f64; 3],
    velocity: [f64; 3],
    on_ground: bool,
    status: Status,
    old_status: Status,
    water_level: f64,
    /// Where the live boat was left, so a shove since then moves the
    /// shadow too.
    held: [f64; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    InWater,
    UnderWater,
    UnderFlowingWater,
    OnLand,
    InAir,
}

/// Whether `kind` is a boat or raft, with or without a chest.
pub fn is_boat(kind: EntityType) -> bool {
    Item::of_boat(kind).is_some()
}

fn has_chest(kind: EntityType) -> bool {
    kind.name().contains("_chest_")
}

#[derive(Debug)]
pub struct Boat {
    pub base: EntityBase,
    hurt_time: i32,
    hurt_dir: i32,
    damage: f32,
    paddles: [bool; 2],
    paddle_positions: [f32; 2],
    status: Status,
    old_status: Status,
    water_level: f64,
    land_friction: f32,
    out_of_control: f32,
    bubble_time: i32,
    above_bubble_column: bool,
    bubble_column_down: bool,
    /// Broken by a hit: gone on its next tick.
    broken: bool,
    changed: Vec<u8>,
    /// A chest boat's slots; empty for a plain boat.
    items: Vec<Option<ItemStack>>,
    loot_table: Option<(String, i64)>,
    leash: Option<tuff_core::world::leash::Leash>,
    shadow: Option<Shadow>,
}

/// A boat's lead is held at its four corners.
const LEASH: tuff_core::world::leash::LeashProfile = tuff_core::world::leash::LeashProfile {
    elastic: 6.0,
    snap: 12.0,
    quad: true,
};

impl Boat {
    pub fn new(
        kind: EntityType,
        id: i32,
        position: [f64; 3],
        yaw: f32,
        rng: &mut impl Rng,
    ) -> Self {
        let mut base = EntityBase::new(kind, id, rng.r#gen(), position, [0.0; 3]);
        base.yaw = yaw;
        Self::with_base(base)
    }

    fn with_base(base: EntityBase) -> Self {
        let slots = if has_chest(base.kind) { CHEST_SLOTS } else { 0 };
        Self {
            base,
            hurt_time: 0,
            hurt_dir: 1,
            damage: 0.0,
            paddles: [false; 2],
            paddle_positions: [0.0; 2],
            status: Status::InAir,
            old_status: Status::InAir,
            water_level: 0.0,
            land_friction: 0.0,
            out_of_control: 0.0,
            bubble_time: 0,
            above_bubble_column: false,
            bubble_column_down: false,
            broken: false,
            changed: Vec::new(),
            items: (0..slots).map(|_| None).collect(),
            loot_table: None,
            leash: None,
            shadow: None,
        }
    }

    fn value(&self, index: u8) -> Option<MetadataValue> {
        Some(match index {
            DATA_HURT => MetadataValue::VarInt(self.hurt_time),
            DATA_HURT_DIR => MetadataValue::VarInt(self.hurt_dir),
            DATA_DAMAGE => MetadataValue::Float(self.damage),
            DATA_PADDLE_LEFT => MetadataValue::Boolean(self.paddles[0]),
            DATA_PADDLE_RIGHT => MetadataValue::Boolean(self.paddles[1]),
            DATA_BUBBLE_TIME => MetadataValue::VarInt(self.bubble_time),
            _ => return None,
        })
    }

    fn set_paddles(&mut self, paddles: [bool; 2]) {
        for (side, index) in [(0, DATA_PADDLE_LEFT), (1, DATA_PADDLE_RIGHT)] {
            if self.paddles[side] != paddles[side] {
                self.paddles[side] = paddles[side];
                self.changed.push(index);
            }
        }
    }

    fn set_bubble_time(&mut self, time: i32) {
        if self.bubble_time != time {
            self.bubble_time = time;
            self.changed.push(DATA_BUBBLE_TIME);
        }
    }

    /// Notes the bubble columns it overlaps: the top cell of one starts
    /// the countdown, a cell below the top pushes a boat nobody drives.
    fn touch_bubble_columns(&mut self, block: BlockLookup) {
        let driven = self.driven();
        let bounds = self.base.bounds();
        let mut velocity = self.base.velocity;
        let mut top = None;
        block_contact::for_each_bubble_column(bounds, block, |cell| {
            if cell.open_above {
                top = Some(cell.drag);
            } else if !driven {
                velocity[1] = cell.capped_push(velocity[1]);
            }
        });
        self.base.velocity = velocity;
        if let Some(down) = top {
            self.above_bubble_column = true;
            self.bubble_column_down = down;
            if self.bubble_time == 0 {
                self.set_bubble_time(BUBBLE_TIME);
            }
        }
    }

    /// Counts down while atop a column; at zero a downward column throws
    /// the riders off and pulls the boat under, an upward one tosses it.
    fn tick_bubble_column(&mut self, cx: &mut TickContext) {
        if !self.above_bubble_column {
            self.set_bubble_time(0);
        }
        if self.bubble_time == 0 {
            return;
        }
        self.set_bubble_time(self.bubble_time - 1);
        if self.bubble_time == 0 {
            let event = if self.bubble_column_down {
                self.eject_riders(cx);
                play::entity_event::BOAT_BUBBLE_DOWN
            } else {
                play::entity_event::BOAT_BUBBLE_UP
            };
            cx.effects.push(Effect::EntityPackets(packets([framed(
                &play::EntityEvent {
                    entity_id: self.base.id,
                    status: event,
                },
            )])));
            if !self.driven() {
                let v = &mut self.base.velocity;
                if self.bubble_column_down {
                    v[1] -= 0.7;
                } else {
                    v[1] = 0.6;
                }
            }
        }
        self.above_bubble_column = false;
    }

    fn eject_riders(&mut self, cx: &mut TickContext) {
        for rider in std::mem::take(&mut self.base.riders) {
            let yaw = cx
                .players
                .iter()
                .find(|p| p.id == rider.id)
                .map_or(self.base.yaw, |p| p.yaw);
            let position = self.dismount_location(0.6, yaw, cx.block);
            cx.effects.push(Effect::Dismount { rider, position });
        }
    }

    /// Seats the living things it bumps while a seat is free and no
    /// player steers it, and pushes everything else pushable it touches.
    fn board_or_push(&mut self, cx: &mut TickContext) {
        let width = f64::from(self.base.kind.size().width);
        let bounds = self.base.bounds();
        let area = physics::inflate(bounds, [0.2, -0.01, 0.2]);
        let taken = self.base.riders.len() + self.base.entity_riders.len();
        let mut free = if !self.driven() && self.accepts_rider() {
            self.seats().saturating_sub(taken)
        } else {
            0
        };
        let carries = taken > 0;
        let own_vehicle = self.base.vehicle.as_ref().map(|mount| mount.id);
        for entity in cx.entities {
            let living = entity.living.is_some_and(|l| l.alive);
            if entity.id == self.base.id
                || Some(entity.id) == own_vehicle
                || entity.vehicle == Some(self.base.id)
                || !(living || is_boat(entity.kind) || super::minecart::is_minecart(entity.kind))
                || !physics::intersects(area, entity.bounds)
            {
                continue;
            }
            if free > 0
                && living
                && entity.vehicle.is_none()
                && !super::swims(entity.kind)
                && entity.kind != EntityType::Creaking
                && entity.bounds[3] - entity.bounds[0] < width
            {
                cx.effects.push(Effect::Mount {
                    passenger: entity.id,
                    vehicle: self.base.id,
                });
                free -= 1;
                continue;
            }
            let reaches = if is_boat(entity.kind) {
                entity.bounds[1] < bounds[4]
            } else {
                entity.bounds[1] <= bounds[1]
            };
            if !reaches {
                continue;
            }
            let xa = entity.position[0] - self.base.position[0];
            let za = entity.position[2] - self.base.position[2];
            let dd = xa.abs().max(za.abs());
            if dd < 0.009_999_999_776_482_582 {
                continue;
            }
            let dd = dd.sqrt();
            let pow = (1.0 / dd).min(1.0) * 0.050_000_000_745_058_06;
            let (xp, zp) = (xa / dd * pow, za / dd * pow);
            if !carries {
                self.base.velocity[0] -= xp;
                self.base.velocity[2] -= zp;
            }
            if !entity.carries {
                cx.effects.push(Effect::Nudge {
                    target: entity.id,
                    push: [xp, 0.0, zp],
                });
            }
        }
        for player in cx.players {
            let pb = player.bounds();
            if self.base.riders.iter().any(|rider| rider.id == player.id)
                || player.vehicle.is_some()
                || !physics::intersects(area, pb)
                || pb[1] > bounds[1]
            {
                continue;
            }
            let xa = player.position[0] - self.base.position[0];
            let za = player.position[2] - self.base.position[2];
            let dd = xa.abs().max(za.abs());
            if dd < 0.009_999_999_776_482_582 {
                continue;
            }
            let dd = dd.sqrt();
            let pow = (1.0 / dd).min(1.0) * 0.050_000_000_745_058_06;
            let (xp, zp) = (xa / dd * pow, za / dd * pow);
            if !carries {
                self.base.velocity[0] -= xp;
                self.base.velocity[2] -= zp;
            }
            cx.effects.push(Effect::Nudge {
                target: player.id,
                push: [xp, 0.0, zp],
            });
        }
    }

    fn driven(&self) -> bool {
        !self.base.riders.is_empty()
    }

    fn height(&self) -> f64 {
        f64::from(self.base.kind.size().height)
    }

    fn fluid_at(pos: BlockPos, block: BlockLookup) -> Option<(f64, bool)> {
        let state = block(pos)?.fluid_state();
        let surface = fluid::surface(pos, WATER, block)?;
        Some((surface, state.is_source()))
    }

    fn under_water(&mut self, bounds: Aabb, block: BlockLookup) -> Option<Status> {
        let top = bounds[4] + 0.001;
        let mut under = false;
        for x in bounds[0].floor() as i32..bounds[3].ceil() as i32 {
            for y in bounds[4].floor() as i32..top.ceil() as i32 {
                for z in bounds[2].floor() as i32..bounds[5].ceil() as i32 {
                    let pos = BlockPos::new(x, y, z);
                    let Some((surface, source)) = Self::fluid_at(pos, block) else {
                        continue;
                    };
                    if top < y as f64 + surface {
                        if !source {
                            return Some(Status::UnderFlowingWater);
                        }
                        under = true;
                    }
                }
            }
        }
        under.then_some(Status::UnderWater)
    }

    fn in_water(&mut self, bounds: Aabb, block: BlockLookup) -> bool {
        let mut inside = false;
        self.water_level = f64::MIN;
        for x in bounds[0].floor() as i32..bounds[3].ceil() as i32 {
            for y in bounds[1].floor() as i32..(bounds[1] + 0.001).ceil() as i32 {
                for z in bounds[2].floor() as i32..bounds[5].ceil() as i32 {
                    let pos = BlockPos::new(x, y, z);
                    if let Some((surface, _)) = Self::fluid_at(pos, block) {
                        let height = y as f64 + surface;
                        self.water_level = self.water_level.max(height);
                        inside |= bounds[1] < height;
                    }
                }
            }
        }
        inside
    }

    fn ground_friction(bounds: Aabb, block: BlockLookup) -> f32 {
        let probe = [
            bounds[0],
            bounds[1] - 0.001,
            bounds[2],
            bounds[3],
            bounds[1],
            bounds[5],
        ];
        let x0 = probe[0].floor() as i32 - 1;
        let x1 = probe[3].ceil() as i32 + 1;
        let y0 = probe[1].floor() as i32 - 1;
        let y1 = probe[4].ceil() as i32 + 1;
        let z0 = probe[2].floor() as i32 - 1;
        let z1 = probe[5].ceil() as i32 + 1;
        let (mut friction, mut count) = (0.0f32, 0);
        for x in x0..x1 {
            for z in z0..z1 {
                let edges = i32::from(x == x0 || x == x1 - 1) + i32::from(z == z0 || z == z1 - 1);
                if edges == 2 {
                    continue;
                }
                for y in y0..y1 {
                    if edges > 0 && (y == y0 || y == y1 - 1) {
                        continue;
                    }
                    let pos = BlockPos::new(x, y, z);
                    let state = block(pos);
                    if state.is_some_and(|s| s.block() == Block::LilyPad) {
                        continue;
                    }
                    if physics::block_boxes(state, pos).any(|b| physics::intersects(b, probe)) {
                        friction += state.map_or(0.6, |s| s.block().movement().friction);
                        count += 1;
                    }
                }
            }
        }
        friction / count as f32
    }

    fn crush_lily_pads(&self, cx: &mut TickContext) {
        for pos in lily_pads_within(self.base.bounds(), cx.block) {
            cx.effects.push(Effect::DestroyBlock { pos, drop: true });
        }
    }

    fn water_level_above(&self, bounds: Aabb, block: BlockLookup) -> f64 {
        let max_y = (bounds[4] - 0.0).ceil() as i32;
        for y in bounds[4].floor() as i32..max_y {
            let mut height = 0.0f64;
            for x in bounds[0].floor() as i32..bounds[3].ceil() as i32 {
                for z in bounds[2].floor() as i32..bounds[5].ceil() as i32 {
                    if let Some((surface, _)) = Self::fluid_at(BlockPos::new(x, y, z), block) {
                        height = height.max(surface);
                    }
                }
            }
            if height < 1.0 {
                return y as f64 + height;
            }
        }
        f64::from(max_y + 1)
    }

    fn status(&mut self, block: BlockLookup) -> Status {
        let bounds = self.base.bounds();
        if let Some(status) = self.under_water(bounds, block) {
            self.water_level = bounds[4];
            return status;
        }
        if self.in_water(bounds, block) {
            return Status::InWater;
        }
        let friction = Self::ground_friction(bounds, block);
        if friction > 0.0 {
            self.land_friction = friction;
            return Status::OnLand;
        }
        Status::InAir
    }

    /// Buoyancy, drag and gravity for a boat nobody drives.
    fn float(&mut self, block: BlockLookup) {
        let mut vertical = if self.base.no_gravity { 0.0 } else { -GRAVITY };
        let mut buoyancy = 0.0;
        if self.old_status == Status::InAir
            && self.status != Status::InAir
            && self.status != Status::OnLand
        {
            self.water_level = self.base.position[1] + self.height();
            let bounds = self.base.bounds();
            let target = self.water_level_above(bounds, block) - self.height() + 0.101;
            let moved = physics::offset(bounds, [0.0, target - self.base.position[1], 0.0]);
            if physics::no_collision(moved, block) {
                self.base.position[1] = target;
                self.base.velocity[1] = 0.0;
            }
            self.status = Status::InWater;
            return;
        }
        let drag = match self.status {
            Status::InWater => {
                buoyancy = (self.water_level - self.base.position[1]) / self.height();
                0.9
            }
            Status::UnderFlowingWater => {
                vertical = -7.0e-4;
                0.9
            }
            Status::UnderWater => {
                buoyancy = 0.009_999_999_776_482_582;
                0.45
            }
            Status::InAir => 0.9,
            Status::OnLand => self.land_friction,
        };
        let v = self.base.velocity;
        self.base.velocity = [
            v[0] * f64::from(drag),
            v[1] + vertical,
            v[2] * f64::from(drag),
        ];
        if buoyancy > 0.0 {
            self.base.velocity[1] = (self.base.velocity[1] + buoyancy * (GRAVITY / 0.65)) * 0.75;
        }
    }

    fn new_shadow(&self) -> Shadow {
        Shadow {
            position: self.base.position,
            velocity: [0.0; 3],
            on_ground: self.base.on_ground,
            status: self.status,
            old_status: self.old_status,
            water_level: self.water_level,
            held: self.base.position,
        }
    }

    /// Runs one tick of the boat the way its driver's client does from the
    /// paddles it reports, with every side effect discarded.
    fn step_shadow(&mut self, cx: &mut TickContext) {
        let block = cx.block;
        let mut shadow = self.shadow.take().unwrap_or_else(|| self.new_shadow());
        let shoved = [0, 1, 2].map(|i| self.base.position[i] - shadow.held[i]);
        for i in 0..3 {
            shadow.position[i] += shoved[i];
            shadow.velocity[i] += self.base.velocity[i];
        }
        if distance(shadow.position, self.base.position) > SHADOW_RANGE + 0.5 {
            shadow = self.new_shadow();
        }
        let live = (self.status, self.old_status, self.water_level);
        std::mem::swap(&mut self.base.position, &mut shadow.position);
        std::mem::swap(&mut self.base.velocity, &mut shadow.velocity);
        std::mem::swap(&mut self.base.on_ground, &mut shadow.on_ground);
        (self.status, self.old_status, self.water_level) =
            (shadow.status, shadow.old_status, shadow.water_level);
        self.old_status = self.status;
        self.status = self.status(block);
        self.float(block);
        let [left, right] = self.paddles;
        let acceleration = match (left, right) {
            (true, true) => 0.04,
            (false, false) => 0.0,
            _ => 0.005,
        };
        let (sin, cos) = (-self.base.yaw.to_radians()).sin_cos();
        self.base.velocity[0] += f64::from(sin) * acceleration;
        self.base.velocity[2] += f64::from(cos) * acceleration;
        let movement = self.base.velocity;
        let moved = physics::collide_with(
            self.base.bounds(),
            movement,
            0.0,
            self.base.on_ground,
            &|pos| physics::Shape::collision(block(pos), pos),
            cx.pistons,
        );
        for axis in 0..3 {
            self.base.position[axis] += moved[axis];
            if moved[axis] != movement[axis] {
                self.base.velocity[axis] = 0.0;
            }
        }
        self.base.on_ground = moved[1] != movement[1] && movement[1] < 0.0;
        std::mem::swap(&mut self.base.position, &mut shadow.position);
        std::mem::swap(&mut self.base.velocity, &mut shadow.velocity);
        std::mem::swap(&mut self.base.on_ground, &mut shadow.on_ground);
        (shadow.status, shadow.old_status, shadow.water_level) =
            (self.status, self.old_status, self.water_level);
        (self.status, self.old_status, self.water_level) = live;
        shadow.held = self.base.position;
        self.shadow = Some(shadow);
    }

    /// Fall distance as the game keeps it for a boat: reset on the ground,
    /// grown while falling anywhere but over water. Nothing consumes it
    /// (a boat never breaks from a fall).
    fn track_fall(&mut self, dy: f64, block: BlockLookup) {
        if self.base.on_ground {
            self.base.fall_distance = 0.0;
            return;
        }
        let [x, y, z] = self.base.position;
        let below = BlockPos::new(x.floor() as i32, y.floor() as i32 - 1, z.floor() as i32);
        let water =
            block(below).is_some_and(|s| tuff_datapack::tags::is(WATER, s.fluid_state().fluid()));
        if !water && dy < 0.0 {
            self.base.fall_distance -= dy;
        }
    }

    fn drop_item(&self) -> Option<ItemStack> {
        let item = Item::of_boat(self.base.kind)?;
        let mut stack = ItemStack::mint(item, 1, MintReason::EntityDrop);
        if let Some(name) = self
            .base
            .custom_name
            .as_ref()
            .and_then(tuff_item::text::TextComponent::from_nbt)
        {
            stack.components.custom_name = Some(Box::new(name));
        }
        Some(stack)
    }

    /// Its contents scatter in pieces of 10 to 30; itself drops as its
    /// item when broken (not by a creative player) and the game rule
    /// allows.
    fn spill(&mut self, cx: &mut TickContext) {
        if !cx.shared.game_rules.borrow().bool("entity_drops") {
            return;
        }
        let position = self.base.position;
        let pos = BlockPos::new(
            position[0].floor() as i32,
            position[1].floor() as i32,
            position[2].floor() as i32,
        );
        let items: Vec<ItemStack> = self.items.iter_mut().filter_map(Option::take).collect();
        for (stack, position, velocity) in block_entity::scatter(pos, items, &mut *cx.rng) {
            cx.effects.push(Effect::DropItem {
                stack,
                position,
                velocity,
                pickup_delay: 10,
            });
        }
    }

    fn destroy(&mut self, drop_self: bool, by_player: Option<i32>, cx: &mut TickContext) {
        self.spill(cx);
        tuff_core::world::leash::untie(&mut self.leash, &self.base, true, cx);
        if drop_self
            && cx.shared.game_rules.borrow().bool("entity_drops")
            && let Some(stack) = self.drop_item()
        {
            cx.effects.push(Effect::DropItem {
                stack,
                position: self.base.position,
                velocity: [
                    cx.rng.r#gen::<f64>() * 0.2 - 0.1,
                    0.2,
                    cx.rng.r#gen::<f64>() * 0.2 - 0.1,
                ],
                pickup_delay: 10,
            });
        }
        if let Some(player) = by_player.filter(|_| has_chest(self.base.kind)) {
            cx.effects.push(Effect::DisturbGuarded { player });
        }
    }

    /// A hit: shakes it and adds ten times the damage; past 40 it breaks,
    /// and a creative player's hit removes it at once.
    fn damage(
        &mut self,
        amount: f32,
        creative: bool,
        by_player: Option<i32>,
        cx: &mut TickContext,
    ) -> bool {
        self.hurt_dir = -self.hurt_dir;
        self.hurt_time = 10;
        self.damage += amount * 10.0;
        self.changed.extend([DATA_HURT_DIR, DATA_HURT, DATA_DAMAGE]);
        if creative || self.damage > BREAK_DAMAGE {
            self.destroy(!creative, by_player, cx);
            return true;
        }
        false
    }

    /// Its lead pulls it before it moves, as an entity's base tick does.
    fn tick_leash(&mut self, cx: &mut TickContext) {
        if self.leash.is_none() {
            return;
        }
        let in_liquid = matches!(
            self.status,
            Status::InWater | Status::UnderWater | Status::UnderFlowingWater
        );
        let friction = if self.base.on_ground {
            self.land_friction
        } else {
            0.6
        };
        let body = tuff_core::world::leash::Body {
            bounds: self.base.bounds(),
            profile: LEASH,
            alive: !self.broken,
            angular_friction: tuff_core::world::leash::angular_friction(
                self.base.on_ground,
                in_liquid,
                friction,
            ),
            motion: self.base.velocity,
            base: &mut self.base,
        };
        tuff_core::world::leash::tick(&mut self.leash, body, cx);
    }

    fn paddle_sounds(&mut self, cx: &mut TickContext) {
        for side in 0..2 {
            if !self.paddles[side] {
                self.paddle_positions[side] = 0.0;
                continue;
            }
            let at = self.paddle_positions[side] % std::f32::consts::TAU;
            let next = (self.paddle_positions[side] + PADDLE_SPEED) % std::f32::consts::TAU;
            let sound = match self.status {
                Status::InWater | Status::UnderWater | Status::UnderFlowingWater => {
                    Some(SoundEvent::EntityBoatPaddleWater)
                }
                Status::OnLand => Some(SoundEvent::EntityBoatPaddleLand),
                Status::InAir => None,
            };
            if !self.base.silent
                && f64::from(at) <= PADDLE_SOUND_TIME
                && f64::from(next) >= PADDLE_SOUND_TIME
                && let Some(sound) = sound
            {
                let (sin, cos) = (-self.base.yaw.to_radians()).sin_cos();
                let pitch = self.base.pitch.to_radians().cos();
                let view = [f64::from(sin * pitch), f64::from(cos * pitch)];
                let (dx, dz) = if side == 1 {
                    (-view[1], view[0])
                } else {
                    (view[1], -view[0])
                };
                let [x, y, z] = self.base.position;
                cx.effects
                    .push(Effect::ChunkPackets(packets([framed(&play::Sound {
                        sound_event: sound.id(),
                        category: play::sound_category::NEUTRAL,
                        position: [x + dx, y, z + dz],
                        volume: 1.0,
                        pitch: 0.8 + 0.4 * cx.rng.r#gen::<f32>(),
                        seed: cx.rng.r#gen(),
                    })])));
            }
            self.paddle_positions[side] += PADDLE_SPEED;
        }
    }
}

impl RegionEntity for Boat {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        if let Some(leash) = &self.leash {
            leash.save(out);
        }
        if !has_chest(self.base.kind) {
            return;
        }
        if let Some((table, seed)) = &self.loot_table {
            out.insert(LOOT_TABLE, table.clone());
            if *seed != 0 {
                out.insert(LOOT_TABLE_SEED, *seed);
            }
        } else {
            out.insert(
                ITEMS,
                Tag::List(
                    encode_items(&self.items)
                        .into_iter()
                        .map(Tag::Compound)
                        .collect(),
                ),
            );
        }
    }

    fn container(&self) -> Option<tuff_core::world::entity_slots::ContainerSlots<'_>> {
        has_chest(self.base.kind).then(|| self.items.as_slice().into())
    }

    fn container_mut(&mut self) -> Option<tuff_core::world::entity_slots::ContainerSlotsMut<'_>> {
        has_chest(self.base.kind).then(|| self.items.as_mut_slice().into())
    }

    fn take_loot_table(&mut self) -> Option<(String, i64)> {
        self.loot_table.take()
    }

    fn loot_pending(&self) -> bool {
        self.loot_table.is_some()
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        [
            DATA_HURT,
            DATA_HURT_DIR,
            DATA_DAMAGE,
            DATA_PADDLE_LEFT,
            DATA_PADDLE_RIGHT,
            DATA_BUBBLE_TIME,
        ]
        .into_iter()
        .filter_map(|index| Some((index, self.value(index)?)))
        .collect()
    }

    fn take_changed_data(&mut self) -> Vec<(u8, MetadataValue)> {
        let mut changed = std::mem::take(&mut self.changed);
        changed.sort_unstable();
        changed.dedup();
        changed
            .into_iter()
            .filter_map(|i| Some((i, self.value(i)?)))
            .collect()
    }

    fn resend_data(&mut self, index: u8) {
        self.changed.push(index);
    }

    fn rotates(&self) -> bool {
        true
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        if self.broken {
            return Tick::Discard;
        }
        self.tick_leash(cx);
        let block = cx.block;
        self.old_status = self.status;
        self.status = self.status(block);
        if matches!(self.status, Status::UnderWater | Status::UnderFlowingWater) {
            self.out_of_control += 1.0;
        } else {
            self.out_of_control = 0.0;
        }
        if self.out_of_control >= TIME_TO_EJECT && self.driven() {
            self.eject_riders(cx);
        }
        if self.hurt_time > 0 {
            self.hurt_time -= 1;
            self.changed.push(DATA_HURT);
        }
        if self.damage > 0.0 {
            self.damage -= 1.0;
            self.changed.push(DATA_DAMAGE);
        }
        if self.driven() {
            self.step_shadow(cx);
            self.base.velocity = [0.0; 3];
        } else {
            self.shadow = None;
            self.set_paddles([false, false]);
            self.float(block);
            let bounds = self.base.bounds();
            let movement = self.base.velocity;
            let moved = physics::collide_with(
                bounds,
                movement,
                0.0,
                self.base.on_ground,
                &|pos| physics::Shape::collision(block(pos), pos),
                cx.pistons,
            );
            for axis in 0..3 {
                self.base.position[axis] += moved[axis];
                if moved[axis] != movement[axis] {
                    self.base.velocity[axis] = 0.0;
                }
            }
            self.base.on_ground = moved[1] != movement[1] && movement[1] < 0.0;
            self.track_fall(moved[1], block);
        }
        block_contact::pricked_by_cactus(self, block);
        self.crush_lily_pads(cx);
        if self.damage > BREAK_DAMAGE {
            self.destroy(true, None, cx);
            self.broken = true;
        }
        self.touch_bubble_columns(block);
        self.tick_bubble_column(cx);
        self.paddle_sounds(cx);
        self.board_or_push(cx);
        Tick::Alive
    }

    fn hurt(&mut self, amount: f32, damage_type: &'static DamageType) -> bool {
        let _ = damage_type;
        self.hurt_dir = -self.hurt_dir;
        self.hurt_time = 10;
        self.damage += amount * 10.0;
        self.changed.extend([DATA_HURT_DIR, DATA_HURT, DATA_DAMAGE]);
        false
    }

    fn attacked(&mut self, attack: &EntityAttack, cx: &mut TickContext) -> Option<AttackOutcome> {
        let player = cx.players.iter().find(|p| p.id == attack.attacker);
        let creative = player.is_some_and(|p| p.creative);
        let by_player = player.map(|p| p.id);
        self.broken |= self.damage(attack.damage, creative, by_player, cx);
        Some(AttackOutcome {
            hurt: true,
            health_lost: 0.0,
        })
    }

    fn hit(&mut self, hit: &EntityHit, cx: &mut TickContext) -> Option<AttackOutcome> {
        self.broken |= self.damage(hit.amount, false, None, cx);
        Some(AttackOutcome {
            hurt: true,
            health_lost: 0.0,
        })
    }

    fn pickable(&self) -> bool {
        true
    }

    fn kill(&mut self, cx: &mut TickContext) -> bool {
        self.spill(cx);
        tuff_core::world::leash::untie(&mut self.leash, &self.base, true, cx);
        true
    }

    fn leash(&self) -> Option<&tuff_core::world::leash::Leash> {
        self.leash.as_ref()
    }

    fn leash_mut(&mut self) -> Option<&mut Option<tuff_core::world::leash::Leash>> {
        Some(&mut self.leash)
    }

    fn leash_profile(&self) -> Option<tuff_core::world::leash::LeashProfile> {
        Some(LEASH)
    }

    fn seats(&self) -> usize {
        if has_chest(self.base.kind) { 1 } else { 2 }
    }

    fn accepts_rider(&self) -> bool {
        self.out_of_control < TIME_TO_EJECT
    }

    fn driven_by_rider(&self) -> bool {
        true
    }

    fn seat(&self, index: usize, riders: usize) -> [f64; 3] {
        let mut offset = if has_chest(self.base.kind) { 0.15 } else { 0.0 };
        if riders > 1 {
            offset = if index == 0 { 0.2 } else { -0.6 };
        }
        let height = self.base.kind.size().height;
        let ride = f64::from(if self.base.kind.name().ends_with("_raft") {
            height * 0.888_888_9
        } else {
            height / 3.0
        });
        let (sin, cos) = (-self.base.yaw.to_radians()).sin_cos();
        [offset * f64::from(sin), ride, offset * f64::from(cos)]
    }

    fn dismount_location(
        &self,
        width: f64,
        yaw: f32,
        block: &dyn Fn(BlockPos) -> Option<BlockState>,
    ) -> [f64; 3] {
        let boat_width = f64::from(self.base.kind.size().width) * std::f64::consts::SQRT_2;
        let top = self.bounds()[4];
        dismount::side_dismount(self.base.position, top, boat_width, width, yaw, block).unwrap_or({
            let [x, _, z] = self.base.position;
            [x, top, z]
        })
    }

    fn clamp_driven_move(&mut self, reported: [f64; 3]) -> [f64; 3] {
        let mut shadow = self.shadow.take().unwrap_or_else(|| self.new_shadow());
        let held = shadow::hold(&mut shadow.position, reported);
        shadow.held = held;
        self.shadow = Some(shadow);
        held
    }

    fn on_mounted(&mut self) {
        self.shadow = None;
    }

    fn steer(&mut self, left: bool, right: bool) {
        self.set_paddles([left, right]);
    }
}

/// Loads a saved boat, chest boat or raft.
pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut boat = Boat::with_base(base);
    boat.leash = tuff_core::world::leash::Leash::load(nbt);
    if has_chest(boat.base.kind) {
        if let Some(Tag::List(entries)) = nbt.remove(ITEMS) {
            boat.items = decode_items(&entries, CHEST_SLOTS);
        }
        if let Some(Tag::String(table)) = nbt.remove(LOOT_TABLE) {
            let seed = match nbt.remove(LOOT_TABLE_SEED) {
                Some(Tag::Long(seed)) => seed,
                _ => 0,
            };
            boat.loot_table = Some((table, seed));
        }
    }
    Some(Box::new(boat))
}

/// Registers every boat type's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    for kind in EntityType::ALL {
        if is_boat(kind) {
            registry.register(kind, load);
        }
    }
}

/// The saved custom name a boat takes from the item placing it.
pub fn saved_name(stack: &ItemStack) -> Option<Tag> {
    stack
        .components
        .custom_name
        .as_ref()
        .map(|name| name.to_nbt())
}

fn lily_pads_within(b: Aabb, block: BlockLookup) -> Vec<BlockPos> {
    const EPS: f64 = 1.0e-5;
    let mut pads = Vec::new();
    for x in (b[0] + EPS).floor() as i32..=(b[3] - EPS).floor() as i32 {
        for y in (b[1] + EPS).floor() as i32..=(b[4] - EPS).floor() as i32 {
            for z in (b[2] + EPS).floor() as i32..=(b[5] - EPS).floor() as i32 {
                let pos = BlockPos::new(x, y, z);
                if block(pos).is_some_and(|s| s.block() == Block::LilyPad) {
                    pads.push(pos);
                }
            }
        }
    }
    pads
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_boat_finds_the_lily_pads_in_its_box() {
        let pad = BlockPos::new(0, 64, 0);
        let block = |pos: BlockPos| {
            Some(if pos == pad {
                Block::LilyPad.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let inside = [-0.5, 64.0, -0.5, 0.7, 64.5, 0.7];
        assert_eq!(lily_pads_within(inside, &block), vec![pad]);
        let beside = [1.0, 64.0, 0.0, 2.0, 64.5, 1.0];
        assert!(lily_pads_within(beside, &block).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn boat_entity_data_matches_the_dumped_layout() {
        let boat = Boat::with_base(EntityBase::new(
            EntityType::OakChestBoat,
            1,
            1,
            [0.0; 3],
            [0.0; 3],
        ));
        for (index, value) in boat.metadata() {
            assert_eq!(
                EntityType::OakChestBoat.data_serializer(index),
                Some(value.type_id() as u8),
                "index {index}"
            );
        }
        assert_eq!(boat.seats(), 1);
        assert!((boat.seat(0, 1)[2] - 0.15).abs() < 1.0e-6);
    }

    #[tuff_core::watchdog]
    #[test]
    fn chest_boats_load_and_save_their_items_and_pending_loot() {
        let base = || EntityBase::new(EntityType::OakChestBoat, 1, 1, [0.0; 3], [0.0; 3]);
        let stack = ItemStack::mint(Item::Diamond, 2, MintReason::Test);
        let mut nbt = Compound::new().with(
            ITEMS,
            Tag::List(vec![
                Tag::Compound(stack.to_nbt().with("Slot", 26i8)),
                Tag::Compound(stack.to_nbt().with("Slot", 27i8)),
            ]),
        );
        let boat = load(base(), &mut nbt).unwrap();
        assert!(nbt.is_empty(), "the loader takes its keys");
        let items = boat.container().unwrap();
        assert_eq!(items.len(), CHEST_SLOTS);
        assert_eq!(items[26], Some(stack.dup_unchecked()));
        assert_eq!(items.iter().flatten().count(), 1, "slot 27 is out of range");
        let mut out = Compound::new();
        boat.save(&mut out);
        let saved = out.get_list(ITEMS).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].as_compound().unwrap().get_byte("Slot"), Some(26));

        let mut nbt = Compound::new()
            .with(LOOT_TABLE, "minecraft:chests/simple_dungeon")
            .with(LOOT_TABLE_SEED, 7i64);
        let mut boat = load(base(), &mut nbt).unwrap();
        assert!(boat.loot_pending());
        let mut out = Compound::new();
        boat.save(&mut out);
        assert_eq!(out.get_long(LOOT_TABLE_SEED), Some(7));
        assert!(out.get(ITEMS).is_none());
        assert_eq!(
            boat.take_loot_table(),
            Some(("minecraft:chests/simple_dungeon".to_owned(), 7))
        );
        assert!(!boat.loot_pending());

        let plain = load(
            EntityBase::new(EntityType::OakBoat, 1, 1, [0.0; 3], [0.0; 3]),
            &mut Compound::new(),
        )
        .unwrap();
        assert!(plain.container().is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn boats_slide_on_ice_but_stop_on_stone() {
        assert!(Block::Ice.movement().friction > 0.9);
        assert!(Block::BlueIce.movement().friction > Block::Ice.movement().friction);
        for floor in [Block::Ice, Block::BlueIce, Block::Stone] {
            let lookup = move |pos: BlockPos| (pos.y == 0).then(|| floor.default_state());
            let mut boat = Boat::with_base(EntityBase::new(
                EntityType::OakBoat,
                1,
                1,
                [0.5, 1.0, 0.5],
                [0.0; 3],
            ));
            boat.base.velocity = [0.4, 0.0, 0.0];
            boat.old_status = boat.status;
            boat.status = boat.status(&lookup);
            assert_eq!(boat.status, Status::OnLand, "{}", floor.name());
            boat.float(&lookup);
            let kept = 0.4 * f64::from(floor.movement().friction);
            assert!(
                (boat.base.velocity[0] - kept).abs() < 1.0e-6,
                "{}: kept {} of its speed, wanted {kept}",
                floor.name(),
                boat.base.velocity[0]
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn boat_items_place_their_boats_and_players_sit_on_them() {
        assert_eq!(Item::OakBoat.boat(), Some(EntityType::OakBoat));
        assert_eq!(
            Item::of_boat(EntityType::BambooChestRaft),
            Some(Item::BambooChestRaft)
        );
        assert!(is_boat(EntityType::CherryChestBoat));
        assert!(!is_boat(EntityType::Minecart));
        assert!((EntityType::Player.vehicle_attachment()[1] - 0.6).abs() < 1.0e-6);
    }
}
