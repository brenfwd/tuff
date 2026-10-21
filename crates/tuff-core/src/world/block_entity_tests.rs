use super::*;
use rand::SeedableRng;
use tuff_item::item::MintReason;
use tuff_vanilla_data::Item;

fn stack(item: Item, count: u8) -> Option<ItemStack> {
    Some(ItemStack::mint(item, count, MintReason::Test))
}

#[tuff_core::watchdog]
#[test]
fn a_golem_tops_up_stacks_until_the_first_empty_slot() {
    let mut items = vec![
        stack(Item::Dirt, 60),
        stack(Item::Stone, 1),
        None,
        stack(Item::Dirt, 1),
    ];
    let mut changed = Vec::new();
    let (left, ended) = golem_place(
        &mut items,
        ItemStack::mint(Item::Dirt, 10, MintReason::Test),
        &mut changed,
    );
    assert_eq!((left, ended), (None, true));
    assert_eq!(items[0], stack(Item::Dirt, 64));
    assert_eq!(
        items[2],
        stack(Item::Dirt, 6),
        "the rest fills the empty slot"
    );
    assert_eq!(items[3], stack(Item::Dirt, 1), "nothing reaches past it");
    assert_eq!(changed, vec![0, 2]);
    let mut full = vec![stack(Item::Dirt, 64), stack(Item::Dirt, 62)];
    let (left, ended) = golem_place(
        &mut full,
        ItemStack::mint(Item::Dirt, 5, MintReason::Test),
        &mut Vec::new(),
    );
    assert_eq!(
        (left, ended),
        (
            Some(ItemStack::mint(Item::Dirt, 3, MintReason::Test)),
            false
        )
    );
    assert_eq!(full[1], stack(Item::Dirt, 64));
}

#[tuff_core::watchdog]
#[test]
fn crafters_spread_automated_items_and_skip_disabled_slots() {
    let mut items = (0..CRAFTER_SLOTS)
        .map(|_| None)
        .collect::<Vec<Option<ItemStack>>>();
    let recipes = crate::world::crafting::RecipeSet::new(std::iter::empty());
    let mut changed = Vec::new();
    for _ in 0..3 {
        insert(
            &recipes,
            ContainerKind::Crafter,
            &mut items,
            None,
            ItemStack::mint(Item::Stone, 1, MintReason::Test),
            &mut changed,
            0b10,
        );
    }
    assert_eq!(items[0].as_ref().map(|s| s.count()), Some(1));
    assert!(items[1].is_none(), "a disabled slot takes nothing");
    assert_eq!(items[2].as_ref().map(|s| s.count()), Some(1));
    assert_eq!(items[3].as_ref().map(|s| s.count()), Some(1));
}

#[tuff_core::watchdog]
#[test]
fn crafters_round_trip_their_slot_states() {
    let mut entity = BlockEntity::new(ContainerKind::Crafter);
    let crafter = entity.crafter_mut().unwrap();
    crafter.disabled = 0b101;
    crafter.triggered = true;
    entity.items[1] = stack(Item::Stone, 3);
    assert_eq!(entity.crafter_signal(), 3);
    let pos = BlockPos::new(1, 2, 3);
    let (_, decoded) = BlockEntity::decode(&entity.encode(pos)).unwrap();
    assert_eq!(decoded, entity);
    assert_eq!(decoded.data(), vec![1, 0, 1, 0, 0, 0, 0, 0, 0, 1]);
}

