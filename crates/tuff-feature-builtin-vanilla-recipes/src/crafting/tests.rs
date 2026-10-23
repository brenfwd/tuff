use super::display::{RecipeDisplay, SlotDisplay, displays_for};
use super::registered::vanilla_set;
use super::*;
use tuff_core::world::crafting::place::{Grid, PostPlace, layout, place_recipe};
use tuff_core::world::inventory::Inventory;
use tuff_item::item::decor::{BannerLayer, Filterable, WrittenBook};
use tuff_nbt::Tag;

fn stack(item: Item, count: u8) -> Option<ItemStack> {
    Some(ItemStack::mint(item, count, MintReason::Test))
}

fn grid3(cells: [Option<ItemStack>; 9]) -> CraftingInput<'static> {
    CraftingInput::positioned(3, 3, Vec::from(cells).leak()).input
}

fn grid2(cells: [Option<ItemStack>; 4]) -> CraftingInput<'static> {
    CraftingInput::positioned(2, 2, Vec::from(cells).leak()).input
}

struct Found {
    name: &'static str,
    result: Option<ItemStack>,
}

fn find_recipe(input: &CraftingInput) -> Option<Found> {
    let (name, recipe) = vanilla_set().find(input)?;
    Some(Found {
        name,
        result: recipe.assemble(input),
    })
}

fn grid_result(grid: &[Option<ItemStack>], width: usize, height: usize) -> Option<ItemStack> {
    vanilla_set().grid_result(grid, width, height)
}

fn take_result(grid: &mut [Option<ItemStack>], width: usize, height: usize) -> Vec<ItemStack> {
    vanilla_set().take_result(grid, width, height)
}

fn recipe(name: &str) -> &'static dyn tuff_core::world::crafting::Recipe {
    vanilla_set().get(name).expect("a data pack recipe")
}

fn found(input: &CraftingInput) -> (&'static str, ItemStack) {
    let found = find_recipe(input).expect("a recipe matches");
    (found.name, found.result.expect("a result"))
}

fn pickaxe(damage: u32) -> ItemStack {
    let mut stack = ItemStack::mint(Item::DiamondPickaxe, 1, MintReason::Test);
    stack.set_damage(damage);
    stack
}

#[tuff_core::watchdog]
#[test]
fn shaped_recipes_match_anywhere_in_the_grid_and_mirrored() {
    let (stick, coal) = (stack(Item::Stick, 1), stack(Item::Coal, 1));
    let top_left = grid3([
        coal.as_ref().map(tuff_item::item::ItemStack::test_copy),
        None,
        None,
        stick.as_ref().map(tuff_item::item::ItemStack::test_copy),
        None,
        None,
        None,
        None,
        None,
    ]);
    let bottom_right = grid3([
        None,
        None,
        None,
        None,
        None,
        coal.as_ref().map(tuff_item::item::ItemStack::test_copy),
        None,
        None,
        stick.as_ref().map(tuff_item::item::ItemStack::test_copy),
    ]);
    assert_eq!(
        found(&top_left),
        (
            "minecraft:torch",
            ItemStack::mint(Item::Torch, 4, MintReason::Test)
        )
    );
    assert_eq!(found(&bottom_right).0, "minecraft:torch");
    let sideways = grid3([coal, stick, None, None, None, None, None, None, None]);
    assert!(find_recipe(&sideways).is_none());

    let (s, x) = (stack(Item::Stick, 1), stack(Item::String, 1));
    let bow = grid3([
        None,
        s.as_ref().map(tuff_item::item::ItemStack::test_copy),
        x.as_ref().map(tuff_item::item::ItemStack::test_copy),
        s.as_ref().map(tuff_item::item::ItemStack::test_copy),
        None,
        x.as_ref().map(tuff_item::item::ItemStack::test_copy),
        None,
        s.as_ref().map(tuff_item::item::ItemStack::test_copy),
        x.as_ref().map(tuff_item::item::ItemStack::test_copy),
    ]);
    let mirrored = grid3([
        x.as_ref().map(tuff_item::item::ItemStack::test_copy),
        s.as_ref().map(tuff_item::item::ItemStack::test_copy),
        None,
        x.as_ref().map(tuff_item::item::ItemStack::test_copy),
        None,
        s.as_ref().map(tuff_item::item::ItemStack::test_copy),
        x,
        s,
        None,
    ]);
    assert_eq!(found(&bow).0, "minecraft:bow");
    assert_eq!(found(&mirrored).0, "minecraft:bow");
    let in_2x2 = grid2([stack(Item::Coal, 1), None, stack(Item::Stick, 1), None]);
    assert_eq!(found(&in_2x2).0, "minecraft:torch");
}

