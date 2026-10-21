//! Menu clicks against the vanilla recipes: crafting grids, the
//! stonecutter, smithing, furnaces and brewing stands.

use tuff_vanilla_data::Item;

use crate::crafting::registered::vanilla_set;
use tuff_core::world::inventory::Inventory;
use tuff_core::world::menu::*;
use tuff_item::item::{ItemStack, MintReason};

fn dirt(count: u8) -> Option<ItemStack> {
    Some(ItemStack::mint(Item::Dirt, count, MintReason::Test))
}

fn craft(
    container: &mut [Option<ItemStack>],
    inventory: &mut Inventory,
    slot: i16,
    button: i8,
    mode: i32,
) -> (Vec<ItemStack>, bool) {
    let mut hook = GridHook::new(MenuKind::Crafting, vanilla_set().clone());
    let drops = click(
        MenuKind::Crafting,
        container,
        inventory,
        &mut None,
        Click { slot, button, mode },
        tuff_core::world::menu::ClickEnv {
            creative: false,
            hook: &mut hook,
            recipes: vanilla_set(),
        },
    );
    (drops, hook.took)
}

fn refresh(container: &mut [Option<ItemStack>]) {
    container[0] = vanilla_set().grid_result(&container[1..10], 3, 3);
}

fn planks(count: u8) -> Option<ItemStack> {
    Some(ItemStack::mint(Item::OakPlanks, count, MintReason::Test))
}

fn count_of(inventory: &Inventory, item: Item) -> u32 {
    inventory
        .slots()
        .iter()
        .flatten()
        .filter(|s| s.item == item)
        .map(|s| s.count() as u32)
        .sum()
}

#[tuff_core::watchdog]
#[test]
fn shift_clicking_the_result_crafts_until_ingredients_run_out() {
    let mut container = (0..10).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    let mut inventory = Inventory::default();
    container[5] = Some(ItemStack::mint(Item::OakLog, 20, MintReason::Test));
    refresh(&mut container);
    assert_eq!(container[0], planks(4));
    let (drops, took) = craft(&mut container, &mut inventory, 0, 0, mode::QUICK_MOVE);
    assert!(drops.is_empty() && took);
    assert_eq!(container[5], None);
    assert_eq!(container[0], None);
    assert_eq!(count_of(&inventory, Item::OakPlanks), 80);
    assert_eq!(inventory.slots()[44], planks(64));
    assert_eq!(inventory.slots()[43], planks(16));
}

#[tuff_core::watchdog]
#[test]
fn shift_clicking_the_result_stops_when_the_inventory_is_full() {
    let mut container = (0..10).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    let mut inventory = Inventory::default();
    for slot in 9..45 {
        inventory.set_slot(
            slot,
            Some(ItemStack::mint(Item::Stone, 64, MintReason::Test)),
        );
    }
    inventory.set_slot(20, planks(62));
    container[1] = Some(ItemStack::mint(Item::OakLog, 20, MintReason::Test));
    refresh(&mut container);
    let (drops, took) = craft(&mut container, &mut inventory, 0, 0, mode::QUICK_MOVE);
    assert!(took);
    assert_eq!(inventory.slots()[20], planks(64));
    assert_eq!(
        drops,
        vec![ItemStack::mint(Item::OakPlanks, 2, MintReason::Test)]
    );
    assert_eq!(
        container[1],
        Some(ItemStack::mint(Item::OakLog, 19, MintReason::Test))
    );
    assert_eq!(container[0], planks(4));
    let (drops, took) = craft(&mut container, &mut inventory, 0, 0, mode::QUICK_MOVE);
    assert!(drops.is_empty() && !took);
    assert_eq!(
        container[1],
        Some(ItemStack::mint(Item::OakLog, 19, MintReason::Test))
    );
}

