use rand::SeedableRng;
use rand::rngs::StdRng;
use tuff_vanilla_data::Frozen;

use super::*;

fn fished(luck: f32, open_water: bool, runs: u64) -> Vec<Item> {
    let rod = ItemStack::new(Item::FishingRod, 1);
    let mut rng = StdRng::seed_from_u64(7);
    (0..runs)
        .flat_map(|_| fishing_drops(LootTables::builtin(), luck, open_water, &rod, &mut rng))
        .map(|s| s.item)
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn fishing_mostly_catches_fish_and_never_treasure_outside_open_water() {
    let caught = fished(0.0, false, 600);
    assert_eq!(caught.len(), 600);
    let fish = caught
        .iter()
        .filter(|item| tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::FISHES, **item))
        .count();
    assert!(fish > 450, "{fish} of 600 were fish");
    assert!(!caught.contains(&Item::NameTag) && !caught.contains(&Item::Saddle));
}

#[tuff_core::watchdog]
#[test]
fn luck_trades_junk_for_treasure_in_open_water() {
    let treasure = |items: &[Item]| {
        items
            .iter()
            .filter(|i| {
                matches!(
                    i,
                    Item::NameTag | Item::Saddle | Item::Bow | Item::FishingRod | Item::Book
                )
            })
            .count()
    };
    let plain = fished(0.0, true, 2000);
    let lucky = fished(3.0, true, 2000);
    assert!(treasure(&plain) > 0, "open water allows treasure");
    assert!(treasure(&lucky) > treasure(&plain));
}

#[tuff_core::watchdog]
#[test]
fn tag_entries_read_the_tags_in_effect() {
    let tags = tuff_datapack::tags::TagSet::vanilla();
    let table = tuff_datapack::loot::parse(
        br##"{"pools":[{"rolls":1,"entries":[{"type":"minecraft:tag","name":"minecraft:wool_carpets","expand":false}]}]}"##,
        &tags,
        &tuff_datapack::loot::Predicates::vanilla(),
    )
    .unwrap();
    let tables = tuff_datapack::loot::LootTables::new(
        [("t:t".to_owned(), Some(table))].into_iter().collect(),
    );
    let mut rng = StdRng::seed_from_u64(1);
    let items = |rng: &mut StdRng| -> Vec<Item> {
        roll_in(&tables, "t:t", rng)
            .iter()
            .map(|s| s.item)
            .collect()
    };
    assert!(!items(&mut rng).contains(&Item::Stone));
    let pack = tuff_datapack::tags::vanilla_with(&[(
        "minecraft:item",
        "minecraft:wool_carpets",
        "minecraft:stone",
    )]);
    assert!(tuff_datapack::tags::sync_scope(pack, || items(&mut rng)).contains(&Item::Stone));
}

#[tuff_core::watchdog]
#[test]
fn trial_chamber_rewards_carry_their_components() {
    let mut rng = StdRng::seed_from_u64(7);
    let (mut heavy_core, mut enchanted, mut bottles) = (false, false, Vec::new());
    for _ in 0..400 {
        for stack in roll("minecraft:chests/trial_chambers/reward_ominous", &mut rng) {
            heavy_core |= stack.item == Item::HeavyCore;
            enchanted |= stack.is_enchanted() || stack.components.stored_enchantments.is_some();
            if stack.item == Item::OminousBottle {
                bottles.push(stack.ominous_bottle_amplifier().unwrap());
            }
        }
    }
    assert!(heavy_core && enchanted);
    assert!(!bottles.is_empty() && bottles.iter().all(|a| (2..=4).contains(a)));

    let drops = roll(
        "minecraft:spawners/trial_chamber/items_to_drop_when_ominous",
        &mut rng,
    );
    assert_eq!(drops.len(), 2);
    assert_eq!(drops[0].item, Item::LingeringPotion);
    assert!(
        drops[0]
            .components
            .potion_contents
            .as_ref()
            .unwrap()
            .potion
            .is_some()
    );
}

#[tuff_core::watchdog]
#[test]
fn trial_chamber_equipment_is_trimmed_and_enchanted() {
    let mut rng = StdRng::seed_from_u64(3);
    let mut trimmed = false;
    for _ in 0..100 {
        let drops = roll("minecraft:equipment/trial_chamber_ranged", &mut rng);
        assert!(drops.iter().any(|s| s.item == Item::Bow));
        for stack in drops.iter().filter(|s| s.item != Item::Bow) {
            trimmed |= stack.components.trim.is_some();
            assert!(stack.enchantment_level("minecraft:protection") == 4);
        }
    }
    assert!(trimmed);
}

#[tuff_core::watchdog]
#[test]
fn set_damage_leaves_the_rolled_share_of_durability() {
    let params = serde_json::json!({"damage": 0.25});
    let mut rng = StdRng::seed_from_u64(1);
    let stack = modify_stack(
        ItemStack::new(Item::Crossbow, 1),
        "minecraft:set_damage",
        &params,
        &bare_context(),
        &mut rng,
    );
    let max = stack.max_damage().unwrap();
    assert_eq!(stack.damage(), (0.75 * max as f32).floor() as u32);
    let book = modify_stack(
        ItemStack::new(Item::Book, 1),
        "minecraft:set_enchantments",
        &serde_json::json!({"enchantments": {"minecraft:wind_burst": 1}}),
        &bare_context(),
        &mut rng,
    );
    assert_eq!(book.item, Item::EnchantedBook);
    assert_eq!(
        book.components
            .stored_enchantments
            .as_ref()
            .unwrap()
            .level("minecraft:wind_burst"),
        1
    );
}

#[tuff_core::watchdog]
#[test]
fn set_stew_effect_appends_one_effect_in_ticks_unless_instant() {
    let params = serde_json::json!({"effects": [
        {"type": "minecraft:night_vision", "duration": 5},
        {"type": "minecraft:instant_health", "duration": 5}
    ]});
    let mut seen = std::collections::HashSet::new();
    let mut rng = StdRng::seed_from_u64(3);
    for _ in 0..40 {
        let stack = modify_stack(
            ItemStack::new(Item::SuspiciousStew, 1),
            "minecraft:set_stew_effect",
            &params,
            &bare_context(),
            &mut rng,
        );
        let effects = stack.components.suspicious_stew_effects.clone().unwrap();
        assert_eq!(effects.len(), 1);
        seen.insert((effects[0].effect.is_instantaneous(), effects[0].duration));
    }
    assert_eq!(seen, [(false, 100), (true, 5)].into_iter().collect());
    let other = modify_stack(
        ItemStack::new(Item::Bread, 1),
        "minecraft:set_stew_effect",
        &params,
        &bare_context(),
        &mut rng,
    );
    assert!(other.components.suspicious_stew_effects.is_none());
}

#[tuff_core::watchdog]
#[test]
fn set_random_dyes_dyes_one_item_and_ignores_non_positive_rolls() {
    let mut rng = StdRng::seed_from_u64(5);
    let dyed = modify_stack(
        ItemStack::new(Item::LeatherChestplate, 3),
        "minecraft:set_random_dyes",
        &serde_json::json!({"number_of_dyes": 2}),
        &bare_context(),
        &mut rng,
    );
    assert_eq!(dyed.count, 1);
    assert!(dyed.components.dyed_color.is_some());
    let plain = modify_stack(
        ItemStack::new(Item::LeatherChestplate, 3),
        "minecraft:set_random_dyes",
        &serde_json::json!({"number_of_dyes": 0}),
        &bare_context(),
        &mut rng,
    );
    assert_eq!(plain.count, 3);
    assert!(plain.components.dyed_color.is_none());
}

