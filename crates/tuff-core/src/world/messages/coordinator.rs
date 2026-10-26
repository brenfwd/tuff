use std::net::IpAddr;
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use crate::protocol::play::{PlayerInfoEntry, Serverbound};
use crate::storage::level::Spawn;
use crate::world::clock::ClockAction;
use crate::world::clock::ClockId;
use crate::world::dimension::DimensionId;
use crate::world::game_rules::RuleValue;
use crate::world::player::Difficulty;
use crate::world::tick_rate::TickAction;
use crate::world::weather::WeatherKind;
use tuff_item::text::TextComponent;
use tuff_storage::admin::NameAndId;
use tuff_storage::admin::PermissionLevel;

use super::*;

#[derive(Debug)]
pub enum CoordinatorMessage {
    /// Sets (or with `None` removes) the server list icon and saves it.
    SetServerIcon {
        png: Option<Vec<u8>>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// A level event delivered to every player in every dimension: those
    /// in `dimension` get its position clamped to at most 32 blocks away,
    /// the others get their own position.
    GlobalLevelEvent {
        dimension: crate::world::dimension::DimensionId,
        event: i32,
        pos: tuff_world_model::BlockPos,
        data: i32,
    },
    /// Whether a player may log in (bans, whitelist, player limit), and
    /// the permission level they get.
    CanLogin {
        user: NameAndId,
        address: IpAddr,
        reply: oneshot::Sender<Result<PermissionLevel, TextComponent>>,
    },
    /// Admits a connectionless fake player, skipping the login gates.
    SpawnFake {
        owner: String,
        spec: crate::world::fake::FakeSpec,
        reply: oneshot::Sender<Result<crate::world::fake::FakeHandle, String>>,
    },
    /// Removes the fake with this uuid, or every fake of `owner` when the
    /// uuid is absent; replies with how many left.
    RemoveFake {
        owner: String,
        uuid: Option<u128>,
        reply: oneshot::Sender<usize>,
    },
    Join {
        profile: crate::auth::GameProfile,
        info: crate::protocol::configuration::ClientInformation,
        /// The client's self-reported brand, if it sent one.
        brand: Option<String>,
        address: IpAddr,
        outbound: mpsc::UnboundedSender<Bytes>,
        /// Lets commands move the connection between play and configuration.
        phase: mpsc::UnboundedSender<PhaseRequest>,
        reply: oneshot::Sender<Result<Joined, TextComponent>>,
    },
    /// Passes `request` to the connection of the player with this uuid, in
    /// play or held in configuration; dropped if it has none.
    Phase {
        player: u128,
        request: PhaseRequest,
    },
    /// The player's Login packet has been sent; it can now receive the player
    /// list and be announced. `written` fires once every client has been sent
    /// its tab list entry: clients ignore a player entity spawned before that,
    /// so the player must not appear in the world until then.
    Spawned {
        player: PlayerId,
        entry: PlayerInfoEntry,
        listed: bool,
        written: oneshot::Sender<()>,
    },
    Info {
        player: PlayerId,
        actions: u8,
        entry: PlayerInfoEntry,
        listed: bool,
    },
    Chat {
        player: PlayerId,
        message: String,
        /// Who else sees the text, after the text filter had its say.
        audience: ChatAudience,
    },
    Announce(TextComponent),
    /// A system message to one online player, dropped if they left.
    Tell {
        uuid: u128,
        message: TextComponent,
    },
    /// An all-or-nothing inventory edit of a player by uuid, online or
    /// not; replies whether it applied (false for a player never seen).
    EditInventory {
        uuid: u128,
        edit: crate::world::inventory_edit::InventoryEdit,
        reply: oneshot::Sender<bool>,
    },
    /// A feature call's inventory edits for one player, applied in order;
    /// dropped if the player left.
    PluginInventory {
        uuid: u128,
        ops: Vec<crate::world::session::plugin_inventory::InventoryOp>,
    },
    /// Meets or un-meets criteria of an advancement for an online player
    /// as a feature asked, dropped if they left or the advancement is
    /// unknown.
    PluginAdvancement {
        uuid: u128,
        grant: bool,
        advancement: String,
        criterion: Option<String>,
    },
    /// Shows an online player title text as a feature asked, dropped if
    /// they left.
    PluginTitle {
        uuid: u128,
        action: crate::world::messages::TitleAction,
    },
    /// Applies one directive to an online player as a feature asked,
    /// dropped if they left.
    PluginDirective {
        uuid: u128,
        directive: Box<crate::world::messages::Directive>,
    },
    /// Moves an online player as a feature asked, dropped if they left.
    TeleportPlayer {
        uuid: u128,
        teleport: crate::world::messages::Teleport,
    },
    /// The non-player entities currently transmitting on locator bars.
    WaypointTransmitters(Vec<crate::world::messages::WaypointTransmitter>),
    /// A player's death message, to those its team lets hear it.
    AnnounceDeath {
        victim: String,
        message: TextComponent,
    },
    /// A tamed animal's death message, for its owner if online. The killer
    /// is named by `killer`, or by the online player `killer_player`.
    PetDied {
        owner: u128,
        victim: TextComponent,
        kind: &'static tuff_vanilla_data::damage_type::DamageType,
        killer: Option<TextComponent>,
        killer_player: Option<u128>,
    },
    /// Runs a `/time` subcommand on a world clock.
    Clock {
        clock: ClockId,
        action: ClockAction,
        reply: oneshot::Sender<ClockOutcome>,
    },
    /// Runs a `/tick` subcommand. A sprint's report goes to
    /// `sprint_report` once the sprint ends, however it ends; a sprint
    /// replaced by another drops it unsent.
    Tick {
        action: TickAction,
        sprint_report: Option<oneshot::Sender<crate::world::tick_rate::SprintReport>>,
        reply: oneshot::Sender<TickOutcome>,
    },
    /// The player's requested `debug_subscription` set, as a
    /// [`crate::world::debug_feeds::mask`].
    DebugSubscriptions {
        player: PlayerId,
        mask: u32,
    },
    /// Sent after the player's data has been saved.
    Left {
        player: PlayerId,
    },
    /// A session's current state, for selectors and commands.
    PlayerState {
        player: PlayerId,
        state: PlayerState,
    },
    Players {
        reply: oneshot::Sender<Vec<PlayerSnapshot>>,
    },
    /// Role attributes changed: resend the player-list names of the
    /// online players they cover.
    RoleNames(crate::permission::Affected),
    /// Online players and the stored lists, for the web panel.
    Roster {
        reply: oneshot::Sender<Roster>,
    },
    /// The login profile of an online player.
    ProfileOf {
        player: PlayerId,
        reply: oneshot::Sender<Option<crate::auth::GameProfile>>,
    },
    /// The operator level `uuid` has right now (`All` if not an op).
    PermissionOf {
        uuid: u128,
        reply: oneshot::Sender<PermissionLevel>,
    },
    /// The profile of a player by name: one who has logged in before, or
    /// in online mode one the profile API knows.
    LookupProfile {
        name: String,
        reply: oneshot::Sender<Option<NameAndId>>,
    },
    /// The full profile, properties included, of a player by name or id:
    /// an online player's own, or in online mode the one the profile API
    /// knows.
    FetchProfile {
        query: ProfileQuery,
        reply: oneshot::Sender<Option<crate::auth::GameProfile>>,
    },
    /// A profile the profile API resolved, remembered like a login.
    ProfileFound(NameAndId),
    /// Answered once every message sent before it has been handled, and
    /// every configuration change before it saved, so the console knows its
    /// feedback is complete.
    Sync {
        reply: oneshot::Sender<()>,
    },
    /// The config actor has handled everything before a [`Self::Sync`].
    ConfigSynced {
        reply: oneshot::Sender<()>,
    },
    /// The whitelist as the config store now holds it, for
    /// `/whitelist reload`.
    WhitelistReloaded {
        whitelist: std::io::Result<Vec<NameAndId>>,
        reply: oneshot::Sender<ListOutcome>,
    },
    /// A command's success, relayed to operators as `chat.type.admin` and
    /// logged, as the game rules allow.
    AdminBroadcast {
        source: FeedbackTarget,
        source_name: TextComponent,
        message: TextComponent,
    },
    /// A line for the online players holding permission node `node`
    /// except `muted`, whatever the game rules (anti-cheat alerts).
    OperatorAlert {
        line: TextComponent,
        node: &'static str,
        muted: std::sync::Arc<std::collections::HashSet<u128>>,
    },
    /// A chat line formatted client-side with a chat type (`/say`, `/me`).
    ChatBroadcast {
        chat_type: &'static str,
        sender: TextComponent,
        message: TextComponent,
    },
    /// Every server.properties entry, after setting `change` (key, value)
    /// when given (saved, and applied live where the server models it).
    ServerProperties {
        change: Option<(String, String)>,
        reply: oneshot::Sender<std::collections::BTreeMap<String, String>>,
    },
    /// Queries (`None`) or sets a game rule.
    GameRule {
        name: String,
        value: Option<RuleValue>,
        reply: oneshot::Sender<GameRuleOutcome>,
    },
    Weather {
        kind: WeatherKind,
        duration: Option<i32>,
        done: oneshot::Sender<()>,
    },
    /// Changes a dimension's world border as `/worldborder` does.
    WorldBorder {
        dimension: DimensionId,
        change: crate::world::border::BorderChange,
        reply: oneshot::Sender<
            Result<Option<crate::world::border::Resized>, crate::world::border::BorderError>,
        >,
    },
    /// Queries (`None`) or sets the difficulty.
    Difficulty {
        difficulty: Option<Difficulty>,
        reply: oneshot::Sender<DifficultyOutcome>,
    },
    /// Locks or unlocks the difficulty and tells every player.
    LockDifficulty {
        locked: bool,
    },
    /// `/defaultgamemode`: sets the mode new players start in.
    DefaultGameMode {
        mode: crate::protocol::play::GameMode,
        reply: oneshot::Sender<()>,
    },
    SetWorldSpawn {
        spawn: Spawn,
        done: oneshot::Sender<()>,
    },
    /// Changes or lists the operator, whitelist and ban lists. `source` is
    /// who asked, as ban entries record it.
    Lists {
        source: String,
        op: ListOp,
        reply: oneshot::Sender<ListOutcome>,
    },
    /// Saves the level and asks every player and region to save. `done`
    /// is told once they have been asked, or with `flush` once everything
    /// is written; `Err` names who holds saving if a hold refused it.
    SaveAll {
        flush: bool,
        done: oneshot::Sender<Result<(), Arc<str>>>,
    },
    /// Chunk storage failed a read or a write: warns operators if the
    /// disk is nearly full.
    ChunkIoFailed,
    Stop,
    Shutdown {
        done: oneshot::Sender<()>,
    },
    /// Test hook: panics the coordinator's loop with this message.
    #[cfg(feature = "test-hooks")]
    Panic(&'static str),
}

/// Operator, whitelist and ban list changes.
#[derive(Debug, Clone)]
pub enum ListOp {
    Op(Vec<NameAndId>),
    Deop(Vec<NameAndId>),
    WhitelistAdd(Vec<NameAndId>),
    WhitelistRemove(Vec<NameAndId>),
    WhitelistList,
    WhitelistOn,
    WhitelistOff,
    WhitelistReload,
    Ban {
        users: Vec<NameAndId>,
        reason: Option<String>,
    },
    BanIp {
        target: String,
        reason: Option<String>,
    },
    BanList {
        players: bool,
        ips: bool,
    },
    Pardon(Vec<NameAndId>),
    PardonIp(String),
}

/// What a `/time` subcommand did to a clock (see
/// [`crate::world::clock::run_clock`]); `name` is the clock's.
#[derive(Debug, Clone, PartialEq)]
pub enum ClockOutcome {
    Query {
        name: &'static str,
        total_ticks: i64,
    },
    QueryTimeline {
        timeline: &'static str,
        value: i64,
        repetitions: bool,
    },
    WrongTimeline {
        timeline: &'static str,
        name: &'static str,
    },
    Set {
        name: &'static str,
        total_ticks: i64,
    },
    AlreadyAtTime {
        name: &'static str,
        ticks: i32,
    },
    NoTimeMarker {
        marker: String,
        name: &'static str,
    },
    AlreadyAtMarker {
        name: &'static str,
        marker: String,
    },
    SetMarker {
        name: &'static str,
        marker: String,
    },
    Paused {
        name: &'static str,
        paused: bool,
    },
    AlreadyPaused {
        name: &'static str,
        paused: bool,
    },
    Rate {
        name: &'static str,
        rate: f32,
    },
    AlreadyRate {
        name: &'static str,
        rate: f32,
    },
}

/// What a `/tick` subcommand did.
#[derive(Debug)]
pub enum TickOutcome {
    Query {
        state: crate::world::tick_rate::TickState,
        sprinting: bool,
        average: std::time::Duration,
        samples: crate::world::tick_rate::Samples,
    },
    Rate(f32),
    Frozen(bool),
    Stepping(u32),
    /// Stepping needs the game frozen.
    NotFrozen,
    StepStopped,
    NotStepping,
    /// A sprint started; `interrupted` when it replaced a running one.
    Sprinting {
        interrupted: bool,
    },
    SprintStopped,
    NotSprinting,
}

/// What `/gamerule` did.
#[derive(Debug, Clone, PartialEq)]
pub enum GameRuleOutcome {
    Unknown,
    /// The rule exists but is switched off in this version's features;
    /// the game says nothing.
    Disabled,
    Query {
        rule: &'static str,
        value: String,
    },
    AlreadySet {
        rule: &'static str,
        value: String,
    },
    Set {
        rule: &'static str,
        value: String,
    },
}

/// What `/difficulty` did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DifficultyOutcome {
    Query(Difficulty),
    Unchanged(Difficulty),
    Set(Difficulty),
}

/// What a list change did. The per-user variants hold the users actually
/// changed; empty means nothing changed.
#[derive(Debug, Clone)]
pub enum ListOutcome {
    Opped(Vec<NameAndId>),
    Deopped(Vec<NameAndId>),
    WhitelistAdded(Vec<NameAndId>),
    WhitelistRemoved(Vec<NameAndId>),
    Whitelist(Vec<String>),
    /// `/whitelist on|off`; `changed` false when it already was.
    WhitelistEnabled {
        on: bool,
        changed: bool,
    },
    WhitelistReloaded,
    /// Each banned user with the ban's reason line.
    Banned(Vec<(NameAndId, TextComponent)>),
    IpBanInvalid,
    IpAlreadyBanned,
    IpBanned {
        ip: String,
        reason: TextComponent,
        /// Online players on that address, who were kicked.
        affected: Vec<String>,
    },
    /// Every matching ban: name or address, who banned, reason line.
    BanList(Vec<(String, String, TextComponent)>),
    Pardoned(Vec<NameAndId>),
    IpPardonInvalid,
    IpNotBanned,
    IpPardoned(String),
}

/// What a connection needs to forward packets to its player actor.
#[derive(Debug)]
pub struct Joined {
    pub player: PlayerId,
    pub packets: mpsc::Sender<Serverbound>,
    pub permission: PermissionLevel,
    /// Turns true once the player's session has left and saved.
    pub done: tokio::sync::watch::Receiver<bool>,
}

/// A request to a player's connection to change protocol phase. Each reply
/// says whether the connection was in the phase the request applies to.
#[derive(Debug)]
pub enum PhaseRequest {
    /// Takes the player out of the world into configuration. With `hold`,
    /// the connection stays there until [`PhaseRequest::Release`]; without,
    /// it re-syncs registries and tags and rejoins at once.
    Configure {
        hold: bool,
        reply: oneshot::Sender<bool>,
    },
    /// Re-syncs a held connection and returns it to play.
    Release { reply: oneshot::Sender<bool> },
}

/// What the other players see of a chat message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatAudience {
    Everyone,
    Masked(String),
    SenderOnly,
}

/// How `FetchProfile` identifies the account.
#[derive(Debug, Clone)]
pub enum ProfileQuery {
    Name(String),
    Id(u128),
}
