//! Entities regions own and save. [`EntityBase`] holds the fields every
//! entity has, loaded and saved once for all types; a type adds its own
//! state and behaviour by implementing [`RegionEntity`], and a [`Registry`]
//! maps saved type ids to loaders, so new types (mobs, projectiles,
//! features) plug in without touching the region or storage code.
//!
//! A region keeps each entity as a [`TrackedEntity`], which adds what the
//! players tracking it need: its sequence number, cached spawn packets and
//! movement pacing.

use std::any::Any;
use std::collections::HashMap;
use std::fmt::Debug;

use bytes::Bytes;
use rand::rngs::StdRng;
use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{BlockState, Item};

use crate::protocol::play::{self, MetadataValue, metadata};
use crate::world::dimension::DimensionId;
use crate::world::explosion::Explosion;
use crate::world::messages::{Packets, PlayerId, Track};
use crate::world::movement_sync::{Movement, MovementSync, Placement, Pose};
use crate::world::{Shared, framed, packets};
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

type NeighbourWork =
    std::sync::Arc<dyn Fn(&mut dyn crate::world::region_api::RegionWorld) + Send + Sync>;

mod effect;
mod registry;
mod tracked;

pub use effect::*;
pub use registry::*;
pub use tracked::*;

const MAX_LOADED_MOTION: f64 = 10.0;
const MAX_HORIZONTAL_POSITION: f64 = 3.0000512e7;
const MAX_VERTICAL_POSITION: f64 = 2.0e7;
const MAX_TAGS: usize = 1024;
const DEFAULT_AIR: i16 = tuff_vanilla_data::entity::MAX_AIR_SUPPLY as i16;
const FIRE_DAMAGE_PERIOD: i16 = 20;
/// Lava sets what falls in alight for 15 seconds and hurts it every tick.
const LAVA_FIRE_TICKS: i16 = 300;
const LAVA_DAMAGE: f32 = 4.0;
/// How long standing in fire sets an entity alight for (eight seconds).
const IN_FIRE_TICKS: i16 = 160;

/// How far an entity's box is shrunk before checking it against lava.
const LAVA_CONTACT_INSET: f64 = 1.0e-5;

/// The saved fields every entity has. Fields this server does not simulate
/// are still loaded, sent to clients where the game does, and saved back;
/// keys no one understands stay in `extra`.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityBase {
    pub id: i32,
    pub uuid: u128,
    pub kind: EntityType,
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
    pub fall_distance: f64,
    /// Ticks left burning; zero or less when not on fire.
    pub fire: i16,
    pub air: i16,
    pub invulnerable: bool,
    pub invulnerable_time: i32,
    pub portal_cooldown: i32,
    pub custom_name: Option<Tag>,
    pub custom_name_visible: bool,
    pub silent: bool,
    pub no_gravity: bool,
    pub glowing: bool,
    pub invisible: bool,
    /// Glowing from the effect rather than the saved tag; not saved.
    pub glowing_effect: bool,
    pub ticks_frozen: i32,
    pub has_visual_fire: bool,
    pub tags: Vec<String>,
    /// Saved riders this server does not understand (non-player entities
    /// nested inside a vehicle's own `Passengers`): kept as saved, not
    /// resolved into live riders.
    pub passengers: Vec<Compound>,
    /// Players riding it now, the driver first; not saved directly (see
    /// [`Self::offline_riders`], which is what actually round-trips them).
    pub riders: Vec<Rider>,
    /// UUIDs of players who were riding this entity but are not connected
    /// right now (logged out while mounted, or reloaded from a save taken
    /// while they were riding). A rejoining player whose UUID is here gets
    /// remounted; written into `Passengers` as `{id: "minecraft:player",
    /// UUID: [I;4]}` markers on save, alongside `riders` (which cannot
    /// itself be saved: a `Rider` holds a live channel handle).
    pub offline_riders: Vec<u128>,
    /// Other entities of the region riding it now, by id, seated after
    /// `riders`; saved nested in its `Passengers`.
    pub entity_riders: Vec<i32>,
    /// Ids of everyone aboard in the order they boarded; the seat order of
    /// `riders` and `entity_riders` together (see [`all_passengers`]).
    pub mount_order: Vec<i32>,
    /// What it rides, when it is an entity's passenger; not saved (the
    /// vehicle saves it nested).
    pub vehicle: Option<Mount>,
    pub extra: Compound,
}

/// The vehicle an entity rides, and everyone on that vehicle, in seat
/// order, as viewers need to see them seated.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mount {
    pub id: i32,
    pub passengers: Vec<i32>,
}

/// `point` turned to face `yaw`, as attachment points turn with their
/// entity.
pub fn turned(point: [f64; 3], yaw: f32) -> [f64; 3] {
    let angle = f64::from(-yaw * 0.017_453_292);
    let (sin, cos) = (angle.sin(), angle.cos());
    [
        point[0] * cos + point[2] * sin,
        point[1],
        point[2] * cos - point[0] * sin,
    ]
}

/// A player riding an entity.
#[derive(Debug, Clone)]
pub struct Rider {
    pub id: i32,
    pub uuid: u128,
    pub handle: crate::world::messages::PlayerHandle,
}

/// One tick's worth of steering from a mount's driving rider: forward/back
/// and left/right (each in -1.0..=1.0, from the held movement keys), the
/// jump key, and the rider's current facing (a mount snaps to it, as
/// vanilla does, rather than turning gradually). This is the server-side
/// steering path (see `RegionEntity::rideable_by_input`): distinct from
/// `driven_by_rider`, which is for vehicles the rider's own client
/// simulates and reports the position of (boats).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RiderInput {
    pub forward: f32,
    pub strafe: f32,
    pub jumping: bool,
    pub yaw: f32,
    /// The rider's look pitch, which a flying mount climbs or dives by.
    pub pitch: f32,
    pub sprinting: bool,
    /// The charge, 1 to 100, of a jump the rider released this tick.
    pub jump_charge: Option<u8>,
    pub main_hand: Option<tuff_vanilla_data::Item>,
    pub off_hand: Option<tuff_vanilla_data::Item>,
}

impl RiderInput {
    pub fn holds(&self, item: tuff_vanilla_data::Item) -> bool {
        self.main_hand == Some(item) || self.off_hand == Some(item)
    }
}

