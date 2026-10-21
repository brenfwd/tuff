//! Minecarts: an entity and the item that places it. The game simulates a
//! minecart server-side even while it is ridden (unlike a boat, whose
//! rider's client drives it): it follows the rail it is on (straight,
//! sloped or curved), powered rails boost or brake it, an activator rail
//! acts on it, and a rider's movement keys nudge a stopped one. A world
//! with the minecart improvements feature flag moves carts the newer way
//! instead (see [`track`]).

pub mod track;

use rand::Rng;
use tuff_vanilla_data::block::Shape;
use tuff_vanilla_data::damage_type::{self, DamageType};
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockKind, BlockState, Item};

use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::block_entity::{self, decode_items, encode_items};
use tuff_core::world::contact;
use tuff_core::world::entity::{
    AttackOutcome, Effect, EntityAttack, EntityBase, EntityHit, EntityInteraction, InteractOutcome,
    RegionEntity, RiderInput, Tick, TickContext, take_number,
};
use tuff_core::world::explosion::{BlockInteraction, Explosion};
use tuff_core::world::menu::MenuKind;
use tuff_core::world::messages::{HandEffect, Igniter};
use tuff_core::world::physics::{self, BlockLookup};
use tuff_core::world::{framed, packets};
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const DATA_HURT: u8 = 8;
const DATA_HURT_DIR: u8 = 9;
const DATA_DAMAGE: u8 = 10;
const DATA_FUEL: u8 = 13;
const DATA_COMMAND: u8 = 13;
const DATA_LAST_OUTPUT: u8 = 14;
const FUEL_TICKS_PER_ITEM: i32 = 3600;
const MAX_FUEL_TICKS: i32 = 32000;
const GRAVITY: f64 = 0.04;
/// How far around itself a minecart looks for moving blocks: more than it
/// moves in a tick.
const MOVING_BLOCK_REACH: f64 = 2.0;
const WATER_GRAVITY: f64 = 0.005;
/// Damage past which a hit breaks it.
const BREAK_DAMAGE: f32 = 40.0;
const MAX_SPEED: f64 = 0.4;
const MAX_SPEED_IN_WATER: f64 = 0.2;
const SLOWDOWN_RIDDEN: f64 = 0.997;
const SLOWDOWN_EMPTY: f64 = 0.96;
const WATER_SLOWDOWN: f64 = 0.949999988079071;
const AIR_DRAG: f64 = 0.949999988079071;
/// How fast gravity pulls a cart down a slope.
const SLIDE: f64 = 0.0078125;
/// Below this squared speed a rider's keys can nudge the cart.
const RIDABLE_THRESHOLD: f64 = 0.01;
const RIDER_NUDGE: f64 = 0.001;
/// Below this speed an unpowered powered rail stops a cart outright.
const BRAKE_STOP_SPEED: f64 = 0.03;
const BOOST: f64 = 0.06;
/// The push a powered rail gives a stopped cart away from a solid block.
const KICK: f64 = 0.02;
/// How far a cart's bottom sits above the rail block below it.
pub(crate) const RAIL_RIDE_HEIGHT: f64 = 0.0625;
/// Ticks between two runs of a command block minecart's command.
const COMMAND_COOLDOWN: u64 = 4;
/// The damage an activator rail shows on an empty passenger cart.
const ACTIVATED_DAMAGE: f32 = 50.0;
/// The fuse a TNT minecart gets when lit.
const TNT_FUSE: i32 = 80;
const TNT_POWER: f32 = 4.0;
const TNT_SPEED_FACTOR: f32 = 1.0;
const MAX_TNT_POWER: f32 = 128.0;
/// The squared speed past which a TNT minecart explodes on hitting
/// something, or lights when broken.
const TNT_VOLATILE_SPEED_SQ: f64 = 0.009999999776482582;
const TNT_PRIMED_EVENT: i8 = 70;
const FUEL: &str = "Fuel";
const PUSH_X: &str = "PushX";
const PUSH_Z: &str = "PushZ";
const ITEMS: &str = "Items";
const HAS_TICKED: &str = "HasTicked";
const ENABLED: &str = "Enabled";
const LOOT_TABLE: &str = "LootTable";
const LOOT_TABLE_SEED: &str = "LootTableSeed";
const FLIPPED: &str = "FlippedRotation";
const FUSE: &str = "fuse";
const EXPLOSION_POWER: &str = "explosion_power";
const EXPLOSION_SPEED_FACTOR: &str = "explosion_speed_factor";

/// The slots of a container minecart of `kind`, 0 for the others.
pub fn container_slots(kind: EntityType) -> usize {
    match kind {
        EntityType::ChestMinecart => 27,
        EntityType::HopperMinecart => 5,
        _ => 0,
    }
}

/// Whether `kind` is any of the minecart entities.
pub fn is_minecart(kind: EntityType) -> bool {
    kind.name().contains("minecart")
}

/// The item that places a minecart of `kind`, if any (command block and
/// spawner minecarts have none).
pub fn minecart_item(kind: EntityType) -> Option<Item> {
    Item::ALL
        .iter()
        .copied()
        .find(|item| item.name() == kind.name())
}

/// The minecart entity `item` places.
pub fn of_minecart_item(item: Item) -> Option<EntityType> {
    EntityType::ALL
        .iter()
        .copied()
        .find(|kind| kind.name().ends_with("minecart") && kind.name() == item.name())
}

/// Whether `state` is one of the four rail blocks (plain, powered,
/// detector, activator).
pub fn is_rail_block(state: BlockState) -> bool {
    state.block().kind() == BlockKind::Rail
}

