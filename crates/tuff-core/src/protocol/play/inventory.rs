use tuff_vanilla_data::Item;
use tuff_vanilla_data::packet::play::Clientbound as Id;
use tuff_vanilla_data::recipe::Ingredient;

use crate::protocol::codec::WriteExt;
use crate::protocol::recipe::{DisplayEntry, RecipeDisplay, SlotDisplay};
use crate::protocol::recipe::{write_ingredient, write_recipe_display, write_slot_display};
use crate::protocol::{Encode, Packet};
use tuff_item::item::{ItemStack, ItemView};
use tuff_item::text::TextComponent;
use tuff_wire::PacketWriter;

#[derive(Packet)]
#[packet(id = Id::SetHeldSlot, encode_only)]
pub struct SetHeldSlot(#[packet(var_int)] pub i32);

/// Set Experience: the bar, level and total of the receiving player.
#[derive(Packet)]
#[packet(id = Id::SetExperience, encode_only)]
pub struct SetExperience {
    pub progress: f32,
    #[packet(var_int)]
    pub level: i32,
    #[packet(var_int)]
    pub total: i32,
}

#[derive(Packet)]
#[packet(id = Id::SetHealth, encode_only)]
pub struct SetHealth {
    pub health: f32,
    #[packet(var_int)]
    pub food: i32,
    pub saturation: f32,
}

pub struct ContainerSetContent<'a> {
    pub window_id: i32,
    pub state_id: i32,
    pub slots: &'a [Option<ItemStack>],
    pub carried: Option<ItemView<'a>>,
}

impl Encode for ContainerSetContent<'_> {
    const ID: i32 = Id::ContainerSetContent.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.window_id);
        w.write_var_int(self.state_id);
        w.write_var_int(self.slots.len() as i32);
        for slot in self.slots {
            w.write_slot(slot.as_ref().map(ItemStack::view));
        }
        w.write_slot(self.carried);
    }
}

#[derive(Packet)]
#[packet(id = Id::ContainerSetSlot, encode_only)]
pub struct ContainerSetSlot<'a> {
    #[packet(var_int)]
    pub window_id: i32,
    #[packet(var_int)]
    pub state_id: i32,
    pub slot: i16,
    #[packet(slot)]
    pub item: Option<ItemView<'a>>,
}

/// Open Screen: shows a container window of `menu_type` (a
/// `minecraft:menu` registry id) titled `title`.
pub struct OpenScreen<'a> {
    pub window_id: i32,
    pub menu_type: u32,
    pub title: &'a TextComponent,
}

impl Encode for OpenScreen<'_> {
    const ID: i32 = Id::OpenScreen.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.window_id);
        w.write_var_int(self.menu_type as i32);
        w.write_text(self.title);
    }
}

/// Mount Screen Open: shows the inventory of the mount `entity_id`, with
/// `columns` columns of chest slots.
#[derive(Packet)]
#[packet(id = Id::MountScreenOpen, encode_only)]
pub struct MountScreenOpen {
    #[packet(var_int)]
    pub window_id: i32,
    #[packet(var_int)]
    pub columns: i32,
    #[packet(int)]
    pub entity_id: i32,
}

/// Merchant Offers: the trades of the merchant whose screen is open.
pub struct MerchantOffers<'a> {
    pub window_id: i32,
    pub offers: &'a [crate::world::trading::MerchantOffer],
    pub level: i32,
    pub xp: i32,
    pub regular: bool,
    pub can_restock: bool,
}

fn write_trade_item(w: &mut PacketWriter, cost: &crate::world::trading::ItemCost) {
    w.write_var_int(cost.item.id() as i32);
    w.write_var_int(cost.count);
    let stack = cost.stack();
    let components = stack.components.network_added();
    w.write_var_int(components.len() as i32);
    for component in &components {
        w.write_var_int(component.kind().id() as i32);
        w.write_component(component);
    }
}

impl Encode for MerchantOffers<'_> {
    const ID: i32 = Id::MerchantOffers.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.window_id);
        w.write_var_int(self.offers.len() as i32);
        for offer in self.offers {
            write_trade_item(w, &offer.buy);
            w.write_slot(Some(offer.sell.view()));
            w.write_bool(offer.buy_b.is_some());
            if let Some(cost) = &offer.buy_b {
                write_trade_item(w, cost);
            }
            w.write_bool(offer.is_out_of_stock());
            w.write_int(offer.uses);
            w.write_int(offer.max_uses);
            w.write_int(offer.xp);
            w.write_int(offer.special_price);
            w.write_float(offer.price_multiplier);
            w.write_int(offer.demand);
        }
        w.write_var_int(self.level);
        w.write_var_int(self.xp);
        w.write_bool(self.regular);
        w.write_bool(self.can_restock);
    }
}

