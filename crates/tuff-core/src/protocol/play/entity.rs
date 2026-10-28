use tuff_vanilla_data::item::ModifierOperation;
use tuff_vanilla_data::packet::play::Clientbound as Id;
use tuff_vanilla_data::registry::Attribute;

use crate::protocol::codec::WriteExt;
use crate::protocol::{Encode, Packet};
use tuff_item::item::{ItemSnapshot, SwingKind};
use tuff_item::text::TextComponent;
use tuff_wire::PacketWriter;
use tuff_world_model::BlockPos;

/// One attribute modifier of an [`UpdateAttributes`] property: a resource
/// location id, an amount and an operation (0 add, 1 add a percent of the
/// current value, 2 multiply by a percent).
pub struct AttributeModifierData<'a> {
    pub id: &'a str,
    pub amount: f64,
    pub operation: ModifierOperation,
}

/// One entry of an [`UpdateAttributes`] packet: the attribute's base value
/// plus the modifiers on it (the client computes the final value itself).
pub struct AttributeProperty<'a> {
    pub attribute: Attribute,
    pub base: f64,
    pub modifiers: &'a [AttributeModifierData<'a>],
}

/// Sets attribute base values and modifiers on an entity. The server always
/// sends the fully computed value as `base` with no modifiers, since it is
/// already authoritative for every attribute a player has.
pub struct UpdateAttributes<'a> {
    pub entity_id: i32,
    pub properties: &'a [AttributeProperty<'a>],
}

impl Encode for UpdateAttributes<'_> {
    const ID: i32 = Id::UpdateAttributes.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        w.write_var_int(self.properties.len() as i32);
        for property in self.properties {
            w.write_var_int(property.attribute.id() as i32);
            w.write_double(property.base);
            w.write_var_int(property.modifiers.len() as i32);
            for modifier in property.modifiers {
                w.write_string(modifier.id);
                w.write_double(modifier.amount);
                w.write_unsigned_byte(modifier.operation.network_id() as u8);
            }
        }
    }
}

pub struct AddEntity {
    pub entity_id: i32,
    pub uuid: u128,
    pub kind: i32,
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub pitch: f32,
    pub yaw: f32,
    pub head_yaw: f32,
    pub data: i32,
}

impl Encode for AddEntity {
    const ID: i32 = Id::AddEntity.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        w.write_uuid(self.uuid);
        w.write_var_int(self.kind);
        for c in self.position {
            w.write_double(c);
        }
        w.write_lp_vec3(self.velocity);
        w.write_angle(self.pitch);
        w.write_angle(self.yaw);
        w.write_angle(self.head_yaw);
        w.write_var_int(self.data);
    }
}

pub struct RemoveEntities<'a>(pub &'a [i32]);

impl Encode for RemoveEntities<'_> {
    const ID: i32 = Id::RemoveEntities.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.0.len() as i32);
        for id in self.0 {
            w.write_var_int(*id);
        }
    }
}

/// One step of a minecart along its track, as clients replay it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackStep {
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    /// Its share of the replay time; 0 places the cart at once.
    pub weight: f32,
}

/// Move Minecart Along Track: the steps a minecart took since its last
/// update, which clients replay over the next three ticks (worlds with
/// the minecart improvements feature).
pub struct MoveMinecartAlongTrack<'a> {
    pub entity_id: i32,
    pub steps: &'a [TrackStep],
}

impl Encode for MoveMinecartAlongTrack<'_> {
    const ID: i32 = Id::MoveMinecartAlongTrack.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        w.write_var_int(self.steps.len() as i32);
        for step in self.steps {
            for c in step.position.into_iter().chain(step.velocity) {
                w.write_double(c);
            }
            w.write_byte(crate::world::movement_sync::pack_degrees(step.yaw));
            w.write_byte(crate::world::movement_sync::pack_degrees(step.pitch));
            w.write_float(step.weight);
        }
    }
}