/// Whether a rail of `shape` slopes.
pub fn is_slope(shape: Shape) -> bool {
    matches!(
        shape,
        Shape::AscendingEast | Shape::AscendingWest | Shape::AscendingNorth | Shape::AscendingSouth
    )
}

/// The two cells, relative to a rail of `shape`, a cart on it leaves
/// toward.
fn exits(shape: Shape) -> Option<([i32; 3], [i32; 3])> {
    const W: [i32; 3] = [-1, 0, 0];
    const E: [i32; 3] = [1, 0, 0];
    const N: [i32; 3] = [0, 0, -1];
    const S: [i32; 3] = [0, 0, 1];
    Some(match shape {
        Shape::NorthSouth => (N, S),
        Shape::EastWest => (W, E),
        Shape::AscendingEast => ([-1, -1, 0], E),
        Shape::AscendingWest => (W, [1, -1, 0]),
        Shape::AscendingNorth => (N, [0, -1, 1]),
        Shape::AscendingSouth => ([0, -1, -1], S),
        Shape::SouthEast => (S, E),
        Shape::SouthWest => (S, W),
        Shape::NorthWest => (N, W),
        Shape::NorthEast => (N, E),
        _ => return None,
    })
}

fn block_of(x: f64, y: f64, z: f64) -> BlockPos {
    BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
}

fn rail_shape(state: Option<BlockState>) -> Option<Shape> {
    state.filter(|s| is_rail_block(*s))?.shape()
}

/// The rail cell a cart at `(x, y, z)` stands in: its own block, or the
/// one below when that is a rail.
fn rail_cell(x: f64, y: f64, z: f64, block: BlockLookup) -> BlockPos {
    let pos = block_of(x, y, z);
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    if block(below).is_some_and(is_rail_block) {
        below
    } else {
        pos
    }
}

/// Where a cart at `(x, y, z)` sits on the rail there, or `None` off rails.
fn rail_position(x: f64, y: f64, z: f64, block: BlockLookup) -> Option<[f64; 3]> {
    let cell = rail_cell(x, y, z, block);
    let (e0, e1) = exits(rail_shape(block(cell))?)?;
    let (xt, yt, zt) = (f64::from(cell.x), f64::from(cell.y), f64::from(cell.z));
    let x0 = xt + 0.5 + f64::from(e0[0]) * 0.5;
    let y0 = yt + RAIL_RIDE_HEIGHT + f64::from(e0[1]) * 0.5;
    let z0 = zt + 0.5 + f64::from(e0[2]) * 0.5;
    let x1 = xt + 0.5 + f64::from(e1[0]) * 0.5;
    let y1 = yt + RAIL_RIDE_HEIGHT + f64::from(e1[1]) * 0.5;
    let z1 = zt + 0.5 + f64::from(e1[2]) * 0.5;
    let (xd, yd, zd) = (x1 - x0, (y1 - y0) * 2.0, z1 - z0);
    let progress = if xd == 0.0 {
        z - zt
    } else if zd == 0.0 {
        x - xt
    } else {
        ((x - x0) * xd + (z - z0) * zd) * 2.0
    };
    let mut y = y0 + yd * progress;
    if yd < 0.0 {
        y += 1.0;
    } else if yd > 0.0 {
        y += 0.5;
    }
    Some([x0 + xd * progress, y, z0 + zd * progress])
}

fn horizontal_sq(v: [f64; 3]) -> f64 {
    v[0] * v[0] + v[2] * v[2]
}

fn wrap_degrees(value: f32) -> f32 {
    let mut wrapped = value % 360.0;
    if wrapped >= 180.0 {
        wrapped -= 360.0;
    }
    if wrapped < -180.0 {
        wrapped += 360.0;
    }
    wrapped
}

/// Where a rider's held keys point, flat on the ground.
fn move_intent(input: RiderInput) -> [f64; 3] {
    let left = f64::from(-input.strafe);
    let forward = f64::from(input.forward);
    let length = left * left + forward * forward;
    if length < 1.0e-7 {
        return [0.0; 3];
    }
    let (left, forward) = if length > 1.0 {
        let norm = length.sqrt();
        (left / norm, forward / norm)
    } else {
        (left, forward)
    };
    let radians = input.yaw * 0.017453292;
    let (sin, cos) = (f64::from(radians.sin()), f64::from(radians.cos()));
    [left * cos - forward * sin, 0.0, forward * cos + left * sin]
}

/// Moves `base` by `delta`, stopping at the blocks `shape` gives and at
/// the `extra` boxes, as a cart moves itself.
fn collide_move(
    base: &mut EntityBase,
    delta: [f64; 3],
    shape: physics::ShapeLookup,
    extra: &[physics::Aabb],
) -> Moved {
    let moved = physics::collide_with(base.bounds(), delta, 0.0, base.on_ground, shape, extra);
    for axis in 0..3 {
        base.position[axis] += moved[axis];
    }
    let differs = |a: f64, b: f64| (a - b).abs() >= 1.0e-5;
    let x_collision = differs(moved[0], delta[0]);
    let z_collision = differs(moved[2], delta[2]);
    let vertical = moved[1] != delta[1];
    base.on_ground = vertical && delta[1] < 0.0;
    let mut landed = None;
    if base.on_ground {
        landed = Some(base.fall_distance);
        base.fall_distance = 0.0;
    } else if moved[1] < 0.0 {
        base.fall_distance -= moved[1];
    }
    if x_collision {
        base.velocity[0] = 0.0;
    }
    if z_collision {
        base.velocity[2] = 0.0;
    }
    if vertical {
        base.velocity[1] = 0.0;
    }
    Moved {
        horizontal_collision: x_collision || z_collision,
        vertical_collision: vertical,
        landed,
    }
}

