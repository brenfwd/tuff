//! The furnace family and campfires against the data pack's recipes.

use tuff_vanilla_data::{Block, BlockState, Item};

use super::smelting as ticking;
use crate::crafting::registered::vanilla_set;
use tuff_core::world::block_entity::{CAMPFIRE_SLOTS, Campfire, ContainerKind, Furnace, furnace};
use tuff_core::world::crafting::smelting::{self, FurnaceTick};
use tuff_item::item::ItemStack;

fn tick_furnace(
    kind: ContainerKind,
    block: BlockState,
    items: &mut [Option<ItemStack>],
    furnace: &mut Furnace,
) -> FurnaceTick {
    ticking::tick_furnace(vanilla_set(), kind, block, items, furnace)
}

fn input_changed(kind: ContainerKind, items: &[Option<ItemStack>], furnace: &mut Furnace) {
    smelting::input_changed(vanilla_set(), kind, items, furnace);
}

fn tick_campfire(
    items: &mut [Option<ItemStack>],
    campfire: &mut Campfire,
    lit: bool,
) -> (bool, Vec<ItemStack>) {
    ticking::tick_campfire(vanilla_set(), items, campfire, lit)
}

fn place_food(
    items: &mut [Option<ItemStack>],
    campfire: &mut Campfire,
    stack: &ItemStack,
) -> Option<usize> {
    smelting::place_food(vanilla_set(), items, campfire, stack)
}

fn can_cook(kind: ContainerKind, item: Item) -> bool {
    vanilla_set().can_cook(kind, item)
}

fn stack(item: Item, count: u8) -> Option<ItemStack> {
    Some(ItemStack::new(item, count))
}

fn run(
    kind: ContainerKind,
    block: Block,
    items: &mut [Option<ItemStack>],
    furnace: &mut Furnace,
    ticks: usize,
) -> Vec<(usize, bool)> {
    let mut flips = Vec::new();
    for tick in 0..ticks {
        let result = tick_furnace(kind, block.default_state(), items, furnace);
        if let Some(lit) = result.lit {
            flips.push((tick, lit));
        }
    }
    flips
}

#[tuff_core::watchdog]
#[test]
fn a_furnace_smelts_one_ore_every_200_ticks_and_a_blast_furnace_every_100() {
    let mut items = vec![stack(Item::IronOre, 3), stack(Item::Coal, 1), None];
    let mut furnace = Furnace::default();
    input_changed(ContainerKind::Furnace, &items, &mut furnace);
    assert_eq!(furnace.cooking_total_time, 200);
    let flips = run(
        ContainerKind::Furnace,
        Block::Furnace,
        &mut items,
        &mut furnace,
        199,
    );
    assert_eq!(flips, vec![(0, true)]);
    assert_eq!(items[furnace::FUEL], None);
    assert_eq!(items[furnace::RESULT], None);
    assert_eq!(furnace.lit_total_time, 1600);
    assert_eq!(
        furnace.lit_time_remaining,
        1600 - 198,
        "the lighting tick does not burn"
    );
    run(
        ContainerKind::Furnace,
        Block::Furnace,
        &mut items,
        &mut furnace,
        1,
    );
    assert_eq!(items[furnace::RESULT], stack(Item::IronIngot, 1));
    assert_eq!(items[furnace::INPUT], stack(Item::IronOre, 2));
    assert_eq!(
        furnace
            .recipes_used
            .get("minecraft:iron_ingot_from_smelting_iron_ore"),
        Some(&1)
    );
    run(
        ContainerKind::Furnace,
        Block::Furnace,
        &mut items,
        &mut furnace,
        400,
    );
    assert_eq!(items[furnace::RESULT], stack(Item::IronIngot, 3));
    assert_eq!(items[furnace::INPUT], None);
    assert_eq!(furnace.cooking_timer, 0);

    let mut items = vec![stack(Item::IronOre, 2), stack(Item::Coal, 1), None];
    let mut furnace = Furnace::default();
    input_changed(ContainerKind::BlastFurnace, &items, &mut furnace);
    assert_eq!(
        furnace.cooking_total_time, 200,
        "the first fuel sets the speed"
    );
    run(
        ContainerKind::BlastFurnace,
        Block::BlastFurnace,
        &mut items,
        &mut furnace,
        100,
    );
    assert_eq!(furnace.speed_multiplier, 2.0);
    assert_eq!(furnace.lit_total_time, 800);
    assert_eq!(items[furnace::RESULT], stack(Item::IronIngot, 1));
    run(
        ContainerKind::BlastFurnace,
        Block::BlastFurnace,
        &mut items,
        &mut furnace,
        100,
    );
    assert_eq!(items[furnace::RESULT], stack(Item::IronIngot, 2));
    assert!(
        !can_cook(ContainerKind::BlastFurnace, Item::Beef),
        "blast furnaces do not cook food"
    );
    assert!(can_cook(ContainerKind::Smoker, Item::Beef));
    assert!(!can_cook(ContainerKind::Smoker, Item::IronOre));
}

