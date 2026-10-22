use super::*;
use crate::world::block_entity::raw;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::registry::{BlockEntityType, EntityType};

#[tuff_core::watchdog]
#[test]
fn block_reach_measures_to_the_nearest_face() {
    let block = BlockPos::new(0, 0, 0);
    assert_eq!(distance_sq_to_block([0.5, 0.5, 0.5], block), 0.0);
    assert_eq!(distance_sq_to_block([3.0, 0.5, 0.5], block), 4.0);
    assert_eq!(distance_sq_to_block([-1.0, 3.0, 0.5], block), 5.0);
}

#[tuff_core::watchdog]
#[test]
fn picked_blocks_copy_block_entity_data_only_when_asked() {
    let pos = BlockPos::new(1, 2, 3);
    let state = Block::OakSign.default_state();
    let mut sign = raw::new(BlockEntityType::Sign, pos);
    sign.insert("is_waxed", true);
    let plain = picked_block_stack(state, Some(&sign), false).unwrap();
    assert_eq!(plain, ItemStack::mint(Item::OakSign, 1, MintReason::Test));
    let waxed = picked_block_stack(state, Some(&sign), true).unwrap();
    let stored = waxed.components.to_nbt();
    assert!(stored.contains_key("minecraft:waxed"));
    assert!(!stored.contains_key("minecraft:block_entity_data"));
    let spawner = raw::new(BlockEntityType::MobSpawner, pos);
    let data = picked_block_stack(Block::Spawner.default_state(), Some(&spawner), true).unwrap();
    let stored = data.components.to_nbt();
    let entity = stored.get_compound("minecraft:block_entity_data").unwrap();
    assert_eq!(entity.get_str("id"), Some("minecraft:mob_spawner"));
    assert!(entity.get("Delay").is_some());
    assert!(entity.get("x").is_none() && entity.get("keepPacked").is_none());
    let wall = Block::OakWallSign.default_state();
    assert_eq!(
        picked_block_stack(wall, None, true).unwrap().item,
        Item::OakSign
    );
}

#[tuff_core::watchdog]
#[test]
fn picked_shulker_boxes_carry_their_contents_as_components() {
    let pos = BlockPos::new(0, 0, 0);
    let mut entity = crate::world::block_entity::BlockEntity::new(
        crate::world::block_entity::ContainerKind::ShulkerBox,
    );
    entity.items[3] = Some(ItemStack::mint(Item::Stone, 4, MintReason::Test));
    let saved = entity.encode(pos);
    let state = Block::ShulkerBox.default_state();
    let stack = picked_block_stack(state, Some(&saved), true).unwrap();
    let container = stack.components.container.as_ref().unwrap();
    assert_eq!(
        container[3],
        Some(ItemStack::mint(Item::Stone, 4, MintReason::Test))
    );
}

#[tuff_core::watchdog]
#[test]
fn picked_entities_give_eggs_boats_and_placing_items() {
    assert_eq!(
        picked_entity_item(EntityType::Zombie),
        Some(Item::ZombieSpawnEgg)
    );
    assert_eq!(picked_entity_item(EntityType::OakBoat), Some(Item::OakBoat));
    assert_eq!(
        picked_entity_item(EntityType::ChestMinecart),
        Some(Item::ChestMinecart)
    );
    assert_eq!(picked_entity_item(EntityType::LeashKnot), Some(Item::Lead));
    assert_eq!(picked_entity_item(EntityType::Item), None);
}

fn block_state_of(stack: &ItemStack) -> Option<tuff_nbt::Compound> {
    stack
        .components
        .to_nbt()
        .get_compound("minecraft:block_state")
        .cloned()
}

#[tuff_core::watchdog]
#[test]
fn plants_and_technical_blocks_pick_what_the_game_gives() {
    let pick = |block: Block| picked_block_stack(block.default_state(), None, false);
    assert_eq!(pick(Block::Wheat).unwrap().item, Item::WheatSeeds);
    assert_eq!(pick(Block::PumpkinStem).unwrap().item, Item::PumpkinSeeds);
    assert_eq!(
        pick(Block::AttachedMelonStem).unwrap().item,
        Item::MelonSeeds
    );
    assert_eq!(pick(Block::CaveVinesPlant).unwrap().item, Item::GlowBerries);
    assert_eq!(pick(Block::KelpPlant).unwrap().item, Item::Kelp);
    assert_eq!(pick(Block::TallSeagrass).unwrap().item, Item::Seagrass);
    assert_eq!(pick(Block::PottedPoppy).unwrap().item, Item::Poppy);
    assert_eq!(pick(Block::CandleCake).unwrap().item, Item::Cake);
    assert_eq!(
        pick(Block::SweetBerryBush).unwrap().item,
        Item::SweetBerries
    );
    assert!(pick(Block::FrostedIce).is_none());
    assert!(pick(Block::NetherPortal).is_none());
    let sticky = Block::PistonHead
        .default_state()
        .with("type", "sticky")
        .unwrap();
    assert_eq!(
        picked_block_stack(sticky, None, false).unwrap().item,
        Item::StickyPiston
    );
}

#[tuff_core::watchdog]
#[test]
fn picked_light_and_hives_record_their_state() {
    let dim = Block::Light.default_state().with("level", "7").unwrap();
    let light = picked_block_stack(dim, None, false).unwrap();
    assert_eq!(block_state_of(&light).unwrap().get_str("level"), Some("7"));
    let placed = light
        .components
        .apply_block_state(Block::Light.default_state());
    assert_eq!(placed, dim);
    let bright = picked_block_stack(Block::Light.default_state(), None, false).unwrap();
    assert_eq!(block_state_of(&bright), None);

    let full = Block::Beehive
        .default_state()
        .with("honey_level", "5")
        .unwrap();
    assert_eq!(
        block_state_of(&picked_block_stack(full, None, false).unwrap()),
        None
    );
    let data = picked_block_stack(full, None, true).unwrap();
    assert_eq!(
        block_state_of(&data).unwrap().get_str("honey_level"),
        Some("5")
    );
}

#[tuff_core::watchdog]
#[test]
fn statues_pots_and_banners_carry_their_looks_without_data() {
    let pos = BlockPos::new(0, 0, 0);
    let statue_state = Block::CopperGolemStatue
        .default_state()
        .with("copper_golem_pose", "star")
        .unwrap();
    let statue = raw::new(BlockEntityType::CopperGolemStatue, pos);
    let picked = picked_block_stack(statue_state, Some(&statue), false).unwrap();
    assert_eq!(
        block_state_of(&picked)
            .unwrap()
            .get_str("copper_golem_pose"),
        Some("star")
    );

    let mut pot = raw::new(BlockEntityType::DecoratedPot, pos);
    pot.insert(
        "sherds",
        tuff_nbt::Tag::List(vec![
            "minecraft:angler_pottery_sherd".into(),
            "minecraft:brick".into(),
            "minecraft:brick".into(),
            "minecraft:brick".into(),
        ]),
    );
    let picked =
        picked_block_stack(Block::DecoratedPot.default_state(), Some(&pot), false).unwrap();
    let stored = picked.components.to_nbt();
    assert!(stored.contains_key("minecraft:pot_decorations"));
    assert!(!stored.contains_key("minecraft:block_entity_data"));
}
