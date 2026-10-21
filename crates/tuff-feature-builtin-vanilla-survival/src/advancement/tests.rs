use super::*;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use trigger::Event;
use tuff_core::world::advancement::Id;
use tuff_core::world::advancement::entry::MAX_DEPTH;
use tuff_core::world::advancement::entry::{Criterion, Display, Entry, Frame, Rewards};
use tuff_core::world::advancement::{Granted, Progress, Tree};
use tuff_core::world::predicate::EntityFacts;
use tuff_datapack::tags::TagSet;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::Frozen;

use tuff_core::world::advancement::{
    Advancements, NoAdvancements, Overlay, OverlayFile, Related, View,
};
use tuff_item::item::{ItemStack, MintReason};
use tuff_vanilla_data::Item;

fn tree() -> Tree {
    super::builtin()
}

fn grant_all(progress: &mut Progress, tree: &Tree, name: &str) {
    let id = tree.id(name).unwrap();
    for index in 0..tree.get(id).criteria.len() {
        progress.grant(tree, id, index as u16, 1_000);
    }
}

#[tuff_core::watchdog]
#[test]
fn requirements_are_an_and_of_ors() {
    let tree = tree();
    let id = tree.id("minecraft:adventure/adventuring_time").unwrap();
    let advancement = tree.get(id);
    assert!(advancement.requirements.len() > 40, "one group per biome");
    let mut progress = Progress::new();
    for (i, _) in advancement.requirements.iter().enumerate().skip(1) {
        let name = advancement.requirements[i][0].clone();
        let index = tree.criterion_index(id, &name).unwrap();
        assert!(!progress.grant(&tree, id, index, 5).completed);
    }
    assert!(!progress.is_done(&tree, id));
    let last = tree
        .criterion_index(id, &advancement.requirements[0][0])
        .unwrap();
    assert_eq!(
        progress.grant(&tree, id, last, 5),
        Granted {
            granted: true,
            completed: true
        }
    );
    assert!(progress.is_done(&tree, id));
    assert!(!progress.grant(&tree, id, last, 6).granted);

    let chest = tree.id("minecraft:recipes/decorations/chest").unwrap();
    let either = tree.criterion_index(chest, "has_lots_of_items").unwrap();
    assert!(progress.grant(&tree, chest, either, 5).completed);
    assert!(progress.revoke(&tree, chest, either));
    assert!(!progress.is_done(&tree, chest));
    assert!(!progress.revoke(&tree, chest, either));
}

#[tuff_core::watchdog]
#[test]
fn the_first_update_resets_and_later_ones_only_carry_changes() {
    let tree = tree();
    let mut progress = Progress::new();
    assert_eq!(progress.flush(&tree), None, "nothing to show a new player");
    assert_eq!(progress.flush(&tree), None);

    grant_all(&mut progress, &tree, "minecraft:story/root");
    let update = progress.flush(&tree).unwrap();
    assert!(!update.reset);
    let names: Vec<&str> = update.added.iter().map(|id| &*tree.get(*id).name).collect();
    assert_eq!(names[0], "minecraft:story/root", "parents come first");
    assert!(names.contains(&"minecraft:story/mine_stone"));
    assert!(
        names.contains(&"minecraft:story/upgrade_tools"),
        "two levels below a done one"
    );
    assert!(
        !names.contains(&"minecraft:story/smelt_iron"),
        "three levels is too deep"
    );
    assert!(!names.contains(&"minecraft:adventure/root"));
    let root = tree.id("minecraft:story/root").unwrap();
    let (_, criteria) = update.progress.iter().find(|(id, _)| *id == root).unwrap();
    assert_eq!(
        criteria,
        &vec![(Frozen::Static("crafting_table"), Some(1_000))]
    );
    assert_eq!(progress.flush(&tree), None);

    let mine = tree.id("minecraft:story/mine_stone").unwrap();
    progress.grant(&tree, mine, 0, 2_000);
    let update = progress.flush(&tree).unwrap();
    assert!(
        update
            .added
            .iter()
            .any(|id| tree.get(*id).name == "minecraft:story/smelt_iron")
    );
    assert!(update.progress.iter().any(|(id, _)| *id == mine));

    progress.revoke(&tree, mine, 0);
    let update = progress.flush(&tree).unwrap();
    assert!(
        update
            .removed
            .iter()
            .any(|id| tree.get(*id).name == "minecraft:story/smelt_iron")
    );
}

