// SPDX-License-Identifier: MIT
//! Typed events. While registering, attach a handler to one event case
//! with [`on`] (or [`listen`] to filter first); it is subscribed for you
//! and gets the case's record:
//!
//! ```ignore
//! use tuff_plugin_sdk::events::{ev, listen, on};
//!
//! on::<ev::Join>(|player| broadcast(&format!("{} joined", player.name)))?;
//! listen::<ev::BlockBreak>()
//!     .in_dimension("minecraft:overworld")
//!     .block("minecraft:diamond_ore")
//!     .run(|change| log(Level::Info, "diamonds!"))?;
//! ```
//!
//! At the lower level, [`subscribe`] to a set of
//! [`EventKinds`] and match each [`Event`] in [`crate::Plugin::on_event`].
//! The markers in [`ev`] and the field accessors are generated from the
//! WIT, as are the C++ SDK's.

use std::cell::RefCell;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::bindings::tuff::plugin::host;
pub use crate::generated::{ev, kind};
pub use crate::types::{
    BlockChange, ChatMessage, Click, ConfigChange, Damage, Death, Direction, Entity, Event,
    EventKinds, Inside, ItemUse, LoginAttempt,
};
use crate::types::{Player, RegisterError, Uuid};

/// While registering: cancels `click` on a block made holding `item` (an
/// item id, empty for any) with `custom_data` top-level key `tag` (empty
/// for any) and clicked block `block` (empty for any). At most 16 filters.
pub fn intercept_click(
    click: crate::ClickKind,
    item: &str,
    tag: &str,
    block: &str,
) -> Result<(), RegisterError> {
    host::intercept_click(&crate::click_filter(click, item, tag, block))
}

/// [`intercept_click`] limited by `scope`: a dimension, a box around the
/// player, and a permission node whose holders are never matched.
pub fn intercept_click_scoped(
    click: crate::ClickKind,
    item: &str,
    tag: &str,
    block: &str,
    scope: crate::FilterScope,
) -> Result<(), RegisterError> {
    let mut filter = crate::click_filter(click, item, tag, block);
    filter.scope = scope;
    host::intercept_click(&filter)
}

/// While registering: cancels the item action `kind` on a stack of `item`
/// (an item id, empty for any) with `custom_data` top-level key `tag` (empty
/// for any), and reports it as an event. At most 16 filters.
pub fn intercept_item(
    kind: crate::ItemActionKind,
    item: &str,
    tag: &str,
) -> Result<(), RegisterError> {
    host::intercept_item(&crate::item_filter(kind, item, tag))
}

/// [`intercept_item`] limited by `scope`.
pub fn intercept_item_scoped(
    kind: crate::ItemActionKind,
    item: &str,
    tag: &str,
    scope: crate::FilterScope,
) -> Result<(), RegisterError> {
    let mut filter = crate::item_filter(kind, item, tag);
    filter.scope = scope;
    host::intercept_item(&filter)
}

/// While registering: cancels window clicks in `container` (empty for any;
/// `player`, `chest`, `furnace`, ...), over the inclusive `slots` range
/// (none for any), on `item` (empty for any), of kind `click` (none for any),
/// and reports every window click as an event. At most 16 filters.
pub fn intercept_inventory(
    container: &str,
    slots: Option<(i16, i16)>,
    item: &str,
    click: Option<crate::InventoryClickKind>,
) -> Result<(), RegisterError> {
    host::intercept_inventory(&crate::inventory_filter(container, slots, item, click))
}

/// While registering: refuses state changes of `kind` (game mode changes
/// into `game_mode`, empty for any; food losses) for `player` (none for
/// anyone) in `dimension` (empty for any), and reports them as events.
/// At most 16 filters.
pub fn intercept_state(
    kind: crate::StateKind,
    player: Option<crate::wit::Uuid>,
    dimension: &str,
    game_mode: &str,
) -> Result<(), RegisterError> {
    host::intercept_state(&crate::state_filter(kind, player, dimension, game_mode))
}

/// While registering: receives client custom payloads on `channel`
/// (`namespace:path`, not `minecraft:`) as [`ev::Payload`] events and may
/// send on it with [`send_payload`]. At most 16 channels.
pub fn register_channel(channel: &str) -> Result<(), RegisterError> {
    host::register_channel(channel)
}