#[tuff_core::watchdog]
#[test]
fn shapeless_recipes_ignore_position_and_accept_tag_members() {
    let planks = grid2([None, None, stack(Item::StrippedOakLog, 3), None]);
    assert_eq!(
        found(&planks),
        (
            "minecraft:oak_planks",
            ItemStack::mint(Item::OakPlanks, 4, MintReason::Test)
        )
    );
    let mossy = grid3([
        None,
        stack(Item::Vine, 1),
        None,
        None,
        None,
        None,
        stack(Item::Cobblestone, 1),
        None,
        None,
    ]);
    assert_eq!(found(&mossy).0, "minecraft:mossy_cobblestone_from_vine");
    let too_many = grid3([
        stack(Item::Vine, 1),
        stack(Item::Vine, 1),
        None,
        None,
        None,
        None,
        stack(Item::Cobblestone, 1),
        None,
        None,
    ]);
    assert!(find_recipe(&too_many).is_none());
    assert!(find_recipe(&CraftingInput::EMPTY).is_none());
}

#[tuff_core::watchdog]
#[test]
fn transmute_keeps_components_and_counts_materials() {
    let mut map = ItemStack::mint(Item::FilledMap, 1, MintReason::Test);
    map.components.map_id = Some(7);
    let cloning = grid3([
        Some(map.test_copy()),
        stack(Item::Map, 1),
        stack(Item::Map, 1),
        None,
        None,
        None,
        None,
        None,
        None,
    ]);
    let (name, result) = found(&cloning);
    assert_eq!(name, "minecraft:map_cloning");
    assert_eq!(result.item(), Item::FilledMap);
    assert_eq!(result.count(), 3);
    assert_eq!(result.components, map.components);

    let mut shulker = ItemStack::mint(Item::ShulkerBox, 1, MintReason::Test);
    shulker.components.container = Some(vec![stack(Item::Apple, 3)]);
    let dyed = grid2([
        Some(shulker.test_copy()),
        stack(Item::BlueDye, 1),
        None,
        None,
    ]);
    let (name, result) = found(&dyed);
    assert_eq!(name, "minecraft:blue_shulker_box");
    assert_eq!(result.item(), Item::BlueShulkerBox);
    assert_eq!(result.components.container, shulker.components.container);
    let same = grid2([
        Some(ItemStack::mint(Item::BlueShulkerBox, 1, MintReason::Test)),
        stack(Item::BlueDye, 1),
        None,
        None,
    ]);
    assert!(find_recipe(&same).is_none());
}

#[tuff_core::watchdog]
#[test]
fn repair_combines_durability_and_keeps_only_curses() {
    let mut first = pickaxe(1000);
    first.components.enchantments = Some(Enchantments::new([
        ("minecraft:efficiency".to_owned(), 5),
        ("minecraft:vanishing_curse".to_owned(), 1),
    ]));
    let mut second = pickaxe(1200);
    second.components.enchantments = Some(Enchantments::new([(
        "minecraft:binding_curse".to_owned(),
        1,
    )]));
    let input = grid2([Some(first), None, None, Some(second)]);
    let (name, result) = found(&input);
    assert_eq!(name, "minecraft:repair_item");
    assert_eq!(result.item(), Item::DiamondPickaxe);
    let remaining = 561 + 361 + 1561 * 5 / 100;
    assert_eq!(result.damage(), 1561 - remaining);
    assert_eq!(result.components.max_damage, None);
    assert_eq!(
        result.components.enchantments.as_ref().unwrap().entries(),
        &[
            ("minecraft:binding_curse".to_owned(), 1),
            ("minecraft:vanishing_curse".to_owned(), 1)
        ]
    );
    let stacked = grid2([
        Some(ItemStack::mint(Item::Stick, 1, MintReason::Test)),
        Some(ItemStack::mint(Item::Stick, 1, MintReason::Test)),
        None,
        None,
    ]);
    assert!(find_recipe(&stacked).is_none_or(|f| f.name != "minecraft:repair_item"));
    let full = grid2([Some(pickaxe(0)), Some(pickaxe(1560)), None, None]);
    assert_eq!(found(&full).1.damage(), 0);
}

