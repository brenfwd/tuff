//! One plugin module instantiated in its own wasmi store: the only place
//! guest code runs. Every call is metered (fuel), memory growth is capped,
//! and every value the guest hands the host is bounds- and size-checked,
//! so a misbehaving module faults instead of hurting the server. The host
//! side of the WIT world's imports is here.
//!
//! Everything the plugin owns at runtime (its registrations, its per-player
//! state, the effects of the call in progress) lives in the store's data,
//! so dropping the sandbox drops all of it.

use std::collections::HashMap;
use std::fmt;

use tuff_core::permission::{Node, Subject, View};
use tuff_storage::admin::PermissionLevel;
use tuff_world_model::BlockPos;

use std::sync::Arc;

use crate::wit::runtime::Engine;

use crate::abi;
use crate::caps::Kind;

mod declared;
mod imports;
mod panel;
mod runner;
mod state;

use crate::entities::EntityOp;
use crate::files::{Files, Op, validate};
use crate::portal::{Target, Teleport};
use crate::storage::{Quota, Storage};
use crate::wit::bindings::capability_types::{
    CallError, CapabilityKind, Denied, Handle, Header, HttpRequest, RouteResponse, WriteMode,
    WsMessage,
};
use crate::wit::bindings::types::{
    self as wit, BlockBox, ClickKind, CommandTree, ConfigDecl, ConfigValue, Entity, Event,
    EventKinds, HostError, Inventory, Invocation, ItemDetails, ItemStack, Location, LogLevel,
    Property, RegisterError, ResourcePack, RichClick, RichSpan, Rotation, SetFlags, Slot, SnbtSlot,
    SuggestRequest, TeamSpec, TitleSpec, Traits, Transform, Uuid, Vec3, WorldError,
};
use crate::wit::bindings::{self, Imports};
use crate::wit::canon::{Bounds, Value};
use crate::wit::runtime::{Host, Runtime};
use crate::wit::tree;
use crate::wit::value::{self, ToValue};
use crate::world::{self, BiomeWrite, BlockWrite, Env};
pub use runner::{Sandbox, engine};
use state::{online_players, permitted, unavailable};
use tuff_core::world::session::plugin_inventory::InventoryOp;

/// Environment variable overriding the per-call fuel allowance.
pub const FUEL_ENV: &str = "TUFF_WASM_FUEL";

/// Default fuel per call: seconds of guest work, enough for large edits.
pub const DEFAULT_FUEL: u64 = 5_000_000_000;

/// Resource caps for one plugin.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Fuel (roughly, wasm instructions) one call may burn.
    pub fuel: u64,
    /// Largest linear memory, in bytes.
    pub memory: usize,
    /// Longest string or value crossing the boundary, in bytes.
    pub text: usize,
    /// Messages (replies and broadcasts) one call may produce.
    pub messages: usize,
    /// Timers one call may schedule.
    pub schedules: usize,
    /// Per-player state keys a plugin may hold for one player.
    pub state_keys: usize,
    /// What the plugin's persistent store may hold.
    pub storage: Quota,
    /// Blocks one call may read.
    pub world_reads: u64,
    /// Blocks one call may queue for writing.
    pub world_writes: usize,
    /// Messages one call may send to other plugins.
    pub sends: usize,
    /// Largest payload of a message or reply between plugins, in bytes.
    pub message_bytes: usize,
    /// Bytes of strings and lists one import call may hand the host.
    pub call_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fuel: std::env::var(FUEL_ENV)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_FUEL),
            memory: 64 << 20,
            text: 32 << 10,
            messages: 64,
            schedules: 64,
            state_keys: 64,
            storage: Quota::default(),
            world_reads: 1 << 20,
            world_writes: 1 << 16,
            sends: 16,
            message_bytes: abi::MESSAGE_BYTES,
            call_bytes: 1 << 20,
        }
    }
}

impl Limits {
    /// What one call may move from the guest to the host.
    pub fn bounds(&self) -> Bounds {
        Bounds {
            string: self.text,
            bytes: self.call_bytes,
            ..Bounds::default()
        }
    }
}

/// The player a call acts for.
#[derive(Clone, Debug, PartialEq)]
pub struct Player {
    pub uuid: u128,
    pub name: String,
    /// Their operator level; looked up when a permission check needs it
    /// if `None`.
    pub level: Option<PermissionLevel>,
    pub position: Option<Position>,
    /// What they carry, when the call knows.
    pub items: Option<Items>,
}

/// A player's inventory as of the start of a call.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Items {
    /// The selected hotbar slot's stack: item id and count.
    pub held: Option<(String, u8)>,
    /// Every occupied slot: its index, item id and count.
    pub slots: Vec<(u8, String, u8)>,
    /// Every occupied slot's whole stack.
    pub stacks: Vec<(u8, tuff_item::item::ItemSnapshot)>,
}

impl Player {
    pub fn new(uuid: u128, name: impl Into<String>) -> Self {
        Self {
            uuid,
            name: name.into(),
            level: None,
            position: None,
            items: None,
        }
    }

    /// The player as the guest sees them.
    pub fn to_wit(&self) -> wit::Player {
        wit::Player {
            uuid: uuid(self.uuid),
            name: self.name.clone(),
        }
    }
}

