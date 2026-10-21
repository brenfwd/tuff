//! Container menus: a typed slot layout (a container section followed by
//! the player's inventory section) and the game's click semantics over it
//! (https://minecraft.wiki/w/Java_Edition_protocol/Inventory).
//!
//! A menu never owns the player's inventory; clicks are computed against a
//! container snapshot plus the inventory the session owns, so the same code
//! serves the survival inventory window, block containers and ender chests.

use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::Menu as MenuType;
use tuff_vanilla_data::tag;

use std::sync::Arc;

use crate::world::crafting::RecipeSet;
use crate::world::inventory::Inventory;
use crate::world::loom::{is_loom_dye, is_loom_pattern_item};
use crate::world::trading::{self, MerchantOffer};
use tuff_item::item::ItemStack;
use tuff_item::item::bundle::{self, fits_inside_container_item};

pub const OUTSIDE: i16 = -999;
/// Main inventory plus hotbar, the section every container menu ends with.
pub const PLAYER_SECTION: usize = 36;
const PLAYER_WINDOW_SLOTS: usize = 46;
const PLAYER_MAIN_START: usize = 9;
const PLAYER_HOTBAR_START: usize = 36;
const PLAYER_OFFHAND: usize = 45;

pub mod mode {
    pub const PICKUP: i32 = 0;
    pub const QUICK_MOVE: i32 = 1;
    pub const SWAP: i32 = 2;
    pub const CLONE: i32 = 3;
    pub const THROW: i32 = 4;
    pub const DRAG: i32 = 5;
    pub const PICKUP_ALL: i32 = 6;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    /// Window 0: crafting result, 2x2 grid, armor, main, hotbar, off hand.
    Player,
    /// A 9-wide grid of `rows` rows: chests, barrels, ender chests.
    Generic {
        rows: u8,
    },
    ShulkerBox,
    Hopper,
    /// Droppers and dispensers.
    Generic3x3,
    /// A crafting table: result slot 0, then the 3x3 grid.
    Crafting,
    /// Input slot 0, result slot 1.
    Stonecutter,
    /// Item slot 0, lapis slot 1.
    Enchantment,
    /// The payment slot 0.
    Beacon,
    /// Input slot 0, addition slot 1, result slot 2.
    Anvil,
    /// Map slot 0, paper/map/glass pane slot 1, result slot 2.
    Cartography,
    /// Banner slot 0, dye slot 1, pattern item slot 2, result slot 3.
    Loom,
    /// Two item slots, then result slot 2.
    Grindstone,
    /// Template, base and addition slots, then result slot 3.
    Smithing,
    /// Input slot 0, fuel slot 1, result slot 2.
    Furnace,
    BlastFurnace,
    Smoker,
    /// Bottle slots 0-2, reagent slot 3, fuel slot 4.
    BrewingStand,
    /// A villager's or wandering trader's trades: two payment slots, then
    /// the result.
    Merchant,
    /// Nine grid slots, then the non-interactive result preview.
    Crafter,
    /// A lectern's open book: one display slot and no player section.
    Lectern,
    /// A mount's inventory: saddle slot 0, body armor slot 1, then
    /// `columns` columns of three chest slots. `saddle` and `armor` say
    /// which equipment slots it uses.
    Mount {
        columns: u8,
        saddle: bool,
        armor: MountArmor,
    },
}

/// What a mount wears in its body slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountArmor {
    None,
    Horse,
    Carpet,
    Nautilus,
}

impl MountArmor {
    pub fn fits(self, stack: &ItemStack) -> bool {
        let body = stack
            .equippable()
            .is_some_and(|eq| eq.slot == EquipmentSlot::Body);
        match self {
            Self::None => false,
            Self::Horse => body && stack.item.name().ends_with("_horse_armor"),
            Self::Nautilus => body && stack.item.name().ends_with("_nautilus_armor"),
            Self::Carpet => {
                body && tuff_datapack::tags::is(tag::item::key::WOOL_CARPETS, stack.item)
            }
        }
    }
}

pub mod mount {
    pub const SADDLE: usize = 0;
    pub const BODY: usize = 1;
    pub const CHEST: usize = 2;
}

pub mod brewing {
    pub const INGREDIENT: usize = 3;
    pub const FUEL: usize = 4;
}

pub mod furnace {
    pub const INPUT: usize = 0;
    pub const FUEL: usize = 1;
    pub const RESULT: usize = 2;
}

pub mod smithing {
    pub const TEMPLATE: usize = 0;
    pub const BASE: usize = 1;
    pub const ADDITION: usize = 2;
    pub const RESULT: usize = 3;
}

pub mod merchant {
    pub const PAYMENT_A: usize = 0;
    pub const PAYMENT_B: usize = 1;
    pub const RESULT: usize = 2;
}

pub mod crafter {
    pub const GRID: usize = 9;
    pub const PREVIEW: usize = 9;
    pub const WIRE_PREVIEW: usize = 45;
}

pub mod stonecutter {
    pub const INPUT: usize = 0;
    pub const RESULT: usize = 1;
}

pub mod enchantment {
    pub const ITEM: usize = 0;
    pub const LAPIS: usize = 1;
}

pub mod anvil {
    pub const INPUT: usize = 0;
    pub const ADDITION: usize = 1;
    pub const RESULT: usize = 2;
}

pub mod grindstone {
    pub const INPUT: usize = 0;
    pub const ADDITIONAL: usize = 1;
    pub const RESULT: usize = 2;
}

pub mod loom {
    pub const BANNER: usize = 0;
    pub const DYE: usize = 1;
    pub const PATTERN: usize = 2;
    pub const RESULT: usize = 3;
}

