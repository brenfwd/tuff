//! Projectiles meeting the world: piercing arrows bouncing off what they
//! fail to hurt, blocks reacting across region borders, big dripleaves,
//! mobs' ammo and crossbows, rocket bursts, and pearls that leave with
//! their thrower.

use super::*;
use crate::world::entity::Culprit;
use crate::world::messages::PlayerLooks;
use crate::world::session::PlayerSession;
use tuff_core::world::projectile::Owner;
use tuff_feature_builtin_vanilla_combat::projectile::arrow::Arrow;
use tuff_feature_builtin_vanilla_combat::projectile::firework::FireworkRocket;
use tuff_feature_builtin_vanilla_combat::projectile::thrown::Thrown;
use tuff_item::item::{Enchantments, PotionContents};
use tuff_vanilla_data::registry::{EntityType, MobEffect, Potion};

fn owner(player: &PlayerHandle) -> Owner {
    Owner::of(
        Culprit::player(player.id, player.id as u128, format!("Player{}", player.id)),
        Some(player.clone()),
    )
}

fn enchanted(item: Item, enchantments: &[(&str, i32)]) -> ItemStack {
    let mut stack = ItemStack::mint(item, 1, MintReason::Test);
    stack.components.enchantments = Some(Enchantments::new(
        enchantments
            .iter()
            .map(|(id, level)| (format!("minecraft:{id}"), *level)),
    ));
    stack
}

fn tilt(state: Option<tuff_vanilla_data::BlockState>) -> Option<&'static str> {
    state.and_then(|s| s.get("tilt"))
}

