use tuff_vanilla_data::biome::Biome;
use tuff_vanilla_data::block::{Facing, Half, Shape, Side, Type};
use tuff_vanilla_data::command::{CommandNode, CommandNodeKind, ROOT};
use tuff_vanilla_data::damage_type::{self, DeathMessageType, Scaling};
use tuff_vanilla_data::loot_table::{self, LootCondition, LootEntry, LootEntryKind, LootTable};
use tuff_vanilla_data::recipe::{self, Ingredient, Recipe, RecipeEntry};
use tuff_vanilla_data::registry::worldgen::FeatureType;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{
    Block, BlockState, Item, Registry, data_registry, dimension_type, packet, tag, version,
};

#[test]
fn version_matches_build_target() {
    assert_eq!(version::NAME, "26.3");
    const { assert!(version::PROTOCOL_VERSION > 0) };
    const { assert!(version::DATA_VERSION > 0) };
}

#[test]
fn packet_ids_round_trip() {
    use packet::handshake::Serverbound as Handshake;
    use packet::play::Clientbound as Play;

    assert_eq!(Handshake::Intention.id(), 0);
    assert_eq!(Handshake::from_id(0), Some(Handshake::Intention));
    assert_eq!(Handshake::from_id(1), None);
    assert_eq!(Handshake::from_id(-1), None);
    for packet in Play::ALL {
        assert_eq!(Play::from_id(packet.id()), Some(packet));
        assert_eq!(Play::from_name(packet.name()), Some(packet));
    }
}

fn assert_registry_round_trips<R: Registry + PartialEq + core::fmt::Debug>() {
    assert!(!R::ALL.is_empty(), "{} is empty", R::KEY);
    for (index, &entry) in R::ALL.iter().enumerate() {
        assert_eq!(entry.id() as usize, index);
        assert_eq!(R::from_id(entry.id()), Some(entry));
        assert_eq!(R::from_name(entry.name()), Some(entry));
        let path = entry.name().strip_prefix("minecraft:").unwrap();
        assert_eq!(R::from_name(path), Some(entry));
    }
    assert_eq!(R::from_id(R::ALL.len() as u32), None);
    assert_eq!(R::from_name("minecraft:definitely_not_an_entry"), None);
}

#[test]
fn registries_round_trip() {
    assert_registry_round_trips::<Block>();
    assert_registry_round_trips::<Item>();
    assert_registry_round_trips::<EntityType>();
    assert_registry_round_trips::<SoundEvent>();
    assert_registry_round_trips::<FeatureType>();
}

#[test]
fn registry_metadata() {
    assert_eq!(Block::KEY, "minecraft:block");
    assert_eq!(FeatureType::KEY, "minecraft:worldgen/feature_type");
    assert_eq!(Block::default(), Block::Air);
    assert_eq!(Block::Stone.to_string(), "minecraft:stone");
    assert_eq!(
        Item::from_name("minecraft:diamond_sword"),
        Some(Item::DiamondSword)
    );
}

#[test]
fn every_block_state_round_trips() {
    for id in 0..BlockState::COUNT as u32 {
        let state = BlockState::from_id(id).unwrap();
        assert_eq!(state.id(), id);
        let block = state.block();
        assert!(block.states().any(|s| s == state));
        assert_eq!(block.state_with(state.properties()), Some(state), "{state}");
        assert_eq!(state.to_string().parse::<BlockState>(), Ok(state));
        for (name, value) in state.properties() {
            assert_eq!(state.get(name), Some(value));
            assert_eq!(state.with(name, value), Some(state));
        }
    }
    assert_eq!(BlockState::from_id(BlockState::COUNT as u32), None);
}

#[test]
fn every_block_has_one_default_state() {
    for block in Block::ALL {
        assert_eq!(block.default_state().block(), block);
        assert_eq!(
            block.states().filter(|s| s.is_default()).count(),
            1,
            "{block}"
        );
    }
}