/// How fast a driving rider can move a mount its client simulates: the
/// movement speed its input drives on the ground, and the horizontal
/// speed one jump or dash adds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RiddenSpeed {
    pub speed: f64,
    pub leap: f64,
    /// The most vertical speed one jump gives, or 0 where unknown.
    pub launch: f64,
    /// Levitation level (amplifier + 1), 0 without the effect.
    pub levitation: u8,
    pub slow_falling: bool,
}

impl PartialEq for Rider {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.uuid == other.uuid
    }
}

/// A numeric tag of any width, as the game's lenient number codecs read.
pub fn number(tag: &Tag) -> Option<f64> {
    match *tag {
        Tag::Byte(v) => Some(v.into()),
        Tag::Short(v) => Some(v.into()),
        Tag::Int(v) => Some(v.into()),
        Tag::Long(v) => Some(v as f64),
        Tag::Float(v) => Some(v.into()),
        Tag::Double(v) => Some(v),
        _ => None,
    }
}

fn numbers<const N: usize>(tag: &Tag) -> Option<[f64; N]> {
    let list = tag.as_list()?;
    let values: Vec<f64> = list.iter().map(number).collect::<Option<_>>()?;
    values.try_into().ok()
}

/// Removes `key` from `nbt` when `parse` accepts its value, so a value of
/// an unexpected type stays behind and is saved back unchanged.
pub fn take<T>(nbt: &mut Compound, key: &str, parse: impl FnOnce(&Tag) -> Option<T>) -> Option<T> {
    let value = parse(nbt.get(key)?)?;
    nbt.remove(key);
    Some(value)
}

/// Like [`take`] for any numeric tag, as the game reads numbers leniently.
pub fn take_number(nbt: &mut Compound, key: &str) -> Option<f64> {
    take(nbt, key, number)
}

pub fn take_bool(nbt: &mut Compound, key: &str) -> Option<bool> {
    take(nbt, key, |tag| number(tag).map(|v| v != 0.0))
}

impl EntityBase {
    pub fn new(
        kind: EntityType,
        id: i32,
        uuid: u128,
        position: [f64; 3],
        velocity: [f64; 3],
    ) -> Self {
        Self {
            id,
            uuid,
            kind,
            position,
            velocity,
            yaw: 0.0,
            pitch: 0.0,
            on_ground: false,
            fall_distance: 0.0,
            fire: 0,
            air: DEFAULT_AIR,
            invulnerable: false,
            invulnerable_time: 0,
            portal_cooldown: 0,
            custom_name: None,
            custom_name_visible: false,
            silent: false,
            no_gravity: false,
            glowing: false,
            invisible: false,
            glowing_effect: false,
            ticks_frozen: 0,
            has_visual_fire: false,
            tags: Vec::new(),
            passengers: Vec::new(),
            riders: Vec::new(),
            offline_riders: Vec::new(),
            entity_riders: Vec::new(),
            mount_order: Vec::new(),
            vehicle: None,
            extra: Compound::new(),
        }
    }

