use super::*;
use crate::world::crafting::RecipeSet;
use tuff_vanilla_data::Item;

fn dirt(count: u8) -> Option<ItemStack> {
    Some(ItemStack::mint(Item::Dirt, count, MintReason::Test))
}

struct Chest {
    container: Vec<Option<ItemStack>>,
    inventory: Inventory,
    drag: Option<Drag>,
}

impl Chest {
    fn new() -> Self {
        Self {
            container: (0..27).map(|_| None).collect::<Vec<Option<ItemStack>>>(),
            inventory: Inventory::default(),
            drag: None,
        }
    }

    fn click(&mut self, slot: i16, button: i8, mode: i32) -> Vec<ItemStack> {
        click(
            MenuKind::Generic { rows: 3 },
            &mut self.container,
            &mut self.inventory,
            &mut self.drag,
            Click { slot, button, mode },
            ClickEnv {
                creative: false,
                hook: &mut NoResult,
                recipes: &RecipeSet::default(),
            },
        )
    }

    fn inventory_slot(&self, slot: usize) -> Option<&ItemStack> {
        self.inventory.slots()[slot].as_ref()
    }
}

#[tuff_core::watchdog]
#[test]
fn window_slots_map_onto_the_inventory_and_back() {
    let kind = MenuKind::Generic { rows: 3 };
    assert_eq!(kind.len(), 63);
    assert_eq!(kind.inventory_slot(26), None);
    assert_eq!(kind.inventory_slot(27), Some(9));
    assert_eq!(kind.inventory_slot(53), Some(35));
    assert_eq!(kind.inventory_slot(54), Some(36));
    assert_eq!(kind.inventory_slot(62), Some(44));
    assert_eq!(kind.inventory_slot(63), None);
    for window in 27..63 {
        let inventory = kind.inventory_slot(window).unwrap();
        assert_eq!(kind.window_slot(inventory), Some(window));
    }
    assert_eq!(kind.window_slot(45), None);
    assert_eq!(MenuKind::Hopper.len(), 41);
    assert_eq!(MenuKind::Player.inventory_slot(45), Some(45));
}

#[tuff_core::watchdog]
#[test]
fn quick_move_fills_the_hotbar_from_the_end_then_the_main_grid() {
    let mut chest = Chest::new();
    chest.container[3] = dirt(40);
    chest.inventory.set_slot(44, dirt(60));
    chest.inventory.set_slot(20, dirt(64));
    chest.click(3, 0, mode::QUICK_MOVE);
    assert_eq!(chest.container[3], None);
    assert_eq!(chest.inventory_slot(44), dirt(64).as_ref());
    assert_eq!(chest.inventory_slot(43), dirt(36).as_ref());
    assert_eq!(chest.inventory_slot(20), dirt(64).as_ref());

    chest.inventory.set_slot(9, dirt(5));
    chest.container[0] = dirt(62);
    chest.click(27, 0, mode::QUICK_MOVE);
    assert_eq!(chest.inventory_slot(9), None);
    assert_eq!(chest.container[0], dirt(64));
    assert_eq!(chest.container[1], dirt(3));
}

#[tuff_core::watchdog]
#[test]
fn quick_move_into_a_mount_fills_armor_then_saddle_then_chest() {
    let kind = MenuKind::Mount {
        columns: 3,
        saddle: true,
        armor: MountArmor::Horse,
    };
    let mut container = (0..kind.container_size())
        .map(|_| None)
        .collect::<Vec<Option<ItemStack>>>();
    let mut inventory = Inventory::default();
    let mut drag = None;
    for item in [
        Item::IronHorseArmor,
        Item::Saddle,
        Item::Saddle,
        Item::RedCarpet,
    ] {
        inventory.set_slot(36, Some(ItemStack::mint(item, 1, MintReason::Test)));
        let slot = kind.window_slot(36).unwrap() as i16;
        click(
            kind,
            &mut container,
            &mut inventory,
            &mut drag,
            Click {
                slot,
                button: 0,
                mode: mode::QUICK_MOVE,
            },
            ClickEnv {
                creative: false,
                hook: &mut NoResult,
                recipes: &RecipeSet::default(),
            },
        );
    }
    let item = |slot: usize| container[slot].as_ref().map(|stack| stack.item());
    assert_eq!(container.len(), 2 + 9);
    assert_eq!(item(mount::BODY), Some(Item::IronHorseArmor));
    assert_eq!(item(mount::SADDLE), Some(Item::Saddle));
    assert_eq!(item(mount::CHEST), Some(Item::Saddle));
    assert_eq!(item(mount::CHEST + 1), Some(Item::RedCarpet));
    let carpet = ItemStack::mint(Item::RedCarpet, 1, MintReason::Test);
    assert!(!kind.may_place(&RecipeSet::default(), mount::BODY, &carpet));
    let saddle = ItemStack::mint(Item::Saddle, 1, MintReason::Test);
    assert_eq!(kind.max_stack(mount::SADDLE, &saddle), 1);
    assert_eq!(kind.menu_type(), None, "mounts open with their own packet");
}

