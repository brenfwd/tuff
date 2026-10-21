//! Place Recipe: the recipe book fills an open crafting grid or furnace.

use super::*;
use crate::protocol::play::{self, GameMode};
use crate::world::crafting::place;
use crate::world::menu::{self, MenuKind, grid_of, set_grid};
use tuff_item::item::DupUnchecked;

impl PlayerSession {
    /// Place Recipe: the recipe book fills the open crafting grid.
    pub(super) async fn place_recipe(&mut self, window_id: i32, recipe: i32, use_max_items: bool) {
        if self.mode.game_mode == GameMode::Spectator || window_id != self.current_window() {
            return;
        }
        let set = self.shared.recipes();
        let Some(entry) = u32::try_from(recipe).ok().and_then(|id| set.display(id)) else {
            return;
        };
        if !self.progress.recipe_book.contains(&entry.recipe) {
            return;
        }
        let Some(recipe) = set.get(&entry.recipe) else {
            return;
        };
        let kind = self
            .menus
            .menu
            .as_ref()
            .map_or(MenuKind::Player, |menu| menu.kind);
        let Some((_, width, height)) = kind.grid() else {
            return;
        };
        if self
            .menus
            .menu
            .as_ref()
            .is_some_and(|menu| menu.closing.is_some() || menu.pending.is_some())
        {
            return;
        }
        let creative = self.mode.abilities.instabuild;
        let mut drops = Vec::new();
        let inventory_before = self.inventory.clone();
        let mut container = self
            .menus
            .menu
            .as_mut()
            .map(|menu| std::mem::take(&mut menu.container));
        let container_before = container.dup_unchecked().unwrap_or_default();
        let mut grid = grid_of(kind, container.as_deref().unwrap_or(&[]), &self.inventory);
        let mut result = kind
            .is_furnace()
            .then(|| {
                container
                    .as_mut()
                    .and_then(|c| c[menu::furnace::RESULT].take())
            })
            .flatten();
        let outcome = place::place_recipe(
            recipe,
            &mut place::Grid {
                width,
                height,
                slots: &mut grid,
            },
            std::slice::from_mut(&mut result),
            &mut self.inventory,
            use_max_items,
            creative,
            &mut drops,
        );
        set_grid(
            kind,
            container.as_deref_mut().unwrap_or(&mut []),
            &mut self.inventory,
            grid,
        );
        if kind.is_furnace()
            && let Some(container) = container.as_mut()
        {
            container[menu::furnace::RESULT] = result;
        }
        if let (Some(menu), Some(container)) = (self.menus.menu.as_mut(), container) {
            menu.container = container;
        }
        if outcome == place::PostPlace::Ghost {
            self.send(&play::PlaceGhostRecipe {
                window_id,
                display: &entry.display,
            });
        }
        match kind {
            MenuKind::Player => {
                self.refresh_inventory_result();
                self.send_inventory();
            }
            _ => {
                self.refresh_station();
                self.send_menu_content();
            }
        }
        self.submit_container_changes(container_before, inventory_before, drops, false)
            .await;
    }
}
