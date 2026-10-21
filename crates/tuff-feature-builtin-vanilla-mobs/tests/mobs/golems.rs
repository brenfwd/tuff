//! Golem assembly: placing a pumpkin head on a body of blocks spawns the
//! mob and clears the blocks it used.

use super::*;
use crate::world::entity::EntityAttack;
use crate::world::messages::HandEffect;
use tuff_vanilla_data::registry::EntityType;

async fn entity_nbt(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().expect("entity exists")
}

/// Opens the container at `pos` as `player`, writes `slots` into it through
/// a transaction, then closes it -- a way to pre-fill a chest for a test
/// without driving a real player click for each item.
async fn seed_chest(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    pos: BlockPos,
    slots: &[(usize, ItemStack)],
    sequence: i32,
) {
    region
        .request(RegionRequest::Place(place(
            player,
            pos,
            Item::Stick,
            sequence,
        )))
        .await;
    expect(events, container_menu).await;
    let changes = slots
        .iter()
        .flat_map(|(slot, stack)| set_slot(pos, *slot, None, Some(stack.test_copy())))
        .collect::<Vec<_>>();
    region
        .request(RegionRequest::ContainerTransaction {
            player: player.clone(),
            window_id: 1,
            pool: pool_for(&changes),
            changes,
            took_result: false,
        })
        .await;
    let (accepted, _) = expect(events, transaction_answer).await;
    assert!(accepted, "seeding {pos:?} was accepted");
    region
        .request(RegionRequest::CloseContainer {
            player: player.id,
            positions: vec![pos],
        })
        .await;
}

