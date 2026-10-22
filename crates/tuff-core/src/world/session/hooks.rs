//! Session hooks: how features take part in a player's session.
//!
//! `PlayerSession`'s gameplay state is public, so a feature crate works on
//! it through its own extension traits (`impl FooExt for PlayerSession`).
//! Core never names those traits; it calls every registered
//! [`SessionHooks`] at fixed points (a tick, a region report, a client
//! command) and a feature forwards to its extension methods from there.
//! State only one feature cares about lives in [`Extensions`], keyed by
//! type, rather than as a core field.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use super::{DamageSource, HurtResult, PlayerSession};
use crate::feature::feature_timer;
use crate::protocol::play::Serverbound;
use crate::world::contact::PortalContact;
use crate::world::menu::MenuKind;
use crate::world::messages::PlayerEvent;

/// What an async hook returns.
pub type HookFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

fn done<'a>() -> HookFuture<'a> {
    Box::pin(async {})
}

/// What an interceptor decides about one serverbound packet.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    /// Pass it on unchanged.
    Allow,
    /// Pass this packet on instead.
    Correct(Box<Serverbound>),
    /// Drop it silently.
    Cancel,
    /// Drop it and disconnect the player.
    Kick(tuff_item::text::TextComponent),
}

/// Sees every serverbound play packet, decoded, before anything else in
/// the session. Interceptors run in ascending `order` (registration order
/// among equals); a corrected packet is what later ones see.
pub trait PacketInterceptor: Send + Sync {
    fn intercept<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a Serverbound,
    ) -> Pin<Box<dyn Future<Output = Verdict> + Send + 'a>>;
}

/// An interceptor with its position in the chain; lower runs first.
#[derive(Clone)]
pub struct Interceptor {
    pub order: i32,
    pub hook: std::sync::Arc<dyn PacketInterceptor>,
}

/// Where in the session tick a tick hook runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTick {
    /// After effects and stats, where held item use advances.
    Use,
    /// After riding and gliding.
    Late,
}

/// What a player did with an item that is worth a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemAction {
    /// Launched a projectile (arrow, trident, snowball, firework).
    Shot,
    /// Took a crafted result.
    Crafted,
    /// Wore a tool or armor piece out.
    Broken,
}

impl ItemAction {
    pub fn name(self) -> &'static str {
        match self {
            Self::Shot => "shot",
            Self::Crafted => "craft",
            Self::Broken => "break",
        }
    }
}

/// One feature's part in every player session. Every method defaults to
/// doing nothing, so a hook set implements only what it needs.
pub trait SessionHooks: Send + Sync {
    /// Once per session tick, at `phase`.
    fn tick<'a>(&'a self, _session: &'a mut PlayerSession, _phase: SessionTick) -> HookFuture<'a> {
        done()
    }

