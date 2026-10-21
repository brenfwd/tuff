//! What the rest of the server asks of whoever owns advancements: counting
//! events, reading progress for selectors, granting and revoking, sending
//! the client its tree, handing out rewards, walking the tree for
//! `/advancement`, and laying data pack and feature JSON over it.

use std::any::Any;
use std::sync::Arc;

use tuff_vanilla_data::Frozen;

use super::{Id, trigger::Event};
use crate::world::session::PlayerSession;
use tuff_datapack::loot::Predicates;
use tuff_datapack::tags::TagSet;
use tuff_item::text::TextComponent;

/// Which advancements around a named one a command covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Related {
    Only,
    From,
    Until,
    Through,
}

/// What a completed advancement gives, for the session to hand out.
#[derive(Clone, Debug, Default)]
pub struct Reward {
    pub experience: i32,
    pub loot: Box<[Frozen<str>]>,
    /// A function to run as the player.
    pub function: Option<Frozen<str>>,
    /// The chat announcement, when the advancement and game rule ask for
    /// one.
    pub announcement: Option<TextComponent>,
}

/// One advancement as JSON, from a data pack or a feature.
pub struct OverlayFile {
    pub id: String,
    /// The pack or feature it came from, for log messages.
    pub origin: String,
    pub json: Vec<u8>,
}

/// Changes laid over a tree.
pub struct Overlay<'a> {
    /// Advancements taken out.
    pub removed: Vec<String>,
    /// Advancements added or replaced.
    pub files: Vec<OverlayFile>,
    /// A file that does not parse removes the advancement it names (as
    /// data packs do) instead of being left out.
    pub unparsed_removes: bool,
    pub predicates: &'a Predicates,
    /// The tags item criteria are indexed by.
    pub tags: &'a TagSet,
}

/// The single owner of advancements. Every session holds the handle in
/// effect for it (the features' one with data pack changes laid over) and
/// asks it rather than reading progress itself.
pub trait Advancements: Send + Sync + 'static {
    fn as_any(&self) -> &dyn Any;

    /// Counts `event` towards every criterion it meets.
    fn fire(&self, session: &mut PlayerSession, event: Event<'_>);
    /// The player's progress as selectors read it.
    fn view(self: Arc<Self>, session: &mut PlayerSession) -> View;
    /// Meets (or un-meets) every criterion of each of `ids`, or just
    /// `criterion` of the only one; how many changed. `show: false`
    /// sends the resulting update without toasts.
    fn set(
        &self,
        session: &mut PlayerSession,
        grant: bool,
        ids: &[Id],
        criterion: Option<&str>,
        show: bool,
    ) -> i32;
    /// Sends what the client has not seen yet; `show: false` asks it not
    /// to show toasts.
    fn flush(&self, session: &mut PlayerSession, show: bool);
    /// Takes the rewards of advancements completed since the last call.
    fn take_rewards(&self, session: &mut PlayerSession) -> Vec<Reward>;
    /// The client opened the tab rooted at `tab`.
    fn seen_tab(&self, session: &mut PlayerSession, tab: &str);
    /// The session moved from `old` to this handle (a data pack reload):
    /// carries its progress over by name.
    fn rebind(&self, session: &mut PlayerSession, old: &dyn Advancements);

    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn ids(&self) -> Vec<Id> {
        (0..self.len() as Id).collect()
    }
    /// The id of `name` (namespace optional).
    fn id(&self, name: &str) -> Option<Id>;
    fn name(&self, id: Id) -> &str;
    fn names(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new((0..self.len() as Id).map(|id| self.name(id)))
    }
    fn has_criterion(&self, id: Id, criterion: &str) -> bool;
    /// The name as chat shows it.
    fn chat_name(&self, id: Id) -> TextComponent;
    /// `id` with its ancestors (nearest first) and/or descendants.
    fn related(&self, id: Id, mode: Related) -> Vec<Id>;
    /// The id here of the advancement `id` names in `other`.
    fn same_as(&self, other: &dyn Advancements, id: Id) -> Option<Id> {
        if std::ptr::addr_eq(self as *const Self, other as *const dyn Advancements) {
            return Some(id);
        }
        (usize::from(id) < other.len())
            .then(|| self.id(other.name(id)))
            .flatten()
    }

    /// A new handle with `overlay` laid over this one, and the ids of the
    /// files that parsed.
    fn with_overlay(&self, overlay: Overlay<'_>) -> (Arc<dyn Advancements>, Vec<String>);
}

impl std::fmt::Debug for dyn Advancements {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Advancements")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

/// The owner when no feature fills the slot: no advancements, nothing
/// counted or sent.
pub struct NoAdvancements;

impl Advancements for NoAdvancements {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn fire(&self, _: &mut PlayerSession, _: Event<'_>) {}

    fn view(self: Arc<Self>, _: &mut PlayerSession) -> View {
        View(self)
    }

    fn set(&self, _: &mut PlayerSession, _: bool, _: &[Id], _: Option<&str>, _: bool) -> i32 {
        0
    }

    fn flush(&self, _: &mut PlayerSession, _: bool) {}

    fn take_rewards(&self, _: &mut PlayerSession) -> Vec<Reward> {
        Vec::new()
    }

    fn seen_tab(&self, _: &mut PlayerSession, _: &str) {}

    fn rebind(&self, _: &mut PlayerSession, _: &dyn Advancements) {}

    fn len(&self) -> usize {
        0
    }

    fn id(&self, _: &str) -> Option<Id> {
        None
    }

    fn name(&self, _: Id) -> &str {
        ""
    }

    fn has_criterion(&self, _: Id, _: &str) -> bool {
        false
    }

    fn chat_name(&self, _: Id) -> TextComponent {
        TextComponent::new("")
    }

    fn related(&self, id: Id, _: Related) -> Vec<Id> {
        vec![id]
    }

    fn with_overlay(&self, _: Overlay<'_>) -> (Arc<dyn Advancements>, Vec<String>) {
        (Arc::new(NoAdvancements), Vec::new())
    }
}

impl ProgressView for NoAdvancements {
    fn done(&self, _: &str) -> Option<bool> {
        None
    }

    fn criterion(&self, _: &str, _: &str) -> Option<bool> {
        None
    }
}

/// A player's progress as others read it.
pub trait ProgressView: Send + Sync {
    /// Whether the advancement is done; `None` when it does not exist.
    fn done(&self, name: &str) -> Option<bool>;
    /// Whether a criterion is met; `None` when it does not exist.
    fn criterion(&self, name: &str, criterion: &str) -> Option<bool>;
}

/// A snapshot of a player's progress, as selectors and predicates read it.
#[derive(Clone)]
pub struct View(pub Arc<dyn ProgressView>);

impl View {
    pub fn done(&self, name: &str) -> Option<bool> {
        self.0.done(name)
    }

    pub fn criterion(&self, name: &str, criterion: &str) -> Option<bool> {
        self.0.criterion(name, criterion)
    }
}

impl PartialEq for View {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("View").finish_non_exhaustive()
    }
}
