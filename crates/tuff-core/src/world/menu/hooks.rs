use tuff_item::item::AsItemView;

use super::*;

/// What a cartography table's second slot accepts.
pub fn is_cartography_additional(stack: &ItemStack) -> bool {
    matches!(stack.item(), Item::Paper | Item::Map | Item::GlassPane)
}

/// What a furnace's fuel slot accepts: fuel, or a bucket to catch water.
pub fn is_fuel(stack: &ItemStack) -> bool {
    stack.item().cooking_fuel().is_some() || stack.item() == Item::Bucket
}

/// Furnace result taken: nothing changes in the slots; the region awards
/// the experience the furnace accumulated.
#[derive(Default)]
pub struct FurnaceHook {
    pub took: bool,
}

impl ResultHook for FurnaceHook {
    fn taken(&mut self, _: &mut [Option<ItemStack>], _: &mut Inventory, _: &mut Vec<ItemStack>) {
        self.took = true;
    }
}

pub(super) fn equipment_slot(stack: &ItemStack) -> Option<EquipmentSlot> {
    stack.equippable().map(|e| e.slot)
}

/// Takes the crafting grid of a menu out of its slots, wherever they live;
/// [`set_grid`] puts it back.
pub fn take_grid(
    kind: MenuKind,
    container: &mut [Option<ItemStack>],
    inventory: &mut Inventory,
) -> Vec<Option<ItemStack>> {
    let (range, _, _) = kind.grid().expect("a crafting menu");
    match kind {
        MenuKind::Player => range.map(|slot| inventory.slot_mut(slot).take()).collect(),
        _ => container[range].iter_mut().map(Option::take).collect(),
    }
}

pub fn set_grid(
    kind: MenuKind,
    container: &mut [Option<ItemStack>],
    inventory: &mut Inventory,
    grid: Vec<Option<ItemStack>>,
) {
    let (range, _, _) = kind.grid().expect("a crafting menu");
    for (slot, stack) in range.zip(grid) {
        match kind {
            MenuKind::Player => inventory.set_slot(slot, stack),
            _ => container[slot] = stack,
        }
    }
}

pub fn result_slot_mut<'a>(
    kind: MenuKind,
    container: &'a mut [Option<ItemStack>],
    inventory: &'a mut Inventory,
) -> &'a mut Option<ItemStack> {
    let slot = kind.result_slot().expect("a crafting menu");
    match kind {
        MenuKind::Player => inventory.slot_mut(slot),
        _ => &mut container[slot],
    }
}

/// Crafting result taken: ingredients are used up and the grid crafts
/// again if it still can.
pub struct GridHook {
    kind: MenuKind,
    recipes: Arc<RecipeSet>,
    pub took: bool,
    /// Each recipe crafted, with the stacks its grid held.
    pub crafted: Vec<(
        tuff_vanilla_data::Frozen<str>,
        Vec<tuff_item::item::ItemSnapshot>,
    )>,
    /// With limited crafting on, the recipe book that gates the result.
    limited: Option<crate::world::recipe_book::RecipeBook>,
}

impl GridHook {
    pub fn new(kind: MenuKind, recipes: Arc<RecipeSet>) -> Self {
        Self {
            kind,
            recipes,
            took: false,
            crafted: Vec::new(),
            limited: None,
        }
    }

    /// Gates each recomputed result on `book`, as the limited_crafting
    /// rule does.
    pub fn limited_to(mut self, book: Option<crate::world::recipe_book::RecipeBook>) -> Self {
        self.limited = book;
        self
    }

    fn result(&self, grid: &[Option<ItemStack>], width: usize, height: usize) -> Option<ItemStack> {
        if let Some(book) = &self.limited
            && let Some(name) = self.recipes.grid_recipe(grid, width, height)
            && !self.recipes.is_special(name)
            && !book.contains(name)
        {
            return None;
        }
        self.recipes.grid_result(grid, width, height)
    }
}

