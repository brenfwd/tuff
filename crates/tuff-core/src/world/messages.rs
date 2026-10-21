//! Messages between the world's actors and the handles used to send them.
//!
//! Channel choices:
//! - Client packets to a player actor are bounded, so a flooding client backs
//!   up into its own socket.
//! - Requests from player actors to a region are bounded; a player actor
//!   awaits capacity, which throttles only that player.
//! - Everything sent *by* regions, other players or the coordinator (player
//!   events, region-to-region traffic, job completions) is unbounded. The
//!   receivers never await anything but their own inbox, so these queues
//!   drain as fast as they fill; bounding them would instead let two actors
//!   deadlock on each other's full queues.
//!
//! Block containers (chests, hoppers, ...) belong to regions, player
//! inventories to sessions, and neither awaits the other, so a menu over a
//! block container works as follows:
//! - A right click reaches the region as a [`Place`]; the region registers
//!   the player as a viewer and answers with [`PlayerEvent::MenuOpen`]
//!   carrying a snapshot of the container's contents. Every later change
//!   (another viewer's click, a hopper) reaches all viewers as
//!   [`PlayerEvent::ContainerSlots`].
//! - The session computes each click against its snapshot plus its own
//!   inventory, applies it locally, and sends the container slots it
//!   changed as a [`RegionRequest::ContainerTransaction`]: for each slot
//!   the value it expected and the new one. The region compares every
//!   expected value with what it holds and applies all or nothing
//!   (compare-and-swap), answering [`PlayerEvent::Transaction`]. On
//!   acceptance the region broadcasts the new slots to every viewer; on
//!   rejection it returns the current contents and the session restores
//!   its inventory and snapshot from before the click and resends both.
//!   The session holds further clicks until the answer arrives, so at most
//!   one transaction per player is ever in flight.
//! - The session closes the menu (Close Container from the client, the
//!   player moving out of range, leaving) with
//!   [`RegionRequest::CloseContainer`]; the region closes it
//!   ([`PlayerEvent::ContainerClosed`]) when the block breaks or unloads.
//!
//! A double chest whose halves belong to two regions opens through both:
//! the clicked region checks its half and hands the click to the other
//! ([`RegionInternal::OpenHalf`]), which checks and views its half and
//! answers ([`RegionInternal::HalfOpened`]); the clicked region then
//! opens the menu, naming the other half's region in
//! [`MenuSource::Blocks`]. A click changing slots of both halves is a
//! two-phase commit run by the session: each region validates and locks
//! its slots ([`RegionRequest::PrepareTransaction`], answered with
//! [`PlayerEvent::TransactionPrepared`]), then the session commits or
//! aborts both ([`RegionRequest::ResolveTransaction`]). Locked slots
//! refuse other transactions and hoppers until resolved.
//! Entity containers (chest boats) work the same way, keyed by the
//! entity's id instead of block positions; the region closes their menus
//! once the entity leaves it (broken, unloaded or handed off).
//! Menus over player-only data (the ender chest) never leave the session;
//! only the block's open and close effects go through the region.

pub mod action;
pub mod advancement;
pub mod block_update;
pub mod command;
pub mod coordinator;
pub mod directive;
pub mod manager;
pub mod menu;
pub mod player;
pub mod region;
pub mod region_internal;
pub mod ride;

pub use action::*;
pub use advancement::*;
pub use block_update::*;
pub use command::*;
pub use coordinator::*;
pub use directive::*;
pub use manager::*;
pub use menu::*;
pub use player::*;
pub use region::*;
pub use region_internal::*;
pub use ride::*;
