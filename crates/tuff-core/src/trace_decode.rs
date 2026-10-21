//! Tracer-only decoder from a clientbound (state, id, body) to readable typed
//! text. Clientbound packets only implement encoding and borrow their fields,
//! so each one is described here by a small wire schema that a tiny
//! interpreter walks. Tests enforce coverage of the generated packet tables
//! and round-trip every encoder through its schema.

use std::fmt::{Debug, Write};

use bytes::Bytes;
use tuff_vanilla_data::packet as p;
use tuff_vanilla_data::registry as reg;

use crate::protocol::Packet;
use crate::protocol::codec::ReadExt;
use crate::protocol::login::{EncryptionRequest, LoginDisconnect, SetCompression};
use crate::protocol::status::{PongResponse, StatusResponse};
use tuff_item::item::Components;
use tuff_item::text::TextComponent;
use tuff_trace::ConnState;
use tuff_wire::{BufferError, PacketReader};
use tuff_world_model::BlockPos;

const MAX_DEBUG_CHARS: usize = 512;
const MAX_LIST_ITEMS: usize = 16;

type Lookup = fn(i32) -> Option<&'static str>;
type Fields = &'static [(&'static str, F)];
type Variants = &'static [(&'static str, Fields)];

#[derive(Clone, Copy)]
enum F {
    VarInt,
    VarLong,
    Int,
    Long,
    Short,
    Byte,
    UByte,
    Bool,
    Float,
    Double,
    Angle,
    Uuid,
    Str,
    Pos,
    Text,
    Nbt,
    LpVec3,
    Vec3,
    Slot,
    Template,
    Rest,
    Reg(Lookup),
    RegPlusOne(Lookup),
    Opt(&'static F),
    List(&'static F),
    Fixed(usize, &'static F),
    Obj(Fields),
    VarTag(Variants),
    ByteTag(Variants),
    Holder(Lookup, Fields),
    Custom(fn(&mut Out) -> Res),
}

enum Stop {
    Malformed,
    Opaque,
}

impl From<BufferError> for Stop {
    fn from(_: BufferError) -> Self {
        Stop::Malformed
    }
}

type Res = std::result::Result<(), Stop>;

struct Out {
    r: PacketReader,
    s: String,
    closers: Vec<&'static str>,
}

impl Out {
    fn open(&mut self, open: &str, close: &'static str) {
        self.s.push_str(open);
        self.closers.push(close);
    }

    fn close(&mut self) {
        if let Some(close) = self.closers.pop() {
            self.s.push_str(close);
        }
    }

    fn fields(&mut self, fields: Fields) -> Res {
        if fields.is_empty() {
            return Ok(());
        }
        self.open(" { ", " }");
        self.field_list(fields)?;
        self.close();
        Ok(())
    }

    fn field_list(&mut self, fields: Fields) -> Res {
        for (i, (name, f)) in fields.iter().enumerate() {
            self.field(i == 0, name, f)?;
        }
        Ok(())
    }

    fn field(&mut self, first: bool, name: &str, f: &F) -> Res {
        if !first {
            self.s.push_str(", ");
        }
        self.s.push_str(name);
        self.s.push_str(": ");
        self.value(f)
    }

    fn debug(&mut self, v: impl Debug) {
        let _ = write!(self.s, "{v:?}");
    }

    fn name(&mut self, id: i32, lookup: Lookup) {
        match lookup(id) {
            Some(name) => self.s.push_str(name),
            None => self.debug(id),
        }
    }

    fn count(&mut self) -> std::result::Result<usize, Stop> {
        let n = self.r.read_var_int()?;
        usize::try_from(n)
            .ok()
            .filter(|n| *n <= self.r.remaining())
            .ok_or(Stop::Malformed)
    }

    fn list(&mut self, n: usize, f: &F) -> Res {
        self.open("[", "]");
        for i in 0..n {
            let mark = self.s.len();
            if i > 0 {
                self.s.push_str(", ");
            }
            self.value(f)?;
            if i >= MAX_LIST_ITEMS {
                self.s.truncate(mark);
            }
        }
        if n > MAX_LIST_ITEMS {
            let _ = write!(self.s, ", ..+{}", n - MAX_LIST_ITEMS);
        }
        self.close();
        Ok(())
    }

    fn text(&mut self) -> Res {
        match self.r.read_nbt()? {
            Some(tag) => match TextComponent::from_nbt(&tag) {
                Some(text) => self.debug(text.plain()),
                None => self.debug(tag),
            },
            None => self.s.push_str("null"),
        }
        Ok(())
    }

    fn components(&mut self, added: usize, removed: usize) -> Res {
        let mut components = Components::default();
        let mut kinds = Vec::new();
        for _ in 0..added {
            let id = self.r.read_var_int()?;
            let kind = u32::try_from(id)
                .ok()
                .and_then(reg::DataComponentType::from_id)
                .ok_or(Stop::Opaque)?;
            if !self.r.read_component_at(kind, &mut components, 0)? {
                return Err(Stop::Opaque);
            }
            kinds.push(kind.name());
        }
        if !kinds.is_empty() {
            let _ = write!(self.s, " +{kinds:?}");
        }
        for i in 0..removed {
            let id = self.r.read_var_int()?;
            self.s.push_str(if i == 0 { " -[" } else { ", " });
            self.name(id, component_name);
        }
        if removed > 0 {
            self.s.push(']');
        }
        Ok(())
    }

    fn item(&mut self, count: i32, item: i32) {
        self.name(item, item_name);
        let _ = write!(self.s, " x{count}");
    }

    fn value(&mut self, f: &F) -> Res {
        match *f {
            F::VarInt => {
                let v = self.r.read_var_int()?;
                self.debug(v)
            }
            F::VarLong => {
                let v = self.r.read_var_long()?;
                self.debug(v)
            }
            F::Int => {
                let v = self.r.read_int()?;
                self.debug(v)
            }
            F::Long => {
                let v = self.r.read_long()?;
                self.debug(v)
            }
            F::Short => {
                let v = self.r.read_short()?;
                self.debug(v)
            }
            F::Byte => {
                let v = self.r.read_byte()?;
                self.debug(v)
            }
            F::UByte => {
                let v = self.r.read_unsigned_byte()?;
                self.debug(v)
            }
            F::Bool => {
                let v = self.r.read_bool()?;
                self.debug(v)
            }
            F::Float => {
                let v = self.r.read_float()?;
                self.debug(v)
            }
            F::Double => {
                let v = self.r.read_double()?;
                self.debug(v)
            }
            F::Angle => {
                let v = self.r.read_unsigned_byte()? as f32 * 360.0 / 256.0;
                self.debug(v)
            }
            F::Uuid => {
                let v = self.r.read_uuid()?;
                let _ = write!(self.s, "{v:032x}");
            }
            F::Str => {
                let v = self.r.read_string()?;
                self.debug(v)
            }
            F::Pos => {
                let BlockPos { x, y, z } = self.r.read_position()?;
                let _ = write!(self.s, "({x}, {y}, {z})");
            }
            F::Text => self.text()?,
            F::Nbt => {
                let v = self.r.read_nbt()?;
                self.debug(v)
            }
            F::LpVec3 => {
                let v = self.r.read_lp_vec3()?;
                self.debug(v)
            }
            F::Vec3 => {
                let v = [
                    self.r.read_double()?,
                    self.r.read_double()?,
                    self.r.read_double()?,
                ];
                self.debug(v)
            }
            F::Slot => {
                let count = self.r.read_var_int()?;
                if count <= 0 {
                    self.s.push_str("empty");
                    return Ok(());
                }
                let item = self.r.read_var_int()?;
                self.item(count, item);
                let added = self.count()?;
                let removed = self.count()?;
                self.components(added, removed)?;
            }
            F::Template => {
                let item = self.r.read_var_int()?;
                let count = self.r.read_var_int()?;
                self.item(count, item);
                let added = self.count()?;
                let removed = self.count()?;
                self.components(added, removed)?;
            }
            F::Rest => {
                let n = self.r.remaining();
                self.r.read_bytes(n)?;
                let _ = write!(self.s, "<{n} bytes>");
            }
            F::Reg(lookup) => {
                let id = self.r.read_var_int()?;
                self.name(id, lookup)
            }
            F::RegPlusOne(lookup) => match self.r.read_var_int()? {
                0 => self.s.push_str("None"),
                id => self.name(id - 1, lookup),
            },
            F::Opt(inner) => {
                if self.r.read_bool()? {
                    self.value(inner)?;
                } else {
                    self.s.push_str("None");
                }
            }
            F::List(inner) => {
                let n = self.count()?;
                self.list(n, inner)?;
            }
            F::Fixed(n, inner) => self.list(n, inner)?,
            F::Obj(fields) => {
                self.open("{ ", " }");
                self.field_list(fields)?;
                self.close();
            }
            F::VarTag(variants) => {
                let tag = self.r.read_var_int()?;
                self.variant(tag, variants)?;
            }
            F::ByteTag(variants) => {
                let tag = i32::from(self.r.read_byte()?);
                self.variant(tag, variants)?;
            }
            F::Holder(lookup, inline) => match self.r.read_var_int()? {
                0 => {
                    self.s.push_str("Inline");
                    self.fields(inline)?;
                }
                id => self.name(id - 1, lookup),
            },
            F::Custom(decode) => decode(self)?,
        }
        Ok(())
    }

    fn variant(&mut self, tag: i32, variants: Variants) -> Res {
        let (name, fields) = usize::try_from(tag)
            .ok()
            .and_then(|i| variants.get(i))
            .ok_or(Stop::Malformed)?;
        self.s.push_str(name);
        self.fields(fields)
    }
}

fn no_name(_: i32) -> Option<&'static str> {
    None
}

fn poi_type(id: i32) -> Option<&'static str> {
    reg::PointOfInterestType::from_id(u32::try_from(id).ok()?).map(|e| e.name())
}

fn game_event_type(id: i32) -> Option<&'static str> {
    reg::GameEvent::from_id(u32::try_from(id).ok()?).map(|e| e.name())
}