    /// The home region's surroundings report was applied; `portal` is the
    /// portal block the body is inside.
    fn surroundings<'a>(
        &'a self,
        _session: &'a mut PlayerSession,
        _portal: Option<PortalContact>,
    ) -> HookFuture<'a> {
        done()
    }

    /// Use Item with nothing core handles (buckets, boats) in `off_hand`.
    fn use_item<'a>(&'a self, _session: &'a mut PlayerSession, _off_hand: bool) -> HookFuture<'a> {
        done()
    }

    /// An event core leaves to features; `take` it to handle it.
    fn event<'a>(
        &'a self,
        _session: &'a mut PlayerSession,
        _event: &'a mut Option<PlayerEvent>,
    ) -> HookFuture<'a> {
        done()
    }

    /// A packet core leaves to features; `take` it to handle it.
    fn packet<'a>(
        &'a self,
        _session: &'a mut PlayerSession,
        _packet: &'a mut Option<Serverbound>,
    ) -> HookFuture<'a> {
        done()
    }

    /// The player died of `kind`, at `killer`'s doing if anyone's.
    fn died(
        &self,
        _session: &mut PlayerSession,
        _kind: &tuff_vanilla_data::damage_type::DamageType,
        _killer: Option<&crate::world::entity::Culprit>,
    ) {
    }

    /// The player did `action` with `stack`. Observe only.
    fn item_action(
        &self,
        _session: &PlayerSession,
        _action: ItemAction,
        _stack: tuff_item::item::ItemView<'_>,
    ) {
    }

    /// A feature's take removed `removed` of `requested` `item` from the
    /// player's inventory. Observe only.
    fn items_taken(
        &self,
        _session: &PlayerSession,
        _item: tuff_vanilla_data::Item,
        _requested: u32,
        _removed: u32,
    ) {
    }

    /// A command or directive moved the player.
    fn teleported(&self, _session: &mut PlayerSession) {}

    /// Whether the session should publish (and so be re-reported) every
    /// tick even while standing still.
    fn wants_reports(&self, _session: &PlayerSession) -> bool {
        false
    }

    /// The client sent Client Command `action`; sets `taken` if this hook
    /// handled it.
    fn client_command<'a>(
        &'a self,
        _session: &'a mut PlayerSession,
        _action: i32,
        _taken: &'a mut bool,
    ) -> HookFuture<'a> {
        done()
    }

    /// Whether the session is detached waiting on this feature (end
    /// credits, ...): movement is ignored and the tick skipped.
    fn suspended(&self, _session: &PlayerSession) -> bool {
        false
    }

    /// The player is in the world, its saved data loaded.
    fn joined<'a>(&'a self, _session: &'a mut PlayerSession) -> HookFuture<'a> {
        done()
    }

    /// Whether the player may be hurt by `source` at all; any hook saying
    /// no cancels the damage before it lands.
    fn allows_player_hurt(&self, _session: &PlayerSession, _source: &DamageSource) -> bool {
        true
    }

    /// What the damage from `source` is multiplied by before it lands
    /// (1 leaves it alone); each hook's factor applies.
    fn player_hurt_scale(&self, _session: &PlayerSession, _source: &DamageSource) -> f32 {
        1.0
    }

    /// Whether the player may take the dropped item `stack` offered to
    /// them; any hook saying no leaves it on the ground.
    fn allows_item_pickup(
        &self,
        _session: &PlayerSession,
        _stack: tuff_item::item::ItemView<'_>,
    ) -> bool {
        true
    }

    /// Whether the player may open a container window of `kind`; any hook
    /// saying no leaves the block or entity unopened.
    fn allows_menu_open(&self, _session: &PlayerSession, _kind: MenuKind) -> bool {
        true
    }

    /// Whether the player may close their open container window of `kind`;
    /// any hook saying no has the window reopened.
    fn allows_menu_close(&self, _session: &PlayerSession, _kind: MenuKind) -> bool {
        true
    }

    /// A container window of `kind` finished opening for the player.
    fn menu_opened(&self, _session: &PlayerSession, _kind: MenuKind) {}

    /// The player's open container window of `kind` was torn down.
    fn menu_closed(&self, _session: &PlayerSession, _kind: MenuKind) {}

    /// The player came back to life.
    fn respawned(&self, _session: &PlayerSession) {}

    /// The player started or stopped sneaking.
    fn sneak_changed(&self, _session: &PlayerSession, _sneaking: bool) {}

    /// Whether the player may change to `mode`; any hook saying no keeps
    /// the current one. Called for every change, `refused` is reported
    /// by the caller through [`Self::game_mode_changed`].
    fn allows_game_mode(&self, _session: &PlayerSession, _mode: super::GameMode) -> bool {
        true
    }

    /// The player's game mode went from `from` to `to`, or `to` was
    /// refused (`cancelled`).
    fn game_mode_changed(
        &self,
        _session: &PlayerSession,
        _from: super::GameMode,
        _to: super::GameMode,
        _cancelled: bool,
    ) {
    }

    /// Whether the player's food level may drop from `before` to the
    /// current level; any hook saying no puts `before` back.
    fn allows_food_loss(&self, _session: &PlayerSession) -> bool {
        true
    }

    /// The food level moved from `from` to `to`; `locked` when the loss
    /// was undone and the level stayed `from`.
    fn food_changed(&self, _session: &PlayerSession, _from: i32, _to: i32, _locked: bool) {}

    /// Whether the player may be moved by `/tp` to `pos` in `dimension`.
    fn allows_teleport(&self, _session: &PlayerSession, _dimension: &str, _pos: [f64; 3]) -> bool {
        true
    }

    /// A `/tp` to `pos` in `dimension` was decided, before the player
    /// moves; `cancelled` when a hook refused it.
    fn teleport_decided(
        &self,
        _session: &PlayerSession,
        _dimension: &str,
        _pos: [f64; 3],
        _cancelled: bool,
    ) {
    }

    /// The player arrived in the dimension `from` was the key of.
    fn dimension_changed(&self, _session: &PlayerSession, _from: &str) {}

    /// The player was hurt (or died of it) by `by`, after `player_hurt`.
    fn player_damaged(
        &self,
        _session: &mut PlayerSession,
        _by: &super::HurtBy,
        _result: &HurtResult,
        _died: bool,
    ) {
    }

    /// The player was hurt (or died of it) through `hurt`.
    fn player_hurt(
        &self,
        _session: &PlayerSession,
        _kind: &'static tuff_vanilla_data::damage_type::DamageType,
        _result: &HurtResult,
        _died: bool,
    ) {
    }

    /// The player is leaving: still in the world, about to be saved.
    fn leaving<'a>(&'a self, _session: &'a mut PlayerSession) -> HookFuture<'a> {
        done()
    }
}

