use std::sync::Arc;

use tuff_vanilla_data::Item;
use tuff_vanilla_data::recipe::{CraftingCategory, Ingredient};
use tuff_vanilla_data::registry::{RecipeBookCategory, TriggerType};

use super::{Cooking, Shaped, Shapeless, SmithingTransform, Stonecutting};
use tuff_core::protocol::recipe::RecipeDisplay;
use tuff_core::world::crafting::*;
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::Frozen;

const DIRT: Ingredient = Ingredient::Item(Item::Dirt);
const STICK: Ingredient = Ingredient::Item(Item::Stick);

const DIAMOND: Shapeless = Shapeless::new("test:diamond", &[DIRT], Item::Diamond);
const HOE: Shaped = Shaped::new(
    "test:hoe",
    &["DD", " S", " S"],
    &[('D', DIRT), ('S', STICK)],
    Item::DiamondHoe,
)
.category(CraftingCategory::Equipment);

fn stack(item: Item, count: u8) -> Option<ItemStack> {
    Some(ItemStack::mint(item, count, MintReason::Test))
}

fn set(recipes: Vec<(&'static str, Arc<dyn Recipe>)>) -> RecipeSet {
    RecipeSet::new(
        recipes
            .into_iter()
            .map(|(name, recipe)| (Frozen::Static(name), recipe)),
    )
}

/// Any enchanted book plus a stick makes a stick, keeping the book.
struct KeepsBook;

impl Recipe for KeepsBook {
    fn matches(&self, input: &CraftingInput) -> bool {
        input.ingredient_count() == 2
            && input.stacks().any(|s| s.item() == Item::Book)
            && input.stacks().any(|s| s.item() == Item::Stick)
    }

    fn assemble(&self, _input: &CraftingInput) -> Option<ItemStack> {
        Some(ItemStack::mint(Item::Stick, 2, MintReason::Test))
    }

    fn remainders(&self, input: &CraftingInput) -> Vec<Option<ItemStack>> {
        input
            .items
            .iter()
            .map(|slot| {
                slot.filter(|s| s.item() == Item::Book)
                    .map(|s| ItemStack::mint_like(s, s.count(), MintReason::Test))
            })
            .collect()
    }
}

/// Two sticks anywhere make a torch; leaves `remainders` to the default.
struct Torch;

impl Recipe for Torch {
    fn matches(&self, input: &CraftingInput) -> bool {
        input.ingredient_count() == 2 && input.stacks().all(|s| s.item() == Item::Stick)
    }

    fn assemble(&self, _input: &CraftingInput) -> Option<ItemStack> {
        Some(ItemStack::mint(Item::Torch, 1, MintReason::Test))
    }
}

#[tuff_core::watchdog]
#[test]
fn shaped_recipes_match_anywhere_and_mirrored() {
    let mut grid = (0..9).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    grid[1] = stack(Item::Dirt, 1);
    grid[2] = stack(Item::Dirt, 1);
    grid[5] = stack(Item::Stick, 1);
    grid[8] = stack(Item::Stick, 1);
    let set = set(vec![("test:hoe", Arc::new(HOE))]);
    assert_eq!(set.grid_recipe(&grid, 3, 3), Some("test:hoe"));
    let mut flipped = (0..9).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    flipped[0] = stack(Item::Dirt, 1);
    flipped[1] = stack(Item::Dirt, 1);
    flipped[3] = stack(Item::Stick, 1);
    flipped[6] = stack(Item::Stick, 1);
    assert_eq!(set.grid_recipe(&flipped, 3, 3), Some("test:hoe"));
    flipped[6] = None;
    flipped[7] = stack(Item::Stick, 1);
    assert!(set.grid_recipe(&flipped, 3, 3).is_none());
    assert_eq!(
        set.grid_result(&grid, 3, 3).map(|s| s.item()),
        Some(Item::DiamondHoe)
    );
}

#[tuff_core::watchdog]
#[test]
fn shapeless_recipes_ignore_position() {
    let set = set(vec![("test:diamond", Arc::new(DIAMOND.count(2)))]);
    let mut grid = (0..4).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    grid[3] = stack(Item::Dirt, 5);
    let result = set.grid_result(&grid, 2, 2).unwrap();
    assert_eq!((result.item(), result.count()), (Item::Diamond, 2));
    grid[0] = stack(Item::Dirt, 1);
    assert!(set.grid_result(&grid, 2, 2).is_none());
}

#[tuff_core::watchdog]
#[test]
fn taking_a_result_uses_one_of_each_ingredient() {
    let set = set(vec![
        ("test:diamond", Arc::new(DIAMOND)),
        ("test:torch", Arc::new(Torch)),
    ]);
    let mut grid = (0..4).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    grid[0] = stack(Item::Dirt, 2);
    assert!(set.take_result(&mut grid, 2, 2).is_empty());
    assert_eq!(grid[0], stack(Item::Dirt, 1));
    set.take_result(&mut grid, 2, 2);
    assert_eq!(grid[0], None);
    assert!(set.grid_result(&grid, 2, 2).is_none());

    let mut grid = vec![stack(Item::Stick, 1), None, None, stack(Item::Stick, 3)];
    set.take_result(&mut grid, 2, 2);
    assert_eq!(grid, vec![None, None, None, stack(Item::Stick, 2)]);
}

#[tuff_core::watchdog]
#[test]
fn code_recipes_choose_what_stays_in_the_grid() {
    let set = set(vec![("test:keeps_book", Arc::new(KeepsBook))]);
    let mut grid = vec![stack(Item::Book, 1), stack(Item::Stick, 1), None, None];
    assert_eq!(
        set.grid_result(&grid, 2, 2),
        Some(ItemStack::mint(Item::Stick, 2, MintReason::Test))
    );
    assert!(set.take_result(&mut grid, 2, 2).is_empty());
    assert_eq!(grid, vec![stack(Item::Book, 1), None, None, None]);
}

#[tuff_core::watchdog]
#[test]
fn remainders_that_cannot_stay_are_returned() {
    let milk = Shapeless::new(
        "test:cake_bit",
        &[Ingredient::Item(Item::MilkBucket)],
        Item::Cake,
    );
    let set = set(vec![("test:cake_bit", Arc::new(milk))]);
    let mut grid = vec![stack(Item::MilkBucket, 1)];
    assert!(set.take_result(&mut grid, 1, 1).is_empty());
    assert_eq!(grid, vec![stack(Item::Bucket, 1)]);
}

#[tuff_core::watchdog]
#[test]
fn the_first_matching_recipe_by_name_wins() {
    let set = set(vec![
        (
            "test:b",
            Arc::new(Shapeless::new("test:b", &[DIRT], Item::Stone)),
        ),
        (
            "test:a",
            Arc::new(Shapeless::new("test:a", &[DIRT], Item::Diamond)),
        ),
    ]);
    let grid = vec![stack(Item::Dirt, 1)];
    assert_eq!(set.grid_recipe(&grid, 1, 1), Some("test:a"));
}

#[tuff_core::watchdog]
#[test]
fn book_entries_are_numbered_in_name_order_with_groups() {
    let set = set(vec![
        ("test:hoe", Arc::new(HOE.group("tools"))),
        ("test:keeps_book", Arc::new(KeepsBook)),
        (
            "test:axe",
            Arc::new(
                Shaped::new("test:axe", &["D"], &[('D', DIRT)], Item::DiamondAxe).group("tools"),
            ),
        ),
        ("test:diamond", Arc::new(DIAMOND)),
    ]);
    let ids: Vec<(&str, u32)> = set.displays().iter().map(|d| (&*d.recipe, d.id)).collect();
    assert_eq!(
        ids,
        vec![("test:axe", 0), ("test:diamond", 1), ("test:hoe", 2)]
    );
    assert!(set.displays_for("test:keeps_book").is_empty());
    assert_eq!(set.display(0).unwrap().group, Some(0));
    assert_eq!(set.display(2).unwrap().group, Some(0));
    assert_eq!(set.display(1).unwrap().group, None);
    let hoe = &set.displays_for("test:hoe")[0];
    assert_eq!(hoe.category, RecipeBookCategory::CraftingEquipment);
    assert_eq!(hoe.requirements, Some(vec![DIRT, DIRT, STICK, STICK]));
    assert_eq!(set.resolve("test:hoe").as_deref(), Some("test:hoe"));
    assert_eq!(set.resolve("test:missing"), None);
}

#[tuff_core::watchdog]
#[test]
fn shaped_placement_names_each_cell() {
    let placement = HOE.placement().unwrap();
    assert_eq!(placement.size, Some((2, 3)));
    assert_eq!(placement.ingredients, vec![DIRT, DIRT, STICK, STICK]);
    assert_eq!(
        placement.layout,
        vec![Some(0), Some(1), None, Some(2), None, Some(3)]
    );
    let shapeless = DIAMOND.placement().unwrap();
    assert_eq!((shapeless.layout, shapeless.size), (vec![Some(0)], None));
}

#[tuff_core::watchdog]
#[test]
fn registering_twice_conflicts_unless_replaced() {
    let mut registry = Registry::default();
    registry.register("a", DIAMOND.name, DIAMOND).unwrap();
    assert!(registry.register("b", DIAMOND.name, DIAMOND).is_err());
    registry
        .replace("b", DIAMOND.name, DIAMOND.count(3))
        .unwrap();
    let set = RecipeSet::from_registry(&registry);
    let grid = vec![stack(Item::Dirt, 1)];
    assert_eq!(set.grid_result(&grid, 1, 1).map(|s| s.count()), Some(3));
    registry.freeze();
    assert!(registry.register("a", "test:late", DIAMOND).is_err());
}

#[tuff_core::watchdog]
#[test]
fn simple_recipes_register_an_unlocking_advancement() {
    let mut registries = tuff_core::registry::Registries::new();
    DIAMOND.register("test", &mut registries).unwrap();
    HOE.register("test", &mut registries).unwrap();
    assert!(registries.recipe_entries.contains("test:diamond"));
    let hoe = registries
        .advancement_definitions
        .get("test:recipes/hoe")
        .expect("the hoe's advancement");
    assert_eq!(hoe.rewards.recipes, &["test:hoe"]);
    assert_eq!(hoe.criteria[0].trigger, TriggerType::RecipeUnlocked.name());
    assert_eq!(
        hoe.criteria[0].conditions,
        Some(r#"{"recipes":"test:hoe"}"#)
    );
    assert_eq!(
        hoe.criteria[1].trigger,
        TriggerType::InventoryChanged.name()
    );
    let conditions: serde_json::Value =
        serde_json::from_str(hoe.criteria[1].conditions.unwrap()).unwrap();
    let mut items: Vec<&str> = conditions["items"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap())
        .collect();
    items.sort();
    assert_eq!(items, vec![Item::Dirt.name(), Item::Stick.name()]);
    assert_eq!(
        unlock_advancement("plain", &[DIRT]).name,
        "minecraft:recipes/plain"
    );
}

const IRON: Cooking = Cooking::new(
    "test:iron",
    CookingStation::Furnace,
    Ingredient::Item(Item::IronOre),
    Item::IronIngot,
)
.experience(0.7);
const QUICK_IRON: Cooking = Cooking::new(
    "test:a_quick_iron",
    CookingStation::Furnace,
    Ingredient::Item(Item::IronOre),
    Item::IronNugget,
)
.time(20);
const SLAB: Stonecutting =
    Stonecutting::new("test:slab", Ingredient::Item(Item::Stone), Item::StoneSlab).count(2);
const STAIRS: Stonecutting = Stonecutting::new(
    "test:stairs",
    Ingredient::Item(Item::Stone),
    Item::StoneStairs,
);
const UPGRADE: SmithingTransform = SmithingTransform::new(
    "test:upgrade",
    Ingredient::Item(Item::NetheriteUpgradeSmithingTemplate),
    Ingredient::Item(Item::IronSword),
    Ingredient::Item(Item::Diamond),
    Item::DiamondSword,
);

fn stations() -> RecipeSet {
    set(vec![
        ("test:iron", Arc::new(IRON)),
        ("test:a_quick_iron", Arc::new(QUICK_IRON)),
        ("test:slab", Arc::new(SLAB)),
        ("test:stairs", Arc::new(STAIRS)),
        ("test:upgrade", Arc::new(UPGRADE)),
    ])
}

#[tuff_core::watchdog]
#[test]
fn cooking_looks_up_the_first_recipe_by_name_for_its_station() {
    use tuff_core::world::block_entity::ContainerKind;
    let set = stations();
    let (name, recipe) = set
        .cooking_recipe(CookingStation::Furnace, Item::IronOre)
        .unwrap();
    assert_eq!((name, recipe.cooking_time), ("test:a_quick_iron", 20));
    assert!(
        set.cooking_recipe(CookingStation::Smoker, Item::IronOre)
            .is_none()
    );
    assert!(set.can_cook(ContainerKind::Furnace, Item::IronOre));
    assert!(!set.can_cook(ContainerKind::Chest, Item::IronOre));
    assert_eq!(set.cooking_experience("test:iron"), Some(0.7));
    assert_eq!(set.cooking_experience("test:slab"), None);
    assert_eq!(set.property_set("furnace_input"), &[Item::IronOre]);
    let furnace = &set.displays_for("test:iron")[0];
    assert_eq!(furnace.category, RecipeBookCategory::FurnaceMisc);
    assert!(matches!(
        furnace.display,
        RecipeDisplay::Furnace { duration: 200, .. }
    ));
}

#[tuff_core::watchdog]
#[test]
fn stonecutter_options_number_its_recipes_in_name_order() {
    let set = stations();
    assert_eq!(set.stonecutter_list().len(), 2);
    assert_eq!(set.stonecutter_options(Item::Stone), vec![0, 1]);
    assert!(set.stonecutter_options(Item::Dirt).is_empty());
    assert!(set.stonecutter_accepts(Item::Stone));
    assert_eq!(
        set.stonecutter_recipe(0),
        Some((
            "test:slab",
            ItemStack::mint(Item::StoneSlab, 2, MintReason::Test)
        ))
    );
    assert_eq!(
        set.stonecutter_recipe(1).map(|(name, _)| name),
        Some("test:stairs")
    );
    assert_eq!(set.stonecutter_recipe(2), None);
}

#[tuff_core::watchdog]
#[test]
fn smithing_transforms_without_template_or_addition_match_empty_slots() {
    let mut transform = UPGRADE;
    transform.recipe.template = None;
    transform.recipe.addition = None;
    let set = set(vec![("test:bare", Arc::new(transform))]);
    let sword = ItemStack::mint(Item::IronSword, 1, MintReason::Test);
    let diamond = ItemStack::mint(Item::Diamond, 1, MintReason::Test);
    let (name, result) = set.smithing(None, Some(&sword), None).unwrap();
    assert_eq!(name, "test:bare");
    assert_eq!(result.unwrap().item(), Item::DiamondSword);
    assert!(set.smithing(Some(&diamond), Some(&sword), None).is_none());
    assert!(set.smithing(None, Some(&sword), Some(&diamond)).is_none());
}

#[tuff_core::watchdog]
#[test]
fn smithing_transforms_keep_the_base_components() {
    let set = stations();
    let template = ItemStack::mint(Item::NetheriteUpgradeSmithingTemplate, 1, MintReason::Test);
    let mut sword = ItemStack::mint(Item::IronSword, 1, MintReason::Test);
    sword.set_damage(7);
    let diamond = ItemStack::mint(Item::Diamond, 1, MintReason::Test);
    let (name, result) = set
        .smithing(Some(&template), Some(&sword), Some(&diamond))
        .unwrap();
    let result = result.unwrap();
    assert_eq!(name, "test:upgrade");
    assert_eq!((result.item(), result.damage()), (Item::DiamondSword, 7));
    assert!(set.smithing(None, Some(&sword), Some(&diamond)).is_none());
    assert!(
        set.smithing(Some(&template), Some(&diamond), Some(&diamond))
            .is_none()
    );
    assert_eq!(
        set.property_set("smithing_template"),
        &[Item::NetheriteUpgradeSmithingTemplate]
    );
}
