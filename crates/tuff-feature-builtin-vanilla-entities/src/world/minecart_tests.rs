use super::*;

#[tuff_core::watchdog]
#[test]
fn minecart_entity_data_matches_the_dumped_layout() {
    let cart = Minecart::with_base(EntityBase::new(
        EntityType::Minecart,
        1,
        1,
        [0.0; 3],
        [0.0; 3],
    ));
    for (index, value) in cart.metadata() {
        assert_eq!(
            EntityType::Minecart.data_serializer(index),
            Some(value.type_id() as u8),
            "index {index}"
        );
    }
    assert_eq!(cart.seats(), 1);
}

#[tuff_core::watchdog]
#[test]
fn command_block_minecart_sends_its_command_and_output_in_the_dumped_layout() {
    let mut cart = Minecart::with_base(EntityBase::new(
        EntityType::CommandBlockMinecart,
        1,
        1,
        [0.0; 3],
        [0.0; 3],
    ));
    cart.base.extra.insert("Command", "say hi");
    cart.show_command();
    let sent = cart.take_changed_data();
    assert_eq!(
        sent,
        vec![(DATA_COMMAND, MetadataValue::String("say hi".into()))]
    );
    for (index, value) in cart.metadata().into_iter().chain(sent) {
        assert_eq!(
            EntityType::CommandBlockMinecart.data_serializer(index),
            Some(value.type_id() as u8),
            "index {index}"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn furnace_minecart_sends_its_fuel_flag_at_the_dumped_index() {
    let cart = Minecart::with_base(EntityBase::new(
        EntityType::FurnaceMinecart,
        1,
        1,
        [0.0; 3],
        [0.0; 3],
    ));
    let sent = cart.metadata();
    assert!(sent.iter().any(|(index, _)| *index == DATA_FUEL));
    for (index, value) in sent {
        assert_eq!(
            EntityType::FurnaceMinecart.data_serializer(index),
            Some(value.type_id() as u8),
            "index {index}"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn minecart_items_map_to_their_entities_by_name() {
    assert_eq!(Item::Minecart.name(), EntityType::Minecart.name());
    assert_eq!(
        of_minecart_item(Item::ChestMinecart),
        Some(EntityType::ChestMinecart)
    );
    assert_eq!(
        minecart_item(EntityType::TntMinecart),
        Some(Item::TntMinecart)
    );
    assert_eq!(of_minecart_item(Item::Saddle), None);
    assert!(is_minecart(EntityType::HopperMinecart));
    assert!(!is_minecart(EntityType::Pig));
    // No item places these normally (spawner minecarts have no item at
    // all; command block minecarts have one but it is creative/give
    // only -- neither is reachable through survival placement here
    // since only `of_minecart_item` is ever called with a held item).
    assert_eq!(minecart_item(EntityType::SpawnerMinecart), None);
}

#[tuff_core::watchdog]
#[test]
fn rail_shapes_lead_to_their_exits() {
    assert_eq!(exits(Shape::EastWest), Some(([-1, 0, 0], [1, 0, 0])));
    assert_eq!(exits(Shape::AscendingEast), Some(([-1, -1, 0], [1, 0, 0])));
    assert_eq!(exits(Shape::NorthEast), Some(([0, 0, -1], [1, 0, 0])));
    assert_eq!(exits(Shape::Straight), None);
}

#[tuff_core::watchdog]
#[test]
fn a_cart_sits_on_the_rail_and_climbs_slopes() {
    let flat = Block::Rail
        .default_state()
        .with_shape(Shape::EastWest)
        .unwrap();
    let slope = Block::Rail
        .default_state()
        .with_shape(Shape::AscendingEast)
        .unwrap();
    let level = move |pos: BlockPos| {
        Some(match (pos.x, pos.y) {
            (0, 64) => flat,
            (1, 64) => slope,
            (_, y) if y < 64 => Block::Stone.default_state(),
            _ => Block::Air.default_state(),
        })
    };
    let on_flat = rail_position(0.3, 64.0625, 0.9, &level).unwrap();
    assert_eq!(on_flat, [0.3, 64.0625, 0.5]);
    let on_slope = rail_position(1.25, 64.3, 0.5, &level).unwrap();
    assert!((on_slope[1] - 64.3125).abs() < 1.0e-9, "{on_slope:?}");
    assert!(rail_position(5.5, 64.0, 0.5, &level).is_none());
}

#[tuff_core::watchdog]
#[test]
fn rider_keys_point_along_the_rider_facing() {
    let ahead = move_intent(RiderInput {
        forward: 1.0,
        yaw: 0.0,
        ..RiderInput::default()
    });
    assert!(
        ahead[0].abs() < 1.0e-6 && (ahead[2] - 1.0).abs() < 1.0e-6,
        "{ahead:?}"
    );
    let west = move_intent(RiderInput {
        forward: 1.0,
        yaw: 90.0,
        ..RiderInput::default()
    });
    assert!((west[0] + 1.0).abs() < 1.0e-6, "{west:?}");
    assert_eq!(move_intent(RiderInput::default()), [0.0; 3]);
}

#[tuff_core::watchdog]
#[test]
fn container_minecarts_coast_by_fill_level() {
    let base = |kind| EntityBase::new(kind, 1, 1, [0.0; 3], [0.0; 3]);
    let mut hopper = Minecart::with_base(base(EntityType::HopperMinecart));
    assert_eq!(
        hopper.container_slowdown(),
        Some(f64::from(0.98f32 + 15.0 * 0.001f32))
    );
    for slot in hopper.items.iter_mut() {
        *slot = Some(ItemStack::new(Item::Stone, 64));
    }
    assert_eq!(hopper.container_slowdown(), Some(f64::from(0.98f32)));
    let plain = Minecart::with_base(base(EntityType::Minecart));
    assert_eq!(plain.container_slowdown(), None);
}

#[tuff_core::watchdog]
#[test]
fn container_minecarts_round_trip_their_items_and_enabled_flag() {
    let base = |kind| EntityBase::new(kind, 1, 1, [0.0; 3], [0.0; 3]);
    let gold = ItemStack::new(Item::GoldIngot, 4);
    let mut nbt = Compound::new()
        .with(
            ITEMS,
            Tag::List(vec![Tag::Compound(gold.to_nbt().with("Slot", 3i8))]),
        )
        .with(ENABLED, false);
    let cart = load(base(EntityType::HopperMinecart), &mut nbt).unwrap();
    assert!(nbt.get(ITEMS).is_none() && nbt.get(ENABLED).is_none());
    assert_eq!(cart.container().unwrap()[3], Some(gold));
    assert_eq!(cart.container_menu(), MenuKind::Hopper);
    assert!(!cart.collects_items());
    assert_eq!(cart.seats(), 0);
    let mut saved = Compound::new();
    cart.save(&mut saved);
    assert_eq!(saved.get_byte(ENABLED), Some(0));
    assert_eq!(saved.get_list(ITEMS).map(<[_]>::len), Some(1));

    let mut nbt = Compound::new().with(LOOT_TABLE, "minecraft:chests/abandoned_mineshaft");
    let chest = load(base(EntityType::ChestMinecart), &mut nbt).unwrap();
    assert_eq!(chest.container().map(<[_]>::len), Some(27));
    assert_eq!(chest.container_menu(), MenuKind::Generic { rows: 3 });
    assert!(chest.loot_pending());
    let mut saved = Compound::new();
    chest.save(&mut saved);
    assert!(saved.get(ENABLED).is_none() && saved.get(ITEMS).is_none());
    assert_eq!(
        saved.get_str(LOOT_TABLE),
        Some("minecraft:chests/abandoned_mineshaft")
    );

    let plain = load(base(EntityType::Minecart), &mut Compound::new()).unwrap();
    assert!(plain.container().is_none());
    assert_eq!(plain.seats(), 1);
}

#[tuff_core::watchdog]
#[test]
fn pushed_carts_move_apart() {
    use rand::SeedableRng;
    use tuff_core::world::dimension::DimensionId;
    use tuff_core::world::entity::EntitySummary;
    use tuff_core::world::poi::PoiStore;

    let mut cart = Minecart::with_base(EntityBase::new(
        EntityType::Minecart,
        1,
        1,
        [0.0; 3],
        [0.0; 3],
    ));
    let other = EntitySummary {
        id: 2,
        uuid: 2,
        kind: EntityType::Minecart,
        position: [1.0, 0.0, 0.0],
        velocity: [0.0; 3],
        yaw: 0.0,
        bounds: [0.0; 6],
        living: None,
        pickable: false,
        item: None,
        vehicle: None,
        carries: false,
        ominous_banner: false,
        blue_sheep: false,
    };
    let shared = tuff_test_support::bare_shared();
    let block = |_: tuff_world_model::BlockPos| Some(tuff_vanilla_data::Block::Air.default_state());
    let light = |_: tuff_world_model::BlockPos| Some((15, 0));
    let biome = |_: tuff_world_model::BlockPos| Some("minecraft:plains");
    let mut rng = rand::rngs::StdRng::seed_from_u64(1);
    let mut effects = Vec::new();
    let mut budget = 10_000;
    let pois = PoiStore::default();
    let mut cx = TickContext {
        tick: 1,
        dimension: DimensionId::for_tests(0),
        block: &block,
        light: &light,
        sky_darken: 0,
        ambient_light: 0.0,
        monsters_burn: true,
        raining: false,
        biome: &biome,
        container: &|_| None,
        container_info: &|_| Default::default(),
        inhabited_time: &|_| 0,
        min_y: -64,
        max_y: 319,
        players: &[],
        players_in_dimension: 0,
        entities: &[],
        entity_grid: None,
        pistons: &[],
        parts: &[],
        shared: &shared,
        rng: &mut rng,
        effects: &mut effects,
        path_budget: &mut budget,
        pois: &pois,
        state: &tuff_core::world::region_api::RegionStateLock::default(),
    };
    cart.push_cart(&other, &mut cx);
    assert!(cart.base.velocity[0] < 0.0);
    let push = effects.iter().find_map(|e| match e {
        Effect::Nudge { target: 2, push } => Some(*push),
        _ => None,
    });
    assert!(push.expect("push")[0] > 0.0);
}