#[tuff_core::watchdog]
#[test]
fn every_kind_round_trips_through_the_stored_schema() {
    for kind in [
        ContainerKind::Chest,
        ContainerKind::TrappedChest,
        ContainerKind::Barrel,
        ContainerKind::ShulkerBox,
        ContainerKind::Hopper,
        ContainerKind::Dropper,
        ContainerKind::Dispenser,
        ContainerKind::Furnace,
        ContainerKind::BlastFurnace,
        ContainerKind::Smoker,
        ContainerKind::Campfire,
    ] {
        let mut entity = BlockEntity::new(kind);
        entity.items[0] = stack(Item::Stone, 3);
        entity.items[kind.size() - 1] = stack(Item::DiamondPickaxe, 1);
        entity.custom_name = Some(TextComponent::new("Stuff"));
        entity.transfer_cooldown = 5;
        entity
            .extra
            .insert("LootTable", "minecraft:chests/simple_dungeon");
        let pos = BlockPos::new(3, -7, 11);
        let nbt = entity.encode(pos);
        assert_eq!(nbt.get_str("id"), Some(kind.entity_type().name()));
        let items = nbt.get_list("Items").unwrap();
        assert_eq!(items.len(), 2);
        let first = items[0].as_compound().unwrap();
        assert_eq!(first.get_byte("Slot"), Some(0));
        assert_eq!(first.get_str("id"), Some("minecraft:stone"));
        assert_eq!(first.get_int("count"), Some(3));
        assert_eq!(
            items[1].as_compound().unwrap().get_byte("Slot"),
            Some(kind.size() as i8 - 1)
        );
        assert_eq!(nbt.get_bool("keepPacked"), Some(false));
        assert_eq!(
            nbt.get_int("TransferCooldown").is_some(),
            kind == ContainerKind::Hopper
        );
        let (decoded_pos, decoded) = BlockEntity::decode(&nbt).unwrap();
        assert_eq!(decoded_pos, pos);
        if kind != ContainerKind::Hopper {
            entity.transfer_cooldown = NO_COOLDOWN;
        }
        assert_eq!(decoded, entity);
    }
}

#[tuff_core::watchdog]
#[test]
fn brewing_stand_progress_round_trips_with_the_game_keys() {
    let mut entity = BlockEntity::new(ContainerKind::BrewingStand);
    entity.items[brewing::INGREDIENT] = stack(Item::NetherWart, 3);
    let stand = entity.brewing_mut().unwrap();
    stand.brew_time = 120;
    stand.total_brew_time = 400;
    stand.fuel = 7;
    stand.total_fuel = 20;
    stand.ingredient = Some(Item::NetherWart);
    let nbt = entity.encode(BlockPos::new(0, 64, 0));
    assert_eq!(nbt.get_int("BrewTime"), Some(120));
    assert_eq!(nbt.get_int("Fuel"), Some(7));
    assert_eq!(nbt.get_int("total_fuel"), Some(20));
    assert_eq!(nbt.get_float("speed_multiplier"), Some(1.0));
    let (_, decoded) = BlockEntity::decode(&nbt).unwrap();
    assert_eq!(decoded, entity);
    assert_eq!(decoded.data(), vec![120, 7, 400, 20]);

    let mut bare = entity.encode(BlockPos::new(0, 64, 0));
    bare.remove("total_brew_time");
    bare.remove("total_fuel");
    let (_, decoded) = BlockEntity::decode(&bare).unwrap();
    assert_eq!(decoded.data()[2..], [400, 20]);
    assert!(decoded.extra.is_empty());
}