/// Sends a custom payload (at most 32767 bytes) on a registered channel
/// to the online player with this hex uuid.
pub fn send_payload(player: &str, channel: &str, data: &[u8]) -> Result<(), crate::HostError> {
    let uuid = crate::uuid_of(player).ok_or(crate::HostError::Invalid("bad uuid".into()))?;
    host::send_payload(uuid, channel, data).map_err(Into::into)
}

/// While registering: claims blocks with id `block` as this plugin's portals.
/// Reports players entering and leaving a box as `region` state events.
pub fn watch_region(region: &crate::types::RegionWatch) -> Result<(), RegisterError> {
    host::watch_region(region)
}

pub fn watch_block(block: &str) -> Result<(), RegisterError> {
    host::watch_block(block)
}

/// One case of [`Event`], named by its marker in [`ev`].
pub trait Case: 'static {
    const KIND: EventKinds;
    type Payload: 'static;

    /// The payload when `event` is of this case.
    fn get(event: &Event) -> Option<&Self::Payload>;
}

/// A payload that happened in a dimension.
pub trait HasDimension {
    fn dimension(&self) -> &str;
}

/// A payload about a block state (`namespace:name[properties]`).
pub trait HasState {
    fn state(&self) -> &str;
}

/// A payload that may name the player behind it.
pub trait HasPlayer {
    fn player(&self) -> Option<&Player>;
}

type Listener = Rc<dyn Fn(&Event)>;
type Filter<P> = Box<dyn Fn(&P) -> bool>;

thread_local! {
    static LISTENERS: RefCell<Vec<Listener>> = const { RefCell::new(Vec::new()) };
}

/// A handler for case `E` being set up: add filters, then [`Listen::run`].
#[must_use = "a listener does nothing until `run` installs it"]
pub struct Listen<E: Case> {
    filters: Vec<Filter<E::Payload>>,
    case: PhantomData<E>,
}

/// Starts a handler for events of case `E`.
pub fn listen<E: Case>() -> Listen<E> {
    Listen {
        filters: Vec::new(),
        case: PhantomData,
    }
}

/// Runs `handler` for every event of case `E`. Only while registering.
pub fn on<E: Case>(handler: impl Fn(&E::Payload) + 'static) -> Result<(), RegisterError> {
    listen::<E>().run(handler)
}

impl<E: Case> Listen<E> {
    /// Only events for which `filter` holds.
    pub fn when(mut self, filter: impl Fn(&E::Payload) -> bool + 'static) -> Self {
        self.filters.push(Box::new(filter));
        self
    }

    /// Subscribes and installs `handler`. Only while registering.
    pub fn run(self, handler: impl Fn(&E::Payload) + 'static) -> Result<(), RegisterError> {
        host::subscribe(E::KIND)?;
        self.install(handler);
        Ok(())
    }

    fn install(self, handler: impl Fn(&E::Payload) + 'static) {
        let Listen { filters, .. } = self;
        let listener: Listener = Rc::new(move |event| {
            if let Some(payload) = E::get(event)
                && filters.iter().all(|f| f(payload))
            {
                handler(payload);
            }
        });
        LISTENERS.with(|l| l.borrow_mut().push(listener));
    }
}

impl<E: Case> Listen<E>
where
    E::Payload: HasDimension,
{
    /// Only events in dimension `key`, such as `minecraft:overworld`.
    pub fn in_dimension(self, key: impl Into<String>) -> Self {
        let key = key.into();
        self.when(move |p| p.dimension() == key)
    }
}

impl<E: Case> Listen<E>
where
    E::Payload: HasState,
{
    /// Only events about block `id` (any properties); `stone` means
    /// `minecraft:stone`.
    pub fn block(self, id: impl Into<String>) -> Self {
        let id = id.into();
        self.when(move |p| same_block(p.state(), &id))
    }
}

impl<E: Case> Listen<E>
where
    E::Payload: HasPlayer,
{
    /// Only events a player caused.
    pub fn by_players(self) -> Self {
        self.when(|p| p.player().is_some())
    }
}