const DEBUG_POI: F = F::Obj(&[
    ("pos", F::Pos),
    ("type", F::Reg(poi_type)),
    ("free_tickets", F::VarInt),
]);
const DEBUG_GAME_EVENT: F = F::Obj(&[("event", F::Reg(game_event_type)), ("pos", F::Vec3)]);
const DEBUG_BOX: F = F::Obj(&[("min", F::Pos), ("max", F::Pos)]);
const DEBUG_STRUCTURES: F = F::List(&F::Obj(&[
    ("box", DEBUG_BOX),
    (
        "pieces",
        F::List(&F::Obj(&[("box", DEBUG_BOX), ("start", F::Bool)])),
    ),
]));
const DEBUG_NODE: F = F::Obj(&[
    ("x", F::Int),
    ("y", F::Int),
    ("z", F::Int),
    ("walked", F::Float),
    ("malus", F::Float),
    ("closed", F::Bool),
    ("type", F::VarInt),
    ("f", F::Float),
]);
const DEBUG_PATH: F = F::Obj(&[
    ("reached", F::Bool),
    ("next", F::Int),
    ("target", F::Pos),
    ("nodes", F::List(&DEBUG_NODE)),
    ("open", F::List(&DEBUG_NODE)),
    ("closed", F::List(&DEBUG_NODE)),
    ("targets", F::List(&DEBUG_NODE)),
    ("max_node_distance", F::Float),
]);
const DEBUG_GOALS: F = F::List(&F::Obj(&[
    ("priority", F::VarInt),
    ("running", F::Bool),
    ("name", F::Str),
]));
const DEBUG_BRAIN: F = F::Obj(&[
    ("name", F::Str),
    ("profession", F::Str),
    ("xp", F::Int),
    ("health", F::Float),
    ("max_health", F::Float),
    ("inventory", F::Str),
    ("wants_golem", F::Bool),
    ("anger", F::Int),
    ("activities", F::List(&F::Str)),
    ("behaviors", F::List(&F::Str)),
    ("memories", F::List(&F::Str)),
    ("gossips", F::List(&F::Str)),
    ("pois", F::List(&F::Pos)),
    ("potential_pois", F::List(&F::Pos)),
]);
const DEBUG_BEE: F = F::Obj(&[
    ("hive", F::Opt(&F::Pos)),
    ("flower", F::Opt(&F::Pos)),
    ("travel_ticks", F::VarInt),
    ("blacklisted", F::List(&F::Pos)),
]);
const DEBUG_HIVE: F = F::Obj(&[
    ("block", F::Reg(block)),
    ("occupants", F::VarInt),
    ("honey", F::VarInt),
    ("sedated", F::Bool),
]);
const DEBUG_BREEZE: F = F::Obj(&[
    ("attack_target", F::Opt(&F::VarInt)),
    ("jump_target", F::Opt(&F::Pos)),
]);
const DEBUG_LISTENER: F = F::Obj(&[("radius", F::VarInt)]);