#[tuff_core::watchdog]
#[test]
fn dye_recipes_mix_colors_into_the_target() {
    let helmet = ItemStack::mint(Item::LeatherHelmet, 1, MintReason::Test);
    let red = grid2([Some(helmet.test_copy()), stack(Item::RedDye, 1), None, None]);
    let (name, result) = found(&red);
    assert_eq!(name, "minecraft:leather_helmet_dyed");
    assert_eq!(result.components.dyed_color, Some(0xB02E26));
    let again = grid3([
        Some(result),
        None,
        stack(Item::BlueDye, 1),
        None,
        None,
        None,
        None,
        None,
        None,
    ]);
    assert_eq!(
        found(&again).1.components.dyed_color,
        Some(decor::apply_dyes(Some(0xB02E26), &[DyeColor::Blue]))
    );
    let no_dye = grid2([Some(helmet), stack(Item::Stick, 1), None, None]);
    assert!(find_recipe(&no_dye).is_none());
    let horse = grid2([
        Some(ItemStack::mint(
            Item::LeatherHorseArmor,
            1,
            MintReason::Test,
        )),
        stack(Item::LimeDye, 1),
        None,
        stack(Item::RedDye, 1),
    ]);
    assert_eq!(
        found(&horse).1.components.dyed_color,
        Some(decor::apply_dyes(None, &[DyeColor::Lime, DyeColor::Red]))
    );
}

#[tuff_core::watchdog]
#[test]
fn taking_a_result_consumes_inputs_and_leaves_remainders() {
    let mut grid = [
        stack(Item::MilkBucket, 1),
        stack(Item::MilkBucket, 1),
        stack(Item::MilkBucket, 1),
        stack(Item::Sugar, 2),
        stack(Item::Egg, 1),
        stack(Item::Sugar, 1),
        stack(Item::Wheat, 1),
        stack(Item::Wheat, 1),
        stack(Item::Wheat, 1),
    ];
    assert_eq!(grid_result(&grid, 3, 3), stack(Item::Cake, 1));
    let leftovers = take_result(&mut grid, 3, 3);
    assert!(leftovers.is_empty());
    assert_eq!(grid[0], stack(Item::Bucket, 1));
    assert_eq!(grid[2], stack(Item::Bucket, 1));
    assert_eq!(grid[3], stack(Item::Sugar, 1));
    assert_eq!(grid[4], None);
    assert_eq!(grid[8], None);
    assert_eq!(grid_result(&grid, 3, 3), None);
    assert_eq!(
        Item::HoneyBottle.crafting_remainder(),
        Some((Item::GlassBottle, 1))
    );
    assert_eq!(Item::MushroomStew.crafting_remainder(), None);

    let mut offset = [
        None,
        None,
        None,
        None,
        stack(Item::Coal, 2),
        None,
        None,
        stack(Item::Stick, 2),
        None,
    ];
    take_result(&mut offset, 3, 3);
    assert_eq!(offset[4], stack(Item::Coal, 1));
    assert_eq!(offset[7], stack(Item::Stick, 1));
}

#[tuff_core::watchdog]
#[test]
fn remainders_that_cannot_stay_go_to_the_player() {
    let mut milk = ItemStack::mint(Item::MilkBucket, 1, MintReason::Test);
    milk.components.custom_name = Some(Box::new(tuff_item::text::TextComponent::new("Moo")));
    let mut grid = [
        Some(milk.test_copy()),
        Some(milk.test_copy()),
        Some(milk),
        stack(Item::Sugar, 1),
        stack(Item::Egg, 1),
        stack(Item::Sugar, 1),
        stack(Item::Wheat, 1),
        stack(Item::Wheat, 1),
        stack(Item::Wheat, 1),
    ];
    assert!(take_result(&mut grid, 3, 3).is_empty());
    assert_eq!(grid[0], stack(Item::Bucket, 1));
    let mut two_deep = [
        stack(Item::MilkBucket, 2),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    ];
    let input = CraftingInput::positioned(3, 3, &two_deep).input;
    assert_eq!(input.crafting_remainders(), vec![stack(Item::Bucket, 1)]);
    let leftovers = take_result(&mut two_deep, 3, 3);
    assert_eq!(
        leftovers,
        vec![ItemStack::mint(Item::Bucket, 1, MintReason::Test)]
    );
    assert_eq!(two_deep[0], stack(Item::MilkBucket, 1));
}

