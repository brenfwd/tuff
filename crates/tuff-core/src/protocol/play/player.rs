use tuff_vanilla_data::packet::play::Clientbound as Id;

use crate::auth::ProfileProperty;
use crate::protocol::{Encode, Packet};
use tuff_item::text::TextComponent;
use tuff_wire::{BufferError, PacketWriter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameMode {
    Survival,
    Creative,
    Adventure,
    Spectator,
}

impl GameMode {
    pub const ALL: [Self; 4] = [
        Self::Survival,
        Self::Creative,
        Self::Adventure,
        Self::Spectator,
    ];

    pub fn id(self) -> i32 {
        self as i32
    }

    pub fn from_id(id: i32) -> Option<Self> {
        Self::ALL.get(usize::try_from(id).ok()?).copied()
    }

    /// The lowercase name used by commands, saved data and translation keys.
    pub fn name(self) -> &'static str {
        match self {
            Self::Survival => "survival",
            Self::Creative => "creative",
            Self::Adventure => "adventure",
            Self::Spectator => "spectator",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.name() == name)
    }

    /// The protocol's optional game mode: 0 for none, else the ID plus one.
    fn optional_id(mode: Option<Self>) -> i32 {
        mode.map_or(0, |mode| mode.id() + 1)
    }
}

use crate::protocol::codec::WriteExt;
use tuff_world_model::BlockPos;

pub struct Login<'a> {
    pub entity_id: i32,
    pub dimensions: &'a [&'a str],
    pub max_players: i32,
    pub view_distance: i32,
    pub simulation_distance: i32,
    pub dimension_type: i32,
    pub dimension: &'a str,
    pub game_mode: GameMode,
    pub previous_game_mode: Option<GameMode>,
    pub sea_level: i32,
    pub online_mode: bool,
    pub reduced_debug_info: bool,
    pub hardcore: bool,
    pub last_death: Option<(&'a str, BlockPos)>,
    pub is_debug: bool,
    pub is_flat: bool,
    pub portal_cooldown: i32,
}

fn write_death_location(w: &mut PacketWriter, death: Option<(&str, BlockPos)>) {
    w.write_bool(death.is_some());
    if let Some((dimension, pos)) = death {
        w.write_string(dimension);
        w.write_position(pos);
    }
}

impl Encode for Login<'_> {
    const ID: i32 = Id::Login.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_int(self.entity_id);
        w.write_bool(self.hardcore);
        w.write_var_int(self.dimensions.len() as i32);
        for dimension in self.dimensions {
            w.write_string(dimension);
        }
        w.write_var_int(self.max_players);
        w.write_var_int(self.view_distance);
        w.write_var_int(self.simulation_distance);
        w.write_bool(self.reduced_debug_info);
        w.write_bool(true);
        w.write_bool(false);
        w.write_var_int(self.dimension_type);
        w.write_string(self.dimension);
        w.write_long(0);
        w.write_var_int(self.game_mode.id());
        w.write_var_int(GameMode::optional_id(self.previous_game_mode));
        w.write_bool(self.is_debug);
        w.write_bool(self.is_flat);
        write_death_location(w, self.last_death);
        w.write_var_int(self.portal_cooldown);
        w.write_var_int(self.sea_level);
        w.write_bool(self.online_mode);
        w.write_bool(false);
    }
}

pub mod respawn {
    /// Nothing survives the respawn: a death.
    pub const KEEP_NOTHING: i8 = 0;
    /// Leaving the end through the credits keeps attributes only.
    pub const KEEP_ATTRIBUTES: i8 = 1;
    /// A dimension change keeps attributes and entity data.
    pub const KEEP_ALL_DATA: i8 = 3;
}

pub struct Respawn<'a> {
    pub dimension_type: i32,
    pub dimension: &'a str,
    pub game_mode: GameMode,
    pub previous_game_mode: Option<GameMode>,
    pub sea_level: i32,
    /// A [`respawn`] data-kept mask.
    pub data_kept: i8,
    pub last_death: Option<(&'a str, BlockPos)>,
    pub is_debug: bool,
    pub is_flat: bool,
    pub portal_cooldown: i32,
}

impl Encode for Respawn<'_> {
    const ID: i32 = Id::Respawn.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.dimension_type);
        w.write_string(self.dimension);
        w.write_long(0);
        w.write_var_int(self.game_mode.id());
        w.write_var_int(GameMode::optional_id(self.previous_game_mode));
        w.write_bool(self.is_debug);
        w.write_bool(self.is_flat);
        write_death_location(w, self.last_death);
        w.write_var_int(self.portal_cooldown);
        w.write_var_int(self.sea_level);
        w.write_byte(self.data_kept);
    }
}

