//! Item counts and the matching that decides whether a set of ingredients
//! can be satisfied from them, as the game's stacked contents do: every
//! ingredient is assigned one item type holding at least `amount` of it, so
//! an item type can serve as many ingredients as its count allows.

use tuff_vanilla_data::Item;
use tuff_vanilla_data::recipe::Ingredient;

use tuff_item::item::ItemStack;

#[derive(Debug, Clone, Default)]
pub struct StackedContents {
    items: Vec<(Item, u32)>,
}

/// Whether a stack may be pulled into a recipe by the recipe book: plain
/// items only, so damaged, enchanted, renamed or custom ones stay put.
pub fn usable_for_crafting(stack: &ItemStack) -> bool {
    stack.damage() == 0
        && stack
            .components
            .enchantments
            .as_ref()
            .is_none_or(|e| e.entries().is_empty())
        && stack.components.custom_name.is_none()
        && crate::custom::item_id(stack).is_none()
}

impl StackedContents {
    pub fn account(&mut self, item: Item, count: u32) {
        if count == 0 {
            return;
        }
        match self.items.iter_mut().find(|(i, _)| *i == item) {
            Some((_, total)) => *total += count,
            None => self.items.push((item, count)),
        }
    }

    /// Counts a stack up to its stack limit, as the recipe book does.
    pub fn account_stack(&mut self, stack: &ItemStack) {
        let count = stack.count().min(stack.max_stack_size());
        self.account(stack.item(), count as u32);
    }

    pub fn account_simple(&mut self, stack: &ItemStack) {
        if usable_for_crafting(stack) {
            self.account_stack(stack);
        }
    }

    /// The item each ingredient is served by, if every ingredient can take
    /// `amount` of one.
    pub fn try_pick(&self, ingredients: &[Ingredient], amount: u32) -> Option<Vec<Item>> {
        if amount == 0 {
            return Some(Vec::new());
        }
        let capacity: Vec<usize> = self
            .items
            .iter()
            .map(|(_, count)| (*count / amount) as usize)
            .collect();
        let mut picker = Picker {
            ingredients,
            items: &self.items,
            capacity,
            assigned: vec![Vec::new(); self.items.len()],
        };
        for ingredient in 0..ingredients.len() {
            let mut visited = vec![false; self.items.len()];
            if !picker.augment(ingredient, &mut visited) {
                return None;
            }
        }
        let mut chosen = vec![None; ingredients.len()];
        for (item, served) in picker.assigned.iter().enumerate() {
            for &ingredient in served {
                chosen[ingredient] = Some(self.items[item].0);
            }
        }
        chosen.into_iter().collect()
    }

    fn upper_bound(&self, ingredients: &[Ingredient]) -> u32 {
        ingredients
            .iter()
            .map(|ingredient| {
                self.items
                    .iter()
                    .filter(|(item, _)| ingredient.matches(*item))
                    .map(|(_, count)| *count)
                    .max()
                    .unwrap_or(0)
            })
            .min()
            .unwrap_or(0)
    }

    /// The largest `amount` that [`Self::try_pick`] accepts, at most
    /// `max_size`, with the items it picked.
    pub fn biggest_craftable(&self, ingredients: &[Ingredient], max_size: u32) -> (u32, Vec<Item>) {
        let mut min = 0;
        let mut max = max_size.min(self.upper_bound(ingredients)) + 1;
        let mut mid;
        loop {
            mid = (min + max) / 2;
            if self.try_pick(ingredients, mid).is_some() {
                if max - min <= 1 {
                    break;
                }
                min = mid;
            } else {
                max = mid;
            }
        }
        let picked = if mid > 0 {
            self.try_pick(ingredients, mid).unwrap_or_default()
        } else {
            Vec::new()
        };
        (mid, picked)
    }
}

struct Picker<'a> {
    ingredients: &'a [Ingredient],
    items: &'a [(Item, u32)],
    capacity: Vec<usize>,
    assigned: Vec<Vec<usize>>,
}

impl Picker<'_> {
    fn augment(&mut self, ingredient: usize, visited: &mut [bool]) -> bool {
        for item in 0..self.items.len() {
            if visited[item] || !self.ingredients[ingredient].matches(self.items[item].0) {
                continue;
            }
            visited[item] = true;
            if self.assigned[item].len() < self.capacity[item] {
                self.assigned[item].push(ingredient);
                return true;
            }
            let served = self.assigned[item].clone();
            for other in served {
                if self.augment(other, visited) {
                    self.assigned[item].retain(|i| *i != other);
                    self.assigned[item].push(ingredient);
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn matching_assigns_shared_ingredients_across_item_types() {
        let planks = Ingredient::any_of(&[Item::OakPlanks, Item::BirchPlanks]);
        let oak = Ingredient::Item(Item::OakPlanks);
        let mut contents = StackedContents::default();
        contents.account(Item::OakPlanks, 1);
        contents.account(Item::BirchPlanks, 1);
        assert_eq!(
            contents.try_pick(&[planks.clone(), oak.clone()], 1),
            Some(vec![Item::BirchPlanks, Item::OakPlanks])
        );
        assert_eq!(contents.try_pick(&[oak.clone(), oak.clone()], 1), None);
        assert_eq!(
            contents.try_pick(&[planks.clone(), planks.clone(), planks.clone()], 1),
            None
        );
        assert_eq!(
            contents.biggest_craftable(std::slice::from_ref(&planks), 64),
            (1, vec![Item::OakPlanks])
        );
        assert_eq!(contents.biggest_craftable(&[planks, oak], 64).0, 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn biggest_craftable_divides_counts_by_ingredient_uses() {
        let stick = Ingredient::Item(Item::Stick);
        let mut contents = StackedContents::default();
        contents.account(Item::Stick, 7);
        assert_eq!(
            contents
                .biggest_craftable(&[stick.clone(), stick.clone()], 64)
                .0,
            3
        );
        assert_eq!(contents.biggest_craftable(&[stick], 2).0, 2);
        assert_eq!(
            contents
                .biggest_craftable(&[Ingredient::Item(Item::Diamond)], 64)
                .0,
            0
        );
    }
}