#[tuff_core::watchdog]
#[test]
fn quick_move_from_a_full_container_leaves_the_stack() {
    let mut chest = Chest::new();
    for slot in &mut chest.container {
        *slot = Some(ItemStack::mint(Item::Stone, 64, MintReason::Test));
    }
    chest.inventory.set_slot(36, dirt(7));
    chest.click(54, 0, mode::QUICK_MOVE);
    assert_eq!(chest.inventory_slot(36), dirt(7).as_ref());
}

#[tuff_core::watchdog]
#[test]
fn drag_splits_the_cursor_across_both_sections() {
    let mut chest = Chest::new();
    chest.inventory.set_carried(dirt(10));
    chest.click(OUTSIDE, 0, mode::DRAG);
    for slot in [0i16, 1, 27, 62] {
        chest.click(slot, 1, mode::DRAG);
    }
    chest.click(OUTSIDE, 2, mode::DRAG);
    assert_eq!(chest.container[0], dirt(2));
    assert_eq!(chest.container[1], dirt(2));
    assert_eq!(chest.inventory_slot(9), dirt(2).as_ref());
    assert_eq!(chest.inventory_slot(44), dirt(2).as_ref());
    assert_eq!(chest.inventory.carried(), dirt(2).as_ref());
}

#[tuff_core::watchdog]
#[test]
fn right_drag_places_one_per_slot_up_to_the_cursor_count() {
    let mut chest = Chest::new();
    chest.inventory.set_carried(dirt(2));
    chest.click(OUTSIDE, 4, mode::DRAG);
    for slot in [0i16, 1, 2] {
        chest.click(slot, 5, mode::DRAG);
    }
    chest.click(OUTSIDE, 6, mode::DRAG);
    assert_eq!(chest.container[0], dirt(1));
    assert_eq!(chest.container[1], dirt(1));
    assert_eq!(chest.container[2], None);
    assert_eq!(chest.inventory.carried(), None);
}

#[tuff_core::watchdog]
#[test]
fn double_click_collects_partial_stacks_across_both_sections_first() {
    let mut chest = Chest::new();
    chest.inventory.set_carried(dirt(1));
    chest.container[0] = dirt(64);
    chest.container[5] = dirt(10);
    chest.inventory.set_slot(40, dirt(20));
    chest
        .inventory
        .set_slot(12, Some(ItemStack::mint(Item::Stone, 3, MintReason::Test)));
    chest.click(7, 0, mode::PICKUP_ALL);
    assert_eq!(chest.inventory.carried(), dirt(64).as_ref());
    assert_eq!(chest.container[5], None);
    assert_eq!(chest.inventory_slot(40), None);
    assert_eq!(chest.container[0], dirt(31));
    assert_eq!(
        chest.inventory_slot(12),
        Some(&ItemStack::mint(Item::Stone, 3, MintReason::Test))
    );
}

#[tuff_core::watchdog]
#[test]
fn pickup_swaps_takes_half_and_merges() {
    let mut chest = Chest::new();
    chest.container[4] = dirt(9);
    chest.click(4, 1, mode::PICKUP);
    assert_eq!(chest.inventory.carried(), dirt(5).as_ref());
    assert_eq!(chest.container[4], dirt(4));
    chest.click(4, 0, mode::PICKUP);
    assert_eq!(chest.container[4], dirt(9));
    assert_eq!(chest.inventory.carried(), None);
    chest
        .inventory
        .set_carried(Some(ItemStack::mint(Item::Stone, 2, MintReason::Test)));
    chest.click(4, 0, mode::PICKUP);
    assert_eq!(
        chest.container[4],
        Some(ItemStack::mint(Item::Stone, 2, MintReason::Test))
    );
    assert_eq!(chest.inventory.carried(), dirt(9).as_ref());
    assert_eq!(
        chest.click(OUTSIDE, 1, mode::PICKUP),
        vec![ItemStack::mint(Item::Dirt, 1, MintReason::Test)]
    );
    assert_eq!(chest.inventory.carried(), dirt(8).as_ref());
}