#[tuff_core::watchdog]
#[test]
fn book_cloning_and_banners_copy_their_source() {
    let mut book = ItemStack::mint(Item::WrittenBook, 1, MintReason::Test);
    book.components.written_book = Some(Box::new(WrittenBook {
        title: Filterable::plain("Tale".into()),
        author: "Alex".into(),
        generation: 0,
        pages: vec![Filterable::plain(Tag::String("once".into()))],
        resolved: false,
    }));
    let input = grid3([
        Some(book.test_copy()),
        stack(Item::WritableBook, 1),
        stack(Item::WritableBook, 1),
        None,
        None,
        None,
        None,
        None,
        None,
    ]);
    let (name, result) = found(&input);
    assert_eq!(name, "minecraft:book_cloning");
    assert_eq!(result.count(), 2);
    assert_eq!(
        result.components.written_book.as_ref().unwrap().generation,
        1
    );
    let remaining = recipe("minecraft:book_cloning").remainders(&input);
    assert_eq!(remaining[0], Some(book.test_copy()));
    assert_eq!(remaining[1], None);
    let mut old = book.test_copy();
    old.components.written_book.as_mut().unwrap().generation = 2;
    assert!(
        find_recipe(&grid2([
            Some(old),
            stack(Item::WritableBook, 1),
            None,
            None
        ]))
        .is_none()
    );

    let mut patterned = ItemStack::mint(Item::WhiteBanner, 1, MintReason::Test);
    patterned.components.banner_patterns = Some(vec![BannerLayer {
        inline: None,
        pattern: "minecraft:stripe_bottom".into(),
        color: DyeColor::Lime,
    }]);
    let duplicate = grid2([
        Some(patterned.test_copy()),
        None,
        stack(Item::WhiteBanner, 1),
        None,
    ]);
    let (name, result) = found(&duplicate);
    assert_eq!(name, "minecraft:white_banner_duplicate");
    assert_eq!(
        result.components.banner_patterns,
        patterned.components.banner_patterns
    );
    let remaining = recipe("minecraft:white_banner_duplicate").remainders(&duplicate);
    assert_eq!(remaining[0], Some(patterned.test_copy()));
    let other_color = grid2([
        Some(patterned.test_copy()),
        stack(Item::RedBanner, 1),
        None,
        None,
    ]);
    assert!(find_recipe(&other_color).is_none());

    let shield = grid2([
        Some(patterned.test_copy()),
        stack(Item::Shield, 1),
        None,
        None,
    ]);
    let (name, result) = found(&shield);
    assert_eq!(name, "minecraft:shield_decoration");
    assert_eq!(result.item(), Item::Shield);
    assert_eq!(result.components.base_color, Some(DyeColor::White));
    assert_eq!(
        result.components.banner_patterns,
        patterned.components.banner_patterns
    );
    assert!(find_recipe(&grid2([Some(patterned), Some(result), None, None])).is_none());
}

#[tuff_core::watchdog]
#[test]
fn fireworks_take_fuel_stars_and_dyes() {
    let mut star = ItemStack::mint(Item::FireworkStar, 1, MintReason::Test);
    star.components.firework_explosion = Some(Box::new(FireworkExplosion {
        shape: FireworkShape::Star,
        colors: vec![1],
        ..Default::default()
    }));
    let rocket = grid3([
        stack(Item::Paper, 1),
        stack(Item::Gunpowder, 1),
        stack(Item::Gunpowder, 1),
        Some(star.test_copy()),
        None,
        None,
        None,
        None,
        None,
    ]);
    let (name, result) = found(&rocket);
    assert_eq!(name, "minecraft:firework_rocket");
    assert_eq!(result.count(), 3);
    let fireworks = result.components.fireworks.clone().unwrap();
    assert_eq!(fireworks.flight_duration, 2);
    assert_eq!(
        fireworks.explosions,
        vec![*star.components.firework_explosion.clone().unwrap()]
    );
    let too_much_fuel = grid3([
        stack(Item::Paper, 1),
        stack(Item::Gunpowder, 1),
        stack(Item::Gunpowder, 1),
        stack(Item::Gunpowder, 1),
        stack(Item::Gunpowder, 1),
        None,
        None,
        None,
        None,
    ]);
    assert!(find_recipe(&too_much_fuel).is_none());

    let new_star = grid3([
        stack(Item::Gunpowder, 1),
        stack(Item::RedDye, 1),
        stack(Item::GoldNugget, 1),
        stack(Item::Diamond, 1),
        None,
        None,
        None,
        None,
        None,
    ]);
    let (name, result) = found(&new_star);
    assert_eq!(name, "minecraft:firework_star");
    let explosion = result.components.firework_explosion.clone().unwrap();
    assert_eq!(explosion.shape, FireworkShape::Star);
    assert_eq!(explosion.colors, vec![DyeColor::Red.firework_color()]);
    assert!(explosion.has_trail && !explosion.has_twinkle);

    let fade = grid2([Some(star.test_copy()), stack(Item::BlueDye, 1), None, None]);
    let (name, result) = found(&fade);
    assert_eq!(name, "minecraft:firework_star_fade");
    let explosion = result.components.firework_explosion.clone().unwrap();
    assert_eq!(explosion.colors, vec![1]);
    assert_eq!(explosion.fade_colors, vec![DyeColor::Blue.firework_color()]);
}