pub fn uuid(uuid: u128) -> Uuid {
    Uuid {
        high: (uuid >> 64) as u64,
        low: uuid as u64,
    }
}

pub fn uuid_of(uuid: Uuid) -> u128 {
    (u128::from(uuid.high) << 64) | u128::from(uuid.low)
}

/// Where a player is: their dimension's key, their feet and, when known,
/// their yaw and pitch.
#[derive(Clone, Debug, PartialEq)]
pub struct Position {
    pub dimension: String,
    pub at: [f64; 3],
    pub look: Option<[f32; 2]>,
    /// Height of the eyes above the feet.
    pub eye_height: f64,
}

pub const STANDING_EYE_HEIGHT: f64 = 1.62;

impl fmt::Display for Position {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [x, y, z] = self.at;
        write!(f, "{x} {y} {z} ")?;
        if let Some([yaw, pitch]) = self.look {
            write!(f, "{yaw} {pitch} ")?;
        }
        f.write_str(&self.dimension)
    }
}

/// A notify or request one call queued for another plugin; `request` is
/// the requester's handle and timeout in ticks.
#[derive(Debug, PartialEq)]
pub struct OutMessage {
    pub target: String,
    pub interface: String,
    pub method: u64,
    pub payload: Vec<u8>,
    pub request: Option<(Handle, u32)>,
}

/// What a call asked the host to do once it returns.
#[derive(Debug, Default, PartialEq)]
pub struct Effects {
    pub replies: Vec<String>,
    pub broadcasts: Vec<String>,
    /// `(player uuid, text)` for players other than the acting one.
    pub tells: Vec<(u128, String)>,
    pub rich_tells: Vec<(u128, tuff_item::text::TextComponent)>,
    /// `(delay in server ticks, token)`.
    pub timers: Vec<(u64, u32)>,
    /// Changes to the persistent store, in order; `None` removes.
    pub stored: Vec<(String, Option<Vec<u8>>)>,
    pub blocks: Vec<BlockWrite>,
    pub biomes: Vec<BiomeWrite>,
    /// Messages to other plugins, in order.
    pub messages: Vec<OutMessage>,
    /// Answers to requests this plugin was delivered.
    pub answers: Vec<(u64, Result<Vec<u8>, String>)>,
    /// Answers to route requests, in order.
    pub responses: Vec<(Handle, RouteResponse)>,
    pub teleports: Vec<Teleport>,
    pub entity_ops: Vec<EntityOp>,
    pub inventory_ops: Vec<(
        u128,
        tuff_core::world::session::plugin_inventory::InventoryOp,
    )>,
    pub advancements: Vec<AdvancementOp>,
    pub displays: Vec<DisplayOp>,
    pub player_ops: Vec<(u128, crate::player_state::PlayerOp)>,
    pub particles: Vec<crate::particles::ParticleSpawn>,
    /// The reason a plugin refused the login being decided.
    pub login_refusal: Option<String>,
    pub menus: Vec<crate::menus::MenuOp>,
    /// Block rule and team changes, in order.
    pub guard: Vec<crate::guard::Op>,
    /// Runtime dimensions discarded during the call, to clean up after
    /// its teleports.
    pub discards: Vec<tuff_core::world::dimension::DimensionId>,
    /// Refused stash moves, for the audit log.
    pub violations: Vec<crate::stash::StashViolation>,
}

impl Effects {
    fn registered_menu(&mut self, op: crate::menus::MenuOp) {
        self.menus.push(op);
    }
}

/// A title, sidebar or team change a plugin queued.
#[derive(Debug, PartialEq)]
pub enum DisplayOp {
    Title {
        player: u128,
        title: String,
        subtitle: String,
        times: [u32; 3],
    },
    ClearTitle(u128),
    ActionBar(u128, String),
    Payload {
        player: u128,
        channel: String,
        data: Vec<u8>,
    },
    Sidebar(Option<(String, Vec<String>)>),
    Team {
        name: String,
        color: Option<String>,
        prefix: String,
        suffix: String,
        members: Vec<String>,
    },
    TeamRemove(String),
}

/// A grant or revoke of an advancement a plugin queued.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvancementOp {
    pub player: u128,
    pub grant: bool,
    pub advancement: String,
    pub criterion: Option<String>,
}

/// A permission node a plugin declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    pub node: String,
    /// 0: every player holds it; otherwise the lowest operator level that
    /// does without a grant.
    pub level: u8,
}

impl Permission {
    pub fn to_node(&self) -> Node {
        match PermissionLevel::from_id(i64::from(self.level)) {
            Some(PermissionLevel::All) | None => Node::new(&self.node, "").by_default(true),
            Some(level) => Node::new(&self.node, "").op_level(level),
        }
    }
}

/// A click a plugin asked the server to cancel and report.
#[derive(Clone, Debug, PartialEq)]
pub struct ClickFilter {
    pub kind: ClickKind,
    /// An item id, none for any item.
    pub item: Option<String>,
    /// A top-level `custom_data` key, none for any.
    pub tag: Option<String>,
    /// A block id the clicked block must be, none for any.
    pub block: Option<String>,
    pub scope: crate::combat::Scope,
}

