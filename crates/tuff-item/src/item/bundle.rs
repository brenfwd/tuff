//! Bundle contents: a weighted list of stacks, filled and emptied one
//! stack at a time. A bundle holds one unit of weight; an item weighs the
//! reciprocal of its stack size, a nested bundle one sixteenth plus what
//! it holds, and anything carrying bees a full unit.

use crate::item::hash::Canon;
use crate::item::{ItemStack, MAX_CONTAINER_SLOTS};
use tuff_nbt::Tag;
use tuff_vanilla_data::tag;

/// Shulker boxes cannot go inside shulker boxes.
pub fn fits_inside_container_item(stack: &ItemStack) -> bool {
    !stack
        .item
        .block()
        .is_some_and(|block| tuff_datapack::tags::is(tag::block::key::SHULKER_BOXES, block))
}

const BEES: &str = "minecraft:bees";
const NESTED_BUNDLE_WEIGHT: Fraction = Fraction { num: 1, den: 16 };
const ONE: Fraction = Fraction { num: 1, den: 1 };
pub const NO_SELECTION: i32 = -1;

/// An exact non-negative ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fraction {
    num: u64,
    den: u64,
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

impl Fraction {
    pub const ZERO: Self = Self { num: 0, den: 1 };

    fn new(num: u64, den: u64) -> Option<Self> {
        if den == 0 {
            return None;
        }
        let divisor = gcd(num, den).max(1);
        Some(Self {
            num: num / divisor,
            den: den / divisor,
        })
    }

    fn add(self, other: Self) -> Option<Self> {
        let num = self
            .num
            .checked_mul(other.den)?
            .checked_add(other.num.checked_mul(self.den)?)?;
        Self::new(num, self.den.checked_mul(other.den)?)
    }

    fn sub(self, other: Self) -> Option<Self> {
        let num = self
            .num
            .checked_mul(other.den)?
            .checked_sub(other.num.checked_mul(self.den)?)?;
        Self::new(num, self.den.checked_mul(other.den)?)
    }

    fn times(self, count: u64) -> Option<Self> {
        Self::new(self.num.checked_mul(count)?, self.den)
    }

    /// The whole number of `unit`s that fit in this ratio.
    fn whole_units_of(self, unit: Self) -> u64 {
        let num = self.num as u128 * unit.den as u128;
        let den = self.den as u128 * unit.num as u128;
        num.checked_div(den).map_or(0, |whole| whole as u64)
    }

    pub fn as_f32(self) -> f32 {
        self.num as f32 / self.den as f32
    }

    pub fn is_zero(self) -> bool {
        self.num == 0
    }

    pub fn at_least_one(self) -> bool {
        self.num >= self.den
    }

    /// `floor(self * factor)`.
    pub fn mul_and_truncate(self, factor: u64) -> u64 {
        ((self.num as u128 * factor as u128) / self.den as u128) as u64
    }
}

/// The weight of one item of `stack`, or `None` when it overflows.
pub fn item_weight(stack: &ItemStack) -> Option<Fraction> {
    if let Some(inner) = &stack.components.bundle_contents {
        return inner.weight()?.add(NESTED_BUNDLE_WEIGHT);
    }
    let has_bees = stack
        .components
        .other
        .get(BEES)
        .and_then(Tag::as_list)
        .is_some_and(|bees| !bees.is_empty());
    if has_bees {
        return Some(ONE);
    }
    Fraction::new(1, stack.max_stack_size().max(1) as u64)
}

/// Whether `stack` may go into a bundle.
pub fn can_fit(stack: &ItemStack) -> bool {
    stack.count > 0 && fits_inside_container_item(stack)
}

#[derive(Debug, Clone, Default)]
pub struct BundleContents {
    items: Vec<ItemStack>,
    selected: i32,
}

impl PartialEq for BundleContents {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}