/// Teleport Entity (`entity_position_sync`) with a linear path.
pub struct EntityPositionSync {
    pub entity_id: i32,
    pub position: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

impl Encode for EntityPositionSync {
    const ID: i32 = Id::EntityPositionSync.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        w.write_var_int(0);
        for c in self.position {
            w.write_double(c);
        }
        w.write_float(self.yaw);
        w.write_float(self.pitch);
        w.write_bool(self.on_ground);
    }
}

/// One stop on a stepped position path, `tick_offset` ticks after the last.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PositionStep {
    pub position: [f64; 3],
    pub tick_offset: i32,
}

/// Entity Position Sync along a stepped path: the client replays the stops
/// instead of gliding straight to the last one.
pub struct EntityPositionSyncStepped<'a> {
    pub entity_id: i32,
    pub steps: &'a [PositionStep],
    pub yaw: f32,
    pub pitch: f32,
    pub on_ground: bool,
}

impl Encode for EntityPositionSyncStepped<'_> {
    const ID: i32 = Id::EntityPositionSync.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        w.write_var_int(1);
        w.write_var_int(self.steps.len() as i32);
        for step in self.steps {
            for c in step.position {
                w.write_double(c);
            }
            w.write_var_int(step.tick_offset);
        }
        w.write_float(self.yaw);
        w.write_float(self.pitch);
        w.write_bool(self.on_ground);
    }
}

pub mod relative {
    pub const X: u32 = 1;
    pub const Y: u32 = 1 << 1;
    pub const Z: u32 = 1 << 2;
    pub const Y_ROT: u32 = 1 << 3;
    pub const X_ROT: u32 = 1 << 4;
    pub const DELTA_X: u32 = 1 << 5;
    pub const DELTA_Y: u32 = 1 << 6;
    pub const DELTA_Z: u32 = 1 << 7;
    pub const ROTATE_DELTA: u32 = 1 << 8;
}

/// Teleport Entity: a move whose components may each be relative to the
/// client's current state, per the [`relative`] flags.
pub struct TeleportEntity {
    pub entity_id: i32,
    pub position: [f64; 3],
    pub velocity: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub relative: u32,
    pub on_ground: bool,
}

impl Encode for TeleportEntity {
    const ID: i32 = Id::TeleportEntity.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        for c in self.position.into_iter().chain(self.velocity) {
            w.write_double(c);
        }
        w.write_float(self.yaw);
        w.write_float(self.pitch);
        w.write_int(self.relative as i32);
        w.write_bool(self.on_ground);
    }
}

/// Move Entity Position (`move_entity_pos`): a linear move by `delta`
/// 4096ths of a block from the position the client last got.
pub struct MoveEntityPos {
    pub entity_id: i32,
    pub delta: [i16; 3],
    pub on_ground: bool,
}

impl Encode for MoveEntityPos {
    const ID: i32 = Id::MoveEntityPos.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        w.write_var_int(i32::from(self.on_ground));
        for d in self.delta {
            w.write_short(d);
        }
    }
}

/// Move Entity Position and Rotation (`move_entity_pos_rot`), with packed
/// angles.
pub struct MoveEntityPosRot {
    pub entity_id: i32,
    pub delta: [i16; 3],
    pub yaw: i8,
    pub pitch: i8,
    pub on_ground: bool,
}

impl Encode for MoveEntityPosRot {
    const ID: i32 = Id::MoveEntityPosRot.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        w.write_var_int(i32::from(self.on_ground));
        for d in self.delta {
            w.write_short(d);
        }
        w.write_byte(self.yaw);
        w.write_byte(self.pitch);
    }
}

/// Move Entity Rotation (`move_entity_rot`), with packed angles.
pub struct MoveEntityRot {
    pub entity_id: i32,
    pub yaw: i8,
    pub pitch: i8,
    pub on_ground: bool,
}

impl Encode for MoveEntityRot {
    const ID: i32 = Id::MoveEntityRot.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        w.write_bool(self.on_ground);
        w.write_byte(self.yaw);
        w.write_byte(self.pitch);
    }
}

