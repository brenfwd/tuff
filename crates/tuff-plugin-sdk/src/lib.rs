// SPDX-License-Identifier: MIT
//! Write tuff runtime plugins in Rust. Build a `cdylib` for
//! `wasm32-unknown-unknown`, implement [`Plugin`], and export it with
//! [`plugin!`], which also writes the manifest section:
//!
//! ```ignore
//! use tuff_plugin_sdk::command::{argument, command, parser};
//! use tuff_plugin_sdk::events::{ev, on};
//! use tuff_plugin_sdk::{Plugin, plugin};
//!
//! struct Hello;
//!
//! impl Plugin for Hello {
//!     fn register() -> Result<(), String> {
//!         command("hello")
//!             .executes(|ctx| {
//!                 ctx.reply(format!("Hello, {}!", ctx.player()?.name))?;
//!                 Ok(1)
//!             })
//!             .then(argument("times", parser::integer_in(1..=3)).executes(|ctx| {
//!                 let times: i32 = ctx.get("times")?;
//!                 for _ in 0..times {
//!                     ctx.reply("Hello!")?;
//!                 }
//!                 Ok(times)
//!             }))
//!             .register()?;
//!         on::<ev::Join>(|player| {
//!             tuff_plugin_sdk::broadcast(&format!("{} joined", player.name));
//!         })?;
//!         Ok(())
//!     }
//! }
//!
//! plugin!(Hello { id: "hello", version: "1.0.0" });
//! ```
//!
//! [`command`] builds typed command trees (arguments parsed by the host
//! and read back with [`command::Ctx::get`] or an [`args!`] struct),
//! [`events`] has the typed events, and [`reply::Replier`] answers a
//! command's sender after an `await`.
//!
//! Capability calls are futures: run them in a task with [`spawn`]
//! (`spawn(async { net::get(url).await; })`) from any callback; see
//! [`net`], [`files`] and [`exec`]. Timer tokens with bit 30 set belong to
//! [`sleep`] and never reach [`Plugin::timer`].
//!
//! This is a layer over the bindings wit-bindgen generates from the plugin
//! WIT world in `crates/tuff-plugin-wasm/wit/`. The raw host imports exist only behind the
//! `raw-abi` cargo feature, for conformance fixtures:
//!
//! ```compile_fail
//! use tuff_plugin_sdk::host;
//! ```

#[doc(hidden)]
pub mod bindings {
    wit_bindgen::generate!({
        path: "../tuff-plugin-wasm/wit",
        world: "plugin-worldgen",
        features: ["experimental"],
        pub_export_macro: true,
        default_bindings_module: "tuff_plugin_sdk::bindings",
        additional_derives: [PartialEq],
    });
}

#[cfg(test)]
extern crate self as tuff_plugin_sdk;

#[cfg(test)]
#[allow(dead_code)] // generated fixture; tests use only part of it
#[rustfmt::skip]
mod iface_fixture;
#[cfg(test)]
mod iface_tests;

pub mod background;
pub mod combat;
pub mod command;
pub mod config;
pub mod dimensions;
pub mod events;
pub mod exec;
pub mod fake;
pub mod files;
mod generated;
pub mod items;
pub mod line_filter;
pub mod menu;
pub mod messaging;
pub mod net;
pub mod pack;
pub mod panel;
pub mod players;
pub mod prelude;
pub mod registry;
pub mod reply;
pub mod rules;
pub mod schedule;
pub mod sockets;
pub mod state;
pub mod store;
pub mod text;
pub mod wire;
pub mod world;
pub use world::RegistryError;

#[cfg(not(test))]
pub(crate) fn schedule_timer(delay: u64, token: i32) -> bool {
    host::schedule(delay, token as u32).is_ok()
}

#[cfg(test)]
pub(crate) fn schedule_timer(_: u64, _: i32) -> bool {
    true
}

#[cfg(feature = "raw-abi")]
#[doc(hidden)]
pub use bindings::tuff::plugin::host;
#[cfg(not(feature = "raw-abi"))]
use bindings::tuff::plugin::host;
pub use bindings::tuff::plugin::types;
pub use dimensions::{ContentBuilder, Worldgen};
pub use exec::{sleep, spawn};
pub use reply::Replier;

use bindings::tuff::plugin::types as wit;

pub use wit::FilterScope;
pub use wit::{
    AssetDecl as AssetSet, AssetFile, ContentDecl as Content, DataFile, EntityDecl as CustomEntity,
    ItemDecl as CustomItem, PartDecl,
};
pub use wit::{BiomeDecl as Biome, DimensionDecl as Dimension, GeneratedChunk as Chunk};

/// No custom content.
pub fn no_content() -> Content {
    Content {
        items: Vec::new(),
        entities: Vec::new(),
        assets: Vec::new(),
        recipes: Vec::new(),
        advancements: Vec::new(),
    }
}

/// The plugin ABI version this SDK speaks.
pub const API_VERSION: u32 = 0;