pub mod abilities {
    pub const INVULNERABLE: i8 = 0x01;
    pub const FLYING: i8 = 0x02;
    pub const MAY_FLY: i8 = 0x04;
    pub const INSTABUILD: i8 = 0x08;
}

#[derive(Packet)]
#[packet(id = Id::PlayerAbilities, encode_only)]
pub struct PlayerAbilities {
    pub flags: i8,
    pub flying_speed: f32,
    pub fov_modifier: f32,
}

/// Synchronize Player Position with absolute coordinates and zero velocity.
pub struct PlayerPosition {
    pub teleport_id: i32,
    pub position: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
}

impl Encode for PlayerPosition {
    const ID: i32 = Id::PlayerPosition.id();

    fn encode(&self, w: &mut PacketWriter) {
        PlayerPositionDelta {
            teleport_id: self.teleport_id,
            position: self.position,
            delta: [0.0; 3],
            yaw: self.yaw,
            pitch: self.pitch,
            relative: 0,
        }
        .encode(w);
    }
}

pub mod position_relative {
    pub const X: u32 = 1 << 0;
    pub const Y: u32 = 1 << 1;
    pub const Z: u32 = 1 << 2;
    pub const YAW: u32 = 1 << 3;
    pub const PITCH: u32 = 1 << 4;
    pub const DELTA_X: u32 = 1 << 5;
    pub const DELTA_Y: u32 = 1 << 6;
    pub const DELTA_Z: u32 = 1 << 7;
    pub const ROTATE_DELTA: u32 = 1 << 8;
}

/// Synchronize Player Position with a velocity and a set of relative fields
/// (see [`position_relative`]).
pub struct PlayerPositionDelta {
    pub teleport_id: i32,
    pub position: [f64; 3],
    pub delta: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    pub relative: u32,
}

impl Encode for PlayerPositionDelta {
    const ID: i32 = Id::PlayerPosition.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.teleport_id);
        for c in self.position {
            w.write_double(c);
        }
        for c in self.delta {
            w.write_double(c);
        }
        w.write_float(self.yaw);
        w.write_float(self.pitch);
        w.write_int(self.relative as i32);
    }
}

/// Sets only the player's rotation; each axis may be relative to the current one.
pub struct PlayerRotation {
    pub yaw: f32,
    pub relative_yaw: bool,
    pub pitch: f32,
    pub relative_pitch: bool,
}

impl Encode for PlayerRotation {
    const ID: i32 = Id::PlayerRotation.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_float(self.yaw);
        w.write_bool(self.relative_yaw);
        w.write_float(self.pitch);
        w.write_bool(self.relative_pitch);
    }
}

/// Turns the client's camera toward a point, or toward an entity anchor.
pub struct PlayerLookAt {
    pub from_eyes: bool,
    pub position: [f64; 3],
    pub entity: Option<(i32, bool)>,
}

impl Encode for PlayerLookAt {
    const ID: i32 = Id::PlayerLookAt.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(i32::from(self.from_eyes));
        for c in self.position {
            w.write_double(c);
        }
        w.write_bool(self.entity.is_some());
        if let Some((id, to_eyes)) = self.entity {
            w.write_var_int(id);
            w.write_var_int(i32::from(to_eyes));
        }
    }
}

pub mod player_info {
    pub const ADD_PLAYER: u8 = 0x01;
    pub const INITIALIZE_CHAT: u8 = 0x02;
    pub const UPDATE_GAME_MODE: u8 = 0x04;
    pub const UPDATE_LISTED: u8 = 0x08;
    pub const UPDATE_LATENCY: u8 = 0x10;
    pub const UPDATE_DISPLAY_NAME: u8 = 0x20;
    pub const UPDATE_LIST_ORDER: u8 = 0x40;
    pub const UPDATE_HAT: u8 = 0x80;
}

#[derive(Debug, Clone)]
pub struct PlayerInfoEntry {
    pub uuid: u128,
    pub name: String,
    pub properties: Vec<ProfileProperty>,
    pub game_mode: GameMode,
    pub listed: bool,
    pub latency: i32,
    pub hat: bool,
    pub display_name: Option<TextComponent>,
    pub list_order: i32,
    /// The client's chat visibility setting; never sent in this packet.
    pub chat_mode: i32,
}