/// Each `debug_subscription` by id with its value; feeds the server
/// never sends stay opaque.
const DEBUG_UPDATES: Variants = &[
    ("dedicated_server_tick_time", &[]),
    ("bees", &[("value", F::Opt(&DEBUG_BEE))]),
    ("brains", &[("value", F::Opt(&DEBUG_BRAIN))]),
    ("breezes", &[("value", F::Opt(&DEBUG_BREEZE))]),
    ("goal_selectors", &[("value", F::Opt(&DEBUG_GOALS))]),
    ("entity_paths", &[("value", F::Opt(&DEBUG_PATH))]),
    (
        "entity_block_intersections",
        &[("value", F::Opt(&F::VarInt))],
    ),
    ("bee_hives", &[("value", F::Opt(&DEBUG_HIVE))]),
    ("pois", &[("value", F::Opt(&DEBUG_POI))]),
    ("redstone_wire_orientations", &[("value", F::Rest)]),
    ("village_sections", &[("present", F::Bool)]),
    ("raids", &[("value", F::Opt(&F::List(&F::Pos)))]),
    ("structures", &[("value", F::Opt(&DEBUG_STRUCTURES))]),
    (
        "game_event_listeners",
        &[("value", F::Opt(&DEBUG_LISTENER))],
    ),
    ("neighbor_updates", &[("value", F::Opt(&F::Pos))]),
    ("game_events", &[("value", F::Opt(&DEBUG_GAME_EVENT))]),
];