/// Events a plugin can [`subscribe`] to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A player joined; the payload is their name.
    Join,
    /// A player left; the payload is their name.
    Leave,
    /// A player chatted; the payload is the message.
    Chat,
    /// A player broke a block; the payload is `"x y z dimension state"`
    /// with the state before the break.
    BlockBreak,
    /// A player placed a block; the payload is `"x y z dimension state"`.
    BlockPlace,
    /// A click filtered with [`intercept_click`] was cancelled; the payload
    /// is `"x y z dimension face state"`.
    LeftClick,
    RightClick,
    /// A non-player entity spawned; the payload is
    /// `"uuid type x y z dimension"`.
    EntitySpawn,
    /// A mob died; same payload.
    EntityDeath,
    /// A mob was hurt and survived; same payload.
    EntityDamage,
    /// A player took damage; the payload is
    /// `"damage_type health_lost x y z dimension"`.
    PlayerHurt,
    /// A player died; the payload is `"damage_type x y z dimension"`.
    PlayerDeath,
    /// A player or entity entered a block claimed with [`watch_block`]; the
    /// payload is `"player <uuid> <name> x y z dimension state"` or
    /// `"entity <uuid> <type> x y z dimension state"`.
    InsideBlock,
    /// One of the plugin's settings changed; the payload is `"key value"`.
    ConfigChange,
    Login,
    /// A click on a plugin menu; the payload is `"uuid menu slot click"`.
    MenuClick,
    /// A plugin menu closed; the payload is `"uuid menu"`.
    MenuClose,
    /// A right-click on a non-player entity; the payload is
    /// `"uuid entity_uuid type cancelled"`.
    EntityInteract,
    /// A use in the air filtered with [`ClickKind::Air`] was cancelled; the
    /// payload is the item id.
    UseItem,
    /// A projectile struck something; the payload is
    /// `"uuid type x y z dimension"`, and [`rules::acting_hit`] says what
    /// it struck.
    ProjectileHit,
    /// A player attacked a non-player entity; the payload is
    /// `"uuid victim_uuid type cancelled"`.
    PlayerAttack,
    /// A hit on a player matched a damage filter; the payload is
    /// `"victim_uuid damage_type attacker_uuid_or_- scale"`.
    DamageFiltered,
    /// A player dropped an item; the payload is `"item count"`.
    ItemDrop,
    /// A player picked up a dropped item; same payload.
    ItemPickup,
    /// A player swapped their hands; same payload.
    ItemSwap,
    /// A player took a crafting result; same payload.
    ItemCraft,
    /// A `take-items` call removed items; the payload is
    /// `"item requested removed"`.
    ItemsTaken,
    /// An explosion went off; the payload is
    /// `"dimension x y z radius blocks"`.
    Explosion,
    /// A player clicked or dragged in a window; the payload is
    /// `"container slot click item count cancelled"` (item `-` when empty).
    InventoryClick,
    /// A container window opened for a player; the payload is the container.
    InventoryOpen,
    /// A container window closed; same payload.
    InventoryClose,
    /// A player state change; the payload is `"respawn dimension x y z"`,
    /// `"sneak true"`, `"game-mode before after cancelled"`,
    /// `"hunger before after locked"` or `"world before after"`.
    PlayerState,
    /// A client sent a custom payload; the payload is `"channel hex"`.
    Payload,
}

fn uuid_hex(uuid: wit::Uuid) -> String {
    uuid.to_string()
}

fn face_name(face: wit::Direction) -> &'static str {
    match face {
        wit::Direction::Down => "down",
        wit::Direction::Up => "up",
        wit::Direction::North => "north",
        wit::Direction::South => "south",
        wit::Direction::West => "west",
        wit::Direction::East => "east",
    }
}