impl ResultHook for GridHook {
    fn taken(
        &mut self,
        container: &mut [Option<ItemStack>],
        inventory: &mut Inventory,
        drops: &mut Vec<ItemStack>,
    ) {
        self.took = true;
        let (_, width, height) = self.kind.grid().expect("a crafting menu");
        let mut grid = take_grid(self.kind, container, inventory);
        if let Some(recipe) = self
            .recipes
            .grid_recipe(&grid, width, height)
            .and_then(|name| self.recipes.resolve(name))
        {
            self.crafted.push((
                recipe,
                grid.iter()
                    .flatten()
                    .map(tuff_item::item::ItemSnapshot::of)
                    .collect(),
            ));
        }
        for leftover in self.recipes.take_result(&mut grid, width, height) {
            if let Some(dropped) = inventory.add(leftover) {
                drops.push(dropped);
            }
        }
        let result = self.result(&grid, width, height);
        set_grid(self.kind, container, inventory, grid);
        *result_slot_mut(self.kind, container, inventory) = result;
    }
}

/// Stonecutter result taken: one input is cut and, if any remains, the
/// chosen recipe offers its result again.
pub struct StonecutterHook {
    /// The chosen stonecutter recipe (see `Recipes::stonecutter_recipe`).
    recipe: Option<usize>,
    recipes: Arc<RecipeSet>,
    pub took: bool,
    /// Each recipe crafted, with its input.
    pub crafted: Vec<(
        tuff_vanilla_data::Frozen<str>,
        Vec<tuff_item::item::ItemSnapshot>,
    )>,
}

impl StonecutterHook {
    pub fn new(recipe: Option<usize>, recipes: Arc<RecipeSet>) -> Self {
        Self {
            recipe,
            recipes,
            took: false,
            crafted: Vec::new(),
        }
    }
}

impl ResultHook for StonecutterHook {
    fn taken(
        &mut self,
        container: &mut [Option<ItemStack>],
        _: &mut Inventory,
        _: &mut Vec<ItemStack>,
    ) {
        self.took = true;
        if let (Some((name, _)), Some(input)) = (
            self.recipe
                .and_then(|index| self.recipes.stonecutter_recipe(index)),
            container[stonecutter::INPUT].as_ref(),
        ) {
            let name = self.recipes.resolve(name).expect("a recipe of this set");
            self.crafted
                .push((name, vec![tuff_item::item::ItemSnapshot::of(input)]));
        }
        let remaining = match container[stonecutter::INPUT].as_mut() {
            Some(input) => {
                input.shrink(1, BurnReason::CraftingInput);
                input.count() > 0
            }
            None => false,
        };
        if !remaining {
            container[stonecutter::INPUT] = None;
            return;
        }
        container[stonecutter::RESULT] = self
            .recipe
            .and_then(|index| self.recipes.stonecutter_recipe(index))
            .map(|(_, result)| result);
    }
}

/// Loom result taken: one banner and one dye are used up.
#[derive(Default)]
pub struct LoomHook {
    pub took: bool,
}

impl ResultHook for LoomHook {
    fn taken(
        &mut self,
        container: &mut [Option<ItemStack>],
        _: &mut Inventory,
        _: &mut Vec<ItemStack>,
    ) {
        self.took = true;
        for slot in [loom::BANNER, loom::DYE] {
            if let Some(stack) = container[slot].as_mut() {
                stack.shrink(1, BurnReason::CraftingInput);
                if stack.count() == 0 {
                    container[slot] = None;
                }
            }
        }
    }
}

/// Cartography table result taken: one map and one of the second input
/// are used up; the new result is worked out by whoever fills it.
#[derive(Default)]
pub struct CartographyHook {
    pub took: bool,
}

impl ResultHook for CartographyHook {
    fn taken(
        &mut self,
        container: &mut [Option<ItemStack>],
        _: &mut Inventory,
        _: &mut Vec<ItemStack>,
    ) {
        self.took = true;
        for slot in [cartography::MAP, cartography::ADDITIONAL] {
            if let Some(stack) = container[slot].as_mut() {
                stack.shrink(1, BurnReason::CraftingInput);
                if stack.count() == 0 {
                    container[slot] = None;
                }
            }
        }
    }
}

