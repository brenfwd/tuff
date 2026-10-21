//! A player's recipe book: the recipes it knows, the ones still
//! highlighted as new, and the per-book GUI settings. Recipes are unlocked
//! by the recipe advancements' rewards (see `world::advancement`).

use std::collections::BTreeSet;

use tuff_vanilla_data::Frozen;

use crate::protocol::play::recipe_flags;
use crate::protocol::recipe::DisplayEntry;
use crate::storage::player_data::RecipeBookSave;
use crate::world::crafting::RecipeSet;

/// Book GUIs in the game's order: crafting, furnace, blast furnace, smoker.
pub const BOOKS: usize = 4;

/// Recipe Book Add entries for the client, with their flags.
pub type Entries<'a> = Vec<(&'a DisplayEntry, u8)>;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecipeBook {
    known: BTreeSet<Frozen<str>>,
    highlight: BTreeSet<Frozen<str>>,
    /// `(open, filtering)` per book.
    pub settings: [(bool, bool); BOOKS],
}

impl RecipeBook {
    /// Loads a saved book, dropping recipes that no longer exist.
    pub fn load(save: &RecipeBookSave, recipes: &RecipeSet) -> Self {
        let resolve = |names: &[String]| -> BTreeSet<Frozen<str>> {
            names
                .iter()
                .filter_map(|name| recipes.resolve(name))
                .collect()
        };
        Self {
            known: resolve(&save.known),
            highlight: resolve(&save.highlight),
            settings: save.settings,
        }
    }

    pub fn save(&self) -> RecipeBookSave {
        RecipeBookSave {
            known: self.known.iter().map(|name| name.to_string()).collect(),
            highlight: self.highlight.iter().map(|name| name.to_string()).collect(),
            settings: self.settings,
        }
    }

    pub fn contains(&self, recipe: &str) -> bool {
        self.known.contains(recipe)
    }

    pub fn known(&self) -> impl Iterator<Item = &str> + '_ {
        self.known.iter().map(|name| &**name)
    }

    /// Everything the client is told on join: every known recipe, new ones
    /// still highlighted.
    pub fn initial_entries<'a>(&self, recipes: &'a RecipeSet) -> Entries<'a> {
        self.known
            .iter()
            .flat_map(|name| {
                let highlight = self.highlight.contains(&**name);
                recipes.displays_for(name).iter().map(move |entry| {
                    (
                        entry,
                        if highlight {
                            recipe_flags::HIGHLIGHT
                        } else {
                            0
                        },
                    )
                })
            })
            .collect()
    }

    /// Learns `names`, returning the entries to send for those that were
    /// new; special recipes are never listed.
    pub fn add<'r, 'a>(
        &mut self,
        recipes: &'r RecipeSet,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Entries<'r> {
        self.learn(recipes, names).0
    }

    /// Like [`Self::add`], also naming the recipes that were new.
    pub fn learn<'r, 'a>(
        &mut self,
        recipes: &'r RecipeSet,
        names: impl IntoIterator<Item = &'a str>,
    ) -> (Entries<'r>, Vec<Frozen<str>>) {
        let mut entries = Vec::new();
        let mut learned = Vec::new();
        for name in names {
            let Some(name) = recipes.resolve(name) else {
                continue;
            };
            if self.known.contains(&name) || recipes.is_special(&name) {
                continue;
            }
            self.known.insert(name.clone());
            self.highlight.insert(name.clone());
            let mut flags = recipe_flags::HIGHLIGHT;
            if recipes.show_notification(&name) {
                flags |= recipe_flags::NOTIFICATION;
            }
            entries.extend(
                recipes
                    .displays_for(&name)
                    .iter()
                    .map(|display| (display, flags)),
            );
            learned.push(name);
        }
        (entries, learned)
    }

    /// Forgets `names`, returning the display ids the client should drop.
    pub fn remove<'a>(
        &mut self,
        recipes: &RecipeSet,
        names: impl IntoIterator<Item = &'a str>,
    ) -> Vec<u32> {
        let mut displays = Vec::new();
        for name in names {
            if self.known.remove(name) {
                self.highlight.remove(name);
                displays.extend(recipes.displays_for(name).iter().map(|display| display.id));
            }
        }
        displays
    }

    /// The client showed the recipe: it is no longer new.
    pub fn seen(&mut self, recipes: &RecipeSet, display: u32) {
        if let Some(entry) = recipes.display(display) {
            self.highlight.remove(&*entry.recipe);
        }
    }

    pub fn set_setting(&mut self, book: usize, open: bool, filtering: bool) {
        if let Some(setting) = self.settings.get_mut(book) {
            *setting = (open, filtering);
        }
    }
}
