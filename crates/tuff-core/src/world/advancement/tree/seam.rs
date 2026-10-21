use super::entry::Entry;
use std::any::Any;
use std::sync::Arc;

use rayon::prelude::*;
use tuff_core::world::advancement::trigger::Event;
use tuff_core::world::advancement::{
    Advancements, Id, Overlay, ProgressView, Related, Reward, View,
};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::PlayerSaved;
use tuff_item::text::TextComponent;
use tuff_nbt::Compound;
use tuff_storage::document::Document;

use super::{Progress, Tree};

impl Advancements for Tree {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn fire(&self, session: &mut PlayerSession, event: Event<'_>) {
        super::session::count_event(session, self, event);
    }

    fn view(self: Arc<Self>, session: &mut PlayerSession) -> View {
        super::session::progress_view(session, self)
    }

    fn set(
        &self,
        session: &mut PlayerSession,
        grant: bool,
        ids: &[Id],
        criterion: Option<&str>,
        show: bool,
    ) -> i32 {
        super::session::advancement_directive(session, self, grant, ids, criterion, show)
    }

    fn flush(&self, session: &mut PlayerSession, show: bool) {
        super::session::flush_advancements(session, self, show);
    }

    fn take_rewards(&self, session: &mut PlayerSession) -> Vec<Reward> {
        super::session::take_advancement_rewards(session, self)
    }

    fn seen_tab(&self, session: &mut PlayerSession, tab: &str) {
        super::session::select_advancement_tab(session, self, tab);
    }

    fn rebind(&self, session: &mut PlayerSession, old: &dyn Advancements) {
        super::session::rebind_advancement_progress(
            session,
            old.as_any().downcast_ref::<Tree>(),
            self,
        );
    }

    fn len(&self) -> usize {
        Tree::len(self)
    }

    fn id(&self, name: &str) -> Option<Id> {
        Tree::id(self, name)
    }

    fn name(&self, id: Id) -> &str {
        &self.get(id).name
    }

    fn names(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(Tree::names(self))
    }

    fn has_criterion(&self, id: Id, criterion: &str) -> bool {
        self.criterion_index(id, criterion).is_some()
    }

    fn chat_name(&self, id: Id) -> TextComponent {
        Tree::chat_name(self, id)
    }

    fn related(&self, target: Id, mode: Related) -> Vec<Id> {
        let (parents, children) = match mode {
            Related::Only => (false, false),
            Related::From => (false, true),
            Related::Until => (true, false),
            Related::Through => (true, true),
        };
        let mut ids = Vec::new();
        if parents {
            let mut parent = self.get(target).parent;
            while let Some(id) = parent {
                ids.push(id);
                parent = self.get(id).parent;
            }
        }
        ids.push(target);
        if children {
            let mut stack: Vec<Id> = self.get(target).children.iter().rev().copied().collect();
            while let Some(id) = stack.pop() {
                ids.push(id);
                stack.extend(self.get(id).children.iter().rev());
            }
        }
        ids
    }

    fn same_as(&self, other: &dyn Advancements, id: Id) -> Option<Id> {
        match other.as_any().downcast_ref::<Tree>() {
            Some(other) => Tree::same_as(self, other, id),
            None => (usize::from(id) < other.len())
                .then(|| Tree::id(self, other.name(id)))
                .flatten(),
        }
    }

    fn with_overlay(&self, overlay: Overlay<'_>) -> (Arc<dyn Advancements>, Vec<String>) {
        let Overlay {
            removed,
            files,
            unparsed_removes,
            predicates,
            tags,
        } = overlay;
        let parsed: Vec<(String, Option<Entry>)> = files
            .into_par_iter()
            .filter_map(|file| {
                match super::parse::parse(&file.id, &file.json, &self.triggers, predicates) {
                    Ok(entry) => Some((file.id, Some(entry))),
                    Err(err) => {
                        log::warn!(
                            "Couldn't parse advancement {} from {}: {err}",
                            file.id,
                            file.origin
                        );
                        unparsed_removes.then_some((file.id, None))
                    }
                }
            })
            .collect();
        let accepted = parsed
            .iter()
            .filter(|(_, entry)| entry.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        let removed = removed.into_iter().map(|id| (id, None));
        let tree = self.overlay_with(removed.chain(parsed), tags);
        (Arc::new(tree), accepted)
    }
}

/// A tree with the progress taken on it, as selectors read it.
pub(super) struct Snapshot(pub Arc<Tree>, pub Progress);

impl ProgressView for Snapshot {
    fn done(&self, name: &str) -> Option<bool> {
        let Snapshot(tree, progress) = self;
        tree.id(name).map(|id| progress.is_done(tree, id))
    }

    fn criterion(&self, name: &str, criterion: &str) -> Option<bool> {
        let Snapshot(tree, progress) = self;
        let id = tree.id(name)?;
        let index = tree.criterion_index(id, criterion)?;
        Some(progress.criterion_done(id, index))
    }
}

/// Progress in the player's advancements document, on the tree the
/// session has in effect.
pub struct SavedProgress;

impl PlayerSaved for SavedProgress {
    fn document(&self, uuid: u128) -> Document<'static> {
        Document::Advancements(uuid)
    }

    fn load(&self, session: &mut PlayerSession, root: Option<Compound>) {
        let handle = session.progress.advancement_tree.clone();
        let Some(tree) = handle.as_any().downcast_ref::<Tree>() else {
            return;
        };
        super::session::state(session).progress = match root {
            Some(root) => Progress::load(tree, &root),
            None => Progress::new(),
        };
    }

    fn save(&self, session: &PlayerSession) -> Option<Compound> {
        let tree = session
            .progress
            .advancement_tree
            .as_any()
            .downcast_ref::<Tree>()?;
        let progress = super::session::done(session)?;
        Some(progress.save(tree))
    }
}