/// The SDK's event and text payload for a WIT event.
#[doc(hidden)]
pub fn payload(event: wit::Event) -> (Event, String) {
    let entity = |e: wit::Entity| {
        let p = e.pos;
        format!(
            "{} {} {} {} {} {}",
            uuid_hex(e.uuid),
            e.kind,
            p.x,
            p.y,
            p.z,
            e.dimension
        )
    };
    let block = |c: wit::BlockChange| {
        let p = c.pos;
        format!("{} {} {} {} {}", p.x, p.y, p.z, c.dimension, c.state)
    };
    let click = |c: wit::Click| {
        let p = c.pos;
        let text = format!(
            "{} {} {} {} {} {}",
            p.x,
            p.y,
            p.z,
            c.dimension,
            face_name(c.face),
            c.state
        );
        text.trim_end().to_owned()
    };
    match event {
        wit::Event::Join(p) => (Event::Join, p.name),
        wit::Event::Leave(p) => (Event::Leave, p.name),
        wit::Event::Chat(c) => (Event::Chat, c.message),
        wit::Event::BlockBreak(c) => (Event::BlockBreak, block(c)),
        wit::Event::BlockPlace(c) => (Event::BlockPlace, block(c)),
        wit::Event::LeftClick(c) => (Event::LeftClick, click(c)),
        wit::Event::RightClick(c) => (Event::RightClick, click(c)),
        wit::Event::EntitySpawn(e) => (Event::EntitySpawn, entity(e)),
        wit::Event::EntityDeath(e) => (Event::EntityDeath, entity(e)),
        wit::Event::EntityDamage(e) => (Event::EntityDamage, entity(e)),
        wit::Event::PlayerHurt(d) => {
            let p = d.pos;
            let text = format!(
                "{} {} {} {} {} {}",
                d.damage_type, d.health_lost, p.x, p.y, p.z, d.dimension
            );
            (Event::PlayerHurt, text)
        }
        wit::Event::PlayerDeath(d) => {
            let p = d.pos;
            let text = format!("{} {} {} {} {}", d.damage_type, p.x, p.y, p.z, d.dimension);
            (Event::PlayerDeath, text)
        }
        wit::Event::InsideBlock(i) => {
            let p = i.pos;
            let who = if i.is_player { "player" } else { "entity" };
            let text = format!(
                "{who} {} {} {} {} {} {} {}",
                uuid_hex(i.uuid),
                i.name,
                p.x,
                p.y,
                p.z,
                i.dimension,
                i.state
            );
            (Event::InsideBlock, text)
        }
        wit::Event::ConfigChange(c) => {
            let text = format!("{} {}", c.key, Setting::from(c.value));
            (Event::ConfigChange, text)
        }
        wit::Event::Login(l) => (Event::Login, format!("{} {}", l.player.name, l.address)),
        wit::Event::MenuClick(c) => {
            let text = format!(
                "{} {} {} {:?}",
                uuid_hex(c.player.uuid),
                c.menu,
                c.slot,
                c.click
            );
            (Event::MenuClick, text)
        }
        wit::Event::MenuClose(c) => (
            Event::MenuClose,
            format!("{} {}", uuid_hex(c.player.uuid), c.menu),
        ),
        wit::Event::EntityInteract(i) => {
            let text = format!(
                "{} {} {} {}",
                uuid_hex(i.player.uuid),
                uuid_hex(i.entity),
                i.kind,
                i.cancelled
            );
            (Event::EntityInteract, text)
        }
        wit::Event::UseItem(u) => (Event::UseItem, u.item),
        wit::Event::ProjectileHit(e) => (Event::ProjectileHit, entity(e)),
        wit::Event::PlayerAttack(a) => {
            let text = format!(
                "{} {} {} {}",
                uuid_hex(a.player.uuid),
                uuid_hex(a.victim),
                a.kind,
                a.cancelled
            );
            (Event::PlayerAttack, text)
        }
        wit::Event::DamageFiltered(d) => {
            let attacker = d.attacker.map_or_else(|| "-".to_owned(), uuid_hex);
            let victim = match &d.victim {
                wit::DamageVictim::Player(p) => uuid_hex(p.uuid),
                wit::DamageVictim::Entity(e) => uuid_hex(e.uuid),
            };
            let text = format!("{} {} {} {}", victim, d.damage_type, attacker, d.scale);
            (Event::DamageFiltered, text)
        }
        wit::Event::ItemDrop(a) => (Event::ItemDrop, format!("{} {}", a.item, a.count)),
        wit::Event::ItemPickup(a) => (Event::ItemPickup, format!("{} {}", a.item, a.count)),
        wit::Event::ItemSwap(a) => (Event::ItemSwap, format!("{} {}", a.item, a.count)),
        wit::Event::ItemCraft(a) => (Event::ItemCraft, format!("{} {}", a.item, a.count)),
        wit::Event::ItemsTaken(t) => (
            Event::ItemsTaken,
            format!("{} {} {}", t.item, t.requested, t.removed),
        ),
        wit::Event::InventoryClick(c) => (
            Event::InventoryClick,
            format!(
                "{} {} {:?} {} {} {}",
                c.container,
                c.slot,
                c.click,
                if c.item.is_empty() { "-" } else { &c.item },
                c.count,
                c.cancelled
            ),
        ),
        wit::Event::Payload(m) => (
            Event::Payload,
            format!(
                "{} {}",
                m.channel,
                m.data
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            ),
        ),
        wit::Event::InventoryOpen(w) => (Event::InventoryOpen, w.container),
        wit::Event::InventoryClose(w) => (Event::InventoryClose, w.container),
        wit::Event::PlayerState(change) => (
            Event::PlayerState,
            match change {
                wit::StateChange::Respawn(r) => {
                    format!(
                        "respawn {} {} {} {}",
                        r.dimension, r.pos.x, r.pos.y, r.pos.z
                    )
                }
                wit::StateChange::Sneak(s) => format!("sneak {}", s.sneaking),
                wit::StateChange::GameMode(c) => {
                    format!("game-mode {} {} {}", c.before, c.after, c.cancelled)
                }
                wit::StateChange::Hunger(c) => {
                    format!("hunger {} {} {}", c.before, c.after, c.locked)
                }
                wit::StateChange::Dimension(c) => format!("world {} {}", c.before, c.after),
                wit::StateChange::Teleport(c) => format!(
                    "teleport {} {} {} {}",
                    c.after_dimension, c.after.x, c.after.z, c.cancelled
                ),
                wit::StateChange::Region(c) => format!("region {} {}", c.region, c.entered),
            },
        ),
        wit::Event::Explosion(e) => (
            Event::Explosion,
            format!(
                "{} {} {} {} {} {}",
                e.dimension, e.center.x, e.center.y, e.center.z, e.radius, e.blocks
            ),
        ),
    }
}

/// Why a host call was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostError {
    /// The call is only allowed once the plugin has registered.
    WrongPhase,
    /// No login (or no player) is being handled to act on.
    WrongContext,
    /// The call has no acting player.
    NoActingPlayer,
    /// A per-plugin or per-call count cap was reached.
    LimitReached,
    /// Text, a key, a payload or a quota is over its size cap.
    TooLarge,
    /// The setting or card series was never declared.
    NotDeclared,
    /// An argument that is not acceptable, with the reason.
    Invalid(String),
    /// The facility is not available right now.
    Unavailable,
    /// The owner has not granted the call's group.
    NotGranted,
}

impl core::fmt::Display for HostError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HostError::WrongPhase => f.write_str("not allowed in this phase"),
            HostError::WrongContext => f.write_str("nothing to act on in this context"),
            HostError::NoActingPlayer => f.write_str("no acting player"),
            HostError::LimitReached => f.write_str("limit reached"),
            HostError::TooLarge => f.write_str("too large"),
            HostError::NotDeclared => f.write_str("not declared"),
            HostError::Unavailable => f.write_str("unavailable"),
            HostError::NotGranted => f.write_str("not granted"),
            HostError::Invalid(reason) => write!(f, "invalid: {reason}"),
        }
    }
}

impl std::error::Error for HostError {}