#[tuff_core::watchdog]
#[test]
fn taking_the_result_uses_one_set_and_crafts_again() {
    let mut container = (0..10).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    let mut inventory = Inventory::default();
    container[1] = Some(ItemStack::mint(Item::OakLog, 3, MintReason::Test));
    refresh(&mut container);
    craft(&mut container, &mut inventory, 0, 0, mode::PICKUP);
    assert_eq!(inventory.carried(), planks(4).as_ref());
    assert_eq!(
        container[1],
        Some(ItemStack::mint(Item::OakLog, 2, MintReason::Test))
    );
    assert_eq!(container[0], planks(4));
    craft(&mut container, &mut inventory, 0, 1, mode::PICKUP);
    assert_eq!(inventory.carried(), planks(8).as_ref());
    assert_eq!(
        container[1],
        Some(ItemStack::mint(Item::OakLog, 1, MintReason::Test))
    );
    inventory.set_carried(Some(ItemStack::mint(Item::OakPlanks, 62, MintReason::Test)));
    craft(&mut container, &mut inventory, 0, 0, mode::PICKUP);
    assert_eq!(inventory.carried(), planks(62).as_ref());
    assert_eq!(
        container[1],
        Some(ItemStack::mint(Item::OakLog, 1, MintReason::Test))
    );
    inventory.set_carried(None);
    let (drops, took) = craft(&mut container, &mut inventory, 0, 1, mode::THROW);
    assert!(took);
    assert_eq!(
        drops,
        vec![ItemStack::mint(Item::OakPlanks, 4, MintReason::Test)]
    );
    assert_eq!(container[1], None);
    assert_eq!(container[0], None);
}

#[tuff_core::watchdog]
#[test]
fn the_result_slot_refuses_items_and_double_clicks_skip_it() {
    let mut container = (0..10).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    let mut inventory = Inventory::default();
    inventory.set_carried(dirt(3));
    craft(&mut container, &mut inventory, 0, 0, mode::PICKUP);
    assert_eq!(container[0], None);
    assert_eq!(inventory.carried(), dirt(3).as_ref());
    container[1] = Some(ItemStack::mint(Item::OakLog, 1, MintReason::Test));
    refresh(&mut container);
    craft(&mut container, &mut inventory, 0, 0, mode::PICKUP);
    assert_eq!(container[0], planks(4));
    assert_eq!(inventory.carried(), dirt(3).as_ref());
    inventory.set_carried(planks(1));
    inventory.set_slot(40, planks(3));
    craft(&mut container, &mut inventory, 20, 0, mode::PICKUP_ALL);
    assert_eq!(inventory.carried(), planks(4).as_ref());
    assert_eq!(container[0], planks(4));
    assert_eq!(
        container[1],
        Some(ItemStack::mint(Item::OakLog, 1, MintReason::Test))
    );
}

#[tuff_core::watchdog]
#[test]
fn stonecutter_cuts_one_input_per_result() {
    let slab = vanilla_set()
        .stonecutter_options(Item::Cobblestone)
        .into_iter()
        .find(|option| {
            vanilla_set().stonecutter_recipe(*option).unwrap().0
                == "minecraft:cobblestone_slab_from_cobblestone_stonecutting"
        })
        .unwrap();
    let mut container = vec![
        Some(ItemStack::mint(Item::Cobblestone, 2, MintReason::Test)),
        Some(ItemStack::mint(Item::CobblestoneSlab, 2, MintReason::Test)),
    ];
    let mut inventory = Inventory::default();
    let take = |container: &mut Vec<Option<ItemStack>>, inventory: &mut Inventory, mode: i32| {
        let mut hook = StonecutterHook::new(Some(slab), vanilla_set().clone());
        click(
            MenuKind::Stonecutter,
            container,
            inventory,
            &mut None,
            Click {
                slot: stonecutter::RESULT as i16,
                button: 0,
                mode,
            },
            tuff_core::world::menu::ClickEnv {
                creative: false,
                hook: &mut hook,
                recipes: vanilla_set(),
            },
        );
        hook.took
    };
    assert!(take(&mut container, &mut inventory, mode::PICKUP));
    assert_eq!(
        inventory.carried(),
        Some(&ItemStack::mint(Item::CobblestoneSlab, 2, MintReason::Test))
    );
    assert_eq!(
        container[stonecutter::INPUT],
        Some(ItemStack::mint(Item::Cobblestone, 1, MintReason::Test))
    );
    assert_eq!(
        container[stonecutter::RESULT],
        Some(ItemStack::mint(Item::CobblestoneSlab, 2, MintReason::Test))
    );
    inventory.set_carried(None);
    container[stonecutter::INPUT] = Some(ItemStack::mint(Item::Cobblestone, 3, MintReason::Test));
    assert!(take(&mut container, &mut inventory, mode::QUICK_MOVE));
    assert_eq!(container, vec![None, None]);
    assert_eq!(count_of(&inventory, Item::CobblestoneSlab), 6);
    assert!(!take(&mut container, &mut inventory, mode::QUICK_MOVE));

    inventory.set_slot(
        36,
        Some(ItemStack::mint(Item::Cobblestone, 5, MintReason::Test)),
    );
    inventory.set_slot(37, dirt(1));
    let mut hook = StonecutterHook::new(None, vanilla_set().clone());
    click(
        MenuKind::Stonecutter,
        &mut container,
        &mut inventory,
        &mut None,
        Click {
            slot: 29,
            button: 0,
            mode: mode::QUICK_MOVE,
        },
        tuff_core::world::menu::ClickEnv {
            creative: false,
            hook: &mut hook,
            recipes: vanilla_set(),
        },
    );
    assert_eq!(
        container[stonecutter::INPUT],
        Some(ItemStack::mint(Item::Cobblestone, 5, MintReason::Test))
    );
    click(
        MenuKind::Stonecutter,
        &mut container,
        &mut inventory,
        &mut None,
        Click {
            slot: 30,
            button: 0,
            mode: mode::QUICK_MOVE,
        },
        tuff_core::world::menu::ClickEnv {
            creative: false,
            hook: &mut hook,
            recipes: vanilla_set(),
        },
    );
    assert_eq!(inventory.slots()[9], dirt(1));
}