/// An item action a plugin asked the server to cancel and report.
#[derive(Clone, Debug, PartialEq)]
pub struct ItemFilter {
    pub kind: wit::ItemActionKind,
    /// An item id, none for any item.
    pub item: Option<String>,
    /// A top-level `custom_data` key, none for any.
    pub tag: Option<String>,
    pub scope: crate::combat::Scope,
}

use declared::setting_value;
mod event_kinds;
pub use declared::{Pack, Setting, config_value};
pub use event_kinds::kind_of;

/// What a plugin claimed while registering.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Registration {
    pub commands: Vec<CommandTree>,
    /// Trees rooted at `/tuff` subcommand names.
    pub tuff_subcommands: Vec<CommandTree>,
    /// `(alias root, target line)`.
    pub aliases: Vec<(String, String)>,
    pub events: EventKinds,
    pub clicks: Vec<ClickFilter>,
    pub items: Vec<ItemFilter>,
    /// Custom payload channels the plugin receives and may send on.
    pub channels: Vec<String>,
    pub inventory: Vec<wit::InventoryFilter>,
    pub state: Vec<wit::StateFilter>,
    pub entity_filters: Vec<crate::menus::EntityFilter>,
    pub command_filters: Vec<crate::line_filters::CommandFilter>,
    pub chat_filters: Vec<crate::line_filters::ChatFilter>,
    pub attack_filters: Vec<crate::combat::AttackFilter>,
    pub damage_filters: Vec<crate::combat::DamageFilter>,
    /// Block ids (`namespace:name`) the plugin treats as its portals.
    pub watched: Vec<String>,
    pub regions: Vec<wit::RegionWatch>,
    pub permissions: Vec<Permission>,
    pub settings: Vec<Setting>,
    pub packs: Vec<Pack>,
    pub panel_pages: Vec<wit::PanelPage>,
    pub panel_apis: Vec<wit::PanelApi>,
    pub panel_cards: Vec<wit::PanelCard>,
    /// `(interface, method keys)` the plugin serves.
    pub provides: Vec<(String, Vec<u64>)>,
}

impl Registration {
    pub fn command_names(&self) -> impl Iterator<Item = &str> {
        self.commands.iter().map(tree::name)
    }

    pub fn tuff_names(&self) -> impl Iterator<Item = &str> {
        self.tuff_subcommands.iter().map(tree::name)
    }

    /// The tree that runs `line` and the part of `line` it starts at.
    fn tree_for<'a>(&self, line: &'a str) -> Option<(&CommandTree, &'a str)> {
        let line = line.trim_start();
        let root = line.split(' ').next().unwrap_or(line);
        if let Some(tree) = self.commands.iter().find(|t| tree::name(t) == root) {
            return Some((tree, line));
        }
        let sub = line.strip_prefix("tuff ")?.trim_start();
        let name = sub.split(' ').next().unwrap_or(sub);
        let tree = self
            .tuff_subcommands
            .iter()
            .find(|t| tree::name(t) == name)?;
        Some((tree, sub))
    }
}

/// Why guest code could not run or was stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault(pub String);

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Fault {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Instantiate,
    Register,
    Running,
}

pub(crate) struct State {
    limits: Limits,
    plugin: String,
    phase: Phase,
    registration: Registration,
    effects: Effects,
    caller: Option<Player>,
    deciding_login: bool,
    players: HashMap<u128, HashMap<String, Vec<u8>>>,
    env: Option<Env>,
    storage: Storage,
    net: Option<crate::net::Net>,
    background: Option<crate::background::Background>,
    /// Blocks read during the call in progress.
    reads: u64,
    files: Option<Arc<Files>>,
    policy: Option<tokio::sync::watch::Receiver<Arc<crate::caps::Policy>>>,
    panel_series: panel::SeriesCache,
    guard: crate::guard::Calls,
    index: Option<tokio::sync::watch::Receiver<Arc<crate::host::Index>>>,
    next_request: Handle,
    outstanding: std::collections::HashSet<Handle>,
    owed: std::collections::HashSet<u64>,
    fakes: crate::fake::Fakes,
    stashes: crate::stash::Stashes,
}

impl State {
    /// Plugin-to-plugin traffic is open by default; a grant check for
    /// cross-plugin messaging belongs here.
    fn may_message(&self, _target: &str) -> Result<(), CallError> {
        Ok(())
    }

    fn message_bytes(&self) -> usize {
        self.limits.message_bytes
    }

    fn new(plugin: &str, limits: Limits) -> Self {
        Self {
            limits,
            plugin: plugin.to_owned(),
            phase: Phase::Instantiate,
            registration: Registration::default(),
            effects: Effects::default(),
            caller: None,
            deciding_login: false,
            players: HashMap::new(),
            env: None,
            storage: Storage::default(),
            net: None,
            background: None,
            reads: 0,
            files: None,
            policy: None,
            panel_series: panel::SeriesCache::default(),
            guard: crate::guard::Calls::default(),
            index: None,
            next_request: 0,
            outstanding: Default::default(),
            owed: Default::default(),
            fakes: Default::default(),
            stashes: Default::default(),
        }
    }