#[tuff_core::watchdog]
#[test]
fn tool_and_block_conditions_read_the_tags_in_effect() {
    use tuff_datapack::tags::{TagEntry, TagSet, vanilla_builders};
    let mut builders = vanilla_builders();
    for registry in ["minecraft:block", "minecraft:item"] {
        builders
            .get_mut(registry)
            .unwrap()
            .get_mut("minecraft:logs")
            .unwrap()
            .push(TagEntry {
                id: "minecraft:glass".to_owned(),
                tag: false,
                required: true,
            });
    }
    let tags = std::sync::Arc::new(TagSet::resolve_all(&builders).0);
    let glass = ItemStack::new(Item::Glass, 1);
    let check = || {
        (
            match_tool(
                &serde_json::json!({"items": "#minecraft:logs"}),
                Some(&glass),
            ),
            match_block(
                &serde_json::json!({"blocks": "#logs"}),
                Block::Glass.default_state(),
                None,
            ),
        )
    };
    assert_eq!(check(), (false, false));
    assert_eq!(tuff_datapack::tags::sync_scope(tags, check), (true, true));
}

fn block_drops(block: Block, tool: Option<Item>, rng: &mut impl Rng) -> Vec<ItemStack> {
    let tool = tool.map(|item| ItemStack::new(item, 1));
    super::block_drops(block.default_state(), tool.as_ref(), None, rng)
}

fn enchanted(item: Item, id: &str, level: i32) -> ItemStack {
    let mut stack = ItemStack::new(item, 1);
    stack.components.enchantments = Some(Enchantments::new([(format!("minecraft:{id}"), level)]));
    stack
}

#[tuff_core::watchdog]
#[test]
fn decorated_pots_keep_or_scatter_their_sherds() {
    use tuff_core::world::block_entity::raw;
    use tuff_vanilla_data::registry::BlockEntityType;
    let mut rng = StdRng::seed_from_u64(3);
    let mut nbt = raw::new(BlockEntityType::DecoratedPot, Default::default());
    let side = |id: &str| Tag::Compound(tuff_nbt::Compound::new().with("id", id));
    nbt.insert(
        "sherds",
        tuff_nbt::Compound::new()
            .with("back", side("minecraft:angler_pottery_sherd"))
            .with("left", side("minecraft:brick"))
            .with("front", side("minecraft:archer_pottery_sherd")),
    );
    let whole = super::block_drops(
        "minecraft:decorated_pot[cracked=false]".parse().unwrap(),
        None,
        Some(&nbt),
        &mut rng,
    );
    assert_eq!(whole.len(), 1);
    assert_eq!(whole[0].item, Item::DecoratedPot);
    assert!(
        whole[0]
            .components
            .other
            .contains_key("minecraft:pot_decorations")
    );
    let cracked = super::block_drops(
        "minecraft:decorated_pot[cracked=true]".parse().unwrap(),
        None,
        Some(&nbt),
        &mut rng,
    );
    let items: Vec<Item> = cracked.iter().map(|stack| stack.item).collect();
    assert_eq!(
        items,
        [
            Item::Brick,
            Item::AnglerPotterySherd,
            Item::ArcherPotterySherd
        ]
    );
}

#[tuff_core::watchdog]
#[test]
fn silk_touch_keeps_the_ore_block_itself() {
    let mut rng = StdRng::seed_from_u64(1);
    let pick = enchanted(Item::DiamondPickaxe, "silk_touch", 1);
    assert_eq!(
        super::block_drops(
            Block::DiamondOre.default_state(),
            Some(&pick),
            None,
            &mut rng
        ),
        vec![ItemStack::new(Item::DiamondOre, 1)]
    );
    let plain = ItemStack::new(Item::DiamondPickaxe, 1);
    assert_eq!(
        super::block_drops(
            Block::DiamondOre.default_state(),
            Some(&plain),
            None,
            &mut rng
        ),
        vec![ItemStack::new(Item::Diamond, 1)]
    );
}

#[tuff_core::watchdog]
#[test]
fn fortune_multiplies_ore_drops_up_to_level_plus_one() {
    let mut rng = StdRng::seed_from_u64(2);
    let pick = enchanted(Item::DiamondPickaxe, "fortune", 3);
    let mut counts = std::collections::HashSet::new();
    for _ in 0..500 {
        let drops = super::block_drops(
            Block::DiamondOre.default_state(),
            Some(&pick),
            None,
            &mut rng,
        );
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0].item, Item::Diamond);
        counts.insert(drops[0].count);
        assert!((1..=4).contains(&drops[0].count));
    }
    assert!(
        counts.len() > 1,
        "fortune should vary the count: {counts:?}"
    );
    let plain = ItemStack::new(Item::DiamondPickaxe, 1);
    for _ in 0..20 {
        let drops = super::block_drops(
            Block::DiamondOre.default_state(),
            Some(&plain),
            None,
            &mut rng,
        );
        assert_eq!(drops, vec![ItemStack::new(Item::Diamond, 1)]);
    }
}

#[tuff_core::watchdog]
#[test]
fn fortune_on_glowstone_stays_within_its_limit() {
    let mut rng = StdRng::seed_from_u64(20);
    let pick = enchanted(Item::DiamondPickaxe, "fortune", 3);
    let mut counts = std::collections::BTreeSet::new();
    for _ in 0..500 {
        let drops = super::block_drops(
            Block::Glowstone.default_state(),
            Some(&pick),
            None,
            &mut rng,
        );
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0].item, Item::GlowstoneDust);
        counts.insert(drops[0].count);
    }
    assert_eq!(counts, [2, 3, 4].into());
}