pub mod cartography {
    pub const MAP: usize = 0;
    pub const ADDITIONAL: usize = 1;
    pub const RESULT: usize = 2;
}

/// What happens when items are taken from a menu's result slot: the
/// ingredients are used up and the result computed afresh.
pub trait ResultHook {
    /// Whether the result slot may be picked up at all right now.
    fn can_take(&self) -> bool {
        true
    }

    fn taken(
        &mut self,
        container: &mut [Option<ItemStack>],
        inventory: &mut Inventory,
        drops: &mut Vec<ItemStack>,
    );
}

/// For menus without a result slot.
pub struct NoResult;

impl ResultHook for NoResult {
    fn taken(&mut self, _: &mut [Option<ItemStack>], _: &mut Inventory, _: &mut Vec<ItemStack>) {}
}

impl MenuKind {
    pub fn container_size(self) -> usize {
        match self {
            Self::Player => 0,
            Self::Generic { rows } => rows as usize * 9,
            Self::ShulkerBox => 27,
            Self::Hopper => 5,
            Self::Generic3x3 => 9,
            Self::Crafting => 10,
            Self::Stonecutter => 2,
            Self::Anvil => 3,
            Self::Enchantment => 2,
            Self::Beacon => 1,
            Self::Cartography => 3,
            Self::Loom => 4,
            Self::Grindstone => 3,
            Self::Smithing => 4,
            Self::Furnace | Self::BlastFurnace | Self::Smoker => 3,
            Self::BrewingStand => 5,
            Self::Merchant => 3,
            Self::Lectern => 1,
            Self::Crafter => 10,
            Self::Mount { columns, .. } => mount::CHEST + columns as usize * 3,
        }
    }

    /// The slot number the client uses for the internal window slot `slot`:
    /// the crafter shows its preview after the player's slots.
    pub fn wire_slot(self, slot: usize) -> usize {
        match (self, slot) {
            (Self::Crafter, crafter::PREVIEW) => crafter::WIRE_PREVIEW,
            (Self::Crafter, s) if s > crafter::PREVIEW => s - 1,
            _ => slot,
        }
    }

    /// The internal window slot for a slot number the client sent.
    pub fn internal_slot(self, wire: usize) -> usize {
        match (self, wire) {
            (Self::Crafter, crafter::WIRE_PREVIEW) => crafter::PREVIEW,
            (Self::Crafter, s) if s >= crafter::PREVIEW => s + 1,
            _ => wire,
        }
    }

    /// The slot that holds a recipe's output, which can only be taken.
    pub fn result_slot(self) -> Option<usize> {
        match self {
            Self::Player | Self::Crafting => Some(0),
            Self::Stonecutter => Some(stonecutter::RESULT),
            Self::Anvil => Some(anvil::RESULT),
            Self::Cartography => Some(cartography::RESULT),
            Self::Loom => Some(loom::RESULT),
            Self::Grindstone => Some(grindstone::RESULT),
            Self::Smithing => Some(smithing::RESULT),
            Self::Furnace | Self::BlastFurnace | Self::Smoker => Some(furnace::RESULT),
            Self::Merchant => Some(merchant::RESULT),
            _ => None,
        }
    }

    /// The container slots the recipe book fills, with the grid's width
    /// and height: a crafting grid, or a furnace's single input slot.
    pub fn grid(self) -> Option<(std::ops::Range<usize>, usize, usize)> {
        match self {
            Self::Player => Some((1..5, 2, 2)),
            Self::Crafting => Some((1..10, 3, 3)),
            Self::Furnace | Self::BlastFurnace | Self::Smoker => Some((0..1, 1, 1)),
            _ => None,
        }
    }

    pub fn is_furnace(self) -> bool {
        matches!(self, Self::Furnace | Self::BlastFurnace | Self::Smoker)
    }

