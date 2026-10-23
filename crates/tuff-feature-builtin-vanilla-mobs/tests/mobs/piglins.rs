//! Piglins and zombified piglins beyond the basics in `hostile`.

use super::zombies::attack;
use super::*;
use crate::world::messages::PlayerLooks;
use tuff_vanilla_data::registry::EntityType;

fn floor_across(x0: i32, x1: i32) -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in x0..=x1 {
        for z in 0..=20 {
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
        }
    }
    blocks
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zombified_piglins_alert_their_neighbours_across_a_region_border() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (west_chunk, east_chunk) = (ChunkPos::new(31, 0), ChunkPos::new(32, 0));
    let west = world.region(west_chunk.region()).await;
    let east = world.region(east_chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&west, &player, &mut events, &[west_chunk]).await;
    subscribe(&east, &player, &mut events, &[east_chunk]).await;
    place_blocks(&world, &west, floor_across(500, 511)).await;
    place_blocks(&world, &east, floor_across(512, 524)).await;
    let y = SKY as f64 + 1.0;
    summon(
        &west,
        EntityType::ZombifiedPiglin,
        Compound::new(),
        [510.5, y, 8.5],
    )
    .await;
    summon(
        &east,
        EntityType::ZombifiedPiglin,
        Compound::new(),
        [514.5, y, 8.5],
    )
    .await;
    world.ticks(5).await;
    let hurt = mobs(&west, EntityType::ZombifiedPiglin).await[0].id;
    let neighbour = mobs(&east, EntityType::ZombifiedPiglin).await[0].id;
    let position = [506.5, y, 8.5];
    west.request(player_at(&player, position, PlayerLooks::default()))
        .await;
    east.request(player_at(&player, position, PlayerLooks::default()))
        .await;
    world.ticks(2).await;
    west.request(attack(&player, hurt, position)).await;
    for _ in 0..40 {
        world.ticks(1).await;
        let nbt = entity_nbt(&east, neighbour).await.unwrap();
        if nbt.get_uuid("angry_at") == Some(77) {
            world.shutdown().await;
            return;
        }
    }
    panic!("the neighbour never turned on the attacker");
}

fn picking_up() -> Compound {
    Compound::new()
        .with("IsImmuneToZombification", true)
        .with("CanPickUpLoot", true)
}

