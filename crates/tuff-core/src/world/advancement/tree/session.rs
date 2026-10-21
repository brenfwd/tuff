//! The session's side of advancements: the per-player state and the
//! functions that count events, grant, flush and hand out rewards.

use super::entry::Key;
use tuff_core::protocol::play;
use tuff_core::world::advancement::trigger::Event;
use tuff_core::world::advancement::{Id, Reward, View, now_millis};
use tuff_core::world::session::PlayerSession;
use tuff_item::text::TextComponent;

use super::{Progress, Tree};

/// A player's advancement progress, the rewards still to give and the
/// view others last read.
#[derive(Default)]
pub struct State {
    pub progress: Progress,
    pub pending: Vec<Id>,
    pub view: Option<(u64, View)>,
}

pub(super) fn state(session: &mut PlayerSession) -> &mut State {
    session.ext.entry::<State>()
}

pub fn done(session: &PlayerSession) -> Option<&Progress> {
    session.ext.get::<State>().map(|state| &state.progress)
}

pub(super) fn count_event(session: &mut PlayerSession, tree: &Tree, event: Event<'_>) {
    let held = std::mem::take(state(session));
    let keys = held
        .progress
        .matching(tree, &event, || session.player_facts());
    *state(session) = held;
    for key in keys {
        grant_criterion(session, tree, key);
    }
}
/// Meets one criterion; a completed advancement grants its recipes
/// at once and queues its other rewards for the next tick.
pub(super) fn grant_criterion(
    session: &mut PlayerSession,
    tree: &Tree,
    (id, criterion): Key,
) -> bool {
    let granted = state(session)
        .progress
        .grant(tree, id, criterion, now_millis());
    if granted.completed {
        completed_advancement(session, tree, id);
    }
    granted.granted
}
pub(super) fn revoke_criterion(
    session: &mut PlayerSession,
    tree: &Tree,
    (id, criterion): Key,
) -> bool {
    state(session).progress.revoke(tree, id, criterion)
}
pub(super) fn completed_advancement(session: &mut PlayerSession, tree: &Tree, id: Id) {
    let recipes = &tree.get(id).rewards.recipes;
    if !recipes.is_empty() {
        session.award_recipes(recipes.iter().map(|recipe| &**recipe));
    }
    state(session).pending.push(id);
}
/// Moves the player's progress (and rewards still to give) from `old`
/// to `new` by name, so the next update resets the client's tree and
/// sends it whole, as the game does. Progress on a tree of another
/// kind starts over.
pub(super) fn rebind_advancement_progress(
    session: &mut PlayerSession,
    old: Option<&Tree>,
    new: &Tree,
) {
    let Some(old) = old else {
        state(session).progress = Progress::new();
        state(session).pending.clear();
        state(session).view = None;
        return;
    };
    state(session).progress = state(session).progress.rebind(old, new);
    state(session).pending = std::mem::take(&mut state(session).pending)
        .into_iter()
        .filter_map(|id| new.same_as(old, id))
        .collect();
    state(session).view = None;
}
/// The rewards of advancements on `tree` completed since the last
/// call.
pub(super) fn take_advancement_rewards(session: &mut PlayerSession, tree: &Tree) -> Vec<Reward> {
    std::mem::take(&mut state(session).pending)
        .into_iter()
        .map(|id| {
            let advancement = tree.get(id);
            let rewards = &advancement.rewards;
            if let Some(function) = &rewards.function {
                tree.run_function(function, session.profile.id);
            }
            let announcement = advancement
                .display
                .as_ref()
                .filter(|display| {
                    display.announce_to_chat
                        && session.shared.rule_bool("show_advancement_messages")
                })
                .map(|display| {
                    TextComponent::translate(
                        format!("chat.type.advancement.{}", display.frame.name()),
                        [
                            TextComponent::new(session.profile.name.clone()),
                            tree.chat_name(id),
                        ],
                    )
                });
            Reward {
                experience: rewards.experience,
                loot: rewards.loot.clone(),
                function: rewards.function.clone(),
                announcement,
            }
        })
        .collect()
}
/// `/advancement` on this player: how many advancements (or 1 for a
/// criterion) changed.
pub(super) fn advancement_directive(
    session: &mut PlayerSession,
    tree: &Tree,
    grant: bool,
    ids: &[Id],
    criterion: Option<&str>,
    show: bool,
) -> i32 {
    if let (Some(criterion), [id]) = (criterion, ids) {
        let Some(index) = tree.criterion_index(*id, criterion) else {
            return 0;
        };
        let changed = if grant {
            grant_criterion(session, tree, (*id, index))
        } else {
            revoke_criterion(session, tree, (*id, index))
        };
        return i32::from(changed);
    }
    if !show {
        flush_advancements(session, tree, true);
    }
    let mut count = 0;
    for &id in ids {
        let criteria = 0..tree.get(id).criteria.len() as u16;
        let changed = if grant {
            !state(session).progress.is_done(tree, id) && {
                for index in criteria {
                    grant_criterion(session, tree, (id, index));
                }
                true
            }
        } else {
            criteria
                .clone()
                .any(|i| state(session).progress.criterion_done(id, i))
                && {
                    for index in criteria {
                        revoke_criterion(session, tree, (id, index));
                    }
                    true
                }
        };
        count += i32::from(changed);
    }
    if !show {
        flush_advancements(session, tree, false);
    }
    count
}
/// Sends the advancements the client has not seen yet; `show: false`
/// asks the client not to show toasts for them.
pub(super) fn flush_advancements(session: &mut PlayerSession, tree: &Tree, show: bool) {
    let Some(update) = state(session).progress.flush(tree) else {
        return;
    };
    let added = update
        .added
        .iter()
        .map(|&id| {
            let advancement = tree.get(id);
            play::AdvancementEntry {
                id: &advancement.name,
                parent: advancement.parent.map(|parent| &*tree.get(parent).name),
                display: advancement
                    .display
                    .as_ref()
                    .map(|display| play::AdvancementDisplay {
                        title: &display.title,
                        description: &display.description,
                        icon: display.icon.view(),
                        frame: display.frame.id(),
                        background: display.background.as_deref(),
                        show_toast: display.show_toast,
                        hidden: display.hidden,
                    }),
                requirements: &advancement.requirements,
                sends_telemetry_event: advancement.sends_telemetry_event,
                position: advancement.position,
            }
        })
        .collect();
    let packet = play::UpdateAdvancements {
        reset: update.reset,
        added,
        removed: update
            .removed
            .iter()
            .map(|&id| &*tree.get(id).name)
            .collect(),
        progress: update
            .progress
            .iter()
            .map(|(id, criteria)| (&*tree.get(*id).name, criteria.as_slice()))
            .collect(),
        show_advancements: show,
    };
    session.send(&packet);
}
pub(super) fn select_advancement_tab(session: &mut PlayerSession, tree: &Tree, tab: &str) {
    let Some(id) = tree.id(tab) else {
        return;
    };
    if let Some(selected) = state(session).progress.select_tab(tree, Some(id)) {
        session.send(&play::SelectAdvancementsTab(
            selected.map(|id| &*tree.get(id).name),
        ));
    }
}
/// The progress on `tree`, taken again only after a change.
pub(super) fn progress_view(session: &mut PlayerSession, tree: std::sync::Arc<Tree>) -> View {
    let version = state(session).progress.version();
    if let Some((seen, view)) = &state(session).view
        && *seen == version
    {
        return view.clone();
    }
    let view = View(std::sync::Arc::new(super::seam::Snapshot(
        tree,
        state(session).progress.clone(),
    )));
    state(session).view = Some((version, view.clone()));
    view
}

/// Test and feature helpers on a session for reading its progress.
pub trait SessionAdvancements {
    fn advancement_done(&self, name: &str) -> bool;
    fn criterion_done(&self, name: &str, criterion: &str) -> bool;
}

impl SessionAdvancements for PlayerSession {
    fn advancement_done(&self, name: &str) -> bool {
        let (Some(tree), Some(progress)) = (self.tree(), done(self)) else {
            return false;
        };
        tree.id(name).is_some_and(|id| progress.is_done(tree, id))
    }

    fn criterion_done(&self, name: &str, criterion: &str) -> bool {
        let (Some(tree), Some(progress)) = (self.tree(), done(self)) else {
            return false;
        };
        tree.id(name).is_some_and(|id| {
            tree.criterion_index(id, criterion)
                .is_some_and(|index| progress.criterion_done(id, index))
        })
    }
}

trait TreeOf {
    fn tree(&self) -> Option<&Tree>;
}

impl TreeOf for PlayerSession {
    fn tree(&self) -> Option<&Tree> {
        self.progress.advancement_tree.as_any().downcast_ref()
    }
}