#[tuff_core::watchdog]
#[test]
fn imbue_copies_the_potion() {
    let mut potion = ItemStack::mint(Item::LingeringPotion, 1, MintReason::Test);
    potion.components.potion_contents = Some(Box::new(tuff_item::item::PotionContents {
        potion: Some(tuff_vanilla_data::registry::Potion::Swiftness),
        ..Default::default()
    }));
    let arrow = || stack(Item::Arrow, 1);
    let input = grid3([
        arrow(),
        arrow(),
        arrow(),
        arrow(),
        Some(potion.test_copy()),
        arrow(),
        arrow(),
        arrow(),
        arrow(),
    ]);
    let (name, result) = found(&input);
    assert_eq!(name, "minecraft:tipped_arrow");
    assert_eq!(result.count(), 8);
    assert_eq!(
        result.components.potion_contents,
        potion.components.potion_contents
    );
}

#[tuff_core::watchdog]
#[test]
fn stonecutter_and_smithing_results() {
    let options = vanilla_set().stonecutter_options(Item::Cobblestone);
    let names: Vec<&str> = options
        .iter()
        .map(|i| vanilla_set().stonecutter_recipe(*i).unwrap().0)
        .collect();
    assert!(names.contains(&"minecraft:cobblestone_slab_from_cobblestone_stonecutting"));
    assert!(names.contains(&"minecraft:cobblestone_stairs_from_cobblestone_stonecutting"));
    assert!(vanilla_set().stonecutter_accepts(Item::Andesite));
    assert!(!vanilla_set().stonecutter_accepts(Item::Dirt));
    assert!(vanilla_set().stonecutter_options(Item::Dirt).is_empty());

    let mut sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    sword.set_damage(10);
    sword.components.enchantments =
        Some(Enchantments::new([("minecraft:sharpness".to_owned(), 3)]));
    let template = ItemStack::mint(Item::NetheriteUpgradeSmithingTemplate, 1, MintReason::Test);
    let ingot = ItemStack::mint(Item::NetheriteIngot, 1, MintReason::Test);
    let (entry, result) = vanilla_set()
        .smithing(Some(&template), Some(&sword), Some(&ingot))
        .unwrap();
    assert_eq!(entry, "minecraft:netherite_sword_smithing");
    let result = result.unwrap();
    assert_eq!(result.item(), Item::NetheriteSword);
    assert_eq!(result.damage(), 10);
    assert_eq!(
        result.components.enchantments,
        sword.components.enchantments
    );
    assert!(
        vanilla_set()
            .smithing(None, Some(&sword), Some(&ingot))
            .is_none()
    );

    let coast = ItemStack::mint(Item::CoastArmorTrimSmithingTemplate, 1, MintReason::Test);
    let boots = ItemStack::mint(Item::IronBoots, 1, MintReason::Test);
    let (entry, trimmed) = vanilla_set()
        .smithing(Some(&coast), Some(&boots), Some(&ingot))
        .unwrap();
    assert_eq!(
        entry,
        "minecraft:coast_armor_trim_smithing_template_smithing_trim"
    );
    let trimmed = trimmed.unwrap();
    let trim = trimmed.components.trim.clone().unwrap();
    assert_eq!(
        (trim.material.as_str(), trim.pattern.as_str()),
        ("minecraft:netherite", "minecraft:coast")
    );
    assert_eq!(
        vanilla_set()
            .smithing(Some(&coast), Some(&trimmed), Some(&ingot))
            .unwrap()
            .1,
        None
    );
    assert!(
        vanilla_set()
            .property_set("smithing_template")
            .binary_search(&Item::CoastArmorTrimSmithingTemplate)
            .is_ok()
    );
    assert!(
        vanilla_set()
            .property_set("smithing_base")
            .binary_search(&Item::IronBoots)
            .is_ok()
    );
    assert!(
        vanilla_set()
            .property_set("furnace_input")
            .binary_search(&Item::IronOre)
            .is_ok()
    );
    assert!(
        vanilla_set()
            .property_set("smoker_input")
            .binary_search(&Item::IronOre)
            .is_err()
    );
}