    /// The recipe property set naming what a furnace menu smelts.
    pub fn cooking_set(self) -> Option<&'static str> {
        Some(match self {
            Self::Furnace => "furnace_input",
            Self::BlastFurnace => "blast_furnace_input",
            Self::Smoker => "smoker_input",
            _ => return None,
        })
    }

    pub fn can_smelt(self, recipes: &RecipeSet, stack: &ItemStack) -> bool {
        self.cooking_set()
            .is_some_and(|set| recipes.property_set(set).binary_search(&stack.item).is_ok())
    }

    /// The most of `stack` that `slot` holds: a furnace's fuel slot takes
    /// one bucket.
    pub fn max_stack(self, slot: usize, stack: &ItemStack) -> u8 {
        if self.is_furnace() && slot == furnace::FUEL && stack.item == Item::Bucket {
            return 1;
        }
        if self == Self::Beacon {
            return 1;
        }
        if self == Self::Enchantment && slot == enchantment::ITEM {
            return 1;
        }
        if self == Self::BrewingStand && slot < brewing::INGREDIENT {
            return 1;
        }
        if matches!(self, Self::Mount { .. }) && slot < mount::CHEST {
            return 1;
        }
        if self == Self::Player && (5..=8).contains(&slot) {
            return 1;
        }
        stack.max_stack_size()
    }

    /// How many of the player's slots the menu shows below its container.
    pub fn player_section(self) -> usize {
        match self {
            Self::Player | Self::Lectern => 0,
            _ => PLAYER_SECTION,
        }
    }

    pub fn len(self) -> usize {
        match self {
            Self::Player => PLAYER_WINDOW_SLOTS,
            Self::Lectern => 1,
            _ => self.container_size() + PLAYER_SECTION,
        }
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// The screen the client shows; the player window has none.
    pub fn menu_type(self) -> Option<MenuType> {
        Some(match self {
            Self::Player => return None,
            Self::Generic { rows: 1 } => MenuType::Generic9x1,
            Self::Generic { rows: 2 } => MenuType::Generic9x2,
            Self::Generic { rows: 3 } => MenuType::Generic9x3,
            Self::Generic { rows: 4 } => MenuType::Generic9x4,
            Self::Generic { rows: 5 } => MenuType::Generic9x5,
            Self::Generic { rows: _ } => MenuType::Generic9x6,
            Self::ShulkerBox => MenuType::ShulkerBox,
            Self::Hopper => MenuType::Hopper,
            Self::Generic3x3 => MenuType::Generic3x3,
            Self::Crafting => MenuType::Crafting,
            Self::Stonecutter => MenuType::Stonecutter,
            Self::Anvil => MenuType::Anvil,
            Self::Enchantment => MenuType::Enchantment,
            Self::Beacon => MenuType::Beacon,
            Self::Cartography => MenuType::CartographyTable,
            Self::Loom => MenuType::Loom,
            Self::Grindstone => MenuType::Grindstone,
            Self::Smithing => MenuType::Smithing,
            Self::Furnace => MenuType::Furnace,
            Self::BlastFurnace => MenuType::BlastFurnace,
            Self::Smoker => MenuType::Smoker,
            Self::BrewingStand => MenuType::BrewingStand,
            Self::Merchant => MenuType::Merchant,
            Self::Lectern => MenuType::Lectern,
            Self::Crafter => MenuType::Crafter3x3,
            Self::Mount { .. } => return None,
        })
    }

    /// The inventory slot a window slot of this menu shows, if it is one of
    /// the player's.
    pub fn inventory_slot(self, slot: usize) -> Option<usize> {
        match self {
            Self::Player => (slot < PLAYER_WINDOW_SLOTS).then_some(slot),
            Self::Lectern => None,
            _ => {
                let offset = slot.checked_sub(self.container_size())?;
                match offset {
                    0..27 => Some(PLAYER_MAIN_START + offset),
                    27..PLAYER_SECTION => Some(PLAYER_HOTBAR_START + offset - 27),
                    _ => None,
                }
            }
        }
    }

    /// The window slot showing an inventory slot, if this menu shows it.
    pub fn window_slot(self, inventory_slot: usize) -> Option<usize> {
        match self {
            Self::Player => (inventory_slot < PLAYER_WINDOW_SLOTS).then_some(inventory_slot),
            Self::Lectern => None,
            _ => {
                let offset = match inventory_slot {
                    PLAYER_MAIN_START..PLAYER_HOTBAR_START => inventory_slot - PLAYER_MAIN_START,
                    PLAYER_HOTBAR_START..PLAYER_OFFHAND => {
                        inventory_slot - PLAYER_HOTBAR_START + 27
                    }
                    _ => return None,
                };
                Some(self.container_size() + offset)
            }
        }
    }

    /// Whether `stack` may be put in `slot`, the per-slot placement rule.
    pub fn may_place(self, recipes: &RecipeSet, slot: usize, stack: &ItemStack) -> bool {
        if self.result_slot() == Some(slot)
            || self == Self::Lectern
            || (self == Self::Crafter && slot == crafter::PREVIEW)
        {
            return false;
        }
        match self {
            Self::Player => match slot {
                5..=8 => equipment_slot(stack).and_then(Inventory::armor_slot) == Some(slot),
                _ => true,
            },
            Self::ShulkerBox if slot < self.container_size() => fits_inside_container_item(stack),
            Self::Grindstone if slot < grindstone::RESULT => {
                stack.is_damageable() || holds_enchantments(stack)
            }
            Self::Beacon => tag::item::BEACON_PAYMENT_ITEMS.contains(&stack.item),
            Self::Enchantment if slot == enchantment::LAPIS => stack.item == Item::LapisLazuli,
            Self::Cartography if slot == cartography::MAP => stack.components.map_id.is_some(),
            Self::Cartography if slot == cartography::ADDITIONAL => {
                is_cartography_additional(stack)
            }
            Self::Loom => match slot {
                loom::BANNER => stack.item.banner_color().is_some(),
                loom::DYE => is_loom_dye(stack),
                _ => is_loom_pattern_item(stack),
            },
            Self::Smithing if slot < smithing::RESULT => {
                let set = match slot {
                    smithing::TEMPLATE => "smithing_template",
                    smithing::BASE => "smithing_base",
                    _ => "smithing_addition",
                };
                recipes.property_set(set).binary_search(&stack.item).is_ok()
            }
            _ if self.is_furnace() && slot == furnace::FUEL => is_fuel(stack),
            Self::BrewingStand => match slot {
                0..brewing::INGREDIENT => recipes.is_potion_input(stack),
                brewing::INGREDIENT => recipes.is_brewing_reagent(stack),
                brewing::FUEL => recipes.is_brewing_fuel(stack),
                _ => true,
            },
            Self::Mount { saddle, armor, .. } => match slot {
                mount::SADDLE => {
                    saddle
                        && stack
                            .equippable()
                            .is_some_and(|eq| eq.slot == EquipmentSlot::Saddle)
                }
                mount::BODY => armor.fits(stack),
                _ => true,
            },
            _ => true,
        }
    }

    fn may_pickup(self, slot: usize) -> bool {
        !(self == Self::Crafter && slot == crafter::PREVIEW)
    }

    /// Where a shift-click on `slot` holding `stack` moves it: slot ranges
    /// tried in order, each forwards or backwards.
    fn quick_move_targets(
        self,
        slot: usize,
        stack: &ItemStack,
        view: &View<'_>,
    ) -> Vec<(usize, usize, bool)> {
        let n = self.container_size();
        let recipes = view.recipes;
        match self {
            Self::Player => {
                let equipment = equipment_slot(stack);
                let armor = equipment
                    .and_then(Inventory::armor_slot)
                    .filter(|s| (5..=8).contains(s));
                match slot {
                    0 => vec![(9, 45, true)],
                    1..=8 => vec![(9, 45, false)],
                    _ if armor.is_some_and(|s| view.get(s).is_none()) => {
                        let s = armor.unwrap();
                        vec![(s, s + 1, false)]
                    }
                    _ if equipment == Some(EquipmentSlot::Offhand)
                        && view.get(PLAYER_OFFHAND).is_none() =>
                    {
                        vec![(PLAYER_OFFHAND, PLAYER_OFFHAND + 1, false)]
                    }
                    9..=35 => vec![(36, 45, false)],
                    36..=44 => vec![(9, 36, false)],
                    _ => vec![(9, 45, false)],
                }
            }
            Self::Crafting => match slot {
                0 => vec![(10, 46, true)],
                1..=9 => vec![(10, 46, false)],
                10..=36 => vec![(1, 10, false), (37, 46, false)],
                _ => vec![(1, 10, false), (10, 37, false)],
            },
            Self::Crafter => match slot {
                0..=8 => vec![(10, 46, false)],
                crafter::PREVIEW => vec![],
                _ => vec![(0, 9, false)],
            },
            Self::Anvil => match slot {
                anvil::RESULT => vec![(3, 39, true)],
                anvil::INPUT | anvil::ADDITION => vec![(3, 39, false)],
                _ => vec![(0, 2, false)],
            },
            Self::Beacon => match slot {
                0 => vec![(1, 37, true)],
                _ if view.get(0).is_none()
                    && self.may_place(recipes, 0, stack)
                    && stack.count == 1 =>
                {
                    vec![(0, 1, false)]
                }
                1..=27 => vec![(28, 37, false)],
                _ => vec![(1, 28, false)],
            },
            Self::Enchantment => match slot {
                enchantment::ITEM | enchantment::LAPIS => vec![(2, 38, true)],
                _ if stack.item == Item::LapisLazuli => vec![(1, 2, true)],
                _ => vec![(0, 1, false)],
            },
            Self::Stonecutter => match slot {
                stonecutter::RESULT => vec![(2, 38, true)],
                stonecutter::INPUT => vec![(2, 38, false)],
                _ if recipes.stonecutter_accepts(stack.item) => vec![(0, 1, false)],
                2..=28 => vec![(29, 38, false)],
                _ => vec![(2, 29, false)],
            },
            Self::Grindstone => match slot {
                grindstone::RESULT => vec![(3, 39, true)],
                grindstone::INPUT | grindstone::ADDITIONAL => vec![(3, 39, false)],
                _ if view.get(grindstone::INPUT).is_none()
                    || view.get(grindstone::ADDITIONAL).is_none() =>
                {
                    vec![(0, 2, false)]
                }
                3..=29 => vec![(30, 39, false)],
                _ => vec![(3, 30, false)],
            },
            Self::Cartography => match slot {
                cartography::RESULT => vec![(3, 39, true)],
                cartography::MAP | cartography::ADDITIONAL => vec![(3, 39, false)],
                _ if stack.components.map_id.is_some() => vec![(0, 1, false)],
                _ if is_cartography_additional(stack) => vec![(1, 2, false)],
                3..=29 => vec![(30, 39, false)],
                _ => vec![(3, 30, false)],
            },
            Self::Loom => match slot {
                loom::RESULT => vec![(4, 40, true)],
                loom::BANNER | loom::DYE | loom::PATTERN => vec![(4, 40, false)],
                _ if stack.item.banner_color().is_some() => vec![(0, 1, false)],
                _ if is_loom_dye(stack) => vec![(1, 2, false)],
                _ if is_loom_pattern_item(stack) => vec![(2, 3, false)],
                4..=30 => vec![(31, 40, false)],
                _ => vec![(4, 31, false)],
            },
            Self::Smithing => match slot {
                smithing::RESULT => vec![(4, 40, true)],
                0..=2 => vec![(4, 40, false)],
                _ if (0..smithing::RESULT).any(|input| {
                    view.get(input).is_none() && self.may_place(recipes, input, stack)
                }) =>
                {
                    vec![(0, smithing::RESULT, false)]
                }
                4..=30 => vec![(31, 40, false)],
                _ => vec![(4, 31, false)],
            },
            _ if self.is_furnace() => match slot {
                furnace::RESULT => vec![(3, 39, true)],
                furnace::INPUT | furnace::FUEL => vec![(3, 39, false)],
                _ if self.can_smelt(recipes, stack) => {
                    vec![(furnace::INPUT, furnace::INPUT + 1, false)]
                }
                _ if stack.item.cooking_fuel().is_some() => {
                    vec![(furnace::FUEL, furnace::FUEL + 1, false)]
                }
                3..=29 => vec![(30, 39, false)],
                _ => vec![(3, 30, false)],
            },
            Self::BrewingStand => match slot {
                0..=4 => vec![(5, 41, true)],
                _ if recipes.is_brewing_fuel(stack) => {
                    let fuel_takes = view.get(brewing::FUEL).is_none_or(|fuel| {
                        fuel.stacks_with(stack) && fuel.count < self.max_stack(brewing::FUEL, fuel)
                    });
                    if fuel_takes {
                        vec![(brewing::FUEL, brewing::FUEL + 1, false)]
                    } else if recipes.is_brewing_reagent(stack) {
                        vec![(brewing::INGREDIENT, brewing::INGREDIENT + 1, false)]
                    } else {
                        Vec::new()
                    }
                }
                _ if recipes.is_brewing_reagent(stack) => {
                    vec![(brewing::INGREDIENT, brewing::INGREDIENT + 1, false)]
                }
                _ if recipes.is_potion_input(stack) => vec![(0, 3, false)],
                5..=31 => vec![(32, 41, false)],
                32..=40 => vec![(5, 32, false)],
                _ => vec![(5, 41, false)],
            },
            Self::Merchant => match slot {
                merchant::RESULT => vec![(3, 39, true)],
                merchant::PAYMENT_A | merchant::PAYMENT_B => vec![(3, 39, false)],
                3..=29 => vec![(30, 39, false)],
                _ => vec![(3, 30, false)],
            },
            Self::Mount { .. } => {
                let accepts = |slot: usize| {
                    self.may_place(recipes, slot, stack)
                        && view.get(slot).is_none_or(|held| {
                            held.stacks_with(stack) && held.count < self.max_stack(slot, held)
                        })
                };
                let main = n + 27;
                match slot {
                    _ if slot < n => vec![(n, self.len(), true)],
                    _ if accepts(mount::BODY) && view.get(mount::BODY).is_none() => {
                        vec![(mount::BODY, mount::BODY + 1, false)]
                    }
                    _ if accepts(mount::SADDLE) && view.get(mount::SADDLE).is_none() => {
                        vec![(mount::SADDLE, mount::SADDLE + 1, false)]
                    }
                    _ if (mount::CHEST..n).any(accepts) => vec![(mount::CHEST, n, false)],
                    _ if slot < main => vec![(main, self.len(), false)],
                    _ => vec![(n, main, false)],
                }
            }
            _ if slot < n => vec![(n, self.len(), true)],
            _ => vec![(0, n, false)],
        }
    }
}