#[tuff_core::watchdog]
#[test]
fn furnace_and_campfire_progress_round_trip() {
    let mut entity = BlockEntity::new(ContainerKind::BlastFurnace);
    let furnace = entity.furnace_mut().unwrap();
    furnace.lit_time_remaining = 350;
    furnace.lit_total_time = 800;
    furnace.cooking_timer = 42;
    furnace.cooking_total_time = 100;
    furnace.speed_multiplier = 2.0;
    furnace
        .recipes_used
        .insert("minecraft:iron_ingot_from_blasting_iron_ore".into(), 3);
    let nbt = entity.encode(BlockPos::new(0, 64, 0));
    assert_eq!(nbt.get_int("lit_time_remaining"), Some(350));
    assert_eq!(nbt.get_int("cooking_time_spent"), Some(42));
    assert_eq!(nbt.get_float("speed_multiplier"), Some(2.0));
    assert_eq!(
        nbt.get_compound("RecipesUsed")
            .unwrap()
            .get_int("minecraft:iron_ingot_from_blasting_iron_ore"),
        Some(3)
    );
    let (_, decoded) = BlockEntity::decode(&nbt).unwrap();
    assert_eq!(decoded, entity);
    assert_eq!(decoded.data(), vec![350, 800, 42, 100]);

    let mut campfire = BlockEntity::new(ContainerKind::Campfire);
    campfire.items[2] = stack(Item::Beef, 1);
    let state = campfire.campfire_mut().unwrap();
    state.cooking_progress[2] = 17;
    state.cooking_time[2] = 600;
    let nbt = campfire.encode(BlockPos::new(1, 2, 3));
    assert_eq!(nbt.get_int_array("CookingTimes"), Some(&[0, 0, 17, 0][..]));
    assert_eq!(
        nbt.get_int_array("CookingTotalTimes"),
        Some(&[0, 0, 600, 0][..])
    );
    let (_, decoded) = BlockEntity::decode(&nbt).unwrap();
    assert_eq!(decoded, campfire);
    let client = campfire.client_data().unwrap();
    assert_eq!(client.get_list("Items").unwrap().len(), 1);
    assert!(!client.contains_key("CookingTimes"));
    assert!(
        BlockEntity::new(ContainerKind::Chest)
            .client_data()
            .is_none()
    );
}

#[tuff_core::watchdog]
#[test]
fn empty_containers_save_an_items_list_except_shulker_boxes() {
    let chest = BlockEntity::new(ContainerKind::Chest).encode(BlockPos::default());
    assert_eq!(chest.get_list("Items"), Some(&[][..]));
    let shulker = BlockEntity::new(ContainerKind::ShulkerBox).encode(BlockPos::default());
    assert!(!shulker.contains_key("Items"));
    assert!(BlockEntity::decode(&shulker).is_some());
}

#[tuff_core::watchdog]
#[test]
fn unmodeled_kinds_and_out_of_range_slots_are_left_alone() {
    let sign = Compound::new()
        .with("id", "minecraft:sign")
        .with("x", 0)
        .with("y", 0)
        .with("z", 0);
    let (typed, raw) = split([sign.clone(), Compound::new().with("id", "minecraft:bell")]);
    assert!(typed.is_empty());
    assert_eq!(raw, HashMap::from([(BlockPos::default(), sign)]));
    let hopper = Compound::new()
        .with("id", "minecraft:hopper")
        .with("x", 1)
        .with("y", 2)
        .with("z", 3)
        .with(
            "Items",
            vec![
                Compound::new()
                    .with("Slot", 7i8)
                    .with("id", "minecraft:stone")
                    .with("count", 1),
            ],
        );
    let (_, entity) = BlockEntity::decode(&hopper).unwrap();
    assert!(entity.is_empty());
    assert_eq!(entity.transfer_cooldown, NO_COOLDOWN);
}

#[tuff_core::watchdog]
#[test]
fn shulker_contents_round_trip_through_item_components() {
    let mut entity = BlockEntity::new(ContainerKind::ShulkerBox);
    entity.items[2] = stack(Item::Stone, 12);
    entity.items[26] = stack(Item::Apple, 1);
    entity.custom_name = Some(TextComponent::new("Box"));
    let pos = BlockPos::new(1, 2, 3);
    let kind = BlockEntityType::ShulkerBox;
    let components = Components::from_nbt(&raw::collect_components(kind, &entity.encode(pos)));
    assert_eq!(components.container.as_ref().unwrap().len(), 27);
    assert_eq!(
        components.custom_name,
        entity.custom_name.clone().map(Box::new)
    );
    let restored = create(kind, pos, &components).unwrap();
    assert_eq!(restored.items, entity.items);
    assert_eq!(restored.custom_name, entity.custom_name);
    assert!(
        raw::collect_components(
            kind,
            &BlockEntity::new(ContainerKind::ShulkerBox).encode(pos)
        )
        .is_empty()
    );
    assert_eq!(
        create(BlockEntityType::Bell, pos, &Components::default()),
        Err(raw::new(BlockEntityType::Bell, pos))
    );
    let Err(bell) = create(BlockEntityType::Bell, pos, &components) else {
        panic!("bells are not modeled");
    };
    assert!(
        bell.get_compound("components")
            .unwrap()
            .contains_key("minecraft:custom_name")
    );
}