#[tuff_core::watchdog]
#[test]
fn looting_raises_rotten_flesh_by_up_to_its_level() {
    let mut rng = StdRng::seed_from_u64(21);
    let counts = |facts: &EntityFacts, rng: &mut StdRng| {
        (0..500)
            .map(|_| {
                super::entity_drops("minecraft:entities/zombie", facts, rng)
                    .iter()
                    .filter(|s| s.item == Item::RottenFlesh)
                    .map(|s| s.count)
                    .sum::<u8>()
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let plain = EntityFacts {
        kind: Some(tuff_vanilla_data::registry::EntityType::Zombie),
        ..Default::default()
    };
    assert_eq!(counts(&plain, &mut rng), [0, 1, 2].into());
    let looting = EntityFacts {
        attacker_weapon: Some(enchanted(Item::DiamondSword, "looting", 3)),
        ..plain
    };
    assert_eq!(counts(&looting, &mut rng), [0, 1, 2, 3, 4, 5].into());
}

#[tuff_core::watchdog]
#[test]
fn apply_bonus_formulas_match_their_math() {
    let mut rng = StdRng::seed_from_u64(3);
    let uniform: Value = serde_json::from_str(
        r#"{"formula":"minecraft:uniform_bonus_count","parameters":{"bonusMultiplier":2}}"#,
    )
    .unwrap();
    for _ in 0..200 {
        let value = super::apply_bonus(&uniform, 1, 3, &mut rng);
        assert!((1..=7).contains(&value), "got {value}");
    }
    let binomial: Value = serde_json::from_str(
        r#"{"formula":"minecraft:binomial_with_bonus_count","parameters":{"extra":0,"probability":1.0}}"#,
    )
    .unwrap();
    assert_eq!(super::apply_bonus(&binomial, 0, 4, &mut rng), 4);
    let ore: Value = serde_json::from_str(r#"{"formula":"minecraft:ore_drops"}"#).unwrap();
    assert_eq!(super::apply_bonus(&ore, 5, 0, &mut rng), 5);
}

#[tuff_core::watchdog]
#[test]
fn table_bonus_picks_the_chance_for_the_tool_level() {
    let mut rng = StdRng::seed_from_u64(4);
    let condition: Value =
        serde_json::from_str(r#"{"chances":[0.0,1.0],"enchantment":"minecraft:fortune"}"#).unwrap();
    let plain = ItemStack::new(Item::WoodenAxe, 1);
    for _ in 0..20 {
        assert!(!super::table_bonus(&condition, Some(&plain), &mut rng));
    }
    let enchanted_axe = enchanted(Item::WoodenAxe, "fortune", 1);
    for _ in 0..20 {
        assert!(super::table_bonus(
            &condition,
            Some(&enchanted_axe),
            &mut rng
        ));
    }
    // A level past the array clamps to the last entry instead of panicking.
    let high = enchanted(Item::WoodenAxe, "fortune", 99);
    assert!(super::table_bonus(&condition, Some(&high), &mut rng));
}

#[tuff_core::watchdog]
#[test]
fn looting_switches_between_the_unenchanted_and_enchanted_chance() {
    let mut rng = StdRng::seed_from_u64(5);
    let condition: Value = serde_json::from_str(
        r#"{"unenchanted_chance":0.0,"enchanted_chance":1.0,"enchantment":"minecraft:looting"}"#,
    )
    .unwrap();
    let facts_plain = EntityFacts::default();
    let context_plain = Context {
        tables: LootTables::builtin(),
        visiting: RefCell::default(),
        state: Block::Air.default_state(),
        entity: Some(&facts_plain),
        tool: None,
        explosion_radius: None,
        block_entity: None,
        location: None,
        world: None,
    };
    for _ in 0..20 {
        assert!(!super::random_chance_with_enchanted_bonus(
            &condition,
            &context_plain,
            &mut rng
        ));
    }
    let weapon = enchanted(Item::DiamondSword, "looting", 1);
    let facts_looting = EntityFacts {
        attacker_weapon: Some(weapon),
        ..Default::default()
    };
    let context_looting = Context {
        tables: LootTables::builtin(),
        visiting: RefCell::default(),
        state: Block::Air.default_state(),
        entity: Some(&facts_looting),
        tool: None,
        explosion_radius: None,
        block_entity: None,
        location: None,
        world: None,
    };
    for _ in 0..20 {
        assert!(super::random_chance_with_enchanted_bonus(
            &condition,
            &context_looting,
            &mut rng
        ));
    }
}

#[tuff_core::watchdog]
#[test]
fn world_blocks_drop_like_vanilla_by_hand() {
    let mut rng = StdRng::seed_from_u64(6);
    assert_eq!(
        block_drops(Block::Dirt, None, &mut rng),
        vec![ItemStack::new(Item::Dirt, 1)]
    );
    assert_eq!(
        block_drops(Block::GrassBlock, None, &mut rng),
        vec![ItemStack::new(Item::Dirt, 1)]
    );
    assert!(block_drops(Block::Stone, None, &mut rng).is_empty());
    assert_eq!(
        block_drops(Block::Stone, Some(Item::WoodenPickaxe), &mut rng),
        vec![ItemStack::new(Item::Cobblestone, 1)]
    );
    assert!(block_drops(Block::Bedrock, None, &mut rng).is_empty());
    assert_eq!(
        block_drops(Block::WallTorch, None, &mut rng),
        vec![ItemStack::new(Item::Torch, 1)]
    );
    assert_eq!(
        super::block_drops(
            "minecraft:oak_wall_sign".parse().unwrap(),
            None,
            None,
            &mut rng
        ),
        vec![ItemStack::new(Item::OakSign, 1)]
    );
}

#[tuff_core::watchdog]
#[test]
fn harvesting_needs_a_correct_tool_only_when_the_block_requires_one() {
    let mut rng = StdRng::seed_from_u64(7);
    assert!(block_drops(Block::IronOre, Some(Item::WoodenPickaxe), &mut rng).is_empty());
    assert_eq!(
        block_drops(Block::IronOre, Some(Item::StonePickaxe), &mut rng),
        vec![ItemStack::new(Item::RawIron, 1)]
    );
    assert!(block_drops(Block::Obsidian, Some(Item::IronPickaxe), &mut rng).is_empty());
    assert_eq!(
        block_drops(Block::Obsidian, Some(Item::DiamondPickaxe), &mut rng),
        vec![ItemStack::new(Item::Obsidian, 1)]
    );
    assert_eq!(
        block_drops(Block::Piston, None, &mut rng),
        vec![ItemStack::new(Item::Piston, 1)]
    );
}

#[tuff_core::watchdog]
#[test]
fn harvesting_honors_a_stacks_own_tool() {
    use tuff_item::item::gear::{BlockSet, Tool, ToolRule};
    let mut rng = StdRng::seed_from_u64(8);
    let mut stick = ItemStack::new(Item::Stick, 1);
    stick.components.tool = Some(Box::new(Tool {
        rules: vec![ToolRule {
            blocks: BlockSet::tag("minecraft:mineable/pickaxe").unwrap(),
            speed: Some(3.0),
            correct_for_drops: Some(true),
        }],
        default_mining_speed: 1.0,
        damage_per_block: 1,
        can_destroy_blocks_in_creative: true,
    }));
    let obsidian = Block::Obsidian.default_state();
    assert!(can_harvest(obsidian, Some(&stick)));
    assert_eq!(
        super::block_drops(obsidian, Some(&stick), None, &mut rng),
        vec![ItemStack::new(Item::Obsidian, 1)]
    );
    let mut blunt = ItemStack::new(Item::DiamondPickaxe, 1);
    blunt.components.removed = vec!["minecraft:tool".into()];
    assert!(!can_harvest(obsidian, Some(&blunt)));
    let mut sword = ItemStack::new(Item::DiamondSword, 1);
    assert!(!sword.can_destroy_blocks_in_creative());
    sword.components.removed = vec!["minecraft:tool".into()];
    assert!(sword.can_destroy_blocks_in_creative());
    let stone = Block::Stone.default_state();
    let plain = tuff_core::world::player::Miner::plain(true);
    assert_eq!(
        tuff_core::world::player::destroy_progress(stone, Some(&stick), &plain, false),
        3.0 / 1.5 / 30.0
    );
    assert_eq!(
        tuff_core::world::player::destroy_progress(stone, Some(&blunt), &plain, false),
        1.0 / 1.5 / 100.0
    );
}

#[tuff_core::watchdog]
#[test]
fn drops_follow_block_state_and_tool_conditions() {
    let mut rng = StdRng::seed_from_u64(9);
    let drops = |state: &str, tool: Option<Item>, rng: &mut _| {
        let tool = tool.map(|item| ItemStack::new(item, 1));
        super::block_drops(state.parse().unwrap(), tool.as_ref(), None, rng)
    };
    assert_eq!(
        drops("minecraft:oak_door[half=lower]", None, &mut rng),
        vec![ItemStack::new(Item::OakDoor, 1)]
    );
    assert!(drops("minecraft:oak_door[half=upper]", None, &mut rng).is_empty());
    assert!(drops("minecraft:red_bed[part=foot]", None, &mut rng).is_empty());
    assert_eq!(
        drops("minecraft:red_bed[part=head]", None, &mut rng),
        vec![ItemStack::new(Item::RedBed, 1)]
    );
    assert_eq!(
        drops("minecraft:oak_slab[type=double]", None, &mut rng),
        vec![ItemStack::new(Item::OakSlab, 2)]
    );
    assert_eq!(
        drops("minecraft:leaf_litter[segment_amount=3]", None, &mut rng),
        vec![ItemStack::new(Item::LeafLitter, 3)]
    );
    assert_eq!(
        drops("minecraft:short_grass", Some(Item::Shears), &mut rng),
        vec![ItemStack::new(Item::ShortGrass, 1)]
    );
    assert!(
        drops("minecraft:short_grass", None, &mut rng)
            .iter()
            .all(|stack| stack.item == Item::WheatSeeds)
    );
}

/// A pool roll picks one weighted entry, not one item per listed entry:
/// a table roll can never produce more stacks than the sum of its
/// pools' declared roll counts, no matter how many entries a pool
/// lists (`minecraft:chests/simple_dungeon` lists 10-12 entries per
/// pool, yet each roll still yields one item).
#[tuff_core::watchdog]
#[test]
fn a_pool_roll_produces_one_stack_not_one_per_entry() {
    let table = LootTable::get("minecraft:chests/simple_dungeon").unwrap();
    assert!(
        table.pools.iter().all(|pool| pool.entries.len() > 1),
        "fixture assumption: every pool lists more than one entry"
    );
    let max_rolls: u32 = table
        .pools
        .iter()
        .map(|pool| match &pool.rolls {
            NumberProvider::Constant(value) => *value as u32,
            NumberProvider::Uniform { max, .. } => *max as u32,
            other => panic!("unexpected rolls provider {other:?}"),
        })
        .sum();
    let context = Context {
        tables: LootTables::builtin(),
        visiting: RefCell::default(),
        state: Block::Air.default_state(),
        entity: None,
        tool: None,
        explosion_radius: None,
        block_entity: None,
        location: None,
        world: None,
    };
    let mut rng = rand::thread_rng();
    for _ in 0..500 {
        let mut drops = Vec::new();
        roll_table(table, &context, &mut rng, &mut drops);
        assert!(
            drops.len() as u32 <= max_rolls,
            "rolled {} stacks, more than the table's {max_rolls} total declared rolls",
            drops.len()
        );
    }
}

/// `number` (a pool's `rolls`/`bonus_rolls` provider) always stays
/// within the provider's own declared bounds.
#[tuff_core::watchdog]
#[test]
fn roll_counts_stay_within_their_declared_range() {
    let mut rng = rand::thread_rng();
    for _ in 0..500 {
        assert_eq!(number(&NumberProvider::Constant(3.0), &mut rng), 3);
        let n = number(&NumberProvider::Uniform { min: 1.0, max: 4.0 }, &mut rng);
        assert!((1..=4).contains(&n), "uniform(1,4) produced {n}");
    }
}

#[tuff_core::watchdog]
#[test]
fn bonus_rolls_scale_with_luck() {
    let pool = LootPool {
        rolls: NumberProvider::Constant(1.0),
        bonus_rolls: NumberProvider::Constant(2.0),
        condition: None,
        functions: Frozen::Static(&[]),
        entries: Frozen::Static(&[]),
    };
    let mut rng = rand::thread_rng();
    assert_eq!(pool_rolls(&pool, 0.0, &mut rng), 1);
    assert_eq!(pool_rolls(&pool, 1.0, &mut rng), 3);
    assert_eq!(pool_rolls(&pool, 1.5, &mut rng), 4);
    assert_eq!(pool_rolls(&pool, -3.0, &mut rng), 0);
}

/// A big splittable stack is broken into more, smaller stacks to fill
/// a tight container, but conserves its total item count.
#[tuff_core::watchdog]
#[test]
fn fill_container_slots_splits_large_stacks_to_use_available_room() {
    let mut rng = rand::thread_rng();
    for _ in 0..100 {
        let drops = vec![ItemStack::new(Item::Bone, 20)];
        let result = fill_container_slots(drops, 6, &mut rng);
        let filled: Vec<&ItemStack> = result.iter().flatten().collect();
        assert!(
            filled.len() > 1,
            "a count-20 stack into 6 slots should split, got {filled:?}"
        );
        let total: u32 = filled.iter().map(|s| s.count as u32).sum();
        assert_eq!(total, 20, "splitting must conserve the total item count");
        assert!(filled.iter().all(|s| s.item == Item::Bone));
    }
}

/// Stacks that are already a single item are never split, no matter
/// how much room the container has: the split loop only ever draws
/// from stacks with `count > 1`.
#[tuff_core::watchdog]
#[test]
fn fill_container_slots_never_splits_stacks_already_at_one() {
    let mut rng = rand::thread_rng();
    let drops = vec![
        ItemStack::new(Item::Bread, 1),
        ItemStack::new(Item::IronIngot, 1),
    ];
    let result = fill_container_slots(drops.clone(), 27, &mut rng);
    let mut filled: Vec<ItemStack> = result.into_iter().flatten().collect();
    filled.sort_by_key(|s| s.item.name());
    let mut expected = drops;
    expected.sort_by_key(|s| s.item.name());
    assert_eq!(filled, expected);
}

/// Rolling a real chest table into a full-size container leaves most
/// slots empty most of the time, matching vanilla dungeon/village
/// chests, instead of always packing every slot.
#[tuff_core::watchdog]
#[test]
fn container_loot_usually_leaves_a_chest_mostly_empty() {
    let name = "minecraft:chests/simple_dungeon";
    let slots = 27;
    let table = LootTable::get(name).unwrap();
    // The expected number of root picks (one per roll, win or lose to
    // splitting) the table's own rolls declare, per full table roll.
    let expected_root_items: f64 = table.pools.iter().map(|pool| pool.rolls.mean()).sum();

    let trials = 300;
    let mut total_filled = 0usize;
    let mut fully_packed = 0usize;
    let mut distinct_fill_counts = std::collections::HashSet::new();
    for seed in 0..trials {
        let mut rng = StdRng::seed_from_u64(seed as u64);
        let items = container_loot(name, slots, &mut rng).unwrap();
        let filled = items.iter().filter(|s| s.is_some()).count();
        total_filled += filled;
        distinct_fill_counts.insert(filled);
        if filled == slots {
            fully_packed += 1;
        }
    }
    let avg_filled = total_filled as f64 / trials as f64;

    assert!(
        fully_packed == 0,
        "{fully_packed} of {trials} rolls packed every one of the {slots} slots; \
         vanilla dungeon chests are rarely, if ever, completely full"
    );
    // Splitting can multiply the root picks into more (smaller) stacks,
    // but not without bound: staying well under 3x the table's own
    // expected root-item count keeps this grounded in the table's
    // declared rolls rather than an arbitrary slot-count fraction.
    assert!(
        avg_filled < expected_root_items * 3.0,
        "average filled slots {avg_filled} is implausibly high for a table \
         whose pools declare only {expected_root_items} expected picks per roll"
    );
    assert!(
        distinct_fill_counts.len() > 1,
        "every trial filled the same number of slots; loot should vary"
    );
}

#[tuff_core::watchdog]
#[test]
fn creepers_drop_music_discs_only_when_a_skeleton_killed_them() {
    use tuff_vanilla_data::registry::EntityType;

    let creeper = |attacker| EntityFacts {
        kind: Some(EntityType::Creeper),
        attacker_kind: attacker,
        ..Default::default()
    };
    let discs = |attacker| {
        (0..40)
            .flat_map(|seed| {
                let mut rng = StdRng::seed_from_u64(seed);
                super::entity_drops("minecraft:entities/creeper", &creeper(attacker), &mut rng)
            })
            .filter(|stack| stack.item.name().contains("music_disc"))
            .count()
    };
    assert_eq!(discs(Some(EntityType::Zombie)), 0);
    assert_eq!(discs(None), 0);
    assert_eq!(discs(Some(EntityType::Skeleton)), 40);
    assert_eq!(discs(Some(EntityType::Stray)), 40);
}

#[tuff_core::watchdog]
#[test]
fn built_in_tag_entries_draw_from_pack_tag_members() {
    use tuff_vanilla_data::registry::EntityType;

    let creeper = EntityFacts {
        kind: Some(EntityType::Creeper),
        attacker_kind: Some(EntityType::Skeleton),
        ..Default::default()
    };
    let stones = || {
        (0..200)
            .flat_map(|seed| {
                let mut rng = StdRng::seed_from_u64(seed);
                super::entity_drops("minecraft:entities/creeper", &creeper, &mut rng)
            })
            .filter(|stack| stack.item == Item::Stone)
            .count()
    };
    assert_eq!(stones(), 0);
    let pack = tuff_datapack::tags::vanilla_with(&[(
        "minecraft:item",
        "minecraft:creeper_drop_music_discs",
        "minecraft:stone",
    )]);
    assert!(tuff_datapack::tags::sync_scope(pack, stones) > 0);
}

#[tuff_core::watchdog]
#[test]
fn furnace_smelt_follows_the_servers_recipes() {
    use std::sync::Arc;
    use tuff_core::world::crafting::{Recipe, RecipeSet};
    use tuff_feature_builtin_vanilla_recipes::crafting::Cooking;
    use tuff_vanilla_data::recipe::Ingredient;
    use tuff_vanilla_data::registry::EntityType;

    let chicken = |on_fire, recipes| EntityFacts {
        kind: Some(EntityType::Chicken),
        on_fire,
        recipes,
        ..Default::default()
    };
    let meat = |facts: &EntityFacts| {
        let mut rng = StdRng::seed_from_u64(1);
        super::entity_drops("minecraft:entities/chicken", facts, &mut rng)
            .into_iter()
            .map(|stack| stack.item)
            .find(|item| *item != Item::Feather)
    };
    assert_eq!(meat(&chicken(false, None)), Some(Item::Chicken));
    assert_eq!(meat(&chicken(true, None)), Some(Item::CookedChicken));

    let charcoal = Cooking::new(
        "test:burnt_chicken",
        CookingStation::Furnace,
        Ingredient::Item(Item::Chicken),
        Item::Charcoal,
    );
    let recipes: Vec<(Frozen<str>, Arc<dyn Recipe>)> =
        vec![(charcoal.name.clone(), Arc::new(charcoal))];
    let recipes = Some(Arc::new(RecipeSet::new(recipes)));
    assert_eq!(meat(&chicken(true, recipes.clone())), Some(Item::Charcoal));
    assert_eq!(
        meat(&chicken(true, Some(Arc::new(RecipeSet::default())))),
        Some(Item::Chicken),
        "a server without smelting recipes smelts nothing"
    );
    assert_eq!(meat(&chicken(false, recipes)), Some(Item::Chicken));
}

#[tuff_core::watchdog]
#[test]
fn a_pack_table_that_reaches_itself_stops_and_overrides_apply() {
    use tuff_datapack::loot::{Predicates, parse};
    use tuff_datapack::tags::TagSet;
    let looping = parse(
        br#"{"pools":[{"rolls":1,"entries":[{"type":"item","name":"diamond"}]},
            {"rolls":1,"entries":[{"type":"loot_table","value":"a:loop"}]}]}"#,
        &TagSet::vanilla(),
        &Predicates::default(),
    )
    .unwrap();
    let stone = parse(
        br#"{"pools":[{"rolls":2,"entries":[{"type":"item","name":"emerald"}]}]}"#,
        &TagSet::vanilla(),
        &Predicates::default(),
    )
    .unwrap();
    let tables = LootTables::new(std::collections::HashMap::from([
        ("a:loop".to_owned(), Some(looping)),
        ("minecraft:blocks/stone".to_owned(), Some(stone)),
        ("minecraft:blocks/dirt".to_owned(), None),
    ]));
    let mut rng = StdRng::seed_from_u64(3);
    assert_eq!(
        roll_in(&tables, "a:loop", &mut rng),
        [ItemStack::new(Item::Diamond, 1)]
    );
    assert_eq!(
        block_drops_in(&tables, Block::Stone.default_state(), None, None, &mut rng),
        []
    );
    let pickaxe = ItemStack::new(Item::WoodenPickaxe, 1);
    assert_eq!(
        block_drops_in(
            &tables,
            Block::Stone.default_state(),
            Some(&pickaxe),
            None,
            &mut rng
        ),
        [
            ItemStack::new(Item::Emerald, 1),
            ItemStack::new(Item::Emerald, 1)
        ]
    );
    assert!(block_drops_in(&tables, Block::Dirt.default_state(), None, None, &mut rng).is_empty());
    assert_eq!(
        super::block_drops(Block::Dirt.default_state(), None, None, &mut rng),
        [ItemStack::new(Item::Dirt, 1)]
    );
}

#[tuff_core::watchdog]
#[test]
fn damage_source_properties_reads_the_killing_blow() {
    let facts = EntityFacts {
        damage_kind: Some(&tuff_vanilla_data::damage_type::PLAYER_ATTACK),
        damage_direct: true,
        ..Default::default()
    };
    let predicate = serde_json::json!({"is_direct": true});
    assert!(super::damage_source_properties(&predicate, &facts));
    let predicate = serde_json::json!({"is_direct": false});
    assert!(!super::damage_source_properties(&predicate, &facts));
    let none = EntityFacts::default();
    assert!(!super::damage_source_properties(
        &serde_json::json!({}),
        &none
    ));
}

#[tuff_core::watchdog]
#[test]
fn location_and_value_checks_gate_entries() {
    use tuff_core::world::predicate::LocationFacts;
    use tuff_datapack::loot::{Predicates, parse};
    use tuff_datapack::tags::TagSet;
    let table = |condition: &str| {
        let json = format!(
            r#"{{"pools":[{{"rolls":1,"entries":[{{"type":"item","name":"emerald","condition":{condition}}}]}}]}}"#
        );
        let parsed = parse(json.as_bytes(), &TagSet::vanilla(), &Predicates::default()).unwrap();
        LootTables::new(std::collections::HashMap::from([(
            "minecraft:blocks/stone".to_owned(),
            Some(parsed),
        )]))
    };
    let plains = LocationFacts {
        biome: Some("minecraft:plains".into()),
        ..Default::default()
    };
    let drops = |tables: &LootTables, at: &LocationFacts| {
        block_drops_located(
            tables,
            Block::Stone.default_state(),
            Some(&ItemStack::new(Item::WoodenPickaxe, 1)),
            None,
            at,
            None,
            &mut StdRng::seed_from_u64(1),
        )
        .len()
    };
    let here =
        table(r#"{"type":"minecraft:location_check","predicate":{"biomes":"minecraft:plains"}}"#);
    assert_eq!(drops(&here, &plains), 1);
    let desert = LocationFacts {
        biome: Some("minecraft:desert".into()),
        ..Default::default()
    };
    assert_eq!(drops(&here, &desert), 0);
    let ints = table(r#"{"type":"minecraft:int_value_check","value":5,"test":{"min":3,"max":4}}"#);
    assert_eq!(drops(&ints, &plains), 0);
    let floats = table(r#"{"type":"minecraft:float_value_check","value":0.5,"test":{"min":0.25}}"#);
    assert_eq!(drops(&floats, &plains), 1);
}

fn modified(modifier: &str, stack: ItemStack) -> ItemStack {
    let value: Value = serde_json::from_str(modifier).unwrap();
    let functions = tuff_datapack::loot::parse_item_modifier(
        &value,
        &tuff_datapack::loot::Predicates::vanilla(),
    )
    .unwrap();
    modify_item(&functions, stack, &mut StdRng::seed_from_u64(3))
}

fn component<'a>(stack: &'a ItemStack, id: &str) -> Option<&'a Tag> {
    stack.components.other.get(id)
}

#[tuff_core::watchdog]
#[test]
fn pillager_outpost_goat_horns_get_an_instrument() {
    let mut rng = StdRng::seed_from_u64(11);
    let horns: Vec<ItemStack> = (0..300)
        .flat_map(|_| roll("minecraft:chests/pillager_outpost", &mut rng))
        .filter(|s| s.item == Item::GoatHorn)
        .collect();
    assert!(!horns.is_empty());
    assert!(
        horns
            .iter()
            .all(|h| component(h, "minecraft:instrument").is_some())
    );
}

#[tuff_core::watchdog]
#[test]
fn lore_and_custom_data_functions_edit_components() {
    let stack = modified(
        r##"[{"type":"minecraft:set_lore","lore":["a","b"],"mode":"replace_all"},
            {"type":"minecraft:set_lore","lore":["c"],"mode":"append"},
            {"type":"minecraft:set_custom_data","tag":{"x":1}},
            {"type":"minecraft:set_custom_data","tag":{"y":2}}]"##,
        ItemStack::new(Item::Stick, 1),
    );
    let lore = component(&stack, "minecraft:lore")
        .and_then(Tag::as_list)
        .unwrap();
    assert_eq!(lore.len(), 3);
    let data = component(&stack, "minecraft:custom_data")
        .and_then(Tag::as_compound)
        .unwrap();
    assert_eq!((data.get_int("x"), data.get_int("y")), (Some(1), Some(2)));
}

#[tuff_core::watchdog]
#[test]
fn set_lore_is_capped_at_256_lines() {
    let lines = vec!["\"x\""; 300].join(",");
    let stack = modified(
        &format!(r##"[{{"type":"minecraft:set_lore","lore":[{lines}],"mode":"replace_all"}}]"##),
        ItemStack::new(Item::Stick, 1),
    );
    let lore = component(&stack, "minecraft:lore")
        .and_then(Tag::as_list)
        .unwrap();
    assert_eq!(lore.len(), 256);
}

#[tuff_core::watchdog]
#[test]
fn filtered_applies_the_branch_the_item_takes_and_discard_empties() {
    let modifier = r##"{"type":"minecraft:filtered","item_filter":{"items":"minecraft:stick"},
        "on_pass":{"type":"minecraft:set_custom_data","tag":{"p":1}},
        "on_fail":{"type":"minecraft:discard"}}"##;
    let pass = modified(modifier, ItemStack::new(Item::Stick, 1));
    assert!(component(&pass, "minecraft:custom_data").is_some());
    let fail = modified(modifier, ItemStack::new(Item::Stone, 1));
    assert_eq!(fail.count, 0);
}

#[tuff_core::watchdog]
#[test]
fn contents_attributes_and_model_data_are_set() {
    let stack = modified(
        r##"[{"type":"minecraft:set_contents","component":"minecraft:bundle_contents",
              "entries":[{"type":"minecraft:item","name":"minecraft:apple"}]},
            {"type":"minecraft:set_custom_model_data","floats":[1.5],"strings":{"values":["s"],"mode":"append"}},
            {"type":"minecraft:set_loot_table","name":"minecraft:chests/x"}]"##,
        ItemStack::new(Item::Bundle, 1),
    );
    assert!(stack.components.bundle_contents.is_some());
    let cmd = component(&stack, "minecraft:custom_model_data")
        .and_then(Tag::as_compound)
        .unwrap();
    assert_eq!(cmd.get_list("floats").map(<[Tag]>::len), Some(1));
    assert!(component(&stack, "minecraft:container_loot").is_some());
}

fn bare_context() -> Context<'static> {
    Context {
        tables: LootTables::builtin(),
        visiting: RefCell::default(),
        state: Block::Air.default_state(),
        entity: None,
        tool: None,
        explosion_radius: None,
        block_entity: None,
        location: None,
        world: None,
    }
}

