//! The example feature loads beside the vanilla set, and its recipes work
//! beside vanilla's and on an inert core.

#![expect(
    clippy::disallowed_methods,
    reason = "test code is exempt from the door gate"
)]

use tuff_core::command::ParseContext;
use tuff_core::feature::{Feature, load_all};
use tuff_core::registry::Registries;
use tuff_core::world::crafting::RecipeSet;
use tuff_feature_builtin_example::{
    DIAMOND_FROM_DIRT, DIAMOND_REPAIR, ExampleFeature, GOLD_DIGGER, greetings,
};
use tuff_item::item::{ItemStack, MintReason};
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::Item;

fn with_vanilla() -> Registries {
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(ExampleFeature));
    let mut registries = Registries::new();
    load_all(&features, &mut registries).expect("the example composes with vanilla");
    registries
}

#[tuff_core::watchdog]
#[test]
fn loads_beside_vanilla_with_its_command_greeting_and_hooks() {
    let mut registries = with_vanilla();
    assert!(
        registries
            .commands
            .roots()
            .children()
            .iter()
            .any(|node| node.name == "gold")
    );
    assert_eq!(
        greetings(&mut registries).get("example:hello").map(|g| g.0),
        Some("Welcome, gold digger")
    );
    assert!(!registries.region_hooks.is_empty());
    assert!(!registries.services.is_empty());
}

#[tuff_core::watchdog]
#[test]
fn commands_name_the_feature_entity_type_and_advancement() {
    let registries = with_vanilla();
    let roots = registries.commands.roots();
    let context = ParseContext {
        allow_selectors: true,
    };
    let parses = |line: &str| {
        let parsed = roots.parse(line, PermissionLevel::Owners, context);
        parsed.errors.is_empty() && !parsed.dangling && parsed.end == line.len()
    };
    assert!(parses("summon example:tick_counter"));
    assert!(parses("summon minecraft:pig"));
    assert!(!parses("summon example:nothing"));
    assert!(parses("kill @e[type=example:tick_counter]"));
    assert!(parses("kill @e[type=!example:tick_counter]"));
    assert!(!parses("kill @e[type=example:nothing]"));
    assert!(parses("advancement grant @s only example:gold_digger"));
    assert!(
        parses("advancement grant @s only example:nothing"),
        "advancement ids are looked up when the command runs, as data packs reload"
    );
    assert!(parses("recipe give @s example:diamond_from_dirt"));
    assert!(parses("recipe give @s minecraft:torch"));
    assert!(!parses("setblock ~ ~ ~ example:nothing"));
    assert!(
        parses("recipe give @s example:nothing"),
        "recipe keys are looked up when the command runs, as data packs reload"
    );

    let tree = registries
        .advancements
        .get()
        .expect("the tree is built")
        .as_any()
        .downcast_ref::<tuff_core::world::advancement::Tree>()
        .expect("the built-in advancements");
    let id = tree.id(GOLD_DIGGER.name).expect("the feature advancement");
    let parent = tree.get(id).parent.expect("it has a parent");
    assert_eq!(tree.get(parent).name, "minecraft:story/root");
    assert!(tree.get(parent).children.contains(&id));
    let story = tree.id("minecraft:story/mine_stone").unwrap();
    assert_eq!(
        tree.get(story).parent,
        Some(parent),
        "vanilla links survive"
    );
    let unlock = tree
        .id("example:recipes/diamond_from_dirt")
        .expect("the recipe's unlock advancement");
    assert_eq!(&*tree.get(unlock).rewards.recipes, &[DIAMOND_FROM_DIRT]);
    assert_eq!(
        tree.len(),
        tuff_vanilla_data::advancement::ALL.len() + 2,
        "gold digger plus the recipe unlock"
    );
}

fn one_cell(item: Item) -> Vec<Option<ItemStack>> {
    vec![Some(ItemStack::mint(item, 1, MintReason::Test))]
}