impl From<types::HostError> for HostError {
    fn from(error: types::HostError) -> Self {
        match error {
            types::HostError::WrongPhase => HostError::WrongPhase,
            types::HostError::WrongContext => HostError::WrongContext,
            types::HostError::NoActingPlayer => HostError::NoActingPlayer,
            types::HostError::LimitReached => HostError::LimitReached,
            types::HostError::TooLarge => HostError::TooLarge,
            types::HostError::NotDeclared => HostError::NotDeclared,
            types::HostError::Unavailable => HostError::Unavailable,
            types::HostError::NotGranted => HostError::NotGranted,
            types::HostError::Invalid(reason) => HostError::Invalid(reason),
        }
    }
}

impl From<HostError> for String {
    fn from(error: HostError) -> Self {
        error.to_string()
    }
}

/// Why a world call was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorldError {
    UnknownDimension,
    /// Outside the world's height, a block state that does not parse, or
    /// the world did not answer.
    Unavailable,
    /// The call's read or write budget, or the box size limit, is spent.
    Limit,
    /// The advancement is outside the namespaces granted with `cap.content`.
    NotGranted,
}

impl From<wit::WorldError> for WorldError {
    fn from(error: wit::WorldError) -> Self {
        match error {
            wit::WorldError::NotGranted => WorldError::NotGranted,
            wit::WorldError::UnknownDimension => WorldError::UnknownDimension,
            wit::WorldError::OverBudget => WorldError::Limit,
            _ => WorldError::Unavailable,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

/// A plugin's callbacks, all optional.
pub trait Plugin {
    /// Claim commands and events; the only time `register_command`,
    /// [`command::Node::register`] and [`subscribe`] are allowed. An error
    /// refuses the load with that message.
    fn register() -> Result<(), String> {
        Ok(())
    }

    /// The plugin is live.
    fn enable() {}

    /// The plugin is being unloaded.
    fn disable() {}

    /// One of the plugin's commands ran; `line` has no slash.
    fn command(line: &str) -> i32 {
        let _ = line;
        0
    }

    /// A command run whose handler id no [`command::Node`] handler or
    /// `register_command` line claims: for trees registered through
    /// [`host`] directly.
    fn invoke(call: types::Invocation) -> command::Outcome {
        Err(format!("unknown handler {}", call.handler))
    }

    /// A subscribed event fired, as the host sent it.
    fn on_event(event: &events::Event) {
        let _ = event;
    }

    /// A timer from [`schedule`] is due.
    fn timer(token: i32) {
        let _ = token;
    }

    /// A capability event no pending call of the SDK's futures claimed
    /// (WebSocket and socket traffic until those get wrappers).
    fn io_event(event: exec::IoEvent) {
        let _ = event;
    }

    /// Completions for the argument node `node` of a command, whose
    /// `suggest` flag is set, given what the player typed of it; `line` is
    /// the command so far, without its slash.
    fn suggest(node: &str, partial: &str, line: &str) -> Vec<String> {
        let _ = (node, partial, line);
        Vec::new()
    }

    /// Another plugin notified or requested an interface this plugin
    /// provides (see [`messaging`]).
    fn message(msg: &messaging::Incoming) {
        let _ = msg;
    }

    /// A plugin this one declares in depends or soft-depends loaded or
    /// unloaded.
    fn plugin_change(id: &str, version: &str, loaded: bool) {
        let _ = (id, version, loaded);
    }

    /// Runs background task `task` (started with [`background::run`]) on
    /// a separate instance of the module where every host call traps, off
    /// the tick thread, so the result must follow from the arguments
    /// alone. Statics from earlier runs may or may not still be there.
    fn background(task: &str, input: &[u8]) -> Result<Vec<u8>, String> {
        let _ = input;
        Err(format!("no background task named {task}"))
    }

    /// Adds this plugin's dimensions with their generator closures. It
    /// runs on separate instances where every host call traps, so the
    /// closures must be deterministic in their arguments. Dimensions are
    /// only registered when the owner granted `cap.dimension`.
    fn worldgen(world: &mut Worldgen) {
        let _ = world;
    }

    /// Adds the custom items, entities, assets, recipes and
    /// advancements this plugin offers, asked once per world start on a
    /// separate instance where every host call traps. Only ids in
    /// namespaces the owner granted with `cap.content` are added, and
    /// they stay until the next start.
    fn content(content: ContentBuilder) -> ContentBuilder {
        content
    }
}

pub fn log(level: Level, text: &str) {
    let level = match level {
        Level::Error => wit::LogLevel::Error,
        Level::Warn => wit::LogLevel::Warn,
        Level::Info => wit::LogLevel::Info,
        Level::Debug => wit::LogLevel::Debug,
    };
    host::log(level, text);
}

/// Which click an [`intercept_click`] filter matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickKind {
    /// Starting to break a block.
    Left,
    /// Using the held item on a block.
    Right,
    /// Using the held item in the air, reported as [`Event::UseItem`];
    /// takes no block.
    Air,
}

/// Which item action an [`events::intercept_item`] filter matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemActionKind {
    Drop,
    Pickup,
    Swap,
    Craft,
}

/// A scope that applies everywhere to everyone.
pub fn scope_everywhere() -> wit::FilterScope {
    wit::FilterScope {
        dimension: None,
        area: None,
        bypass: None,
    }
}

pub(crate) fn item_filter(kind: ItemActionKind, item: &str, tag: &str) -> wit::ItemFilter {
    let some = |text: &str| (!text.is_empty()).then(|| text.to_owned());
    wit::ItemFilter {
        kind: match kind {
            ItemActionKind::Drop => wit::ItemActionKind::Drop,
            ItemActionKind::Pickup => wit::ItemActionKind::Pickup,
            ItemActionKind::Swap => wit::ItemActionKind::Swap,
            ItemActionKind::Craft => wit::ItemActionKind::Craft,
        },
        item: some(item),
        tag: some(tag),
        scope: scope_everywhere(),
    }
}

/// Which window click an [`events::intercept_inventory`] filter matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InventoryClickKind {
    Pickup,
    QuickMove,
    Swap,
    Clone,
    Drop,
    Drag,
    Double,
    /// Keeps the named container's window open when the player closes it.
    Close,
}