/// Something a hit asked of the cart, done at the start of its next tick.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Pending {
    /// Broken by world damage; `ignites` when fire or an explosion did it.
    Destroy { ignites: bool },
    /// A burning projectile flying this fast (squared) struck a TNT
    /// minecart.
    Explode { speed_sq: f64 },
}

/// What one move did.
#[derive(Debug, Default, Clone, Copy)]
struct Moved {
    horizontal_collision: bool,
    vertical_collision: bool,
    /// The fall it ended, if it landed.
    landed: Option<f64>,
}

#[derive(Debug)]
pub struct Minecart {
    pub base: EntityBase,
    hurt_time: i32,
    hurt_dir: i32,
    damage: f32,
    broken: bool,
    breaker: Option<i32>,
    last_activated: u64,
    changed: Vec<u8>,
    items: Vec<Option<ItemStack>>,
    loot_table: Option<(String, i64)>,
    /// A hopper minecart collects items only while enabled; a powered
    /// activator rail disables it.
    enabled: bool,
    /// Its yaw is turned half round from its heading.
    flipped: bool,
    in_water: bool,
    /// The rider's held keys, latched until the next report.
    input: RiderInput,
    /// A TNT minecart's ticks left to explode, -1 while unlit.
    fuse: i32,
    power: f32,
    speed_factor: f32,
    /// Who lit a TNT minecart, for blame.
    igniter: Option<Igniter>,
    pending: Option<Pending>,
    fuel: i32,
    push: [f64; 2],
    /// The boxes of blocks pistons are moving near it this tick.
    moving_blocks: Vec<physics::Aabb>,
    /// A command block minecart's command and last output as viewers
    /// were last sent them.
    shown: (String, Option<TextComponent>),
    /// It has not ticked since it was made (saved as `HasTicked`, which the
    /// game names the other way round).
    first_tick: bool,
    /// The world has the minecart improvements feature: it moves along
    /// tracks the newer way and reports its steps to viewers.
    improved: bool,
    /// It was on a rail at its last step (newer movement only).
    on_rails: bool,
    /// Where its current step started (newer movement only).
    step_start: [f64; 3],
    /// Steps not yet sent to viewers (newer movement only).
    steps: Vec<play::TrackStep>,
}