#[test]
fn well_known_states() {
    assert_eq!(Block::Air.default_state().id(), 0);
    assert_eq!(Block::Stone.default_state().id(), 1);
    assert_eq!(BlockState::default(), Block::Air.default_state());
    assert_eq!(Block::Stone.default_state().facing(), None);
}

#[test]
fn typed_property_accessors() {
    let stairs = Block::OakStairs.default_state();
    assert_eq!(stairs.facing(), Some(Facing::North));
    assert_eq!(stairs.half(), Some(Half::Bottom));
    assert_eq!(stairs.shape(), Some(Shape::Straight));
    assert_eq!(stairs.waterlogged(), Some(false));

    let turned = stairs.with_facing(Facing::East).unwrap();
    assert_eq!(turned.facing(), Some(Facing::East));
    assert_eq!(turned.half(), stairs.half());
    assert_eq!(turned.block(), Block::OakStairs);
    assert_eq!(
        stairs.with_facing(Facing::Up),
        None,
        "stairs only face horizontally"
    );
    assert_eq!(
        stairs.with_half(Half::Upper),
        None,
        "upper belongs to doors"
    );

    let wet = stairs.with_waterlogged(true).unwrap();
    assert_eq!(wet.waterlogged(), Some(true));
    assert_eq!(wet.get("waterlogged"), Some("true"));

    let slab = Block::OakSlab.default_state().with_type(Type::Top).unwrap();
    assert_eq!(slab.r#type(), Some(Type::Top));

    let wheat = Block::Wheat.default_state();
    assert_eq!(wheat.age(), Some(0));
    assert_eq!(wheat.with_age(7).and_then(BlockState::age), Some(7));
    assert_eq!(wheat.with_age(8), None);
}

#[test]
fn directional_properties_can_be_bool_or_enum() {
    let fence = Block::OakFence.default_state().with_north(true).unwrap();
    assert_eq!(fence.north(), Some(true));
    assert_eq!(fence.north_side(), None);

    let wall = Block::CobblestoneWall
        .default_state()
        .with_north_side(Side::Tall)
        .unwrap();
    assert_eq!(wall.north_side(), Some(Side::Tall));
    assert_eq!(wall.north(), None);
    assert_eq!(
        wall.with_north_side(Side::Up),
        None,
        "up belongs to redstone wire"
    );
}

#[test]
fn block_state_strings() {
    let state: BlockState = "minecraft:oak_stairs[facing=east,half=top]"
        .parse()
        .unwrap();
    assert_eq!(state.facing(), Some(Facing::East));
    assert_eq!(state.half(), Some(Half::Top));
    assert_eq!(state.shape(), Some(Shape::Straight));
    assert_eq!("stone".parse(), Ok(Block::Stone.default_state()));
    assert!(
        "minecraft:stone[facing=east]"
            .parse::<BlockState>()
            .is_err()
    );
    assert!(
        "minecraft:oak_stairs[facing=up]"
            .parse::<BlockState>()
            .is_err()
    );
    assert!(
        "minecraft:oak_stairs[facing=east"
            .parse::<BlockState>()
            .is_err()
    );
    assert_eq!(Block::Stone.default_state().to_string(), "minecraft:stone");
}

#[test]
fn item_components() {
    assert_eq!(Item::Stone.max_stack_size(), 64);
    assert_eq!(Item::EnderPearl.max_stack_size(), 16);
    assert_eq!(Item::DiamondSword.max_stack_size(), 1);
    assert!(Item::DiamondSword.max_damage().is_some());
    assert_eq!(Item::Stone.max_damage(), None);
    assert!(!Item::DiamondSword.can_destroy_blocks_in_creative());
    assert!(!Item::Trident.can_destroy_blocks_in_creative());
    assert!(Item::DiamondPickaxe.can_destroy_blocks_in_creative());
    assert!(Item::Stone.can_destroy_blocks_in_creative());
}

#[test]
fn tags_are_flattened() {
    assert!(tag::block::LOGS.contains(&Block::OakLog));
    assert!(
        tag::block::LOGS.contains(&Block::StrippedBirchWood),
        "via #logs_that_burn"
    );
    assert!(tag::item::SWORDS.contains(&Item::DiamondSword));
    assert!(tag::worldgen::biome::IS_OCEAN.contains(&"minecraft:deep_ocean"));
    assert!(tag::block::INCORRECT_FOR_NETHERITE_TOOL.is_empty());
    let (name, entries) = tag::block::ALL
        .iter()
        .find(|(name, _)| *name == "minecraft:logs")
        .unwrap();
    assert_eq!(*name, "minecraft:logs");
    assert_eq!(*entries, tag::block::LOGS);
}

#[test]
fn data_registries() {
    let biomes = data_registry::DataRegistry::get("minecraft:worldgen/biome").unwrap();
    assert!(biomes.entries.contains(&"minecraft:plains"));
    assert!(biomes.entries.is_sorted());
    assert!(
        data_registry::DAMAGE_TYPE
            .entries
            .contains(&"minecraft:fall")
    );
    assert!(data_registry::ALL.len() > 10);
}

#[test]
fn dimension_types() {
    let overworld = dimension_type::OVERWORLD;
    assert_eq!(overworld.name, "minecraft:overworld");
    assert_eq!(overworld.min_y, -64);
    assert_eq!(overworld.height, 384);
    assert!(overworld.has_skylight);
    const { assert!(dimension_type::THE_NETHER.has_ceiling) };
    assert_eq!(dimension_type::ALL.len(), 4);
}

#[test]
fn damage_types() {
    let fall = damage_type::FALL;
    assert_eq!(fall.name, "minecraft:fall");
    assert_eq!(fall.message_id, "fall");
    assert_eq!(fall.exhaustion, 0.0);
    assert_eq!(fall.scaling, Scaling::WhenCausedByLivingNonPlayer);
    assert_eq!(
        fall.death_message_type,
        Some(DeathMessageType::FallVariants)
    );
    assert_eq!(fall.effects, None);
    assert!(damage_type::ALL.contains(&fall));
}

#[test]
fn biomes() {
    let plains = Biome::get("plains").unwrap();
    assert_eq!(plains.name, "minecraft:plains");
    assert_eq!(plains.temperature, 0.8);
    assert_eq!(plains.effects.water_color, 0x3F76E4);
    assert!(plains.carvers.contains(&"minecraft:cave"));
    assert!(plains.attribute("minecraft:visual/sky_color").is_some());
    assert!(Biome::get("minecraft:not_a_biome").is_none());
    for (name, biome) in tuff_vanilla_data::biome::ALL {
        assert_eq!(Biome::get(name).map(|b| b.name), Some(*name));
        assert_eq!(biome.name, *name);
    }
}

#[test]
fn shaped_recipe() {
    let entry = RecipeEntry::get("minecraft:diamond_sword").unwrap();
    let Recipe::CraftingShaped(sword) = &entry.recipe else {
        panic!("expected a shaped recipe, got {:?}", entry.recipe);
    };
    assert_eq!(sword.result.item, Item::DiamondSword);
    assert_eq!(sword.result.count, 1);
    assert_eq!(sword.pattern.len(), 3);
    let blade = sword.pattern[0].chars().next().unwrap();
    assert!(sword.ingredient(blade).unwrap().matches(Item::Diamond));
    let handle = sword.pattern[2].chars().next().unwrap();
    assert_eq!(
        sword.ingredient(handle),
        Some(Ingredient::Item(Item::Stick))
    );
    assert_eq!(sword.ingredient(' '), None);
}

#[test]
fn every_recipe_is_reachable() {
    assert!(recipe::ALL.len() > 1000);
    assert!(recipe::ALL.is_sorted_by_key(|(name, _)| *name));
    for (name, entry) in recipe::ALL {
        assert_eq!(entry.name, *name);
        assert!(std::ptr::eq(RecipeEntry::get(name).unwrap(), *entry));
    }
}

fn entries(table: &LootTable) -> impl Iterator<Item = &LootEntry> {
    fn walk<'a>(entries: &'a [LootEntry], out: &mut Vec<&'a LootEntry>) {
        for entry in entries {
            out.push(entry);
            if let LootEntryKind::Group(children)
            | LootEntryKind::Alternatives(children)
            | LootEntryKind::Sequence(children) = &entry.kind
            {
                walk(children, out);
            }
        }
    }
    let mut out = Vec::new();
    for pool in table.pools.iter() {
        walk(&pool.entries, &mut out);
    }
    out.into_iter()
}