const DEBUG_UPDATE: F = F::VarTag(DEBUG_UPDATES);

const DEBUG_EVENTS: Variants = &[
    ("dedicated_server_tick_time", &[]),
    ("bees", &[("value", DEBUG_BEE)]),
    ("brains", &[("value", DEBUG_BRAIN)]),
    ("breezes", &[("value", DEBUG_BREEZE)]),
    ("goal_selectors", &[("value", DEBUG_GOALS)]),
    ("entity_paths", &[("value", DEBUG_PATH)]),
    ("entity_block_intersections", &[("value", F::VarInt)]),
    ("bee_hives", &[("value", DEBUG_HIVE)]),
    ("pois", &[("value", DEBUG_POI)]),
    ("redstone_wire_orientations", &[("value", F::Rest)]),
    ("village_sections", &[]),
    ("raids", &[("value", F::List(&F::Pos))]),
    ("structures", &[("value", DEBUG_STRUCTURES)]),
    ("game_event_listeners", &[("value", DEBUG_LISTENER)]),
    ("neighbor_updates", &[("pos", F::Pos)]),
    ("game_events", &[("value", DEBUG_GAME_EVENT)]),
];

fn item_name(id: i32) -> Option<&'static str> {
    tuff_vanilla_data::Item::from_id(u32::try_from(id).ok()?).map(|i| i.name())
}