/// The contents of the container at `pos`, read by opening and immediately
/// closing it as `player`.
async fn chest_contents(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    pos: BlockPos,
    sequence: i32,
) -> Vec<Option<ItemStack>> {
    region
        .request(RegionRequest::Place(place(
            player,
            pos,
            Item::Stick,
            sequence,
        )))
        .await;
    let open = expect(events, container_menu).await;
    region
        .request(RegionRequest::CloseContainer {
            player: player.id,
            positions: vec![pos],
        })
        .await;
    match open.source {
        MenuSource::Blocks { contents, .. } => stacks(&contents),
        other => panic!("expected a block container, got {other:?}"),
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pumpkin_on_an_iron_body_assembles_a_healable_iron_golem() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;

    let center = BlockPos::new(8, SKY, 8);
    let bottom = BlockPos::new(8, SKY - 1, 8);
    let arm_a = BlockPos::new(9, SKY, 8);
    let arm_b = BlockPos::new(7, SKY, 8);
    let head_pos = BlockPos::new(8, SKY + 1, 8);
    let mut blocks = vec![
        (bottom, Block::IronBlock.default_state()),
        (center, Block::IronBlock.default_state()),
        (arm_a, Block::IronBlock.default_state()),
        (arm_b, Block::IronBlock.default_state()),
    ];
    for x in 6..=10 {
        for z in 6..=10 {
            blocks.push((BlockPos::new(x, SKY - 2, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;

    let head = place(&player, center, Item::CarvedPumpkin, 1);
    region.request(RegionRequest::Place(head)).await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(outcome, ActionOutcome::Placed { .. }),
        "{outcome:?}"
    );
    world.ticks(2).await;

    for pos in [center, bottom, arm_a, arm_b, head_pos] {
        assert_eq!(
            block_at(&region, pos).await.map(|s| s.block()),
            Some(Block::Air),
            "{pos:?} is cleared"
        );
    }

    let golems = mobs(&region, EntityType::IronGolem).await;
    assert_eq!(golems.len(), 1, "{golems:?}");
    let golem = &golems[0];
    assert_eq!(golem.position, [8.5, (SKY - 1) as f64, 8.5]);

    // Hurt it first: a full-health golem ignores an iron ingot (nothing to
    // heal), matching the game's own "did health change" check.
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: golem.id,
            attack: EntityAttack {
                attacker: player.id,
                attacker_uuid: 77,
                attacker_position: golem.position,
                attacker_yaw: 0.0,
                attacker_pitch: 0.0,
                damage: 10.0,
                knockback: 0.0,
                weapon: None,
                critical: false,
                magic: false,
                sweep: false,
                part: None,
            },
            sweep: None,
        })
        .await;
    world.ticks(2).await;

    let effect = interact(&region, &player, &mut events, golem.id, Item::IronIngot).await;
    assert!(
        matches!(effect, HandEffect::Consume(1)),
        "an iron ingot heals it: {effect:?}"
    );

    world.shutdown().await;
}

/// Out of water an iron golem keeps its full air: it never runs out and
/// "drowns" for no damage every second, which clients showed as a golem
/// flashing red without losing health.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_iron_golem_on_dry_land_never_runs_out_of_air() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::IronGolem,
        Compound::new(),
        [8.5, (SKY + 1) as f64, 8.5],
    )
    .await;
    let golem = mobs(&region, EntityType::IronGolem).await[0].id;
    world.ticks(400).await;
    let nbt = entity_nbt(&region, golem).await;
    assert_eq!(nbt.get_short("Air"), Some(300));
    assert_eq!(nbt.get_float("Health"), Some(100.0));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pumpkin_on_snow_assembles_a_shearable_snow_golem() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;

    let top = BlockPos::new(8, SKY, 8);
    let bottom = BlockPos::new(8, SKY - 1, 8);
    let head_pos = BlockPos::new(8, SKY + 1, 8);
    place_blocks(
        &world,
        &region,
        [
            (bottom, Block::SnowBlock.default_state()),
            (top, Block::SnowBlock.default_state()),
        ],
    )
    .await;

    let head = place(&player, top, Item::CarvedPumpkin, 1);
    region.request(RegionRequest::Place(head)).await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(outcome, ActionOutcome::Placed { .. }),
        "{outcome:?}"
    );
    world.ticks(2).await;

    for pos in [top, bottom, head_pos] {
        assert_eq!(
            block_at(&region, pos).await.map(|s| s.block()),
            Some(Block::Air),
            "{pos:?} is cleared"
        );
    }

    let golems = mobs(&region, EntityType::SnowGolem).await;
    assert_eq!(golems.len(), 1, "{golems:?}");

    let effect = interact(&region, &player, &mut events, golems[0].id, Item::Shears).await;
    assert!(
        matches!(effect, HandEffect::Damage(1)),
        "shears take its pumpkin: {effect:?}"
    );

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pumpkin_on_a_weathered_copper_block_assembles_a_copper_golem_and_a_matching_chest() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;

    let body = BlockPos::new(8, SKY, 8);
    let head_pos = BlockPos::new(8, SKY + 1, 8);
    place_blocks(
        &world,
        &region,
        [(body, Block::WeatheredCopper.default_state())],
    )
    .await;

    // Simulate a player building the golem: place the pumpkin on top of
    // the block of copper, the way one is built in survival.
    let head = place(&player, body, Item::CarvedPumpkin, 1);
    region.request(RegionRequest::Place(head)).await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(outcome, ActionOutcome::Placed { .. }),
        "{outcome:?}"
    );
    world.ticks(2).await;

    assert_eq!(
        block_at(&region, head_pos).await.map(|s| s.block()),
        Some(Block::Air),
        "the head is consumed"
    );
    assert_eq!(
        block_at(&region, body).await.map(|s| s.block()),
        Some(Block::WeatheredCopperChest),
        "the block of copper becomes a matching copper chest, not air"
    );

    let golems = mobs(&region, EntityType::CopperGolem).await;
    assert_eq!(golems.len(), 1, "{golems:?}");
    assert_eq!(golems[0].position[0], 8.5);
    assert_eq!(golems[0].position[2], 8.5);
    assert!(
        (golems[0].position[1] - (SKY + 1) as f64).abs() < 0.5,
        "spawns at the pumpkin's position, settling onto the new chest: {golems:?}"
    );

    let nbt = entity_nbt(&region, golems[0].id).await;
    assert_eq!(
        nbt.get_str("weather_state"),
        Some("weathered"),
        "the golem's oxidation matches the block of copper used"
    );

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_copper_golem_sorts_by_item_type_across_several_chests() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;

    place_blocks(&world, &region, pen(0, 10)).await;

    let copper = BlockPos::new(2, SKY + 1, 2);
    // An ineligible chest (already holds a different item) sits right next
    // to the source, closer than the eligible one, so the golem must visit
    // and reject it before finding the chest that actually takes diamonds.
    let ineligible = BlockPos::new(3, SKY + 1, 2);
    let eligible = BlockPos::new(9, SKY + 1, 9);
    place_blocks(
        &world,
        &region,
        [
            (copper, Block::CopperChest.default_state()),
            (ineligible, Block::Chest.default_state()),
            (eligible, Block::TrappedChest.default_state()),
        ],
    )
    .await;

    seed_chest(
        &region,
        &player,
        &mut events,
        copper,
        &[
            (0, ItemStack::mint(Item::Diamond, 16, MintReason::Test)),
            (1, ItemStack::mint(Item::Emerald, 5, MintReason::Test)),
        ],
        1,
    )
    .await;
    seed_chest(
        &region,
        &player,
        &mut events,
        ineligible,
        &[(0, ItemStack::mint(Item::GoldIngot, 3, MintReason::Test))],
        2,
    )
    .await;

    summon(
        &region,
        EntityType::CopperGolem,
        Compound::new(),
        [2.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let golems = mobs(&region, EntityType::CopperGolem).await;
    assert_eq!(golems.len(), 1, "{golems:?}");

    // Poll instead of ticking a fixed count: stop the instant the diamonds
    // land, before the golem can start a second trip for the emeralds
    // (which neither destination chest would ever accept).
    let mut sequence = 3;
    let mut filled = (0..27).map(|_| None).collect();
    for _ in 0..800 {
        world.ticks(1).await;
        sequence += 1;
        filled = chest_contents(&region, &player, &mut events, eligible, sequence).await;
        if filled[0].is_some() {
            break;
        }
    }
    assert_eq!(
        filled[0],
        Some(ItemStack::mint(Item::Diamond, 16, MintReason::Test)),
        "all 16 diamonds (the transport cap) land in the chest that will take them"
    );
    assert!(
        filled[1..].iter().all(Option::is_none),
        "the emeralds never follow, since neither destination accepts them: {filled:?}"
    );

    sequence += 1;
    let skipped = chest_contents(&region, &player, &mut events, ineligible, sequence).await;
    assert_eq!(
        skipped[0],
        Some(ItemStack::mint(Item::GoldIngot, 3, MintReason::Test)),
        "the golem never touches a chest holding a different item: {skipped:?}"
    );

    sequence += 1;
    let source = chest_contents(&region, &player, &mut events, copper, sequence).await;
    assert_eq!(
        source[0], None,
        "the copper chest's diamonds are gone: {source:?}"
    );
    assert_eq!(
        source[1],
        Some(ItemStack::mint(Item::Emerald, 5, MintReason::Test)),
        "its emeralds are untouched so far: {source:?}"
    );

    world.shutdown().await;
}

/// The lid events (position, open count) a viewer got in `events`.
fn lid_events(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> Vec<(BlockPos, u8)> {
    let id = tuff_vanilla_data::packet::play::Clientbound::BlockEvent.id();
    let mut found = Vec::new();
    while let Ok(event) = events.try_recv() {
        let PlayerEvent::ChunkPackets { packets, .. } = event else {
            continue;
        };
        for frame in packets.iter().filter(|f| framed_packet_id(f) == id) {
            let mut reader = framed_payload(frame);
            reader.read_var_int().unwrap();
            let packed = reader.read_long().unwrap();
            let pos = BlockPos::new(
                (packed >> 38) as i32,
                (packed << 52 >> 52) as i32,
                (packed << 26 >> 38) as i32,
            );
            reader.read_unsigned_byte().unwrap();
            found.push((pos, reader.read_unsigned_byte().unwrap()));
        }
    }
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_copper_golem_holds_each_chest_lid_open_while_it_works() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;

    place_blocks(&world, &region, pen(0, 10)).await;
    let copper = BlockPos::new(2, SKY + 1, 2);
    let eligible = BlockPos::new(9, SKY + 1, 9);
    place_blocks(
        &world,
        &region,
        [
            (copper, Block::CopperChest.default_state()),
            (eligible, Block::Chest.default_state()),
        ],
    )
    .await;
    seed_chest(
        &region,
        &player,
        &mut events,
        copper,
        &[(0, ItemStack::mint(Item::Diamond, 16, MintReason::Test))],
        1,
    )
    .await;
    lid_events(&mut events);

    summon(
        &region,
        EntityType::CopperGolem,
        Compound::new(),
        [2.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    world.ticks(600).await;

    let lids = lid_events(&mut events);
    for chest in [copper, eligible] {
        let counts: Vec<u8> = lids
            .iter()
            .filter(|(pos, _)| *pos == chest)
            .map(|(_, count)| *count)
            .collect();
        assert!(
            counts.contains(&1),
            "{chest:?} opens: {counts:?} of {lids:?}"
        );
        assert_eq!(
            counts.last(),
            Some(&0),
            "{chest:?} shuts again: {counts:?} of {lids:?}"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_copper_golem_fills_a_double_chest_split_across_a_region_border() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west_chunk = ChunkPos::new(31, 0);
    let east_chunk = ChunkPos::new(32, 0);
    let west = world.region(west_chunk.region()).await;
    let east = world.region(east_chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&west, &player, &mut events, &[west_chunk]).await;
    subscribe(&east, &player, &mut events, &[east_chunk]).await;

    let mut floor = Vec::new();
    for x in 503..=520 {
        for z in 0..=10 {
            floor.push((BlockPos::new(x, SKY, z), Block::GrassBlock.default_state()));
            if !(504..=519).contains(&x) || !(1..=9).contains(&z) {
                floor.push((BlockPos::new(x, SKY + 1, z), Block::Stone.default_state()));
                floor.push((BlockPos::new(x, SKY + 2, z), Block::Stone.default_state()));
            }
        }
    }
    let (west_floor, east_floor): (Vec<_>, Vec<_>) =
        floor.into_iter().partition(|(p, _)| p.x < 512);
    place_blocks(&world, &west, west_floor).await;
    place_blocks(&world, &east, east_floor).await;

    let copper = BlockPos::new(506, SKY + 1, 5);
    let near_half = BlockPos::new(511, SKY + 1, 5);
    let far_half = BlockPos::new(512, SKY + 1, 5);
    place_blocks(
        &world,
        &west,
        [
            (copper, Block::CopperChest.default_state()),
            (
                near_half,
                "minecraft:chest[facing=north,type=left]".parse().unwrap(),
            ),
        ],
    )
    .await;
    place_blocks(
        &world,
        &east,
        [(
            far_half,
            "minecraft:chest[facing=north,type=right]".parse().unwrap(),
        )],
    )
    .await;

    seed_chest(
        &west,
        &player,
        &mut events,
        copper,
        &[(0, ItemStack::mint(Item::Diamond, 16, MintReason::Test))],
        1,
    )
    .await;

    summon(
        &west,
        EntityType::CopperGolem,
        Compound::new(),
        [507.5, SKY as f64 + 1.0, 5.5],
    )
    .await;

    let mut sequence = 1;
    let mut diamonds = 0;
    for _ in 0..800 {
        world.ticks(1).await;
        sequence += 1;
        let merged = chest_contents(&west, &player, &mut events, near_half, sequence).await;
        diamonds = merged
            .iter()
            .flatten()
            .filter(|stack| stack.item == Item::Diamond)
            .map(|stack| i32::from(stack.count()))
            .sum();
        if diamonds > 0 {
            break;
        }
    }
    assert_eq!(diamonds, 16, "the golem filled the double chest");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fully_oxidized_copper_golem_turns_into_a_statue_that_an_axe_reanimates() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;

    place_blocks(&world, &region, pen(0, 6)).await;
    let feet = BlockPos::new(3, SKY + 1, 3);
    summon(
        &region,
        EntityType::CopperGolem,
        Compound::new().with("weather_state", "oxidized"),
        [feet.x as f64 + 0.5, feet.y as f64, feet.z as f64 + 0.5],
    )
    .await;
    let golems = mobs(&region, EntityType::CopperGolem).await;
    assert_eq!(golems.len(), 1, "{golems:?}");

    let mut turned = None;
    for _ in 0..4000 {
        world.ticks(1).await;
        if mobs(&region, EntityType::CopperGolem).await.is_empty() {
            turned = block_at(&region, feet).await;
            break;
        }
    }
    let statue = turned.expect("a fully oxidized golem in the open eventually statues");
    assert_eq!(statue.block(), Block::OxidizedCopperGolemStatue);

    // A statue at any stage but unaffected refuses the axe (it just
    // scrapes, via the generic wax/oxidation transform).
    assert!(statue.facing().is_some(), "the statue keeps a facing");

    // A directly built unaffected, unwaxed statue reanimates.
    let unaffected = BlockPos::new(5, SKY + 1, 3);
    place_blocks(
        &world,
        &region,
        [(unaffected, Block::CopperGolemStatue.default_state())],
    )
    .await;
    let mut click = place(&player, unaffected, Item::DiamondAxe, 1);
    click.item = None;
    region.request(RegionRequest::Place(click)).await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 1,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(
        matches!(
            outcome,
            ActionOutcome::Used {
                effect: HandEffect::Damage(1),
                ..
            }
        ),
        "{outcome:?}"
    );
    world.ticks(2).await;
    assert_eq!(
        block_at(&region, unaffected).await.map(|s| s.block()),
        Some(Block::Air),
        "the statue block is removed"
    );
    let reanimated = mobs(&region, EntityType::CopperGolem).await;
    assert_eq!(reanimated.len(), 1, "{reanimated:?}");

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn iron_golems_turn_on_players_the_villagers_hate() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(0, 12)).await;
    let gossip = Compound::new()
        .with("Target", Tag::uuid(77))
        .with("Type", "major_negative")
        .with("Value", 100);
    summon(
        &region,
        EntityType::Villager,
        Compound::new().with("Gossips", Tag::List(vec![Tag::Compound(gossip)])),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    summon(
        &region,
        EntityType::IronGolem,
        Compound::new(),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [9.5, SKY as f64 + 1.0, 9.5],
            crate::world::messages::PlayerLooks::default(),
        ))
        .await;
    let hit = next_hit(&world, &mut events, 400).await;
    assert_eq!(
        hit.attacker.map(|a| a.kind),
        Some(EntityType::IronGolem),
        "the golem defends the village"
    );
    assert!((7.5..=21.5).contains(&hit.amount), "{}", hit.amount);
    assert_eq!(hit.push, Some([0.0, 0.400_000_005_960_464_5, 0.0]));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn iron_golems_leave_well_liked_players_alone() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(0, 12)).await;
    summon(
        &region,
        EntityType::Villager,
        Compound::new(),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    summon(
        &region,
        EntityType::IronGolem,
        Compound::new(),
        [6.5, SKY as f64 + 1.0, 6.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [9.5, SKY as f64 + 1.0, 9.5],
            crate::world::messages::PlayerLooks::default(),
        ))
        .await;
    for _ in 0..200 {
        world.ticks(1).await;
        while let Ok(event) = events.try_recv() {
            assert!(
                !matches!(event, PlayerEvent::EntityHurt(_)),
                "an unprovoked golem attacked"
            );
        }
    }
    world.shutdown().await;
}