#[tuff_core::watchdog]
#[test]
fn the_fire_goes_out_when_fuel_runs_out_and_progress_cools() {
    let mut items = vec![stack(Item::IronOre, 1), stack(Item::Stick, 1), None];
    let mut furnace = Furnace::default();
    input_changed(ContainerKind::Furnace, &items, &mut furnace);
    let flips = run(
        ContainerKind::Furnace,
        Block::Furnace,
        &mut items,
        &mut furnace,
        120,
    );
    assert_eq!(flips, vec![(0, true), (100, false)]);
    assert_eq!(items[furnace::RESULT], None);
    assert_eq!(furnace.cooking_timer, 100 - 2 * 20);
    run(
        ContainerKind::Furnace,
        Block::Furnace,
        &mut items,
        &mut furnace,
        100,
    );
    assert_eq!(furnace.cooking_timer, 0);
    items[furnace::FUEL] = stack(Item::Coal, 1);
    run(
        ContainerKind::Furnace,
        Block::Furnace,
        &mut items,
        &mut furnace,
        200,
    );
    assert_eq!(items[furnace::RESULT], stack(Item::IronIngot, 1));
    assert!(furnace.is_lit());
    let flips = run(
        ContainerKind::Furnace,
        Block::Furnace,
        &mut items,
        &mut furnace,
        1401,
    );
    assert_eq!(flips, vec![(1400, false)], "coal burns 1600 ticks");
}

#[tuff_core::watchdog]
#[test]
fn a_lava_bucket_leaves_its_bucket_and_full_outputs_block_cooking() {
    let mut items = vec![
        stack(Item::Cobblestone, 1),
        stack(Item::LavaBucket, 1),
        None,
    ];
    let mut furnace = Furnace::default();
    input_changed(ContainerKind::Furnace, &items, &mut furnace);
    let tick = tick_furnace(
        ContainerKind::Furnace,
        Block::Furnace.default_state(),
        &mut items,
        &mut furnace,
    );
    assert_eq!(tick.lit, Some(true));
    assert!(tick.drops.is_empty());
    assert_eq!(items[furnace::FUEL], stack(Item::Bucket, 1));
    assert_eq!(furnace.lit_total_time, 20000);

    let mut items = vec![
        stack(Item::Cobblestone, 1),
        stack(Item::Coal, 1),
        stack(Item::Stone, 64),
    ];
    let mut furnace = Furnace::default();
    input_changed(ContainerKind::Furnace, &items, &mut furnace);
    let tick = tick_furnace(
        ContainerKind::Furnace,
        Block::Furnace.default_state(),
        &mut items,
        &mut furnace,
    );
    assert_eq!(tick.lit, None);
    assert_eq!(items[furnace::FUEL], stack(Item::Coal, 1));
    items[furnace::RESULT] = stack(Item::IronIngot, 1);
    let tick = tick_furnace(
        ContainerKind::Furnace,
        Block::Furnace.default_state(),
        &mut items,
        &mut furnace,
    );
    assert_eq!(tick.lit, None, "a different output blocks cooking");
}

#[tuff_core::watchdog]
#[test]
fn relighting_keeps_the_progress_ratio_across_speeds() {
    let mut items = vec![stack(Item::IronOre, 1), None, None];
    let mut furnace = Furnace {
        cooking_timer: 100,
        cooking_total_time: 200,
        ..Furnace::default()
    };
    items[furnace::FUEL] = stack(Item::Coal, 1);
    tick_furnace(
        ContainerKind::BlastFurnace,
        Block::BlastFurnace.default_state(),
        &mut items,
        &mut furnace,
    );
    assert_eq!(furnace.cooking_total_time, 100);
    assert_eq!(furnace.cooking_timer, 51);
}

#[tuff_core::watchdog]
#[test]
fn campfires_cook_for_600_ticks_and_cool_when_unlit() {
    let mut items = vec![None; CAMPFIRE_SLOTS];
    let mut campfire = Campfire::default();
    assert_eq!(
        place_food(&mut items, &mut campfire, &ItemStack::new(Item::Stone, 1)),
        None
    );
    let beef = ItemStack::new(Item::Beef, 5);
    assert_eq!(place_food(&mut items, &mut campfire, &beef), Some(0));
    assert_eq!(place_food(&mut items, &mut campfire, &beef), Some(1));
    assert_eq!(items[0], stack(Item::Beef, 1));
    assert_eq!(campfire.cooking_time, [600, 600, 0, 0]);
    for _ in 0..599 {
        let (changed, popped) = tick_campfire(&mut items, &mut campfire, true);
        assert!(changed && popped.is_empty());
    }
    let (_, popped) = tick_campfire(&mut items, &mut campfire, true);
    assert_eq!(
        popped,
        vec![
            ItemStack::new(Item::CookedBeef, 1),
            ItemStack::new(Item::CookedBeef, 1)
        ]
    );
    assert!(items.iter().all(Option::is_none));
    place_food(&mut items, &mut campfire, &beef);
    for _ in 0..10 {
        tick_campfire(&mut items, &mut campfire, true);
    }
    assert_eq!(campfire.cooking_progress[0], 10);
    let (changed, popped) = tick_campfire(&mut items, &mut campfire, false);
    assert!(changed && popped.is_empty());
    assert_eq!(campfire.cooking_progress[0], 8);
    for _ in 0..10 {
        tick_campfire(&mut items, &mut campfire, false);
    }
    assert_eq!(campfire.cooking_progress[0], 0);
    assert!(
        campfire.cooking_progress[1] > 0,
        "a popped slot keeps its progress until it cools"
    );
    for _ in 0..300 {
        tick_campfire(&mut items, &mut campfire, false);
    }
    assert!(!tick_campfire(&mut items, &mut campfire, false).0);
    for _ in 0..4 {
        place_food(&mut items, &mut campfire, &beef);
    }
    assert_eq!(place_food(&mut items, &mut campfire, &beef), None);
}