fn component_name(id: i32) -> Option<&'static str> {
    reg::DataComponentType::from_id(u32::try_from(id).ok()?).map(|k| k.name())
}

fn entity_type(id: i32) -> Option<&'static str> {
    reg::EntityType::from_id(u32::try_from(id).ok()?).map(|e| e.name())
}

fn block(id: i32) -> Option<&'static str> {
    reg::Block::from_id(u32::try_from(id).ok()?).map(|b| b.name())
}

fn block_state(id: i32) -> Option<&'static str> {
    tuff_vanilla_data::block::BlockState::from_id(u32::try_from(id).ok()?).map(|s| s.block().name())
}

fn sound_event(id: i32) -> Option<&'static str> {
    reg::SoundEvent::from_id(u32::try_from(id).ok()?).map(|e| e.name())
}

fn mob_effect(id: i32) -> Option<&'static str> {
    reg::MobEffect::from_id(u32::try_from(id).ok()?).map(|e| e.name())
}

fn menu(id: i32) -> Option<&'static str> {
    reg::Menu::from_id(u32::try_from(id).ok()?).map(|e| e.name())
}

fn attribute(id: i32) -> Option<&'static str> {
    reg::Attribute::from_id(u32::try_from(id).ok()?).map(|e| e.name())
}

fn particle(id: i32) -> Option<&'static str> {
    reg::ParticleType::from_id(u32::try_from(id).ok()?).map(|e| e.name())
}

fn block_entity(id: i32) -> Option<&'static str> {
    reg::BlockEntityType::from_id(u32::try_from(id).ok()?).map(|e| e.name())
}

fn map_decoration(id: i32) -> Option<&'static str> {
    reg::MapDecorationType::from_id(u32::try_from(id).ok()?).map(|e| e.name())
}

fn data_entry(
    registry: &'static tuff_vanilla_data::data_registry::DataRegistry,
    id: i32,
) -> Option<&'static str> {
    registry.entries.get(usize::try_from(id).ok()?).copied()
}

fn damage_type(id: i32) -> Option<&'static str> {
    data_entry(&tuff_vanilla_data::data_registry::DAMAGE_TYPE, id)
}

fn dimension_type(id: i32) -> Option<&'static str> {
    data_entry(&tuff_vanilla_data::data_registry::DIMENSION_TYPE, id)
}

fn chat_type(id: i32) -> Option<&'static str> {
    data_entry(&tuff_vanilla_data::data_registry::CHAT_TYPE, id)
}

fn game_mode(id: i32) -> Option<&'static str> {
    crate::protocol::play::GameMode::from_id(id).map(|m| m.name())
}

fn game_event_name(id: i32) -> Option<&'static str> {
    Some(match id {
        0 => "no_respawn_block_available",
        1 => "start_raining",
        2 => "stop_raining",
        3 => "change_game_mode",
        4 => "win_game",
        5 => "demo_event",
        6 => "play_arrow_hit_sound",
        7 => "rain_level_change",
        8 => "thunder_level_change",
        9 => "puffer_fish_sting",
        10 => "guardian_elder_effect",
        11 => "immediate_respawn",
        12 => "limited_crafting",
        13 => "level_chunks_load_start",
        _ => return None,
    })
}

fn opaque(_: &mut Out) -> Res {
    Err(Stop::Opaque)
}

fn game_event(o: &mut Out) -> Res {
    let id = o.r.read_unsigned_byte()?;
    o.name(i32::from(id), game_event_name);
    Ok(())
}