#[test]
fn stone_loot_table_inlines_silk_touch_predicate() {
    let stone = LootTable::get("minecraft:blocks/stone").unwrap();
    let silk_touch: Vec<LootCondition> = entries(stone)
        .filter_map(|entry| entry.condition.clone())
        .filter(|condition| {
            condition
                .params
                .as_ref()
                .is_some_and(|p| p.contains("minecraft:silk_touch"))
        })
        .collect();
    assert!(!silk_touch.is_empty());
    for condition in silk_touch {
        assert_eq!(condition.kind, "minecraft:match_tool");
    }
    assert!(entries(stone).any(|entry| matches!(entry.kind, LootEntryKind::Item(Item::Stone))));
    assert!(
        entries(stone).any(|entry| matches!(entry.kind, LootEntryKind::Item(Item::Cobblestone)))
    );
}

#[test]
fn every_loot_table_is_reachable() {
    assert!(loot_table::ALL.len() > 1000);
    assert!(loot_table::ALL.is_sorted_by_key(|(name, _)| *name));
    for (name, table) in loot_table::ALL {
        assert!(std::ptr::eq(LootTable::get(name).unwrap(), *table));
        for entry in entries(table) {
            if let LootEntryKind::LootTable(target) = &entry.kind {
                assert!(
                    LootTable::get(target).is_some(),
                    "{name} references {target}"
                );
            }
        }
    }
    assert!(LootTable::get("blocks/stone").is_some());
}