pub(crate) fn inventory_filter(
    container: &str,
    slots: Option<(i16, i16)>,
    item: &str,
    click: Option<InventoryClickKind>,
) -> wit::InventoryFilter {
    let some = |text: &str| (!text.is_empty()).then(|| text.to_owned());
    wit::InventoryFilter {
        container: some(container),
        first_slot: slots.map(|(first, _)| first),
        last_slot: slots.map(|(_, last)| last),
        item: some(item),
        click: click.map(|click| match click {
            InventoryClickKind::Pickup => wit::InventoryClickKind::Pickup,
            InventoryClickKind::QuickMove => wit::InventoryClickKind::QuickMove,
            InventoryClickKind::Swap => wit::InventoryClickKind::Swap,
            InventoryClickKind::Clone => wit::InventoryClickKind::Clone,
            InventoryClickKind::Drop => wit::InventoryClickKind::Drop,
            InventoryClickKind::Drag => wit::InventoryClickKind::Drag,
            InventoryClickKind::Double => wit::InventoryClickKind::Double,
            InventoryClickKind::Close => wit::InventoryClickKind::Close,
        }),
    }
}

/// Which state change an [`events::intercept_state`] filter refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateKind {
    GameMode,
    HungerLoss,
    Teleport,
}

pub(crate) fn state_filter(
    kind: StateKind,
    player: Option<wit::Uuid>,
    dimension: &str,
    game_mode: &str,
) -> wit::StateFilter {
    let some = |text: &str| (!text.is_empty()).then(|| text.to_owned());
    wit::StateFilter {
        kind: match kind {
            StateKind::GameMode => wit::StateKind::GameMode,
            StateKind::HungerLoss => wit::StateKind::HungerLoss,
            StateKind::Teleport => wit::StateKind::Teleport,
        },
        player,
        dimension: some(dimension),
        game_mode: some(game_mode),
    }
}

pub(crate) fn click_filter(
    click: ClickKind,
    item: &str,
    tag: &str,
    block: &str,
) -> wit::ClickFilter {
    let some = |text: &str| (!text.is_empty()).then(|| text.to_owned());
    wit::ClickFilter {
        kind: match click {
            ClickKind::Left => wit::ClickKind::Left,
            ClickKind::Right => wit::ClickKind::Right,
            ClickKind::Air => wit::ClickKind::Air,
        },
        item: some(item),
        tag: some(tag),
        block: some(block),
        scope: scope_everywhere(),
    }
}

fn uuid_of(uuid: &str) -> Option<wit::Uuid> {
    (uuid.len() == 32).then(|| wit::Uuid::parse(uuid)).flatten()
}

fn rotation_of(rotation: Option<(f32, f32)>) -> Option<wit::Rotation> {
    rotation.map(|(yaw, pitch)| wit::Rotation { yaw, pitch })
}

/// Queues moving the online player `uuid` (32 hex digits) to `at` in
/// `dimension`, optionally turning them to `(yaw, pitch)`.
pub fn teleport_player(
    uuid: &str,
    dimension: &str,
    at: [f64; 3],
    rotation: Option<(f32, f32)>,
) -> Result<(), WorldError> {
    let uuid = uuid_of(uuid).ok_or(WorldError::Unavailable)?;
    let pos = wit::Vec3 {
        x: at[0],
        y: at[1],
        z: at[2],
    };
    host::teleport_player(uuid, dimension, pos, rotation_of(rotation)).map_err(WorldError::from)
}

/// Queues moving the loaded non-player entity `uuid` (32 hex digits).
pub fn teleport_entity(
    uuid: &str,
    dimension: &str,
    at: [f64; 3],
    rotation: Option<(f32, f32)>,
) -> Result<(), WorldError> {
    let uuid = uuid_of(uuid).ok_or(WorldError::Unavailable)?;
    let pos = wit::Vec3 {
        x: at[0],
        y: at[1],
        z: at[2],
    };
    host::teleport_entity(uuid, dimension, pos, rotation_of(rotation)).map_err(WorldError::from)
}

/// One entity for [`glide_entities`]: uuid as 32 hex digits, feet, and
/// yaw and pitch.
pub type Glide<'a> = (&'a str, [f64; 3], Option<(f32, f32)>);