#[derive(Packet)]
#[packet(id = Id::RotateHead, encode_only)]
pub struct RotateHead {
    #[packet(var_int)]
    pub entity_id: i32,
    #[packet(angle)]
    pub head_yaw: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    Byte(i8),
    VarInt(i32),
    /// 0 for absent, 1 + the value otherwise (an entity id).
    OptionalVarInt(Option<i32>),
    VarLong(i64),
    Float(f32),
    Slot(Option<ItemSnapshot>),
    OptionalPosition(Option<BlockPos>),
    Position(BlockPos),
    /// A dimension key and a position, or none.
    OptionalGlobalPosition(Option<(String, BlockPos)>),
    /// An owner or other tracked entity, by UUID (resolved client-side to
    /// whichever tracked entity currently has it).
    OptionalUuid(Option<u128>),
    /// A block state ID.
    BlockState(u32),
    /// A block state ID, or none (written as air).
    OptionalBlockState(Option<u32>),
    Pose(i32),
    /// 0 for left, 1 for right.
    HumanoidArm(i32),
    /// A direction by its protocol id (down, up, north, south, west, east).
    Direction(i32),
    /// Pitch, yaw and roll in degrees.
    Rotations([f32; 3]),
    Vector3([f32; 3]),
    /// x, y, z, w.
    Quaternion([f32; 4]),
    ResolvableProfile(Box<ResolvableProfile>),
    OptionalText(Option<TextComponent>),
    String(String),
    Text(TextComponent),
    Boolean(bool),
    /// A registry holder (a variant), written as its id under the
    /// serializer the type's data layout names.
    Holder {
        serializer: u8,
        id: i32,
    },
    /// A particle type id followed by its options, already encoded.
    Particle {
        id: i32,
        options: Vec<u8>,
    },
    /// A list of particles, each a type id and its encoded options.
    Particles(Vec<(i32, Vec<u8>)>),
    /// A villager's type, profession and level, under the serializer the
    /// type's data layout names.
    VillagerData {
        serializer: u8,
        kind: i32,
        profession: i32,
        level: i32,
    },
}

impl MetadataValue {
    pub fn type_id(&self) -> i32 {
        use tuff_vanilla_data::entity::data_serializer as id;
        i32::from(match self {
            Self::Byte(_) => id::BYTE,
            Self::VarInt(_) => id::INT,
            Self::OptionalVarInt(_) => id::OPTIONAL_UNSIGNED_INT,
            Self::VarLong(_) => id::LONG,
            Self::Float(_) => id::FLOAT,
            Self::OptionalText(_) => id::OPTIONAL_COMPONENT,
            Self::String(_) => id::STRING,
            Self::Text(_) => id::COMPONENT,
            Self::Slot(_) => id::ITEM_STACK,
            Self::Boolean(_) => id::BOOLEAN,
            Self::OptionalPosition(_) => id::OPTIONAL_BLOCK_POS,
            Self::Position(_) => id::BLOCK_POS,
            Self::OptionalGlobalPosition(_) => id::OPTIONAL_GLOBAL_POS,
            Self::OptionalUuid(_) => id::OPTIONAL_LIVING_ENTITY_REFERENCE,
            Self::BlockState(_) => id::BLOCK_STATE,
            Self::OptionalBlockState(_) => id::OPTIONAL_BLOCK_STATE,
            Self::Pose(_) => id::POSE,
            Self::HumanoidArm(_) => id::HUMANOID_ARM,
            Self::Direction(_) => id::DIRECTION,
            Self::Rotations(_) => id::ROTATIONS,
            Self::Vector3(_) => id::VECTOR3,
            Self::Quaternion(_) => id::QUATERNION,
            Self::ResolvableProfile(_) => id::RESOLVABLE_PROFILE,
            Self::Particle { .. } => id::PARTICLE,
            Self::Particles(_) => id::PARTICLES,
            Self::Holder { serializer, .. } | Self::VillagerData { serializer, .. } => *serializer,
        })
    }
}

/// A player profile that clients may complete themselves: any of name,
/// UUID and properties, plus optional skin overrides.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvableProfile {
    pub name: Option<String>,
    pub id: Option<u128>,
    pub properties: Vec<crate::auth::ProfileProperty>,
    pub texture: Option<String>,
    pub cape: Option<String>,
    pub elytra: Option<String>,
    /// 0 wide, 1 slim.
    pub model: Option<i32>,
}