mod hooks;
pub use hooks::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drag {
    /// 0 splits evenly, 1 places one per slot, 2 fills each (creative).
    kind: u8,
    slots: Vec<usize>,
}

/// The slots of a menu, looked up through its layout.
struct View<'a> {
    kind: MenuKind,
    recipes: &'a RecipeSet,
    container: &'a mut [Option<ItemStack>],
    inventory: &'a mut Inventory,
    hook: &'a mut dyn ResultHook,
    drops: Vec<ItemStack>,
    creative: bool,
}

impl View<'_> {
    fn len(&self) -> usize {
        self.kind.len()
    }

    fn is_result(&self, slot: usize) -> bool {
        self.kind.result_slot() == Some(slot)
    }

    /// Runs the result hook after items left the result slot.
    fn taken(&mut self, slot: usize) {
        if self.is_result(slot) {
            let mut drops = std::mem::take(&mut self.drops);
            self.hook.taken(self.container, self.inventory, &mut drops);
            self.drops = drops;
        }
    }

    fn get(&self, slot: usize) -> Option<&ItemStack> {
        if slot < self.kind.container_size() {
            self.container[slot].as_ref()
        } else {
            self.kind
                .inventory_slot(slot)
                .and_then(|s| self.inventory.slots()[s].as_ref())
        }
    }

    fn get_mut(&mut self, slot: usize) -> &mut Option<ItemStack> {
        if slot < self.kind.container_size() {
            &mut self.container[slot]
        } else {
            let index = self
                .kind
                .inventory_slot(slot)
                .expect("window slot in range");
            self.inventory.slot_mut(index)
        }
    }

    fn take(&mut self, slot: usize, count: u8) -> Option<ItemStack> {
        let entry = self.get_mut(slot);
        let stack = entry.as_mut()?;
        let taken = count.min(stack.count);
        if taken == 0 {
            return None;
        }
        if stack.count > taken {
            stack.count -= taken;
            Some(stack.clone().with_count(taken))
        } else {
            entry.take()
        }
    }

    fn may_place(&self, slot: usize, stack: &ItemStack) -> bool {
        self.kind.may_place(self.recipes, slot, stack)
    }

    fn may_pickup(&self, slot: usize) -> bool {
        self.kind.may_pickup(slot)
            && (!self.is_result(slot) || self.hook.can_take())
            && !(self.kind == MenuKind::Player
                && (5..=8).contains(&slot)
                && !self.creative
                && self.get(slot).is_some_and(has_binding_curse))
    }

    fn max_stack(&self, slot: usize, stack: &ItemStack) -> u8 {
        self.kind.max_stack(slot, stack)
    }

    fn allow_modification(&self, slot: usize) -> bool {
        self.may_pickup(slot) && self.get(slot).is_none_or(|s| self.may_place(slot, s))
    }

    /// Takes up to `amount` from `slot`, never more than `max`, as the
    /// game's `tryRemove`.
    fn try_remove(&mut self, slot: usize, amount: u8, max: u8) -> Option<ItemStack> {
        if !self.may_pickup(slot) {
            return None;
        }
        let count = self.get(slot).map_or(0, |s| s.count);
        if !self.allow_modification(slot) && max < count {
            return None;
        }
        self.take(slot, amount.min(max))
    }

    /// Inserts up to `amount` of `stack` into `slot`, returning what is
    /// left, as the game's `safeInsert`.
    fn safe_insert(&mut self, slot: usize, mut stack: ItemStack, amount: u8) -> Option<ItemStack> {
        if !self.may_place(slot, &stack) {
            return Some(stack);
        }
        let limit = self.max_stack(slot, &stack);
        let existing = self.get_mut(slot);
        let current = existing.as_ref().map_or(0, |s| s.count);
        let transferable = amount.min(stack.count).min(limit.saturating_sub(current));
        match existing {
            None => {
                *existing = Some(stack.clone().with_count(transferable));
                stack.count -= transferable;
            }
            Some(target) if target.stacks_with(&stack) => {
                target.count += transferable;
                stack.count -= transferable;
            }
            Some(_) => {}
        }
        (stack.count > 0).then_some(stack)
    }

    /// Whether `stack` could go into `slot` regardless of how much room it
    /// has, as the game's `canItemQuickReplace` with `ignoreSize`.
    fn quick_replaceable(&self, slot: usize, stack: &ItemStack) -> bool {
        match self.get(slot) {
            None => true,
            Some(existing) => {
                existing.stacks_with(stack) && existing.count <= self.max_stack(slot, stack)
            }
        }
    }

    /// Merges `stack` into `start..end`, first onto matching stacks then
    /// into empty slots; returns whether anything moved.
    fn move_into(
        &mut self,
        stack: &mut ItemStack,
        start: usize,
        end: usize,
        backwards: bool,
    ) -> bool {
        let mut changed = false;
        let order = || -> Vec<usize> {
            if backwards {
                (start..end).rev().collect()
            } else {
                (start..end).collect()
            }
        };
        if stack.max_stack_size() > 1 {
            for slot in order() {
                if stack.count == 0 {
                    break;
                }
                let limit = self.max_stack(slot, stack);
                let Some(target) = self.get_mut(slot).as_mut() else {
                    continue;
                };
                if !target.stacks_with(stack) {
                    continue;
                }
                let total = target.count as u16 + stack.count as u16;
                if total <= limit as u16 {
                    target.count = total as u8;
                    stack.count = 0;
                    changed = true;
                } else if target.count < limit {
                    stack.count -= limit - target.count;
                    target.count = limit;
                    changed = true;
                }
            }
        }
        if stack.count > 0 {
            for slot in order() {
                if self.get(slot).is_some() || !self.may_place(slot, stack) {
                    continue;
                }
                let moved = stack.count.min(self.max_stack(slot, stack));
                *self.get_mut(slot) = Some(stack.clone().with_count(moved));
                stack.count -= moved;
                changed = true;
                break;
            }
        }
        changed
    }

    /// Shift-click: returns a copy of what the slot held before, or `None`
    /// if nothing could move.
    fn quick_move(&mut self, slot: usize) -> Option<ItemStack> {
        let mut stack = self.get(slot)?.clone();
        let clicked = stack.clone();
        let targets = self.kind.quick_move_targets(slot, &stack, self);
        let mut moved = false;
        for (start, end, backwards) in targets {
            moved |= self.move_into(&mut stack, start, end, backwards);
            if stack.count == 0 {
                break;
            }
        }
        if !moved {
            return None;
        }
        if self.is_result(slot) {
            *self.get_mut(slot) = None;
            self.taken(slot);
            if stack.count > 0 {
                self.drops.push(stack);
            }
        } else {
            *self.get_mut(slot) = (stack.count > 0).then_some(stack);
        }
        Some(clicked)
    }
}