fn damaged_pickaxe_and_diamond() -> Vec<Option<ItemStack>> {
    let mut pickaxe = ItemStack::mint(Item::IronPickaxe, 1, MintReason::Test);
    pickaxe.set_damage(100);
    vec![
        Some(pickaxe),
        Some(ItemStack::mint(Item::Diamond, 1, MintReason::Test)),
    ]
}

#[tuff_core::watchdog]
#[test]
fn recipes_are_added_beside_vanilla() {
    let registries = with_vanilla();
    let recipes = RecipeSet::from_registry(&registries.recipe_entries);
    assert_eq!(
        recipes.grid_result(&one_cell(Item::Dirt), 1, 1),
        Some(ItemStack::mint(Item::Diamond, 1, MintReason::Test))
    );
    assert_eq!(
        recipes.grid_result(&one_cell(Item::OakLog), 1, 1),
        Some(ItemStack::mint(Item::OakPlanks, 4, MintReason::Test)),
        "vanilla recipes still craft"
    );
    let entry = &recipes.displays_for(DIAMOND_FROM_DIRT)[0];
    assert_eq!(
        recipes.display(entry.id).map(|e| &*e.recipe),
        Some(DIAMOND_FROM_DIRT)
    );

    let grid = damaged_pickaxe_and_diamond();
    assert_eq!(recipes.grid_recipe(&grid, 2, 1), Some(DIAMOND_REPAIR));
    let repaired = recipes.grid_result(&grid, 2, 1).unwrap();
    assert_eq!((repaired.item(), repaired.damage()), (Item::IronPickaxe, 0));
    assert!(
        recipes.displays_for(DIAMOND_REPAIR).is_empty(),
        "code recipes stay out of the book"
    );
}

#[tuff_core::watchdog]
#[test]
fn crafting_uses_up_the_ingredients() {
    let recipes = RecipeSet::from_registry(&with_vanilla().recipe_entries);
    let mut grid = vec![Some(ItemStack::mint(Item::Dirt, 2, MintReason::Test))];
    recipes.take_result(&mut grid, 1, 1);
    assert_eq!(
        grid,
        vec![Some(ItemStack::mint(Item::Dirt, 1, MintReason::Test))]
    );
    recipes.take_result(&mut grid, 1, 1);
    assert_eq!(grid, vec![None]);
    assert_eq!(recipes.grid_result(&grid, 1, 1), None);

    let mut grid = damaged_pickaxe_and_diamond();
    recipes.take_result(&mut grid, 2, 1);
    assert_eq!(grid, vec![None, None]);
}

#[tuff_core::watchdog]
#[test]
fn recipes_work_on_an_inert_core() {
    let features: Vec<Box<dyn Feature>> = vec![Box::new(ExampleFeature)];
    let mut registries = Registries::new();
    load_all(&features, &mut registries).unwrap();
    let recipes = RecipeSet::from_registry(&registries.recipe_entries);
    assert_eq!(
        recipes.names().collect::<Vec<_>>(),
        vec![DIAMOND_FROM_DIRT, DIAMOND_REPAIR,]
    );
    assert!(recipes.grid_result(&one_cell(Item::Dirt), 1, 1).is_some());
    assert!(recipes.grid_result(&one_cell(Item::OakLog), 1, 1).is_none());
}

#[tuff_core::watchdog]
#[test]
fn custom_item_resolves_beside_vanilla() {
    let registries = with_vanilla();
    let content = registries.custom.content();
    let charm = content
        .item(tuff_feature_builtin_example::GOLD_NUGGET_ITEM)
        .unwrap();
    let stack = charm.stack(1);
    assert_eq!(stack.item(), Item::GoldNugget);
    assert_eq!(
        content.item_of(&stack).map(|d| d.id.as_str()),
        Some(tuff_feature_builtin_example::GOLD_NUGGET_ITEM)
    );
    assert!(
        content
            .assets()
            .contains_key("assets/example/items/gold_nugget_charm.json")
    );
    assert!(
        !content
            .assets()
            .keys()
            .any(|path| path.contains("/blockstates/")),
        "no block repaints"
    );
}