#[test]
fn inline_loot_tables() {
    let trial = LootTable::get("equipment/trial_chamber").unwrap();
    let inline: Vec<&LootTable> = entries(trial)
        .filter_map(|entry| match &entry.kind {
            LootEntryKind::Inline(table) => Some(&**table),
            _ => None,
        })
        .collect();
    assert!(!inline.is_empty());
    for table in inline {
        assert!(table.kind.is_none());
        assert!(!table.pools.is_empty());
    }
}

#[test]
fn command_tree() {
    assert!(matches!(ROOT.kind, CommandNodeKind::Root));
    for name in ["give", "execute", "teleport", "tp"] {
        let node = ROOT.child(name).unwrap_or_else(|| panic!("no /{name}"));
        assert!(matches!(node.kind, CommandNodeKind::Literal));
    }

    let tp = ROOT.child("tp").unwrap();
    assert_eq!(tp.redirect, Some(&["teleport"][..]));
    assert!(std::ptr::eq(
        tp.redirect_target().unwrap(),
        ROOT.child("teleport").unwrap()
    ));

    let run = CommandNode::resolve(["execute", "run"]).unwrap();
    assert!(std::ptr::eq(run.redirect_target().unwrap(), &ROOT));

    let targets = CommandNode::resolve(["give", "targets"]).unwrap();
    let CommandNodeKind::Argument { parser, .. } = targets.kind else {
        panic!("expected an argument node");
    };
    assert_eq!(parser, "minecraft:entity");
    assert!(ROOT.child("give").unwrap().permissions.is_some());
    assert!(CommandNode::resolve(["give", "not_an_argument"]).is_none());
}

#[test]
fn deprecated_translation_keys_are_removed_and_renamed() {
    use tuff_vanilla_data::lang::lookup;
    assert!(lookup("item.minecraft.creeper_banner_pattern.desc").is_none());
    assert!(lookup("item.minecraft.dune_armor_trim_smithing_template.new").is_none());
    assert!(lookup("item.minecraft.dune_armor_trim_smithing_template").is_some());
}