/// Whether block state `state` is of block `id`.
pub fn same_block(state: &str, id: &str) -> bool {
    let name = state.split('[').next().unwrap_or(state);
    let strip = |s: &str| s.strip_prefix("minecraft:").unwrap_or(s).to_owned();
    strip(name) == strip(id)
}

/// Runs the handlers [`on`] and [`listen`] installed;
/// [`crate::plugin!`] calls it for every event.
#[doc(hidden)]
pub fn dispatch(event: &Event) {
    let listeners = LISTENERS.with(|l| l.borrow().clone());
    for listener in listeners {
        listener(event);
    }
}

/// Refuses the login a [`Event::Login`] reports, with `reason` shown to the
/// player. Only while handling that event.
pub fn refuse_login(reason: &str) -> Result<(), crate::HostError> {
    host::refuse_login(reason).map_err(Into::into)
}

impl Uuid {
    pub const fn from_u128(id: u128) -> Self {
        Self {
            high: (id >> 64) as u64,
            low: id as u64,
        }
    }

    pub const fn as_u128(self) -> u128 {
        ((self.high as u128) << 64) | self.low as u128
    }

    /// Reads 32 hex digits, with or without the four dashes.
    pub fn parse(text: &str) -> Option<Self> {
        let digits: String = match text.len() {
            36 => text.chars().filter(|&c| c != '-').collect(),
            _ => text.to_owned(),
        };
        if digits.len() != 32 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u128::from_str_radix(&digits, 16).ok().map(Self::from_u128)
    }
}

/// 32 lowercase hex digits, no dashes.
impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}{:016x}", self.high, self.low)
    }
}

impl Eq for Uuid {}

impl std::hash::Hash for Uuid {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_u128().hash(state);
    }
}

impl From<RegisterError> for String {
    fn from(error: RegisterError) -> Self {
        error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Player;

    #[tuff_core::watchdog]
    #[test]
    fn uuids_print_and_parse_as_hex() {
        let uuid = Uuid::from_u128(0x0123_4567_89ab_cdef_0011_2233_4455_6677);
        let text = uuid.to_string();
        assert_eq!(text, "0123456789abcdef0011223344556677");
        assert_eq!(Uuid::parse(&text), Some(uuid));
        assert_eq!(
            Uuid::parse("01234567-89ab-cdef-0011-223344556677"),
            Some(uuid)
        );
        assert_eq!(uuid.as_u128(), 0x0123_4567_89ab_cdef_0011_2233_4455_6677);
        assert_eq!(Uuid::parse("+123456789abcdef0011223344556677"), None);
        assert_eq!(Uuid::parse("abc"), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn events_know_their_kind() {
        let player = Player {
            uuid: Uuid::from_u128(1),
            name: "Alex".into(),
        };
        assert_eq!(kind(&Event::Leave(player)), EventKinds::LEAVE);
    }

    fn broken(state: &str, dimension: &str) -> Event {
        Event::BlockBreak(BlockChange {
            player: None,
            pos: crate::types::BlockPos { x: 0, y: 0, z: 0 },
            dimension: dimension.into(),
            state: state.into(),
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn listeners_see_only_their_case_and_filters() {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        listen::<ev::BlockBreak>()
            .in_dimension("minecraft:overworld")
            .block("diamond_ore")
            .install(move |c| log.borrow_mut().push(c.state.clone()));
        dispatch(&broken("minecraft:stone", "minecraft:overworld"));
        dispatch(&broken("minecraft:diamond_ore", "minecraft:the_nether"));
        dispatch(&broken("minecraft:diamond_ore", "minecraft:overworld"));
        dispatch(&broken("minecraft:diamond_ore[x=1]", "minecraft:overworld"));
        dispatch(&Event::Join(Player {
            uuid: Uuid::from_u128(1),
            name: "Alex".into(),
        }));
        assert_eq!(
            *seen.borrow(),
            ["minecraft:diamond_ore", "minecraft:diamond_ore[x=1]"]
        );
        assert!(same_block("minecraft:stone", "minecraft:stone"));
        assert!(!same_block("minecraft:stone_bricks", "stone"));
    }
}