#[tuff_core::watchdog]
#[test]
fn displays_follow_the_game_shapes() {
    let torch = displays_for("minecraft:torch").next().unwrap();
    match &torch.display {
        RecipeDisplay::Shaped {
            width,
            height,
            ingredients,
            result,
            station,
        } => {
            assert_eq!((*width, *height), (1, 2));
            assert_eq!(ingredients.len(), 2);
            assert_eq!(
                *result,
                SlotDisplay::template(tuff_item::item::ItemPicture::new(Item::Torch, 4))
            );
            assert_eq!(*station, SlotDisplay::Item(Item::CraftingTable));
        }
        other => panic!("torch is shaped, got {other:?}"),
    }
    assert_eq!(torch.requirements.as_ref().map(Vec::len), Some(2));
    assert!(displays_for("minecraft:repair_item").next().is_none());
    let cloning: Vec<_> = displays_for("minecraft:map_cloning").collect();
    assert_eq!(cloning.len(), 8);
    assert!(
        matches!(&cloning[7].display, RecipeDisplay::Shapeless { ingredients, .. } if ingredients.len() == 9)
    );
    assert!(matches!(
        &cloning[0].display,
        RecipeDisplay::Shapeless {
            result: SlotDisplay::Composite(_),
            ..
        }
    ));
    let dye = displays_for("minecraft:leather_helmet_dyed")
        .next()
        .unwrap();
    assert!(matches!(
        &dye.display,
        RecipeDisplay::Shapeless {
            result: SlotDisplay::Dyed { .. },
            ..
        }
    ));
    let smelt = displays_for("minecraft:iron_ingot_from_smelting_iron_ore")
        .next()
        .unwrap();
    assert!(matches!(
        &smelt.display,
        RecipeDisplay::Furnace {
            fuel: SlotDisplay::AnyFuel,
            duration: 200,
            ..
        }
    ));
    assert_eq!(
        smelt.category,
        tuff_vanilla_data::registry::RecipeBookCategory::FurnaceMisc
    );
    let cut = displays_for("minecraft:andesite_slab_from_andesite_stonecutting")
        .next()
        .unwrap();
    assert!(matches!(&cut.display, RecipeDisplay::Stonecutter { .. }));
    let trim = displays_for("minecraft:coast_armor_trim_smithing_template_smithing_trim")
        .next()
        .unwrap();
    assert!(matches!(
        &trim.display,
        RecipeDisplay::Smithing {
            result: SlotDisplay::SmithingTrim { .. },
            ..
        }
    ));
    assert!(
        vanilla_set()
            .displays()
            .iter()
            .enumerate()
            .all(|(i, e)| e.id as usize == i)
    );
    let (planks, sticks) = (
        displays_for("minecraft:oak_planks").next().unwrap(),
        displays_for("minecraft:birch_planks").next().unwrap(),
    );
    assert_eq!(planks.group, sticks.group);
    assert!(planks.group.is_some());
}

#[tuff_core::watchdog]
#[test]
fn placement_layout_centres_small_recipes() {
    let cells: Vec<Option<usize>> = vec![Some(0), Some(1)];
    assert_eq!(
        layout(3, 3, Some((1, 2)), &cells),
        vec![(Some(0), 1), (Some(1), 4)]
    );
    assert_eq!(
        layout(2, 2, Some((1, 2)), &cells),
        vec![(Some(0), 0), (Some(1), 2)]
    );
    let one = vec![Some(0)];
    assert_eq!(layout(3, 3, Some((1, 1)), &one), vec![(Some(0), 4)]);
    let shapeless = vec![Some(0), Some(1), Some(2)];
    assert_eq!(
        layout(3, 3, None, &shapeless),
        vec![(Some(0), 0), (Some(1), 1), (Some(2), 2)]
    );
    let bow = recipe("minecraft:bow").placement().unwrap();
    assert_eq!(bow.size, Some((3, 3)));
    assert_eq!(
        bow.layout,
        vec![
            None,
            Some(0),
            Some(1),
            Some(2),
            None,
            Some(3),
            None,
            Some(4),
            Some(5)
        ]
    );
}