fn optional_block_state(o: &mut Out) -> Res {
    match o.r.read_var_int()? {
        0 => o.s.push_str("None"),
        id => o.name(id, block_state),
    }
    Ok(())
}

fn entity_metadata(o: &mut Out) -> Res {
    use tuff_vanilla_data::entity::data_serializer as ds;
    o.open("[", "]");
    let mut first = true;
    loop {
        let index = o.r.read_unsigned_byte()?;
        if index == 0xFF {
            break;
        }
        if !first {
            o.s.push_str(", ");
        }
        first = false;
        let serializer = o.r.read_var_int()?;
        let _ = write!(o.s, "{index}=");
        let f = match u8::try_from(serializer).map_err(|_| Stop::Malformed)? {
            ds::BYTE => F::Byte,
            ds::LONG => F::VarLong,
            ds::FLOAT => F::Float,
            ds::STRING => F::Str,
            ds::COMPONENT => F::Text,
            ds::OPTIONAL_COMPONENT => F::Opt(&F::Text),
            ds::ITEM_STACK => F::Slot,
            ds::BOOLEAN => F::Bool,
            ds::ROTATIONS | ds::VECTOR3 => F::Fixed(3, &F::Float),
            ds::QUATERNION => F::Fixed(4, &F::Float),
            ds::BLOCK_POS => F::Pos,
            ds::OPTIONAL_BLOCK_POS => F::Opt(&F::Pos),
            ds::OPTIONAL_LIVING_ENTITY_REFERENCE => F::Opt(&F::Uuid),
            ds::BLOCK_STATE => F::Reg(block_state),
            ds::OPTIONAL_BLOCK_STATE => F::Custom(optional_block_state),
            ds::OPTIONAL_UNSIGNED_INT => F::RegPlusOne(no_name),
            ds::VILLAGER_DATA => F::Fixed(3, &F::VarInt),
            ds::RESOLVABLE_PROFILE => F::Custom(resolvable_profile),
            ds::OPTIONAL_GLOBAL_POS => F::Opt(&F::Obj(&[("dimension", F::Str), ("pos", F::Pos)])),
            ds::PARTICLE | ds::PARTICLES => {
                let _ = write!(o.s, "serializer {serializer}");
                return Err(Stop::Opaque);
            }
            _ => F::VarInt,
        };
        o.value(&f)?;
    }
    o.close();
    Ok(())
}

fn particle_with_options(o: &mut Out) -> Res {
    let id = o.r.read_var_int()?;
    o.name(id, particle);
    let name = particle(id).unwrap_or_default();
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    let options: &[F] = match name {
        "block" | "block_marker" | "falling_dust" | "dust_pillar" | "block_crumble" => {
            &[F::Reg(block_state)]
        }
        "dust" => &[F::Int, F::Float],
        "dust_color_transition" => &[F::Int, F::Int, F::Float],
        "entity_effect" | "tinted_leaves" | "flash" => &[F::Int],
        "sculk_charge" | "power" => &[F::Float],
        "shriek" => &[F::VarInt],
        "item" => &[F::Slot],
        "vibration" | "trail" => return Err(Stop::Opaque),
        _ => &[],
    };
    for f in options {
        o.s.push(' ');
        o.value(f)?;
    }
    Ok(())
}

fn resolvable_profile(o: &mut Out) -> Res {
    match o.r.read_var_int()? {
        0 => {
            o.s.push_str("Partial");
            o.fields(&[
                ("name", F::Opt(&F::Str)),
                ("id", F::Opt(&F::Uuid)),
                ("properties", F::List(&PROFILE_PROPERTY)),
                ("texture", F::Opt(&F::Str)),
                ("cape", F::Opt(&F::Str)),
                ("elytra", F::Opt(&F::Str)),
                ("model", F::Opt(&F::VarInt)),
            ])
        }
        _ => Err(Stop::Opaque),
    }
}