impl ResolvableProfile {
    /// Reads the saved form: a bare name, or a compound with `name`, `id`,
    /// `properties` and the override keys. Anything malformed is `None`.
    pub fn from_nbt(tag: &tuff_nbt::Tag) -> Option<Self> {
        use tuff_nbt::Tag;
        if let Some(name) = tag.as_str() {
            return Self::valid_name(name).then(|| Self {
                name: Some(name.to_owned()),
                ..Self::default()
            });
        }
        let nbt = tag.as_compound()?;
        let text = |key: &str| match nbt.get(key) {
            None => Some(None),
            Some(Tag::String(v)) => Some(Some(v.clone())),
            Some(_) => None,
        };
        let name = text("name")?;
        if name.as_deref().is_some_and(|n| !Self::valid_name(n)) {
            return None;
        }
        let id = match nbt.get("id") {
            None => None,
            Some(tag) => Some(tag.as_uuid()?),
        };
        let mut properties = Vec::new();
        for entry in nbt
            .get("properties")
            .map_or(&[][..], |t| t.as_list().unwrap_or(&[]))
        {
            let entry = entry.as_compound()?;
            properties.push(crate::auth::ProfileProperty {
                name: entry.get_str("name")?.to_owned(),
                value: entry.get_str("value")?.to_owned(),
                signature: entry.get_str("signature").map(str::to_owned),
            });
        }
        let model = match text("model")?.as_deref() {
            None => None,
            Some("wide") => Some(0),
            Some("slim") => Some(1),
            Some(_) => return None,
        };
        Some(Self {
            name,
            id,
            properties,
            texture: text("texture")?,
            cape: text("cape")?,
            elytra: text("elytra")?,
            model,
        })
    }

    fn valid_name(name: &str) -> bool {
        name.len() <= 16 && name.chars().all(|c| c > ' ' && c < '\u{7f}')
    }

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(0);
        w.write_bool(self.name.is_some());
        if let Some(name) = &self.name {
            w.write_string(name);
        }
        w.write_bool(self.id.is_some());
        if let Some(id) = self.id {
            w.write_uuid(id);
        }
        w.write_var_int(self.properties.len() as i32);
        for property in &self.properties {
            w.write_string(&property.name);
            w.write_string(&property.value);
            w.write_bool(property.signature.is_some());
            if let Some(signature) = &property.signature {
                w.write_string(signature);
            }
        }
        for texture in [&self.texture, &self.cape, &self.elytra] {
            w.write_bool(texture.is_some());
            if let Some(texture) = texture {
                w.write_string(texture);
            }
        }
        w.write_bool(self.model.is_some());
        if let Some(model) = self.model {
            w.write_var_int(model);
        }
    }
}

pub mod metadata {
    pub const ENTITY_FLAGS: u8 = 0;
    /// Ticks of breath left under water.
    pub const AIR_SUPPLY: u8 = 1;
    pub const CUSTOM_NAME: u8 = 2;
    pub const CUSTOM_NAME_VISIBLE: u8 = 3;
    pub const SILENT: u8 = 4;
    pub const NO_GRAVITY: u8 = 5;
    pub const POSE: u8 = 6;
    /// Ticks spent freezing in powder snow.
    pub const TICKS_FROZEN: u8 = 7;
    pub const ITEM_STACK: u8 = 8;
    /// An experience orb's value.
    pub const EXPERIENCE_VALUE: u8 = 8;
    /// Living entity hand states: an item in use and which hand holds it.
    pub const LIVING_FLAGS: u8 = 8;
    pub const HEALTH: u8 = 9;
    /// Living entities: the particles of their visible effects.
    pub const EFFECT_PARTICLES: u8 = 10;
    /// Living entities: whether every visible effect is ambient.
    pub const EFFECT_AMBIENCE: u8 = 11;
    pub const MAIN_ARM: u8 = 15;
    pub const SKIN_PARTS: u8 = 16;
    /// Living entities: the bed they sleep in.
    /// Arrows stuck in a living entity.
    pub const ARROW_COUNT: u8 = 12;
    pub const SLEEPING_POS: u8 = 14;
    /// Player absorption hearts.
    pub const ABSORPTION: u8 = 17;
    /// Player score, shown on the death screen.
    pub const SCORE: u8 = 18;
    /// Player: the variant of the parrot on each shoulder.
    pub const SHOULDER_LEFT: u8 = 19;
    pub const SHOULDER_RIGHT: u8 = 20;
    /// Primed TNT: ticks left on the fuse.
    pub const TNT_FUSE: u8 = 8;
    /// Primed TNT: the block state it shows.
    pub const TNT_BLOCK_STATE: u8 = 9;

