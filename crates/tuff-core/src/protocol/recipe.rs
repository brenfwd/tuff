//! Wire forms of the recipe book's display trees and ingredient sets
//! (https://minecraft.wiki/w/Java_Edition_protocol/Recipes).

use tuff_vanilla_data::Item;
use tuff_vanilla_data::recipe::Ingredient;
use tuff_vanilla_data::registry::{
    DataComponentType, RecipeBookCategory, RecipeDisplay as DisplayKind, SlotDisplay as SlotKind,
};

use crate::protocol::codec::WriteExt;
use tuff_item::item::ItemStack;
use tuff_wire::PacketWriter;

#[derive(Debug, Clone, PartialEq)]
pub enum SlotDisplay {
    Empty,
    AnyFuel,
    WithAnyPotion(Box<SlotDisplay>),
    OnlyWithComponent(Box<SlotDisplay>, DataComponentType),
    Item(Item),
    Stack(ItemStack),
    /// The items an ingredient accepts, sent as an inline id set.
    Ingredient(Ingredient),
    Dyed {
        dye: Box<SlotDisplay>,
        target: Box<SlotDisplay>,
    },
    SmithingTrim {
        base: Box<SlotDisplay>,
        material: Box<SlotDisplay>,
        /// Index into the `minecraft:trim_pattern` registry.
        pattern: u32,
    },
    WithRemainder {
        input: Box<SlotDisplay>,
        remainder: Box<SlotDisplay>,
    },
    Composite(Vec<SlotDisplay>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecipeDisplay {
    Shapeless {
        ingredients: Vec<SlotDisplay>,
        result: SlotDisplay,
        station: SlotDisplay,
    },
    Shaped {
        width: u32,
        height: u32,
        ingredients: Vec<SlotDisplay>,
        result: SlotDisplay,
        station: SlotDisplay,
    },
    Furnace {
        ingredient: SlotDisplay,
        fuel: SlotDisplay,
        result: SlotDisplay,
        station: SlotDisplay,
        duration: u32,
        experience: f32,
    },
    Stonecutter {
        input: SlotDisplay,
        result: SlotDisplay,
        station: SlotDisplay,
    },
    Smithing {
        template: SlotDisplay,
        base: SlotDisplay,
        addition: SlotDisplay,
        result: SlotDisplay,
        station: SlotDisplay,
    },
}

/// One entry of the recipe book: a display with the id the client refers
/// to it by.
#[derive(Debug, Clone)]
pub struct DisplayEntry {
    pub id: u32,
    /// The name of the recipe it shows.
    pub recipe: tuff_vanilla_data::Frozen<str>,
    pub display: RecipeDisplay,
    pub group: Option<u32>,
    pub category: RecipeBookCategory,
    /// The ingredients the book checks against the inventory; `None` for
    /// recipes it cannot place.
    pub requirements: Option<Vec<Ingredient>>,
}

/// An ingredient as an inline ID Set.
pub fn write_ingredient(w: &mut PacketWriter, ingredient: &Ingredient) {
    let ids: Vec<u32> = ingredient.items().iter().map(|item| item.id()).collect();
    w.write_id_list(&ids);
}

pub fn write_slot_display(w: &mut PacketWriter, display: &SlotDisplay) {
    let kind = match display {
        SlotDisplay::Empty => SlotKind::Empty,
        SlotDisplay::AnyFuel => SlotKind::AnyFuel,
        SlotDisplay::WithAnyPotion(_) => SlotKind::WithAnyPotion,
        SlotDisplay::OnlyWithComponent(..) => SlotKind::OnlyWithComponent,
        SlotDisplay::Item(_) => SlotKind::Item,
        SlotDisplay::Stack(_) => SlotKind::ItemStack,
        SlotDisplay::Ingredient(_) => SlotKind::Tag,
        SlotDisplay::Dyed { .. } => SlotKind::Dyed,
        SlotDisplay::SmithingTrim { .. } => SlotKind::SmithingTrim,
        SlotDisplay::WithRemainder { .. } => SlotKind::WithRemainder,
        SlotDisplay::Composite(_) => SlotKind::Composite,
    };
    w.write_var_int(kind.id() as i32);
    match display {
        SlotDisplay::Empty | SlotDisplay::AnyFuel => {}
        SlotDisplay::WithAnyPotion(inner) => write_slot_display(w, inner),
        SlotDisplay::OnlyWithComponent(inner, component) => {
            write_slot_display(w, inner);
            w.write_var_int(component.id() as i32);
        }
        SlotDisplay::Item(item) => w.write_var_int(item.id() as i32),
        SlotDisplay::Stack(stack) => w.write_item_template(stack.view()),
        SlotDisplay::Ingredient(ingredient) => write_ingredient(w, ingredient),
        SlotDisplay::Dyed { dye, target } => {
            write_slot_display(w, dye);
            write_slot_display(w, target);
        }
        SlotDisplay::SmithingTrim {
            base,
            material,
            pattern,
        } => {
            write_slot_display(w, base);
            write_slot_display(w, material);
            w.write_var_int(*pattern as i32 + 1);
        }
        SlotDisplay::WithRemainder { input, remainder } => {
            write_slot_display(w, input);
            write_slot_display(w, remainder);
        }
        SlotDisplay::Composite(options) => {
            w.write_var_int(options.len() as i32);
            for option in options {
                write_slot_display(w, option);
            }
        }
    }
}

fn write_slot_displays(w: &mut PacketWriter, displays: &[SlotDisplay]) {
    w.write_var_int(displays.len() as i32);
    for display in displays {
        write_slot_display(w, display);
    }
}

pub fn write_recipe_display(w: &mut PacketWriter, display: &RecipeDisplay) {
    match display {
        RecipeDisplay::Shapeless {
            ingredients,
            result,
            station,
        } => {
            w.write_var_int(DisplayKind::CraftingShapeless.id() as i32);
            write_slot_displays(w, ingredients);
            write_slot_display(w, result);
            write_slot_display(w, station);
        }
        RecipeDisplay::Shaped {
            width,
            height,
            ingredients,
            result,
            station,
        } => {
            w.write_var_int(DisplayKind::CraftingShaped.id() as i32);
            w.write_var_int(*width as i32);
            w.write_var_int(*height as i32);
            write_slot_displays(w, ingredients);
            write_slot_display(w, result);
            write_slot_display(w, station);
        }
        RecipeDisplay::Furnace {
            ingredient,
            fuel,
            result,
            station,
            duration,
            experience,
        } => {
            w.write_var_int(DisplayKind::Furnace.id() as i32);
            write_slot_display(w, ingredient);
            write_slot_display(w, fuel);
            write_slot_display(w, result);
            write_slot_display(w, station);
            w.write_var_int(*duration as i32);
            w.write_float(*experience);
        }
        RecipeDisplay::Stonecutter {
            input,
            result,
            station,
        } => {
            w.write_var_int(DisplayKind::Stonecutter.id() as i32);
            write_slot_display(w, input);
            write_slot_display(w, result);
            write_slot_display(w, station);
        }
        RecipeDisplay::Smithing {
            template,
            base,
            addition,
            result,
            station,
        } => {
            w.write_var_int(DisplayKind::Smithing.id() as i32);
            write_slot_display(w, template);
            write_slot_display(w, base);
            write_slot_display(w, addition);
            write_slot_display(w, result);
            write_slot_display(w, station);
        }
    }
}