impl BundleContents {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: NO_SELECTION,
        }
    }

    pub fn from_items(items: Vec<ItemStack>) -> Self {
        Self {
            items,
            selected: NO_SELECTION,
        }
    }

    /// Stacks inserted one by one into an empty bundle, as a loot or
    /// container rewrite does; what does not fit is dropped.
    pub fn filled_from(stacks: impl IntoIterator<Item = ItemStack>) -> Self {
        let mut contents = Self::new();
        for mut stack in stacks {
            contents.try_insert(&mut stack);
        }
        contents
    }

    pub fn items(&self) -> &[ItemStack] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn selected(&self) -> i32 {
        self.selected
    }

    pub fn selected_item(&self) -> Option<&ItemStack> {
        usize::try_from(self.selected)
            .ok()
            .and_then(|index| self.items.get(index))
    }

    /// Total weight, or `None` for a tree too heavy to count.
    pub fn weight(&self) -> Option<Fraction> {
        let mut total = Fraction::ZERO;
        for stack in &self.items {
            total = total.add(item_weight(stack)?.times(stack.count as u64)?)?;
        }
        Some(total)
    }

    /// The weight, or a full unit when it cannot be counted.
    pub fn weight_or_full(&self) -> Fraction {
        self.weight().unwrap_or(ONE)
    }

    pub fn is_full(&self) -> bool {
        self.weight_or_full().at_least_one()
    }

    /// How many items of `weight` still fit.
    pub fn room_for(&self, weight: Fraction) -> u64 {
        match self.weight().and_then(|current| ONE.sub(current)) {
            Some(free) => free.whole_units_of(weight),
            None => 0,
        }
    }

    /// Inserts as much of `stack` as fits, taking it from `stack`;
    /// returns the amount moved. The inserted stack goes first, merged
    /// with an identical stackable one.
    pub fn try_insert(&mut self, stack: &mut ItemStack) -> u8 {
        if !can_fit(stack) || self.items.len() >= MAX_CONTAINER_SLOTS {
            return 0;
        }
        let Some(weight) = item_weight(stack) else {
            return 0;
        };
        let amount = (stack.count as u64).min(self.room_for(weight)) as u8;
        if amount == 0 {
            return 0;
        }
        let existing = if stack.max_stack_size() > 1 {
            self.items.iter().position(|held| held.stacks_with(stack))
        } else {
            None
        };
        let mut moved = stack.clone().with_count(amount);
        if let Some(index) = existing {
            let held = self.items.remove(index);
            moved.count = held.count.saturating_add(amount);
        }
        stack.count -= amount;
        self.items.insert(0, moved);
        amount
    }

    /// Takes out the selected stack, else the first; the selection
    /// resets.
    pub fn remove_one(&mut self) -> Option<ItemStack> {
        if self.items.is_empty() {
            return None;
        }
        let index = self.index_if_selected().unwrap_or(0);
        let removed = self.items.remove(index);
        self.selected = NO_SELECTION;
        Some(removed)
    }

    fn index_if_selected(&self) -> Option<usize> {
        usize::try_from(self.selected)
            .ok()
            .filter(|index| *index < self.items.len())
    }

    /// Selects `index`, or clears the selection when it is already
    /// selected or out of range.
    pub fn toggle_selected(&mut self, index: i32) {
        let outside = index < 0 || index as usize >= self.items.len();
        self.selected = if self.selected == index || outside {
            NO_SELECTION
        } else {
            index
        };
    }

    /// How many stacks the tooltip grid shows.
    pub fn items_to_show(&self) -> usize {
        let stacks = self.items.len();
        let available = if stacks > 12 { 11 } else { 12 };
        let on_last_row = stacks % 4;
        let empty = if on_last_row == 0 { 0 } else { 4 - on_last_row };
        stacks.min(available - empty)
    }

    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        let entries = tag.as_list()?;
        if entries.len() > MAX_CONTAINER_SLOTS {
            return None;
        }
        let mut items = Vec::with_capacity(entries.len());
        for entry in entries {
            items.push(ItemStack::from_nbt(entry.as_compound()?)?);
        }
        Some(Self::from_items(items))
    }

    pub fn to_nbt(&self) -> Tag {
        Tag::List(
            self.items
                .iter()
                .map(|stack| Tag::Compound(stack.to_nbt()))
                .collect(),
        )
    }

    pub fn canon(&self) -> Canon {
        Canon::List(self.items.iter().map(ItemStack::canon).collect())
    }
}

impl ItemStack {
    /// Whether this is a bundle (it can hold the contents component).
    pub fn is_bundle(&self) -> bool {
        self.item.name().ends_with("bundle")
    }

    /// The fullness the client draws on the item bar, 0 to 1.
    pub fn bundle_fullness(&self) -> f32 {
        self.components
            .bundle_contents
            .as_ref()
            .map_or(0.0, |contents| contents.weight_or_full().as_f32())
    }