#[tuff_core::watchdog]
#[test]
fn recipe_book_placement_moves_ingredients_and_reports_missing_ones() {
    let torch = recipe("minecraft:torch");
    let mut inventory = Inventory::default();
    inventory.set_slot(36, stack(Item::Coal, 5));
    inventory.set_slot(20, stack(Item::Stick, 3));
    let mut grid = (0..4).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    let mut drops = Vec::new();
    let outcome = place_recipe(
        torch,
        &mut Grid {
            width: 2,
            height: 2,
            slots: &mut grid,
        },
        &mut [],
        &mut inventory,
        false,
        false,
        &mut drops,
    );
    assert_eq!(outcome, PostPlace::Nothing);
    assert_eq!(
        grid,
        vec![stack(Item::Coal, 1), None, stack(Item::Stick, 1), None]
    );
    assert_eq!(inventory.slots()[36], stack(Item::Coal, 4));
    assert_eq!(inventory.slots()[20], stack(Item::Stick, 2));
    assert_eq!(grid_result(&grid, 2, 2), stack(Item::Torch, 4));

    let outcome = place_recipe(
        torch,
        &mut Grid {
            width: 2,
            height: 2,
            slots: &mut grid,
        },
        &mut [],
        &mut inventory,
        false,
        false,
        &mut drops,
    );
    assert_eq!(outcome, PostPlace::Nothing);
    assert_eq!(
        grid,
        vec![stack(Item::Coal, 2), None, stack(Item::Stick, 2), None]
    );

    let outcome = place_recipe(
        torch,
        &mut Grid {
            width: 2,
            height: 2,
            slots: &mut grid,
        },
        &mut [],
        &mut inventory,
        true,
        false,
        &mut drops,
    );
    assert_eq!(
        grid,
        vec![stack(Item::Coal, 3), None, stack(Item::Stick, 3), None]
    );
    assert_eq!(outcome, PostPlace::Nothing);
    assert_eq!(inventory.slots()[36], stack(Item::Coal, 2));
    assert_eq!(inventory.slots()[20], None);

    let bread = recipe("minecraft:bread");
    let outcome = place_recipe(
        bread,
        &mut Grid {
            width: 2,
            height: 2,
            slots: &mut grid,
        },
        &mut [],
        &mut inventory,
        false,
        false,
        &mut drops,
    );
    assert_eq!(outcome, PostPlace::Ghost);
    assert_eq!(
        grid,
        (0..4).map(|_| None).collect::<Vec<Option<ItemStack>>>()
    );
    assert_eq!(inventory.slots()[36], stack(Item::Coal, 5));
    assert_eq!(inventory.slots()[37], stack(Item::Stick, 3));
    assert!(drops.is_empty());

    let mut enchanted = ItemStack::mint(Item::Stick, 1, MintReason::Test);
    enchanted.components.custom_name = Some(Box::new(tuff_item::text::TextComponent::new("Rod")));
    let mut named = Inventory::default();
    named.set_slot(36, Some(enchanted));
    named.set_slot(37, stack(Item::Coal, 1));
    let mut grid = (0..4).map(|_| None).collect::<Vec<Option<ItemStack>>>();
    let outcome = place_recipe(
        torch,
        &mut Grid {
            width: 2,
            height: 2,
            slots: &mut grid,
        },
        &mut [],
        &mut named,
        false,
        false,
        &mut drops,
    );
    assert_eq!(outcome, PostPlace::Ghost);
}

#[tuff_core::watchdog]
#[test]
fn placement_needs_room_to_clear_the_grid_unless_dropping_is_allowed() {
    let torch = recipe("minecraft:torch");
    let mut inventory = Inventory::default();
    for slot in 9..45 {
        inventory.set_slot(slot, stack(Item::Dirt, 64));
    }
    inventory.set_slot(36, stack(Item::Coal, 1));
    inventory.set_slot(37, stack(Item::Stick, 1));
    let mut grid = vec![stack(Item::Stone, 1), None, None, None];
    let mut drops = Vec::new();
    let outcome = place_recipe(
        torch,
        &mut Grid {
            width: 2,
            height: 2,
            slots: &mut grid,
        },
        &mut [],
        &mut inventory,
        false,
        false,
        &mut drops,
    );
    assert_eq!(outcome, PostPlace::Nothing);
    assert_eq!(grid[0], stack(Item::Stone, 1));
    let outcome = place_recipe(
        torch,
        &mut Grid {
            width: 2,
            height: 2,
            slots: &mut grid,
        },
        &mut [],
        &mut inventory,
        false,
        true,
        &mut drops,
    );
    assert_eq!(outcome, PostPlace::Nothing);
    assert_eq!(
        drops,
        vec![ItemStack::mint(Item::Stone, 1, MintReason::Test)]
    );
    assert_eq!(
        grid,
        vec![stack(Item::Coal, 1), None, stack(Item::Stick, 1), None]
    );
}