#[tuff_core::watchdog]
#[test]
fn smithing_uses_one_of_each_input_and_gates_its_slots() {
    let mut container = (0..4).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    let mut inventory = Inventory::default();
    let mut sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    sword.set_damage(5);
    inventory.set_carried(dirt(1));
    click(
        MenuKind::Smithing,
        &mut container,
        &mut inventory,
        &mut None,
        Click {
            slot: smithing::TEMPLATE as i16,
            button: 0,
            mode: mode::PICKUP,
        },
        tuff_core::world::menu::ClickEnv {
            creative: false,
            hook: &mut SmithingHook::new(vanilla_set().clone()),
            recipes: vanilla_set(),
        },
    );
    assert_eq!(container[smithing::TEMPLATE], None);
    inventory.set_carried(None);
    inventory.set_slot(36, Some(sword.test_copy()));
    click(
        MenuKind::Smithing,
        &mut container,
        &mut inventory,
        &mut None,
        Click {
            slot: 31,
            button: 0,
            mode: mode::QUICK_MOVE,
        },
        tuff_core::world::menu::ClickEnv {
            creative: false,
            hook: &mut SmithingHook::new(vanilla_set().clone()),
            recipes: vanilla_set(),
        },
    );
    assert_eq!(container[smithing::BASE], Some(sword));
    container[smithing::TEMPLATE] = Some(ItemStack::mint(
        Item::NetheriteUpgradeSmithingTemplate,
        2,
        MintReason::Test,
    ));
    container[smithing::ADDITION] =
        Some(ItemStack::mint(Item::NetheriteIngot, 1, MintReason::Test));
    container[smithing::RESULT] = smithing_result(vanilla_set(), &container);
    assert_eq!(
        container[smithing::RESULT].as_ref().map(|s| s.item),
        Some(Item::NetheriteSword)
    );
    let mut hook = SmithingHook::new(vanilla_set().clone());
    click(
        MenuKind::Smithing,
        &mut container,
        &mut inventory,
        &mut None,
        Click {
            slot: smithing::RESULT as i16,
            button: 0,
            mode: mode::PICKUP,
        },
        tuff_core::world::menu::ClickEnv {
            creative: false,
            hook: &mut hook,
            recipes: vanilla_set(),
        },
    );
    assert!(hook.took);
    assert_eq!(
        inventory.carried().map(|s| (s.item, s.damage())),
        Some((Item::NetheriteSword, 5))
    );
    assert_eq!(
        container[smithing::TEMPLATE],
        Some(ItemStack::mint(
            Item::NetheriteUpgradeSmithingTemplate,
            1,
            MintReason::Test
        ))
    );
    assert_eq!(container[smithing::BASE], None);
    assert_eq!(container[smithing::ADDITION], None);
    assert_eq!(container[smithing::RESULT], None);
}