#[tuff_core::watchdog]
#[test]
fn copy_custom_data_copies_block_entity_paths() {
    let source = Compound::new().with("a", Compound::new().with("b", 7));
    let mut context = bare_context();
    context.block_entity = Some(&source);
    let params: Value = serde_json::from_str(
        r#"{"source":{"type":"minecraft:context","target":"block_entity"},
            "ops":[{"source":"a.b","target":"c.d","op":"replace"}]}"#,
    )
    .unwrap();
    let stack = modify_stack(
        ItemStack::new(Item::Stick, 1),
        "minecraft:copy_custom_data",
        &params,
        &context,
        &mut StdRng::seed_from_u64(1),
    );
    let data = component(&stack, "minecraft:custom_data")
        .and_then(Tag::as_compound)
        .unwrap();
    assert_eq!(data.get_compound("c").and_then(|c| c.get_int("d")), Some(7));
}

#[tuff_core::watchdog]
#[test]
fn copy_state_copies_listed_properties_of_the_matching_block() {
    let state: tuff_vanilla_data::BlockState = "minecraft:bee_nest[honey_level=5]".parse().unwrap();
    let mut context = bare_context();
    context.state = state;
    let run = |params: &str, context: &Context| {
        modify_stack(
            ItemStack::new(Item::BeeNest, 1),
            "minecraft:copy_state",
            &serde_json::from_str(params).unwrap(),
            context,
            &mut StdRng::seed_from_u64(1),
        )
    };
    let stack = run(
        r#"{"block":"minecraft:bee_nest","properties":["honey_level","nope"]}"#,
        &context,
    );
    let copied = component(&stack, "minecraft:block_state")
        .and_then(Tag::as_compound)
        .unwrap();
    assert_eq!(copied.get_str("honey_level"), Some("5"));
    assert_eq!(copied.len(), 1);
    let other = run(
        r#"{"block":"minecraft:beehive","properties":["honey_level"]}"#,
        &context,
    );
    assert!(component(&other, "minecraft:block_state").is_none());
}