/// Queues moving loaded non-player entities (uuid as 32 hex digits) as
/// steps of their own, so viewers see relative moves instead of a teleport.
pub fn glide_entities(moves: &[Glide<'_>]) -> Result<(), WorldError> {
    let moves = moves
        .iter()
        .map(|(uuid, at, rotation)| {
            Ok(host::EntityGlide {
                entity: uuid_of(uuid).ok_or(WorldError::Unavailable)?,
                pos: wit::Vec3 {
                    x: at[0],
                    y: at[1],
                    z: at[2],
                },
                rotation: rotation_of(*rotation),
            })
        })
        .collect::<Result<Vec<_>, WorldError>>()?;
    host::glide_entities(&moves).map_err(WorldError::from)
}

/// The state of each loaded non-player entity (uuid as 32 hex digits), in
/// order; none for one that is not loaded.
pub fn entity_states(uuids: &[&str]) -> Result<Vec<Option<host::EntityState>>, WorldError> {
    let uuids = uuids
        .iter()
        .map(|uuid| uuid_of(uuid).ok_or(WorldError::Unavailable))
        .collect::<Result<Vec<_>, _>>()?;
    host::entity_states(&uuids).map_err(WorldError::from)
}

/// Queues moving the acting player to `at` within their dimension.
pub fn teleport_self(at: [f64; 3], rotation: Option<(f32, f32)>) -> Result<(), WorldError> {
    let pos = wit::Vec3 {
        x: at[0],
        y: at[1],
        z: at[2],
    };
    host::teleport_self(pos, rotation_of(rotation)).map_err(WorldError::from)
}

/// Messages every player; false when the per-call limit is reached.
pub fn broadcast(text: &str) -> bool {
    host::broadcast(text).is_ok()
}

/// A setting's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Setting {
    Bool(bool),
    Int(i64),
    Text(String),
}

impl From<wit::ConfigValue> for Setting {
    fn from(value: wit::ConfigValue) -> Self {
        match value {
            wit::ConfigValue::Boolean(b) => Setting::Bool(b),
            wit::ConfigValue::Integer(i) => Setting::Int(i),
            wit::ConfigValue::Text(t) => Setting::Text(t),
        }
    }
}

impl From<Setting> for wit::ConfigValue {
    fn from(value: Setting) -> Self {
        match value {
            Setting::Bool(b) => wit::ConfigValue::Boolean(b),
            Setting::Int(i) => wit::ConfigValue::Integer(i),
            Setting::Text(t) => wit::ConfigValue::Text(t),
        }
    }
}

impl std::fmt::Display for Setting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Setting::Bool(b) => write!(f, "{b}"),
            Setting::Int(i) => write!(f, "{i}"),
            Setting::Text(t) => f.write_str(t),
        }
    }
}

fn text_in<'a>(buf: &'a mut [u8], text: &str) -> Option<&'a str> {
    let out = buf.get_mut(..text.len())?;
    out.copy_from_slice(text.as_bytes());
    std::str::from_utf8(out).ok()
}

fn pos([x, y, z]: [i32; 3]) -> wit::BlockPos {
    wit::BlockPos { x, y, z }
}

/// The block state at a position in `dimension` (`overworld` or a full
/// key), written into `buf`; loads the chunk.
pub fn block_get<'a>(
    dimension: &str,
    at: [i32; 3],
    buf: &'a mut [u8],
) -> Result<&'a str, WorldError> {
    let state = host::block_get(dimension, pos(at))?;
    text_in(buf, &state).ok_or(WorldError::Limit)
}

/// Queues setting a block, applied after the callback returns; `updates`
/// false skips neighbour updates.
pub fn block_set(
    dimension: &str,
    at: [i32; 3],
    state: &str,
    updates: bool,
) -> Result<(), WorldError> {
    let flags = match updates {
        true => wit::SetFlags::empty(),
        false => wit::SetFlags::NO_UPDATES,
    };
    Ok(host::block_set(dimension, pos(at), state, flags)?)
}

/// The biome name of the 4x4x4 cell holding `at`, written into `buf`;
/// loads the chunk.
pub fn biome_get<'a>(
    dimension: &str,
    at: [i32; 3],
    buf: &'a mut [u8],
) -> Result<&'a str, WorldError> {
    let name = host::biome_get(dimension, pos(at))?;
    text_in(buf, &name).ok_or(WorldError::Limit)
}

/// Queues setting the biome of every 4x4x4 cell holding a block of the box
/// from `a` to `b`, applied after the callback returns.
pub fn biome_set(dimension: &str, a: [i32; 3], b: [i32; 3], biome: &str) -> Result<(), WorldError> {
    Ok(host::biome_set(dimension, pos(a), pos(b), biome)?)
}

/// Text formatted into a fixed buffer, without allocating; output past
/// `N` bytes is cut at a character boundary.
pub struct Text<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> Text<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or_default()
    }
}

impl<const N: usize> Default for Text<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> std::fmt::Write for Text<N> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let room = N - self.len;
        let mut take = s.len().min(room);
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        if take < s.len() {
            Err(std::fmt::Error)
        } else {
            Ok(())
        }
    }
}