impl Minecart {
    pub fn new(
        kind: EntityType,
        id: i32,
        position: [f64; 3],
        yaw: f32,
        rng: &mut impl Rng,
    ) -> Self {
        let mut base = EntityBase::new(kind, id, rng.r#gen(), position, [0.0; 3]);
        base.yaw = yaw;
        let mut cart = Self::with_base(base);
        cart.first_tick = true;
        cart
    }

    fn with_base(base: EntityBase) -> Self {
        let slots = container_slots(base.kind);
        Self {
            base,
            hurt_time: 0,
            hurt_dir: 1,
            damage: 0.0,
            breaker: None,
            last_activated: 0,
            broken: false,
            changed: Vec::new(),
            items: vec![None; slots],
            loot_table: None,
            enabled: true,
            flipped: false,
            in_water: false,
            input: RiderInput::default(),
            fuse: -1,
            power: TNT_POWER,
            speed_factor: TNT_SPEED_FACTOR,
            igniter: None,
            pending: None,
            fuel: 0,
            push: [0.0; 2],
            moving_blocks: Vec::new(),
            shown: (String::new(), None),
            first_tick: false,
            improved: false,
            on_rails: false,
            step_start: [0.0; 3],
            steps: Vec::new(),
        }
    }

    fn is_command_block(&self) -> bool {
        self.base.kind == EntityType::CommandBlockMinecart
    }

    fn command(&self) -> String {
        self.base.extra.get_str("Command").unwrap_or("").to_owned()
    }

    fn last_output(&self) -> Option<TextComponent> {
        self.base
            .extra
            .get("LastOutput")
            .and_then(TextComponent::from_nbt)
    }

    /// Tells viewers a command or output that changed in its saved data
    /// (an edit, a finished run).
    fn show_command(&mut self) {
        let now = (self.command(), self.last_output());
        if now.0 != self.shown.0 {
            self.changed.push(DATA_COMMAND);
        }
        if now.1 != self.shown.1 {
            self.changed.push(DATA_LAST_OUTPUT);
        }
        self.shown = now;
    }

    fn is_tnt(&self) -> bool {
        self.base.kind == EntityType::TntMinecart
    }

    fn is_furnace(&self) -> bool {
        self.base.kind == EntityType::FurnaceMinecart
    }

    fn rideable(&self) -> bool {
        self.base.kind == EntityType::Minecart
    }

    fn ridden(&self) -> bool {
        !self.base.riders.is_empty()
    }

    fn value(&self, index: u8) -> Option<MetadataValue> {
        Some(match index {
            DATA_HURT => MetadataValue::VarInt(self.hurt_time),
            DATA_HURT_DIR => MetadataValue::VarInt(self.hurt_dir),
            DATA_DAMAGE => MetadataValue::Float(self.damage),
            DATA_FUEL if self.is_furnace() => MetadataValue::Boolean(self.fuel > 0),
            DATA_COMMAND if self.is_command_block() => MetadataValue::String(self.command()),
            DATA_LAST_OUTPUT if self.is_command_block() => {
                MetadataValue::Text(self.last_output().unwrap_or_else(|| TextComponent::new("")))
            }
            _ => return None,
        })
    }

    fn drop_item(&self) -> Option<ItemStack> {
        Some(ItemStack::new(minecart_item(self.base.kind)?, 1))
    }

    /// Its contents scatter in pieces of 10 to 30.
    fn spill(&mut self, cx: &mut TickContext) {
        if !cx.shared.game_rules.borrow().bool("entity_drops") {
            return;
        }
        let [x, y, z] = self.base.position;
        let pos = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
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

    /// Its contents spill (unless `entity_drops` is off); itself drops as its item when broken
    /// (not by a creative player) and the game rule allows.
    fn destroy(&mut self, drop_self: bool, cx: &mut TickContext) {
        self.spill(cx);
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
        let guarded = matches!(
            self.base.kind,
            EntityType::ChestMinecart | EntityType::HopperMinecart
        );
        if let Some(player) = self.breaker.filter(|_| guarded) {
            cx.effects.push(Effect::DisturbGuarded { player });
        }
    }

    fn mark_hurt(&mut self, amount: f32) {
        self.hurt_dir = -self.hurt_dir;
        self.hurt_time = 10;
        self.damage += amount * 10.0;
        self.changed.extend([DATA_HURT_DIR, DATA_HURT, DATA_DAMAGE]);
    }

    /// A hit that may break it; true when it is gone.
    fn damage(&mut self, amount: f32, creative: bool, ignites: bool, cx: &mut TickContext) -> bool {
        self.mark_hurt(amount);
        if (!creative && self.damage > BREAK_DAMAGE) || (self.is_tnt() && ignites) {
            return self.broken_by(ignites, cx);
        }
        if creative {
            self.destroy(false, cx);
            return true;
        }
        false
    }

    /// Broken by a hit: a TNT minecart lights instead when the hit burns
    /// or it is moving fast; true when it is gone.
    fn broken_by(&mut self, ignites: bool, cx: &mut TickContext) -> bool {
        if self.is_tnt() && (ignites || horizontal_sq(self.base.velocity) >= TNT_VOLATILE_SPEED_SQ)
        {
            if self.fuse < 0 {
                self.prime(cx);
                self.fuse = cx.rng.gen_range(0..20) + cx.rng.gen_range(0..20);
            }
            return false;
        }
        self.destroy(true, cx);
        true
    }

    fn has_container(&self) -> bool {
        !self.items.is_empty()
    }

    /// Lights a TNT minecart.
    fn prime(&mut self, cx: &mut TickContext) {
        if !cx.shared.rule_bool("tnt_explodes") {
            return;
        }
        self.fuse = TNT_FUSE;
        let event = framed(&play::EntityEvent {
            entity_id: self.base.id,
            status: TNT_PRIMED_EVENT,
        });
        cx.effects.push(Effect::EntityPackets(packets([event])));
        if !self.base.silent {
            let sound = framed(&play::Sound {
                sound_event: SoundEvent::EntityTntPrimed.id(),
                category: play::sound_category::BLOCKS,
                position: self.base.position,
                volume: 1.0,
                pitch: 1.0,
                seed: cx.rng.r#gen(),
            });
            cx.effects.push(Effect::ChunkPackets(packets([sound])));
        }
    }

    /// A TNT minecart going off with the force of moving at `speed_sq`:
    /// the tick outcome when it is gone.
    fn explode(&mut self, speed_sq: f64, cx: &mut TickContext) -> Option<Tick> {
        if !cx.shared.rule_bool("tnt_explodes") {
            return (self.fuse >= 0).then_some(Tick::Discard);
        }
        let speed = speed_sq.sqrt().min(5.0);
        let radius = (f64::from(self.power)
            + f64::from(self.speed_factor) * cx.rng.r#gen::<f64>() * 1.5 * speed)
            as f32;
        let decay = cx.shared.rule_bool("tnt_explosion_drop_decay");
        let blocks = if self.fuse >= 0 {
            BlockInteraction::SpareRails { decay }
        } else if decay {
            BlockInteraction::DestroyWithDecay
        } else {
            BlockInteraction::Destroy
        };
        let damage_type = if self.igniter.is_some() {
            &damage_type::PLAYER_EXPLOSION
        } else {
            &damage_type::EXPLOSION
        };
        Some(Tick::Explode(Explosion {
            center: self.base.position,
            radius,
            fire: false,
            blocks,
            damage_type,
            igniter: self.igniter.take(),
            culprit: None,
            dangerous_wither_skull: false,
            spares_nether_portals: false,
        }))
    }

    /// Moves by `delta`, stopping at collision boxes.
    fn move_by(&mut self, delta: [f64; 3], block: BlockLookup) -> Moved {
        collide_move(
            &mut self.base,
            delta,
            &|pos| physics::Shape::collision(block(pos), pos),
            &self.moving_blocks,
        )
    }

    fn max_speed(&self) -> f64 {
        let base = if self.in_water {
            MAX_SPEED_IN_WATER
        } else {
            MAX_SPEED
        };
        match (self.is_furnace(), self.in_water) {
            (true, true) => base * 0.75,
            (true, false) => base * 0.5,
            _ => base,
        }
    }

    fn container_slowdown(&self) -> Option<f64> {
        if self.items.is_empty() {
            return None;
        }
        if self.loot_table.is_some() {
            return Some(f64::from(0.98f32));
        }
        let mut fill = 0.0f32;
        for stack in self.items.iter().flatten() {
            fill += f32::from(stack.count) / f32::from(stack.max_stack_size().min(64));
        }
        fill /= self.items.len() as f32;
        let signal = (fill * 14.0).floor() as i32 + i32::from(fill > 0.0);
        Some(f64::from(0.98f32 + (15 - signal) as f32 * 0.001f32))
    }

    fn natural_slowdown(&mut self) {
        let slowdown = if let Some(keep) = self.container_slowdown() {
            keep
        } else if self.ridden() {
            SLOWDOWN_RIDDEN
        } else {
            SLOWDOWN_EMPTY
        };
        self.base.velocity = self.slowed(self.base.velocity, slowdown);
    }

    /// `v` after a tick's friction of `slowdown` (a furnace minecart's
    /// push first), flat.
    fn slowed(&mut self, mut v: [f64; 3], slowdown: f64) -> [f64; 3] {
        if self.is_furnace() && self.push[0] * self.push[0] + self.push[1] * self.push[1] > 1.0e-7 {
            let [px, pz] = self.push;
            if px * px + pz * pz > 1.0e-4 && horizontal_sq(v) > 0.001 {
                let length = (px * px + pz * pz).sqrt();
                let dot = px * v[0] + pz * v[2];
                let along = dot / horizontal_sq(v);
                let (ax, az) = (v[0] * along, v[2] * along);
                let alen = (ax * ax + az * az).sqrt();
                if alen > 0.0 {
                    self.push = [ax / alen * length, az / alen * length];
                }
            }
            v = [v[0] * 0.8 + self.push[0], 0.0, v[2] * 0.8 + self.push[1]];
            if self.in_water {
                v = v.map(|c| c * 0.1);
            }
        } else if self.is_furnace() {
            v = [v[0] * 0.98, 0.0, v[2] * 0.98];
        }
        v = [v[0] * slowdown, 0.0, v[2] * slowdown];
        if self.in_water {
            v = v.map(|c| c * WATER_SLOWDOWN);
        }
        v
    }

    /// Follows the rail at `pos`.
    fn move_along_track(&mut self, pos: BlockPos, state: BlockState, block: BlockLookup) -> Moved {
        let Some(shape) = state.shape() else {
            return Moved::default();
        };
        let Some((e0, e1)) = exits(shape) else {
            return Moved::default();
        };
        self.base.fall_distance = 0.0;
        let [x, y, z] = self.base.position;
        let old = rail_position(x, y, z, block);
        let mut rail_y = f64::from(pos.y);
        let powered_rail = state.block() == Block::PoweredRail;
        let power_track = powered_rail && state.powered() == Some(true);
        let mut halt_track = powered_rail && !power_track;
        let slide = if self.in_water { SLIDE * 0.2 } else { SLIDE };
        let ridden = self.ridden();
        let intent = move_intent(self.input);
        let v = &mut self.base.velocity;
        match shape {
            Shape::AscendingEast => v[0] -= slide,
            Shape::AscendingWest => v[0] += slide,
            Shape::AscendingNorth => v[2] += slide,
            Shape::AscendingSouth => v[2] -= slide,
            _ => {}
        }
        if is_slope(shape) {
            rail_y += 1.0;
        }
        let mut xd = f64::from(e1[0] - e0[0]);
        let mut zd = f64::from(e1[2] - e0[2]);
        let length = (xd * xd + zd * zd).sqrt();
        if v[0] * xd + v[2] * zd < 0.0 {
            xd = -xd;
            zd = -zd;
        }
        let pow = horizontal_sq(*v).sqrt().min(2.0);
        v[0] = pow * xd / length;
        v[2] = pow * zd / length;
        if ridden && horizontal_sq(intent) > 0.0 && horizontal_sq(*v) < RIDABLE_THRESHOLD {
            v[0] += intent[0] * RIDER_NUDGE;
            v[2] += intent[2] * RIDER_NUDGE;
            halt_track = false;
        }
        if halt_track {
            if horizontal_sq(*v).sqrt() < BRAKE_STOP_SPEED {
                *v = [0.0; 3];
            } else {
                *v = [v[0] * 0.5, 0.0, v[2] * 0.5];
            }
        }
        let (px, pz) = (f64::from(pos.x), f64::from(pos.z));
        let x0 = px + 0.5 + f64::from(e0[0]) * 0.5;
        let z0 = pz + 0.5 + f64::from(e0[2]) * 0.5;
        let x1 = px + 0.5 + f64::from(e1[0]) * 0.5;
        let z1 = pz + 0.5 + f64::from(e1[2]) * 0.5;
        let (xd, zd) = (x1 - x0, z1 - z0);
        let progress = if xd == 0.0 {
            z - pz
        } else if zd == 0.0 {
            x - px
        } else {
            ((x - x0) * xd + (z - z0) * zd) * 2.0
        };
        self.base.position = [x0 + xd * progress, rail_y, z0 + zd * progress];
        let scale = if self.ridden() { 0.75 } else { 1.0 };
        let max = self.max_speed();
        let v = self.base.velocity;
        let moved = self.move_by(
            [
                (scale * v[0]).clamp(-max, max),
                0.0,
                (scale * v[2]).clamp(-max, max),
            ],
            block,
        );
        let cell = |p: [f64; 3]| (p[0].floor() as i32 - pos.x, p[2].floor() as i32 - pos.z);
        let at = cell(self.base.position);
        if e0[1] != 0 && at == (e0[0], e0[2]) {
            self.base.position[1] += f64::from(e0[1]);
        } else if e1[1] != 0 && at == (e1[0], e1[2]) {
            self.base.position[1] += f64::from(e1[1]);
        }
        self.natural_slowdown();
        let [x, y, z] = self.base.position;
        if let (Some(old), Some(new)) = (old, rail_position(x, y, z, block)) {
            let speed = (old[1] - new[1]) * 0.05;
            let v = &mut self.base.velocity;
            let other = horizontal_sq(*v).sqrt();
            if other > 0.0 {
                v[0] *= (other + speed) / other;
                v[2] *= (other + speed) / other;
            }
            self.base.position[1] = new[1];
        }
        let (xn, zn) = cell(self.base.position);
        if xn != 0 || zn != 0 {
            let v = &mut self.base.velocity;
            let other = horizontal_sq(*v).sqrt();
            v[0] = other * f64::from(xn);
            v[2] = other * f64::from(zn);
        }
        if power_track {
            self.boost(pos, shape, block);
        }
        moved
    }

    /// A powered rail speeds a moving cart up, and kicks a stopped one off
    /// a conductor at one end of a flat rail.
    fn boost(&mut self, pos: BlockPos, shape: Shape, block: BlockLookup) {
        let v = &mut self.base.velocity;
        let speed = horizontal_sq(*v).sqrt();
        if speed > RIDABLE_THRESHOLD {
            v[0] += v[0] / speed * BOOST;
            v[2] += v[2] / speed * BOOST;
            return;
        }
        let conductor = |x: i32, z: i32| {
            block(BlockPos::new(pos.x + x, pos.y, pos.z + z))
                .is_some_and(|state| state.is_redstone_conductor())
        };
        match shape {
            Shape::EastWest => {
                if conductor(-1, 0) {
                    v[0] = KICK;
                } else if conductor(1, 0) {
                    v[0] = -KICK;
                }
            }
            Shape::NorthSouth => {
                if conductor(0, -1) {
                    v[2] = KICK;
                } else if conductor(0, 1) {
                    v[2] = -KICK;
                }
            }
            _ => {}
        }
    }

    /// No rail under it: it rolls and falls freely.
    fn come_off_track(&mut self, block: BlockLookup) -> Moved {
        let max = self.max_speed();
        let v = &mut self.base.velocity;
        v[0] = v[0].clamp(-max, max);
        v[2] = v[2].clamp(-max, max);
        if self.base.on_ground {
            *v = v.map(|c| c * 0.5);
        }
        let moved = self.move_by(self.base.velocity, block);
        if !self.base.on_ground {
            self.base.velocity = self.base.velocity.map(|c| c * AIR_DRAG);
        }
        moved
    }

    /// An activator rail under it, powered or not.
    fn activate(&mut self, powered: bool, cx: &mut TickContext) {
        match self.base.kind {
            EntityType::HopperMinecart => self.enabled = !powered,
            EntityType::TntMinecart if powered && self.fuse < 0 => {
                self.prime(cx);
            }
            EntityType::CommandBlockMinecart if powered => {
                if cx.tick.saturating_sub(self.last_activated) >= COMMAND_COOLDOWN {
                    self.last_activated = cx.tick;
                    let command = self.command();
                    let track_output = self.base.extra.get_bool("TrackOutput").unwrap_or(true);
                    self.base.extra.insert("SuccessCount", 0i32);
                    if track_output {
                        self.base.extra.remove("LastOutput");
                    }
                    if !command.is_empty() {
                        let name = self
                            .base
                            .custom_name
                            .as_ref()
                            .and_then(tuff_item::text::TextComponent::from_nbt)
                            .map_or_else(|| "@".to_owned(), |t| t.plain());
                        cx.effects.push(Effect::RunCommand {
                            command,
                            name,
                            position: self.base.position,
                            track_output,
                        });
                    }
                }
            }
            EntityType::Minecart if powered => {
                for rider in std::mem::take(&mut self.base.riders) {
                    let yaw = cx
                        .players
                        .iter()
                        .find(|p| p.id == rider.id)
                        .map_or(self.base.yaw, |p| p.yaw);
                    let position = self.dismount_location(0.6, yaw, cx.block);
                    cx.effects.push(Effect::Dismount { rider, position });
                }
                if self.hurt_time == 0 {
                    self.hurt_dir = -self.hurt_dir;
                    self.hurt_time = 10;
                    self.damage = ACTIVATED_DAMAGE;
                    self.changed.extend([DATA_HURT_DIR, DATA_HURT, DATA_DAMAGE]);
                }
            }
            _ => {}
        }
    }

    /// A tick's movement the default way: gravity, then along the rail it
    /// is on or freely off rails, then it turns to face its way and an
    /// empty passenger cart moving fast enough picks up a mob.
    fn tick_on_rails(&mut self, cx: &mut TickContext) -> Moved {
        let block = cx.block;
        let (start, start_yaw) = (self.base.position, self.base.yaw);
        if !self.base.no_gravity {
            self.base.velocity[1] -= if self.in_water {
                WATER_GRAVITY
            } else {
                GRAVITY
            };
        }
        let [x, y, z] = self.base.position;
        let pos = rail_cell(x, y, z, block);
        let moved = match block(pos).filter(|state| is_rail_block(*state)) {
            Some(state) => {
                let moved = self.move_along_track(pos, state, block);
                if state.block() == Block::ActivatorRail {
                    self.activate(state.powered() == Some(true), cx);
                }
                moved
            }
            None => self.come_off_track(block),
        };
        self.turn(start, start_yaw);
        let [vx, _, vz] = self.base.velocity;
        let empty = self.base.riders.is_empty() && self.base.entity_riders.is_empty();
        if self.rideable() && empty && vx * vx + vz * vz > 0.01 {
            let area = physics::inflate(self.base.bounds(), [0.2, 0.0, 0.2]);
            super::pick_up_riders(&self.base, self.seats(), area, cx, |kind, _| {
                kind != EntityType::IronGolem
            });
        }
        moved
    }

    /// Turns to face the way it moved since `start`.
    fn turn(&mut self, start: [f64; 3], start_yaw: f32) {
        self.base.pitch = 0.0;
        let x_diff = start[0] - self.base.position[0];
        let z_diff = start[2] - self.base.position[2];
        if x_diff * x_diff + z_diff * z_diff > 0.001 {
            self.base.yaw = z_diff.atan2(x_diff).to_degrees() as f32;
            if self.flipped {
                self.base.yaw += 180.0;
            }
        }
        let turned = wrap_degrees(self.base.yaw - start_yaw);
        if !(-170.0..170.0).contains(&turned) {
            self.base.yaw += 180.0;
            self.flipped = !self.flipped;
        }
        self.base.yaw %= 360.0;
    }

    /// Players walking into it push it away.
    fn pushed_by_players(&mut self, cx: &TickContext) {
        let bounds = self.base.bounds();
        for player in cx.players {
            if self.base.riders.iter().any(|rider| rider.id == player.id)
                || !physics::intersects(player.bounds(), bounds)
            {
                continue;
            }
            let xa = player.position[0] - self.base.position[0];
            let za = player.position[2] - self.base.position[2];
            let dd = xa * xa + za * za;
            if dd < 9.999999747378752e-5 {
                continue;
            }
            let dd = dd.sqrt();
            let pow = (1.0 / dd).min(1.0) * 0.10000000149011612 * 0.5;
            self.base.velocity[0] -= xa / dd * pow;
            self.base.velocity[2] -= za / dd * pow;
        }
    }

    /// Pushes and is pushed by mobs it touches, and by other carts unless
    /// it moves the newer way (which pushes only what it bumps into).
    fn collide_with_others(&mut self, cx: &mut TickContext) {
        let bounds = self.base.bounds();
        let grown = [
            bounds[0] - 0.2,
            bounds[1],
            bounds[2] - 0.2,
            bounds[3] + 0.2,
            bounds[4],
            bounds[5] + 0.2,
        ];
        let me = self.base.position;
        for other in cx.entities {
            if other.id == self.base.id || self.base.riders.iter().any(|r| r.id == other.id) {
                continue;
            }
            let xa = other.position[0] - me[0];
            let za = other.position[2] - me[2];
            if is_minecart(other.kind) {
                if self.improved || !physics::intersects(other.bounds, grown) {
                    continue;
                }
                self.push_cart(other, cx);
            } else if other.living.is_some()
                && other.kind != EntityType::Player
                && physics::intersects(other.bounds, bounds)
            {
                let dd = xa * xa + za * za;
                if dd < 9.999999747378752e-5 {
                    continue;
                }
                let dd = dd.sqrt();
                let pow = (1.0 / dd).min(1.0) * 0.10000000149011612 * 0.5;
                let (xp, zp) = (xa / dd * pow, za / dd * pow);
                self.base.velocity[0] -= xp;
                self.base.velocity[2] -= zp;
                cx.effects.push(Effect::Nudge {
                    target: other.id,
                    push: [xp / 4.0, 0.0, zp / 4.0],
                });
            }
        }
    }

    /// The other cart's own push logic with this cart as the pusher.
    fn push_cart(&mut self, other: &tuff_core::world::entity::EntitySummary, cx: &mut TickContext) {
        let xa = self.base.position[0] - other.position[0];
        let za = self.base.position[2] - other.position[2];
        let dd = xa * xa + za * za;
        if dd < 9.999999747378752e-5 {
            return;
        }
        let dd = dd.sqrt();
        let pow = (1.0 / dd).min(1.0) * 0.10000000149011612 * 0.5;
        let (xp, zp) = (xa / dd * pow, za / dd * pow);
        let dir_len = dd;
        let (dx, dz) = (xa / dir_len, za / dir_len);
        let yaw = f64::from(other.yaw) * 0.017453292;
        let (fx, fz) = (yaw.cos(), yaw.sin());
        let flen = (fx * fx + fz * fz).sqrt();
        if (dx * fx / flen + dz * fz / flen).abs() < 0.800000011920929 {
            return;
        }
        let mine = self.base.velocity;
        let theirs = other.velocity;
        let other_furnace = other.kind == EntityType::FurnaceMinecart;
        let me_furnace = self.is_furnace();
        let (my_new, their_new) = if me_furnace && !other_furnace {
            (
                [mine[0] * 0.95, mine[1], mine[2] * 0.95],
                [
                    theirs[0] * 0.2 + mine[0] - xp,
                    theirs[1],
                    theirs[2] * 0.2 + mine[2] - zp,
                ],
            )
        } else if !me_furnace && other_furnace {
            (
                [
                    mine[0] * 0.2 + theirs[0] + xp,
                    mine[1],
                    mine[2] * 0.2 + theirs[2] + zp,
                ],
                [theirs[0] * 0.95, theirs[1], theirs[2] * 0.95],
            )
        } else {
            let (xdd, zdd) = ((theirs[0] + mine[0]) / 2.0, (theirs[2] + mine[2]) / 2.0);
            (
                [mine[0] * 0.2 + xdd + xp, mine[1], mine[2] * 0.2 + zdd + zp],
                [
                    theirs[0] * 0.2 + xdd - xp,
                    theirs[1],
                    theirs[2] * 0.2 + zdd - zp,
                ],
            )
        };
        self.base.velocity = my_new;
        cx.effects.push(Effect::Nudge {
            target: other.id,
            push: [their_new[0] - theirs[0], 0.0, their_new[2] - theirs[2]],
        });
    }

    fn feel_fluids(&mut self, cx: &TickContext) {
        let bounds = self.base.bounds();
        let height = f64::from(self.base.kind.size().height);
        let [x, y, z] = self.base.position;
        let immersion = contact::immersion(cx, bounds, [x, y + height * 0.85, z]);
        self.in_water = immersion.submersion.water > 0.0;
        immersion.push(&mut self.base.velocity);
    }
}

/// Loads a saved minecart.
pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut cart = Minecart::with_base(base);
    if let Some(Tag::Byte(flipped)) = nbt.remove(FLIPPED) {
        cart.flipped = flipped != 0;
    }
    if let Some(Tag::Byte(first)) = nbt.remove(HAS_TICKED) {
        cart.first_tick = first != 0;
    }
    if cart.is_tnt() {
        if let Some(fuse) = take_number(nbt, FUSE) {
            cart.fuse = fuse as i32;
        }
        if let Some(power) = take_number(nbt, EXPLOSION_POWER) {
            cart.power = (power as f32).clamp(0.0, MAX_TNT_POWER);
        }
        if let Some(factor) = take_number(nbt, EXPLOSION_SPEED_FACTOR) {
            cart.speed_factor = (factor as f32).clamp(0.0, MAX_TNT_POWER);
        }
    }
    if cart.is_furnace() {
        cart.push = [
            take_number(nbt, PUSH_X).unwrap_or(0.0),
            take_number(nbt, PUSH_Z).unwrap_or(0.0),
        ];
        cart.fuel = take_number(nbt, FUEL).unwrap_or(0.0) as i32;
    }
    if cart.base.kind == EntityType::HopperMinecart
        && let Some(Tag::Byte(enabled)) = nbt.remove(ENABLED)
    {
        cart.enabled = enabled != 0;
    }
    if cart.has_container() {
        if let Some(Tag::List(entries)) = nbt.remove(ITEMS) {
            cart.items = decode_items(&entries, cart.items.len());
        }
        if let Some(Tag::String(table)) = nbt.remove(LOOT_TABLE) {
            let seed = match nbt.remove(LOOT_TABLE_SEED) {
                Some(Tag::Long(seed)) => seed,
                _ => 0,
            };
            cart.loot_table = Some((table, seed));
        }
    }
    Some(Box::new(cart))
}

/// Puts an edited command into the command block minecart `id`.
fn edit_command(
    world: &mut dyn tuff_core::world::region_api::RegionWorld,
    id: i32,
    command: &str,
    track_output: bool,
) {
    let Some(tracked) = world.entity_mut(id) else {
        return;
    };
    let Some(cart) = tracked.entity.downcast_mut::<Minecart>() else {
        return;
    };
    if !cart.is_command_block() {
        return;
    }
    cart.base.extra.insert("Command", command);
    cart.base.extra.insert("TrackOutput", track_output);
    if !track_output {
        cart.base.extra.remove("LastOutput");
    }
    cart.show_command();
}

/// The command block minecart editor's Done: operators in creative set
/// the command, which the cart's viewers see at once.
pub struct CommandMinecartHooks;

impl tuff_core::world::session::hooks::SessionHooks for CommandMinecartHooks {
    fn packet<'a>(
        &'a self,
        session: &'a mut tuff_core::world::session::PlayerSession,
        packet: &'a mut Option<tuff_core::protocol::play::Serverbound>,
    ) -> tuff_core::world::session::hooks::HookFuture<'a> {
        use tuff_core::protocol::play::Serverbound;
        Box::pin(async move {
            if !matches!(packet, Some(Serverbound::SetCommandMinecart { .. })) {
                return;
            }
            let Some(Serverbound::SetCommandMinecart {
                entity,
                command,
                track_output,
            }) = packet.take()
            else {
                return;
            };
            let allowed = session.mode.abilities.instabuild
                && session.mode.permission >= tuff_storage::admin::PermissionLevel::Gamemasters;
            if !allowed {
                session.send(&play::SystemChat {
                    content: &TextComponent::translate("advMode.notAllowed", []),
                    overlay: false,
                });
                return;
            }
            let dimension = session.dimension_id();
            for ((at, _), region) in &session.view.regions {
                if *at != dimension {
                    continue;
                }
                let command = command.clone();
                region.internal(tuff_core::world::messages::RegionInternal::Run(
                    tuff_core::world::region_api::RegionTask::new(move |world| {
                        edit_command(world, entity, &command, track_output);
                    }),
                ));
            }
            if !command.is_empty() {
                let key = if session.shared.rule_bool("command_blocks_work") {
                    "advMode.setCommand.success"
                } else {
                    "advMode.setCommand.disabled"
                };
                session.send(&play::SystemChat {
                    content: &TextComponent::translate(key, [TextComponent::new(command)]),
                    overlay: false,
                });
            }
        })
    }
}

/// Registers every minecart type's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    for kind in EntityType::ALL {
        if is_minecart(kind) {
            registry.register(kind, load);
        }
    }
}

mod entity;

#[cfg(test)]
#[path = "minecart_tests.rs"]
mod tests;
