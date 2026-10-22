//! Bows, crossbows, tridents and the arrows, bolts and rockets they fire:
//! what their shots carry, how victims take them, and the enchantments
//! that shape them.

use super::*;
use crate::world::entity::{EntityHit, Rebound};
use crate::world::inventory::Inventory;
use crate::world::messages::PlayerLooks;
use crate::world::session::PlayerSession;
use tuff_feature_builtin_vanilla_combat::projectile::arrow::Arrow;
use tuff_item::item::PotionContents;
use tuff_vanilla_data::registry::{EntityType, MobEffect, Potion};

fn owner(player: &PlayerHandle) -> tuff_core::world::projectile::Owner {
    tuff_core::world::projectile::Owner::of(
        crate::world::entity::Culprit::player(player.id, 77, "Tester"),
        Some(player.clone()),
    )
}

fn tipped(potion: Potion) -> ItemStack {
    let mut stack = ItemStack::mint(Item::TippedArrow, 1, MintReason::Test);
    stack.components.potion_contents = Some(Box::new(PotionContents {
        potion: Some(potion),
        ..Default::default()
    }));
    stack
}

/// Shoots `item` as an arrow from the west at a player standing in the
/// pen, and returns the hit the player's session is sent.
async fn shot_at_player(item: &ItemStack) -> EntityHit {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (shooter, mut shooter_events) = PlayerHandle::new(1000);
    let (target, mut events) = PlayerHandle::new(1001);
    subscribe(&region, &shooter, &mut shooter_events, &[chunk]).await;
    subscribe(&region, &target, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    region
        .request(player_at(
            &target,
            [10.5, f64::from(SKY) + 1.0, 7.5],
            PlayerLooks::default(),
        ))
        .await;
    let arrow = Arrow::from_item(
        item.test_copy(),
        world.shared.next_entity_id(),
        [4.5, f64::from(SKY) + 2.0, 7.5],
        [2.5, 0.0, 0.0],
        owner(&shooter),
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(arrow)))
        .await;
    let hit = next_hit(&world, &mut events, 40).await;
    world.shutdown().await;
    hit
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tipped_arrows_carry_an_eighth_of_their_potion_and_stick() {
    let hit = shot_at_player(&tipped(Potion::Poison)).await;
    assert_eq!(hit.projectile, Some(EntityType::Arrow));
    let poison = hit
        .effects
        .iter()
        .find(|e| e.effect == MobEffect::Poison)
        .expect("the potion goes along");
    assert_eq!(poison.duration, 900 / 8);
    assert!(hit.sticks, "a plain arrow stays stuck if it lands");
    assert!(hit.rebound.is_some(), "and bounces off if it does not");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spectral_arrows_make_their_victim_glow() {
    let hit = shot_at_player(&ItemStack::mint(Item::SpectralArrow, 1, MintReason::Test)).await;
    assert_eq!(hit.projectile, Some(EntityType::SpectralArrow));
    let glow = hit
        .effects
        .iter()
        .find(|e| e.effect == MobEffect::Glowing)
        .expect("spectral arrows add glowing");
    assert_eq!(glow.duration, 200);
}

fn arrow_hit(world: &TestWorld, rebound: bool) -> EntityHit {
    let mut hit = EntityHit::new(4.0, &tuff_vanilla_data::damage_type::ARROW);
    hit.projectile = Some(EntityType::Arrow);
    hit.position = Some([0.0, 0.0, 3.0]);
    hit.sticks = true;
    if rebound {
        let arrow = Arrow::new(
            world.shared.next_entity_id(),
            [0.5, 151.0, 0.5],
            [0.0, 0.0, -0.3],
            Default::default(),
        );
        hit.rebound = Some(Rebound::new(Box::new(arrow)));
    }
    hit
}

fn session(world: &TestWorld, id: i32) -> PlayerSession {
    let (session, outbound) = session_with_outbound(world, id);
    std::mem::forget(outbound);
    session
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn landed_arrows_stick_in_players_and_work_loose() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let mut player = session(&world, 1000);
    player.stand_at([0.5, 151.0, 0.5], true);
    player
        .handle_event(PlayerEvent::EntityHurt(Box::new(arrow_hit(&world, false))))
        .await;
    assert_eq!(player.health(), 16.0);
    assert_eq!(player.body.stuck_arrows.count, 1);
    for _ in 0..20 * 29 - 1 {
        player.body.stuck_arrows.tick();
    }
    assert_eq!(player.body.stuck_arrows.count, 1);
    assert!(
        player.body.stuck_arrows.tick(),
        "it falls out after 29 seconds"
    );
    assert_eq!(player.body.stuck_arrows.count, 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raised_shields_send_arrows_back_without_sticking() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let mut player = session(&world, 1000);
    player.stand_at([0.5, 151.0, 0.5], true);
    player.inventory_mut().set_slot(
        Inventory::OFFHAND_SLOT,
        Some(ItemStack::mint(Item::Shield, 1, MintReason::Test)),
    );
    player.hold_up(true, 10);
    player
        .handle_event(PlayerEvent::EntityHurt(Box::new(arrow_hit(&world, true))))
        .await;
    assert_eq!(player.health(), 20.0);
    assert_eq!(player.body.stuck_arrows.count, 0);
    wait_for(&world, &region, 20, |found| {
        found.iter().any(|s| s.kind == EntityType::Arrow)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn arrows_stick_in_the_mobs_they_hurt_and_bounce_off_invulnerable_ones() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new()
            .with("NoAI", true)
            .with("Invulnerable", true),
        [9.5, f64::from(SKY) + 1.0, 7.5],
    )
    .await;
    world.ticks(2).await;
    let arrow = Arrow::new(
        world.shared.next_entity_id(),
        [4.5, f64::from(SKY) + 1.8, 7.5],
        [2.0, 0.0, 0.0],
        owner(&player),
    );
    let first = arrow.base.id;
    region
        .request(RegionRequest::SpawnEntity(Box::new(arrow)))
        .await;
    let found = wait_for(&world, &region, 40, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::Arrow && s.id != first)
    })
    .await;
    let bounced = found.iter().find(|s| s.kind == EntityType::Arrow).unwrap();
    assert!(bounced.position[0] < 9.5, "it flies back the way it came");
    world.shutdown().await;
}

fn enchanted(item: Item, enchantments: &[(&str, i32)]) -> ItemStack {
    let mut stack = ItemStack::mint(item, 1, MintReason::Test);
    stack.components.enchantments = Some(tuff_item::item::Enchantments::new(
        enchantments
            .iter()
            .map(|(id, level)| (format!("minecraft:{id}"), *level)),
    ));
    stack
}

fn release() -> crate::protocol::play::Serverbound {
    crate::protocol::play::Serverbound::PlayerAction {
        status: crate::protocol::play::PlayerActionStatus::ReleaseUseItem,
        position: BlockPos::new(0, 0, 0),
        face: 0,
        sequence: 2,
    }
}

fn use_item() -> crate::protocol::play::Serverbound {
    crate::protocol::play::Serverbound::UseItem {
        off_hand: false,
        sequence: 1,
    }
}

/// A player holding `crossbow` with `ammo` in the next hotbar slot.
fn crossbowman(world: &TestWorld, crossbow: ItemStack, ammo: ItemStack) -> PlayerSession {
    let mut player = session(world, 1000);
    player.stand_at([4.5, 151.0, 6.5], true);
    player
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(0), Some(crossbow));
    player
        .inventory_mut()
        .set_slot(Inventory::hotbar_slot(1), Some(ammo));
    player
}

fn loaded(player: &PlayerSession) -> Vec<ItemStack> {
    player
        .inventory()
        .hand(false)
        .and_then(|s| {
            s.components
                .charged_projectiles
                .as_ref()
                .map(|p| p.iter().map(ItemStack::test_copy).collect())
        })
        .unwrap_or_default()
}

fn ammo_left(player: &PlayerSession) -> u8 {
    player.inventory().slots()[Inventory::hotbar_slot(1)]
        .as_ref()
        .map_or(0, |s| s.count())
}

async fn count_of(world: &TestWorld, region: &RegionHandle, kind: EntityType, n: usize) {
    wait_for(world, region, 40, |found| {
        found.iter().filter(|s| s.kind == kind).count() == n
    })
    .await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crossbows_load_over_their_charge_time_and_fire_on_the_next_use() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let mut player = crossbowman(
        &world,
        ItemStack::mint(Item::Crossbow, 1, MintReason::Test),
        ItemStack::mint(Item::Arrow, 2, MintReason::Test),
    );
    player.handle_packet(use_item()).await;
    assert!(player.is_using(), "an empty crossbow starts loading");
    for _ in 0..25 {
        player.run_tick().await;
    }
    assert!(loaded(&player).is_empty(), "a full charge is 25 ticks");
    player.run_tick().await;
    assert_eq!(loaded(&player).len(), 1);
    assert_eq!(ammo_left(&player), 1);
    player.handle_packet(release()).await;
    assert!(!player.is_using());
    player.handle_packet(use_item()).await;
    assert!(loaded(&player).is_empty(), "firing empties it");
    assert_eq!(player.inventory().hand(false).unwrap().damage(), 1);
    count_of(&world, &region, EntityType::Arrow, 1).await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multishot_loads_one_arrow_and_fires_three_and_quick_charge_loads_sooner() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let crossbow = enchanted(Item::Crossbow, &[("multishot", 1), ("quick_charge", 3)]);
    let mut player = crossbowman(
        &world,
        crossbow,
        ItemStack::mint(Item::Arrow, 5, MintReason::Test),
    );
    player.handle_packet(use_item()).await;
    for _ in 0..11 {
        player.run_tick().await;
    }
    assert_eq!(loaded(&player).len(), 3, "loaded within ten ticks");
    assert_eq!(ammo_left(&player), 4, "the side bolts are free");
    player.handle_packet(release()).await;
    player.handle_packet(use_item()).await;
    assert_eq!(player.inventory().hand(false).unwrap().damage(), 3);
    count_of(&world, &region, EntityType::Arrow, 3).await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crossbows_load_rockets_from_the_off_hand_and_fire_them() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let mut player = crossbowman(
        &world,
        ItemStack::mint(Item::Crossbow, 1, MintReason::Test),
        ItemStack::mint(Item::Arrow, 5, MintReason::Test),
    );
    player.inventory_mut().set_slot(
        Inventory::OFFHAND_SLOT,
        Some(ItemStack::mint(Item::FireworkRocket, 2, MintReason::Test)),
    );
    player.handle_packet(use_item()).await;
    for _ in 0..26 {
        player.run_tick().await;
    }
    assert_eq!(loaded(&player)[0].item(), Item::FireworkRocket);
    assert_eq!(ammo_left(&player), 5, "held rockets come before arrows");
    player.handle_packet(release()).await;
    player.handle_packet(use_item()).await;
    assert_eq!(
        player.inventory().hand(false).unwrap().damage(),
        3,
        "a rocket wears a crossbow by three"
    );
    count_of(&world, &region, EntityType::FireworkRocket, 1).await;
    world.shutdown().await;
}

fn starry_rocket(stars: usize) -> ItemStack {
    let mut rocket = ItemStack::mint(Item::FireworkRocket, 1, MintReason::Test);
    rocket.components.fireworks = Some(Box::new(tuff_item::item::decor::Fireworks {
        flight_duration: 1,
        explosions: vec![
            tuff_item::item::decor::FireworkExplosion::from_nbt(
                &tuff_nbt::Tag::Compound(Compound::new().with("shape", "small_ball"))
            )
            .unwrap();
            stars
        ],
    }));
    rocket
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rockets_with_stars_burst_on_what_they_hit_and_hurt_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (target, mut events) = PlayerHandle::new(1001);
    subscribe(&region, &target, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    region
        .request(player_at(
            &target,
            [10.5, f64::from(SKY) + 1.0, 7.5],
            PlayerLooks::default(),
        ))
        .await;
    let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(1);
    let rocket = tuff_feature_builtin_vanilla_combat::projectile::firework::FireworkRocket::shot(
        world.shared.next_entity_id(),
        [5.5, f64::from(SKY) + 1.5, 7.5],
        [1.6, 0.0, 0.0],
        starry_rocket(2),
        Default::default(),
        &mut rng,
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(rocket)))
        .await;
    let hit = next_hit(&world, &mut events, 20).await;
    assert_eq!(hit.kind.name, "minecraft:fireworks");
    assert!(
        hit.amount > 0.0 && hit.amount <= 9.0,
        "at most 5 + 2 per star: {}",
        hit.amount
    );
    count_of(&world, &region, EntityType::FireworkRocket, 0).await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rockets_used_on_a_block_launch_from_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    region
        .request(RegionRequest::Place(offering(place(
            &player,
            BlockPos::new(7, SKY, 7),
            Item::FireworkRocket,
            1,
        ))))
        .await;
    let found = wait_for(&world, &region, 5, |found| {
        found.iter().any(|s| s.kind == EntityType::FireworkRocket)
    })
    .await;
    let rocket = found
        .iter()
        .find(|s| s.kind == EntityType::FireworkRocket)
        .unwrap();
    assert!(
        rocket.position[1] > f64::from(SKY) + 1.0,
        "it starts on top"
    );
    world.ticks(40).await;
    count_of(&world, &region, EntityType::FireworkRocket, 0).await;
    world.shutdown().await;
}

fn trident_with(enchantments: &[(&str, i32)]) -> ItemStack {
    enchanted(Item::Trident, enchantments)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drowned_with_tridents_throw_them() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let overworld = crate::world::clock::clock_id("minecraft:overworld").unwrap();
    world
        .clocks
        .send_modify(|clocks| clocks.get_mut(overworld).set(18000));
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let trident = ItemStack::mint(Item::Trident, 1, MintReason::Test).to_nbt();
    summon(
        &region,
        EntityType::Drowned,
        Compound::new().with("equipment", Compound::new().with("mainhand", trident)),
        [3.5, f64::from(SKY) + 1.0, 3.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [10.5, f64::from(SKY) + 1.0, 10.5],
            PlayerLooks::default(),
        ))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.kind.name, "minecraft:trident");
    assert_eq!(hit.projectile, Some(EntityType::Trident));
    assert_eq!(hit.amount, 8.0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn players_throw_tridents_held_back_long_enough() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let mut player = session(&world, 1000);
    player.stand_at([4.5, 151.0, 6.5], true);
    player.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(ItemStack::mint(Item::Trident, 1, MintReason::Test)),
    );
    player.handle_packet(use_item()).await;
    for _ in 0..5 {
        player.run_tick().await;
    }
    player.handle_packet(release()).await;
    assert!(
        player.inventory().hand(false).is_some(),
        "not held back for half a second"
    );
    player.handle_packet(use_item()).await;
    for _ in 0..10 {
        player.run_tick().await;
    }
    player.handle_packet(release()).await;
    assert!(player.inventory().hand(false).is_none(), "it was thrown");
    let found = wait_for(&world, &region, 20, |found| {
        found.iter().any(|s| s.kind == EntityType::Trident)
    })
    .await;
    assert!(found.iter().any(|s| s.kind == EntityType::Trident));

    let mut dry = session(&world, 1001);
    dry.stand_at([4.5, 151.0, 6.5], true);
    dry.inventory_mut().set_slot(
        Inventory::hotbar_slot(0),
        Some(trident_with(&[("riptide", 1)])),
    );
    dry.handle_packet(use_item()).await;
    assert!(!dry.is_using(), "riptide needs water or rain");
    world.weather.send_modify(|weather| {
        weather.raining = true;
        weather.rain_level = 1.0;
    });
    dry.handle_packet(use_item()).await;
    assert!(
        !dry.is_using(),
        "rain that does not fall on the player is no use"
    );
    dry.surroundings.contact.in_rain = true;
    dry.handle_packet(use_item()).await;
    assert!(dry.is_using());
    for _ in 0..10 {
        dry.run_tick().await;
    }
    dry.handle_packet(release()).await;
    assert!(dry.inventory().hand(false).is_some(), "riptide keeps it");
    assert!(dry.motion.spin_attack_ticks > 0, "and launches into a spin");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loyal_tridents_fly_home_to_be_picked_up() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    for y in SKY + 1..=SKY + 3 {
        for z in 2..=13 {
            blocks.push((BlockPos::new(12, y, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    region
        .request(player_at(
            &player,
            [4.5, f64::from(SKY) + 1.0, 7.5],
            PlayerLooks::default(),
        ))
        .await;
    let trident = Arrow::trident(
        trident_with(&[("loyalty", 3)]),
        world.shared.next_entity_id(),
        [5.5, f64::from(SKY) + 2.0, 7.5],
        [2.0, 0.0, 0.0],
        owner(&player),
    );
    let id = trident.base.id;
    region
        .request(RegionRequest::SpawnEntity(Box::new(trident)))
        .await;
    let offered = ticking_expect(&world, &mut events, 200, |event| match event {
        PlayerEvent::OfferItem { item, stack, .. } if item == id => Some(stack),
        _ => None,
    })
    .await;
    assert_eq!(offered.item(), Item::Trident);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn channeling_tridents_call_lightning_on_what_they_hit_in_a_storm() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (shooter, mut shooter_events) = PlayerHandle::new(1000);
    let (target, mut events) = PlayerHandle::new(1001);
    subscribe(&region, &shooter, &mut shooter_events, &[chunk]).await;
    subscribe(&region, &target, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    world.ticks(20).await;
    world.weather.send_modify(|weather| {
        weather.raining = true;
        weather.thundering = true;
        weather.rain_level = 1.0;
        weather.thunder_level = 1.0;
    });
    region
        .request(player_at(
            &target,
            [10.5, f64::from(SKY) + 1.0, 7.5],
            PlayerLooks::default(),
        ))
        .await;
    let trident = Arrow::trident(
        trident_with(&[("channeling", 1)]),
        world.shared.next_entity_id(),
        [4.5, f64::from(SKY) + 2.0, 7.5],
        [2.5, 0.0, 0.0],
        owner(&shooter),
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(trident)))
        .await;
    wait_for(&world, &region, 20, |found| {
        found.iter().any(|s| s.kind == EntityType::LightningBolt)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_channeling_trident_striking_a_lightning_rod_in_a_storm_calls_lightning() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (shooter, mut shooter_events) = PlayerHandle::new(1000);
    subscribe(&region, &shooter, &mut shooter_events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    blocks.push((
        BlockPos::new(10, SKY + 1, 7),
        Block::LightningRod.default_state(),
    ));
    blocks.push((
        BlockPos::new(10, SKY + 2, 7),
        Block::LightningRod.default_state(),
    ));
    place_blocks(&world, &region, blocks).await;
    world.ticks(20).await;
    let throw = |world: &TestWorld| {
        Arrow::trident(
            trident_with(&[("channeling", 1)]),
            world.shared.next_entity_id(),
            [4.5, f64::from(SKY) + 2.5, 7.5],
            [2.5, 0.0, 0.0],
            owner(&shooter),
        )
    };
    region
        .request(RegionRequest::SpawnEntity(Box::new(throw(&world))))
        .await;
    world.ticks(20).await;
    assert!(
        mobs(&region, EntityType::LightningBolt).await.is_empty(),
        "no lightning in clear weather"
    );
    world.weather.send_modify(|weather| {
        weather.raining = true;
        weather.thundering = true;
        weather.rain_level = 1.0;
        weather.thunder_level = 1.0;
    });
    region
        .request(RegionRequest::SpawnEntity(Box::new(throw(&world))))
        .await;
    wait_for(&world, &region, 20, |found| {
        found.iter().any(|s| s.kind == EntityType::LightningBolt)
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spinning_player_running_into_a_mob_is_told_to_hit_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", true),
        [7.5, f64::from(SKY) + 1.0, 7.5],
    )
    .await;
    world.ticks(2).await;
    let cow = mobs(&region, EntityType::Cow).await[0].id;
    let spinning = PlayerLooks {
        spin_attack: true,
        ..Default::default()
    };
    region
        .request(player_at(
            &player,
            [7.6, f64::from(SKY) + 1.0, 7.5],
            spinning,
        ))
        .await;
    let touched = ticking_expect(&world, &mut events, 5, |event| match event {
        PlayerEvent::SpinAttackTouched { target } => Some(target),
        _ => None,
    })
    .await;
    assert_eq!(touched, cow);
    world.shutdown().await;
}

async fn pearl_holds(region: &RegionHandle, chunk: ChunkPos) -> bool {
    let (reply, answer) = oneshot::channel();
    let at = BlockPos::new(chunk.x * 16, 64, chunk.z * 16);
    region
        .request(RegionRequest::WhenLoaded {
            min: at,
            max: at,
            run: crate::world::region_api::RegionTask::new(move |world| {
                let held = world
                    .state()
                    .get::<tuff_feature_builtin_vanilla_combat::world::region::pearls::PearlTickets>()
                    .holds(chunk);
                let _ = reply.send(held);
            }),
        })
        .await;
    answer.await.unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn flying_pearls_hold_their_chunks_until_a_while_after_they_land() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let pearl = tuff_feature_builtin_vanilla_combat::projectile::thrown::Thrown::new(
        EntityType::EnderPearl,
        world.shared.next_entity_id(),
        [7.5, f64::from(SKY) + 30.0, 7.5],
        [0.0, 0.0, 0.0],
        ItemStack::mint(Item::EnderPearl, 1, MintReason::Test),
        owner(&player),
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(pearl)))
        .await;
    world.ticks(2).await;
    assert!(pearl_holds(&region, chunk).await);
    assert!(
        pearl_holds(&region, ChunkPos::new(-1, -1)).await,
        "and its neighbours"
    );
    wait_for(&world, &region, 100, |found| {
        found.iter().all(|s| s.kind != EntityType::EnderPearl)
    })
    .await;
    world.ticks(41).await;
    assert!(!pearl_holds(&region, chunk).await, "let go once it landed");
    world.shutdown().await;
}