    pub const FLAG_ON_FIRE: i8 = 0x01;
    pub const FLAG_SNEAKING: i8 = 0x02;
    pub const FLAG_SPRINTING: i8 = 0x08;
    pub const FLAG_SWIMMING: i8 = 0x10;
    pub const FLAG_INVISIBLE: i8 = 0x20;
    pub const FLAG_GLOWING: i8 = 0x40;
    /// Gliding with an elytra.
    pub const FLAG_FALL_FLYING: i8 = -0x80;
    pub const LIVING_FLAG_HAND_ACTIVE: i8 = 0x01;
    pub const LIVING_FLAG_OFF_HAND: i8 = 0x02;
    pub const LIVING_FLAG_SPIN_ATTACK: i8 = 0x04;
}

pub struct SetEntityData<'a> {
    pub entity_id: i32,
    pub values: &'a [(u8, MetadataValue)],
}

impl Encode for SetEntityData<'_> {
    const ID: i32 = Id::SetEntityData.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        for (index, value) in self.values {
            w.write_unsigned_byte(*index);
            w.write_var_int(value.type_id());
            match value {
                MetadataValue::Byte(v) => w.write_byte(*v),
                MetadataValue::VarInt(v) => w.write_var_int(*v),
                MetadataValue::OptionalVarInt(v) => w.write_var_int(v.map_or(0, |v| v + 1)),
                MetadataValue::VarLong(v) => w.write_var_long(*v),
                MetadataValue::Float(v) => w.write_float(*v),
                MetadataValue::Slot(v) => w.write_viewer_slot(v.as_ref().map(ItemSnapshot::view)),
                MetadataValue::Pose(v)
                | MetadataValue::HumanoidArm(v)
                | MetadataValue::Direction(v) => w.write_var_int(*v),
                MetadataValue::Rotations(angles) => {
                    for angle in angles {
                        w.write_float(*angle);
                    }
                }
                MetadataValue::Vector3(v) => v.iter().for_each(|c| w.write_float(*c)),
                MetadataValue::Quaternion(q) => q.iter().for_each(|c| w.write_float(*c)),
                MetadataValue::ResolvableProfile(profile) => profile.encode(w),
                MetadataValue::Holder { id, .. } => w.write_var_int(*id),
                MetadataValue::VillagerData {
                    kind,
                    profession,
                    level,
                    ..
                } => {
                    w.write_var_int(*kind);
                    w.write_var_int(*profession);
                    w.write_var_int(*level);
                }
                MetadataValue::OptionalPosition(v) => {
                    w.write_bool(v.is_some());
                    if let Some(pos) = v {
                        w.write_position(*pos);
                    }
                }
                MetadataValue::Position(pos) => w.write_position(*pos),
                MetadataValue::OptionalGlobalPosition(v) => {
                    w.write_bool(v.is_some());
                    if let Some((dimension, pos)) = v {
                        w.write_string(dimension);
                        w.write_position(*pos);
                    }
                }
                MetadataValue::OptionalUuid(v) => {
                    w.write_bool(v.is_some());
                    if let Some(uuid) = v {
                        w.write_uuid(*uuid);
                    }
                }
                MetadataValue::BlockState(v) => w.write_var_int(*v as i32),
                MetadataValue::OptionalBlockState(v) => w.write_var_int(v.unwrap_or(0) as i32),
                MetadataValue::OptionalText(v) => {
                    w.write_bool(v.is_some());
                    if let Some(text) = v {
                        w.write_text(text);
                    }
                }
                MetadataValue::String(v) => w.write_string(v),
                MetadataValue::Text(v) => w.write_text(v),
                MetadataValue::Boolean(v) => w.write_bool(*v),
                MetadataValue::Particle { id, options } => {
                    w.write_var_int(*id);
                    w.write_bytes(options);
                }
                MetadataValue::Particles(particles) => {
                    w.write_var_int(particles.len() as i32);
                    for (id, options) in particles {
                        w.write_var_int(*id);
                        w.write_bytes(options);
                    }
                }
            }
        }
        w.write_unsigned_byte(0xFF);
    }
}

