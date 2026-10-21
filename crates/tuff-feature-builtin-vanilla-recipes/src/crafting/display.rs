//! What the recipe book shows for each vanilla recipe: the game's recipe
//! display and slot display trees (Recipe Book Add, Place Ghost Recipe).

use tuff_item::item::BurnReason;
use tuff_vanilla_data::data_registry::TRIM_PATTERN;
use tuff_vanilla_data::recipe::{Ingredient, Recipe};
use tuff_vanilla_data::registry::{DataComponentType, RecipeBookCategory};
use tuff_vanilla_data::{Frozen, Item};

use super::result_stack;
use super::{Shaped, Shapeless};
use tuff_core::world::crafting::{Recipe as _, crafting_category};
use tuff_item::item::{ItemStack, MintReason};

pub use tuff_core::protocol::recipe::{DisplayEntry, RecipeDisplay, SlotDisplay};

/// The entries of one recipe among the data pack's.
pub fn displays_for(recipe: &str) -> impl Iterator<Item = &'static DisplayEntry> {
    super::registered::vanilla_set().displays_for(recipe).iter()
}

pub fn group_of(recipe: &Recipe) -> Option<&str> {
    match recipe {
        Recipe::CraftingShaped(r) => r.group.as_deref(),
        Recipe::CraftingShapeless(r) => r.group.as_deref(),
        Recipe::CraftingTransmute(r) => r.group.as_deref(),
        Recipe::CraftingDye(r) => r.group.as_deref(),
        Recipe::CraftingImbue(r) => r.group.as_deref(),
        Recipe::Smelting(r)
        | Recipe::Blasting(r)
        | Recipe::Smoking(r)
        | Recipe::CampfireCooking(r) => r.group.as_deref(),
        _ => None,
    }
    .filter(|group| !group.is_empty())
}

pub fn category_of(recipe: &Recipe) -> RecipeBookCategory {
    match recipe {
        Recipe::CraftingTransmute(r) => crafting_category(r.category),
        Recipe::CraftingDye(r) => crafting_category(r.category),
        Recipe::CraftingImbue(r) => crafting_category(r.category),
        Recipe::SmithingTrim(_) => RecipeBookCategory::Smithing,
        _ => RecipeBookCategory::CraftingMisc,
    }
}

fn ingredient(ingredient: &Ingredient) -> SlotDisplay {
    SlotDisplay::Ingredient(ingredient.clone())
}

fn stack(stack: ItemStack) -> SlotDisplay {
    SlotDisplay::template(stack)
}

fn crafting_table() -> SlotDisplay {
    SlotDisplay::Item(Item::CraftingTable)
}