fn equipment(o: &mut Out) -> Res {
    o.open("[", "]");
    loop {
        let slot = o.r.read_unsigned_byte()?;
        let _ = write!(o.s, "{}=", slot & 0x7F);
        o.value(&F::Slot)?;
        if slot & 0x80 == 0 {
            break;
        }
        o.s.push_str(", ");
    }
    o.close();
    Ok(())
}

fn section_block(o: &mut Out) -> Res {
    let v = o.r.read_var_long()?;
    let index = v & 0xFFF;
    let _ = write!(
        o.s,
        "({}, {}, {})=",
        index >> 8,
        index & 0xF,
        (index >> 4) & 0xF
    );
    o.name((v >> 12) as i32, block_state);
    Ok(())
}

fn section_blocks(o: &mut Out) -> Res {
    let packed = o.r.read_long()?;
    let x = packed >> 42;
    let y = packed << 44 >> 44;
    let z = packed << 22 >> 42;
    let _ = write!(o.s, "({x}, {y}, {z}), blocks: ");
    let n = o.count()?;
    o.list(n, &F::Custom(section_block))
}

fn sound_position(o: &mut Out) -> Res {
    let v = [o.r.read_int()?, o.r.read_int()?, o.r.read_int()?].map(|c| f64::from(c) / 8.0);
    o.debug(v);
    Ok(())
}

fn stop_sound(o: &mut Out) -> Res {
    let flags = o.r.read_unsigned_byte()?;
    o.s.push_str("{ category: ");
    if flags & 1 != 0 {
        o.value(&F::VarInt)?;
    } else {
        o.s.push_str("None");
    }
    o.s.push_str(", sound: ");
    if flags & 2 != 0 {
        o.value(&F::Str)?;
    } else {
        o.s.push_str("None");
    }
    o.s.push_str(" }");
    Ok(())
}

fn map_patch(o: &mut Out) -> Res {
    let width = o.r.read_unsigned_byte()?;
    if width == 0 {
        o.s.push_str("None");
        return Ok(());
    }
    let height = o.r.read_unsigned_byte()?;
    let x = o.r.read_unsigned_byte()?;
    let y = o.r.read_unsigned_byte()?;
    let n = o.count()?;
    o.r.read_bytes(n)?;
    let _ = write!(
        o.s,
        "{{ width: {width}, height: {height}, x: {x}, y: {y}, colors: <{n} bytes> }}"
    );
    Ok(())
}

fn player_info_update(o: &mut Out) -> Res {
    let actions = o.r.read_unsigned_byte()?;
    let _ = write!(o.s, "{actions:#04x}, entries: ");
    let parts: [(u8, &str, F); 8] = [
        (0x01, "name", F::Str),
        (0x01, "properties", F::List(&PROFILE_PROPERTY)),
        (0x02, "chat_session", OPAQUE),
        (0x04, "game_mode", F::Reg(game_mode)),
        (0x08, "listed", F::Bool),
        (0x10, "latency", F::VarInt),
        (0x20, "display_name", TEXT_OPT),
        (0x40, "list_order", F::VarInt),
    ];
    let n = o.count()?;
    o.open("[", "]");
    for i in 0..n {
        if i > 0 {
            o.s.push_str(", ");
        }
        o.open("{ ", " }");
        o.field(true, "uuid", &F::Uuid)?;
        for (bit, name, f) in &parts {
            if actions & bit != 0 {
                o.field(false, name, f)?;
            }
        }
        if actions & 0x80 != 0 {
            o.field(false, "hat", &F::Bool)?;
        }
        o.close();
    }
    o.close();
    Ok(())
}

fn commands(o: &mut Out) -> Res {
    let n = o.count()?;
    let _ = write!(o.s, "{n} nodes");
    Err(Stop::Opaque)
}

