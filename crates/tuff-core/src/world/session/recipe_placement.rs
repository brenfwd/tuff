//! Place Recipe: the recipe book fills an open crafting grid or furnace.

use super::*;
use crate::protocol::play::{self, GameMode};
use crate::world::crafting::place;
use crate::world::menu::{self, MenuKind, set_grid, take_grid};
use crate::world::session::menus::Sim;

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
        let mut sim = match (self.menu_mirrored(), self.menus.menu.as_ref()) {
            (true, Some(menu)) => Some(Sim::of(&self.inventory, &menu.container)),
            _ => None,
        };
        let mut own = match sim {
            Some(_) => None,
            None => self
                .menus
                .menu
                .as_mut()
                .map(|menu| std::mem::take(&mut menu.container)),
        };
        let outcome = {
            let mut none = Vec::new();
            let (container, inventory) = match sim.as_mut() {
                Some(sim) => (&mut sim.container, &mut sim.inventory),
                None => (own.as_mut().unwrap_or(&mut none), &mut self.inventory),
            };
            let mut grid = take_grid(kind, container, inventory);
            let mut result = kind
                .is_furnace()
                .then(|| container[menu::furnace::RESULT].take())
                .flatten();
            let outcome = place::place_recipe(
                recipe,
                &mut place::Grid {
                    width,
                    height,
                    slots: &mut grid,
                },
                std::slice::from_mut(&mut result),
                inventory,
                use_max_items,
                creative,
                &mut drops,
            );
            set_grid(kind, container, inventory, grid);
            if kind.is_furnace() {
                container[menu::furnace::RESULT] = result;
            }
            outcome
        };
        if let (Some(menu), Some(container)) = (self.menus.menu.as_mut(), own) {
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
                if sim.is_none() {
                    self.send_menu_content();
                }
            }
        }
        match sim {
            Some(mut sim) => {
                sim.drops = drops;
                self.settle_click(sim, false, true).await;
            }
            None => {
                for stack in drops {
                    self.throw_item(stack).await;
                }
            }
        }
    }
}