/// The displays of a recipe, in the game's order; empty for recipes the
/// book never lists.
pub fn displays_of(recipe: &Recipe) -> Vec<RecipeDisplay> {
    match recipe {
        Recipe::CraftingShaped(recipe) => Shaped {
            name: Frozen::Static(""),
            recipe: recipe.clone(),
        }
        .displays(),
        Recipe::CraftingShapeless(recipe) => Shapeless {
            name: Frozen::Static(""),
            recipe: recipe.clone(),
        }
        .displays(),
        Recipe::CraftingTransmute(transmute) => {
            let (min, max) = super::transmute_bounds(transmute);
            let mut ingredients = vec![ingredient(&transmute.input)];
            let mut displays = Vec::new();
            for materials in min..=max {
                ingredients.push(ingredient(&transmute.material));
                let count = super::transmute_count(transmute, materials);
                let result = match &transmute.result {
                    Some(result) => {
                        let mut shown = result_stack(result);
                        shown.resize(
                            count.min(99) as u8,
                            MintReason::Template,
                            BurnReason::Template,
                        );
                        stack(shown)
                    }
                    None => {
                        let items: &[Item] = transmute.input.items();
                        SlotDisplay::Composite(
                            items
                                .iter()
                                .map(|item| {
                                    stack(ItemStack::mint(
                                        *item,
                                        count.min(99) as u8,
                                        MintReason::Template,
                                    ))
                                })
                                .collect(),
                        )
                    }
                };
                displays.push(RecipeDisplay::Shapeless {
                    ingredients: ingredients.clone(),
                    result,
                    station: crafting_table(),
                });
            }
            displays
        }
        Recipe::CraftingDye(dye) => {
            let dyes = SlotDisplay::OnlyWithComponent(
                Box::new(ingredient(&dye.dye)),
                DataComponentType::Dye,
            );
            let target = ingredient(&dye.target);
            vec![RecipeDisplay::Shapeless {
                ingredients: vec![target.clone(), dyes.clone()],
                result: SlotDisplay::Dyed {
                    dye: Box::new(dyes),
                    target: Box::new(target),
                },
                station: crafting_table(),
            }]
        }
        Recipe::CraftingImbue(imbue) => {
            let material = ingredient(&imbue.material);
            let source = SlotDisplay::WithAnyPotion(Box::new(ingredient(&imbue.source)));
            let m = || material.clone();
            vec![RecipeDisplay::Shaped {
                width: 3,
                height: 3,
                ingredients: vec![m(), m(), m(), m(), source, m(), m(), m(), m()],
                result: SlotDisplay::WithAnyPotion(Box::new(stack(result_stack(&imbue.result)))),
                station: crafting_table(),
            }]
        }
        Recipe::SmithingTrim(trim) => {
            let base = ingredient(&trim.base);
            let material = ingredient(&trim.addition);
            let pattern = TRIM_PATTERN
                .entries
                .binary_search(&trim.pattern)
                .map_or(0, |index| index as u32);
            vec![RecipeDisplay::Smithing {
                template: ingredient(&trim.template),
                base: base.clone(),
                addition: material.clone(),
                result: SlotDisplay::SmithingTrim {
                    base: Box::new(base),
                    material: Box::new(material),
                    pattern,
                },
                station: SlotDisplay::Item(Item::SmithingTable),
            }]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::protocol::recipe::write_recipe_display;
    use tuff_vanilla_data::Item;
    use tuff_vanilla_data::registry::{RecipeDisplay as DisplayKind, SlotDisplay as SlotKind};
    use tuff_wire::{PacketReader, PacketWriter};

    fn encoded(display: &RecipeDisplay) -> PacketReader {
        let mut w = PacketWriter::new();
        write_recipe_display(&mut w, display);
        PacketReader::new(w.into_bytes())
    }

    #[tuff_core::watchdog]
    #[test]
    fn shaped_displays_encode_their_grid_and_inline_id_sets() {
        let torch = displays_for("minecraft:torch").next().unwrap();
        let mut r = encoded(&torch.display);
        assert_eq!(
            r.read_var_int().unwrap(),
            DisplayKind::CraftingShaped.id() as i32
        );
        assert_eq!(r.read_var_int().unwrap(), 1);
        assert_eq!(r.read_var_int().unwrap(), 2);
        assert_eq!(r.read_var_int().unwrap(), 2);
        assert_eq!(r.read_var_int().unwrap(), SlotKind::Tag.id() as i32);
        assert_eq!(r.read_var_int().unwrap(), 3);
        assert_eq!(r.read_var_int().unwrap(), Item::Coal.id() as i32);
        assert_eq!(r.read_var_int().unwrap(), Item::Charcoal.id() as i32);
        assert_eq!(r.read_var_int().unwrap(), SlotKind::Tag.id() as i32);
        assert_eq!(r.read_var_int().unwrap(), 2);
        assert_eq!(r.read_var_int().unwrap(), Item::Stick.id() as i32);
        assert_eq!(r.read_var_int().unwrap(), SlotKind::ItemStack.id() as i32);
        assert_eq!(r.read_var_int().unwrap(), Item::Torch.id() as i32);
        assert_eq!(r.read_var_int().unwrap(), 4);
        assert_eq!(r.read_var_int().unwrap(), 0);
        assert_eq!(r.read_var_int().unwrap(), 0);
        assert_eq!(r.read_var_int().unwrap(), SlotKind::Item.id() as i32);
        assert_eq!(r.read_var_int().unwrap(), Item::CraftingTable.id() as i32);
        assert_eq!(r.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_display_kind_encodes_completely() {
        for name in [
            "minecraft:oak_planks",
            "minecraft:iron_ingot_from_smelting_iron_ore",
            "minecraft:andesite_slab_from_andesite_stonecutting",
            "minecraft:netherite_sword_smithing",
            "minecraft:coast_armor_trim_smithing_template_smithing_trim",
            "minecraft:leather_helmet_dyed",
            "minecraft:tipped_arrow",
            "minecraft:map_cloning",
        ] {
            for entry in displays_for(name) {
                let mut r = encoded(&entry.display);
                let kind = r.read_var_int().unwrap();
                assert!(DisplayKind::from_id(kind as u32).is_some(), "{name}");
                assert!(r.remaining() > 0, "{name}");
            }
        }
        let trim = displays_for("minecraft:coast_armor_trim_smithing_template_smithing_trim")
            .next()
            .unwrap();
        let mut r = encoded(&trim.display);
        assert_eq!(r.read_var_int().unwrap(), DisplayKind::Smithing.id() as i32);
        for _ in 0..3 {
            assert_eq!(r.read_var_int().unwrap(), SlotKind::Tag.id() as i32);
            let count = r.read_var_int().unwrap() - 1;
            for _ in 0..count {
                r.read_var_int().unwrap();
            }
        }
        assert_eq!(
            r.read_var_int().unwrap(),
            SlotKind::SmithingTrim.id() as i32
        );
    }
}