    fn file_op(&self, op: Result<Op, Denied>) -> Result<Handle, Denied> {
        match &self.files {
            Some(files) => files.submit(op?),
            None => Err(Denied::NotGranted("files".into())),
        }
    }
}

impl Host for State {
    fn call(
        &mut self,
        index: usize,
        args: Vec<Value<'_>>,
    ) -> value::Result<Option<Box<dyn ToValue>>> {
        bindings::dispatch(self, index, args)
    }
}

impl Imports for State {
    fn host_log(&mut self, level: LogLevel, message: &str) -> value::Result<()> {
        State::host_log(self, level, message)
    }
    fn host_register_command(
        &mut self,
        tree: CommandTree,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_register_command(self, tree)
    }
    fn host_register_tuff_subcommand(
        &mut self,
        tree: CommandTree,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_register_tuff_subcommand(self, tree)
    }
    fn host_register_alias(
        &mut self,
        name: &str,
        target: &str,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_register_alias(self, name, target)
    }
    fn host_subscribe(&mut self, kinds: EventKinds) -> value::Result<Result<(), RegisterError>> {
        State::host_subscribe(self, kinds)
    }

    fn host_intercept_click(
        &mut self,
        filter: wit::ClickFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_intercept_click(self, filter)
    }

    fn host_register_channel(&mut self, channel: &str) -> value::Result<Result<(), RegisterError>> {
        State::host_register_channel(self, channel)
    }

    fn host_send_payload(
        &mut self,
        player: Uuid,
        channel: &str,
        data: &[u8],
    ) -> value::Result<Result<(), HostError>> {
        State::host_send_payload(self, player, channel, data)
    }

    fn host_intercept_item(
        &mut self,
        filter: wit::ItemFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_intercept_item(self, filter)
    }

    fn host_intercept_inventory(
        &mut self,
        filter: wit::InventoryFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_intercept_inventory(self, filter)
    }

    fn host_intercept_state(
        &mut self,
        filter: wit::StateFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_intercept_state(self, filter)
    }

    fn host_intercept_entity_interact(
        &mut self,
        filter: wit::EntityFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_intercept_entity_interact(self, filter)
    }

    fn host_intercept_command(
        &mut self,
        filter: wit::CommandFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_intercept_command(self, filter)
    }

    fn host_intercept_attack(
        &mut self,
        filter: wit::AttackFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_intercept_attack(self, filter)
    }

    fn host_intercept_chat(
        &mut self,
        filter: wit::ChatFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_intercept_chat(self, filter)
    }

    fn host_intercept_damage(
        &mut self,
        filter: wit::DamageFilter,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_intercept_damage(self, filter)
    }

    fn host_open_menu(
        &mut self,
        player: Uuid,
        menu: &str,
        rows: u8,
        title: &str,
        items: Vec<wit::MenuItem>,
    ) -> value::Result<Result<(), wit::MenuError>> {
        State::host_open_menu(self, player, menu, rows, title, items)
    }

    fn host_update_menu_slot(
        &mut self,
        player: Uuid,
        slot: u8,
        item: Option<wit::MenuItem>,
    ) -> value::Result<Result<(), wit::MenuError>> {
        State::host_update_menu_slot(self, player, slot, item)
    }

    fn host_close_menu(&mut self, player: Uuid) -> value::Result<()> {
        State::host_close_menu(self, player)
    }

    fn host_watch_block(&mut self, block: &str) -> value::Result<Result<(), RegisterError>> {
        State::host_watch_block(self, block)
    }

    fn host_watch_region(
        &mut self,
        region: wit::RegionWatch,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_watch_region(self, region)
    }

    fn host_teleport_player(
        &mut self,
        player: Uuid,
        dimension: &str,
        pos: Vec3,
        rotation: Option<Rotation>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_teleport_player(self, player, dimension, pos, rotation)
    }

    fn host_teleport_entity(
        &mut self,
        entity: Uuid,
        dimension: &str,
        pos: Vec3,
        rotation: Option<Rotation>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_teleport_entity(self, entity, dimension, pos, rotation)
    }

    fn host_spawn_entity(
        &mut self,
        dimension: &str,
        kind: &str,
        pos: Vec3,
        snbt: Option<String>,
    ) -> value::Result<Result<Uuid, WorldError>> {
        State::host_spawn_entity(self, dimension, kind, pos, snbt)
    }

    fn host_remove_entity(&mut self, entity: Uuid) -> value::Result<Result<(), WorldError>> {
        State::host_remove_entity(self, entity)
    }

    fn host_entity_data(
        &mut self,
        entity: Uuid,
    ) -> value::Result<Result<Option<String>, WorldError>> {
        State::host_entity_data(self, entity)
    }

    fn host_entity_merge(
        &mut self,
        entity: Uuid,
        snbt: &str,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_entity_merge(self, entity, snbt)
    }

    fn host_entity_set_name(
        &mut self,
        entity: Uuid,
        name: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_entity_set_name(self, entity, name)
    }

    fn host_entity_strike(
        &mut self,
        attacker: Uuid,
        victim: Uuid,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_entity_strike(self, attacker, victim)
    }

    fn host_glide_entities(
        &mut self,
        moves: Vec<bindings::host::EntityGlide>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_glide_entities(self, moves)
    }

