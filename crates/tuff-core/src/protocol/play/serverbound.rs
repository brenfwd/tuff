use bytes::Bytes;
use tuff_vanilla_data::packet::play::Serverbound as ServerId;

use super::*;
use crate::protocol::codec::{CreativeSlot, HashedSlot, ReadExt, WriteExt};
use crate::protocol::configuration::ClientInformation;
use crate::world::player::Difficulty;
use tuff_wire::{BufferError, PacketReader, PacketWriter};
use tuff_world_model::{BlockPos, Face};

const MAX_GAME_RULE_EDITS: i32 = 256;
const MAX_GAME_RULE_VALUE: usize = 32_767;
const MAX_ARGUMENT_SIGNATURES: i32 = 8;
const MAX_ARGUMENT_NAME: usize = 16;
const MESSAGE_SIGNATURE_BYTES: usize = 256;
use crate::protocol::chat_session::{LastSeenUpdate, Signature};
const MAX_CHAT_LENGTH: usize = 256;
pub const MAX_CUSTOM_PAYLOAD: usize = 32767;
const MAX_COMMAND_BLOCK_COMMAND: usize = 32767;
const SIGN_LINES: usize = 4;
const MAX_SIGN_LINE: usize = 384;
const MAX_BOOK_PACKET_PAGES: i32 = 200;
const MAX_BOOK_PACKET_PAGE: usize = 8192;
/// UTF-16 code units in a Command Suggestions Request.
const MAX_SUGGESTION_TEXT: usize = 32_500;
const MAX_CHANGED_SLOTS: i32 = 128;
const MAX_IDENTIFIER_BYTES: usize = 32_767 * 3;
const MAX_DEBUG_SUBSCRIPTIONS: i32 = 32;

/// What the jigsaw block editor sends.
#[derive(Debug, Clone, PartialEq)]
pub struct JigsawEdit {
    pub position: BlockPos,
    pub name: String,
    pub target: String,
    pub pool: String,
    pub final_state: String,
    /// `rollable` or `aligned`.
    pub joint: &'static str,
    pub selection_priority: i32,
    pub placement_priority: i32,
}