fn recipe_book_add(o: &mut Out) -> Res {
    let n = o.count()?;
    let _ = write!(o.s, "{n} entries");
    Err(Stop::Opaque)
}

fn trade_cost(o: &mut Out) -> Res {
    let item = o.r.read_var_int()?;
    let count = o.r.read_var_int()?;
    o.item(count, item);
    let added = o.count()?;
    o.components(added, 0)
}

fn advancement_flags(o: &mut Out) -> Res {
    let flags = o.r.read_int()?;
    o.debug(flags);
    if flags & 1 != 0 {
        o.s.push_str(", background: ");
        o.value(&F::Str)?;
    }
    Ok(())
}

fn packet<P: Packet + Debug>(r: &mut PacketReader) -> Option<String> {
    Some(format!("{:?}", P::decode(r).ok()?))
}

fn schema(state: ConnState, id: i32) -> Option<(String, Fields)> {
    match state {
        ConnState::Login if id == p::login::Clientbound::LoginFinished.id() => {
            Some(("LoginFinished".into(), LOGIN_FINISHED))
        }
        ConnState::Configuration => {
            let kind = p::configuration::Clientbound::from_id(id)?;
            CONFIGURATION
                .iter()
                .find(|(k, _)| *k == kind)
                .map(|(k, f)| (format!("{k:?}"), *f))
        }
        ConnState::Play => {
            let kind = p::play::Clientbound::from_id(id)?;
            PLAY.iter()
                .find(|(k, _)| *k == kind)
                .map(|(k, f)| (format!("{k:?}"), *f))
        }
        _ => None,
    }
}

fn fixed(state: ConnState, id: i32, r: &mut PacketReader) -> Option<Option<String>> {
    use p::{login as l, status as s};
    Some(match state {
        ConnState::Status if id == s::Clientbound::StatusResponse.id() => {
            packet::<StatusResponse>(r)
        }
        ConnState::Status if id == s::Clientbound::PongResponse.id() => packet::<PongResponse>(r),
        ConnState::Login if id == l::Clientbound::LoginDisconnect.id() => {
            packet::<LoginDisconnect>(r)
        }
        ConnState::Login if id == l::Clientbound::Hello.id() => packet::<EncryptionRequest>(r),
        ConnState::Login if id == l::Clientbound::LoginCompression.id() => {
            packet::<SetCompression>(r)
        }
        _ => return None,
    })
}

fn decode_schema(name: String, fields: Fields, r: PacketReader) -> Option<String> {
    let mut out = Out {
        r,
        s: name,
        closers: Vec::new(),
    };
    match out.fields(fields) {
        Ok(()) => {
            if out.r.remaining() != 0 {
                let _ = write!(out.s, " +{} trailing bytes", out.r.remaining());
            }
        }
        Err(Stop::Opaque) => {
            let _ = write!(out.s, " ..<{} bytes>", out.r.remaining());
            while !out.closers.is_empty() {
                out.close();
            }
        }
        Err(Stop::Malformed) => return None,
    }
    Some(out.s)
}

fn cap(mut text: String, total: usize) -> String {
    if let Some((at, _)) = text.char_indices().nth(MAX_DEBUG_CHARS) {
        text.truncate(at);
        text.push_str(&format!("... ({total} bytes)"));
    }
    text
}

/// Typed text for a clientbound payload that starts with its id, or None
/// when the packet is not covered or does not decode cleanly.
pub fn typed_text(state: ConnState, payload: &Bytes) -> Option<String> {
    let mut reader = PacketReader::new(payload.clone());
    let id = reader.read_var_int().ok()?;
    let text = match fixed(state, id, &mut reader) {
        Some(text) => text?,
        None => {
            let (name, fields) = schema(state, id)?;
            decode_schema(name, fields, reader)?
        }
    };
    Some(cap(text, payload.len()))
}

mod schemas;
use schemas::*;

#[cfg(test)]
mod tests;
