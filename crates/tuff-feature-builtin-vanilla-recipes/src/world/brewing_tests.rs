//! The brewing stand against the data pack's recipes.

use tuff_vanilla_data::registry::Potion;
use tuff_vanilla_data::{Block, BlockState, Item};

use crate::crafting::registered::vanilla_set;
use tuff_core::world::block_entity::brewing;
use tuff_core::world::crafting::brewing::{BrewTick, Brewing};
use tuff_core::world::crafting::result_stack;
use tuff_item::item::{ComponentPatch, ItemStack, PotionContents};
use tuff_vanilla_data::recipe::BrewingRecipe;

fn tick(block: BlockState, items: &mut [Option<ItemStack>], stand: &mut Brewing) -> BrewTick {
    super::brewing::tick(vanilla_set(), block, items, stand)
}

fn recipe_for(input: &ItemStack, reagent: &ItemStack) -> Option<&'static BrewingRecipe> {
    vanilla_set().brewing_recipe(input, reagent)
}

fn is_reagent(stack: &ItemStack) -> bool {
    vanilla_set().is_brewing_reagent(stack)
}

fn is_potion_input(stack: &ItemStack) -> bool {
    vanilla_set().is_potion_input(stack)
}

fn is_fuel(stack: &ItemStack) -> bool {
    vanilla_set().is_brewing_fuel(stack)
}

fn potion(item: Item, potion: Potion) -> Option<ItemStack> {
    Some(ItemStack::new(item, 1).with_components(ComponentPatch {
        potion_contents: Some(Box::new(PotionContents {
            potion: Some(potion),
            ..Default::default()
        })),
        ..Default::default()
    }))
}

fn run(items: &mut [Option<ItemStack>], stand: &mut Brewing, ticks: usize) -> usize {
    let mut brews = 0;
    for _ in 0..ticks {
        brews += tick(Block::BrewingStand.default_state(), items, stand).brewed as usize;
    }
    brews
}

#[tuff_core::watchdog]
#[test]
fn nether_wart_brews_awkward_potions_in_400_ticks_with_one_fuel_use() {
    let mut items = vec![
        potion(Item::Potion, Potion::Water),
        None,
        potion(Item::Potion, Potion::Water),
        Some(ItemStack::new(Item::NetherWart, 2)),
        Some(ItemStack::new(Item::BlazePowder, 1)),
    ];
    let mut stand = Brewing::default();
    let first = tick(Block::BrewingStand.default_state(), &mut items, &mut stand);
    assert_eq!(first.bottles, Some([true, false, true]));
    assert_eq!(items[brewing::FUEL], None);
    assert_eq!(stand.fuel, 19);
    assert_eq!(stand.total_fuel, 20);
    assert_eq!(stand.brew_time, 400);
    assert_eq!(run(&mut items, &mut stand, 399), 0);
    assert_eq!(run(&mut items, &mut stand, 1), 1);
    assert_eq!(items[0], potion(Item::Potion, Potion::Awkward));
    assert_eq!(items[2], potion(Item::Potion, Potion::Awkward));
    assert_eq!(items[1], None);
    assert_eq!(
        items[brewing::INGREDIENT],
        Some(ItemStack::new(Item::NetherWart, 1))
    );
    assert_eq!(stand.brew_time, 0);
    run(&mut items, &mut stand, 5);
    assert_eq!(stand.brew_time, 0, "awkward potions take no nether wart");
    assert_eq!(stand.fuel, 19);
}

#[tuff_core::watchdog]
#[test]
fn a_changed_reagent_stops_the_brew_and_conversions_follow_the_recipes() {
    let mut items = vec![
        potion(Item::Potion, Potion::Awkward),
        None,
        None,
        Some(ItemStack::new(Item::MagmaCream, 1)),
        Some(ItemStack::new(Item::BlazePowder, 1)),
    ];
    let mut stand = Brewing::default();
    run(&mut items, &mut stand, 10);
    items[brewing::INGREDIENT] = Some(ItemStack::new(Item::Sugar, 1));
    run(&mut items, &mut stand, 1);
    assert_eq!(stand.brew_time, 0);
    items[brewing::INGREDIENT] = Some(ItemStack::new(Item::MagmaCream, 1));
    assert_eq!(run(&mut items, &mut stand, 401), 1);
    assert_eq!(items[0], potion(Item::Potion, Potion::FireResistance));
    assert_eq!(items[brewing::INGREDIENT], None);

    let fire = potion(Item::Potion, Potion::FireResistance).unwrap();
    let gunpowder = ItemStack::new(Item::Gunpowder, 1);
    let splash = recipe_for(&fire, &gunpowder).unwrap();
    assert_eq!(
        Some(result_stack(&splash.output)),
        potion(Item::SplashPotion, Potion::FireResistance)
    );
    let breath = ItemStack::new(Item::DragonBreath, 1);
    let splash = potion(Item::SplashPotion, Potion::FireResistance).unwrap();
    assert_eq!(
        Some(result_stack(&recipe_for(&splash, &breath).unwrap().output)),
        potion(Item::LingeringPotion, Potion::FireResistance)
    );
    assert!(recipe_for(&fire, &breath).is_none());
    assert!(is_reagent(&ItemStack::new(Item::NetherWart, 1)));
    assert!(!is_reagent(&ItemStack::new(Item::Stick, 1)));
    assert!(is_potion_input(&ItemStack::new(Item::GlassBottle, 1)));
    assert!(is_fuel(&ItemStack::new(Item::BlazePowder, 1)));
}

#[tuff_core::watchdog]
#[test]
fn a_dragon_breath_reagent_leaves_its_bottle() {
    let mut items = vec![
        potion(Item::SplashPotion, Potion::Swiftness),
        None,
        None,
        Some(ItemStack::new(Item::DragonBreath, 2)),
        Some(ItemStack::new(Item::BlazePowder, 1)),
    ];
    let mut stand = Brewing::default();
    let mut drops = Vec::new();
    for _ in 0..401 {
        drops.extend(tick(Block::BrewingStand.default_state(), &mut items, &mut stand).drops);
    }
    assert_eq!(items[0], potion(Item::LingeringPotion, Potion::Swiftness));
    assert_eq!(drops, vec![ItemStack::new(Item::GlassBottle, 1)]);
    assert_eq!(
        items[brewing::INGREDIENT],
        Some(ItemStack::new(Item::DragonBreath, 1))
    );
}