pub struct PlayerInfoUpdate<'a> {
    pub actions: u8,
    pub entries: &'a [PlayerInfoEntry],
}

impl Encode for PlayerInfoUpdate<'_> {
    const ID: i32 = Id::PlayerInfoUpdate.id();

    fn encode(&self, w: &mut PacketWriter) {
        use player_info::*;
        w.write_unsigned_byte(self.actions);
        w.write_var_int(self.entries.len() as i32);
        for entry in self.entries {
            w.write_uuid(entry.uuid);
            if self.actions & ADD_PLAYER != 0 {
                w.write_string(&entry.name);
                w.write_var_int(entry.properties.len() as i32);
                for property in &entry.properties {
                    w.write_string(&property.name);
                    w.write_string(&property.value);
                    w.write_bool(property.signature.is_some());
                    if let Some(signature) = &property.signature {
                        w.write_string(signature);
                    }
                }
            }
            if self.actions & INITIALIZE_CHAT != 0 {
                w.write_bool(false);
            }
            if self.actions & UPDATE_GAME_MODE != 0 {
                w.write_var_int(entry.game_mode.id());
            }
            if self.actions & UPDATE_LISTED != 0 {
                w.write_bool(entry.listed);
            }
            if self.actions & UPDATE_LATENCY != 0 {
                w.write_var_int(entry.latency);
            }
            if self.actions & UPDATE_DISPLAY_NAME != 0 {
                w.write_bool(entry.display_name.is_some());
                if let Some(name) = &entry.display_name {
                    w.write_text(name);
                }
            }
            if self.actions & UPDATE_LIST_ORDER != 0 {
                w.write_var_int(entry.list_order);
            }
            if self.actions & UPDATE_HAT != 0 {
                w.write_bool(entry.hat);
            }
        }
    }
}

pub struct PlayerInfoRemove<'a>(pub &'a [u128]);

impl Encode for PlayerInfoRemove<'_> {
    const ID: i32 = Id::PlayerInfoRemove.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.0.len() as i32);
        for uuid in self.0 {
            w.write_uuid(*uuid);
        }
    }
}

#[derive(Packet)]
#[packet(id = Id::SetTitleText, encode_only)]
pub struct SetTitleText<'a>(pub &'a TextComponent);

#[derive(Packet)]
#[packet(id = Id::SetSubtitleText, encode_only)]
pub struct SetSubtitleText<'a>(pub &'a TextComponent);

#[derive(Packet)]
#[packet(id = Id::SetActionBarText, encode_only)]
pub struct SetActionBarText<'a>(pub &'a TextComponent);

#[derive(Packet)]
#[packet(id = Id::SetTitlesAnimation, encode_only)]
pub struct SetTitlesAnimation {
    #[packet(int)]
    pub fade_in: i32,
    #[packet(int)]
    pub stay: i32,
    #[packet(int)]
    pub fade_out: i32,
}

#[derive(Packet)]
#[packet(id = Id::ClearTitles, encode_only)]
pub struct ClearTitles {
    pub reset: bool,
}

pub mod effect_flags {
    pub const AMBIENT: u8 = 0x01;
    pub const SHOW_PARTICLES: u8 = 0x02;
    pub const SHOW_ICON: u8 = 0x04;
    pub const BLEND: u8 = 0x08;
}

#[derive(Packet)]
#[packet(id = Id::UpdateMobEffect, encode_only)]
pub struct UpdateMobEffect {
    #[packet(var_int)]
    pub entity_id: i32,
    /// The `minecraft:mob_effect` registry ID.
    #[packet(var_int)]
    pub effect: i32,
    #[packet(var_int)]
    pub amplifier: i32,
    /// Ticks, or -1 for an infinite effect.
    #[packet(var_int)]
    pub duration: i32,
    pub flags: u8,
}

#[derive(Packet)]
#[packet(id = Id::RemoveMobEffect, encode_only)]
pub struct RemoveMobEffect {
    #[packet(var_int)]
    pub entity_id: i32,
    #[packet(var_int)]
    pub effect: i32,
}

