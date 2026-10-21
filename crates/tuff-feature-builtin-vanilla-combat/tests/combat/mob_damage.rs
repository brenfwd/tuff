//! Damage between mobs and players: enchantment protection on mob armor
//! and Thorns acting in both directions.

use super::*;
use crate::world::entity::{Culprit, EntityAttack, EntityHit};
use crate::world::inventory::Inventory;
use tuff_item::item::Enchantments;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::EntityType;

fn enchanted(item: Item, enchantments: &[(&str, i32)]) -> ItemStack {
    let mut stack = ItemStack::mint(item, 1, MintReason::Test);
    stack.components.enchantments = Some(Enchantments::new(
        enchantments
            .iter()
            .map(|(id, level)| (format!("minecraft:{id}"), *level)),
    ));
    stack
}

fn punch(attacker: i32, damage: f32) -> EntityAttack {
    EntityAttack {
        attacker,
        attacker_uuid: 77,
        attacker_position: [3.5, SKY as f64 + 1.0, 3.5],
        attacker_yaw: 0.0,
        attacker_pitch: 0.0,
        damage,
        knockback: 0.0,
        weapon: None,
        critical: false,
        magic: false,
        sweep: false,
        part: None,
    }
}

async fn health(region: &RegionHandle, id: i32) -> f32 {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    match answer.await.unwrap().expect("entity exists").get("Health") {
        Some(Tag::Float(health)) => *health,
        other => panic!("no Health: {other:?}"),
    }
}

async fn zombie_wearing(region: &RegionHandle, chest: ItemStack, at: [f64; 3]) -> i32 {
    summon(
        region,
        EntityType::Zombie,
        Compound::new()
            .with("NoAI", true)
            .with("PersistenceRequired", true)
            .with("equipment", Compound::new().with("chest", chest.to_nbt())),
        at,
    )
    .await;
    mobs(region, EntityType::Zombie)
        .await
        .into_iter()
        .find(|s| (s.position[0] - at[0]).abs() < 0.5 && (s.position[2] - at[2]).abs() < 0.5)
        .expect("the zombie spawned")
        .id
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn protection_on_a_mobs_armor_softens_hits() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let y = SKY as f64 + 1.0;
    let plain = zombie_wearing(
        &region,
        ItemStack::mint(Item::IronChestplate, 1, MintReason::Test),
        [5.5, y, 5.5],
    )
    .await;
    let protected = zombie_wearing(
        &region,
        enchanted(Item::IronChestplate, &[("protection", 4)]),
        [10.5, y, 10.5],
    )
    .await;
    for target in [plain, protected] {
        region
            .request(RegionRequest::AttackEntity {
                attacker: player.clone(),
                target,
                attack: punch(player.id, 10.0),
                sweep: None,
            })
            .await;
    }
    world.ticks(1).await;
    let (plain, protected) = (
        health(&region, plain).await,
        health(&region, protected).await,
    );
    assert!(plain < 20.0, "the plain zombie was hurt: {plain}");
    let lost = |health: f32| 20.0 - health;
    assert!(
        (lost(protected) - lost(plain) * 0.84).abs() < 0.01,
        "four protection points take 16% off: plain lost {}, protected {}",
        lost(plain),
        lost(protected)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thorns_on_a_mobs_armor_hurts_the_player_who_hits_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    region
        .request(update(&player, 1, [3.5, SKY as f64 + 1.0, 3.5]))
        .await;
    let zombie = zombie_wearing(
        &region,
        enchanted(Item::IronChestplate, &[("thorns", 10)]),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: zombie,
            attack: punch(player.id, 2.0),
            sweep: None,
        })
        .await;
    let hit = expect(&mut events, |event| match event {
        PlayerEvent::EntityHurt(hit) => Some(hit),
        _ => None,
    })
    .await;
    assert_eq!(hit.kind.name, damage_type::THORNS.name);
    assert_eq!(hit.attacker.map(|a| a.id), Some(zombie));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn thorns_on_a_players_armor_hurts_the_mob_that_hits_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, outbound) = session_with_outbound(&world, 1000);
    std::mem::forget(outbound);
    let chest = Inventory::armor_slot(EquipmentSlot::Chest).unwrap();
    player.inventory_mut().set_slot(
        chest,
        Some(enchanted(Item::IronChestplate, &[("thorns", 10)])),
    );
    player.update_view().await;
    let at = [8.5, SKY as f64 + 1.0, 8.5];
    let region = world.region(ChunkPos::of_position(at).region()).await;
    let (reply, loaded) = oneshot::channel();
    region
        .request(RegionRequest::AwaitChunk {
            pos: BlockPos::containing(at),
            reply,
        })
        .await;
    loaded.await.unwrap();
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Zombie,
        Compound::new()
            .with("NoAI", true)
            .with("PersistenceRequired", true),
        at,
    )
    .await;
    let zombie = mobs(&region, EntityType::Zombie).await[0].id;
    let mut hit = EntityHit::new(2.0, &damage_type::MOB_ATTACK);
    hit.attacker = Some(Culprit {
        id: zombie,
        uuid: 5,
        kind: EntityType::Zombie,
        name: tuff_item::text::TextComponent::new("Zombie"),
        charged: None,
        owner_name: None,
        held_name: None,
    });
    hit.position = Some(at);
    player
        .handle_event(PlayerEvent::EntityHurt(Box::new(hit)))
        .await;
    assert!(player.health() < 20.0);
    let worn = player.inventory().slots()[chest].as_ref().unwrap().damage();
    assert!(worn >= 2, "thorns wears the armor: {worn}");
    let mut hurt = false;
    for _ in 0..20 {
        world.ticks(1).await;
        if health(&region, zombie).await < 20.0 {
            hurt = true;
            break;
        }
    }
    assert!(hurt, "the zombie took thorns damage");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn effects_drop_chances_leash_and_home_survive_a_save() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let effect = Compound::new()
        .with("id", "minecraft:speed")
        .with("amplifier", 1i8)
        .with("duration", 4000);
    summon(
        &region,
        EntityType::Zombie,
        Compound::new()
            .with("NoAI", true)
            .with("PersistenceRequired", true)
            .with("active_effects", Tag::List(vec![Tag::Compound(effect)]))
            .with("drop_chances", Compound::new().with("head", 0.5f32))
            .with("leash", Tag::IntArray(vec![1, 2, 3]))
            .with("home_pos", Tag::IntArray(vec![4, 5, 6]))
            .with("home_radius", 7),
        [3.5, SKY as f64 + 1.0, 3.5],
    )
    .await;
    let id = mobs(&region, EntityType::Zombie).await[0].id;
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    let nbt = answer.await.unwrap().expect("entity exists");
    assert_eq!(effect_ids(&nbt), vec!["minecraft:speed".to_owned()]);
    assert!(nbt.get("leash").is_some());
    assert!(nbt.get("home_pos").is_some());
    assert!(nbt.get("home_radius").is_some());
    let chances = nbt.get("drop_chances").and_then(Tag::as_compound).unwrap();
    assert_eq!(chances.get("head"), Some(&Tag::Float(0.5)));
}