#[tuff_core::watchdog]
#[test]
fn hopper_transfers_one_item_into_matching_or_empty_slots() {
    let mut hopper = BlockEntity::new(ContainerKind::Hopper);
    hopper.items[3] = stack(Item::Stone, 2);
    let mut chest = BlockEntity::new(ContainerKind::Chest);
    chest.items[0] = stack(Item::Dirt, 1);
    chest.items[1] = stack(Item::Stone, 64);
    chest.items[5] = stack(Item::Stone, 3);
    let moved = transfer_one(
        &crate::world::crafting::RecipeSet::default(),
        ContainerKind::Hopper,
        &mut hopper.items,
        None,
        ContainerKind::Chest,
        &mut chest.items,
        Some(Facing::Up),
    );
    assert_eq!(moved, Some((3, vec![2])));
    assert_eq!(chest.items[2], stack(Item::Stone, 1));
    assert_eq!(chest.items[5], stack(Item::Stone, 3));
    assert_eq!(hopper.items[3], stack(Item::Stone, 1));
    let moved = transfer_one(
        &crate::world::crafting::RecipeSet::default(),
        ContainerKind::Hopper,
        &mut hopper.items,
        None,
        ContainerKind::Chest,
        &mut chest.items,
        Some(Facing::Up),
    );
    assert_eq!(moved, Some((3, vec![2])));
    assert_eq!(chest.items[2], stack(Item::Stone, 2));
    assert_eq!(hopper.items[3], None);
    assert_eq!(
        transfer_one(
            &crate::world::crafting::RecipeSet::default(),
            ContainerKind::Hopper,
            &mut hopper.items,
            None,
            ContainerKind::Chest,
            &mut chest.items,
            Some(Facing::Up),
        ),
        None
    );
}

#[tuff_core::watchdog]
#[test]
fn shulker_boxes_refuse_shulker_boxes_from_hoppers() {
    let mut hopper = BlockEntity::new(ContainerKind::Hopper);
    hopper.items[0] = stack(Item::ShulkerBox, 1);
    hopper.items[1] = stack(Item::Stone, 1);
    let mut box_items = BlockEntity::new(ContainerKind::ShulkerBox).items;
    let moved = transfer_one(
        &crate::world::crafting::RecipeSet::default(),
        ContainerKind::Hopper,
        &mut hopper.items,
        None,
        ContainerKind::ShulkerBox,
        &mut box_items,
        Some(Facing::Up),
    );
    assert_eq!(moved, Some((1, vec![0])));
    assert_eq!(box_items[0], stack(Item::Stone, 1));
    assert_eq!(hopper.items[0], stack(Item::ShulkerBox, 1));
}