/// Set Cooldown: the client greys out every item in the cooldown group.
#[derive(Packet)]
#[packet(id = Id::Cooldown, encode_only)]
pub struct Cooldown<'a> {
    pub group: &'a str,
    #[packet(var_int)]
    pub ticks: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Movement {
    pub position: Option<[f64; 3]>,
    pub rotation: Option<(f32, f32)>,
    pub on_ground: bool,
    pub horizontal_collision: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerActionStatus {
    StartDestroyBlock = 0,
    ChangeDestroyDirection = 1,
    AbortDestroyBlock = 2,
    StopDestroyBlock = 3,
    DropAllItems = 4,
    DropItem = 5,
    ReleaseUseItem = 6,
    SwapItemWithOffhand = 7,
    Stab = 8,
}

impl PlayerActionStatus {
    pub fn from_id(id: i32) -> Result<Self, BufferError> {
        match id {
            0 => Ok(Self::StartDestroyBlock),
            1 => Ok(Self::ChangeDestroyDirection),
            2 => Ok(Self::AbortDestroyBlock),
            3 => Ok(Self::StopDestroyBlock),
            4 => Ok(Self::DropAllItems),
            5 => Ok(Self::DropItem),
            6 => Ok(Self::ReleaseUseItem),
            7 => Ok(Self::SwapItemWithOffhand),
            8 => Ok(Self::Stab),
            other => Err(BufferError::InvalidEnum(other)),
        }
    }

    pub fn id(self) -> i32 {
        self as i32
    }
}

/// Custom Payload: a plugin message to the client on `channel`.
#[derive(Packet)]
#[packet(id = Id::CustomPayload, encode_only)]
pub struct CustomPayload<'a> {
    pub channel: &'a str,
    #[packet(rest)]
    pub data: &'a [u8],
}

pub struct TabList<'a> {
    pub header: &'a TextComponent,
    pub footer: &'a TextComponent,
}

impl Encode for TabList<'_> {
    const ID: i32 = Id::TabList.id();

    fn encode(&self, w: &mut PacketWriter) {
        use crate::protocol::codec::WriteExt;
        w.write_text(self.header);
        w.write_text(self.footer);
    }
}

#[cfg(test)]
mod death_location_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn respawn_writes_the_last_death_location() {
        let encode = |death| {
            let mut w = PacketWriter::new();
            Respawn {
                dimension_type: 0,
                dimension: "a",
                game_mode: GameMode::Survival,
                previous_game_mode: None,
                sea_level: 63,
                data_kept: 0,
                last_death: death,
                is_debug: false,
                is_flat: false,
                portal_cooldown: 0,
            }
            .encode(&mut w);
            w.into_bytes().len()
        };
        let without = encode(None);
        let with = encode(Some(("minecraft:overworld", BlockPos::new(1, 2, 3))));
        assert_eq!(with - without, 20 + 8);
    }
}

#[cfg(test)]
mod position_tests {
    use super::*;
    use crate::protocol::encode_packet;

    #[tuff_core::watchdog]
    #[test]
    fn relative_position_carries_delta_and_mask() {
        let bytes = encode_packet(&PlayerPositionDelta {
            teleport_id: 2,
            position: [0.0; 3],
            delta: [1.0, 2.0, 3.0],
            yaw: 0.0,
            pitch: 0.0,
            relative: position_relative::X | position_relative::DELTA_Y,
        })
        .into_bytes();
        let absolute = encode_packet(&PlayerPosition {
            teleport_id: 2,
            position: [0.0; 3],
            yaw: 0.0,
            pitch: 0.0,
        })
        .into_bytes();
        assert_eq!(bytes.len(), absolute.len());
        assert_eq!(&bytes[bytes.len() - 4..], &[0, 0, 0, 0x41]);
        assert_eq!(&absolute[absolute.len() - 4..], &[0, 0, 0, 0]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn rotation_packet_is_two_floats_and_two_flags() {
        let bytes = encode_packet(&PlayerRotation {
            yaw: 90.0,
            relative_yaw: true,
            pitch: 0.0,
            relative_pitch: false,
        })
        .into_bytes();
        assert_eq!(
            &bytes[bytes.len() - 10..],
            &[0x42, 0xb4, 0, 0, 1, 0, 0, 0, 0, 0]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn look_at_packet_writes_anchor_point_and_optional_entity() {
        let point = encode_packet(&PlayerLookAt {
            from_eyes: true,
            position: [1.0, 2.0, 3.0],
            entity: None,
        })
        .into_bytes();
        assert_eq!(point[point.len() - 26], 1);
        assert_eq!(point[point.len() - 1], 0);
        let entity = encode_packet(&PlayerLookAt {
            from_eyes: false,
            position: [0.0; 3],
            entity: Some((5, true)),
        })
        .into_bytes();
        assert_eq!(&entity[entity.len() - 3..], &[1, 5, 1]);
    }
}