pub mod equipment_slot {
    pub const MAIN_HAND: u8 = 0;
    pub const OFF_HAND: u8 = 1;
    pub const FEET: u8 = 2;
    pub const LEGS: u8 = 3;
    pub const CHEST: u8 = 4;
    pub const HEAD: u8 = 5;
    pub const BODY: u8 = 6;
    pub const SADDLE: u8 = 7;
}

pub struct SetEquipment<'a> {
    pub entity_id: i32,
    pub items: &'a [(u8, Option<tuff_item::item::AnyItem<'a>>)],
}

impl Encode for SetEquipment<'_> {
    const ID: i32 = Id::SetEquipment.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        for (i, (slot, item)) in self.items.iter().enumerate() {
            let more = if i + 1 < self.items.len() { 0x80 } else { 0 };
            w.write_unsigned_byte(slot | more);
            w.write_viewer_any_slot(*item);
        }
    }
}

pub struct DamageEvent {
    pub entity_id: i32,
    pub damage_type: i32,
    pub cause: Option<i32>,
    pub direct: Option<i32>,
    pub source_position: Option<[f64; 3]>,
}

impl Encode for DamageEvent {
    const ID: i32 = Id::DamageEvent.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        w.write_var_int(self.damage_type);
        w.write_var_int(self.cause.map_or(0, |id| id + 1));
        w.write_var_int(self.direct.map_or(0, |id| id + 1));
        w.write_bool(self.source_position.is_some());
        for c in self.source_position.into_iter().flatten() {
            w.write_double(c);
        }
    }
}

pub mod entity_event {
    pub const DEATH: i8 = 3;
    pub const TOTEM_OF_UNDYING: i8 = 35;
    /// Sent to the user alone: finishes the client's own item use, which
    /// otherwise keeps animating and repeating the use while held.
    pub const USE_ITEM_COMPLETE: i8 = 9;
    /// Permission level 0 (all players); levels 1-4 follow consecutively.
    pub const PERMISSION_LEVEL_ALL: i8 = 24;
    pub const REDUCED_DEBUG_INFO: i8 = 22;
    pub const NO_REDUCED_DEBUG_INFO: i8 = 23;
    /// Portal particles: a chorus fruit or enderman teleport.
    pub const TELEPORT: i8 = 46;
    /// A silverfish leaving (or entering) an infested block.
    pub const SILVERFISH_MERGE_WITH_STONE: i8 = 20;
    /// The held item broke: clients play the break sound and particles for
    /// the item they last saw in that hand.
    pub const MAIN_HAND_BREAK: i8 = 47;
    pub const OFF_HAND_BREAK: i8 = 48;
    pub const HEAD_BREAK: i8 = 49;
    pub const CHEST_BREAK: i8 = 50;
    pub const LEGS_BREAK: i8 = 51;
    pub const FEET_BREAK: i8 = 52;
    /// Bubbles around a living entity that just drowned a little.
    pub const DROWN_PARTICLES: i8 = 67;
    /// A boat atop a bubble column is pulled under (down) or tossed up.
    pub const BOAT_BUBBLE_DOWN: i8 = 71;
    pub const BOAT_BUBBLE_UP: i8 = 72;
}

#[derive(Packet)]
#[packet(id = Id::EntityEvent, encode_only)]
pub struct EntityEvent {
    #[packet(int)]
    pub entity_id: i32,
    pub status: i8,
}