/// Close Container: the server forces the client's window shut.
#[derive(Packet)]
#[packet(id = Id::ContainerClose, encode_only)]
pub struct ContainerClose(#[packet(var_int)] pub i32);

/// Set Container Property: a window's numbered value, such as the
/// stonecutter's selected recipe.
#[derive(Packet)]
#[packet(id = Id::ContainerSetData, encode_only)]
pub struct ContainerSetData {
    #[packet(var_int)]
    pub window_id: i32,
    pub property: i16,
    pub value: i16,
}

/// Update Recipes: the item sets crafting stations accept and the
/// stonecutter's recipe list.
pub struct UpdateRecipes<'a> {
    pub property_sets: &'a [(&'a str, Vec<Item>)],
    pub stonecutter: &'a [(Ingredient, SlotDisplay)],
}

impl Encode for UpdateRecipes<'_> {
    const ID: i32 = Id::UpdateRecipes.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.property_sets.len() as i32);
        for (name, items) in self.property_sets {
            w.write_string(&format!("minecraft:{name}"));
            w.write_var_int(items.len() as i32);
            for item in items {
                w.write_var_int(item.id() as i32);
            }
        }
        w.write_var_int(self.stonecutter.len() as i32);
        for (ingredient, display) in self.stonecutter {
            write_ingredient(w, ingredient);
            write_slot_display(w, display);
        }
    }
}

pub mod recipe_flags {
    pub const NOTIFICATION: u8 = 1;
    pub const HIGHLIGHT: u8 = 2;
}

/// Recipe Book Add: entries with their flags.
pub struct RecipeBookAdd<'a> {
    pub entries: &'a [(&'a DisplayEntry, u8)],
    pub replace: bool,
}

impl Encode for RecipeBookAdd<'_> {
    const ID: i32 = Id::RecipeBookAdd.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.entries.len() as i32);
        for (entry, flags) in self.entries {
            w.write_var_int(entry.id as i32);
            write_recipe_display(w, &entry.display);
            w.write_var_int(entry.group.map_or(0, |group| group as i32 + 1));
            w.write_var_int(entry.category.id() as i32);
            w.write_bool(entry.requirements.is_some());
            if let Some(requirements) = &entry.requirements {
                w.write_var_int(requirements.len() as i32);
                for ingredient in requirements {
                    write_ingredient(w, ingredient);
                }
            }
            w.write_unsigned_byte(*flags);
        }
        w.write_bool(self.replace);
    }
}

/// Recipe Book Remove: display ids the client forgets.
pub struct RecipeBookRemove<'a>(pub &'a [u32]);

impl Encode for RecipeBookRemove<'_> {
    const ID: i32 = Id::RecipeBookRemove.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.0.len() as i32);
        for id in self.0 {
            w.write_var_int(*id as i32);
        }
    }
}

/// Recipe Book Settings: `(open, filtering)` per book, in the game's book
/// order (crafting, furnace, blast furnace, smoker).
pub struct RecipeBookSettings(pub [(bool, bool); 4]);

impl Encode for RecipeBookSettings {
    const ID: i32 = Id::RecipeBookSettings.id();

    fn encode(&self, w: &mut PacketWriter) {
        for (open, filtering) in self.0 {
            w.write_bool(open);
            w.write_bool(filtering);
        }
    }
}

/// Place Ghost Recipe: the client greys the recipe into the grid because
/// its ingredients are missing.
pub struct PlaceGhostRecipe<'a> {
    pub window_id: i32,
    pub display: &'a RecipeDisplay,
}

impl Encode for PlaceGhostRecipe<'_> {
    const ID: i32 = Id::PlaceGhostRecipe.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.window_id);
        write_recipe_display(w, self.display);
    }
}

#[derive(Packet)]
#[packet(id = Id::SetPlayerInventory, encode_only)]
pub struct SetPlayerInventory<'a> {
    #[packet(var_int)]
    pub slot: i32,
    #[packet(slot)]
    pub item: Option<ItemView<'a>>,
}

#[derive(Packet)]
#[packet(id = Id::SetCursorItem, encode_only)]
pub struct SetCursorItem<'a>(#[packet(slot)] pub Option<ItemView<'a>>);