async fn drop_item(region: &RegionHandle, item: Item, count: u8, at: [f64; 3]) {
    summon(
        region,
        EntityType::Item,
        Compound::new().with(
            "Item",
            tuff_item::item::ItemPicture::new(item, count).to_nbt(),
        ),
        at,
    )
    .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piglins_pocket_gold_nuggets_and_keep_them_when_saved() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    summon(&region, EntityType::Piglin, picking_up(), [4.5, y, 4.5]).await;
    drop_item(&region, Item::GoldNugget, 5, [6.5, y, 4.5]).await;
    region
        .request(player_at(&player, [12.5, y, 12.5], in_gold()))
        .await;
    let piglin = mobs(&region, EntityType::Piglin).await[0].id;
    for _ in 0..300 {
        world.ticks(1).await;
        let nbt = entity_nbt(&region, piglin).await.unwrap();
        if let Some(Tag::List(list)) = nbt.get("Inventory")
            && let Some(stack) = list.first().and_then(|t| t.as_compound())
        {
            assert_eq!(stack.get_str("id"), Some("minecraft:gold_nugget"));
            assert_eq!(stack.get_int("count"), Some(5));
            world.shutdown().await;
            return;
        }
    }
    panic!("the nuggets never went into a pocket");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piglins_admire_other_gold_items_and_hold_them() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    summon(&region, EntityType::Piglin, picking_up(), [4.5, y, 4.5]).await;
    drop_item(&region, Item::GoldBlock, 1, [6.5, y, 4.5]).await;
    region
        .request(player_at(&player, [12.5, y, 12.5], in_gold()))
        .await;
    let piglin = mobs(&region, EntityType::Piglin).await[0].id;
    for _ in 0..400 {
        world.ticks(1).await;
        let nbt = entity_nbt(&region, piglin).await.unwrap();
        let held = nbt
            .get("equipment")
            .and_then(|e| e.as_compound())
            .and_then(|e| e.get("mainhand"))
            .and_then(|m| m.as_compound())
            .and_then(|m| m.get_str("id"));
        if held == Some("minecraft:gold_block") {
            world.shutdown().await;
            return;
        }
    }
    panic!("the gold block was never taken up");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piglins_run_from_zombified_piglins() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    summon(&region, EntityType::Piglin, picking_up(), [8.5, y, 8.5]).await;
    summon(
        &region,
        EntityType::ZombifiedPiglin,
        Compound::new().with("NoAI", true),
        [6.5, y, 8.5],
    )
    .await;
    world.ticks(60).await;
    let piglin = mobs(&region, EntityType::Piglin).await[0].position;
    let distance = crate::world::distance_sq(piglin, [6.5, y, 8.5]).sqrt();
    assert!(distance > 5.5, "ran off: {distance}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn baby_piglins_never_attack() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    summon(
        &region,
        EntityType::Piglin,
        picking_up().with("IsBaby", true),
        [8.5, y, 8.5],
    )
    .await;
    region
        .request(player_at(&player, [9.5, y, 8.5], PlayerLooks::default()))
        .await;
    world.ticks(200).await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::EntityHurt(_)),
            "babies are harmless"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piglins_with_crossbows_shoot_players_without_gold() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    let crossbow = tuff_item::item::ItemPicture::new(Item::Crossbow, 1).to_nbt();
    summon(
        &region,
        EntityType::Piglin,
        picking_up().with("equipment", Compound::new().with("mainhand", crossbow)),
        [3.5, y, 8.5],
    )
    .await;
    region
        .request(player_at(&player, [12.5, y, 8.5], PlayerLooks::default()))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.projectile, Some(EntityType::Arrow));
    assert_eq!(hit.attacker.map(|a| a.kind), Some(EntityType::Piglin));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piglins_hunt_adult_hoglins_after_a_rest() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    let sword = tuff_item::item::ItemPicture::new(Item::GoldenSword, 1).to_nbt();
    summon(
        &region,
        EntityType::Piglin,
        picking_up().with("equipment", Compound::new().with("mainhand", sword)),
        [4.5, y, 4.5],
    )
    .await;
    summon(
        &region,
        EntityType::Hoglin,
        Compound::new()
            .with("IsImmuneToZombification", true)
            .with("Health", 6.0f32),
        [11.5, y, 11.5],
    )
    .await;
    wait_for(&world, &region, 4000, |found| {
        !found.iter().any(|s| s.kind == EntityType::Hoglin)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piglins_open_wooden_doors_on_the_way_to_a_player() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let door = BlockPos::new(8, SKY + 1, 7);
    let oak = Block::OakDoor
        .default_state()
        .with("facing", "east")
        .unwrap();
    let mut blocks = roofed_pen(2, 13);
    for z in 2..=13 {
        for y in 1..=3 {
            blocks.push((BlockPos::new(8, SKY + y, z), Block::Stone.default_state()));
        }
    }
    blocks.push((door, oak.with("half", "lower").unwrap()));
    blocks.push((
        BlockPos::new(8, SKY + 2, 7),
        oak.with("half", "upper").unwrap(),
    ));
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Piglin,
        Compound::new()
            .with("PersistenceRequired", true)
            .with("IsImmuneToZombification", true),
        [5.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    let position = [11.5, SKY as f64 + 1.0, 7.5];
    region
        .request(player_at(&player, position, PlayerLooks::default()))
        .await;
    world.ticks(1).await;
    // It cannot see through the closed door, so the player provokes it.
    let piglin = mobs(&region, EntityType::Piglin).await[0].id;
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: piglin,
            attack: crate::world::entity::EntityAttack {
                attacker: player.id,
                attacker_uuid: 77,
                attacker_position: position,
                attacker_yaw: 0.0,
                attacker_pitch: 0.0,
                damage: 1.0,
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
    for _ in 0..600 {
        region
            .request(player_at(&player, position, PlayerLooks::default()))
            .await;
        world.ticks(1).await;
        if block_at(&region, door)
            .await
            .is_some_and(|s| s.block() == Block::OakDoor && s.open() == Some(true))
        {
            world.shutdown().await;
            return;
        }
    }
    panic!("the door never opened");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piglins_pocket_picked_up_items_with_their_components() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    summon(&region, EntityType::Piglin, picking_up(), [4.5, y, 4.5]).await;
    let mut named = ItemStack::mint(Item::GoldNugget, 3, MintReason::Test);
    named.components.custom_name = Some(Box::new(tuff_item::text::TextComponent::new("Shiny")));
    summon(
        &region,
        EntityType::Item,
        Compound::new().with("Item", named.to_nbt()),
        [6.5, y, 4.5],
    )
    .await;
    region
        .request(player_at(&player, [12.5, y, 12.5], in_gold()))
        .await;
    let piglin = mobs(&region, EntityType::Piglin).await[0].id;
    for _ in 0..300 {
        world.ticks(1).await;
        let nbt = entity_nbt(&region, piglin).await.unwrap();
        if let Some(Tag::List(list)) = nbt.get("Inventory")
            && let Some(stack) = list.first().and_then(|t| t.as_compound())
        {
            let back = ItemStack::mint_from_nbt(stack, MintReason::Test).expect("a saved stack");
            assert_eq!(back.count(), 3);
            assert_eq!(back.components.custom_name, named.components.custom_name);
            world.shutdown().await;
            return;
        }
    }
    panic!("the nuggets never went into a pocket");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piglins_walk_away_from_soul_lanterns() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = roofed_pen(2, 13);
    let lantern = BlockPos::new(8, SKY + 1, 8);
    blocks.push((lantern, Block::SoulLantern.default_state()));
    place_blocks(&world, &region, blocks).await;
    let y = SKY as f64 + 1.0;
    summon(&region, EntityType::Piglin, picking_up(), [9.5, y, 8.5]).await;
    region
        .request(player_at(&player, [12.5, y, 12.5], in_gold()))
        .await;
    world.ticks(120).await;
    let piglin = mobs(&region, EntityType::Piglin).await[0].position;
    let distance = crate::world::distance_sq(piglin, [8.5, y, 8.5]).sqrt();
    assert!(distance > 3.0, "kept away: {distance}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn baby_piglins_climb_onto_baby_hoglins() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    summon(
        &region,
        EntityType::Hoglin,
        Compound::new()
            .with("IsImmuneToZombification", true)
            .with("NoAI", true)
            .with("Age", -24000),
        [8.5, y, 8.5],
    )
    .await;
    summon(
        &region,
        EntityType::Piglin,
        picking_up().with("IsBaby", true),
        [5.5, y, 8.5],
    )
    .await;
    region
        .request(player_at(&player, [12.5, y, 12.5], in_gold()))
        .await;
    wait_for(&world, &region, 400, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::Piglin && s.position[1] > y + 0.3)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piglins_with_crossbows_back_away_from_close_players() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    let crossbow = tuff_item::item::ItemPicture::new(Item::Crossbow, 1).to_nbt();
    summon(
        &region,
        EntityType::Piglin,
        picking_up().with("equipment", Compound::new().with("mainhand", crossbow)),
        [8.5, y, 8.5],
    )
    .await;
    let at = [5.5, y, 8.5];
    region
        .request(player_at(&player, at, PlayerLooks::default()))
        .await;
    world.ticks(80).await;
    let piglin = mobs(&region, EntityType::Piglin).await[0].position;
    let distance = crate::world::distance_sq(piglin, at).sqrt();
    assert!(distance > 3.5, "backed away: {distance}");
    world.shutdown().await;
}