#[tuff_core::watchdog]
#[test]
fn hidden_advancements_stay_hidden_until_done() {
    let tree = tree();
    let mut progress = Progress::new();
    grant_all(&mut progress, &tree, "minecraft:nether/root");
    let update = progress.flush(&tree).unwrap();
    let hidden = tree.id("minecraft:nether/create_full_beacon").unwrap();
    assert!(tree.get(hidden).display.as_ref().unwrap().hidden || !update.added.contains(&hidden));
    let all_effects = tree.id("minecraft:nether/all_effects").unwrap();
    assert!(tree.get(all_effects).display.as_ref().unwrap().hidden);
    assert!(!update.added.contains(&all_effects));
}

#[tuff_core::watchdog]
#[test]
fn recipe_advancements_show_only_once_done() {
    let tree = tree();
    let mut progress = Progress::new();
    let chest = tree.id("minecraft:recipes/decorations/chest").unwrap();
    progress.grant(&tree, chest, 0, 1);
    let update = progress.flush(&tree).unwrap();
    let root = tree.id("minecraft:recipes/root").unwrap();
    assert_eq!(
        update.added,
        vec![root, chest],
        "a done descendant shows its root"
    );
    let other = tree.id("minecraft:recipes/decorations/barrel").unwrap();
    assert!(
        !update.added.contains(&other),
        "displayless ones only once done"
    );
}

#[tuff_core::watchdog]
#[test]
fn progress_saves_and_loads_in_the_game_format() {
    let tree = tree();
    let mut progress = Progress::new();
    let stone = tree.id("minecraft:story/mine_stone").unwrap();
    progress.grant(&tree, stone, 0, 1_700_000_000_123);
    let time = tree.id("minecraft:adventure/adventuring_time").unwrap();
    progress.grant(&tree, time, 0, 1_700_000_001_000);
    let saved = progress.save(&tree);
    let entry = saved.get_compound("minecraft:story/mine_stone").unwrap();
    assert_eq!(entry.get("done"), Some(&Tag::Byte(1)));
    assert_eq!(
        entry.get_compound("criteria").unwrap().get("get_stone"),
        Some(&Tag::String("2023-11-14 22:13:20 +0000".into()))
    );
    assert_eq!(
        saved
            .get_compound("minecraft:adventure/adventuring_time")
            .unwrap()
            .get("done"),
        Some(&Tag::Byte(0))
    );
    assert!(saved.get("DataVersion").is_some());

    let loaded = Progress::load(&tree, &saved);
    assert!(loaded.is_done(&tree, stone));
    assert!(loaded.criterion_done(time, 0));
    assert_eq!(loaded.save(&tree), saved);

    let mut unknown = saved.clone();
    unknown.insert("minecraft:no/such", Tag::Compound(Compound::new()));
    assert_eq!(Progress::load(&tree, &unknown).save(&tree), saved);
}

#[tuff_core::watchdog]
#[test]
fn selecting_a_tab_only_accepts_displayed_roots() {
    let tree = tree();
    let mut progress = Progress::new();
    let story = tree.id("minecraft:story/root").unwrap();
    let stone = tree.id("minecraft:story/mine_stone").unwrap();
    assert_eq!(progress.select_tab(&tree, Some(story)), Some(Some(story)));
    assert_eq!(progress.select_tab(&tree, Some(story)), None);
    assert_eq!(progress.select_tab(&tree, Some(stone)), Some(None));
}

#[tuff_core::watchdog]
#[test]
fn inventory_criteria_are_found_through_the_item_index() {
    let tree = tree();
    let progress = Progress::new();
    let player = EntityFacts::default();
    let redstone = ItemStack::mint(Item::Redstone, 1, MintReason::Test);
    let inventory = vec![Some(redstone.view())];
    let event = Event::InventoryChanged {
        changed: Some(&redstone),
        inventory: &inventory,
    };
    let names: Vec<&str> = progress
        .matching(&tree, &event, || player.clone())
        .into_iter()
        .map(|(id, _)| &*tree.get(id).name)
        .collect();
    assert!(names.contains(&"minecraft:recipes/redstone/piston"));
    assert!(!names.contains(&"minecraft:recipes/decorations/chest"));
}