/// A suspicious stew crafted from a bowl, two mushrooms and a flower must
/// carry that flower's `minecraft:suspicious_stew_effects` component (the
/// recipe's result already sets it; the crafting result path must apply
/// it like any other recipe result component).
#[tuff_core::watchdog]
#[test]
fn suspicious_stew_carries_its_flowers_effect() {
    use tuff_item::item::SuspiciousEffect;
    use tuff_vanilla_data::registry::MobEffect;

    let input = grid3([
        stack(Item::Bowl, 1),
        stack(Item::BrownMushroom, 1),
        stack(Item::BrownMushroom, 1),
        stack(Item::Poppy, 1),
        None,
        None,
        None,
        None,
        None,
    ]);
    let (name, result) = found(&input);
    assert_eq!(name, "minecraft:suspicious_stew_from_poppy");
    assert_eq!(result.item(), Item::SuspiciousStew);
    assert_eq!(
        result.components.suspicious_stew_effects,
        Some(vec![SuspiciousEffect {
            effect: MobEffect::NightVision,
            duration: 100,
        }])
    );
}

#[tuff_core::watchdog]
#[test]
fn decorated_pots_take_the_four_side_items() {
    let pot = grid3([
        None,
        stack(Item::Brick, 1),
        None,
        stack(Item::AnglerPotterySherd, 1),
        None,
        stack(Item::Brick, 1),
        None,
        stack(Item::ArcherPotterySherd, 1),
        None,
    ]);
    let (name, result) = found(&pot);
    assert_eq!(name, "minecraft:decorated_pot");
    assert_eq!(result.item(), Item::DecoratedPot);
    let sides = result
        .components
        .other
        .get("minecraft:pot_decorations")
        .and_then(tuff_item::item::pot_decorations_from_nbt)
        .unwrap();
    let items: Vec<_> = sides
        .iter()
        .map(|side| side.as_ref().map(|s| s.item()))
        .collect();
    assert_eq!(
        items,
        [
            Some(Item::Brick),
            Some(Item::AnglerPotterySherd),
            Some(Item::Brick),
            Some(Item::ArcherPotterySherd)
        ]
    );
    let corner = grid3([
        stack(Item::Brick, 1),
        None,
        None,
        stack(Item::Brick, 1),
        None,
        stack(Item::Brick, 1),
        None,
        stack(Item::Brick, 1),
        None,
    ]);
    assert!(find_recipe(&corner).is_none());
}

#[tuff_core::watchdog]
#[test]
fn extending_a_map_surrounds_it_with_paper() {
    let mut map = ItemStack::mint(Item::FilledMap, 1, MintReason::Test);
    map.components.other.insert("minecraft:map_id", 7);
    let mut cells: [Option<ItemStack>; 9] = std::array::from_fn(|_| stack(Item::Paper, 1));
    cells[4] = Some(map.test_copy());
    let (name, result) = found(&grid3(cells));
    assert_eq!(name, "minecraft:map_extending");
    assert_eq!(result.item(), Item::FilledMap);
    assert_eq!(result.count(), 1);
    assert!(result.components.other.contains_key("minecraft:map_id"));
}

#[tuff_core::watchdog]
#[test]
fn clearing_the_grid_shares_one_free_slot_between_matching_stacks() {
    let torch = recipe("minecraft:torch");
    let full_but_one = || {
        let mut inventory = Inventory::default();
        for slot in 9..44 {
            inventory.set_slot(slot, stack(Item::Dirt, 64));
        }
        inventory.set_slot(36, stack(Item::Coal, 1));
        inventory.set_slot(37, stack(Item::Stick, 1));
        inventory
    };
    let attempt = |grid: &mut Vec<Option<ItemStack>>| {
        let mut inventory = full_but_one();
        let mut drops = Vec::new();
        place_recipe(
            torch,
            &mut Grid {
                width: 2,
                height: 2,
                slots: grid,
            },
            &mut [],
            &mut inventory,
            false,
            false,
            &mut drops,
        );
        assert!(drops.is_empty());
    };
    let mut shared = vec![stack(Item::Stone, 30), stack(Item::Stone, 34), None, None];
    attempt(&mut shared);
    assert_eq!(
        shared,
        vec![stack(Item::Coal, 1), None, stack(Item::Stick, 1), None]
    );
    let mut overflow = vec![stack(Item::Stone, 30), stack(Item::Stone, 35), None, None];
    attempt(&mut overflow);
    assert_eq!(overflow[1], stack(Item::Stone, 35));
    let mut mixed = vec![stack(Item::Stone, 1), stack(Item::Gravel, 1), None, None];
    attempt(&mut mixed);
    assert_eq!(mixed[1], stack(Item::Gravel, 1));
}