    fn host_entity_states(
        &mut self,
        entities: Vec<Uuid>,
    ) -> value::Result<Result<Vec<Option<bindings::host::EntityState>>, WorldError>> {
        State::host_entity_states(self, entities)
    }

    fn host_teleport_self(
        &mut self,
        pos: Vec3,
        rotation: Option<Rotation>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_teleport_self(self, pos, rotation)
    }

    fn host_give_item(
        &mut self,
        item: &str,
        count: u32,
        custom_data: Option<String>,
        custom_name: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_give_item(self, item, count, custom_data, custom_name)
    }

    fn host_give_item_to(
        &mut self,
        player: Uuid,
        item: &str,
        count: u32,
        custom_data: Option<String>,
        custom_name: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_give_item_to(self, player, item, count, custom_data, custom_name)
    }

    fn host_take_items(
        &mut self,
        player: Uuid,
        item: &str,
        count: u32,
        all_or_nothing: bool,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_take_items(self, player, item, count, all_or_nothing)
    }

    fn host_stash_new(&mut self) -> value::Result<Result<u64, WorldError>> {
        State::host_stash_new(self)
    }

    fn host_stash_mint(
        &mut self,
        stash: u64,
        item: &str,
        count: u32,
        custom_data: Option<String>,
        custom_name: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_stash_mint(self, stash, item, count, custom_data, custom_name)
    }

    fn host_stash_contents(
        &mut self,
        stash: u64,
    ) -> value::Result<Option<Vec<crate::wit::bindings::types::ItemStack>>> {
        State::host_stash_contents(self, stash)
    }

    fn host_stash_split(
        &mut self,
        stash: u64,
        item: Option<String>,
        count: u32,
    ) -> value::Result<Result<u64, WorldError>> {
        State::host_stash_split(self, stash, item, count)
    }

    fn host_stash_merge(
        &mut self,
        target: u64,
        source: u64,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_stash_merge(self, target, source)
    }

    fn host_stash_burn(&mut self, stash: u64) -> value::Result<Result<(), WorldError>> {
        State::host_stash_burn(self, stash)
    }

    fn host_give_stash(
        &mut self,
        player: Uuid,
        stash: u64,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_give_stash(self, player, stash)
    }

    fn host_take_into_stash(
        &mut self,
        player: Uuid,
        item: &str,
        count: u32,
        all_or_nothing: bool,
        stash: u64,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_take_into_stash(self, player, item, count, all_or_nothing, stash)
    }

    fn host_clear_inventory(&mut self, player: Uuid) -> value::Result<Result<(), WorldError>> {
        State::host_clear_inventory(self, player)
    }

    fn host_set_slot(
        &mut self,
        player: Uuid,
        slot: u8,
        stack: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_set_slot(self, player, slot, stack)
    }

    fn host_set_keep_inventory(
        &mut self,
        player: Uuid,
        keep: Option<bool>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_set_keep_inventory(self, player, keep)
    }

    fn host_acting_inventory_snbt(&mut self) -> value::Result<Option<Vec<SnbtSlot>>> {
        State::host_acting_inventory_snbt(self)
    }

    fn host_advancement_grant(
        &mut self,
        player: Uuid,
        advancement: &str,
        criterion: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_advancement_grant(self, player, advancement, criterion)
    }

    fn host_advancement_revoke(
        &mut self,
        player: Uuid,
        advancement: &str,
        criterion: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_advancement_revoke(self, player, advancement, criterion)
    }

    fn host_online_players(&mut self) -> value::Result<Vec<wit::Player>> {
        State::host_online_players(self)
    }

    fn host_player_info(&mut self, player: Uuid) -> value::Result<Option<wit::PlayerState>> {
        State::host_player_info(self, player)
    }

    fn host_player_location(&mut self, player: Uuid) -> value::Result<Option<Location>> {
        State::host_player_location(self, player)
    }

    fn host_set_game_mode(
        &mut self,
        player: Uuid,
        mode: wit::GameMode,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_set_game_mode(self, player, mode)
    }

    fn host_set_vitals(
        &mut self,
        player: Uuid,
        health: Option<f32>,
        food: Option<i32>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_set_vitals(self, player, health, food)
    }

    fn host_set_xp(
        &mut self,
        player: Uuid,
        level: i32,
        points: i32,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_set_xp(self, player, level, points)
    }

    fn host_add_effect(
        &mut self,
        player: Uuid,
        effect: &str,
        duration: i32,
        amplifier: u8,
        particles: bool,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_add_effect(self, player, effect, duration, amplifier, particles)
    }

    fn host_clear_effects(
        &mut self,
        player: Uuid,
        effect: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_clear_effects(self, player, effect)
    }

    fn host_kill_player(&mut self, player: Uuid) -> value::Result<Result<(), WorldError>> {
        State::host_kill_player(self, player)
    }

    fn host_respawn_player(&mut self, player: Uuid) -> value::Result<Result<(), WorldError>> {
        State::host_respawn_player(self, player)
    }

    fn host_create_dimension(
        &mut self,
        key: &str,
        base: &str,
        copy: Option<wit::ChunkArea>,
    ) -> value::Result<Result<(), wit::DimensionError>> {
        State::host_create_dimension(self, key, base, copy)
    }