/// Award Statistics: `(statistic type id, entry id, value)` triples.
pub struct AwardStats<'a>(pub &'a [(u32, u32, i32)]);

impl Encode for AwardStats<'_> {
    const ID: i32 = Id::AwardStats.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.0.len() as i32);
        for (kind, id, value) in self.0 {
            w.write_var_int(*kind as i32);
            w.write_var_int(*id as i32);
            w.write_var_int(*value);
        }
    }
}

/// Set Passengers: who rides a vehicle.
pub struct SetPassengers<'a> {
    pub vehicle: i32,
    pub passengers: &'a [i32],
}

impl Encode for SetPassengers<'_> {
    const ID: i32 = Id::SetPassengers.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.vehicle);
        w.write_var_int(self.passengers.len() as i32);
        for passenger in self.passengers {
            w.write_var_int(*passenger);
        }
    }
}

/// Set Entity Link: a lead from `holder` to `attached`; holder 0 unties it.
pub struct SetEntityLink {
    pub attached: i32,
    pub holder: i32,
}

impl Encode for SetEntityLink {
    const ID: i32 = Id::SetEntityLink.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_int(self.attached);
        w.write_int(self.holder);
    }
}

/// Move Vehicle: puts the vehicle the client drives back where the server
/// has it.
pub struct MoveVehicle {
    pub position: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
}

impl Encode for MoveVehicle {
    const ID: i32 = Id::MoveVehicle.id();

    fn encode(&self, w: &mut PacketWriter) {
        for c in self.position {
            w.write_double(c);
        }
        w.write_float(self.yaw);
        w.write_float(self.pitch);
    }
}

#[derive(Packet)]
#[packet(id = Id::SetEntityMotion, encode_only)]
pub struct SetEntityMotion {
    #[packet(var_int)]
    pub entity_id: i32,
    #[packet(lp_vec3)]
    pub velocity: [f64; 3],
}

#[derive(Packet)]
#[packet(id = Id::PlayerCombatKill, encode_only)]
pub struct PlayerCombatKill<'a> {
    #[packet(var_int)]
    pub player_id: i32,
    pub message: &'a TextComponent,
}

#[derive(Packet)]
#[packet(id = Id::PlayerCombatEnter, encode_only)]
pub struct PlayerCombatEnter;

#[derive(Packet)]
#[packet(id = Id::LowDiskSpaceWarning, encode_only)]
pub struct LowDiskSpaceWarning;

#[derive(Packet)]
#[packet(id = Id::PlayerCombatEnd, encode_only)]
pub struct PlayerCombatEnd {
    #[packet(var_int)]
    pub duration: i32,
}

pub mod animation {
    pub const WAKE_UP: u8 = 0;
    pub const CRITICAL_HIT: u8 = 1;
    pub const MAGIC_CRITICAL_HIT: u8 = 2;
}

#[derive(Packet)]
#[packet(id = Id::Animate, encode_only)]
pub struct Animate {
    #[packet(var_int)]
    pub entity_id: i32,
    pub animation: u8,
}

pub struct SwingAnimation {
    pub entity_id: i32,
    pub hand: i32,
    pub kind: SwingKind,
    pub duration_ticks: i32,
}

impl SwingAnimation {
    pub const DEFAULT_DURATION_TICKS: i32 = tuff_item::item::DEFAULT_SWING_TICKS;

    pub fn whack(entity_id: i32, hand: i32) -> Self {
        Self {
            entity_id,
            hand,
            kind: SwingKind::Whack,
            duration_ticks: Self::DEFAULT_DURATION_TICKS,
        }
    }
}

impl Encode for SwingAnimation {
    const ID: i32 = Id::SwingAnimation.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        w.write_var_int(self.hand);
        w.write_var_int(self.kind as i32);
        w.write_var_int(self.duration_ticks);
    }
}