fn furnace_click(
    container: &mut [Option<ItemStack>],
    inventory: &mut Inventory,
    slot: i16,
    mode: i32,
) -> bool {
    let mut hook = FurnaceHook::default();
    click(
        MenuKind::Furnace,
        container,
        inventory,
        &mut None,
        Click {
            slot,
            button: 0,
            mode,
        },
        tuff_core::world::menu::ClickEnv {
            creative: false,
            hook: &mut hook,
            recipes: vanilla_set(),
        },
    );
    hook.took
}

fn brewing_click(container: &mut [Option<ItemStack>], inventory: &mut Inventory, slot: i16) {
    click(
        MenuKind::BrewingStand,
        container,
        inventory,
        &mut None,
        Click {
            slot,
            button: 0,
            mode: mode::QUICK_MOVE,
        },
        tuff_core::world::menu::ClickEnv {
            creative: false,
            hook: &mut NoResult,
            recipes: vanilla_set(),
        },
    );
}

#[tuff_core::watchdog]
#[test]
fn brewing_stands_take_one_bottle_per_slot_and_fuel_before_reagents() {
    let mut container = (0..5).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    let mut inventory = Inventory::default();
    inventory.set_slot(
        36,
        Some(ItemStack::mint(Item::GlassBottle, 5, MintReason::Test)),
    );
    inventory.set_slot(
        37,
        Some(ItemStack::mint(Item::BlazePowder, 70, MintReason::Test)),
    );
    inventory.set_slot(
        38,
        Some(ItemStack::mint(Item::NetherWart, 3, MintReason::Test)),
    );
    inventory.set_slot(39, dirt(2));
    brewing_click(&mut container, &mut inventory, 32);
    for slot in &container[..3] {
        assert_eq!(
            *slot,
            Some(ItemStack::mint(Item::GlassBottle, 1, MintReason::Test))
        );
    }
    assert_eq!(
        inventory.slots()[36],
        Some(ItemStack::mint(Item::GlassBottle, 2, MintReason::Test))
    );
    brewing_click(&mut container, &mut inventory, 34);
    assert_eq!(
        container[brewing::INGREDIENT],
        Some(ItemStack::mint(Item::NetherWart, 3, MintReason::Test))
    );
    container[brewing::INGREDIENT] = None;
    inventory.set_slot(
        37,
        Some(ItemStack::mint(Item::BlazePowder, 64, MintReason::Test)),
    );
    brewing_click(&mut container, &mut inventory, 33);
    assert_eq!(
        container[brewing::FUEL],
        Some(ItemStack::mint(Item::BlazePowder, 64, MintReason::Test))
    );
    inventory.set_slot(
        37,
        Some(ItemStack::mint(Item::BlazePowder, 5, MintReason::Test)),
    );
    brewing_click(&mut container, &mut inventory, 33);
    assert_eq!(
        container[brewing::INGREDIENT],
        Some(ItemStack::mint(Item::BlazePowder, 5, MintReason::Test)),
        "blaze powder goes to the reagent slot once the fuel slot is full"
    );
    brewing_click(&mut container, &mut inventory, 35);
    assert_eq!(inventory.slots()[9], dirt(2));
    assert!(!MenuKind::BrewingStand.may_place(
        vanilla_set(),
        0,
        &ItemStack::mint(Item::Stone, 1, MintReason::Test)
    ));
}