    fn host_discard_dimension(
        &mut self,
        key: &str,
    ) -> value::Result<Result<(), wit::DimensionError>> {
        State::host_discard_dimension(self, key)
    }

    fn host_runtime_dimensions(&mut self) -> value::Result<Vec<String>> {
        State::host_runtime_dimensions(self)
    }

    fn host_set_respawn_point(
        &mut self,
        player: Uuid,
        dimension: &str,
        pos: wit::BlockPos,
        rotation: Rotation,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_set_respawn_point(self, player, dimension, pos, rotation)
    }

    fn host_declare_permission(
        &mut self,
        node: &str,
        level: u8,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_declare_permission(self, node, level)
    }

    fn host_declare_config(
        &mut self,
        decl: ConfigDecl,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_declare_config(self, decl)
    }

    fn host_register_resource_pack(
        &mut self,
        pack: ResourcePack,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_register_resource_pack(self, pack)
    }

    fn host_register_panel_page(
        &mut self,
        page: wit::PanelPage,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_register_panel_page(self, page)
    }

    fn host_register_panel_api(
        &mut self,
        api: wit::PanelApi,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_register_panel_api(self, api)
    }

    fn host_register_panel_card(
        &mut self,
        card: wit::PanelCard,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_register_panel_card(self, card)
    }

    fn host_panel_card_set(
        &mut self,
        series: &str,
        value: i64,
    ) -> value::Result<Result<(), HostError>> {
        State::host_panel_card_set(self, series, value)
    }

    fn host_config_get(&mut self, key: &str) -> value::Result<Option<ConfigValue>> {
        State::host_config_get(self, key)
    }

    fn host_config_set(
        &mut self,
        key: &str,
        value: ConfigValue,
    ) -> value::Result<Result<(), HostError>> {
        State::host_config_set(self, key, value)
    }

    fn host_refuse_login(&mut self, reason: &str) -> value::Result<Result<(), HostError>> {
        State::host_refuse_login(self, reason)
    }

    fn host_reply(&mut self, message: &str) -> value::Result<Result<(), HostError>> {
        State::host_reply(self, message)
    }

    fn host_broadcast(&mut self, message: &str) -> value::Result<Result<(), HostError>> {
        State::host_broadcast(self, message)
    }

    fn host_tell(&mut self, player: Uuid, message: &str) -> value::Result<Result<(), HostError>> {
        State::host_tell(self, player, message)
    }

    fn host_tell_rich(
        &mut self,
        player: Uuid,
        spans: Vec<RichSpan>,
    ) -> value::Result<Result<(), HostError>> {
        State::host_tell_rich(self, player, spans)
    }

    fn host_show_title(
        &mut self,
        player: Uuid,
        title: TitleSpec,
    ) -> value::Result<Result<(), HostError>> {
        State::host_show_title(self, player, title)
    }

    fn host_clear_title(&mut self, player: Uuid) -> value::Result<Result<(), HostError>> {
        State::host_clear_title(self, player)
    }

    fn host_action_bar(
        &mut self,
        player: Uuid,
        text: &str,
    ) -> value::Result<Result<(), HostError>> {
        State::host_action_bar(self, player, text)
    }

    fn host_sidebar_set(
        &mut self,
        title: &str,
        lines: Vec<String>,
    ) -> value::Result<Result<(), HostError>> {
        State::host_sidebar_set(self, title, lines)
    }

    fn host_sidebar_clear(&mut self) -> value::Result<Result<(), HostError>> {
        State::host_sidebar_clear(self)
    }

    fn host_team_set(
        &mut self,
        name: &str,
        team: TeamSpec,
    ) -> value::Result<Result<(), HostError>> {
        State::host_team_set(self, name, team)
    }

    fn host_team_remove(&mut self, name: &str) -> value::Result<Result<(), HostError>> {
        State::host_team_remove(self, name)
    }

    fn host_provide(
        &mut self,
        interface: &str,
        methods: Vec<u64>,
    ) -> value::Result<Result<(), RegisterError>> {
        State::host_provide(self, interface, methods)
    }

    fn host_request(
        &mut self,
        target: Option<String>,
        interface: &str,
        method: u64,
        payload: &[u8],
        timeout_ticks: u32,
    ) -> value::Result<Result<Handle, CallError>> {
        State::host_request(self, target, interface, method, payload, timeout_ticks)
    }

    fn host_notify(
        &mut self,
        target: Option<String>,
        interface: &str,
        method: u64,
        payload: &[u8],
    ) -> value::Result<Result<u32, CallError>> {
        State::host_notify(self, target, interface, method, payload)
    }

    fn host_respond(
        &mut self,
        request: u64,
        outcome: Result<Vec<u8>, String>,
    ) -> value::Result<Result<(), HostError>> {
        State::host_respond(self, request, outcome)
    }

    fn host_plugin_loaded(&mut self, id: &str) -> value::Result<Option<String>> {
        State::host_plugin_loaded(self, id)
    }

    fn host_schedule(&mut self, delay: u64, token: u32) -> value::Result<Result<(), HostError>> {
        State::host_schedule(self, delay, token)
    }

    fn host_acting_player(&mut self) -> value::Result<Option<wit::Player>> {
        State::host_acting_player(self)
    }