/// The offer a merchant screen's payment slots buy and the result it
/// shows: the hinted or first offer they pay for, in either slot order,
/// unless it is out of stock.
pub fn merchant_sell_item(
    container: &[Option<ItemStack>],
    offers: &[MerchantOffer],
    hint: usize,
) -> (Option<usize>, Option<ItemStack>) {
    let (a, b) = match container[merchant::PAYMENT_A].view() {
        None => (container[merchant::PAYMENT_B].view(), None),
        Some(a) => (Some(a), container[merchant::PAYMENT_B].view()),
    };
    if a.is_none() || offers.is_empty() {
        return (None, None);
    }
    let mut active = None;
    let mut offer = trading::recipe_for(offers, a, b, hint);
    if offer.is_none_or(|i| offers[i].is_out_of_stock()) {
        active = offer;
        offer = trading::recipe_for(offers, b, a, hint);
    }
    match offer.filter(|i| !offers[*i].is_out_of_stock()) {
        Some(index) => {
            let sell = &offers[index].sell;
            (
                Some(index),
                Some(ItemStack::mint_like(
                    sell,
                    sell.count(),
                    MintReason::Preview,
                )),
            )
        }
        None => (active, None),
    }
}

/// Merchant result taken: the payment slots pay for the active offer,
/// which is used once more, and the result is worked out again.
pub struct MerchantHook<'a> {
    pub offers: &'a mut [MerchantOffer],
    pub active: Option<usize>,
    pub hint: usize,
    /// Offers taken, in order, with how many times.
    pub trades: Vec<(usize, u32)>,
}

impl ResultHook for MerchantHook<'_> {
    fn made_as(&self) -> MintReason {
        MintReason::Trade
    }

    fn taken(
        &mut self,
        container: &mut [Option<ItemStack>],
        _: &mut Inventory,
        _: &mut Vec<ItemStack>,
    ) {
        if let Some(index) = self.active {
            let offer = &self.offers[index];
            let mut a = container[merchant::PAYMENT_A].take();
            let mut b = container[merchant::PAYMENT_B].take();
            if offer.take(&mut a, &mut b) || offer.take(&mut b, &mut a) {
                self.offers[index].uses += 1;
                match self.trades.last_mut() {
                    Some((last, times)) if *last == index => *times += 1,
                    _ => self.trades.push((index, 1)),
                }
            }
            container[merchant::PAYMENT_A] = a;
            container[merchant::PAYMENT_B] = b;
        }
        let (active, result) = merchant_sell_item(container, self.offers, self.hint);
        self.active = active;
        container[merchant::RESULT] = result;
    }
}

/// Choosing an offer on a merchant screen: what the payment slots hold
/// goes back to the inventory, then the inventory fills them with what the
/// offer costs.
pub fn merchant_move_items(
    container: &mut [Option<ItemStack>],
    inventory: &mut Inventory,
    offer: &MerchantOffer,
) {
    let mut hook = NoResult;
    let mut view = View {
        creative: false,
        kind: MenuKind::Merchant,
        recipes: &crate::world::crafting::RecipeSet::default(),
        container,
        inventory,
        hook: &mut hook,
        drops: Vec::new(),
    };
    for slot in [merchant::PAYMENT_A, merchant::PAYMENT_B] {
        let Some(mut stack) = view.container[slot].take() else {
            continue;
        };
        if !view.move_into(&mut stack, 3, 39, true) {
            view.container[slot] = Some(stack);
            return;
        }
        view.container[slot] = (stack.count() > 0).then_some(stack);
    }
    if view.container[merchant::PAYMENT_A].is_some()
        || view.container[merchant::PAYMENT_B].is_some()
    {
        return;
    }
    let costs = [Some(&offer.buy), offer.buy_b.as_ref()];
    for (slot, cost) in costs.into_iter().enumerate() {
        let Some(cost) = cost else {
            continue;
        };
        for window in 3..39 {
            let Some(item) = view.get(window) else {
                continue;
            };
            if !cost.matches(item.view()) {
                continue;
            }
            if view.container[slot]
                .as_ref()
                .is_some_and(|c| !c.stacks_with(item))
            {
                continue;
            }
            let max = item.max_stack_size();
            let have = view.container[slot].as_ref().map_or(0, |c| c.count());
            let Some(part) = ItemStack::take_from(view.get_mut(window), max - have) else {
                continue;
            };
            ItemStack::put_back(&mut view.container[slot], part);
            if view.container[slot].as_ref().map_or(0, |c| c.count()) >= max {
                break;
            }
        }
    }
}