/// How a player takes damage (armor, shields, enchantments, ...). Core
/// without one only takes the amount off the health.
pub trait PlayerDamage: Send + Sync {
    fn hurt<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        source: DamageSource,
        amount: f32,
    ) -> Pin<Box<dyn Future<Output = HurtResult> + Send + 'a>>;

    /// A hit by another entity (a mob, or a projectile someone shot)
    /// landed: what acts once a hit lands (Thorns on the armor) runs.
    fn entity_hit_landed<'a>(
        &'a self,
        _session: &'a mut PlayerSession,
        _hit: &'a crate::world::entity::EntityHit,
    ) -> HookFuture<'a> {
        done()
    }
}

/// A feature's per-player record, read on join and written on save.
pub trait PlayerSaved: Send + Sync {
    /// The document this owner keeps for `uuid`.
    fn document(&self, uuid: u128) -> tuff_storage::document::Document<'static>;
    /// On join: what was read (None for a new player).
    fn load(&self, session: &mut PlayerSession, root: Option<tuff_nbt::Compound>);
    /// On save: a snapshot to write (None writes nothing).
    fn save(&self, session: &PlayerSession) -> Option<tuff_nbt::Compound>;
}

/// Per-player state owned by features, one value per type.
#[derive(Default)]
pub struct Extensions(HashMap<TypeId, Box<dyn Any + Send + Sync>>);

impl Extensions {
    pub fn get<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.0.get(&TypeId::of::<T>())?.downcast_ref()
    }

    pub fn get_mut<T: Any + Send + Sync>(&mut self) -> Option<&mut T> {
        self.0.get_mut(&TypeId::of::<T>())?.downcast_mut()
    }

    pub fn entry<T: Any + Send + Sync + Default>(&mut self) -> &mut T {
        self.0
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut()
            .expect("extension keyed by its own type")
    }

    pub fn entry_or<T: Any + Send + Sync>(&mut self, value: T) -> &mut T {
        self.0
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(value))
            .downcast_mut()
            .expect("extension keyed by its own type")
    }

    pub fn remove<T: Any + Send + Sync>(&mut self) -> Option<T> {
        self.0
            .remove(&TypeId::of::<T>())
            .and_then(|value| value.downcast().ok())
            .map(|value| *value)
    }
}