/// Guest export glue behind [`plugin!`]; not an authoring surface.
#[doc(hidden)]
#[macro_export]
macro_rules! export_plugin {
    (@guest $plugin:ty) => {
        struct TuffPluginGuest;

        impl $crate::bindings::exports::tuff::plugin::guest::Guest for TuffPluginGuest {
            fn register() -> Result<(), String> {
                $crate::reply::enter();
                <$plugin as $crate::Plugin>::register()
            }

            fn enable() {
                $crate::reply::enter();
                $crate::schedule::enabled();
                <$plugin as $crate::Plugin>::enable();
            }

            fn disable() {
                $crate::reply::enter();
                $crate::schedule::disabled();
                <$plugin as $crate::Plugin>::disable();
            }

            fn run_command(call: $crate::types::Invocation) -> Result<i32, String> {
                $crate::reply::enter();
                match $crate::command::dispatch(call) {
                    Ok(outcome) => outcome,
                    Err(call) if $crate::command::is_line(&call) => {
                        Ok(<$plugin as $crate::Plugin>::command(&call.line))
                    }
                    Err(call) => <$plugin as $crate::Plugin>::invoke(*call),
                }
            }

            fn suggest(req: $crate::types::SuggestRequest) -> Vec<String> {
                $crate::reply::enter();
                match $crate::command::suggest(req) {
                    Ok(found) => found,
                    Err(req) => {
                        <$plugin as $crate::Plugin>::suggest(&req.node, &req.partial, &req.line)
                    }
                }
            }

            fn on_event(ev: $crate::types::Event) {
                $crate::reply::enter();
                $crate::events::dispatch(&ev);
                <$plugin as $crate::Plugin>::on_event(&ev);
            }

            fn on_timer(token: u32) {
                $crate::reply::enter();
                if !$crate::exec::timer(token) {
                    <$plugin as $crate::Plugin>::timer(token as i32);
                }
            }

            fn on_message(msg: $crate::messaging::Incoming) {
                $crate::reply::enter();
                if let Some(msg) = $crate::messaging::dispatch(msg) {
                    <$plugin as $crate::Plugin>::message(&msg);
                }
            }
        }

        impl $crate::bindings::exports::tuff::plugin::worldgen::Guest for TuffPluginGuest {
            fn declare_dimensions() -> Vec<$crate::Dimension> {
                let mut world = $crate::dimensions::Worldgen::default();
                <$plugin as $crate::Plugin>::worldgen(&mut world);
                world.declarations()
            }

            fn generate_chunk(
                dimension: String,
                seed: i64,
                chunk_x: i32,
                chunk_z: i32,
                min_y: i32,
                height: i32,
            ) -> Result<$crate::Chunk, String> {
                let mut world = $crate::dimensions::Worldgen::default();
                <$plugin as $crate::Plugin>::worldgen(&mut world);
                world.generate(&$crate::dimensions::GenArgs {
                    dimension: &dimension,
                    seed,
                    chunk_x,
                    chunk_z,
                    min_y,
                    height,
                })
            }
        }

        impl $crate::bindings::exports::tuff::plugin::background_tasks::Guest
            for TuffPluginGuest
        {
            fn run_task(task: String, input: Vec<u8>) -> Result<Vec<u8>, String> {
                <$plugin as $crate::Plugin>::background(&task, &input)
            }
        }

        impl $crate::bindings::exports::tuff::plugin::content::Guest for TuffPluginGuest {
            fn declare_content() -> $crate::Content {
                <$plugin as $crate::Plugin>::content($crate::dimensions::ContentBuilder::new())
                    .build()
            }
        }

        impl $crate::bindings::exports::tuff::plugin::capability_events::Guest
            for TuffPluginGuest
        {
            fn on_io(event: $crate::exec::IoEvent) {
                $crate::reply::enter();
                if let $crate::exec::IoEvent::PluginChange(change) = event {
                    <$plugin as $crate::Plugin>::plugin_change(
                        &change.id,
                        &change.version,
                        change.loaded,
                    );
                } else if let Some(event) = $crate::exec::dispatch(event) {
                    <$plugin as $crate::Plugin>::io_event(event);
                }
            }
        }

        $crate::bindings::export!(TuffPluginGuest with_types_in $crate::bindings);
    };
}

/// Exports plugin type `$plugin` with its manifest written as fields;
/// `api` is added for you; `caps` lists typed capability requests
/// (`files`, `http`, `websocket`, `tcp`, `udp`, `route`, `listen_tcp`,
/// `listen_udp`, `dimension`, `content`, each with an optional reason) and
/// `requests` lists raw capability lines. A plugin
/// that adds dimensions may claim one as the spawn dimension with
/// `spawn_dimension: "ns:key"` (honoured only for a dimension it was
/// granted; the operator's `tuff.world.spawnDimension` wins):
///
/// ```ignore
/// tuff_plugin_sdk::plugin!(Hello {
///     id: "hello",
///     version: "1.0.0",
///     description: "Says hello",
///     depends: "economy >= 1.2.0",
///     soft_depends: "bots",
///     conflicts: "legacy-hello",
///     load_before: "greeter",
///     caps: [
///         http("api.example.com/", "Fetches greetings"),
///         files("16 MiB"),
///     ],
/// });
/// ```
#[macro_export]
macro_rules! plugin {
    (@line requests [$($line:literal),* $(,)?]) => {
        ::core::concat!($($line, "\n"),*)
    };
    (@line caps [$($kind:ident($value:literal $(, $reason:literal)?)),* $(,)?]) => {
        ::core::concat!($(
            "cap.", $crate::plugin!(@kind $kind), " = ", $value,
            $(" -- ", $reason,)? "\n"
        ),*)
    };
    (@kind listen_tcp) => { "listen-tcp" };
    (@kind listen_udp) => { "listen-udp" };
    (@kind files) => { "files" };
    (@kind http) => { "http" };
    (@kind websocket) => { "websocket" };
    (@kind tcp) => { "tcp" };
    (@kind udp) => { "udp" };
    (@kind route) => { "route" };
    (@kind dimension) => { "dimension" };
    (@kind content) => { "content" };
    (@line soft_depends $value:literal) => {
        ::core::concat!("soft-depends = ", $value, "\n")
    };
    (@line load_before $value:literal) => {
        ::core::concat!("load-before = ", $value, "\n")
    };
    (@line $key:ident $value:literal) => {
        ::core::concat!(::core::stringify!($key), " = ", $value, "\n")
    };
    ($plugin:ty { $($key:ident : $value:tt),* $(,)? }) => {
        const _: () = ::core::assert!($crate::API_VERSION == 0);
        const TUFF_PLUGIN_MANIFEST_TEXT: &str =
            ::core::concat!($($crate::plugin!(@line $key $value),)* "api = 0\n");

        #[unsafe(link_section = "tuff-plugin")]
        #[used]
        static TUFF_PLUGIN_MANIFEST: [u8; TUFF_PLUGIN_MANIFEST_TEXT.len()] =
            $crate::manifest_bytes(TUFF_PLUGIN_MANIFEST_TEXT);

        $crate::export_plugin!(@guest $plugin);
    };
}