#[tuff_core::watchdog]
#[test]
fn number_keys_swap_with_the_hotbar_and_throw_drops_from_the_container() {
    let mut chest = Chest::new();
    chest.container[2] = dirt(3);
    chest
        .inventory
        .set_slot(37, Some(ItemStack::mint(Item::Stone, 1, MintReason::Test)));
    chest.click(2, 1, mode::SWAP);
    assert_eq!(
        chest.container[2],
        Some(ItemStack::mint(Item::Stone, 1, MintReason::Test))
    );
    assert_eq!(chest.inventory_slot(37), dirt(3).as_ref());
    assert_eq!(
        chest.click(55, 1, mode::THROW),
        vec![ItemStack::mint(Item::Dirt, 3, MintReason::Test)]
    );
    assert_eq!(chest.inventory_slot(37), None);
    assert_eq!(
        chest.click(2, 0, mode::THROW),
        vec![ItemStack::mint(Item::Stone, 1, MintReason::Test)]
    );
}

#[tuff_core::watchdog]
#[test]
fn shulker_boxes_refuse_shulker_boxes_and_clone_needs_creative() {
    let mut container = (0..27).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    let mut inventory = Inventory::default();
    let mut drag = None;
    inventory.set_carried(Some(ItemStack::mint(Item::ShulkerBox, 1, MintReason::Test)));
    click(
        MenuKind::ShulkerBox,
        &mut container,
        &mut inventory,
        &mut drag,
        Click {
            slot: 0,
            button: 0,
            mode: mode::PICKUP,
        },
        ClickEnv {
            creative: false,
            hook: &mut NoResult,
            recipes: &RecipeSet::default(),
        },
    );
    assert_eq!(container[0], None);
    click(
        MenuKind::ShulkerBox,
        &mut container,
        &mut inventory,
        &mut drag,
        Click {
            slot: 27,
            button: 0,
            mode: mode::PICKUP,
        },
        ClickEnv {
            creative: false,
            hook: &mut NoResult,
            recipes: &RecipeSet::default(),
        },
    );
    assert_eq!(
        inventory.slots()[9],
        Some(ItemStack::mint(Item::ShulkerBox, 1, MintReason::Test))
    );
    click(
        MenuKind::ShulkerBox,
        &mut container,
        &mut inventory,
        &mut drag,
        Click {
            slot: 27,
            button: 0,
            mode: mode::CLONE,
        },
        ClickEnv {
            creative: false,
            hook: &mut NoResult,
            recipes: &RecipeSet::default(),
        },
    );
    assert_eq!(inventory.carried(), None);
    inventory.set_slot(10, dirt(2));
    click(
        MenuKind::ShulkerBox,
        &mut container,
        &mut inventory,
        &mut drag,
        Click {
            slot: 28,
            button: 0,
            mode: mode::CLONE,
        },
        ClickEnv {
            creative: true,
            hook: &mut NoResult,
            recipes: &RecipeSet::default(),
        },
    );
    assert_eq!(inventory.carried(), dirt(64).as_ref());
}

fn bundle_with(stacks: Vec<ItemStack>) -> ItemStack {
    let mut bundle = ItemStack::mint(Item::Bundle, 1, MintReason::Test);
    for mut stack in stacks {
        bundle.edit_bundle(|contents| contents.try_insert(&mut stack));
    }
    bundle
}

fn held(bundle: &ItemStack) -> Vec<(Item, u8)> {
    bundle
        .components
        .bundle_contents
        .as_ref()
        .map(|c| c.items().iter().map(|s| (s.item(), s.count())).collect())
        .unwrap_or_default()
}

#[tuff_core::watchdog]
#[test]
fn a_carried_bundle_takes_the_clicked_stack_up_to_its_room() {
    let mut chest = Chest::new();
    chest
        .inventory
        .set_carried(Some(bundle_with(vec![ItemStack::mint(
            Item::Stone,
            40,
            MintReason::Test,
        )])));
    chest.container[0] = dirt(64);
    chest.click(0, 0, mode::PICKUP);
    let bundle = chest.inventory.carried().unwrap();
    assert_eq!(held(bundle), vec![(Item::Dirt, 24), (Item::Stone, 40)]);
    assert_eq!(chest.container[0], dirt(40));
    assert_eq!(
        chest.inventory.take_bundle_sounds(),
        vec![BundleSound::Insert]
    );
}

#[tuff_core::watchdog]
#[test]
fn a_full_bundle_refuses_with_the_fail_sound_and_changes_nothing() {
    let mut chest = Chest::new();
    chest
        .inventory
        .set_carried(Some(bundle_with(vec![ItemStack::mint(
            Item::Stone,
            64,
            MintReason::Test,
        )])));
    chest.container[0] = dirt(5);
    chest.click(0, 0, mode::PICKUP);
    assert_eq!(chest.container[0], dirt(5));
    assert_eq!(
        chest.inventory.take_bundle_sounds(),
        vec![BundleSound::InsertFail]
    );
}

