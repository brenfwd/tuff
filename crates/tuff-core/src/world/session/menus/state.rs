use super::*;

/// What a crafting grid makes for a player: with limited crafting on,
/// only recipes in their recipe book (or special ones).
pub(super) fn limited_grid_result(
    shared: &crate::world::Shared,
    book: &crate::world::recipe_book::RecipeBook,
    grid: &[Option<ItemStack>],
    width: usize,
    height: usize,
) -> Option<ItemStack> {
    let set = &shared.recipes();
    if shared.game_rules.borrow().bool("limited_crafting")
        && let Some(name) = set.grid_recipe(grid, width, height)
        && !set.is_special(name)
        && !book.contains(name)
    {
        return None;
    }
    set.grid_result(grid, width, height)
}

/// The recipe book a crafting result is gated on, if limited crafting is on.
pub(crate) fn crafting_gate(
    shared: &crate::world::Shared,
    book: &crate::world::recipe_book::RecipeBook,
) -> Option<crate::world::recipe_book::RecipeBook> {
    shared
        .game_rules
        .borrow()
        .bool("limited_crafting")
        .then(|| book.clone())
}

/// The open menu, its id counters, the ender chest and creative drop budget.
pub struct MenuState {
    pub ender_items: Vec<Option<ItemStack>>,
    pub menu: Option<OpenMenu>,
    pub window_counter: i32,
    pub txn_counter: u64,
    pub creative_drops: u32,
    /// Inventory-changing input held back while a container transaction
    /// waits for its region, replayed in order once it is answered.
    /// Behind a mutex only so the session stays `Sync`; never contended.
    pub deferred: std::sync::Mutex<VecDeque<Deferred>>,
    /// A world save waiting for the pending container transaction to be
    /// answered before the player snapshots.
    pub save_barrier: Option<crate::world::messages::SaveBarrier>,
}

impl Default for MenuState {
    fn default() -> Self {
        Self {
            ender_items: (0..ENDER_CHEST_SLOTS).map(|_| None).collect(),
            menu: None,
            window_counter: 0,
            txn_counter: 0,
            creative_drops: 0,
            deferred: std::sync::Mutex::new(VecDeque::new()),
            save_barrier: None,
        }
    }
}