    fn host_acting_location(&mut self) -> value::Result<Option<Location>> {
        State::host_acting_location(self)
    }

    fn host_has_permission(&mut self, node: &str) -> value::Result<Option<bool>> {
        State::host_has_permission(self, node)
    }

    fn host_acting_inventory(&mut self) -> value::Result<Option<Inventory>> {
        State::host_acting_inventory(self)
    }

    fn host_state_get(&mut self, key: &str) -> value::Result<Result<Option<Vec<u8>>, HostError>> {
        State::host_state_get(self, key)
    }

    fn host_state_set(
        &mut self,
        key: &str,
        value: Option<Vec<u8>>,
    ) -> value::Result<Result<(), HostError>> {
        State::host_state_set(self, key, value)
    }

    fn host_storage_get(&mut self, key: &str) -> value::Result<Option<Vec<u8>>> {
        State::host_storage_get(self, key)
    }

    fn host_storage_set(
        &mut self,
        key: &str,
        value: Option<Vec<u8>>,
    ) -> value::Result<Result<(), HostError>> {
        State::host_storage_set(self, key, value)
    }

    fn host_block_get(
        &mut self,
        dimension: &str,
        pos: wit::BlockPos,
    ) -> value::Result<Result<String, WorldError>> {
        State::host_block_get(self, dimension, pos)
    }

    fn host_blocks_get(
        &mut self,
        dimension: &str,
        a: wit::BlockPos,
        b: wit::BlockPos,
    ) -> value::Result<Result<BlockBox, WorldError>> {
        State::host_blocks_get(self, dimension, a, b)
    }

    fn host_block_set(
        &mut self,
        dimension: &str,
        pos: wit::BlockPos,
        state: &str,
        flags: SetFlags,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_block_set(self, dimension, pos, state, flags)
    }

    fn host_spawn_particles(
        &mut self,
        dimension: &str,
        spawn: wit::ParticleSpawn,
        viewer: Option<Uuid>,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_spawn_particles(self, dimension, spawn, viewer)
    }

    fn host_generated_blocks_get(
        &mut self,
        dimension: &str,
        a: wit::BlockPos,
        b: wit::BlockPos,
        seed: Option<i64>,
        biome: Option<String>,
    ) -> value::Result<Result<BlockBox, WorldError>> {
        State::host_generated_blocks_get(self, dimension, a, b, seed, biome)
    }

    fn host_biome_get(
        &mut self,
        dimension: &str,
        pos: wit::BlockPos,
    ) -> value::Result<Result<String, WorldError>> {
        State::host_biome_get(self, dimension, pos)
    }

    fn host_biome_set(
        &mut self,
        dimension: &str,
        a: wit::BlockPos,
        b: wit::BlockPos,
        biome: &str,
    ) -> value::Result<Result<(), WorldError>> {
        State::host_biome_set(self, dimension, a, b, biome)
    }

    fn host_entities_in_box(
        &mut self,
        dimension: &str,
        a: Vec3,
        b: Vec3,
    ) -> value::Result<Result<Vec<Entity>, WorldError>> {
        State::host_entities_in_box(self, dimension, a, b)
    }

    fn host_block_normalize(&mut self, state: &str) -> value::Result<Option<String>> {
        State::host_block_normalize(self, state)
    }

    fn host_block_properties(&mut self, state: &str) -> value::Result<Option<Vec<Property>>> {
        State::host_block_properties(self, state)
    }

    fn host_block_tag(&mut self, tag: &str) -> value::Result<Option<Vec<String>>> {
        State::host_block_tag(self, tag)
    }

    fn host_item_tag(&mut self, tag: &str) -> value::Result<Option<Vec<String>>> {
        State::host_item_tag(self, tag)
    }

    fn host_block_transform(
        &mut self,
        state: &str,
        op: Transform,
    ) -> value::Result<Option<String>> {
        State::host_block_transform(self, state, op)
    }

    fn host_item_info(&mut self, item: &str) -> value::Result<Option<ItemDetails>> {
        State::host_item_info(self, item)
    }

    fn host_block_traits(&mut self, state: &str) -> value::Result<Option<Traits>> {
        State::host_block_traits(self, state)
    }

    fn host_add_block_rule(
        &mut self,
        rule: wit::BlockRule,
    ) -> value::Result<Result<u32, HostError>> {
        State::host_add_block_rule(self, rule)
    }

    fn host_add_change_rule(
        &mut self,
        rule: wit::ChangeRule,
    ) -> value::Result<Result<u32, HostError>> {
        State::host_add_change_rule(self, rule)
    }

    fn host_remove_block_rule(&mut self, id: u32) -> value::Result<bool> {
        State::host_remove_block_rule(self, id)
    }

    fn host_set_team(
        &mut self,
        player: Uuid,
        team: Option<String>,
    ) -> value::Result<Result<(), HostError>> {
        State::host_set_team(self, player, team)
    }

    fn host_fake_player_spawn(
        &mut self,
        spec: wit::FakeSpec,
    ) -> value::Result<Result<Uuid, wit::FakeError>> {
        State::host_fake_player_spawn(self, spec)
    }

    fn host_fake_player_remove(&mut self, fake: Uuid) -> value::Result<Result<(), wit::FakeError>> {
        State::host_fake_player_remove(self, fake)
    }

