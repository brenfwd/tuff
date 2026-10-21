//! `/damage`, `/spreadplayers` and `/particle` as the game runs them.

use super::*;
use crate::world::messages::{Directive, DirectiveSource};
use crate::world::region_api::RegionTask;
use tuff_test_support::framed_packet_id;
use tuff_vanilla_data::registry::EntityType;

fn source() -> DirectiveSource {
    DirectiveSource {
        name: tuff_item::text::TextComponent::new("Server"),
        player: None,
    }
}

async fn pen_world(world: &TestWorld, chunk: ChunkPos) -> RegionHandle {
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(world, &viewer, &mut events, chunk).await;
    place_blocks(world, &region, pen(18, 29)).await;
    region
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn damage_hurts_mobs_and_players() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = pen_world(&world, ChunkPos::new(1, 1)).await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [20.5, SKY as f64 + 1.0, 20.5],
    )
    .await;
    world.ticks(2).await;
    let (mut player, _frames) = session_with_outbound(&world, 2000);
    player
        .run_command("scoreboard objectives add out dummy")
        .await;
    player
        .run_command("execute store success score #r out run damage @e[type=pig,limit=1] 1000")
        .await;
    let stored = world
        .shared
        .scoreboard
        .read(|board| board.score("#r", "out").map(|s| s.value))
        .await
        .flatten();
    assert_eq!(stored, Some(1));
    world.ticks(40).await;
    assert!(mobs(&region, EntityType::Pig).await.is_empty());

    player
        .set_game_mode(crate::protocol::play::GameMode::Survival)
        .await;
    let before = player.body.health.value;
    let hurt = Directive::Hurt {
        kind: &tuff_vanilla_data::damage_type::GENERIC,
        amount: 3.0,
        position: Some([1.0, 100.0, 1.0]),
        attacker: Some(crate::world::entity::Culprit {
            id: 9000,
            uuid: 9,
            kind: EntityType::Zombie,
            name: tuff_item::text::TextComponent::new("Zed"),
            charged: None,
            owner_name: None,
            held_name: None,
        }),
        direct: true,
    };
    assert!(
        player
            .apply_directive(hurt.clone(), source())
            .await
            .affected
    );
    assert_eq!(player.body.health.value, before - 3.0);

    player
        .set_game_mode(crate::protocol::play::GameMode::Creative)
        .await;
    assert!(!player.apply_directive(hurt, source()).await.affected);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blamed_hit_on_a_mob_victim_kills_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = pen_world(&world, ChunkPos::new(1, 1)).await;
    summon(
        &region,
        EntityType::Pig,
        Compound::new().with("NoAI", true),
        [20.5, SKY as f64 + 1.0, 20.5],
    )
    .await;
    world.ticks(2).await;
    let pig = mobs(&region, EntityType::Pig).await[0].id;
    let culprit = crate::world::entity::Culprit {
        id: 9000,
        uuid: 9,
        kind: EntityType::Zombie,
        name: tuff_item::text::TextComponent::new("Zed"),
        charged: None,
        owner_name: None,
        held_name: None,
    };
    let (sent, got) = std::sync::mpsc::channel();
    region
        .request(RegionRequest::Run(RegionTask::new(move |world| {
            let _ = sent.send(world.hurt_entity(
                pig,
                &tuff_vanilla_data::damage_type::PLAYER_ATTACK,
                1000.0,
                Some(culprit),
            ));
        })))
        .await;
    assert!(got.recv().unwrap());
    world.ticks(40).await;
    assert!(mobs(&region, EntityType::Pig).await.is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loot_spawn_rolls_a_table_into_item_entities() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = pen_world(&world, ChunkPos::new(1, 1)).await;
    world.ticks(2).await;
    let (mut player, _frames) = session_with_outbound(&world, 2000);
    player
        .run_command("scoreboard objectives add out dummy")
        .await;
    player
        .run_command(
            "execute store result score #r out run loot spawn 20.5 70 20.5 loot minecraft:chests/simple_dungeon",
        )
        .await;
    let stored = world
        .shared
        .scoreboard
        .read(|board| board.score("#r", "out").map(|s| s.value))
        .await
        .flatten();
    let rolled = stored.expect("the command ran");
    assert!(rolled > 0);
    world.ticks(2).await;
    assert_eq!(mobs(&region, EntityType::Item).await.len(), rolled as usize);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn debug_start_and_stop_guard_a_single_running_profiler() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _frames) = session_with_outbound(&world, 2000);
    player
        .run_command("scoreboard objectives add out dummy")
        .await;
    for (command, expected) in [
        ("execute store success score #r out run debug stop", 0),
        ("execute store success score #r out run debug start", 1),
        ("execute store success score #r out run debug start", 0),
        ("execute store success score #r out run debug stop", 1),
        ("execute store success score #r out run debug stop", 0),
    ] {
        player.run_command(command).await;
        let stored = world
            .shared
            .scoreboard
            .read(|board| board.score("#r", "out").map(|s| s.value))
            .await
            .flatten();
        assert_eq!(stored, Some(expected), "{command}");
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spreadplayers_spreads_entities_apart_on_the_ground() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = pen_world(&world, ChunkPos::new(1, 1)).await;
    for x in [19.5, 19.7, 19.9] {
        summon(
            &region,
            EntityType::Pig,
            Compound::new().with("NoAI", true),
            [x, SKY as f64 + 1.0, 19.5],
        )
        .await;
    }
    world.ticks(2).await;
    let (mut player, _frames) = session_with_outbound(&world, 2000);
    player
        .run_command("spreadplayers 24 24 4 5 false @e[type=pig]")
        .await;
    world.ticks(2).await;
    let pigs = mobs(&region, EntityType::Pig).await;
    assert_eq!(pigs.len(), 3);
    for pig in &pigs {
        let [x, y, z] = pig.position;
        assert!(
            (19.0..=29.0).contains(&x) && (19.0..=29.0).contains(&z),
            "{x} {z}"
        );
        assert_eq!(y, f64::from(SKY) + 1.0);
    }
    for (i, a) in pigs.iter().enumerate() {
        for b in &pigs[i + 1..] {
            let (dx, dz) = (a.position[0] - b.position[0], a.position[2] - b.position[2]);
            assert!((dx * dx + dz * dz).sqrt() >= 3.5);
        }
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn particle_directive_reaches_players_in_range_only() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, mut frames) = session_with_outbound(&world, 2000);
    player.pose.position = [8.5, 100.0, 8.5];
    let id = tuff_vanilla_data::packet::play::Clientbound::LevelParticles.id();
    let send = |position: [f64; 3], force: bool, dimension| Directive::Particle {
        dimension,
        particle: tuff_vanilla_data::registry::ParticleType::Flame.id(),
        options: std::sync::Arc::from(Vec::new()),
        force,
        position,
        delta: [0.0; 3],
        speed: 0.0,
        count: 1,
    };
    let dimension = player.dimension;
    let near = player
        .apply_directive(send([8.5, 101.0, 8.5], false, dimension), source())
        .await;
    assert!(near.affected);
    let mut seen = false;
    while let Ok(frame) = frames.try_recv() {
        seen |= framed_packet_id(&frame) == id;
    }
    assert!(seen);

    let far = [8.5, 100.0, 108.5];
    assert!(
        !player
            .apply_directive(send(far, false, dimension), source())
            .await
            .affected
    );
    assert!(
        player
            .apply_directive(send(far, true, dimension), source())
            .await
            .affected
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn item_slots_are_replaced_filled_and_overridden() {
    use crate::world::messages::SlotFill;
    use tuff_item::item::{ItemStack, MintReason};
    use tuff_vanilla_data::Item;

    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (mut player, _frames) = session_with_outbound(&world, 2000);
    let stick = ItemStack::mint(Item::Stick, 1, MintReason::Test);
    let ids = |name: &str| crate::world::predicate::slot_range(name).unwrap();
    let set = |name: &str, mode| Directive::SetSlots {
        ids: ids(name),
        name: name.to_owned(),
        stack: tuff_item::item::ItemSnapshot::of(&stick),
        mode,
    };

    let reply = player
        .apply_directive(set("weapon.mainhand", SlotFill::Replace), source())
        .await;
    assert_eq!((reply.affected, reply.value), (true, 1));
    let held = player.inventory().selected();
    assert_eq!(
        player.inventory().slots()[crate::world::inventory::Inventory::hotbar_slot(held)]
            .as_ref()
            .map(|s| s.item),
        Some(Item::Stick)
    );

    let reply = player
        .apply_directive(set("armor.*", SlotFill::Fill), source())
        .await;
    assert_eq!(reply.value, 4, "the body slot is not a player's");
    let reply = player
        .apply_directive(set("container.*", SlotFill::Replace), source())
        .await;
    assert_eq!(reply.value, 1);
    let reply = player
        .apply_directive(set("enderchest.3", SlotFill::Fill), source())
        .await;
    assert!(reply.affected, "ender chest slots are writable");
    let reply = player
        .apply_directive(set("hotbar.*", SlotFill::Override), source())
        .await;
    assert_eq!(reply.value, 9);
    let filled = player
        .inventory()
        .slots()
        .iter()
        .flatten()
        .filter(|s| s.item == Item::Stick)
        .count();
    assert_eq!(filled, 4 + 1, "armor, and hotbar.0 which is container.0");
    world.shutdown().await;
}