    /// Takes the common fields out of `nbt`, leaving the rest for the
    /// type's loader. `None` without a finite position.
    pub fn load(kind: EntityType, id: i32, nbt: &mut Compound) -> Option<Self> {
        let position = numbers::<3>(nbt.get("Pos")?).filter(|p| p.iter().all(|c| c.is_finite()))?;
        nbt.remove("Pos");
        let position = [
            position[0].clamp(-MAX_HORIZONTAL_POSITION, MAX_HORIZONTAL_POSITION),
            position[1].clamp(-MAX_VERTICAL_POSITION, MAX_VERTICAL_POSITION),
            position[2].clamp(-MAX_HORIZONTAL_POSITION, MAX_HORIZONTAL_POSITION),
        ];
        let velocity = take(nbt, "Motion", numbers::<3>)
            .unwrap_or_default()
            .map(|v| if v.abs() > MAX_LOADED_MOTION { 0.0 } else { v });
        let uuid = take(nbt, "UUID", Tag::as_uuid).unwrap_or_else(rand::random);
        let mut base = Self::new(kind, id, uuid, position, velocity);
        if let Some([yaw, pitch]) = nbt
            .get("Rotation")
            .and_then(numbers::<2>)
            .filter(|r| r.iter().all(|c| c.is_finite()))
        {
            nbt.remove("Rotation");
            (base.yaw, base.pitch) = (yaw as f32, pitch as f32);
        }
        base.fall_distance = take_number(nbt, "fall_distance").unwrap_or(0.0);
        base.fire = take_number(nbt, "Fire").map_or(0, |v| v as i16);
        base.air = take_number(nbt, "Air").map_or(DEFAULT_AIR, |v| v as i16);
        base.on_ground = take_bool(nbt, "OnGround").unwrap_or(false);
        base.invulnerable = take_bool(nbt, "Invulnerable").unwrap_or(false);
        base.invulnerable_time = take_number(nbt, "invulnerable_time").map_or(0, |v| v as i32);
        base.portal_cooldown = take_number(nbt, "PortalCooldown").map_or(0, |v| v as i32);
        base.custom_name = nbt.remove("CustomName");
        base.custom_name_visible = take_bool(nbt, "CustomNameVisible").unwrap_or(false);
        base.silent = take_bool(nbt, "Silent").unwrap_or(false);
        base.no_gravity = take_bool(nbt, "NoGravity").unwrap_or(false);
        base.glowing = take_bool(nbt, "Glowing").unwrap_or(false);
        base.invisible = take_bool(nbt, "Invisible").unwrap_or(false);
        base.ticks_frozen = take_number(nbt, "TicksFrozen").map_or(0, |v| v as i32);
        base.has_visual_fire = take_bool(nbt, "HasVisualFire").unwrap_or(false);
        base.tags = take(nbt, "Tags", |tag| {
            let list = tag.as_list()?;
            (list.len() <= MAX_TAGS).then_some(())?;
            list.iter()
                .map(|t| t.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or_default();
        let passengers = take(nbt, "Passengers", |tag| {
            tag.as_list()?
                .iter()
                .map(|t| t.as_compound().cloned())
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or_default();
        let (players, rest): (Vec<_>, Vec<_>) = passengers.into_iter().partition(|entry| {
            entry.get("id").and_then(Tag::as_str) == Some("minecraft:player")
                && entry.get("UUID").is_some()
        });
        base.offline_riders = players
            .iter()
            .filter_map(|entry| entry.get("UUID").and_then(Tag::as_uuid))
            .collect();
        base.passengers = rest;
        Some(base)
    }

    /// Writes the fields saved before a type's own, in the game's order.
    pub fn save(&self, out: &mut Compound) {
        out.insert("Pos", Tag::List(self.position.map(Tag::Double).to_vec()));
        out.insert("Motion", Tag::List(self.velocity.map(Tag::Double).to_vec()));
        out.insert(
            "Rotation",
            Tag::List(vec![Tag::Float(self.yaw), Tag::Float(self.pitch)]),
        );
        out.insert("fall_distance", self.fall_distance);
        out.insert("Fire", self.fire);
        out.insert("Air", self.air);
        out.insert("OnGround", self.on_ground);
        out.insert("Invulnerable", self.invulnerable);
        out.insert("PortalCooldown", self.portal_cooldown);
        if self.invulnerable_time > 0 {
            out.insert("invulnerable_time", self.invulnerable_time);
        }
        out.insert("UUID", Tag::uuid(self.uuid));
        if let Some(name) = &self.custom_name {
            out.insert("CustomName", name.clone());
        }
        for (key, set) in [
            ("CustomNameVisible", self.custom_name_visible),
            ("Silent", self.silent),
            ("NoGravity", self.no_gravity),
            ("Glowing", self.glowing),
            ("Invisible", self.invisible),
        ] {
            if set {
                out.insert(key, true);
            }
        }
        if self.ticks_frozen > 0 {
            out.insert("TicksFrozen", self.ticks_frozen);
        }
        if self.has_visual_fire {
            out.insert("HasVisualFire", true);
        }
        if !self.tags.is_empty() {
            let tags = self.tags.iter().map(|t| Tag::from(t.as_str())).collect();
            out.insert("Tags", Tag::List(tags));
        }
    }

    pub fn chunk(&self) -> ChunkPos {
        ChunkPos::of_position(self.position)
    }

    pub fn on_fire(&self) -> bool {
        self.fire > 0 || self.has_visual_fire
    }

    pub fn flags(&self) -> i8 {
        let mut flags = 0;
        if self.on_fire() {
            flags |= metadata::FLAG_ON_FIRE;
        }
        if self.glowing || self.glowing_effect {
            flags |= metadata::FLAG_GLOWING;
        }
        if self.invisible {
            flags |= metadata::FLAG_INVISIBLE;
        }
        flags
    }

    /// The entity data entries every type has, where they differ from what
    /// a client assumes for a new entity.
    pub fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        let mut values = Vec::new();
        let flags = self.flags();
        if flags != 0 {
            values.push((metadata::ENTITY_FLAGS, MetadataValue::Byte(flags)));
        }
        if self.air != DEFAULT_AIR {
            values.push((metadata::AIR_SUPPLY, MetadataValue::VarInt(self.air.into())));
        }
        if self.ticks_frozen > 0 {
            values.push((
                metadata::TICKS_FROZEN,
                MetadataValue::VarInt(self.ticks_frozen),
            ));
        }
        if let Some(name) = self.custom_name.as_ref().and_then(TextComponent::from_nbt) {
            values.push((
                metadata::CUSTOM_NAME,
                MetadataValue::OptionalText(Some(name)),
            ));
        }
        for (index, set) in [
            (metadata::CUSTOM_NAME_VISIBLE, self.custom_name_visible),
            (metadata::SILENT, self.silent),
            (metadata::NO_GRAVITY, self.no_gravity),
        ] {
            if set {
                values.push((index, MetadataValue::Boolean(true)));
            }
        }
        values
    }

    /// The box the entity occupies, from its type's size.
    /// Where the entity's eyes are, by its type's dumped eye height.
    pub fn eye_position(&self) -> [f64; 3] {
        let [x, y, z] = self.position;
        [x, y + f64::from(self.kind.size().eye_height), z]
    }

    pub fn bounds(&self) -> [f64; 6] {
        let size = self.kind.size();
        let half = f64::from(size.width) / 2.0;
        let [x, y, z] = self.position;
        [
            x - half,
            y,
            z - half,
            x + half,
            y + f64::from(size.height),
            z + half,
        ]
    }
}

/// The eye height of an entity of `kind` occupying `bounds`: the type's
/// eye height scaled like its box (babies).
pub fn eye_height(kind: EntityType, bounds: [f64; 6]) -> f64 {
    let size = kind.size();
    let eye = f64::from(size.eye_height);
    if size.height <= 0.0 {
        return eye;
    }
    eye * (bounds[4] - bounds[1]) / f64::from(size.height)
}

/// A player near the region, as entities see it.
#[derive(Debug, Clone, Copy)]
pub struct NearbyPlayer<'a> {
    pub id: PlayerId,
    pub uuid: u128,
    pub name: &'a str,
    pub position: [f64; 3],
    pub eye_height: f64,
    pub yaw: f32,
    pub pitch: f32,
    /// What the main and off hand hold.
    pub held: [Option<Item>; 2],
    pub creative: bool,
    /// The player's box height, which follows its pose.
    pub height: f64,
    pub sneaking: bool,
    pub invisible: bool,
    /// Creative players (and those with the ability) are not attacked.
    pub invulnerable: bool,
    /// Feet, legs, chest and head.
    pub armor: [Option<Item>; 4],
    /// Its active effects, one bit per registry id.
    pub effects: u64,
    /// How far its client last moved it in one tick.
    pub movement: [f64; 3],
    /// The entity of this region it rides.
    pub vehicle: Option<i32>,
    /// Ticks it has lain in bed (capped at 100) while asleep.
    pub sleeping: Option<u32>,
    /// Who hurt it lately (id, stamp) in a way wolves answer.
    pub last_hurt_by: Option<(i32, u64)>,
    /// What it attacked last (id, stamp).
    pub last_hurt_mob: Option<(i32, u64)>,
    /// A tamed parrot may land on its shoulder.
    pub perchable: bool,
    pub sprinting: bool,
}

impl NearbyPlayer<'_> {
    pub fn bounds(&self) -> [f64; 6] {
        let [x, y, z] = self.position;
        [x - 0.3, y, z - 0.3, x + 0.3, y + self.height, z + 0.3]
    }

    pub fn eye(&self) -> [f64; 3] {
        let [x, y, z] = self.position;
        [x, y + self.eye_height, z]
    }

    /// How far away a mob of `kind` notices this player, as a fraction of
    /// its follow range: sneaking, invisibility and heads worn for the mob.
    pub fn visibility(&self, kind: EntityType) -> f64 {
        let mut visibility = 1.0;
        if self.sneaking {
            visibility *= 0.8;
        }
        if self.invisible {
            let worn = self.armor.iter().filter(|a| a.is_some()).count() as f32;
            visibility *= 0.7 * f64::from((worn / 4.0).max(0.1));
        }
        if let Some(head) = self.armor[3]
            && let Some((kinds, factor)) = head.mob_visibility()
            && kinds.contains(&kind)
        {
            visibility *= f64::from(factor);
        }
        visibility.clamp(0.0, 10.0)
    }
}

/// Who caused damage, as a victim sees it: for death messages, retaliation
/// and damage type conditions.
#[derive(Debug, Clone, PartialEq)]
pub struct Culprit {
    pub id: i32,
    pub uuid: u128,
    pub kind: EntityType,
    pub name: TextComponent,
    /// A charged creeper, one of whose victims drops its head.
    pub charged: Option<HeadDrop>,
    /// The online player a tamed attacker belongs to, whose team it takes.
    pub owner_name: Option<String>,
    /// The custom name of the item in its hand, for death messages.
    pub held_name: Option<Box<TextComponent>>,
}

/// The one head a charged creeper's blast may drop, shared by every
/// victim of it.
#[derive(Debug, Clone, Default)]
pub struct HeadDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl HeadDrop {
    /// No victim has dropped its head yet.
    pub fn available(&self) -> bool {
        !self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Takes the head drop; false if another victim already did.
    pub fn claim(&self) -> bool {
        !self.0.swap(true, std::sync::atomic::Ordering::AcqRel)
    }
}

impl PartialEq for HeadDrop {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Culprit {
    pub fn player(id: PlayerId, uuid: u128, name: impl Into<String>) -> Self {
        Self {
            id,
            uuid,
            kind: EntityType::Player,
            name: TextComponent::new(name.into()),
            charged: None,
            owner_name: None,
            held_name: None,
        }
    }

    /// Remembers the weapon's custom name, shown in bracketed form.
    pub fn holding(mut self, weapon: Option<&ItemStack>) -> Self {
        self.held_name = weapon
            .and_then(|stack| stack.components.custom_name.as_deref())
            .map(|name| {
                Box::new(
                    TextComponent::new("[")
                        .append(name.clone())
                        .append(TextComponent::new("]")),
                )
            });
        self
    }

    /// The player's name, when a player is the culprit.
    pub fn player_name(&self) -> Option<&str> {
        (self.kind == EntityType::Player).then_some(self.name.text.as_str())
    }

    /// An entity by its custom name, or its type's name.
    pub fn entity(base: &EntityBase) -> Self {
        let name = base
            .custom_name
            .as_ref()
            .and_then(TextComponent::from_nbt)
            .unwrap_or_else(|| {
                let (namespace, path) = base.kind.name().split_once(':').unwrap_or(("", ""));
                TextComponent::translate(format!("entity.{namespace}.{path}"), [])
            });
        Self {
            id: base.id,
            uuid: base.uuid,
            kind: base.kind,
            name,
            charged: None,
            owner_name: None,
            held_name: None,
        }
    }

    pub fn is_player(&self) -> bool {
        self.kind == EntityType::Player
    }
}

/// A boss health bar a [`RegionEntity`] wants shown to nearby players (see
/// [`RegionEntity::boss_bar`]): the wire fields of the game's Boss Event
/// packet, minus the identifying UUID (the entity's own).
#[derive(Debug, Clone)]
pub struct BossBarState {
    pub name: TextComponent,
    /// 0.0 to 1.0.
    pub progress: f32,
    pub color: i32,
    pub overlay: i32,
    pub flags: u8,
}

/// Damage an entity (a mob's attack, a projectile) does to a player or
/// another entity.
#[derive(tuff_item::item::DupClone, Debug)]
pub struct EntityHit {
    pub amount: f32,
    pub kind: &'static DamageType,
    /// The entity to blame (the shooter of an arrow).
    pub attacker: Option<Culprit>,
    /// The entity that hit, when it is not the attacker (a projectile).
    pub projectile: Option<EntityType>,
    /// The entity id of that projectile.
    pub projectile_id: Option<i32>,
    /// Where the hit comes from, for knockback and shields.
    pub position: Option<[f64; 3]>,
    /// Knockback on top of the usual push, and its direction (x, z).
    pub knockback: Option<(f64, f64, f64)>,
    /// A push added to the victim's motion, its horizontal part reduced by
    /// knockback resistance (a Punch arrow).
    pub push: Option<[f64; 3]>,
    /// Set when `push` is a throw worked out for this power: the victim's
    /// knockback resistance is taken off the power, not multiplied in.
    pub push_power: Option<f64>,
    /// The attacker wants a `Notice::HitLanded` if this hurts the victim.
    pub report_landed: bool,
    /// The attacker wants a `Notice::ShieldBlocked` if a raised shield
    /// stops this hit.
    pub report_blocked: bool,
    /// Seconds of fire the hit sets the victim alight for, if it lands.
    pub ignite: f32,
    /// The attacker's weapon: its enchantments act on the victim.
    pub weapon: Option<ItemStack>,
    /// The part hit, for entities made of parts.
    pub part: Option<usize>,
    /// Status effects the victim gets if the hit lands (a wither
    /// skeleton's wither, a stray's slowness arrow).
    pub effects: Vec<crate::world::effects::ActiveEffect>,
    /// An arrow that stays stuck in the victim if the hit lands.
    pub sticks: bool,
    /// The projectile, bounced back, if the hit fails to hurt.
    pub rebound: Option<Rebound>,
    /// How fast the projectile flew when it hit.
    pub projectile_velocity: Option<[f64; 3]>,
}

/// A type of entity a region owns. The region handles what is common:
/// movement across chunks and regions, pacing and sending movement,
/// saving, loading and tracking; the type supplies its own state, physics
/// and the entity data clients need.
pub trait RegionEntity: Any + Send + Debug {
    fn base(&self) -> &EntityBase;
    fn base_mut(&mut self) -> &mut EntityBase;

    /// The saved type id; the entity type's name unless a feature maps its
    /// own type onto a vanilla one.
    fn saved_id(&self) -> &str {
        self.base().kind.name()
    }

    /// Writes the fields only this type has; the common ones are written by
    /// [`save`].
    fn save(&self, out: &mut Compound);

    /// Entity data beyond the common entries, for spawn packets.
    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        Vec::new()
    }

    /// The Spawn Entity packet's data field.
    fn spawn_data(&self) -> i32 {
        0
    }

    /// Where the Spawn Entity packet puts it (block-attached entities
    /// send their block, not their centre).
    fn spawn_position(&self) -> [f64; 3] {
        self.base().position
    }

    /// Whether its entity data changes every tick without being resent
    /// (a fuse clients count down themselves), so spawn packets are built
    /// fresh for each report instead of cached.
    fn ticking_data(&self) -> bool {
        false
    }

    fn bounds(&self) -> [f64; 6] {
        self.base().bounds()
    }

    /// How viewers' copies collide when that differs from the default box.
    fn track_shape(&self) -> Option<crate::world::messages::Shape> {
        None
    }

    /// The name selectors and command feedback use.
    fn display_name(&self) -> String {
        self.base().kind.name().to_owned()
    }

    /// Experience it holds, for commands that count it.
    fn experience(&self) -> i32 {
        0
    }

    /// Takes damage; true when that destroyed it. Invulnerable entities are
    /// never asked.
    fn hurt(&mut self, _amount: f32, _damage_type: &'static DamageType) -> bool {
        false
    }

    /// Whether damage of this type never hurts it (a fire resistant item
    /// in lava).
    fn resists(&self, _damage_type: &'static DamageType) -> bool {
        false
    }

    /// [`hurt`](Self::hurt) when something is to blame (a blast's source).
    fn hurt_blamed(
        &mut self,
        amount: f32,
        damage_type: &'static DamageType,
        _culprit: Option<&Culprit>,
    ) -> bool {
        self.hurt(amount, damage_type)
    }

    fn tick(&mut self, context: &mut TickContext) -> Tick;

    /// The head's yaw, for types whose head turns apart from the body.
    fn head_yaw(&self) -> f32 {
        self.base().yaw
    }

    /// Its current value for an entity debug feed (see
    /// `debug_feeds::ENTITY_FEEDS`), asked only while someone subscribes.
    fn debug_value(
        &self,
        _feed: tuff_vanilla_data::registry::DebugSubscription,
        _tick: u64,
    ) -> Option<crate::protocol::play::DebugValue> {
        None
    }

    /// The entity it attacks, if it is a mob with a target.
    fn attack_target(&self) -> Option<i32> {
        None
    }

    /// It as advancement predicates see it.
    fn facts(&self) -> crate::world::predicate::EntityFacts {
        crate::world::predicate::EntityFacts {
            kind: Some(self.base().kind),
            position: self.base().position,
            ..Default::default()
        }
    }

    /// How far it shows on players' locator bars; zero when it does not.
    fn waypoint_transmit_range(&self) -> f64 {
        0.0
    }

    /// What other mobs see of it, for living types.
    fn living(&self) -> Option<LivingSummary> {
        None
    }

    /// Whether projectiles hit it though it is not alive (end crystals).
    fn pickable(&self) -> bool {
        false
    }

    /// Whether it is a sheep with blue wool.
    fn blue_sheep(&self) -> bool {
        false
    }

    /// Whether viewers are sent its rotation and head, as for living
    /// things.
    fn rotates(&self) -> bool {
        self.living().is_some()
    }

    /// Whether portals and gateways take it (not the dragon).
    fn uses_portals(&self) -> bool {
        true
    }

    /// Called once it has been moved to another dimension by a portal.
    fn used_portal(&mut self) {}

    /// How many ids after its own it needs for parts clients number after
    /// it, asked once when a region takes it in; zero once reserved.
    fn take_id_span(&mut self) -> i32 {
        0
    }

    /// The boxes of the parts clients hit separately (the dragon's), in
    /// part order.
    fn parts(&self) -> Vec<[f64; 6]> {
        Vec::new()
    }

    /// Where each part stands (its feet), in part order.
    fn part_positions(&self) -> Vec<[f64; 3]> {
        Vec::new()
    }

    /// World damage to one part; like [`RegionEntity::hurt`].
    fn hurt_part(&mut self, _part: usize, _amount: f32, _damage_type: &'static DamageType) -> bool {
        false
    }

    /// Entity data changed since last asked, for the viewers.
    fn take_changed_data(&mut self) -> Vec<(u8, MetadataValue)> {
        Vec::new()
    }

    /// Queues data index `index` to go out with the next changed data;
    /// indices the type does not track are ignored.
    fn resend_data(&mut self, _index: u8) {}

    /// A player's melee attack; `None` if it cannot be attacked.
    fn attacked(
        &mut self,
        _attack: &EntityAttack,
        _context: &mut TickContext,
    ) -> Option<AttackOutcome> {
        None
    }

    /// An explosion blamed on `owner` reached it: a redirectable projectile
    /// becomes theirs.
    fn redirect_owner(&mut self, _owner: u128) {}

    /// A hit by a mob or a projectile; `None` if it cannot be hurt so.
    fn hit(&mut self, _hit: &EntityHit, _context: &mut TickContext) -> Option<AttackOutcome> {
        None
    }

    /// Attribute values the client syncs for it, empty for entities
    /// without attributes.
    fn synced_attributes(&self) -> Vec<(tuff_vanilla_data::registry::Attribute, f64)> {
        Vec::new()
    }

    /// What it wears and holds in each slot it has, by equipment slot id,
    /// for viewers.
    fn equipment(&self) -> Vec<(u8, Option<ItemStack>)> {
        Vec::new()
    }

    /// What it holds in its main hand; `None` when it has no hands.
    fn main_hand_item(&self) -> Option<Option<ItemStack>> {
        None
    }

    fn set_main_hand_item(&mut self, _stack: Option<ItemStack>) {}

    /// Whether its equipment changed since last asked.
    fn take_equipment_changed(&mut self) -> bool {
        false
    }

    /// A player's right click.
    fn interact(
        &mut self,
        _interaction: &EntityInteraction,
        _context: &mut TickContext,
    ) -> InteractOutcome {
        None
    }

    fn notice(&mut self, _notice: Notice) {}

    /// What an [`Effect::TakeItem`] it asked for actually took, with its
    /// components.
    fn took_item(&mut self, _stack: ItemStack) {}

    /// Shears it without a player (a dispenser); false if it has nothing
    /// to shear right now.
    fn shear(&mut self, _context: &mut TickContext) -> bool {
        false
    }

    /// A dispenser fires `stack` at it (equipping, a brush, a chest);
    /// `None` when it has no use for the item.
    fn dispensed_on(
        &mut self,
        _stack: &ItemStack,
        _context: &mut TickContext,
    ) -> Option<crate::world::region_api::DispenseOutcome> {
        None
    }

    /// What a player touching it could pick up (a stuck arrow), if
    /// anything right now.
    fn ground_pickup(&mut self) -> Option<GroundPickup<'_>> {
        None
    }

    /// Whether resting on a wooden button keeps it pressed.
    fn presses_buttons(&self) -> bool {
        false
    }

    /// Whether it is a cat sitting down, which keeps a chest below shut.
    fn sits_on_chests(&self) -> bool {
        false
    }

    /// Whether it is a minecart, which detector rails sense.
    fn rides_rails(&self) -> bool {
        false
    }

    /// The steps it took along a track since viewers last heard, which
    /// they replay instead of plain movement updates (minecarts in a world
    /// with the minecart improvements feature); `None` for anything else.
    fn track_steps(&mut self) -> Option<&mut Vec<play::TrackStep>> {
        None
    }

    /// A shulker box lid opened into it.
    fn shulker_box_pushed(&mut self) {}

    /// Its region is in crowd mode: its next tick may run simplified.
    fn crowd(&mut self) {}

    /// Whether moving blocks pass through it without pushing it.
    fn ignores_pistons(&self) -> bool {
        false
    }

    /// A spear's extra knockback with no damage.
    fn knocked(&mut self, _power: f32, _dx: f64, _dz: f64) {}

    /// Whether something pushed it since last asked, so its velocity must
    /// go out now.
    fn take_impulse(&mut self) -> bool {
        false
    }

    /// Whether its position was flagged for sync (it bounced) since last
    /// asked, so viewers get the stops it made.
    fn take_position_sync(&mut self) -> bool {
        false
    }

    /// The stacks an item entity holds inside its own stack (a shulker
    /// box's slots, a bundle's contents), taken as it is destroyed.
    fn spilled_contents(&mut self) -> Vec<ItemStack> {
        Vec::new()
    }

    /// A boss health bar it wants shown to nearby players this tick
    /// (`None`, the default, shows nothing). A small, generic hook so any
    /// boss-tier mob -- or a future feature's -- gets one broadcast by the
    /// region (`region/boss_bar.rs`) without any tracking-system changes.
    fn boss_bar(&self) -> Option<BossBarState> {
        None
    }

    /// For an item frame: the way it faces (away from the block it hangs
    /// on) and what a comparator reading it through that block gets. The
    /// region notices changes once a tick and tells the comparators.
    fn comparator_frame(&self) -> Option<(tuff_vanilla_data::block::Facing, u8)> {
        None
    }

    /// The slots of an inventory players can open on it (a chest boat),
    /// which the region serves as a menu like a block container.
    fn container(&self) -> Option<&[Option<ItemStack>]> {
        None
    }

    fn container_mut(&mut self) -> Option<&mut [Option<ItemStack>]> {
        None
    }

    /// What the entity holds on display or wears, indexed by the slot
    /// numbers its held-item edits report (an item frame, an armor stand).
    fn held_items(&self) -> Option<Vec<Option<ItemStack>>> {
        None
    }

    /// Sets held slots as [`Self::held_items`] indexes them; false if
    /// none was valid.
    fn set_held_items(&mut self, _slots: Vec<(usize, Option<ItemStack>)>) -> bool {
        false
    }

    /// Whether `player` opens its container now: with a click (`sneaking`
    /// as given) that did not mount it, or with the inventory key while
    /// riding it (`sneaking` true). A mount opens only for some players.
    fn container_opens_for(&self, _player: i32, _sneaking: bool) -> bool {
        true
    }

    /// A loot table (and seed, 0 for none) its container still has to
    /// roll on first open; taking it clears it.
    fn take_loot_table(&mut self) -> Option<(String, i64)> {
        None
    }

    fn loot_pending(&self) -> bool {
        false
    }

    /// The menu its container opens as; a 9-wide grid by default.
    fn container_menu(&self) -> crate::world::menu::MenuKind {
        let rows = self.container().map_or(0, <[_]>::len) / 9;
        crate::world::menu::MenuKind::Generic { rows: rows as u8 }
    }

    /// Whether it acts as a hopper this tick (an enabled hopper minecart):
    /// the region then pulls one item a tick into its container from the
    /// container above it or from dropped items around it.
    fn collects_items(&self) -> bool {
        false
    }

    /// The offspring of breeding with `partner`, if they can breed.
    fn breed(
        &mut self,
        _partner: &mut dyn RegionEntity,
        _context: &mut TickContext,
    ) -> Option<Box<dyn RegionEntity>> {
        None
    }

    /// A player took one of its offers (merchants): true when it agrees.
    /// `None` when it is no merchant.
    fn trade(
        &mut self,
        _request: &crate::world::trading::TradeRequest,
        _context: &mut TickContext,
    ) -> Option<bool> {
        None
    }

    /// The player `player` closed its trading screen.
    fn stop_trading(&mut self, _player: u128) {}

    /// Lasting potion effects reach it (a splash potion).
    fn add_effects(&mut self, _effects: &[crate::world::effects::ActiveEffect]) {}

    /// `/effect give`: false if it cannot take the effect or nothing
    /// changed.
    fn give_effect(&mut self, _effect: crate::world::effects::ActiveEffect) -> bool {
        false
    }

    /// `/effect clear`: one effect, or all with `None`. False if nothing
    /// was removed.
    fn clear_effects(&mut self, _effect: Option<tuff_vanilla_data::registry::MobEffect>) -> bool {
        false
    }

    /// `/attribute`: reads or edits one attribute. Only living entities
    /// answer anything but `NotLiving`.
    fn edit_attribute(
        &mut self,
        _command: &crate::world::messages::AttributeCommand,
    ) -> crate::world::messages::AttributeAnswer {
        crate::world::messages::AttributeAnswer::NotLiving
    }

    /// `/debugpath`: searches a ground path to `target` as a mob would.
    fn debug_path(
        &mut self,
        _target: tuff_world_model::BlockPos,
        _cx: &mut TickContext,
    ) -> DebugPath {
        DebugPath::NotMob
    }

    /// Points of interest it has claimed (a villager's bed and job site).
    fn poi_claims(&self) -> Vec<tuff_world_model::BlockPos> {
        Vec::new()
    }

    /// The bed it is sleeping in, if any. Lets a player's right-click on an
    /// occupied bed find and eject the mob rather than just being told it
    /// is occupied (that message is for an occupant that is a player).
    fn sleeping_in(&self) -> Option<BlockPos> {
        None
    }

    /// A player right-clicked the bed it is sleeping in: get up.
    fn wake_from_bed(&mut self, _cx: &mut TickContext) {}

    /// Sets up a freshly spawned entity (not a loaded one): variants,
    /// colors and the like. `group_size` is how many of its spawn group
    /// came before it.
    fn finalize_spawn(&mut self, _context: &mut TickContext, _group_size: u32) {}

    /// `/kill`: true when it is gone at once; otherwise it dies its own way.
    fn kill(&mut self, _context: &mut TickContext) -> bool {
        true
    }

    /// A player-rules melee swing at the player or entity `victim`; false
    /// if it cannot strike or the victim is out of reach.
    fn melee_strike(&mut self, _victim: u128, _context: &mut TickContext) -> bool {
        false
    }

    /// A block changed nearby, which may spoil a path.
    fn block_changed(&mut self, _pos: BlockPos) {}

    /// Whether it never despawns.
    fn is_persistent(&self) -> bool {
        true
    }

    /// Whether it is marked to stay (persistence required, riding, a
    /// bucketed fish...); such mobs do not count towards natural spawn caps.
    fn persistence_required(&self) -> bool {
        false
    }

    /// Its experience orb behaviour, for experience orbs.
    fn orb(&self) -> Option<&dyn crate::world::experience::Orb> {
        None
    }

    fn orb_mut(&mut self) -> Option<&mut dyn crate::world::experience::Orb> {
        None
    }

    /// The lead on it, if any.
    fn leash(&self) -> Option<&crate::world::leash::Leash> {
        None
    }

    /// Its lead, for kinds that can wear one at all (mobs, boats); `None`
    /// for every other kind.
    fn leash_mut(&mut self) -> Option<&mut Option<crate::world::leash::Leash>> {
        None
    }

    /// How a lead tied to it now behaves; `None` when it cannot be leashed
    /// now.
    fn leash_profile(&self) -> Option<crate::world::leash::LeashProfile> {
        None
    }

    /// Whether it is saved with its chunk (a leash knot is not: its
    /// leashed entities tie it again).
    fn persists(&self) -> bool {
        true
    }

    /// How many players can ride it at once; zero for things nobody rides.
    fn seats(&self) -> usize {
        0
    }

    /// Whether a player may get on now.
    fn accepts_rider(&self) -> bool {
        true
    }

    /// Whether it flies where its driver steers it, climbing and diving
    /// freely (a harnessed happy ghast).
    fn flying_vehicle(&self) -> bool {
        false
    }

    /// Whether its first rider steers it, the rider's client moving it
    /// (a boat).
    fn driven_by_rider(&self) -> bool {
        false
    }

    /// Whether its first rider steers it with server-simulated movement
    /// fed by `steer_with_input` (a saddled, tamed mount). Any future mob
    /// or feature opts into being ridden this way the same generic way.
    fn rideable_by_input(&self) -> bool {
        false
    }

    /// Whether its first rider's held movement keys reach it each tick
    /// through `steer_with_input`: a server-steered mount, or a minecart
    /// a rider nudges along its rail.
    fn takes_rider_input(&self) -> bool {
        self.rideable_by_input()
    }

    /// Whether the driving rider currently controls it: either it moves
    /// itself from the rider's own reports (`driven_by_rider`), or it
    /// takes steering input from the rider each tick (`rideable_by_input`).
    /// A mount that answers only to a lure is not controlled while its
    /// rider holds none: the rider's client leaves it to the server then.
    fn controls_movement(&self) -> bool {
        self.driven_by_rider() || self.rideable_by_input()
    }

    /// One tick of steering input from the driving rider. Only called
    /// when `takes_rider_input` is true; latched until the next call, so
    /// the entity keeps moving the same way between input changes.
    fn steer_with_input(&mut self, _input: RiderInput) {}

    /// Where its driver's client may put it: a server-side run of the
    /// ride holds the reported position to what the ride allows.
    fn clamp_driven_move(&mut self, reported: [f64; 3]) -> [f64; 3] {
        reported
    }

    /// The top speed its driving rider can give it, when it moves the way
    /// a ridden mob does.
    fn ridden_speed(&self) -> Option<RiddenSpeed> {
        None
    }

    /// Called right after a player successfully mounts it, so any
    /// steering input latched from a previous rider can't move it before
    /// the new one sends its own.
    fn on_mounted(&mut self) {}

    /// Whether a plain click holding `stack` is not meant to mount it
    /// (feeding a pig).
    fn mount_refused_by(&self, _stack: Option<&ItemStack>) -> bool {
        false
    }

    /// A rider used an item that spurs the mount on (a carrot or warped
    /// fungus on a stick); `roll` is a random number for the new boost's
    /// length. True when it took effect and the item should wear.
    fn boost(&mut self, _roll: u32) -> bool {
        false
    }

    /// Where the rider in seat `index` of `riders` sits, from its position.
    fn seat(&self, index: usize, _riders: usize) -> [f64; 3] {
        let base = self.base();
        let seats = base.kind.passenger_attachments();
        turned(seats[index.min(seats.len() - 1)], base.yaw)
    }

    /// The point of it that sits on a seat, from its position.
    fn vehicle_attachment(&self) -> [f64; 3] {
        let base = self.base();
        turned(base.kind.vehicle_attachment(), base.yaw)
    }

    /// Where a rider of `width` facing `yaw` gets off.
    fn dismount_location(
        &self,
        _width: f64,
        _yaw: f32,
        _block: &dyn Fn(BlockPos) -> Option<BlockState>,
    ) -> [f64; 3] {
        let [x, _, z] = self.base().position;
        [x, self.bounds()[4], z]
    }

    /// The driver's paddle or steering input, shown to viewers.
    fn steer(&mut self, _left: bool, _right: bool) {}
}

impl dyn RegionEntity {
    pub fn downcast_ref<T: RegionEntity>(&self) -> Option<&T> {
        (self as &dyn Any).downcast_ref()
    }

    pub fn downcast_mut<T: RegionEntity>(&mut self) -> Option<&mut T> {
        (self as &mut dyn Any).downcast_mut()
    }
}

/// The whole saved form: id, common fields, the type's fields, unknown
/// fields as loaded, then riders.
pub fn save(entity: &dyn RegionEntity) -> Compound {
    let base = entity.base();
    let mut out = Compound::new().with("id", entity.saved_id());
    base.save(&mut out);
    entity.save(&mut out);
    for (key, value) in base.extra.iter() {
        out.insert(key, value.clone());
    }
    let passengers = saved_passengers(base);
    if !passengers.is_empty() {
        out.insert("Passengers", passengers);
    }
    out
}

/// `Passengers` as saved: unrecognized nested entities kept verbatim, plus
/// a marker per rider (online now or left mounted) so a restart or a chunk
/// reload does not lose the mount -- see [`EntityBase::offline_riders`].
fn saved_passengers(base: &EntityBase) -> Vec<Compound> {
    let mut out = base.passengers.clone();
    let mut seen: Vec<u128> = Vec::new();
    for uuid in base
        .riders
        .iter()
        .map(|rider| rider.uuid)
        .chain(base.offline_riders.iter().copied())
    {
        if seen.contains(&uuid) {
            continue;
        }
        seen.push(uuid);
        out.push(
            Compound::new()
                .with("id", "minecraft:player")
                .with("UUID", Tag::uuid(uuid)),
        );
    }
    out
}

/// What [`base_tick`] did that the region shows or acts on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BaseTick {
    /// Fire destroyed it.
    pub destroyed: bool,
    /// Lava hurt a thing that is not alive, with the burn sound (living
    /// things play it themselves when the hurt lands).
    pub burned: bool,
    /// Rain or water put out its fire, with the extinguish sound.
    pub extinguished: bool,
    /// A burning thing inside this water or powder snow cauldron put its
    /// fire out and the cauldron loses a level.
    pub lower_cauldron: Option<BlockPos>,
}

/// The part of [`EntityBase`] the game ticks for every entity: portal
/// cooldown, burning, lava and fire blocks, and water or rain (`in_rain`)
/// putting the fire out. `changed` is set when its entity data changed.
pub fn base_tick(
    entity: &mut dyn RegionEntity,
    changed: &mut bool,
    in_rain: bool,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> BaseTick {
    let fire_immune = entity.base().kind.size().fire_immune;
    let base = entity.base_mut();
    if base.portal_cooldown > 0 {
        base.portal_cooldown -= 1;
    }
    let burning = base.on_fire();
    let in_lava = crate::world::contact::box_touches(
        base.bounds(),
        LAVA_CONTACT_INSET,
        tuff_vanilla_data::tag::fluid::key::LAVA,
        block,
    );
    let mut outcome = BaseTick::default();
    if base.fire > 0 {
        if fire_immune {
            base.fire = 0;
        } else {
            if base.fire % FIRE_DAMAGE_PERIOD == 0 && !in_lava && !base.invulnerable {
                outcome.destroyed = entity.hurt(1.0, &tuff_vanilla_data::damage_type::ON_FIRE);
            }
            entity.base_mut().fire -= 1;
        }
    }
    let cauldron = crate::world::block_contact::cauldron_inside(entity.base().bounds(), block);
    if let Some(pos) = cauldron.extinguishes
        && entity.base().fire > 0
    {
        outcome.lower_cauldron = Some(pos);
        entity.base_mut().fire = 0;
    }
    let burning_before_contact = entity.base().fire > 0;
    if cauldron.lava && !fire_immune {
        let base = entity.base_mut();
        base.ticks_frozen = 0;
        base.fire = base.fire.max(LAVA_FIRE_TICKS);
        if !base.invulnerable {
            let lava = &tuff_vanilla_data::damage_type::LAVA;
            outcome.burned |= entity.living().is_none() && !entity.resists(lava);
            outcome.destroyed |= entity.hurt(LAVA_DAMAGE, lava);
        }
    }
    if in_lava && !fire_immune {
        let base = entity.base_mut();
        base.fire = base.fire.max(LAVA_FIRE_TICKS);
        if !base.invulnerable {
            let lava = &tuff_vanilla_data::damage_type::LAVA;
            outcome.burned = entity.living().is_none() && !entity.resists(lava);
            outcome.destroyed |= entity.hurt(LAVA_DAMAGE, lava);
        }
    }
    if !fire_immune
        && !outcome.destroyed
        && let Some(damage) = crate::world::contact::fire_contact(entity.base().bounds(), block)
    {
        let base = entity.base_mut();
        if base.fire < 0 {
            base.fire += 1;
        }
        if base.fire >= 0 {
            base.fire = base.fire.max(IN_FIRE_TICKS);
        }
        if !base.invulnerable {
            outcome.destroyed |= entity.hurt(damage, &tuff_vanilla_data::damage_type::IN_FIRE);
        }
    }
    let in_water = || {
        crate::world::contact::box_touches(
            entity.base().bounds(),
            LAVA_CONTACT_INSET,
            tuff_vanilla_data::tag::fluid::key::WATER,
            block,
        )
    };
    if in_rain || (entity.base().fire > 0 && in_water()) {
        let base = entity.base_mut();
        base.fire = base.fire.min(0);
        outcome.extinguished = burning_before_contact && base.fire <= 0;
    }
    *changed |= entity.base().on_fire() != burning;
    outcome
}

mod summaries;
pub use summaries::*;

#[cfg(test)]
mod tests;

/// The entity tick's parallel batch size: `TUFF_ENTITY_COMPUTE_MIN_BATCH`,
/// else 64.
pub fn compute_min_batch() -> usize {
    std::env::var("TUFF_ENTITY_COMPUTE_MIN_BATCH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64)
}