/// What the structure block editor sends; the enums are their ordinals.
#[derive(Debug, Clone, PartialEq)]
pub struct StructureBlockEdit {
    pub position: BlockPos,
    /// 0 update data, 1 save area, 2 load area, 3 scan area.
    pub update: i32,
    /// 0 save, 1 load, 2 corner, 3 data.
    pub mode: i32,
    pub name: String,
    pub offset: [i32; 3],
    pub size: [i32; 3],
    /// 0 none, 1 left-right, 2 front-back.
    pub mirror: i32,
    /// Quarter turns clockwise.
    pub rotation: i32,
    pub data: String,
    pub integrity: f32,
    pub seed: i64,
    pub ignore_entities: bool,
    pub show_air: bool,
    pub show_bounding_box: bool,
    pub strict: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Serverbound {
    /// Confirm Teleportation: the teleport id, and where the client put
    /// itself on applying it.
    AcceptTeleportation {
        id: i32,
        position: [f64; 3],
        rotation: (f32, f32),
    },
    Attack(i32),
    /// Spectator Action: the entity a spectator wants to view through.
    Spectate(Option<i32>),
    /// The spectator menu's teleport to a player, by UUID.
    TeleportToEntity(u128),
    /// A right click on an entity.
    Interact {
        entity: i32,
        off_hand: bool,
        /// Where on the entity, relative to its position.
        location: [f64; 3],
        sneaking: bool,
    },
    ChangeGameMode(GameMode),
    ChangeDifficulty(Difficulty),
    LockDifficulty(bool),
    /// The vehicle the client drives moved.
    MoveVehicle {
        position: [f64; 3],
        yaw: f32,
        pitch: f32,
        on_ground: bool,
    },
    /// Which boat paddles turn.
    PaddleBoat {
        left: bool,
        right: bool,
    },
    Chat(String),
    ChatCommand(String),
    CookieResponse(crate::protocol::cookie::CookieResponse),
    CustomClickAction(crate::protocol::custom::CustomClickAction),
    /// Game rule edits as `(rule id, serialized value)`, applied only for gamemasters.
    SetGameRule(Vec<(String, String)>),
    /// Command Suggestions Request: the text before the cursor, with its
    /// leading `/`.
    CommandSuggestion {
        id: i32,
        text: String,
    },
    ClientCommand(i32),
    /// Custom Payload: a plugin message on `channel`.
    CustomPayload {
        channel: String,
        data: Bytes,
    },
    ClientInformation(ClientInformation),
    ContainerClick {
        window_id: i32,
        state_id: i32,
        slot: i16,
        button: i8,
        mode: i32,
        changed: Vec<(i16, Option<HashedSlot>)>,
        carried: Option<HashedSlot>,
    },
    ContainerClose(i32),
    /// Click Container Button: a stonecutter recipe choice and the like.
    ContainerButtonClick {
        window_id: i32,
        button: i32,
    },
    /// Set Slot State: a crafter slot switched on or off.
    ContainerSlotState {
        slot: i32,
        window_id: i32,
        enabled: bool,
    },
    /// Select Trade: the offer chosen on a merchant screen.
    SelectTrade(i32),
    /// Set Beacon: the effects chosen on a beacon screen, as mob effect
    /// registry ids.
    SetBeacon {
        primary: Option<i32>,
        secondary: Option<i32>,
    },
    /// Select Bundle Item: which stack of the bundle in a window slot
    /// comes out next.
    SelectBundleItem {
        slot: i32,
        index: i32,
    },
    /// Place Recipe: fill the grid from the recipe book.
    PlaceRecipe {
        window_id: i32,
        recipe: i32,
        use_max_items: bool,
    },
    /// Set Seen Recipe: the highlight on a new recipe goes away.
    RecipeBookSeenRecipe(i32),
    RecipeBookChangeSettings {
        book: i32,
        open: bool,
        filtering: bool,
    },
    /// Seen Advancements: the client opened a tab (`Some`) or closed the
    /// advancement screen (`None`).
    SeenAdvancements(Option<String>),
    KeepAlive(i64),
    Pong(i32),
    Move(Movement),
    /// Player Abilities: whether the client says it is flying.
    PlayerAbilities {
        flying: bool,
    },
    PlayerAction {
        status: PlayerActionStatus,
        position: BlockPos,
        face: i8,
        sequence: i32,
    },
    PlayerCommand(i32),
    /// Releasing a charged jump while riding, with its charge (0 to 100).
    RidingJump(i32),
    PlayerInput(u8),
    Punch,
    SetCarriedItem(i16),
    SetCreativeModeSlot {
        slot: i16,
        stack: CreativeSlot,
    },
    UseItemOn {
        hand: i32,
        position: BlockPos,
        face: Face,
        /// Where on the block the cursor was, each axis in 0..=1.
        cursor: [f32; 3],
        sequence: i32,
    },
    UseItem {
        off_hand: bool,
        sequence: i32,
    },
    /// Pick Item From Block (middle click).
    PickItemFromBlock {
        position: BlockPos,
        include_data: bool,
    },
    /// Pick Item From Entity (middle click on an entity).
    PickItemFromEntity {
        entity: i32,
        include_data: bool,
    },
    /// Block Entity Tag Query: a gamemaster asks for a block entity's NBT.
    BlockEntityTagQuery {
        transaction: i32,
        position: BlockPos,
    },
    /// Entity Tag Query: a gamemaster asks for an entity's NBT.
    EntityTagQuery {
        transaction: i32,
        entity: i32,
    },
    /// Edit Book: the pages (and the title when signing) of a book in a
    /// hotbar slot or the offhand (slot 40).
    EditBook {
        slot: i32,
        pages: Vec<String>,
        title: Option<String>,
    },
    /// Rename Item: the anvil's name field.
    RenameItem(String),
    /// Update Sign: the lines the sign editor was closed with.
    SignUpdate {
        position: BlockPos,
        front: bool,
        lines: Vec<String>,
    },
    /// Set Command Block: the command block editor's Done.
    SetCommandBlock {
        position: BlockPos,
        command: String,
        mode: i32,
        track_output: bool,
        conditional: bool,
        automatic: bool,
    },
    /// Jigsaw Generate: the jigsaw block editor's Generate button.
    JigsawGenerate {
        position: BlockPos,
        levels: i32,
        keep_jigsaws: bool,
    },
    /// Set Jigsaw Block: the jigsaw block editor's Done.
    SetJigsawBlock(Box<JigsawEdit>),
    /// Set Test Block: the test block editor's Done; `mode` is the ordinal
    /// of start, log, fail, accept.
    SetTestBlock {
        position: BlockPos,
        mode: i32,
        message: String,
    },
    /// Set Structure Block: the structure block editor's Done, Save, Load
    /// and Detect.
    SetStructureBlock(Box<StructureBlockEdit>),
    /// Set Command Minecart: the minecart command editor's Done.
    SetCommandMinecart {
        entity: i32,
        command: String,
        track_output: bool,
    },
    /// Chunks per tick the client wants.
    ChunkBatchReceived(f32),
    /// The client finished a tick.
    ClientTickEnd,
    /// A resource pack status; `None` for a status outside the protocol.
    ResourcePack {
        id: u128,
        status: Option<crate::protocol::pack::PackStatus>,
    },
    /// Debug Subscription Request: the `debug_subscription` registry ids
    /// an operator's client wants debug data for.
    DebugSubscriptionRequest(Vec<i32>),
    /// Player Loaded: the client has the terrain around it and will move.
    PlayerLoaded,
    /// Acknowledge Configuration: the client has left play after Start
    /// Configuration; what follows is configuration traffic.
    ConfigurationAcknowledged,
    /// Chat Session Update: the key the player's chat messages are signed with.
    ChatSessionUpdate(crate::protocol::chat_session::ChatSession),
    /// Chat: a message with the signature and acknowledgements it came with.
    SignedChat(Box<SignedChat>),
    /// Signed Chat Command: a command with its argument signatures.
    SignedCommand(Box<SignedCommand>),
    /// Chat Acknowledgement: how many tracked messages scrolled out of the
    /// client's window.
    ChatAck(i32),
    /// Packets the server accepts but has no use for.
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedChat {
    pub message: String,
    pub timestamp_ms: i64,
    pub salt: i64,
    pub signature: Option<Signature>,
    pub update: LastSeenUpdate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedCommand {
    pub command: String,
    pub timestamp_ms: i64,
    pub salt: i64,
    pub signatures: Vec<(String, Signature)>,
    pub update: LastSeenUpdate,
}

/// Reads and bounds the part of a signed chat command after the command text:
/// timestamp, salt, up to 8 named 256-byte argument signatures and the
/// last-seen acknowledgement.
fn read_signed_command(
    r: &mut PacketReader,
    command: String,
) -> Result<SignedCommand, tuff_wire::BufferError> {
    let timestamp_ms = r.read_long()?;
    let salt = r.read_long()?;
    let count = r.read_var_int()?;
    if !(0..=MAX_ARGUMENT_SIGNATURES).contains(&count) {
        return Err(tuff_wire::BufferError::InvalidArrayLength(count));
    }
    let mut signatures = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let name = r.read_string_chars(MAX_ARGUMENT_NAME)?;
        signatures.push((name, read_signature(r)?));
    }
    let update = LastSeenUpdate::decode(r)?;
    if r.remaining() != 0 {
        return Err(tuff_wire::BufferError::InvalidString);
    }
    Ok(SignedCommand {
        command,
        timestamp_ms,
        salt,
        signatures,
        update,
    })
}

fn read_signature(r: &mut PacketReader) -> Result<Signature, tuff_wire::BufferError> {
    let bytes = r.read_bytes(MESSAGE_SIGNATURE_BYTES)?;
    Ok(bytes[..]
        .try_into()
        .expect("read_bytes returns the length asked"))
}

fn read_signed_chat(
    r: &mut PacketReader,
    message: String,
) -> Result<SignedChat, tuff_wire::BufferError> {
    let timestamp_ms = r.read_long()?;
    let salt = r.read_long()?;
    let signature = if r.read_bool()? {
        Some(read_signature(r)?)
    } else {
        None
    };
    let update = LastSeenUpdate::decode(r)?;
    if r.remaining() != 0 {
        return Err(tuff_wire::BufferError::InvalidString);
    }
    Ok(SignedChat {
        message,
        timestamp_ms,
        salt,
        signature,
        update,
    })
}

impl Serverbound {
    pub fn decode(payload: Bytes) -> anyhow::Result<Self> {
        let payload_copy = payload.clone();
        let mut r = PacketReader::new(payload);
        let id = r.read_var_int()?;
        let Some(kind) = ServerId::from_id(id) else {
            anyhow::bail!("unknown play packet {id:#04x}");
        };
        let _span = tuff_timings::span!("net.decode", tuff_timings::Tag::Name(kind.name()));
        let flags_to_ground = |flags: i8| flags & 0x01 != 0;
        let flags_to_collision = |flags: i8| flags & 0x02 != 0;
        let packet = match kind {
            ServerId::AcceptTeleportation => {
                let id = r.read_var_int()?;
                let position = [r.read_double()?, r.read_double()?, r.read_double()?];
                let rotation = (r.read_float()?, r.read_float()?);
                Self::AcceptTeleportation {
                    id,
                    position,
                    rotation,
                }
            }
            ServerId::Attack => Self::Attack(r.read_var_int()?),
            ServerId::SpectatorAction => Self::Spectate(match r.read_var_int()? {
                0 => None,
                n => Some(n.wrapping_sub(1)),
            }),
            ServerId::TeleportToEntity => Self::TeleportToEntity(r.read_uuid()?),
            ServerId::MoveVehicle => Self::MoveVehicle {
                position: [r.read_double()?, r.read_double()?, r.read_double()?],
                yaw: r.read_float()?,
                pitch: r.read_float()?,
                on_ground: r.read_bool()?,
            },
            ServerId::PaddleBoat => Self::PaddleBoat {
                left: r.read_bool()?,
                right: r.read_bool()?,
            },
            ServerId::Interact => Self::Interact {
                entity: r.read_var_int()?,
                off_hand: r.read_var_int()? == 1,
                location: r.read_lp_vec3()?,
                sneaking: r.read_bool()?,
            },
            ServerId::ChangeGameMode => {
                let id = r.read_var_int()?;
                Self::ChangeGameMode(GameMode::from_id(id).ok_or(BufferError::InvalidEnum(id))?)
            }
            ServerId::ChangeDifficulty => {
                let id = r.read_var_int()?;
                Self::ChangeDifficulty(
                    u8::try_from(id)
                        .ok()
                        .and_then(Difficulty::from_id)
                        .ok_or(BufferError::InvalidEnum(id))?,
                )
            }
            ServerId::LockDifficulty => Self::LockDifficulty(r.read_bool()?),
            ServerId::Chat => {
                let message = r.read_string_chars(MAX_CHAT_LENGTH)?;
                return Ok(Self::SignedChat(Box::new(read_signed_chat(
                    &mut r, message,
                )?)));
            }
            ServerId::ChatAck => Self::ChatAck(r.read_var_int()?),
            ServerId::ChatCommand => {
                let command = r.read_string_chars(MAX_CHAT_LENGTH)?;
                return Ok(Self::ChatCommand(command));
            }
            ServerId::ChatCommandSigned => {
                let command = r.read_string_chars(MAX_CHAT_LENGTH)?;
                return Ok(Self::SignedCommand(Box::new(read_signed_command(
                    &mut r, command,
                )?)));
            }
            ServerId::CookieResponse => {
                Self::CookieResponse(crate::protocol::cookie::CookieResponse::decode(&mut r)?)
            }
            ServerId::SetGameRule => {
                let count = r.read_var_int()?;
                anyhow::ensure!(
                    (0..=MAX_GAME_RULE_EDITS).contains(&count),
                    "bad game rule edit count {count}"
                );
                let edits = (0..count)
                    .map(|_| {
                        Ok((
                            r.read_string_with_max(MAX_IDENTIFIER_BYTES)?,
                            r.read_string_chars(MAX_GAME_RULE_VALUE)?,
                        ))
                    })
                    .collect::<Result<_, tuff_wire::BufferError>>()?;
                Self::SetGameRule(edits)
            }
            ServerId::CommandSuggestion => Self::CommandSuggestion {
                id: r.read_var_int()?,
                text: r.read_string_chars(MAX_SUGGESTION_TEXT)?,
            },
            ServerId::ClientCommand => Self::ClientCommand(r.read_var_int()?),
            ServerId::CustomClickAction => {
                Self::CustomClickAction(crate::protocol::custom::CustomClickAction::decode(&mut r)?)
            }
            ServerId::CustomPayload => {
                let channel = r.read_string_with_max(256)?;
                anyhow::ensure!(
                    r.remaining() <= MAX_CUSTOM_PAYLOAD,
                    "custom payload too large"
                );
                let data = r.read_bytes(r.remaining())?;
                Self::CustomPayload { channel, data }
            }
            ServerId::ClientInformation => {
                Self::ClientInformation(ClientInformation::decode(&mut r)?)
            }
            ServerId::ContainerClick => {
                let window_id = r.read_var_int()?;
                let state_id = r.read_var_int()?;
                let slot = r.read_short()?;
                let button = r.read_byte()?;
                let mode = r.read_var_int()?;
                let len = r.read_var_int()?;
                anyhow::ensure!(
                    (0..=MAX_CHANGED_SLOTS).contains(&len),
                    "too many changed slots"
                );
                let mut changed = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    changed.push((r.read_short()?, r.read_hashed_slot()?));
                }
                let carried = r.read_hashed_slot()?;
                Self::ContainerClick {
                    window_id,
                    state_id,
                    slot,
                    button,
                    mode,
                    changed,
                    carried,
                }
            }
            ServerId::ContainerClose => Self::ContainerClose(r.read_var_int()?),
            ServerId::ContainerButtonClick => Self::ContainerButtonClick {
                window_id: r.read_var_int()?,
                button: r.read_var_int()?,
            },
            ServerId::ContainerSlotStateChanged => Self::ContainerSlotState {
                slot: r.read_var_int()?,
                window_id: r.read_var_int()?,
                enabled: r.read_bool()?,
            },
            ServerId::SelectTrade => Self::SelectTrade(r.read_var_int()?),
            ServerId::SetBeacon => Self::SetBeacon {
                primary: if r.read_bool()? {
                    Some(r.read_var_int()?)
                } else {
                    None
                },
                secondary: if r.read_bool()? {
                    Some(r.read_var_int()?)
                } else {
                    None
                },
            },
            ServerId::BundleItemSelected => {
                let slot = r.read_var_int()?;
                let index = r.read_var_int()?;
                if index < -1 {
                    return Err(tuff_wire::BufferError::InvalidArrayLength(index).into());
                }
                Self::SelectBundleItem { slot, index }
            }
            ServerId::PlaceRecipe => Self::PlaceRecipe {
                window_id: r.read_var_int()?,
                recipe: r.read_var_int()?,
                use_max_items: r.read_bool()?,
            },
            ServerId::RecipeBookSeenRecipe => Self::RecipeBookSeenRecipe(r.read_var_int()?),
            ServerId::RecipeBookChangeSettings => Self::RecipeBookChangeSettings {
                book: r.read_var_int()?,
                open: r.read_bool()?,
                filtering: r.read_bool()?,
            },
            ServerId::SeenAdvancements => match r.read_var_int()? {
                0 => Self::SeenAdvancements(Some(r.read_string_with_max(MAX_IDENTIFIER_BYTES)?)),
                1 => Self::SeenAdvancements(None),
                other => anyhow::bail!("unknown seen advancements action {other}"),
            },
            ServerId::KeepAlive => Self::KeepAlive(r.read_long()?),
            ServerId::Pong => Self::Pong(r.read_int()?),
            ServerId::MovePlayerPos => {
                let position = [r.read_double()?, r.read_double()?, r.read_double()?];
                let flags = r.read_byte()?;
                Self::Move(Movement {
                    position: Some(position),
                    rotation: None,
                    on_ground: flags_to_ground(flags),
                    horizontal_collision: flags_to_collision(flags),
                })
            }
            ServerId::MovePlayerPosRot => {
                let position = [r.read_double()?, r.read_double()?, r.read_double()?];
                let rotation = (r.read_float()?, r.read_float()?);
                let flags = r.read_byte()?;
                Self::Move(Movement {
                    position: Some(position),
                    rotation: Some(rotation),
                    on_ground: flags_to_ground(flags),
                    horizontal_collision: flags_to_collision(flags),
                })
            }
            ServerId::MovePlayerRot => {
                let rotation = (r.read_float()?, r.read_float()?);
                let flags = r.read_byte()?;
                Self::Move(Movement {
                    position: None,
                    rotation: Some(rotation),
                    on_ground: flags_to_ground(flags),
                    horizontal_collision: flags_to_collision(flags),
                })
            }
            ServerId::MovePlayerStatusOnly => {
                let flags = r.read_byte()?;
                Self::Move(Movement {
                    position: None,
                    rotation: None,
                    on_ground: flags_to_ground(flags),
                    horizontal_collision: flags_to_collision(flags),
                })
            }
            ServerId::PlayerAbilities => Self::PlayerAbilities {
                flying: r.read_byte()? & abilities::FLYING != 0,
            },
            ServerId::PlayerAction => Self::PlayerAction {
                status: PlayerActionStatus::from_id(r.read_var_int()?)?,
                position: r.read_position()?,
                face: r.read_byte()?,
                sequence: r.read_var_int()?,
            },
            ServerId::PlayerCommand => {
                r.read_var_int()?;
                let action = r.read_var_int()?;
                let data = r.read_var_int()?;
                if action == 3 {
                    Self::RidingJump(data.clamp(0, 100))
                } else {
                    Self::PlayerCommand(action)
                }
            }
            ServerId::PlayerInput => Self::PlayerInput(r.read_unsigned_byte()?),
            ServerId::EditBook => {
                let slot = r.read_var_int()?;
                let count = r.read_var_int()?;
                anyhow::ensure!(
                    (0..=MAX_BOOK_PACKET_PAGES).contains(&count),
                    "bad book page count"
                );
                let pages = (0..count)
                    .map(|_| r.read_string_chars(MAX_BOOK_PACKET_PAGE))
                    .collect::<Result<Vec<_>, _>>()?;
                let title = if r.read_bool()? {
                    Some(r.read_string_chars(MAX_BOOK_PACKET_PAGE)?)
                } else {
                    None
                };
                Self::EditBook { slot, pages, title }
            }
            ServerId::RenameItem => Self::RenameItem(r.read_string_chars(32767)?),
            ServerId::SignUpdate => {
                let position = r.read_position()?;
                let front = r.read_bool()?;
                let mut lines = Vec::with_capacity(SIGN_LINES);
                for _ in 0..SIGN_LINES {
                    let line = r.read_string_chars(MAX_SIGN_LINE)?;
                    anyhow::ensure!(
                        line.encode_utf16().count() <= MAX_SIGN_LINE,
                        "sign line too long"
                    );
                    lines.push(line);
                }
                Self::SignUpdate {
                    position,
                    front,
                    lines,
                }
            }
            ServerId::SetCommandBlock => {
                let position = r.read_position()?;
                let command = r.read_string_chars(MAX_COMMAND_BLOCK_COMMAND)?;
                let mode = r.read_var_int()?;
                let flags = r.read_byte()?;
                Self::SetCommandBlock {
                    position,
                    command,
                    mode,
                    track_output: flags & 1 != 0,
                    conditional: flags & 2 != 0,
                    automatic: flags & 4 != 0,
                }
            }
            ServerId::JigsawGenerate => Self::JigsawGenerate {
                position: r.read_position()?,
                levels: r.read_var_int()?,
                keep_jigsaws: r.read_bool()?,
            },
            ServerId::SetJigsawBlock => Self::SetJigsawBlock(Box::new(JigsawEdit {
                position: r.read_position()?,
                name: r.read_string_with_max(MAX_IDENTIFIER_BYTES)?,
                target: r.read_string_with_max(MAX_IDENTIFIER_BYTES)?,
                pool: r.read_string_with_max(MAX_IDENTIFIER_BYTES)?,
                final_state: r.read_string_with_max(MAX_IDENTIFIER_BYTES)?,
                joint: match r.read_string_with_max(MAX_IDENTIFIER_BYTES)?.as_str() {
                    "rollable" => "rollable",
                    _ => "aligned",
                },
                selection_priority: r.read_var_int()?,
                placement_priority: r.read_var_int()?,
            })),
            ServerId::SetTestBlock => Self::SetTestBlock {
                position: r.read_position()?,
                mode: r.read_var_int()?,
                message: r.read_string_with_max(MAX_IDENTIFIER_BYTES)?,
            },
            ServerId::SetStructureBlock => {
                let position = r.read_position()?;
                let update = r.read_var_int()?;
                let mode = r.read_var_int()?;
                let name = r.read_string_with_max(MAX_IDENTIFIER_BYTES)?;
                let mut triple = |low: i32, high: i32| -> anyhow::Result<[i32; 3]> {
                    Ok([
                        i32::from(r.read_byte()?).clamp(low, high),
                        i32::from(r.read_byte()?).clamp(low, high),
                        i32::from(r.read_byte()?).clamp(low, high),
                    ])
                };
                let offset = triple(-48, 48)?;
                let size = triple(0, 48)?;
                let mirror = r.read_var_int()?;
                let rotation = r.read_var_int()?;
                let data = r.read_string_with_max(128 * 3)?;
                let integrity = r.read_float()?.clamp(0.0, 1.0);
                let seed = r.read_var_long()?;
                let flags = r.read_byte()?;
                Self::SetStructureBlock(Box::new(StructureBlockEdit {
                    position,
                    update,
                    mode,
                    name,
                    offset,
                    size,
                    mirror,
                    rotation,
                    data,
                    integrity,
                    seed,
                    ignore_entities: flags & 1 != 0,
                    show_air: flags & 2 != 0,
                    show_bounding_box: flags & 4 != 0,
                    strict: flags & 8 != 0,
                }))
            }
            ServerId::SetCommandMinecart => Self::SetCommandMinecart {
                entity: r.read_var_int()?,
                command: r.read_string_chars(MAX_COMMAND_BLOCK_COMMAND)?,
                track_output: r.read_bool()?,
            },
            ServerId::Punch => Self::Punch,
            ServerId::SetCarriedItem => Self::SetCarriedItem(r.read_short()?),
            ServerId::SetCreativeModeSlot => {
                let slot = r.read_short()?;
                let stack = match r.read_creative_slot() {
                    Ok(stack) if r.remaining() == 0 => stack,
                    // A component this server decodes wrongly shows up as
                    // an error or leftover bytes here. That is a gap in
                    // this server, not a hostile client: keep the player,
                    // show them their real inventory, and log enough to
                    // fix the codec.
                    result => {
                        log::warn!(
                            "creative slot {slot} could not be decoded ({}); payload: {}",
                            match &result {
                                Ok(_) => format!("{} trailing bytes", r.remaining()),
                                Err(err) => err.to_string(),
                            },
                            hex(&payload_copy)
                        );
                        CreativeSlot::Unsupported
                    }
                };
                let packet = Self::SetCreativeModeSlot { slot, stack };
                if matches!(
                    packet,
                    Self::SetCreativeModeSlot {
                        stack: CreativeSlot::Unsupported,
                        ..
                    }
                ) {
                    return Ok(packet);
                }
                packet
            }
            ServerId::UseItemOn => {
                let hand = r.read_var_int()?;
                let position = r.read_position()?;
                let face = Face::from_id(r.read_var_int()?)?;
                let mut cursor = [0.0; 3];
                for value in &mut cursor {
                    let read = r.read_float()?;
                    *value = if read.is_finite() {
                        read.clamp(0.0, 1.0)
                    } else {
                        0.5
                    };
                }
                r.read_bool()?;
                r.read_bool()?;
                Self::UseItemOn {
                    hand,
                    position,
                    face,
                    cursor,
                    sequence: r.read_var_int()?,
                }
            }
            ServerId::UseItem => {
                let hand = r.read_var_int()?;
                let sequence = r.read_var_int()?;
                r.read_float()?;
                r.read_float()?;
                Self::UseItem {
                    off_hand: hand == 1,
                    sequence,
                }
            }
            ServerId::PickItemFromBlock => Self::PickItemFromBlock {
                position: r.read_position()?,
                include_data: r.read_bool()?,
            },
            ServerId::PickItemFromEntity => Self::PickItemFromEntity {
                entity: r.read_var_int()?,
                include_data: r.read_bool()?,
            },
            ServerId::BlockEntityTagQuery => Self::BlockEntityTagQuery {
                transaction: r.read_var_int()?,
                position: r.read_position()?,
            },
            ServerId::EntityTagQuery => Self::EntityTagQuery {
                transaction: r.read_var_int()?,
                entity: r.read_var_int()?,
            },
            ServerId::ChunkBatchReceived => Self::ChunkBatchReceived(r.read_float()?),
            ServerId::ClientTickEnd => Self::ClientTickEnd,
            ServerId::PlayerLoaded => Self::PlayerLoaded,
            ServerId::ConfigurationAcknowledged => Self::ConfigurationAcknowledged,
            ServerId::DebugSubscriptionRequest => {
                let count = r.read_var_int()?;
                anyhow::ensure!(
                    (0..=MAX_DEBUG_SUBSCRIPTIONS).contains(&count),
                    "bad debug subscription count {count}"
                );
                let ids = (0..count)
                    .map(|_| r.read_var_int())
                    .collect::<Result<_, _>>()?;
                Self::DebugSubscriptionRequest(ids)
            }
            ServerId::ChatSessionUpdate => {
                Self::ChatSessionUpdate(crate::protocol::chat_session::ChatSession::decode(&mut r)?)
            }
            ServerId::ResourcePack => {
                let (id, status) = crate::protocol::pack::decode_response(&mut r)?;
                Self::ResourcePack { id, status }
            }
            _ => return Ok(Self::Ignored),
        };
        anyhow::ensure!(r.remaining() == 0, "trailing bytes in play packet {kind}");
        Ok(packet)
    }

    /// The packet as a client sends it, id first. Only the packets a
    /// scripted client needs are covered; the rest give `None`.
    pub fn encode(&self) -> Option<PacketWriter> {
        let mut w = PacketWriter::new();
        match self {
            Self::AcceptTeleportation {
                id,
                position,
                rotation,
            } => {
                w.write_var_int(ServerId::AcceptTeleportation.id());
                w.write_var_int(*id);
                position.iter().for_each(|v| w.write_double(*v));
                w.write_float(rotation.0);
                w.write_float(rotation.1);
            }
            Self::KeepAlive(id) => {
                w.write_var_int(ServerId::KeepAlive.id());
                w.write_long(*id);
            }
            Self::Pong(id) => {
                w.write_var_int(ServerId::Pong.id());
                w.write_int(*id);
            }
            Self::Move(movement) => {
                let kind = match (movement.position, movement.rotation) {
                    (Some(_), Some(_)) => ServerId::MovePlayerPosRot,
                    (Some(_), None) => ServerId::MovePlayerPos,
                    (None, Some(_)) => ServerId::MovePlayerRot,
                    (None, None) => ServerId::MovePlayerStatusOnly,
                };
                w.write_var_int(kind.id());
                if let Some(position) = movement.position {
                    position.iter().for_each(|v| w.write_double(*v));
                }
                if let Some((yaw, pitch)) = movement.rotation {
                    w.write_float(yaw);
                    w.write_float(pitch);
                }
                w.write_byte(movement.on_ground as i8 | (movement.horizontal_collision as i8) << 1);
            }
            Self::ClientInformation(info) => {
                w.write_var_int(ServerId::ClientInformation.id());
                info.encode(&mut w);
            }
            Self::ChunkBatchReceived(rate) => {
                w.write_var_int(ServerId::ChunkBatchReceived.id());
                w.write_float(*rate);
            }
            Self::ClientTickEnd => w.write_var_int(ServerId::ClientTickEnd.id()),
            Self::PlayerLoaded => w.write_var_int(ServerId::PlayerLoaded.id()),
            Self::ConfigurationAcknowledged => {
                w.write_var_int(ServerId::ConfigurationAcknowledged.id())
            }
            Self::DebugSubscriptionRequest(ids) => {
                w.write_var_int(ServerId::DebugSubscriptionRequest.id());
                w.write_var_int(ids.len() as i32);
                ids.iter().for_each(|id| w.write_var_int(*id));
            }
            Self::Spectate(entity) => {
                w.write_var_int(ServerId::SpectatorAction.id());
                w.write_var_int(entity.map_or(0, |e| e.wrapping_add(1)));
            }
            Self::TeleportToEntity(uuid) => {
                w.write_var_int(ServerId::TeleportToEntity.id());
                w.write_uuid(*uuid);
            }
            Self::Attack(entity) => {
                w.write_var_int(ServerId::Attack.id());
                w.write_var_int(*entity);
            }
            Self::Interact {
                entity,
                off_hand,
                location,
                sneaking,
            } => {
                w.write_var_int(ServerId::Interact.id());
                w.write_var_int(*entity);
                w.write_var_int(i32::from(*off_hand));
                w.write_lp_vec3(*location);
                w.write_bool(*sneaking);
            }
            Self::Punch => w.write_var_int(ServerId::Punch.id()),
            Self::CustomClickAction(action) => {
                w.write_var_int(ServerId::CustomClickAction.id());
                action.encode(&mut w);
            }
            Self::CookieResponse(response) => {
                w.write_var_int(ServerId::CookieResponse.id());
                response.encode(&mut w);
            }
            Self::SetGameRule(edits) => {
                w.write_var_int(ServerId::SetGameRule.id());
                w.write_var_int(edits.len() as i32);
                for (rule, value) in edits {
                    w.write_string(rule);
                    w.write_string(value);
                }
            }
            Self::ChatCommand(command) => {
                w.write_var_int(ServerId::ChatCommand.id());
                w.write_string(command);
            }
            Self::CustomPayload { channel, data } => {
                w.write_var_int(ServerId::CustomPayload.id());
                w.write_string(channel);
                w.write_bytes(data);
            }
            Self::Chat(message) => {
                w.write_var_int(ServerId::Chat.id());
                w.write_string(message);
                w.write_long(0);
                w.write_long(0);
                w.write_bool(false);
                w.write_var_int(0);
                w.write_bytes(&[0; 3]);
                w.write_byte(0);
            }
            Self::ChangeDifficulty(difficulty) => {
                w.write_var_int(ServerId::ChangeDifficulty.id());
                w.write_var_int(i32::from(difficulty.id()));
            }
            Self::SignedChat(chat) => {
                w.write_var_int(ServerId::Chat.id());
                w.write_string(&chat.message);
                w.write_long(chat.timestamp_ms);
                w.write_long(chat.salt);
                w.write_bool(chat.signature.is_some());
                if let Some(signature) = &chat.signature {
                    w.write_bytes(signature);
                }
                chat.update.encode_into(&mut w);
            }
            Self::SignedCommand(command) => {
                w.write_var_int(ServerId::ChatCommandSigned.id());
                w.write_string(&command.command);
                w.write_long(command.timestamp_ms);
                w.write_long(command.salt);
                w.write_var_int(command.signatures.len() as i32);
                for (name, signature) in &command.signatures {
                    w.write_string(name);
                    w.write_bytes(signature);
                }
                command.update.encode_into(&mut w);
            }
            Self::ChatAck(offset) => {
                w.write_var_int(ServerId::ChatAck.id());
                w.write_var_int(*offset);
            }
            Self::LockDifficulty(locked) => {
                w.write_var_int(ServerId::LockDifficulty.id());
                w.write_bool(*locked);
            }
            Self::ClientCommand(action) => {
                w.write_var_int(ServerId::ClientCommand.id());
                w.write_var_int(*action);
            }
            Self::ContainerClick {
                window_id,
                state_id,
                slot,
                button,
                mode,
                changed,
                carried,
            } => {
                w.write_var_int(ServerId::ContainerClick.id());
                w.write_var_int(*window_id);
                w.write_var_int(*state_id);
                w.write_short(*slot);
                w.write_byte(*button);
                w.write_var_int(*mode);
                w.write_var_int(changed.len() as i32);
                for (slot, view) in changed {
                    w.write_short(*slot);
                    write_hashed_slot(&mut w, view.as_ref());
                }
                write_hashed_slot(&mut w, carried.as_ref());
            }
            Self::ContainerClose(window) => {
                w.write_var_int(ServerId::ContainerClose.id());
                w.write_var_int(*window);
            }
            Self::ContainerButtonClick { window_id, button } => {
                w.write_var_int(ServerId::ContainerButtonClick.id());
                w.write_var_int(*window_id);
                w.write_var_int(*button);
            }
            Self::PlayerAction {
                status,
                position,
                face,
                sequence,
            } => {
                w.write_var_int(ServerId::PlayerAction.id());
                w.write_var_int(status.id());
                w.write_position(*position);
                w.write_byte(*face);
                w.write_var_int(*sequence);
            }
            Self::SetCarriedItem(slot) => {
                w.write_var_int(ServerId::SetCarriedItem.id());
                w.write_short(*slot);
            }
            Self::SelectTrade(index) => {
                w.write_var_int(ServerId::SelectTrade.id());
                w.write_var_int(*index);
            }
            Self::PlayerInput(flags) => {
                w.write_var_int(ServerId::PlayerInput.id());
                w.write_unsigned_byte(*flags);
            }
            Self::PlayerCommand(action) => {
                w.write_var_int(ServerId::PlayerCommand.id());
                w.write_var_int(0);
                w.write_var_int(*action);
                w.write_var_int(0);
            }
            Self::UseItemOn {
                hand,
                position,
                face,
                cursor,
                sequence,
            } => {
                w.write_var_int(ServerId::UseItemOn.id());
                w.write_var_int(*hand);
                w.write_position(*position);
                w.write_var_int(face.id());
                cursor.iter().for_each(|v| w.write_float(*v));
                w.write_bool(false);
                w.write_bool(false);
                w.write_var_int(*sequence);
            }
            Self::UseItem { off_hand, sequence } => {
                w.write_var_int(ServerId::UseItem.id());
                w.write_var_int(i32::from(*off_hand));
                w.write_var_int(*sequence);
                w.write_float(0.0);
                w.write_float(0.0);
            }
            Self::ResourcePack {
                id,
                status: Some(status),
            } => {
                return Some(crate::protocol::pack::encode_response_writer(
                    *id,
                    *status,
                    ServerId::ResourcePack.id(),
                ));
            }
            _ => return None,
        }
        Some(w)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_hashed_slot(w: &mut PacketWriter, slot: Option<&HashedSlot>) {
    w.write_bool(slot.is_some());
    let Some(slot) = slot else { return };
    w.write_var_int(slot.item as i32);
    w.write_var_int(slot.count);
    w.write_var_int(slot.added.len() as i32);
    for (kind, hash) in &slot.added {
        w.write_var_int(*kind as i32);
        w.write_int(*hash);
    }
    w.write_var_int(slot.removed.len() as i32);
    for kind in &slot.removed {
        w.write_var_int(*kind as i32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_command(signatures: &[(&str, usize)], trailing: usize) -> Bytes {
        let mut w = PacketWriter::new();
        w.write_var_int(ServerId::ChatCommandSigned.id());
        w.write_string("msg @a hi");
        w.write_long(1_700_000_000_000);
        w.write_long(42);
        w.write_var_int(signatures.len() as i32);
        for (name, len) in signatures {
            w.write_string(name);
            w.write_bytes(&vec![7; *len]);
        }
        w.write_var_int(0);
        w.write_bytes(&[0, 0, 0]);
        w.write_bytes(&[0]);
        w.write_bytes(&vec![0; trailing]);
        w.into_bytes()
    }

    #[tuff_core::watchdog]
    #[test]
    fn signed_command_decodes_with_argument_signatures() {
        let ok = signed_command(&[("message", 256)], 0);
        match Serverbound::decode(ok).unwrap() {
            Serverbound::SignedCommand(command) => {
                assert_eq!(command.command, "msg @a hi");
                assert_eq!(command.signatures, vec![("message".to_owned(), [7; 256])]);
                assert_eq!(command.timestamp_ms, 1_700_000_000_000);
                assert_eq!(command.salt, 42);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn signed_command_rejects_malformed_argument_signatures() {
        assert!(Serverbound::decode(signed_command(&[("message", 255)], 0)).is_err());
        assert!(
            Serverbound::decode(signed_command(&[("a_name_over_sixteen_chars", 256)], 0)).is_err()
        );
        assert!(Serverbound::decode(signed_command(&[("message", 256)], 1)).is_err());
        let nine = vec![("a", 256); 9];
        assert!(Serverbound::decode(signed_command(&nine, 0)).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn signed_chat_and_acks_round_trip() {
        let update = LastSeenUpdate {
            offset: 3,
            acknowledged: 0b101,
            checksum: -7,
        };
        for packet in [
            Serverbound::SignedChat(Box::new(SignedChat {
                message: "hi".into(),
                timestamp_ms: 1_700_000_000_123,
                salt: -5,
                signature: Some([9; 256]),
                update,
            })),
            Serverbound::SignedChat(Box::new(SignedChat {
                message: "unsigned".into(),
                timestamp_ms: 1,
                salt: 2,
                signature: None,
                update,
            })),
            Serverbound::SignedCommand(Box::new(SignedCommand {
                command: "say hi".into(),
                timestamp_ms: 4,
                salt: 5,
                signatures: vec![("message".into(), [1; 256])],
                update,
            })),
            Serverbound::ChatAck(12),
        ] {
            let bytes = packet.encode().unwrap().into_bytes();
            assert_eq!(Serverbound::decode(bytes).unwrap(), packet);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn signed_chat_rejects_short_signatures_and_trailing_bytes() {
        let build = |signature: usize, trailing: usize| {
            let mut w = PacketWriter::new();
            w.write_var_int(ServerId::Chat.id());
            w.write_string("hi");
            w.write_long(0);
            w.write_long(0);
            w.write_bool(true);
            w.write_bytes(&vec![0; signature]);
            w.write_var_int(0);
            w.write_bytes(&[0, 0, 0]);
            w.write_byte(0);
            w.write_bytes(&vec![0; trailing]);
            w.into_bytes()
        };
        assert!(Serverbound::decode(build(256, 0)).is_ok());
        assert!(Serverbound::decode(build(255, 0)).is_err());
        assert!(Serverbound::decode(build(256, 1)).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn set_game_rule_round_trips_and_bounds_edit_count() {
        let packet = Serverbound::SetGameRule(vec![
            ("minecraft:advance_time".into(), "false".into()),
            ("minecraft:random_tick_speed".into(), "7".into()),
        ]);
        let bytes = packet.encode().unwrap().into_bytes();
        assert_eq!(Serverbound::decode(bytes).unwrap(), packet);

        let mut w = PacketWriter::new();
        w.write_var_int(ServerId::SetGameRule.id());
        w.write_var_int(MAX_GAME_RULE_EDITS + 1);
        assert!(Serverbound::decode(w.into_bytes()).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn play_cookie_response_round_trips() {
        let packet = Serverbound::CookieResponse(crate::protocol::cookie::CookieResponse {
            key: "tuff:session".into(),
            payload: Some(Bytes::from_static(&[1, 2, 3])),
        });
        let bytes = packet.encode().unwrap().into_bytes();
        assert_eq!(Serverbound::decode(bytes).unwrap(), packet);
    }

    #[tuff_core::watchdog]
    #[test]
    fn spectator_action_uses_optional_var_int() {
        let mut w = Vec::new();
        for entity in [Some(100), Some(0), None] {
            let packet = Serverbound::Spectate(entity);
            w.push(packet.encode().unwrap().into_bytes());
        }
        let id = ServerId::SpectatorAction.id() as u8;
        assert_eq!(&w[0][..], &[id, 101]);
        assert_eq!(&w[1][..], &[id, 1]);
        assert_eq!(&w[2][..], &[id, 0]);
        let wire = [id, 0xc9, 0x01];
        assert_eq!(
            Serverbound::decode(bytes::Bytes::copy_from_slice(&wire)).unwrap(),
            Serverbound::Spectate(Some(200))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn jigsaw_generate_decodes() {
        let id = ServerId::JigsawGenerate.id() as u8;
        let position = BlockPos::new(3, 70, -4);
        let mut wire = vec![id];
        wire.extend(position.pack().to_be_bytes());
        wire.extend([7, 1]);
        assert_eq!(
            Serverbound::decode(bytes::Bytes::from(wire)).unwrap(),
            Serverbound::JigsawGenerate {
                position,
                levels: 7,
                keep_jigsaws: true
            }
        );
    }

    fn utf(wire: &mut Vec<u8>, text: &str) {
        wire.push(text.len() as u8);
        wire.extend(text.as_bytes());
    }

    #[tuff_core::watchdog]
    #[test]
    fn jigsaw_and_test_block_edits_decode() {
        let position = BlockPos::new(3, 70, -4);
        let mut wire = vec![ServerId::SetJigsawBlock.id() as u8];
        wire.extend(position.pack().to_be_bytes());
        for text in ["a:b", "c:d", "minecraft:empty", "minecraft:air", "rollable"] {
            utf(&mut wire, text);
        }
        wire.extend([2, 5]);
        assert_eq!(
            Serverbound::decode(bytes::Bytes::from(wire)).unwrap(),
            Serverbound::SetJigsawBlock(Box::new(JigsawEdit {
                position,
                name: "a:b".to_owned(),
                target: "c:d".to_owned(),
                pool: "minecraft:empty".to_owned(),
                final_state: "minecraft:air".to_owned(),
                joint: "rollable",
                selection_priority: 2,
                placement_priority: 5,
            }))
        );

        let mut wire = vec![ServerId::SetTestBlock.id() as u8];
        wire.extend(position.pack().to_be_bytes());
        wire.push(2);
        utf(&mut wire, "hello");
        assert_eq!(
            Serverbound::decode(bytes::Bytes::from(wire)).unwrap(),
            Serverbound::SetTestBlock {
                position,
                mode: 2,
                message: "hello".to_owned()
            }
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn tag_queries_decode() {
        let id = ServerId::EntityTagQuery.id() as u8;
        assert_eq!(
            Serverbound::decode(bytes::Bytes::copy_from_slice(&[id, 5, 9])).unwrap(),
            Serverbound::EntityTagQuery {
                transaction: 5,
                entity: 9
            }
        );
        let id = ServerId::BlockEntityTagQuery.id() as u8;
        let mut wire = vec![id, 3];
        wire.extend(0u64.to_be_bytes());
        assert!(matches!(
            Serverbound::decode(bytes::Bytes::from(wire)).unwrap(),
            Serverbound::BlockEntityTagQuery { transaction: 3, .. }
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn client_encoding_decodes_back() {
        let moves = [
            (Some([1.5, 64.0, -3.25]), Some((90.0, -10.0))),
            (Some([1.5, 64.0, -3.25]), None),
            (None, Some((90.0, -10.0))),
            (None, None),
        ];
        let mut packets: Vec<Serverbound> = moves
            .into_iter()
            .map(|(position, rotation)| {
                Serverbound::Move(Movement {
                    position,
                    rotation,
                    on_ground: true,
                    horizontal_collision: false,
                })
            })
            .collect();
        packets.extend([
            Serverbound::AcceptTeleportation {
                id: 3,
                position: [1.5, -60.0, 8.25],
                rotation: (45.0, -5.0),
            },
            Serverbound::KeepAlive(42),
            Serverbound::Pong(-5),
            Serverbound::ClientInformation(ClientInformation::default()),
            Serverbound::ChunkBatchReceived(25.0),
            Serverbound::ClientTickEnd,
            Serverbound::Attack(17),
            Serverbound::Spectate(Some(5)),
            Serverbound::Spectate(None),
            Serverbound::TeleportToEntity(0x0123_4567_89ab_cdef_0011_2233_4455_6677),
            Serverbound::Punch,
            Serverbound::ChatCommand("time set day".into()),
            Serverbound::ClientCommand(0),
            Serverbound::ContainerClose(3),
            Serverbound::PlayerAction {
                status: PlayerActionStatus::StopDestroyBlock,
                position: BlockPos::new(-5, 70, 12),
                face: 1,
                sequence: 9,
            },
            Serverbound::PlayerCommand(1),
            Serverbound::SelectTrade(2),
            Serverbound::PlayerInput(0x21),
            Serverbound::SetCarriedItem(4),
            Serverbound::UseItemOn {
                hand: 0,
                position: BlockPos::new(1, -3, 2),
                face: Face::East,
                cursor: [0.5, 1.0, 0.25],
                sequence: 4,
            },
            Serverbound::UseItem {
                off_hand: true,
                sequence: 5,
            },
            Serverbound::DebugSubscriptionRequest(vec![0, 7]),
            Serverbound::DebugSubscriptionRequest(Vec::new()),
            Serverbound::PlayerLoaded,
        ]);
        for packet in packets {
            let bytes = packet.encode().unwrap().into_bytes();
            assert_eq!(Serverbound::decode(bytes).unwrap(), packet);
        }
        assert!(Serverbound::RenameItem(String::new()).encode().is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn debug_subscription_count_is_bounded() {
        let mut w = PacketWriter::new();
        w.write_var_int(ServerId::DebugSubscriptionRequest.id());
        w.write_var_int(MAX_DEBUG_SUBSCRIPTIONS + 1);
        assert!(Serverbound::decode(w.into_bytes()).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn bundle_selection_below_minus_one_is_rejected() {
        for (index, ok) in [(-1, true), (0, true), (-2, false)] {
            let mut w = PacketWriter::new();
            w.write_var_int(ServerId::BundleItemSelected.id());
            w.write_var_int(3);
            w.write_var_int(index);
            assert_eq!(Serverbound::decode(w.into_bytes()).is_ok(), ok);
        }
    }
}