impl PlayerSession {
    pub async fn run_session_tick_hooks(&mut self, phase: SessionTick) {
        let hooks = self.shared.session_hooks.clone();
        let owners = self.shared.hook_owners.clone();
        for (i, hook) in hooks.iter().enumerate() {
            let _feature = feature_timer(&owners.session_hooks, i);
            hook.tick(self, phase).await;
        }
    }

    pub async fn surroundings_hooks(&mut self, portal: Option<PortalContact>) {
        let hooks = self.shared.session_hooks.clone();
        for hook in hooks.iter() {
            hook.surroundings(self, portal).await;
        }
    }

    pub async fn joined_hooks(&mut self) {
        let hooks = self.shared.session_hooks.clone();
        for hook in hooks.iter() {
            hook.joined(self).await;
        }
    }

    pub async fn leaving_hooks(&mut self) {
        let hooks = self.shared.session_hooks.clone();
        for hook in hooks.iter() {
            hook.leaving(self).await;
        }
    }

    pub fn item_action_hooks(&self, action: ItemAction, stack: tuff_item::item::ItemView<'_>) {
        for hook in self.shared.session_hooks.iter() {
            hook.item_action(self, action, stack);
        }
    }

    pub fn items_taken_hooks(&self, item: tuff_vanilla_data::Item, requested: u32, removed: u32) {
        for hook in self.shared.session_hooks.iter() {
            hook.items_taken(self, item, requested, removed);
        }
    }

    pub fn died_hooks(
        &mut self,
        kind: &tuff_vanilla_data::damage_type::DamageType,
        killer: Option<&crate::world::entity::Culprit>,
    ) {
        let hooks = self.shared.session_hooks.clone();
        for hook in hooks.iter() {
            hook.died(self, kind, killer);
        }
    }

    pub fn respawned_hooks(&self) {
        for hook in self.shared.session_hooks.iter() {
            hook.respawned(self);
        }
    }

    pub fn sneak_changed_hooks(&self, sneaking: bool) {
        for hook in self.shared.session_hooks.iter() {
            hook.sneak_changed(self, sneaking);
        }
    }

    pub fn allows_game_mode_hooks(&self, mode: super::GameMode) -> bool {
        self.shared
            .session_hooks
            .iter()
            .all(|hook| hook.allows_game_mode(self, mode))
    }

    pub fn game_mode_changed_hooks(
        &self,
        from: super::GameMode,
        to: super::GameMode,
        cancelled: bool,
    ) {
        for hook in self.shared.session_hooks.iter() {
            hook.game_mode_changed(self, from, to, cancelled);
        }
    }

    /// Reports the food level moving from `from`; a loss a hook refuses
    /// is undone.
    pub fn food_changed_hooks(&mut self, from: i32) {
        if self.body.food.level == from {
            return;
        }
        let locked = self.body.food.level < from
            && !self
                .shared
                .session_hooks
                .iter()
                .all(|hook| hook.allows_food_loss(self));
        let to = self.body.food.level;
        if locked {
            self.body.food.level = from;
        }
        for hook in self.shared.session_hooks.iter() {
            hook.food_changed(self, from, to, locked);
        }
    }

    /// Runs the `/tp` hooks; false when a hook refused the move.
    pub fn teleport_decided_hooks(&self, dimension: &str, pos: [f64; 3]) -> bool {
        let allowed = self
            .shared
            .session_hooks
            .iter()
            .all(|hook| hook.allows_teleport(self, dimension, pos));
        for hook in self.shared.session_hooks.iter() {
            hook.teleport_decided(self, dimension, pos, !allowed);
        }
        allowed
    }