#[tuff_core::watchdog]
#[test]
fn every_displayed_tree_is_laid_out_without_overlap() {
    let tree = tree();
    let mut seen: HashMap<(Id, [u32; 2]), &Frozen<str>> = HashMap::new();
    for id in 0..tree.len() as Id {
        let advancement = tree.get(id);
        if advancement.display.is_none() {
            continue;
        }
        let key = (advancement.root, advancement.position.map(f32::to_bits));
        if let Some(other) = seen.insert(key, &advancement.name) {
            panic!("{} overlaps {other}", advancement.name);
        }
    }
    let root = tree.get(tree.id("minecraft:story/root").unwrap());
    assert_eq!(root.position[0], 0.0);
}

/// The recipes the advancements an event meets would reward.
fn rewarded<'a>(tree: &'a Tree, event: &Event<'_>) -> Vec<&'a str> {
    Progress::new()
        .matching(tree, event, EntityFacts::default)
        .into_iter()
        .flat_map(|(id, _)| tree.get(id).rewards.recipes.iter().map(|r| &**r))
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn recipe_unlocks_come_from_the_recipe_advancements() {
    let tree = tree();
    let redstone = ItemStack::mint(Item::Redstone, 1, MintReason::Test);
    let one = vec![Some(redstone.view())];
    let arrived = |stack: &ItemStack, inventory: &[Option<tuff_item::item::ItemView<'_>>]| {
        rewarded(
            &tree,
            &Event::InventoryChanged {
                changed: Some(stack),
                inventory,
            },
        )
    };
    assert!(arrived(&redstone, &one).contains(&"minecraft:piston"));
    let wheat = ItemStack::mint(Item::Wheat, 1, MintReason::Test);
    assert!(arrived(&wheat, &[Some(wheat.view())]).contains(&"minecraft:bread"));
    let netherite = ItemStack::mint(Item::NetheriteIngot, 1, MintReason::Test);
    assert!(
        arrived(&netherite, &[Some(netherite.view())])
            .contains(&"minecraft:netherite_sword_smithing")
    );
    assert!(rewarded(&tree, &Event::Tick).contains(&"minecraft:crafting_table"));
    let water = tuff_vanilla_data::Block::Water.default_state();
    assert!(rewarded(&tree, &Event::EnterBlock(water)).contains(&"minecraft:oak_boat"));

    let dirt = ItemStack::mint(Item::Dirt, 1, MintReason::Test);
    let mut inventory = (0..43)
        .map(|_| None)
        .collect::<Vec<Option<tuff_item::item::ItemView<'_>>>>();
    for slot in inventory.iter_mut().take(9) {
        *slot = Some(dirt.view());
    }
    assert!(!arrived(&dirt, &inventory).contains(&"minecraft:chest"));
    inventory[40] = Some(dirt.view());
    assert!(
        arrived(&dirt, &inventory).contains(&"minecraft:chest"),
        "the off hand counts as an occupied slot"
    );
    for advancement in &tuff_vanilla_data::advancement::ALL {
        assert!(
            !advancement
                .rewards
                .recipes
                .contains(&"minecraft:repair_item")
        );
    }
}

/// The advancement names any criterion `event` meets.
fn met_advancements<'a>(tree: &'a Tree, event: &Event<'_>) -> Vec<&'a str> {
    Progress::new()
        .matching(tree, event, EntityFacts::default)
        .into_iter()
        .map(|(id, _)| &*tree.get(id).name)
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn enter_block_only_matches_the_block_it_names() {
    let tree = tree();
    let water = tuff_vanilla_data::Block::Water.default_state();
    let gateway = tuff_vanilla_data::Block::EndGateway.default_state();
    // Standing in water must only meet water-gated criteria, never an
    // unrelated enter_block criterion for a different block (this was the
    // "Remote Getaway" bug: entering water granted the end gateway
    // advancement because its block condition was silently ignored).
    let in_water = met_advancements(&tree, &Event::EnterBlock(water));
    assert!(in_water.contains(&"minecraft:recipes/transportation/oak_boat"));
    assert!(!in_water.contains(&"minecraft:end/enter_end_gateway"));
    let in_gateway = met_advancements(&tree, &Event::EnterBlock(gateway));
    assert!(in_gateway.contains(&"minecraft:end/enter_end_gateway"));
    assert!(!in_gateway.contains(&"minecraft:recipes/transportation/oak_boat"));
}

#[tuff_core::watchdog]
#[test]
fn crafted_recipes_meet_recipe_crafted_criteria() {
    let tree = tree();
    let met = |event: &Event<'_>| -> Vec<(&str, &str)> {
        Progress::new()
            .matching(&tree, event, EntityFacts::default)
            .into_iter()
            .map(|(id, c)| {
                (
                    &*tree.get(id).name,
                    &*tree.get(id).criteria[c as usize].name,
                )
            })
            .collect()
    };
    let trim = "minecraft:coast_armor_trim_smithing_template_smithing_trim";
    let found = met(&Event::RecipeCrafted {
        recipe: trim,
        ingredients: &[],
    });
    assert!(
        found
            .iter()
            .any(|(name, _)| *name == "minecraft:adventure/trim_with_any_armor_pattern"),
        "{found:?}"
    );
    let sherds = [
        ItemStack::mint(Item::ExplorerPotterySherd, 1, MintReason::Test),
        ItemStack::mint(Item::HeartPotterySherd, 1, MintReason::Test),
        ItemStack::mint(Item::AnglerPotterySherd, 1, MintReason::Test),
        ItemStack::mint(Item::ArcherPotterySherd, 1, MintReason::Test),
    ];
    let pot = |ingredients: &[ItemStack]| {
        met(&Event::RecipeCrafted {
            recipe: "minecraft:decorated_pot",
            ingredients,
        })
        .iter()
        .any(|(name, _)| *name == "minecraft:adventure/craft_decorated_pot_using_only_sherds")
    };
    assert!(pot(&sherds));
    let bricks = [
        ItemStack::mint(Item::Brick, 1, MintReason::Test),
        ItemStack::mint(Item::Brick, 1, MintReason::Test),
        ItemStack::mint(Item::Brick, 1, MintReason::Test),
        ItemStack::mint(Item::Brick, 1, MintReason::Test),
    ];
    assert!(!pot(&bricks));
}

fn entry(name: &str, parent: Option<&str>, displayed: bool) -> Entry {
    let display = displayed.then(|| Display {
        title: TextComponent::new(name),
        description: TextComponent::default(),
        icon: ItemStack::mint(Item::Stone, 1, MintReason::Test),
        frame: Frame::Task,
        background: parent
            .is_none()
            .then(|| Frozen::from("minecraft:block/stone".to_owned())),
        show_toast: true,
        announce_to_chat: true,
        hidden: false,
    });
    Entry::new(
        Frozen::from(name.to_owned()),
        parent.map(|parent| Frozen::from(parent.to_owned())),
        display,
        vec![Criterion {
            name: Frozen::from("done".to_owned()),
            trigger: Frozen::Static("minecraft:impossible"),
            conditions: Value::Null,
        }],
        None,
        Rewards::default(),
        false,
    )
    .unwrap()
}

#[tuff_core::watchdog]
#[test]
fn laying_out_the_built_in_trees_again_gives_their_compiled_positions() {
    let tree = tree();
    let entries: Vec<Arc<Entry>> = tree.nodes.iter().map(|n| n.entry().clone()).collect();
    let placed = tuff_core::world::advancement::tree::place(entries, None);
    assert_eq!(placed.len(), tree.len());
    for (id, node) in placed.iter().enumerate() {
        let original = tree.get(id as Id);
        assert_eq!(node.name, original.name);
        assert_eq!(node.parent, original.parent, "{}", node.name);
        assert_eq!(node.root, original.root, "{}", node.name);
        assert_eq!(node.children, original.children, "{}", node.name);
        assert_eq!(node.position, original.position, "{}", node.name);
    }
}

#[tuff_core::watchdog]
#[test]
fn an_overlay_replaces_adds_and_removes_advancements_by_name() {
    let base = tree();
    let story = base.id("minecraft:story/root").unwrap();
    let overlay = base.overlay([
        (
            "pack:tab/root".to_owned(),
            Some(entry("pack:tab/root", None, true)),
        ),
        (
            "pack:tab/b".to_owned(),
            Some(entry("pack:tab/b", Some("pack:tab/root"), true)),
        ),
        (
            "pack:tab/a".to_owned(),
            Some(entry("pack:tab/a", Some("pack:tab/root"), true)),
        ),
        (
            "minecraft:story/mine_stone".to_owned(),
            Some(entry(
                "minecraft:story/mine_stone",
                Some("minecraft:story/root"),
                true,
            )),
        ),
        ("minecraft:husbandry/plant_seed".to_owned(), None),
        (
            "pack:lost".to_owned(),
            Some(entry("pack:lost", Some("pack:missing"), true)),
        ),
    ]);
    assert_eq!(
        overlay.len(),
        base.len() + 3 - 1 - count_below(&base, "minecraft:husbandry/plant_seed")
    );
    assert!(overlay.id("pack:lost").is_none(), "an orphan is left out");
    assert!(overlay.id("minecraft:husbandry/plant_seed").is_none());
    assert!(overlay.id("minecraft:husbandry/root").is_some());

    let root = overlay.id("pack:tab/root").unwrap();
    let a = overlay.id("pack:tab/a").unwrap();
    let b = overlay.id("pack:tab/b").unwrap();
    assert_eq!(&*overlay.get(root).children, &[a, b]);
    assert_eq!(overlay.get(root).position, [0.0, 0.5]);
    assert_eq!(overlay.get(a).position, [1.0, 0.0]);
    assert_eq!(overlay.get(b).position, [1.0, 1.0]);

    let stone = overlay.id("minecraft:story/mine_stone").unwrap();
    assert_eq!(&*overlay.get(stone).criteria[0].name, "done");
    assert!(
        overlay.get(stone).display.as_ref().unwrap().title
            == TextComponent::new("minecraft:story/mine_stone")
    );

    let same = |name: &str| {
        let (old, new) = (base.id(name).unwrap(), overlay.id(name).unwrap());
        (base.get(old).position, overlay.get(new).position)
    };
    let (old, new) = same("minecraft:adventure/kill_a_mob");
    assert_eq!(old, new, "an untouched tree keeps its layout");
    assert!(Arc::ptr_eq(
        base.get(story).entry(),
        overlay
            .get(overlay.id("minecraft:story/root").unwrap())
            .entry()
    ));
}

fn count_below(tree: &Tree, name: &str) -> usize {
    let mut stack = vec![tree.id(name).unwrap()];
    let mut count = 0;
    while let Some(id) = stack.pop() {
        stack.extend(tree.get(id).children.iter());
        count += usize::from(id != tree.id(name).unwrap());
    }
    count
}

#[tuff_core::watchdog]
#[test]
fn cycles_and_overly_deep_chains_are_left_out() {
    let base = Tree::new(&[], trigger::builtin_registry());
    let mut changes = vec![
        ("a:x".to_owned(), Some(entry("a:x", Some("a:y"), false))),
        ("a:y".to_owned(), Some(entry("a:y", Some("a:x"), false))),
        ("d:0".to_owned(), Some(entry("d:0", None, false))),
    ];
    for depth in 1..=MAX_DEPTH + 5 {
        let name = format!("d:{depth}");
        let parent = format!("d:{}", depth - 1);
        changes.push((name.clone(), Some(entry(&name, Some(&parent), false))));
    }
    let tree = base.overlay(changes);
    assert!(tree.id("a:x").is_none() && tree.id("a:y").is_none());
    assert!(tree.id(&format!("d:{MAX_DEPTH}")).is_some());
    assert!(tree.id(&format!("d:{}", MAX_DEPTH + 1)).is_none());
    assert_eq!(tree.len(), MAX_DEPTH + 1);
}

#[tuff_core::watchdog]
#[test]
fn progress_moves_to_a_reloaded_tree_by_name_and_resets_the_client() {
    let base = tree();
    let mut progress = Progress::new();
    grant_all(&mut progress, &base, "minecraft:story/root");
    grant_all(&mut progress, &base, "minecraft:story/mine_stone");
    grant_all(&mut progress, &base, "minecraft:husbandry/plant_seed");
    assert!(progress.flush(&base).is_some());

    let reloaded = base.overlay([
        ("a:first".to_owned(), Some(entry("a:first", None, false))),
        ("minecraft:husbandry/plant_seed".to_owned(), None),
    ]);
    let moved = progress.rebind(&base, &reloaded);
    assert!(moved.version() > progress.version());
    let stone = reloaded.id("minecraft:story/mine_stone").unwrap();
    assert_ne!(
        Some(stone),
        base.id("minecraft:story/mine_stone"),
        "ids shifted"
    );
    assert!(moved.is_done(&reloaded, stone));
    assert_eq!(moved.started().count(), 2, "the removed one is dropped");
    let mut moved = moved;
    let update = moved.flush(&reloaded).unwrap();
    assert!(update.reset);
    let root = reloaded.id("minecraft:story/root").unwrap();
    assert!(update.added.contains(&root) && update.added.contains(&stone));
    assert_eq!(
        moved
            .save(&reloaded)
            .get_compound("minecraft:story/mine_stone"),
        progress
            .save(&base)
            .get_compound("minecraft:story/mine_stone")
    );
}

fn met_by<'a>(tree: &'a Tree, event: &Event<'_>, player: &EntityFacts) -> Vec<&'a str> {
    Progress::new()
        .matching(tree, event, || player.clone())
        .into_iter()
        .map(|(id, _)| &*tree.get(id).name)
        .collect()
}

fn at(position: [f64; 3]) -> EntityFacts {
    EntityFacts {
        kind: Some(tuff_vanilla_data::registry::EntityType::Player),
        position,
        dimension: "minecraft:overworld".into(),
        ..EntityFacts::default()
    }
}

/// The built-in tags with `minecraft:glass` added to the block and item
/// `#minecraft:logs_that_burn`.
fn glass_burns() -> tuff_datapack::tags::TagSet {
    use tuff_datapack::tags::{TagEntry, TagSet, vanilla_builders};
    let mut builders = vanilla_builders();
    for registry in ["minecraft:block", "minecraft:item"] {
        builders
            .get_mut(registry)
            .unwrap()
            .get_mut("minecraft:logs_that_burn")
            .unwrap()
            .push(TagEntry {
                id: "minecraft:glass".to_owned(),
                tag: false,
                required: true,
            });
    }
    TagSet::resolve_all(&builders).0
}

#[tuff_core::watchdog]
#[test]
fn levitating_far_enough_meets_the_levitation_criterion() {
    let tree = tree();
    let event = Event::Levitation {
        start: [0.0, 70.0, 0.0],
        duration: 200,
    };
    assert!(met_by(&tree, &event, &at([0.0, 125.0, 0.0])).contains(&"minecraft:end/levitate"));
    assert!(!met_by(&tree, &event, &at([0.0, 100.0, 0.0])).contains(&"minecraft:end/levitate"));
}

#[tuff_core::watchdog]
#[test]
fn item_criteria_are_indexed_by_the_tags_the_tree_was_built_with() {
    let base = tree();
    let glass = ItemStack::mint(Item::Glass, 1, MintReason::Test);
    let inventory = vec![Some(glass.view())];
    let event = Event::InventoryChanged {
        changed: Some(&glass),
        inventory: &inventory,
    };
    let met = |tree: &Tree| {
        Progress::new()
            .matching(tree, &event, EntityFacts::default)
            .into_iter()
            .any(|(id, _)| &*tree.get(id).name == "minecraft:recipes/misc/charcoal")
    };
    let tags = std::sync::Arc::new(glass_burns());
    let reloaded = base.overlay_with([], &tags);
    assert!(!met(&base));
    assert!(
        !tuff_datapack::tags::sync_scope(tags.clone(), || met(&base)),
        "the base tree's index has no glass"
    );
    assert!(tuff_datapack::tags::sync_scope(tags, || met(&reloaded)));
}

#[tuff_core::watchdog]
#[test]
fn falling_from_the_build_limit_to_the_bottom_meets_its_criterion() {
    let tree = tree();
    let name = "minecraft:adventure/fall_from_world_height";
    let event = Event::FallFromHeight {
        start: [0.0, 320.0, 0.0],
    };
    assert!(met_by(&tree, &event, &at([0.0, -60.0, 0.0])).contains(&name));
    assert!(!met_by(&tree, &event, &at([0.0, 64.0, 0.0])).contains(&name));
    let low = Event::FallFromHeight {
        start: [0.0, 100.0, 0.0],
    };
    assert!(!met_by(&tree, &low, &at([0.0, -60.0, 0.0])).contains(&name));
}

#[tuff_core::watchdog]
#[test]
fn using_item_criteria_test_the_item() {
    let registry = trigger::builtin_registry();
    let conditions = serde_json::json!({"item": {"items": "minecraft:spyglass"}});
    let spyglass = ItemStack::mint(Item::Spyglass, 1, MintReason::Test);
    let bow = ItemStack::mint(Item::Bow, 1, MintReason::Test);
    let player = at([0.0, 64.0, 0.0]);
    let met = |stack| {
        registry.matches(
            "minecraft:using_item",
            &conditions,
            &Event::UsingItem(stack),
            &player,
        )
    };
    assert!(met(&spyglass));
    assert!(!met(&bow));
}

#[tuff_core::watchdog]
#[test]
fn predicates_read_the_tags_in_effect() {
    let glass = ItemStack::mint(Item::Glass, 1, MintReason::Test);
    let items = serde_json::json!({"items": "#minecraft:logs"});
    let blocks = serde_json::json!({"blocks": "#logs"});
    let check = || {
        (
            tuff_core::world::predicate::item(&items, &glass),
            tuff_core::world::predicate::block(
                &blocks,
                tuff_vanilla_data::Block::Glass.default_state(),
            ),
        )
    };
    assert_eq!(check(), (false, false));
    let tags = std::sync::Arc::new(glass_burns());
    assert_eq!(tuff_datapack::tags::sync_scope(tags, check), (true, true));
}

#[tuff_core::watchdog]
#[test]
fn related_walks_ancestors_and_descendants() {
    let tree = tree();
    let id = tree.id("minecraft:story/mine_stone").unwrap();
    assert_eq!(tree.related(id, Related::Only), vec![id]);
    let until = tree.related(id, Related::Until);
    assert_eq!(until.last(), Some(&id));
    assert_eq!(until.len(), 2);
    let from = tree.related(id, Related::From);
    assert_eq!(from[0], id);
    assert!(from.len() > 1);
    let through = tree.related(id, Related::Through);
    assert_eq!(through.len(), until.len() + from.len() - 1);
    assert_eq!(tree.ids().len(), tree.len());
}

#[tuff_core::watchdog]
#[test]
fn handles_overlay_and_match_ids_by_name() {
    let base: Arc<dyn Advancements> = shared_builtin();
    let predicates = tuff_datapack::loot::Predicates::vanilla();
    let tags = TagSet::vanilla();
    let file = |id: &str, json: &str| OverlayFile {
        id: id.to_owned(),
        origin: "test".to_owned(),
        json: json.as_bytes().to_vec(),
    };
    let overlay = |unparsed_removes| Overlay {
        removed: vec!["minecraft:story/mine_diamond".to_owned()],
        files: vec![
            file(
                "a:root",
                r#"{"criteria":{"x":{"trigger":"minecraft:impossible"}}}"#,
            ),
            file("minecraft:story/mine_stone", "not json"),
        ],
        unparsed_removes,
        predicates: &predicates,
        tags: &tags,
    };
    let (kept, accepted) = base.with_overlay(overlay(false));
    assert_eq!(accepted, ["a:root"]);
    assert!(kept.id("a:root").is_some());
    assert!(kept.id("minecraft:story/mine_diamond").is_none());
    assert!(kept.id("minecraft:story/mine_stone").is_some());
    let (removed, _) = base.with_overlay(overlay(true));
    assert!(removed.id("minecraft:story/mine_stone").is_none());

    let iron = base.id("minecraft:story/smelt_iron").unwrap();
    let moved = kept.same_as(&*base, iron).unwrap();
    assert_ne!(moved, iron, "a:root sorts before every vanilla name");
    assert_eq!(kept.name(moved), "minecraft:story/smelt_iron");
    assert_eq!(base.same_as(&*base, iron), Some(iron));
    let gone = base.id("minecraft:story/mine_diamond").unwrap();
    assert_eq!(kept.same_as(&*base, gone), None);
    assert!(kept.has_criterion(kept.id("a:root").unwrap(), "x"));
    assert_eq!(
        base.names().count(),
        base.len(),
        "names cover every advancement"
    );
}

#[tuff_core::watchdog]
#[test]
fn no_advancements_is_inert() {
    let none: Arc<dyn Advancements> = Arc::new(NoAdvancements);
    assert!(none.is_empty() && none.ids().is_empty());
    assert_eq!(none.id("minecraft:story/root"), None);
    assert_eq!(none.same_as(&*shared_builtin(), 0), None);
    let tags = TagSet::vanilla();
    let (laid, accepted) = none.with_overlay(Overlay {
        removed: Vec::new(),
        files: Vec::new(),
        unparsed_removes: true,
        predicates: &tuff_datapack::loot::Predicates::vanilla(),
        tags: &tags,
    });
    assert!(laid.is_empty() && accepted.is_empty());
    let view = View(Arc::new(NoAdvancements));
    assert_eq!(view.done("minecraft:story/root"), None);
    assert_eq!(
        view.criterion("minecraft:story/root", "crafting_table"),
        None
    );
}

#[tuff_core::watchdog]
#[test]
fn advancement_ids_complete_from_the_tree_in_effect() {
    use tuff_core::command::SuggestContext;
    use tuff_core::command::args::suggest_argument;
    let entry = Entry::new(
        "pack:custom".to_owned().into(),
        None,
        None,
        vec![Criterion {
            name: "a".to_owned().into(),
            trigger: Frozen::Static("minecraft:impossible"),
            conditions: serde_json::Value::Null,
        }],
        None,
        Rewards::default(),
        false,
    )
    .unwrap();
    let tree = shared_builtin().overlay([("pack:custom".to_owned(), Some(entry))]);
    let props = Some(r#"{"registry":"minecraft:advancement"}"#);
    let mut context = SuggestContext::default();
    assert!(suggest_argument("minecraft:resource_key", props, "pack:", &context).is_empty());
    context.advancements = Some(Arc::new(tree));
    assert_eq!(
        suggest_argument("minecraft:resource_key", props, "pack:", &context),
        ["pack:custom"]
    );
}

#[tuff_core::watchdog]
#[test]
fn provided_and_pack_advancements_land_in_the_built_tree() {
    let base: Arc<dyn Advancements> = shared_builtin();
    let predicates = tuff_datapack::loot::Predicates::vanilla();
    let tags = TagSet::vanilla();
    let file = |id: &str, json: &str| OverlayFile {
        id: id.to_owned(),
        origin: "test".to_owned(),
        json: json.as_bytes().to_vec(),
    };
    let (tree, accepted) = base.with_overlay(Overlay {
        removed: vec!["minecraft:story/mine_stone".to_owned()],
        files: vec![
            file(
                "mine:found_ruby",
                r#"{"criteria":{"never":{"trigger":"minecraft:impossible"}}}"#,
            ),
            file(
                "mine:bad",
                r#"{"criteria":{"x":{"trigger":"mine:no_such_trigger"}}}"#,
            ),
            file(
                "minecraft:story/root",
                r#"{"criteria":{"y":{"trigger":"minecraft:impossible"}}}"#,
            ),
        ],
        unparsed_removes: false,
        predicates: &predicates,
        tags: &tags,
    });
    assert_eq!(accepted, ["mine:found_ruby", "minecraft:story/root"]);
    assert!(tree.id("mine:found_ruby").is_some());
    assert!(tree.id("mine:bad").is_none());
    assert!(tree.id("minecraft:story/mine_stone").is_none());
    assert!(tree.id("minecraft:husbandry/root").is_some());
    let root = tree.id("minecraft:story/root").unwrap();
    assert!(tree.has_criterion(root, "y"));
    assert!(!tree.has_criterion(root, "crafting_table"));
}