#[tuff_core::watchdog]
#[test]
fn a_carried_bundle_puts_one_stack_into_an_empty_slot_on_right_click() {
    let mut chest = Chest::new();
    chest.inventory.set_carried(Some(bundle_with(vec![
        ItemStack::mint(Item::Stone, 3, MintReason::Test),
        ItemStack::mint(Item::Dirt, 5, MintReason::Test),
    ])));
    chest.click(4, 1, mode::PICKUP);
    assert_eq!(chest.container[4], dirt(5));
    assert_eq!(
        held(chest.inventory.carried().unwrap()),
        vec![(Item::Stone, 3)]
    );
    assert_eq!(
        chest.inventory.take_bundle_sounds(),
        vec![BundleSound::RemoveOne]
    );
    chest.click(5, 1, mode::PICKUP);
    let emptied = chest.inventory.carried().unwrap();
    assert_eq!(emptied.components.bundle_contents, None);
}

#[tuff_core::watchdog]
#[test]
fn clicking_a_bundle_in_a_slot_with_the_cursor_fills_or_empties_it() {
    let mut chest = Chest::new();
    chest.container[0] = Some(bundle_with(Vec::new()));
    chest.inventory.set_carried(dirt(10));
    chest.click(0, 0, mode::PICKUP);
    assert_eq!(chest.inventory.carried(), None);
    assert_eq!(
        held(chest.container[0].as_ref().unwrap()),
        vec![(Item::Dirt, 10)]
    );
    chest.click(0, 1, mode::PICKUP);
    assert_eq!(chest.inventory.carried(), dirt(10).as_ref());
    assert_eq!(
        chest.container[0]
            .as_ref()
            .unwrap()
            .components
            .bundle_contents,
        None
    );
}

#[tuff_core::watchdog]
#[test]
fn a_plain_click_on_a_bundle_still_picks_it_up_and_clears_its_selection() {
    let mut chest = Chest::new();
    let mut bundle = bundle_with(vec![
        ItemStack::mint(Item::Dirt, 2, MintReason::Test),
        ItemStack::mint(Item::Stone, 2, MintReason::Test),
    ]);
    bundle
        .components
        .bundle_contents
        .as_mut()
        .unwrap()
        .toggle_selected(1);
    chest.container[0] = Some(bundle);
    chest.click(0, 0, mode::PICKUP);
    let carried = chest.inventory.carried().unwrap();
    assert_eq!(
        carried
            .components
            .bundle_contents
            .as_ref()
            .unwrap()
            .selected(),
        -1
    );
    assert_eq!(chest.container[0], None);
}

#[tuff_core::watchdog]
#[test]
fn shift_clicking_does_not_treat_a_bundle_as_a_container() {
    let mut chest = Chest::new();
    chest.container[0] = Some(bundle_with(Vec::new()));
    chest.inventory.set_carried(dirt(10));
    chest.click(0, 0, mode::QUICK_MOVE);
    assert_eq!(chest.inventory.take_bundle_sounds(), Vec::new());
}

#[tuff_core::watchdog]
#[test]
fn crafter_slots_map_to_the_client_layout() {
    let kind = MenuKind::Crafter;
    assert_eq!(kind.wire_slot(crafter::PREVIEW), 45);
    assert_eq!(kind.wire_slot(10), 9);
    assert_eq!(kind.wire_slot(46 - 1), 44);
    assert_eq!(kind.wire_slot(3), 3);
    for slot in 0..46 {
        assert_eq!(kind.internal_slot(kind.wire_slot(slot)), slot);
    }
    assert_eq!(MenuKind::Anvil.wire_slot(5), 5);
}

#[tuff_core::watchdog]
#[test]
fn armor_slots_hold_one_item() {
    let stack = ItemStack::mint(Item::Dirt, 64, MintReason::Test);
    assert_eq!(MenuKind::Player.max_stack(5, &stack), 1);
    assert_eq!(MenuKind::Player.max_stack(9, &stack), 64);
}

#[tuff_core::watchdog]
#[test]
fn binding_curse_blocks_taking_armor_unless_creative() {
    let kind = MenuKind::Player;
    let mut cursed = ItemStack::mint(Item::IronHelmet, 1, MintReason::Test);
    cursed.components.enchantments = Some(tuff_item::item::Enchantments::new([(
        "minecraft:binding_curse".to_string(),
        1,
    )]));
    for (creative, expect_taken) in [(false, false), (true, true)] {
        let mut inventory = Inventory::default();
        inventory.set_slot(5, Some(cursed.test_copy()));
        let mut drag = None;
        click(
            kind,
            &mut [],
            &mut inventory,
            &mut drag,
            Click {
                slot: 5,
                button: 0,
                mode: mode::PICKUP,
            },
            ClickEnv {
                creative,
                hook: &mut NoResult,
                recipes: &RecipeSet::default(),
            },
        );
        assert_eq!(inventory.carried().is_some(), expect_taken);
    }
}