/// A Click Container action.
#[derive(Debug, Clone, Copy)]
pub struct Click {
    pub slot: i16,
    pub button: i8,
    pub mode: i32,
}

/// What a click needs beyond the menu itself.
pub struct ClickEnv<'a> {
    pub creative: bool,
    pub hook: &'a mut dyn ResultHook,
    pub recipes: &'a RecipeSet,
}

/// Applies a click to a menu of `kind` over `container` and the player's
/// inventory, returning the stacks to drop into the world. `creative`
/// enables clone clicks and fill drags; `hook` runs when the result slot
/// is taken from.
pub fn click(
    kind: MenuKind,
    container: &mut [Option<ItemStack>],
    inventory: &mut Inventory,
    drag: &mut Option<Drag>,
    click: Click,
    env: ClickEnv<'_>,
) -> Vec<ItemStack> {
    let Click { slot, button, mode } = click;
    let ClickEnv {
        creative,
        hook,
        recipes,
    } = env;
    let mut view = View {
        kind,
        recipes,
        container,
        inventory,
        hook,
        drops: Vec::new(),
        creative,
    };
    let index = usize::try_from(slot).ok().filter(|s| *s < view.len());
    if mode == mode::DRAG {
        drag_step(&mut view, drag, index, button, creative);
        return view.drops;
    }
    if drag.take().is_some() {
        return view.drops;
    }
    let primary = matches!(mode, mode::PICKUP | mode::QUICK_MOVE) && matches!(button, 0 | 1);
    if !primary {
        match (mode, index) {
            (mode::SWAP, Some(index)) => swap_with_hotbar(&mut view, index, button),
            (mode::CLONE, Some(index)) if creative && view.inventory.carried().is_none() => {
                if let Some(stack) = view.get(index) {
                    let cloned = stack.clone().with_count(stack.max_stack_size());
                    view.inventory.set_carried(Some(cloned));
                }
            }
            (mode::THROW, Some(index)) if view.inventory.carried().is_none() => {
                let amount = match button {
                    0 => 1,
                    _ => view.get(index).map_or(0, |s| s.count),
                };
                while let Some(stack) = view.try_remove(index, amount, u8::MAX) {
                    let item = stack.item;
                    view.drops.push(stack);
                    view.taken(index);
                    if button != 1 || !view.get(index).is_some_and(|s| s.item == item) {
                        break;
                    }
                }
            }
            (mode::PICKUP_ALL, Some(index)) => pickup_all(&mut view, index, button),
            _ => {}
        }
        return view.drops;
    }
    let left = button == 0;
    let Some(index) = index else {
        if slot == OUTSIDE
            && let Some(mut carried) = view.inventory.carried().cloned()
        {
            if left {
                view.inventory.set_carried(None);
                view.drops.push(carried);
            } else {
                let one = carried.clone().with_count(1);
                carried.count -= 1;
                view.inventory
                    .set_carried((carried.count > 0).then_some(carried));
                view.drops.push(one);
            }
        }
        return view.drops;
    };
    if mode == mode::QUICK_MOVE {
        if !view.may_pickup(index) {
            return view.drops;
        }
        while let Some(moved) = view.quick_move(index) {
            let same = view.get(index).is_some_and(|s| s.item == moved.item);
            if !same {
                break;
            }
        }
        return view.drops;
    }
    pickup(&mut view, index, left);
    view.drops
}