fn dripleaf() -> tuff_vanilla_data::BlockState {
    "minecraft:big_dripleaf[facing=west,tilt=none,waterlogged=false]"
        .parse()
        .unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piercing_arrows_bounce_off_what_they_fail_to_hurt() {
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
    let mut arrow = Arrow::new(
        world.shared.next_entity_id(),
        [4.5, f64::from(SKY) + 1.8, 7.5],
        [2.0, 0.0, 0.0],
        owner(&player),
    );
    arrow.pierce = 2;
    let first = arrow.base.id;
    region
        .request(RegionRequest::SpawnEntity(Box::new(arrow)))
        .await;
    let found = wait_for(&world, &region, 40, |found| {
        let arrows: Vec<_> = found
            .iter()
            .filter(|s| s.kind == EntityType::Arrow)
            .collect();
        arrows.len() == 1 && arrows[0].id != first
    })
    .await;
    let bounced = found.iter().find(|s| s.kind == EntityType::Arrow).unwrap();
    assert!(bounced.position[0] < 9.5, "it flies back the way it came");
    world.ticks(5).await;
    let arrows = snapshots(&region)
        .await
        .into_iter()
        .filter(|s| s.kind == EntityType::Arrow)
        .count();
    assert_eq!(
        arrows, 1,
        "the piercing arrow is gone, only its bounce flies"
    );
    let id = bounced.id;
    let carried = region
        .ask(move |world| {
            world
                .entity(id)
                .and_then(|tracked| tracked.entity.downcast_ref::<Arrow>())
                .map(|arrow| (arrow.item.item(), arrow.item.count()))
        })
        .await
        .flatten();
    assert_eq!(
        carried,
        Some((tuff_vanilla_data::Item::Arrow, 1)),
        "the bounce carries the arrow's own item"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn arrows_turn_back_at_the_world_border() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let id = world.dimension("minecraft:overworld");
    world.borders[id.index()]
        .send_replace(tuff_core::world::border::Border::fixed([10.0, 8.0], 10.0));
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let arrow = Arrow::new(
        world.shared.next_entity_id(),
        [9.5, f64::from(SKY) + 3.5, 8.5],
        [2.0, 0.0, 0.0],
        owner(&player),
    );
    region
        .request(RegionRequest::SpawnEntity(Box::new(arrow)))
        .await;
    world.ticks(15).await;
    let arrows: Vec<_> = snapshots(&region)
        .await
        .into_iter()
        .filter(|s| s.kind == EntityType::Arrow)
        .collect();
    assert!(!arrows.is_empty());
    assert!(
        arrows.iter().all(|a| a.position[0] < 15.0),
        "it never leaves the border: {arrows:?}"
    );
    world.shutdown().await;
}

/// A stone floor over x 505..=519, z 5..=11, which straddles the region
/// border at x 512.
fn border_floor() -> Vec<(BlockPos, tuff_vanilla_data::BlockState)> {
    let mut blocks = Vec::new();
    for x in 505..=519 {
        for z in 5..=11 {
            blocks.push((BlockPos::new(x, SKY, z), Block::Stone.default_state()));
        }
    }
    blocks
}

async fn border_area(
    world: &TestWorld,
    viewer: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
) -> Area {
    let chunks: Vec<ChunkPos> = (30..=33)
        .flat_map(|x| (-1..=1).map(move |z| ChunkPos::new(x, z)))
        .collect();
    Area::subscribe(world, viewer, events, &chunks).await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn arrows_tip_big_dripleaves_owned_by_the_next_region() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (viewer, mut events) = PlayerHandle::new(1000);
    let area = border_area(&world, &viewer, &mut events).await;
    let west = area.owner([500.0, 0.0, 8.0]).clone();
    let east = area.owner([520.0, 0.0, 8.0]).clone();
    let leaf = BlockPos::new(512, SKY + 1, 8);
    place_blocks(&world, &west, border_floor()).await;
    place_blocks(&world, &east, [(leaf, dripleaf())]).await;
    world.ticks(5).await;
    assert_eq!(tilt(block_at(&east, leaf).await), Some("none"));
    let arrow = Arrow::new(
        world.shared.next_entity_id(),
        [509.5, f64::from(SKY) + 1.8, 8.5],
        [1.0, 0.0, 0.0],
        owner(&viewer),
    );
    west.request(RegionRequest::SpawnEntity(Box::new(arrow)))
        .await;
    let mut tipped = false;
    for _ in 0..20 {
        world.ticks(1).await;
        if tilt(block_at(&east, leaf).await) == Some("full") {
            tipped = true;
            break;
        }
    }
    assert!(tipped, "the leaf in the next region tips over");
    world.ticks(101).await;
    assert_eq!(
        tilt(block_at(&east, leaf).await),
        Some("none"),
        "and springs back after five seconds"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn big_dripleaves_tip_under_what_stands_on_them_and_spring_back_when_powered() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, ChunkPos::new(0, 0)).await;
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let leaf = BlockPos::new(8, SKY + 1, 8);
    place_blocks(&world, &region, [(leaf, dripleaf())]).await;
    world.ticks(3).await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("NoAI", true),
        [8.5, f64::from(SKY) + 1.0 + 15.0 / 16.0, 8.5],
    )
    .await;
    let mut seen = Vec::new();
    for _ in 0..40 {
        world.ticks(1).await;
        let now = tilt(block_at(&region, leaf).await).unwrap_or("gone");
        if seen.last() != Some(&now) {
            seen.push(now);
        }
        if now == "full" {
            break;
        }
    }
    assert_eq!(seen, ["unstable", "partial", "full"]);
    place_blocks(
        &world,
        &region,
        [(
            BlockPos::new(9, SKY + 1, 8),
            Block::RedstoneBlock.default_state(),
        )],
    )
    .await;
    assert_eq!(
        tilt(block_at(&region, leaf).await),
        Some("none"),
        "power sets it level at once"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn skeletons_shoot_the_tipped_arrows_they_hold() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let mut poison = ItemStack::mint(Item::TippedArrow, 5, MintReason::Test);
    poison.components.potion_contents = Some(Box::new(PotionContents {
        potion: Some(Potion::Poison),
        ..Default::default()
    }));
    let equipment = Compound::new()
        .with(
            "mainhand",
            tuff_item::item::ItemPicture::new(Item::Bow, 1).to_nbt(),
        )
        .with("offhand", poison.to_nbt());
    summon(
        &region,
        EntityType::Skeleton,
        Compound::new().with("equipment", equipment),
        [3.5, f64::from(SKY) + 1.0, 3.5],
    )
    .await;
    region
        .request(player_at(
            &player,
            [12.5, f64::from(SKY) + 1.0, 12.5],
            PlayerLooks::default(),
        ))
        .await;
    let hit = next_hit(&world, &mut events, 600).await;
    assert_eq!(hit.projectile, Some(EntityType::Arrow));
    let effect = hit
        .effects
        .iter()
        .find(|e| e.effect == MobEffect::Poison)
        .expect("the arrow carries its potion");
    assert_eq!(effect.duration, 900 / 8);
    world.shutdown().await;
}

/// The stacks a mob's main-hand crossbow holds loaded.
async fn loaded(region: &RegionHandle, id: i32) -> Vec<ItemStack> {
    let Some(nbt) = entity_nbt(region, id).await else {
        return Vec::new();
    };
    nbt.get("equipment")
        .and_then(|e| e.as_compound()?.get("mainhand")?.as_compound())
        .and_then(|nbt| ItemStack::mint_from_nbt(nbt, MintReason::Test))
        .and_then(|mut crossbow| {
            std::mem::take(&mut crossbow.components)
                .into_patch()
                .charged_projectiles
        })
        .unwrap_or_default()
}

async fn crossbow_pillager(
    world: &TestWorld,
    region: &RegionHandle,
    player: &PlayerHandle,
    crossbow: ItemStack,
    offhand: Option<ItemStack>,
) -> i32 {
    place_blocks(world, region, pen(0, 12)).await;
    let mut equipment = Compound::new().with("mainhand", crossbow.to_nbt());
    if let Some(offhand) = offhand {
        equipment.insert("offhand", offhand.to_nbt());
    }
    summon(
        region,
        EntityType::Pillager,
        Compound::new().with("equipment", equipment),
        [1.5, f64::from(SKY) + 1.0, 1.5],
    )
    .await;
    region
        .request(player_at(
            player,
            [7.5, f64::from(SKY) + 1.0, 1.5],
            PlayerLooks::default(),
        ))
        .await;
    snapshots(region)
        .await
        .iter()
        .find(|s| s.kind == EntityType::Pillager)
        .expect("the pillager is there")
        .id
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pillagers_load_multishot_crossbows_and_fire_three_piercing_bolts() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let crossbow = enchanted(Item::Crossbow, &[("multishot", 1), ("piercing", 2)]);
    let pillager = crossbow_pillager(&world, &region, &player, crossbow, None).await;
    let mut charged = Vec::new();
    for _ in 0..300 {
        world.ticks(1).await;
        charged = loaded(&region, pillager).await;
        if !charged.is_empty() {
            break;
        }
    }
    assert_eq!(charged.len(), 3, "Multishot loads three");
    assert!(charged.iter().all(|s| s.item() == Item::Arrow));
    let found = wait_for(&world, &region, 100, |found| {
        found.iter().filter(|s| s.kind == EntityType::Arrow).count() >= 3
    })
    .await;
    let arrow = found.iter().find(|s| s.kind == EntityType::Arrow).unwrap();
    let nbt = entity_nbt(&region, arrow.id)
        .await
        .expect("the bolt is there");
    assert_eq!(nbt.get_byte("PierceLevel"), Some(2));
    assert!(
        loaded(&region, pillager).await.is_empty(),
        "firing empties the crossbow"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pillagers_fire_the_rockets_they_hold() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let rockets = ItemStack::mint(Item::FireworkRocket, 4, MintReason::Test);
    let crossbow = ItemStack::mint(Item::Crossbow, 1, MintReason::Test);
    let pillager = crossbow_pillager(&world, &region, &player, crossbow, Some(rockets)).await;
    wait_for(&world, &region, 400, |found| {
        found.iter().any(|s| s.kind == EntityType::FireworkRocket)
    })
    .await;
    let nbt = entity_nbt(&region, pillager).await.unwrap();
    let offhand = nbt
        .get("equipment")
        .and_then(|e| e.as_compound()?.get("offhand")?.as_compound())
        .and_then(|nbt| ItemStack::mint_from_nbt(nbt, MintReason::Test))
        .expect("rockets left in hand");
    assert_eq!(offhand.count(), 3, "one rocket was loaded and fired");
    world.shutdown().await;
}

fn starry_rocket() -> ItemStack {
    let mut stack = ItemStack::mint(Item::FireworkRocket, 1, MintReason::Test);
    stack.components.fireworks = Some(Box::new(tuff_item::item::decor::Fireworks {
        flight_duration: 1,
        explosions: vec![Default::default()],
    }));
    stack
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rocket_bursts_hurt_what_the_next_region_holds() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (viewer, mut events) = PlayerHandle::new(1000);
    let area = border_area(&world, &viewer, &mut events).await;
    let west = area.owner([500.0, 0.0, 8.0]).clone();
    let east = area.owner([520.0, 0.0, 8.0]).clone();
    place_blocks(&world, &west, border_floor()).await;
    summon(
        &east,
        EntityType::Cow,
        Compound::new().with("NoAI", true),
        [513.5, f64::from(SKY) + 1.0, 8.5],
    )
    .await;
    world.ticks(3).await;
    let health = |found: &[crate::world::messages::EntitySnapshot]| -> Option<f32> {
        found
            .iter()
            .find(|s| s.kind == EntityType::Cow)
            .and_then(|s| s.nbt.as_ref()?.get_float("Health"))
    };
    let mut rocket = FireworkRocket::shot(
        world.shared.next_entity_id(),
        [510.5, f64::from(SKY) + 1.5, 8.5],
        [0.0, 0.0, 0.0],
        starry_rocket(),
        owner(&viewer),
        &mut <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(1),
    );
    rocket.base.no_gravity = true;
    west.request(RegionRequest::SpawnEntity(Box::new(rocket)))
        .await;
    let mut hurt = false;
    for _ in 0..60 {
        world.ticks(1).await;
        let (reply, answer) = oneshot::channel();
        east.request(RegionRequest::EntitiesWithData { reply })
            .await;
        if health(&answer.await.unwrap()).is_some_and(|h| h < 10.0) {
            hurt = true;
            break;
        }
    }
    assert!(hurt, "the burst reaches the cow over the border");
    world.shutdown().await;
}

fn session(world: &TestWorld, id: i32) -> PlayerSession {
    let (session, outbound) = session_with_outbound(world, id);
    std::mem::forget(outbound);
    session
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pearls_leave_with_their_thrower_and_come_back_with_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(2000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let mut player = session(&world, 1000);
    player.stand_at([0.5, f64::from(SKY) + 1.0, 0.5], true);
    let hovering = [7.5, f64::from(SKY) + 20.0, 7.5];
    let mut pearl = Thrown::new(
        EntityType::EnderPearl,
        world.shared.next_entity_id(),
        hovering,
        [0.0, 0.0, 0.0],
        ItemStack::mint(Item::EnderPearl, 1, MintReason::Test),
        owner(&player.conn.handle),
    );
    pearl.base.no_gravity = true;
    region
        .request(RegionRequest::SpawnEntity(Box::new(pearl)))
        .await;
    wait_for(&world, &region, 10, |found| {
        found.iter().any(|s| s.kind == EntityType::EnderPearl)
    })
    .await;
    player.leaving_hooks().await;
    world.ticks(1).await;
    let pearls = snapshots(&region)
        .await
        .into_iter()
        .filter(|s| s.kind == EntityType::EnderPearl)
        .count();
    assert_eq!(pearls, 0, "the pearl left with its thrower");
    player.save_task().await.unwrap();
    let mut back = session(&world, 1000);
    back.load_saved().await;
    back.joined_hooks().await;
    let found = wait_for(&world, &region, 20, |found| {
        found.iter().any(|s| s.kind == EntityType::EnderPearl)
    })
    .await;
    let pearl = found
        .iter()
        .find(|s| s.kind == EntityType::EnderPearl)
        .unwrap();
    assert_eq!(pearl.position, hovering, "it is back where it was");
    back.leaving_hooks().await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn boosting_rockets_follow_their_glider_over_a_region_border() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (viewer, mut events) = PlayerHandle::new(1000);
    let area = border_area(&world, &viewer, &mut events).await;
    let (glider, _glider_events) = PlayerHandle::new(1001);
    let y = f64::from(SKY) + 10.0;
    let at = |x: f64| [x, y, 8.5];
    for region in area.regions() {
        region
            .request(player_at(&glider, at(505.5), PlayerLooks::default()))
            .await;
    }
    let rocket = FireworkRocket::attached(
        world.shared.next_entity_id(),
        glider.id,
        at(505.5),
        ItemStack::mint(Item::FireworkRocket, 1, MintReason::Test),
        owner(&glider),
        40,
    );
    let id = rocket.base.id;
    area.owner(at(505.5))
        .request(RegionRequest::SpawnEntity(Box::new(rocket)))
        .await;
    let mut x = 505.5;
    for _ in 0..12 {
        x += 1.5;
        for region in area.regions() {
            region
                .request(player_at(&glider, at(x), PlayerLooks::default()))
                .await;
        }
        world.ticks(1).await;
    }
    let rocket = area
        .entities()
        .await
        .into_iter()
        .find(|s| s.id == id)
        .expect("the rocket still flies");
    assert!(
        (rocket.position[0] - x).abs() < 0.01,
        "it keeps up with the glider, at {} against {x}",
        rocket.position[0]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pearls_vanish_when_their_thrower_dies_unless_the_rule_is_off() {
    for keep in [false, true] {
        let dir = TestDir::new();
        let world = TestWorld::start(dir.path());
        if keep {
            world.game_rules.send_modify(|rules| {
                rules
                    .set(
                        "ender_pearls_vanish_on_death",
                        crate::world::game_rules::RuleValue::Bool(false),
                    )
                    .expect("a boolean rule");
            });
        }
        let chunk = ChunkPos::new(0, 0);
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(2000);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(2, 13)).await;
        let mut player = session(&world, 1000);
        player.stand_at([0.5, f64::from(SKY) + 1.0, 0.5], true);
        let mut pearl = Thrown::new(
            EntityType::EnderPearl,
            world.shared.next_entity_id(),
            [7.5, f64::from(SKY) + 20.0, 7.5],
            [0.0, 0.0, 0.0],
            ItemStack::mint(Item::EnderPearl, 1, MintReason::Test),
            owner(&player.conn.handle),
        );
        pearl.base.no_gravity = true;
        region
            .request(RegionRequest::SpawnEntity(Box::new(pearl)))
            .await;
        wait_for(&world, &region, 10, |found| {
            found.iter().any(|s| s.kind == EntityType::EnderPearl)
        })
        .await;
        player
            .hurt_by(
                1000.0,
                &tuff_vanilla_data::damage_type::GENERIC_KILL,
                None,
                None,
            )
            .await;
        assert!(player.is_dead());
        player.run_tick().await;
        world.ticks(2).await;
        let pearls = snapshots(&region)
            .await
            .into_iter()
            .filter(|s| s.kind == EntityType::EnderPearl)
            .count();
        assert_eq!(pearls, usize::from(keep));
        world.shutdown().await;
    }
}