#[cfg(test)]
mod swing_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn swing_encodes_kind_and_duration() {
        let mut w = PacketWriter::new();
        SwingAnimation {
            entity_id: 1,
            hand: 0,
            kind: SwingKind::Stab,
            duration_ticks: 9,
        }
        .encode(&mut w);
        assert!(w.into_bytes().ends_with(&[1, 0, 2, 9]));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatCompletionAction {
    Add = 0,
    Remove = 1,
    Set = 2,
}

/// Adds, removes or replaces the extra names the client offers when
/// completing chat.
pub struct CustomChatCompletions<'a> {
    pub action: ChatCompletionAction,
    pub entries: &'a [&'a str],
}

impl Encode for CustomChatCompletions<'_> {
    const ID: i32 = Id::CustomChatCompletions.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.action as i32);
        w.write_var_int(self.entries.len() as i32);
        for entry in self.entries {
            w.write_string(entry);
        }
    }
}

#[cfg(test)]
mod chat_completion_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn completions_encode_action_then_prefixed_entries() {
        let mut w = PacketWriter::new();
        CustomChatCompletions {
            action: ChatCompletionAction::Set,
            entries: &["ab", "c"],
        }
        .encode(&mut w);
        assert_eq!(&w.into_bytes()[..], &[2, 2, 2, b'a', b'b', 1, b'c']);
    }
}

#[derive(Packet)]
#[packet(id = Id::TakeItemEntity, encode_only)]
pub struct TakeItemEntity {
    #[packet(var_int)]
    pub collected: i32,
    #[packet(var_int)]
    pub collector: i32,
    #[packet(var_int)]
    pub count: i32,
}

#[derive(Packet)]
#[packet(id = Id::SetCamera, encode_only)]
pub struct SetCamera {
    #[packet(var_int)]
    pub entity_id: i32,
}

/// The post-processing effects the client applies, by id.
pub struct PostEffects<'a> {
    pub effects: &'a [String],
}

impl Encode for PostEffects<'_> {
    const ID: i32 = Id::PostEffects.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.effects.len() as i32);
        for effect in self.effects {
            w.write_string(effect);
        }
    }
}

/// Camera tilt for the hurt player; `yaw` is the damage direction relative to
/// the player's facing, in degrees.
pub struct HurtAnimation {
    pub entity_id: i32,
    pub yaw: f32,
}

impl Encode for HurtAnimation {
    const ID: i32 = Id::HurtAnimation.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entity_id);
        w.write_float(self.yaw);
    }
}

/// Reply to a block entity or entity tag query.
pub struct TagQuery<'a> {
    pub transaction_id: i32,
    pub tag: Option<&'a tuff_nbt::Tag>,
}

impl Encode for TagQuery<'_> {
    const ID: i32 = Id::TagQuery.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.transaction_id);
        match self.tag {
            Some(tag) => {
                let _ = w.write_nbt(tag);
            }
            None => w.write_unsigned_byte(0),
        }
    }
}

/// Opens or closes a group of packets the client applies together.
#[derive(Packet)]
#[packet(id = Id::StartConfiguration, encode_only)]
pub struct StartConfiguration;

#[derive(Packet)]
#[packet(id = Id::ProjectilePower, encode_only)]
pub struct ProjectilePower {
    #[packet(var_int)]
    pub entity_id: i32,
    pub acceleration_power: f64,
}

#[derive(Packet)]
#[packet(id = Id::BundleDelimiter, encode_only)]
pub struct BundleDelimiter;

#[cfg(test)]
mod stepped_sync_tests {
    use super::*;
    use crate::protocol::encode_packet;

    #[tuff_core::watchdog]
    #[test]
    fn a_stepped_sync_writes_type_one_then_the_steps() {
        let bytes = encode_packet(&EntityPositionSyncStepped {
            entity_id: 5,
            steps: &[PositionStep {
                position: [1.0, 2.0, 3.0],
                tick_offset: 4,
            }],
            yaw: 0.0,
            pitch: 0.0,
            on_ground: true,
        })
        .into_bytes();
        let tail = 3 * 8 + 1 + 4 + 4 + 1;
        let body = &bytes[bytes.len() - tail - 3..];
        assert_eq!(&body[..3], &[5, 1, 1]);
        assert_eq!(&body[3..11], &1.0f64.to_be_bytes());
        assert_eq!(body[27], 4);
        assert_eq!(body[body.len() - 1], 1);
    }
}