/// The sounds a bundle makes when clicked with; the player's own client
/// plays them itself, so only others are sent them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleSound {
    Insert,
    InsertFail,
    RemoveOne,
}

/// A bundle on the cursor clicking a slot: the slot's stack goes in (left)
/// or one stack comes out into an empty slot (right).
fn bundle_on_slot(view: &mut View<'_>, index: usize, left: bool) -> bool {
    let Some(mut bundle) = view.inventory.carried().filter(|c| c.is_bundle()).cloned() else {
        return false;
    };
    let other = view.get(index).cloned();
    let handled = match (left, other) {
        (true, Some(other)) => {
            let inserted = bundle.edit_bundle(|contents| {
                let room = bundle::item_weight(&other).map_or(0, |w| contents.room_for(w));
                if !bundle::can_fit(&other) {
                    return 0;
                }
                let room = room.min(u8::MAX as u64) as u8;
                let Some(mut taken) = view.try_remove(index, other.count, room) else {
                    return 0;
                };
                view.taken(index);
                contents.try_insert(&mut taken)
            });
            view.inventory.note_bundle_sound(if inserted > 0 {
                BundleSound::Insert
            } else {
                BundleSound::InsertFail
            });
            true
        }
        (false, None) => {
            bundle.edit_bundle(|contents| {
                let Some(removed) = contents.remove_one() else {
                    return;
                };
                let amount = removed.count;
                match view.safe_insert(index, removed, amount) {
                    Some(mut rest) => {
                        contents.try_insert(&mut rest);
                    }
                    None => view.inventory.note_bundle_sound(BundleSound::RemoveOne),
                }
            });
            true
        }
        _ => false,
    };
    if handled {
        view.inventory.set_carried(Some(bundle));
    }
    handled
}