    pub fn dimension_changed_hooks(&self, from: &str) {
        for hook in self.shared.session_hooks.iter() {
            hook.dimension_changed(self, from);
        }
    }

    pub fn allows_menu_open_hooks(&self, kind: MenuKind) -> bool {
        self.shared
            .session_hooks
            .iter()
            .all(|hook| hook.allows_menu_open(self, kind))
    }

    pub fn allows_menu_close_hooks(&self, kind: MenuKind) -> bool {
        self.shared
            .session_hooks
            .iter()
            .all(|hook| hook.allows_menu_close(self, kind))
    }

    pub fn menu_opened_hooks(&self, kind: MenuKind) {
        for hook in self.shared.session_hooks.iter() {
            hook.menu_opened(self, kind);
        }
    }

    pub fn menu_closed_hooks(&self, kind: MenuKind) {
        for hook in self.shared.session_hooks.iter() {
            hook.menu_closed(self, kind);
        }
    }

    pub async fn use_item_hooks(&mut self, off_hand: bool) {
        let hooks = self.shared.session_hooks.clone();
        for hook in hooks.iter() {
            hook.use_item(self, off_hand).await;
        }
    }

    pub fn teleported_hooks(&mut self) {
        let hooks = self.shared.session_hooks.clone();
        for hook in hooks.iter() {
            hook.teleported(self);
        }
    }

    pub fn hooks_want_reports(&self) -> bool {
        self.shared
            .session_hooks
            .iter()
            .any(|hook| hook.wants_reports(self))
    }

    pub fn hooks_suspended(&self) -> bool {
        self.shared
            .session_hooks
            .iter()
            .any(|hook| hook.suspended(self))
    }

    /// Offers `event` to the hooks; returns it if none took it.
    pub async fn event_hooks(&mut self, event: PlayerEvent) -> Option<PlayerEvent> {
        let hooks = self.shared.session_hooks.clone();
        let owners = self.shared.hook_owners.clone();
        let mut event = Some(event);
        for (i, hook) in hooks.iter().enumerate() {
            let _feature = feature_timer(&owners.session_hooks, i);
            hook.event(self, &mut event).await;
            event.as_ref()?;
        }
        event
    }

    /// Offers `packet` to the hooks; returns it if none took it.
    /// Runs the interceptor chain; `None` means the packet is dropped.
    pub async fn intercept(&mut self, packet: Serverbound) -> Option<Serverbound> {
        if self.shared.interceptors.is_empty() {
            return Some(packet);
        }
        let chain = self.shared.interceptors.clone();
        let owners = self.shared.hook_owners.clone();
        let mut packet = packet;
        for (i, interceptor) in chain.iter().enumerate() {
            let _feature = feature_timer(&owners.packet_interceptors, i);
            match interceptor.hook.intercept(self, &packet).await {
                Verdict::Allow => {}
                Verdict::Correct(replacement) => packet = *replacement,
                Verdict::Cancel => return None,
                Verdict::Kick(reason) => {
                    self.kick(reason);
                    return None;
                }
            }
        }
        Some(packet)
    }

    pub async fn packet_hooks(&mut self, packet: Serverbound) -> Option<Serverbound> {
        let hooks = self.shared.session_hooks.clone();
        let owners = self.shared.hook_owners.clone();
        let mut packet = Some(packet);
        for (i, hook) in hooks.iter().enumerate() {
            let _feature = feature_timer(&owners.session_hooks, i);
            hook.packet(self, &mut packet).await;
            packet.as_ref()?;
        }
        packet
    }

    /// Offers a client command to the hooks; returns whether one took it.
    pub async fn client_command_hooks(&mut self, action: i32) -> bool {
        let hooks = self.shared.session_hooks.clone();
        let mut taken = false;
        for hook in hooks.iter() {
            hook.client_command(self, action, &mut taken).await;
            if taken {
                break;
            }
        }
        taken
    }
}