    fn host_fake_player_input(
        &mut self,
        fake: Uuid,
        input: wit::FakeInput,
    ) -> value::Result<Result<(), wit::FakeError>> {
        State::host_fake_player_input(self, fake, input)
    }

    fn host_fake_player_act(
        &mut self,
        fake: Uuid,
        action: wit::FakeAction,
    ) -> value::Result<Result<(), wit::FakeError>> {
        State::host_fake_player_act(self, fake, action)
    }

    fn host_fake_player_state(&mut self, fake: Uuid) -> value::Result<Option<wit::FakeState>> {
        State::host_fake_player_state(self, fake)
    }

    fn host_fake_player_inventory(&mut self, fake: Uuid) -> value::Result<Option<wit::Inventory>> {
        State::host_fake_player_inventory(self, fake)
    }

    fn host_fake_player_break_ticks(
        &mut self,
        fake: Uuid,
        state: &str,
    ) -> value::Result<Option<wit::BreakTool>> {
        State::host_fake_player_break_ticks(self, fake, state)
    }

    fn host_fake_players(&mut self) -> value::Result<Vec<Uuid>> {
        State::host_fake_players(self)
    }

    fn host_acting_damage(&mut self) -> value::Result<Option<wit::DamageSource>> {
        State::host_acting_damage(self)
    }

    fn host_acting_hit(&mut self) -> value::Result<Option<wit::Hit>> {
        State::host_acting_hit(self)
    }

    fn background_run(
        &mut self,
        task: &str,
        input: &[u8],
    ) -> value::Result<Result<Handle, Denied>> {
        State::background_run(self, task, input)
    }

    fn background_cancel(&mut self, task: Handle) -> value::Result<()> {
        State::background_cancel(self, task)
    }

    fn net_granted(&mut self, kind: CapabilityKind, target: &str) -> value::Result<bool> {
        State::net_granted(self, kind, target)
    }

    fn net_fetch(&mut self, request: HttpRequest) -> value::Result<Result<Handle, Denied>> {
        State::net_fetch(self, request)
    }

    fn net_ws_connect(
        &mut self,
        url: &str,
        headers: Vec<Header>,
        protocols: Vec<String>,
    ) -> value::Result<Result<Handle, Denied>> {
        State::net_ws_connect(self, url, headers, protocols)
    }

    fn net_ws_send(
        &mut self,
        socket: Handle,
        message: WsMessage,
    ) -> value::Result<Result<(), Denied>> {
        State::net_ws_send(self, socket, message)
    }

    fn net_tcp_connect(
        &mut self,
        host: &str,
        port: u16,
        tls: bool,
    ) -> value::Result<Result<Handle, Denied>> {
        State::net_tcp_connect(self, host, port, tls)
    }

    fn net_udp_connect(&mut self, host: &str, port: u16) -> value::Result<Result<Handle, Denied>> {
        State::net_udp_connect(self, host, port)
    }

    fn net_send(&mut self, socket: Handle, data: &[u8]) -> value::Result<Result<(), Denied>> {
        State::net_send(self, socket, data)
    }

    fn net_listen(
        &mut self,
        kind: CapabilityKind,
        bind: Option<String>,
        port: u16,
    ) -> value::Result<Result<Handle, Denied>> {
        State::net_listen(self, kind, bind, port)
    }

    fn net_reply_to(
        &mut self,
        listener: Handle,
        peer: &str,
        data: &[u8],
    ) -> value::Result<Result<(), Denied>> {
        State::net_reply_to(self, listener, peer, data)
    }

    fn net_respond(
        &mut self,
        request: Handle,
        response: RouteResponse,
    ) -> value::Result<Result<(), Denied>> {
        State::net_respond(self, request, response)
    }

    fn net_close(&mut self, target: Handle) -> value::Result<()> {
        State::net_close(self, target)
    }

    fn files_read(
        &mut self,
        path: &str,
        offset: u64,
        len: u32,
    ) -> value::Result<Result<Handle, Denied>> {
        State::files_read(self, path, offset, len)
    }

    fn files_write(
        &mut self,
        path: &str,
        mode: WriteMode,
        data: &[u8],
    ) -> value::Result<Result<Handle, Denied>> {
        State::files_write(self, path, mode, data)
    }

    fn files_list_dir(&mut self, path: &str) -> value::Result<Result<Handle, Denied>> {
        State::files_list_dir(self, path)
    }

    fn files_stat(&mut self, path: &str) -> value::Result<Result<Handle, Denied>> {
        State::files_stat(self, path)
    }

    fn files_remove(&mut self, path: &str) -> value::Result<Result<Handle, Denied>> {
        State::files_remove(self, path)
    }

    fn files_rename(&mut self, src: &str, dst: &str) -> value::Result<Result<Handle, Denied>> {
        State::files_rename(self, src, dst)
    }

    fn files_create_dir(&mut self, path: &str) -> value::Result<Result<Handle, Denied>> {
        State::files_create_dir(self, path)
    }

    fn files_usage(&mut self) -> value::Result<(u64, u64)> {
        State::files_usage(self)
    }
}

#[cfg(test)]
mod tests;