/// A stack on the cursor clicking a bundle in a slot: the cursor goes in
/// (left) or one stack comes out onto the cursor (right). Other clicks
/// only clear the bundle's selection and carry on as usual.
fn stack_on_bundle(view: &mut View<'_>, index: usize, left: bool) -> bool {
    let Some(mut bundle) = view.get(index).filter(|s| s.is_bundle()).cloned() else {
        return false;
    };
    let carried = view.inventory.carried().cloned();
    match (left, carried) {
        (false, None) => {
            if view.allow_modification(index) {
                let removed = bundle.edit_bundle(|contents| contents.remove_one());
                if let Some(removed) = removed {
                    view.inventory.note_bundle_sound(BundleSound::RemoveOne);
                    view.inventory.set_carried(Some(removed));
                }
            }
        }
        (true, Some(mut carried)) => {
            let inserted = view.allow_modification(index)
                && bundle.edit_bundle(|c| c.try_insert(&mut carried)) > 0;
            view.inventory.note_bundle_sound(if inserted {
                BundleSound::Insert
            } else {
                BundleSound::InsertFail
            });
            view.inventory
                .set_carried((carried.count > 0).then_some(carried));
        }
        _ => {
            if let Some(contents) = &mut bundle.components.bundle_contents {
                contents.toggle_selected(-1);
            }
            *view.get_mut(index) = Some(bundle);
            return false;
        }
    }
    *view.get_mut(index) = Some(bundle);
    true
}

fn pickup(view: &mut View<'_>, index: usize, left: bool) {
    if bundle_on_slot(view, index, left) || stack_on_bundle(view, index, left) {
        return;
    }
    let clicked = view.get(index).cloned();
    let carried = view.inventory.carried().cloned();
    match (clicked, carried) {
        (None, Some(carried)) => {
            let amount = if left { carried.count } else { 1 };
            let rest = view.safe_insert(index, carried, amount);
            view.inventory.set_carried(rest);
        }
        (None, None) => {}
        (Some(_), _) if !view.may_pickup(index) => {}
        (Some(clicked), None) => {
            let amount = if left {
                clicked.count
            } else {
                clicked.count.div_ceil(2)
            };
            if let Some(taken) = view.try_remove(index, amount, u8::MAX) {
                view.inventory.set_carried(Some(taken));
                view.taken(index);
            }
        }
        (Some(clicked), Some(mut carried)) => {
            if view.may_place(index, &carried) {
                if clicked.stacks_with(&carried) {
                    let amount = if left { carried.count } else { 1 };
                    let rest = view.safe_insert(index, carried, amount);
                    view.inventory.set_carried(rest);
                } else if carried.count <= view.max_stack(index, &carried) {
                    *view.get_mut(index) = Some(carried);
                    view.inventory.set_carried(Some(clicked));
                }
            } else if clicked.stacks_with(&carried) {
                let room = carried.max_stack_size().saturating_sub(carried.count);
                if let Some(taken) = view.try_remove(index, clicked.count, room) {
                    carried.count += taken.count;
                    view.inventory.set_carried(Some(carried));
                    view.taken(index);
                }
            }
        }
    }
}