#[tuff_core::watchdog]
#[test]
fn copy_custom_data_reads_storage_and_shorthand_context_sources() {
    let world = SkyWorld;
    let mut context = bare_context();
    context.world = Some(&world);
    let run = |source: &str, context: &Context| {
        let params: Value = serde_json::from_str(&format!(
            r#"{{"source":{source},"ops":[{{"source":"a.b","target":"c","op":"replace"}}]}}"#
        ))
        .unwrap();
        modify_stack(
            ItemStack::new(Item::Stick, 1),
            "minecraft:copy_custom_data",
            &params,
            context,
            &mut StdRng::seed_from_u64(1),
        )
    };
    let stack = run(r#"{"type":"minecraft:storage","source":"loot"}"#, &context);
    let data = component(&stack, "minecraft:custom_data")
        .and_then(Tag::as_compound)
        .unwrap();
    assert_eq!(data.get_int("c"), Some(9));
    let missing = run(r#"{"type":"minecraft:storage","source":"other"}"#, &context);
    assert!(component(&missing, "minecraft:custom_data").is_none());
    let entity = Compound::new().with("a", Compound::new().with("b", 4));
    context.block_entity = Some(&entity);
    let stack = run(r#""block_entity""#, &context);
    let data = component(&stack, "minecraft:custom_data")
        .and_then(Tag::as_compound)
        .unwrap();
    assert_eq!(data.get_int("c"), Some(4));
}

#[tuff_core::watchdog]
#[test]
fn slot_entries_provide_container_slots_filtered_limited_and_stored() {
    let items = |stacks: &[(i8, Item, u8)]| {
        Tag::List(
            stacks
                .iter()
                .map(|(slot, item, count)| {
                    let mut nbt = ItemStack::new(*item, *count).to_nbt();
                    nbt.insert("Slot", Tag::Byte(*slot));
                    Tag::Compound(nbt)
                })
                .collect(),
        )
    };
    let chest = Compound::new().with(
        "Items",
        items(&[
            (3, Item::Stick, 2),
            (0, Item::Apple, 4),
            (1, Item::Stick, 1),
        ]),
    );
    let stored: std::collections::BTreeMap<String, (String, Vec<u8>)> = [(
        "minecraft:all".to_owned(),
        (
            "pack".to_owned(),
            br#"{"type":"minecraft:slot_range","source":"block_entity","slots":"container.*"}"#
                .to_vec(),
        ),
    )]
    .into();
    let sources = tuff_datapack::slot_source::SlotSources::new(stored, &|name| {
        tuff_core::world::predicate::slot_range(name).is_some()
    });
    let tables =
        LootTables::new(Default::default()).with_slot_sources(std::sync::Arc::new(sources));
    let mut context = bare_context();
    context.tables = &tables;
    context.block_entity = Some(&chest);
    let provide = |slot_source: &str| {
        super::slots::provide(&format!(r#"{{"slot_source":{slot_source}}}"#), &context)
    };
    assert_eq!(
        provide(r#""all""#),
        vec![
            ItemStack::new(Item::Apple, 4),
            ItemStack::new(Item::Stick, 1),
            ItemStack::new(Item::Stick, 2)
        ]
    );
    assert_eq!(
        provide(r#"{"type":"minecraft:limit_slots","limit":2,"slot_source":"all"}"#),
        vec![
            ItemStack::new(Item::Apple, 4),
            ItemStack::new(Item::Stick, 1)
        ]
    );
    assert_eq!(
        provide(
            r#"{"type":"minecraft:filtered","item_filter":{"items":"minecraft:stick"},"slot_source":{"type":"minecraft:slot_range","source":"block_entity","slots":"container.3"}}"#
        ),
        vec![ItemStack::new(Item::Stick, 2)]
    );
    assert!(
        provide(r#"{"type":"minecraft:slot_range","source":"this","slots":"container.*"}"#)
            .is_empty()
    );
    assert!(provide(r#""missing""#).is_empty());

    let entry = LootEntry {
        kind: LootEntryKind::Unknown {
            kind: Frozen::Static("minecraft:slots"),
            raw: Frozen::Static(r#"{"type":"minecraft:slots","slot_source":"all"}"#),
        },
        weight: 1,
        quality: 0,
        condition: None,
        functions: Frozen::Static(&[]),
    };
    let mut drops = Vec::new();
    create_entry_leaf(&entry, &context, &mut StdRng::seed_from_u64(1), &mut drops);
    assert_eq!(drops.len(), 3);
}

#[tuff_core::watchdog]
#[test]
fn a_sequence_runs_its_functions_and_fill_player_head_names_the_profile() {
    let mut rng = StdRng::seed_from_u64(1);
    let stack = modified(
        r#"{"type":"minecraft:sequence","functions":[{"type":"minecraft:set_custom_data","tag":{"a":1}}]}"#,
        ItemStack::new(Item::Stick, 1),
    );
    assert!(component(&stack, "minecraft:custom_data").is_some());

    let facts = EntityFacts {
        player_name: Some("Steve".into()),
        ..Default::default()
    };
    let mut context = bare_context();
    context.entity = Some(&facts);
    let stack = modify_stack(
        ItemStack::new(Item::PlayerHead, 1),
        "minecraft:fill_player_head",
        &Value::Null,
        &context,
        &mut rng,
    );
    let profile = component(&stack, "minecraft:profile")
        .and_then(Tag::as_compound)
        .unwrap();
    assert_eq!(profile.get_str("name"), Some("Steve"));
}

#[tuff_core::watchdog]
#[test]
fn pool_level_functions_set_item_count_and_name() {
    let mut rng = StdRng::seed_from_u64(1);
    let context = bare_context();
    let stack = |kind: &str, params: serde_json::Value, s: ItemStack, rng: &mut StdRng| {
        modify_stack(s, kind, &params, &context, rng)
    };
    let swapped = stack(
        "minecraft:set_item",
        serde_json::json!({"item": "minecraft:diamond"}),
        ItemStack::new(Item::Stick, 3),
        &mut rng,
    );
    assert_eq!((swapped.item, swapped.count), (Item::Diamond, 3));
    let counted = stack(
        "minecraft:set_count",
        serde_json::json!({"count": 5, "add": true}),
        swapped,
        &mut rng,
    );
    assert_eq!(counted.count, 8);
    let limited = stack(
        "minecraft:limit_count",
        serde_json::json!({"limit": {"max": 4}}),
        counted,
        &mut rng,
    );
    assert_eq!(limited.count, 4);
    let named = stack(
        "minecraft:set_name",
        serde_json::json!({"name": "Hi"}),
        limited,
        &mut rng,
    );
    assert!(
        named
            .components
            .to_nbt()
            .get("minecraft:custom_name")
            .is_some()
    );
}

#[tuff_core::watchdog]
#[test]
fn set_damage_add_uses_integer_division_for_the_base() {
    let mut stack = ItemStack::new(Item::Crossbow, 1);
    let max = stack.max_damage().unwrap();
    stack.set_damage(max / 2);
    let mut rng = StdRng::seed_from_u64(1);
    let out = modify_stack(
        stack,
        "minecraft:set_damage",
        &serde_json::json!({"damage": 0.25, "add": true}),
        &bare_context(),
        &mut rng,
    );
    assert_eq!(out.damage(), 0);
}

#[tuff_core::watchdog]
#[test]
fn set_random_potion_picks_from_the_options_or_the_whole_registry() {
    let mut rng = StdRng::seed_from_u64(3);
    for _ in 0..20 {
        let out = modify_stack(
            ItemStack::new(Item::Potion, 1),
            "minecraft:set_random_potion",
            &serde_json::json!({"options": ["minecraft:swiftness", "minecraft:healing"]}),
            &bare_context(),
            &mut rng,
        );
        let potion = out
            .components
            .potion_contents
            .as_ref()
            .unwrap()
            .potion
            .unwrap();
        assert!(matches!(
            potion.name(),
            "minecraft:swiftness" | "minecraft:healing"
        ));
    }
    let any = modify_stack(
        ItemStack::new(Item::Potion, 1),
        "minecraft:set_random_potion",
        &serde_json::json!({}),
        &bare_context(),
        &mut rng,
    );
    assert!(
        any.components
            .potion_contents
            .as_ref()
            .unwrap()
            .potion
            .is_some()
    );
}

struct SkyWorld;

impl tuff_core::world::predicate::PredicateWorld for SkyWorld {
    fn location(&self, position: [f64; 3]) -> LocationFacts {
        LocationFacts {
            position,
            ..Default::default()
        }
    }
    fn raining(&self) -> bool {
        true
    }
    fn thundering(&self) -> bool {
        false
    }
    fn clock(&self, id: &str) -> Option<i64> {
        (id == "minecraft:overworld").then_some(24_100)
    }
    fn score(&self, holder: &str, objective: &str) -> Option<i32> {
        (holder == "Steve" && objective == "coins").then_some(7)
    }
    fn team(&self, _holder: &str) -> Option<&str> {
        None
    }
    fn random(&self) -> f32 {
        0.0
    }
    fn random_int(&self, min: i64, _max: i64) -> i64 {
        min
    }
    fn storage(&self, id: &str) -> Option<Compound> {
        (id == "minecraft:loot").then(|| Compound::new().with("a", Compound::new().with("b", 9)))
    }
    fn attribute(&self, key: &str, _position: [f64; 3]) -> Option<Value> {
        (key == "minecraft:gameplay/can_start_raid").then_some(Value::Bool(true))
    }
}

#[tuff_core::watchdog]
#[test]
fn world_conditions_read_the_level_when_one_is_given() {
    let check = |context: &Context, json: &str| {
        let value: Value = serde_json::from_str(json).unwrap();
        json_condition(&value, context, &mut StdRng::seed_from_u64(1))
    };
    let weather = r#"{"type":"minecraft:weather_check","raining":true,"thundering":false}"#;
    let time = r#"{"type":"minecraft:time_check","clock":"minecraft:overworld","period":24000,"value":{"min":0,"max":200}}"#;
    let attribute = r#"{"type":"minecraft:environment_attribute_check","attribute":"minecraft:gameplay/can_start_raid","value":true}"#;
    let scores =
        r#"{"type":"minecraft:entity_scores","entity":"this","scores":{"coins":{"min":5}}}"#;
    let mut context = bare_context();
    for json in [weather, time, attribute, scores] {
        assert!(!check(&context, json));
    }
    let world = SkyWorld;
    context.world = Some(&world);
    for json in [weather, time, attribute] {
        assert!(check(&context, json));
    }
    assert!(!check(&context, scores));
    let steve = EntityFacts {
        score_holder: Some("Steve".into()),
        ..Default::default()
    };
    context.entity = Some(&steve);
    assert!(check(&context, scores));
    let high = scores.replace("\"min\":5", "\"min\":8");
    assert!(!check(&context, &high));
}

#[tuff_core::watchdog]
#[test]
fn score_providers_read_this_or_a_fixed_name_with_a_fallback() {
    let read = |context: &Context, json: &str| {
        let value: Value = serde_json::from_str(json).unwrap();
        json_int(
            &value,
            &mut StdRng::seed_from_u64(1),
            context.scores().as_ref(),
        )
    };
    let this = r#"{"type":"minecraft:score","target":"this","score":"coins","fallback":3}"#;
    let fixed = r#"{"type":"minecraft:score","target":{"type":"minecraft:fixed","name":"Steve"},"score":"coins"}"#;
    let missing = r#"{"type":"minecraft:score","target":{"type":"minecraft:fixed","name":"Alex"},"score":"coins"}"#;
    let mut context = bare_context();
    assert_eq!(read(&context, fixed), None);
    let world = SkyWorld;
    context.world = Some(&world);
    assert_eq!(read(&context, fixed), Some(7));
    assert_eq!(read(&context, missing), Some(0));
    assert_eq!(read(&context, this), Some(3));
    let steve = EntityFacts {
        score_holder: Some("Steve".into()),
        ..Default::default()
    };
    context.entity = Some(&steve);
    assert_eq!(read(&context, this), Some(7));
}

fn stone_drops_of(entry: &str, block_entity: Option<&Compound>) -> Vec<ItemStack> {
    use tuff_datapack::loot::{Predicates, parse};
    use tuff_datapack::tags::TagSet;
    let json = format!(r#"{{"pools":[{{"rolls":1,"entries":[{entry}]}}]}}"#);
    let parsed = parse(json.as_bytes(), &TagSet::vanilla(), &Predicates::default()).unwrap();
    let tables = LootTables::new(std::collections::HashMap::from([(
        "minecraft:blocks/stone".to_owned(),
        Some(parsed),
    )]));
    block_drops_in(
        &tables,
        Block::Stone.default_state(),
        Some(&ItemStack::new(Item::WoodenPickaxe, 1)),
        block_entity,
        &mut StdRng::seed_from_u64(1),
    )
}

#[tuff_core::watchdog]
#[test]
fn random_chance_accepts_a_float_provider() {
    let always = r#"{"type":"item","name":"emerald","condition":{"type":"minecraft:random_chance","chance":{"type":"minecraft:constant","value":1.0}}}"#;
    assert_eq!(stone_drops_of(always, None).len(), 1);
    let never = always.replace("1.0", "0.0");
    assert!(stone_drops_of(&never, None).is_empty());
}

#[tuff_core::watchdog]
#[test]
fn a_count_above_the_max_stack_splits_into_several_stacks() {
    let entry = r#"{"type":"item","name":"ender_pearl","modifier":[{"type":"minecraft:set_count","count":40}]}"#;
    let counts: Vec<u8> = stone_drops_of(entry, None)
        .iter()
        .map(|s| s.count)
        .collect();
    assert_eq!(counts, [16, 16, 8]);
}

#[tuff_core::watchdog]
#[test]
fn entry_functions_apply_to_non_item_entries() {
    let entry = r#"{"type":"minecraft:tag","name":"minecraft:logs","expand":true,"modifier":[{"type":"minecraft:set_count","count":3}]}"#;
    let drops = stone_drops_of(entry, None);
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].count, 3);
}

#[tuff_core::watchdog]
#[test]
fn contents_dynamic_drops_the_block_entity_items() {
    let mut item = Compound::new();
    item.insert("id", Tag::String("minecraft:diamond".into()));
    item.insert("count", Tag::Int(5));
    item.insert("Slot", Tag::Byte(0));
    let mut entity = Compound::new();
    entity.insert("Items", Tag::List(vec![Tag::Compound(item)]));
    let drops = stone_drops_of(
        r#"{"type":"minecraft:dynamic","name":"minecraft:contents"}"#,
        Some(&entity),
    );
    assert_eq!(drops.len(), 1);
    assert_eq!((drops[0].item, drops[0].count), (Item::Diamond, 5));
}

#[tuff_core::watchdog]
#[test]
fn furnace_smelt_scales_by_result_count_and_use_input_count() {
    let beef = ItemStack::new(Item::Beef, 3);
    let kept = modified(r#"[{"type":"minecraft:furnace_smelt"}]"#, beef.clone());
    assert_eq!((kept.item, kept.count), (Item::CookedBeef, 3));
    let single = modified(
        r#"[{"type":"minecraft:furnace_smelt","use_input_count":false}]"#,
        beef,
    );
    assert_eq!((single.item, single.count), (Item::CookedBeef, 1));
}

#[tuff_core::watchdog]
#[test]
fn book_page_functions_apply_list_operations() {
    let book = modified(
        r#"[{"type":"minecraft:set_writable_book_pages","pages":["a","b"]},
            {"type":"minecraft:set_writable_book_pages","pages":["c"],"mode":"append"}]"#,
        ItemStack::new(Item::WritableBook, 1),
    );
    let nbt = book.components.to_nbt();
    let content = nbt.get_compound("minecraft:writable_book_content").unwrap();
    assert_eq!(
        content.get("pages").and_then(Tag::as_list).unwrap().len(),
        3
    );
    let written = modified(
        r#"[{"type":"minecraft:set_written_book_pages","pages":["x"]}]"#,
        ItemStack::new(Item::WrittenBook, 1),
    );
    assert!(
        written
            .components
            .to_nbt()
            .get("minecraft:written_book_content")
            .is_some()
    );
}

#[tuff_core::watchdog]
#[test]
fn toggle_tooltips_hides_and_shows_components() {
    let stack = modified(
        r#"[{"type":"minecraft:toggle_tooltips","toggles":{"minecraft:enchantments":false,"minecraft:lore":false}},
            {"type":"minecraft:toggle_tooltips","toggles":{"minecraft:lore":true}}]"#,
        ItemStack::new(Item::Stick, 1),
    );
    let display = component(&stack, "minecraft:tooltip_display").unwrap();
    let hidden: Vec<&str> = display
        .as_compound()
        .unwrap()
        .get("hidden_components")
        .and_then(Tag::as_list)
        .unwrap()
        .iter()
        .filter_map(Tag::as_str)
        .collect();
    assert_eq!(hidden, ["minecraft:enchantments"]);
}

#[tuff_core::watchdog]
#[test]
fn expanded_tag_entries_weigh_each_item_against_siblings() {
    let tags = tuff_datapack::tags::TagSet::vanilla();
    let table = tuff_datapack::loot::parse(
        br##"{"pools":[{"rolls":1,"entries":[
            {"type":"minecraft:tag","name":"minecraft:logs","expand":true},
            {"type":"minecraft:item","name":"minecraft:stone"}]}]}"##,
        &tags,
        &tuff_datapack::loot::Predicates::vanilla(),
    )
    .unwrap();
    let tables = tuff_datapack::loot::LootTables::new(
        [("t:t".to_owned(), Some(table))].into_iter().collect(),
    );
    let mut rng = StdRng::seed_from_u64(5);
    let stones = (0..400)
        .flat_map(|_| roll_in(&tables, "t:t", &mut rng))
        .filter(|s| s.item == Item::Stone)
        .count();
    assert!(stones < 80, "stone dropped {stones} of 400 rolls");
}

#[tuff_core::watchdog]
#[test]
fn match_tool_uses_the_full_item_predicate() {
    let mut rng = StdRng::seed_from_u64(1);
    let context = bare_context();
    let condition: Value = serde_json::from_str(
        r#"{"type":"minecraft:match_tool","predicate":{"items":"minecraft:stick","count":{"min":2}}}"#,
    )
    .unwrap();
    let with = |count| {
        let tool = ItemStack::new(Item::Stick, count);
        let context = Context {
            tool: Some(&tool),
            ..bare_context()
        };
        json_condition(&condition, &context, &mut StdRng::seed_from_u64(1))
    };
    assert!(with(2));
    assert!(!with(1));
    assert!(!json_condition(&condition, &context, &mut rng));
}

#[tuff_core::watchdog]
#[test]
fn match_block_checks_block_entity_nbt() {
    let mut entity = Compound::new();
    entity.insert("Lock", Tag::String("k".into()));
    let condition: Value = serde_json::from_str(
        r#"{"type":"minecraft:match_block","blocks":"minecraft:chest","nbt":"{Lock:\"k\"}"}"#,
    )
    .unwrap();
    let at = |entity: Option<&Compound>| {
        let context = Context {
            state: Block::Chest.default_state(),
            block_entity: entity,
            ..bare_context()
        };
        json_condition(&condition, &context, &mut StdRng::seed_from_u64(1))
    };
    assert!(at(Some(&entity)));
    assert!(!at(Some(&Compound::new())));
    assert!(!at(None));
}

#[tuff_core::watchdog]
#[test]
fn entity_properties_fall_through_to_the_full_predicate() {
    let location: &'static LocationFacts = Box::leak(Box::default());
    let facts = EntityFacts::default();
    let context = Context {
        entity: Some(Box::leak(Box::new(facts))),
        location: Some(location),
        ..bare_context()
    };
    let condition = |max: f64| -> Value {
        serde_json::json!({"type":"minecraft:entity_properties","entity":"this",
            "predicate":{"minecraft:distance":{"absolute":{"max":max}}}})
    };
    let mut rng = StdRng::seed_from_u64(1);
    assert!(json_condition(&condition(1.0), &context, &mut rng));
    assert!(!json_condition(&condition(-1.0), &context, &mut rng));
}