#[tuff_core::watchdog]
#[test]
fn hoppers_feed_furnaces_by_face_and_take_only_results_and_buckets() {
    let kind = ContainerKind::Furnace;
    let mut hopper = BlockEntity::new(ContainerKind::Hopper);
    hopper.items[0] = stack(Item::Coal, 2);
    hopper.items[1] = stack(Item::IronOre, 2);
    let mut furnace = BlockEntity::new(kind);
    let from_above = transfer_one(
        &crate::world::crafting::RecipeSet::default(),
        ContainerKind::Hopper,
        &mut hopper.items,
        None,
        kind,
        &mut furnace.items,
        Some(Facing::Up),
    );
    assert_eq!(from_above, Some((0, vec![furnace::INPUT])));
    assert_eq!(furnace.items[furnace::INPUT], stack(Item::Coal, 1));
    let from_side = transfer_one(
        &crate::world::crafting::RecipeSet::default(),
        ContainerKind::Hopper,
        &mut hopper.items,
        None,
        kind,
        &mut furnace.items,
        Some(Facing::West),
    );
    assert_eq!(from_side, Some((0, vec![furnace::FUEL])));
    assert_eq!(furnace.items[furnace::FUEL], stack(Item::Coal, 1));
    hopper.items[0] = None;
    assert_eq!(
        transfer_one(
            &crate::world::crafting::RecipeSet::default(),
            ContainerKind::Hopper,
            &mut hopper.items,
            None,
            kind,
            &mut furnace.items,
            Some(Facing::East),
        ),
        None,
        "ore is not fuel"
    );
    let mut bucket_hopper = BlockEntity::new(ContainerKind::Hopper);
    bucket_hopper.items[0] = stack(Item::Bucket, 3);
    assert_eq!(
        transfer_one(
            &crate::world::crafting::RecipeSet::default(),
            ContainerKind::Hopper,
            &mut bucket_hopper.items,
            None,
            kind,
            &mut furnace.items,
            Some(Facing::North),
        ),
        None,
        "a coal-holding fuel slot refuses buckets"
    );
    furnace.items[furnace::FUEL] = None;
    assert!(
        transfer_one(
            &crate::world::crafting::RecipeSet::default(),
            ContainerKind::Hopper,
            &mut bucket_hopper.items,
            None,
            kind,
            &mut furnace.items,
            Some(Facing::North),
        )
        .is_some()
    );
    assert_eq!(
        transfer_one(
            &crate::world::crafting::RecipeSet::default(),
            ContainerKind::Hopper,
            &mut bucket_hopper.items,
            None,
            kind,
            &mut furnace.items,
            Some(Facing::North),
        ),
        None,
        "only one bucket goes in"
    );

    furnace.items[furnace::INPUT] = stack(Item::IronOre, 4);
    furnace.items[furnace::RESULT] = stack(Item::IronIngot, 2);
    let mut below = BlockEntity::new(ContainerKind::Hopper);
    let pulled = transfer_one(
        &crate::world::crafting::RecipeSet::default(),
        kind,
        &mut furnace.items,
        Some(Facing::Down),
        ContainerKind::Hopper,
        &mut below.items,
        None,
    );
    assert_eq!(pulled, Some((furnace::RESULT, vec![0])));
    assert_eq!(below.items[0], stack(Item::IronIngot, 1));
    furnace.items[furnace::RESULT] = None;
    let pulled = transfer_one(
        &crate::world::crafting::RecipeSet::default(),
        kind,
        &mut furnace.items,
        Some(Facing::Down),
        ContainerKind::Hopper,
        &mut below.items,
        None,
    );
    assert_eq!(pulled, Some((furnace::FUEL, vec![1])));
    assert_eq!(below.items[1], stack(Item::Bucket, 1));
    assert_eq!(furnace.items[furnace::FUEL], None);
    furnace.items[furnace::FUEL] = stack(Item::Coal, 5);
    assert_eq!(
        transfer_one(
            &crate::world::crafting::RecipeSet::default(),
            kind,
            &mut furnace.items,
            Some(Facing::Down),
            ContainerKind::Hopper,
            &mut below.items,
            None,
        ),
        None,
        "fuel and input never leave through the bottom"
    );
    assert_eq!(furnace.items[furnace::INPUT], stack(Item::IronOre, 4));
}

#[tuff_core::watchdog]
#[test]
fn scatter_splits_stacks_inside_the_block() {
    let mut rng = rand::rngs::StdRng::seed_from_u64(7);
    let pos = BlockPos::new(10, 64, -5);
    let drops = scatter(
        pos,
        [ItemStack::mint(Item::Stone, 64, MintReason::Test)],
        &mut rng,
    );
    assert!(drops.len() >= 3);
    assert_eq!(
        drops.iter().map(|(s, _, _)| s.count() as u32).sum::<u32>(),
        64
    );
    for (stack, position, _) in &drops {
        assert!((1..=30).contains(&stack.count()));
        assert!(position[0] >= 10.0 && position[0] < 11.0);
        assert!(position[1] >= 64.0 && position[1] < 65.0);
        assert!(position[2] >= -5.0 && position[2] < -4.0);
    }
    assert!(scatter(pos, [], &mut rng).is_empty());
}