    /// Changes the bundle's contents through `edit`, keeping the stored
    /// patch minimal (an empty bundle is the item's default).
    pub fn edit_bundle<R>(&mut self, edit: impl FnOnce(&mut BundleContents) -> R) -> R {
        let mut contents = self.components.bundle_contents.take().unwrap_or_default();
        let result = edit(&mut contents);
        self.components.bundle_contents = (!contents.is_empty()).then_some(contents);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Item;

    fn stack(item: Item, count: u8) -> ItemStack {
        ItemStack::new(item, count)
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_bundle_holds_one_unit_of_weight() {
        let mut contents = BundleContents::new();
        let mut dirt = stack(Item::Dirt, 64);
        assert_eq!(contents.try_insert(&mut dirt), 64);
        assert!(contents.is_full());
        let mut more = stack(Item::Dirt, 1);
        assert_eq!(contents.try_insert(&mut more), 0);
        assert_eq!(more.count, 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn unstackable_items_weigh_a_whole_unit_and_sixteens_a_sixteenth() {
        let mut contents = BundleContents::new();
        let mut pearls = stack(Item::EnderPearl, 16);
        assert_eq!(contents.try_insert(&mut pearls), 16);
        assert!(contents.is_full());
        let mut other = BundleContents::new();
        let mut sword = stack(Item::DiamondSword, 1);
        assert_eq!(other.try_insert(&mut sword), 1);
        assert!(other.is_full());
    }

    #[tuff_core::watchdog]
    #[test]
    fn inserting_merges_identical_stacks_and_puts_them_first() {
        let mut contents = BundleContents::new();
        contents.try_insert(&mut stack(Item::Dirt, 3));
        contents.try_insert(&mut stack(Item::Stone, 2));
        contents.try_insert(&mut stack(Item::Dirt, 4));
        assert_eq!(contents.items()[0].item, Item::Dirt);
        assert_eq!(contents.items()[0].count, 7);
        assert_eq!(contents.len(), 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn removing_takes_the_selected_stack_else_the_first() {
        let mut contents = BundleContents::new();
        contents.try_insert(&mut stack(Item::Dirt, 3));
        contents.try_insert(&mut stack(Item::Stone, 2));
        contents.toggle_selected(1);
        assert_eq!(contents.remove_one().unwrap().item, Item::Dirt);
        assert_eq!(contents.selected(), NO_SELECTION);
        assert_eq!(contents.remove_one().unwrap().item, Item::Stone);
        assert!(contents.remove_one().is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn toggling_the_selected_or_an_invalid_index_clears_it() {
        let mut contents = BundleContents::new();
        contents.try_insert(&mut stack(Item::Dirt, 3));
        contents.toggle_selected(0);
        assert_eq!(contents.selected(), 0);
        contents.toggle_selected(0);
        assert_eq!(contents.selected(), NO_SELECTION);
        contents.toggle_selected(5);
        assert_eq!(contents.selected(), NO_SELECTION);
    }

    #[tuff_core::watchdog]
    #[test]
    fn nested_bundles_add_a_sixteenth_to_their_own_weight() {
        let mut inner = stack(Item::Bundle, 1);
        inner.edit_bundle(|c| c.try_insert(&mut stack(Item::Dirt, 32)));
        let weight = item_weight(&inner).unwrap();
        assert_eq!(weight.mul_and_truncate(32), 18);
        let mut outer = BundleContents::new();
        assert_eq!(outer.try_insert(&mut inner), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn shulker_boxes_do_not_fit() {
        let mut contents = BundleContents::new();
        assert_eq!(contents.try_insert(&mut stack(Item::ShulkerBox, 1)), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_grid_shows_whole_rows_and_leaves_room_for_an_overflow_marker() {
        let counts = [(1, 1), (4, 4), (5, 5), (12, 12), (13, 8)];
        for (stacks, shown) in counts {
            let items = (0..stacks).map(|_| stack(Item::Dirt, 1)).collect();
            assert_eq!(BundleContents::from_items(items).items_to_show(), shown);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn nbt_round_trips_without_the_selection() {
        let mut contents = BundleContents::new();
        contents.try_insert(&mut stack(Item::Dirt, 3));
        contents.toggle_selected(0);
        let back = BundleContents::from_nbt(&contents.to_nbt()).unwrap();
        assert_eq!(back, contents);
        assert_eq!(back.selected(), NO_SELECTION);
    }
}