#[tuff_core::watchdog]
#[test]
fn furnace_menus_sort_shift_clicks_into_input_and_fuel_slots() {
    let mut container = (0..3).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    let mut inventory = Inventory::default();
    inventory.set_slot(
        36,
        Some(ItemStack::mint(Item::IronOre, 5, MintReason::Test)),
    );
    inventory.set_slot(37, Some(ItemStack::mint(Item::Coal, 3, MintReason::Test)));
    inventory.set_slot(38, dirt(2));
    inventory.set_slot(39, Some(ItemStack::mint(Item::Bucket, 4, MintReason::Test)));
    assert!(!furnace_click(
        &mut container,
        &mut inventory,
        30,
        mode::QUICK_MOVE
    ));
    assert_eq!(
        container[furnace::INPUT],
        Some(ItemStack::mint(Item::IronOre, 5, MintReason::Test))
    );
    furnace_click(&mut container, &mut inventory, 31, mode::QUICK_MOVE);
    assert_eq!(
        container[furnace::FUEL],
        Some(ItemStack::mint(Item::Coal, 3, MintReason::Test))
    );
    furnace_click(&mut container, &mut inventory, 32, mode::QUICK_MOVE);
    assert_eq!(
        container[furnace::INPUT],
        Some(ItemStack::mint(Item::IronOre, 5, MintReason::Test))
    );
    assert_eq!(inventory.slots()[9], dirt(2), "dirt moves to the main grid");
    furnace_click(&mut container, &mut inventory, 33, mode::QUICK_MOVE);
    assert_eq!(
        inventory.slots()[10],
        Some(ItemStack::mint(Item::Bucket, 4, MintReason::Test)),
        "empty buckets are not shift-clicked into the fuel slot"
    );
    container[furnace::FUEL] = None;
    inventory.set_carried(Some(ItemStack::mint(Item::Bucket, 4, MintReason::Test)));
    furnace_click(
        &mut container,
        &mut inventory,
        furnace::FUEL as i16,
        mode::PICKUP,
    );
    assert_eq!(
        container[furnace::FUEL],
        Some(ItemStack::mint(Item::Bucket, 1, MintReason::Test))
    );
    assert_eq!(
        inventory.carried(),
        Some(&ItemStack::mint(Item::Bucket, 3, MintReason::Test))
    );
    inventory.set_carried(dirt(1));
    furnace_click(
        &mut container,
        &mut inventory,
        furnace::RESULT as i16,
        mode::PICKUP,
    );
    assert_eq!(container[furnace::RESULT], None);
    assert_eq!(inventory.carried(), dirt(1).as_ref());
    inventory.set_carried(None);
    container[furnace::RESULT] = Some(ItemStack::mint(Item::IronIngot, 7, MintReason::Test));
    assert!(furnace_click(
        &mut container,
        &mut inventory,
        furnace::RESULT as i16,
        mode::QUICK_MOVE
    ));
    assert_eq!(container[furnace::RESULT], None);
    assert_eq!(count_of(&inventory, Item::IronIngot), 7);
    let blast = MenuKind::BlastFurnace;
    assert!(!blast.can_smelt(
        vanilla_set(),
        &ItemStack::mint(Item::Beef, 1, MintReason::Test)
    ));
    assert!(blast.can_smelt(
        vanilla_set(),
        &ItemStack::mint(Item::IronOre, 1, MintReason::Test)
    ));
    assert!(MenuKind::Smoker.can_smelt(
        vanilla_set(),
        &ItemStack::mint(Item::Beef, 1, MintReason::Test)
    ));
}

#[tuff_core::watchdog]
#[test]
fn the_inventory_grid_crafts_and_closing_returns_it() {
    let mut inventory = Inventory::default();
    inventory.set_slot(1, Some(ItemStack::mint(Item::OakLog, 2, MintReason::Test)));
    inventory.set_slot(0, vanilla_set().grid_result(inventory.craft_grid(), 2, 2));
    assert_eq!(inventory.slots()[0], planks(4));
    let mut hook = GridHook::new(MenuKind::Player, vanilla_set().clone());
    inventory.click(0, 0, mode::QUICK_MOVE, false, &mut hook, vanilla_set());
    assert!(hook.took);
    assert_eq!(inventory.slots()[1], None);
    assert_eq!(inventory.slots()[0], None);
    assert_eq!(count_of(&inventory, Item::OakPlanks), 8);
    inventory.set_slot(3, dirt(2));
    inventory.set_slot(0, dirt(1));
    assert!(inventory.close().is_empty());
    assert_eq!(inventory.slots()[3], None);
    assert_eq!(inventory.slots()[0], None);
    assert_eq!(count_of(&inventory, Item::Dirt), 2);
}