/// Smithing result taken: one of each input is used up.
pub struct SmithingHook {
    recipes: Arc<RecipeSet>,
    pub took: bool,
    /// Each recipe crafted, with its inputs.
    pub crafted: Vec<(
        tuff_vanilla_data::Frozen<str>,
        Vec<tuff_item::item::ItemSnapshot>,
    )>,
}

impl SmithingHook {
    pub fn new(recipes: Arc<RecipeSet>) -> Self {
        Self {
            recipes,
            took: false,
            crafted: Vec::new(),
        }
    }
}

impl ResultHook for SmithingHook {
    fn taken(
        &mut self,
        container: &mut [Option<ItemStack>],
        _: &mut Inventory,
        _: &mut Vec<ItemStack>,
    ) {
        self.took = true;
        let inputs = [smithing::TEMPLATE, smithing::BASE, smithing::ADDITION]
            .map(|slot| container[slot].as_ref());
        if let Some((name, _)) = self.recipes.smithing(inputs[0], inputs[1], inputs[2]) {
            let name = self.recipes.resolve(name).expect("a recipe of this set");
            self.crafted.push((
                name,
                inputs
                    .into_iter()
                    .flatten()
                    .map(tuff_item::item::ItemSnapshot::of)
                    .collect(),
            ));
        }
        for slot in [smithing::TEMPLATE, smithing::BASE, smithing::ADDITION] {
            if let Some(stack) = container[slot].as_mut() {
                stack.shrink(1, BurnReason::CraftingInput);
                if stack.count() == 0 {
                    container[slot] = None;
                }
            }
        }
        container[smithing::RESULT] = smithing_result(&self.recipes, container);
    }
}

pub fn smithing_result(recipes: &RecipeSet, container: &[Option<ItemStack>]) -> Option<ItemStack> {
    recipes
        .smithing(
            container[smithing::TEMPLATE].as_ref(),
            container[smithing::BASE].as_ref(),
            container[smithing::ADDITION].as_ref(),
        )
        .and_then(|(_, result)| result)
}

/// Anvil result taken: the addition is used up as far as the repair needed
/// and the input is consumed. The session charges the levels afterwards.
pub struct AnvilHook {
    pub cost: i32,
    pub repair_count: u8,
    pub only_renaming: bool,
    pub affordable: bool,
    pub took: bool,
}

impl AnvilHook {
    pub fn new(cost: i32, repair_count: u8, only_renaming: bool, affordable: bool) -> Self {
        Self {
            cost,
            repair_count,
            only_renaming,
            affordable,
            took: false,
        }
    }
}

impl ResultHook for AnvilHook {
    fn can_take(&self) -> bool {
        self.affordable && self.cost > 0
    }

    fn taken(
        &mut self,
        container: &mut [Option<ItemStack>],
        _: &mut Inventory,
        _: &mut Vec<ItemStack>,
    ) {
        self.took = true;
        if self.repair_count > 0 {
            if let Some(stack) = container[anvil::ADDITION].as_mut()
                && stack.count() > self.repair_count
            {
                stack.shrink(self.repair_count, BurnReason::CraftingInput);
            } else {
                container[anvil::ADDITION] = None;
            }
        } else if !self.only_renaming {
            container[anvil::ADDITION] = None;
        }
        container[anvil::INPUT] = None;
    }
}

/// Grindstone result taken: both inputs are consumed and the experience
/// they were worth is reported for the session to drop.
pub struct GrindstoneHook {
    pub took: bool,
    pub experience: i32,
    pub rules: std::sync::Arc<dyn crate::world::stations::StationRules>,
}

impl GrindstoneHook {
    pub fn new(rules: std::sync::Arc<dyn crate::world::stations::StationRules>) -> Self {
        Self {
            took: false,
            experience: 0,
            rules,
        }
    }
}

impl ResultHook for GrindstoneHook {
    fn taken(
        &mut self,
        container: &mut [Option<ItemStack>],
        _: &mut Inventory,
        _: &mut Vec<ItemStack>,
    ) {
        self.took = true;
        self.experience = [grindstone::INPUT, grindstone::ADDITIONAL]
            .into_iter()
            .filter_map(|slot| container[slot].as_ref())
            .map(|stack| self.rules.grindstone_experience(stack))
            .sum();
        container[grindstone::INPUT] = None;
        container[grindstone::ADDITIONAL] = None;
    }
}