fn swap_with_hotbar(view: &mut View<'_>, index: usize, button: i8) {
    let source_slot = match button {
        0..=8 => PLAYER_HOTBAR_START + button as usize,
        40 => PLAYER_OFFHAND,
        _ => return,
    };
    if view.kind.inventory_slot(index) == Some(source_slot) {
        return;
    }
    let source = view.inventory.slots()[source_slot].clone();
    let target = view.get(index).cloned();
    match (source, target) {
        (None, None) => {}
        (None, Some(target)) => {
            if view.may_pickup(index) {
                *view.inventory.slot_mut(source_slot) = Some(target);
                *view.get_mut(index) = None;
                view.taken(index);
            }
        }
        (Some(source), None) => {
            if view.may_place(index, &source) {
                let limit = view.max_stack(index, &source);
                if source.count > limit {
                    let mut rest = source.clone();
                    rest.count -= limit;
                    *view.inventory.slot_mut(source_slot) = Some(rest);
                    *view.get_mut(index) = Some(source.with_count(limit));
                } else {
                    *view.inventory.slot_mut(source_slot) = None;
                    *view.get_mut(index) = Some(source);
                }
            }
        }
        (Some(source), Some(target)) => {
            if view.may_pickup(index) && view.may_place(index, &source) {
                let limit = view.max_stack(index, &source);
                if source.count > limit {
                    let mut rest = source.clone();
                    rest.count -= limit;
                    *view.inventory.slot_mut(source_slot) = Some(rest);
                    *view.get_mut(index) = Some(source.with_count(limit));
                    if let Some(left) = view.inventory.add(target) {
                        view.drops.push(left);
                    }
                } else {
                    *view.inventory.slot_mut(source_slot) = Some(target);
                    *view.get_mut(index) = Some(source);
                }
            }
        }
    }
}

fn pickup_all(view: &mut View<'_>, index: usize, button: i8) {
    if view.kind == MenuKind::Merchant {
        return;
    }
    let Some(mut carried) = view.inventory.carried().cloned() else {
        return;
    };
    if view.get(index).is_some() && view.may_pickup(index) {
        return;
    }
    let limit = carried.max_stack_size();
    let order: Vec<usize> = if button == 0 {
        (0..view.len()).collect()
    } else {
        (0..view.len()).rev().collect()
    };
    for pass in 0..2 {
        for &slot in &order {
            if carried.count >= limit {
                break;
            }
            let Some(stack) = view.get(slot) else {
                continue;
            };
            if view.is_result(slot)
                || !view.quick_replaceable(slot, &carried)
                || !view.may_pickup(slot)
            {
                continue;
            }
            if pass == 0 && stack.count == stack.max_stack_size() {
                continue;
            }
            let count = stack.count;
            if let Some(taken) = view.try_remove(slot, count, limit - carried.count) {
                carried.count += taken.count;
            }
        }
    }
    view.inventory.set_carried(Some(carried));
}

fn drag_step(
    view: &mut View<'_>,
    drag: &mut Option<Drag>,
    index: Option<usize>,
    button: i8,
    creative: bool,
) {
    let header = button & 3;
    let kind = ((button >> 2) & 3) as u8;
    let Some(carried) = view.inventory.carried().cloned() else {
        *drag = None;
        return;
    };
    match (header, drag.as_mut()) {
        (0, None) => {
            if kind < 2 || (kind == 2 && creative) {
                *drag = Some(Drag {
                    kind,
                    slots: Vec::new(),
                });
            }
        }
        (1, Some(current)) => {
            let Some(slot) = index else {
                return;
            };
            if view.quick_replaceable(slot, &carried)
                && view.may_place(slot, &carried)
                && (current.kind == 2 || carried.count as usize > current.slots.len())
                && !current.slots.contains(&slot)
            {
                current.slots.push(slot);
            }
        }
        (2, Some(_)) => {
            let current = drag.take().unwrap();
            finish_drag(view, current, carried);
        }
        _ => *drag = None,
    }
}

fn finish_drag(view: &mut View<'_>, drag: Drag, carried: ItemStack) {
    if drag.slots.len() == 1 {
        pickup(view, drag.slots[0], drag.kind != 1);
        return;
    }
    if drag.slots.is_empty() {
        return;
    }
    let mut remaining = carried.count;
    let slots = drag.slots.len() as u8;
    for slot in drag.slots {
        if !(view.quick_replaceable(slot, &carried)
            && view.may_place(slot, &carried)
            && (drag.kind == 2 || carried.count >= slots))
        {
            continue;
        }
        let existing = view.get(slot).map_or(0, |s| s.count);
        let limit = view.max_stack(slot, &carried);
        let per_slot = match drag.kind {
            0 => carried.count / slots,
            1 => 1,
            _ => limit,
        };
        let new_count = per_slot.saturating_add(existing).min(limit);
        remaining = remaining.saturating_sub(new_count - existing);
        *view.get_mut(slot) = Some(carried.clone().with_count(new_count));
    }
    view.inventory
        .set_carried((remaining > 0).then(|| carried.with_count(remaining)));
}

/// Whether `stack` carries enchantments a grindstone could remove.
pub fn holds_enchantments(stack: &ItemStack) -> bool {
    [
        &stack.components.enchantments,
        &stack.components.stored_enchantments,
    ]
    .into_iter()
    .any(|list| list.as_ref().is_some_and(|e| !e.entries().is_empty()))
}

#[cfg(test)]
mod tests;

fn has_binding_curse(stack: &ItemStack) -> bool {
    let registry = tuff_item::enchantment::current();
    tuff_item::enchantment::leveled(&registry, stack.components.enchantments.as_ref())
        .any(|(_, def, _)| def.effects.prevent_armor_change)
}