/// `text` as a byte array, for [`plugin!`]'s manifest section.
#[doc(hidden)]
pub const fn manifest_bytes<const N: usize>(text: &str) -> [u8; N] {
    let bytes = text.as_bytes();
    let mut out = [0u8; N];
    let mut i = 0;
    while i < N {
        out[i] = bytes[i];
        i += 1;
    }
    out
}

/// Declares a struct of command arguments for
/// [`command::Node::with`]: each field becomes an argument node of that
/// name, in order, parsed by its type's default parser (see
/// [`command::ArgType`]) or by the parser after `=`. `Option` fields may be
/// left out by the player.
///
/// ```ignore
/// tuff_plugin_sdk::args! {
///     struct Greet {
///         target: Player,
///         times: Option<i32> = parser::integer_in(1..=3),
///     }
/// }
/// ```
#[macro_export]
macro_rules! args {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $(
                $(#[$fmeta:meta])*
                $fvis:vis $field:ident : $ty:ty $(= $parser:expr)?
            ),* $(,)?
        }
    ) => {
        $(#[$meta])*
        $vis struct $name {
            $($(#[$fmeta])* $fvis $field: $ty,)*
        }

        impl $crate::command::FromArgs for $name {
            fn spec() -> ::std::vec::Vec<$crate::command::ArgSpec> {
                ::std::vec![$($crate::command::ArgSpec {
                    name: ::core::stringify!($field),
                    parser: $crate::args!(@parser $ty $(, $parser)?),
                    optional: <$ty as $crate::command::ArgType>::OPTIONAL,
                }),*]
            }

            fn from_args(
                args: &$crate::command::Args,
            ) -> ::core::result::Result<Self, ::std::string::String> {
                ::core::result::Result::Ok(Self {
                    $($field: args.get::<$ty>(::core::stringify!($field))?,)*
                })
            }
        }
    };
    (@parser $ty:ty) => {
        <$ty as $crate::command::ArgType>::parser()
    };
    (@parser $ty:ty, $parser:expr) => {
        $parser
    };
}

#[cfg(test)]
mod caps_tests {
    #[tuff_core::watchdog]
    #[test]
    fn typed_caps_render_manifest_lines() {
        let text = plugin!(@line caps [
            http("a.example/", "why"),
            files("1 MiB"),
            listen_tcp("25570"),
        ]);
        assert_eq!(
            text,
            "cap.http = a.example/ -- why\ncap.files = 1 MiB\ncap.listen-tcp = 25570\n"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::bindings::tuff::plugin::capability_types as cap;
    use super::exec::{Call, Completion, testing};
    use super::*;

    fn data(bytes: &[u8]) -> Completion {
        Completion::File(Ok(cap::FileResult::Data(bytes.to_vec())))
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_task_waits_for_its_event() {
        let seen = Rc::new(Cell::new(0usize));
        let out = seen.clone();
        spawn(async move {
            let Completion::File(Ok(cap::FileResult::Data(d))) = Call::file(7).await else {
                panic!("wrong completion");
            };
            out.set(d.len());
        });
        assert_eq!(seen.get(), 0);
        assert!(testing::complete(7, data(b"abc")).is_none());
        assert_eq!(seen.get(), 3);
    }

    #[tuff_core::watchdog]
    #[test]
    fn events_for_unknown_handles_are_handed_back() {
        let event = cap::IoEvent::Closed(cap::SocketClosed {
            socket: 1,
            error: None,
            code: None,
            reason: String::new(),
        });
        assert!(exec::dispatch(event).is_some());
        assert!(testing::complete(99, data(b"")).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn tasks_spawn_tasks_and_dropped_calls_ignore_late_events() {
        let order = Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = order.clone();
        spawn(async move {
            let inner = log.clone();
            spawn(async move {
                Call::file(21).await;
                inner.borrow_mut().push("inner");
            });
            Call::file(20).await;
            log.borrow_mut().push("outer");
            let dropped = Call::file(22);
            drop(dropped);
        });
        testing::complete(20, data(b""));
        testing::complete(21, data(b""));
        testing::complete(22, data(b""));
        assert_eq!(*order.borrow(), ["outer", "inner"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn calls_of_different_kinds_sharing_a_handle_do_not_collide() {
        let got = Rc::new(Cell::new(0u8));
        let (a, b) = (got.clone(), got.clone());
        spawn(async move {
            if let Completion::Http(_) = Call::http(5).await {
                a.set(a.get() + 1);
            }
        });
        spawn(async move {
            if let Completion::File(_) = Call::file(5).await {
                b.set(b.get() + 10);
            }
        });
        testing::complete(5, data(b""));
        assert_eq!(got.get(), 10);
        testing::complete(
            5,
            Completion::Http(Err(cap::Denied::Malformed(String::new()))),
        );
        assert_eq!(got.get(), 11);
    }

    #[tuff_core::watchdog]
    #[test]
    fn timers_below_the_base_are_the_plugins() {
        assert!(!exec::timer(5));
    }

    #[tuff_core::watchdog]
    #[test]
    fn json_strings_escape() {
        assert_eq!(net::json_string("a\"b\\\n"), "\"a\\\"b\\\\\\n\"");
    }
}
