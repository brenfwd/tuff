//! Mobs riding mobs: saved `Passengers` come back seated, ride, steer,
//! cross region borders and get off when their vehicle dies.

use super::*;
use crate::protocol::play;
use tuff_vanilla_data::registry::EntityType;

fn passengers_of(frame: &bytes::Bytes) -> Option<(i32, Vec<i32>)> {
    let mut reader = tuff_test_support::framed_payload(frame);
    if reader.read_var_int().ok()? != <play::SetPassengers as crate::protocol::Encode>::ID {
        return None;
    }
    let vehicle = reader.read_var_int().ok()?;
    let count = reader.read_var_int().ok()?;
    let riders = (0..count).map(|_| reader.read_var_int().unwrap()).collect();
    Some((vehicle, riders))
}

fn jockey(rider: Compound) -> Compound {
    Compound::new().with("NoAI", true).with(
        "Passengers",
        Tag::List(vec![Tag::Compound(rider.with("id", "minecraft:skeleton"))]),
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_saved_jockey_rides_its_spider_and_is_saved_nested() {
    let dir = TestDir::new();
    let chunk = ChunkPos::new(1, 1);
    let skeleton_uuid;
    {
        let world = TestWorld::start(dir.path());
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(1000);
        view_around(&world, &viewer, &mut events, chunk).await;
        place_blocks(&world, &region, pen(18, 29)).await;
        summon(
            &region,
            EntityType::Spider,
            jockey(Compound::new().with("NoAI", true)),
            [24.5, SKY as f64 + 3.0, 24.5],
        )
        .await;
        world.ticks(30).await;
        let spider = mobs(&region, EntityType::Spider).await[0].clone();
        let skeleton = mobs(&region, EntityType::Skeleton).await[0].clone();
        skeleton_uuid = skeleton.uuid;
        let seat = EntityType::Spider.passenger_attachments()[0];
        let expected = spider.position[1] + seat[1] - EntityType::Skeleton.vehicle_attachment()[1];
        assert!(
            (spider.position[1] - (SKY + 1) as f64).abs() < 1.0e-6,
            "the spider landed: {:?}",
            spider.position
        );
        assert!(
            (skeleton.position[1] - expected).abs() < 1.0e-6
                && (skeleton.position[0] - spider.position[0]).abs() < 1.0e-6,
            "the skeleton sits on the spider: {:?} on {:?}",
            skeleton.position,
            spider.position
        );
        let (late, mut late_events) = PlayerHandle::new(1001);
        region
            .request(RegionRequest::Subscribe {
                player: late.clone(),
                chunks: vec![chunk],
            })
            .await;
        let seat_id = (spider.id, vec![skeleton.id]);
        let listed = expect(&mut late_events, |event| match event {
            PlayerEvent::Track(track) => {
                let seated: Vec<_> = track.spawn.iter().filter_map(passengers_of).collect();
                seated.contains(&seat_id).then_some(seated)
            }
            _ => None,
        })
        .await;
        assert!(
            listed.contains(&(spider.id, vec![skeleton.id])),
            "a new viewer sees the skeleton seated: {listed:?}"
        );
        world.shutdown().await;
    }
    let saved = stored_entities(dir.path(), chunk);
    assert!(
        saved
            .iter()
            .all(|e| e.get_str("id") != Some("minecraft:skeleton")),
        "the skeleton is not saved on its own"
    );
    let spider = saved
        .iter()
        .find(|e| e.get_str("id") == Some("minecraft:spider"))
        .expect("the spider was saved");
    let nested = spider.get_list("Passengers").expect("its rider is nested");
    assert_eq!(nested.len(), 1);
    let rider = nested[0].as_compound().unwrap();
    assert_eq!(rider.get_str("id"), Some("minecraft:skeleton"));
    assert_eq!(rider.get_uuid("UUID"), Some(skeleton_uuid));

    let world = TestWorld::start(dir.path());
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1002);
    view_around(&world, &viewer, &mut events, chunk).await;
    world.ticks(3).await;
    let spider = mobs(&region, EntityType::Spider).await[0].clone();
    let skeleton = mobs(&region, EntityType::Skeleton).await;
    assert_eq!(skeleton.len(), 1);
    assert_eq!(skeleton[0].uuid, skeleton_uuid);
    assert!(
        skeleton[0].position[1] > spider.position[1] + 0.05,
        "still seated after a reload"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rider_gets_off_when_its_vehicle_dies() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(18, 29)).await;
    summon(
        &region,
        EntityType::Spider,
        jockey(Compound::new().with("NoAI", true)),
        [24.5, SKY as f64 + 1.0, 24.5],
    )
    .await;
    world.ticks(3).await;
    let spider = mobs(&region, EntityType::Spider).await[0].id;
    let killed = region
        .ask(move |world| {
            let ids: Vec<i32> = vec![spider];
            ids.into_iter().filter(|id| world.kill_entity(*id)).count()
        })
        .await
        .unwrap();
    assert_eq!(killed, 1);
    let found = wait_for(&world, &region, 60, |found| {
        !found.iter().any(|s| s.kind == EntityType::Spider)
    })
    .await;
    world.ticks(20).await;
    let skeleton = mobs(&region, EntityType::Skeleton).await;
    assert_eq!(skeleton.len(), 1, "{found:#?}");
    assert!(
        (skeleton[0].position[1] - (SKY + 1) as f64).abs() < 1.0e-6,
        "it fell to the ground: {:?}",
        skeleton[0].position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rider_crosses_a_region_border_on_its_vehicle() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&west, &viewer, &mut events, &[ChunkPos::new(31, 0)]).await;
    subscribe(&east, &viewer, &mut events, &[ChunkPos::new(32, 0)]).await;
    world.ticks(3).await;
    let nbt = jockey(Compound::new().with("NoAI", true))
        .with("NoGravity", true)
        .with(
            "Motion",
            Tag::List(vec![Tag::Double(1.0), Tag::Double(0.0), Tag::Double(0.0)]),
        );
    summon(
        &west,
        EntityType::Spider,
        nbt,
        [508.5, SKY as f64 + 20.0, 8.5],
    )
    .await;
    wait_for(&world, &east, 60, |found| {
        found.iter().any(|s| s.kind == EntityType::Spider)
            && found.iter().any(|s| s.kind == EntityType::Skeleton)
    })
    .await;
    assert!(snapshots(&west).await.is_empty());
    world.ticks(3).await;
    let spider = mobs(&east, EntityType::Spider).await[0].clone();
    let skeleton = mobs(&east, EntityType::Skeleton).await[0].clone();
    assert!(
        (skeleton.position[0] - spider.position[0]).abs() < 1.0e-6
            && skeleton.position[1] > spider.position[1] + 0.05,
        "still riding: {:?} on {:?}",
        skeleton.position,
        spider.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zombie_riding_a_chicken_steers_it_toward_its_target() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    view_around(&world, &player, &mut events, chunk).await;
    place_blocks(&world, &region, pen(18, 40)).await;
    let rider = Compound::new()
        .with("id", "minecraft:zombie")
        .with("IsBaby", true)
        .with("PersistenceRequired", true);
    summon(
        &region,
        EntityType::Chicken,
        Compound::new()
            .with("PersistenceRequired", true)
            .with("Passengers", Tag::List(vec![Tag::Compound(rider)])),
        [20.5, SKY as f64 + 1.0, 24.5],
    )
    .await;
    summon(
        &region,
        EntityType::Chicken,
        Compound::new().with("PersistenceRequired", true),
        [20.5, SKY as f64 + 1.0, 34.5],
    )
    .await;
    let target = [36.5, SKY as f64 + 1.0, 24.5];
    region.request(player_update(&player, target, None)).await;
    world.publish_players(OVERWORLD, &[(&player, target)]);
    let found = wait_for(&world, &region, 400, |found| {
        found.iter().any(|s| {
            s.kind == EntityType::Chicken
                && s.position[2] < 30.0
                && (s.position[0] - target[0]).abs() < 3.0
        })
    })
    .await;
    let zombie = found.iter().find(|s| s.kind == EntityType::Zombie).unwrap();
    let mut chickens: Vec<_> = found
        .iter()
        .filter(|s| s.kind == EntityType::Chicken)
        .collect();
    let zx = zombie.position[0];
    chickens.sort_by(|a, b| {
        (a.position[0] - zx)
            .abs()
            .total_cmp(&(b.position[0] - zx).abs())
    });
    let (chicken, lone) = (chickens[0], chickens[1]);
    assert!(
        lone.position[0] < target[0] - 6.0,
        "a chicken nobody rides does not head there: {:?}",
        lone.position
    );
    assert!(
        (zombie.position[0] - chicken.position[0]).abs() < 0.5,
        "the zombie rode along: {:?} {:?}",
        zombie.position,
        chicken.position
    );
    world.shutdown().await;
}

async fn crammed_pigs(rule: i32) -> usize {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world.game_rules.send_modify(|rules| {
        rules
            .set(
                "max_entity_cramming",
                crate::world::game_rules::RuleValue::Int(rule),
            )
            .expect("an integer rule");
    });
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(24, 24)).await;
    for _ in 0..30 {
        summon(
            &region,
            EntityType::Pig,
            Compound::new().with("NoAI", true),
            [24.5, SKY as f64 + 1.0, 24.5],
        )
        .await;
    }
    world.ticks(100).await;
    let left = mobs(&region, EntityType::Pig)
        .await
        .into_iter()
        .filter(|s| (s.position[0] - 24.5).abs() < 2.0 && (s.position[2] - 24.5).abs() < 2.0)
        .count();
    world.shutdown().await;
    left
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crammed_mobs_take_damage_unless_the_rule_is_off() {
    let crammed = crammed_pigs(24).await;
    assert!(crammed < 30, "cramming killed some pigs: {crammed} left");
    assert!(
        crammed >= 23,
        "but stops once few enough are left: {crammed}"
    );
    assert_eq!(crammed_pigs(0).await, 30, "a rule of 0 turns cramming off");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_and_a_boat_push_each_other_apart() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(18, 29)).await;
    let ground = SKY as f64 + 1.0;
    summon(
        &region,
        EntityType::Horse,
        Compound::new().with("NoAI", true),
        [24.5, ground, 24.5],
    )
    .await;
    summon(
        &region,
        EntityType::OakBoat,
        Compound::new(),
        [25.0, ground, 24.5],
    )
    .await;
    world.ticks(20).await;
    let horse = mobs(&region, EntityType::Horse).await[0].position;
    let boat = mobs(&region, EntityType::OakBoat).await[0].position;
    assert!(
        horse[0] < 24.45,
        "the boat pushed the horse away: {horse:?}"
    );
    assert!(boat[0] > 25.05, "the horse pushed the boat away: {boat:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_squeezed_among_too_many_mobs_takes_cramming_damage() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(24, 24)).await;
    let spot = [24.5, SKY as f64 + 1.0, 24.5];
    for _ in 0..28 {
        summon(
            &region,
            EntityType::Pig,
            Compound::new()
                .with("NoAI", true)
                .with("Invulnerable", true),
            spot,
        )
        .await;
    }
    let (player, mut heard) = PlayerHandle::new(2000);
    region
        .request(player_at(
            &player,
            spot,
            crate::world::messages::PlayerLooks::default(),
        ))
        .await;
    let mut crushed = false;
    for _ in 0..60 {
        world.ticks(1).await;
        while let Ok(event) = heard.try_recv() {
            crushed |= matches!(
                event,
                PlayerEvent::Hurt { kind, amount, .. }
                    if kind.name.ends_with("cramming") && amount == 6.0
            );
        }
        if crushed {
            break;
        }
    }
    assert!(crushed, "the player was crammed");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_players_own_tamed_wolves_cram_it_when_its_team_pushes_only_other_teams() {
    use crate::world::scoreboard::{CollisionRule, Team};
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .scoreboard
        .update(|board| {
            let mut team = Team::new("pack");
            team.collision_rule = CollisionRule::PushOtherTeams;
            board.add_team(team);
            board.join_team("Tester", "pack");
        })
        .await
        .unwrap();
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(24, 24)).await;
    let spot = [24.5, SKY as f64 + 1.0, 24.5];
    for _ in 0..28 {
        summon(
            &region,
            EntityType::Wolf,
            Compound::new()
                .with("NoAI", true)
                .with("Invulnerable", true)
                .with("Owner", Tag::uuid(77)),
            spot,
        )
        .await;
    }
    let (player, mut heard) = PlayerHandle::new(2000);
    region
        .request(player_at(
            &player,
            spot,
            crate::world::messages::PlayerLooks::default(),
        ))
        .await;
    let mut crushed = false;
    for _ in 0..80 {
        world.ticks(1).await;
        while let Ok(event) = heard.try_recv() {
            crushed |= matches!(
                event,
                PlayerEvent::Hurt { kind, amount, .. }
                    if kind.name.ends_with("cramming") && amount == 6.0
            );
        }
        if crushed {
            break;
        }
    }
    assert!(crushed, "wolves on the owner's team push it");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tamed_wolf_snapshot_names_its_online_owner() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(24, 24)).await;
    let spot = [24.5, SKY as f64 + 1.0, 24.5];
    summon(
        &region,
        EntityType::Wolf,
        Compound::new()
            .with("NoAI", true)
            .with("Owner", Tag::uuid(77)),
        spot,
    )
    .await;
    let (player, _heard) = PlayerHandle::new(2000);
    region
        .request(player_at(
            &player,
            spot,
            crate::world::messages::PlayerLooks::default(),
        ))
        .await;
    world.ticks(2).await;
    let (reply, answer) = tokio::sync::oneshot::channel();
    region
        .request(crate::world::messages::RegionRequest::Entities { reply })
        .await;
    let wolf = answer
        .await
        .unwrap()
        .into_iter()
        .find(|e| e.kind == EntityType::Wolf)
        .unwrap();
    assert_eq!(wolf.owner_name.as_deref(), Some("Tester"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ravager_carrying_a_vindicator_hunts_on_its_own_while_both_hit() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(0, 14)).await;
    let rider = Compound::new().with("id", "minecraft:vindicator");
    summon(
        &region,
        EntityType::Ravager,
        Compound::new().with("Passengers", Tag::List(vec![Tag::Compound(rider)])),
        [1.5, SKY as f64 + 1.0, 1.5],
    )
    .await;
    let position = [12.5, SKY as f64 + 1.0, 1.5];
    region
        .request(player_at(
            &player,
            position,
            crate::world::messages::PlayerLooks::default(),
        ))
        .await;
    let mut attackers = Vec::new();
    for _ in 0..12 {
        let hit = next_hit(&world, &mut events, 600).await;
        let attacker = hit.attacker.expect("a mob is to blame").kind;
        attackers.push(attacker);
        if attackers.contains(&EntityType::Ravager) && attackers.contains(&EntityType::Vindicator) {
            break;
        }
    }
    assert!(
        attackers.contains(&EntityType::Ravager),
        "the ravager chased and bit: {attackers:?}"
    );
    assert!(
        attackers.contains(&EntityType::Vindicator),
        "the vindicator reached down from its seat: {attackers:?}"
    );
    let ravager = mobs(&region, EntityType::Ravager).await[0].clone();
    let vindicator = mobs(&region, EntityType::Vindicator).await[0].clone();
    assert!(
        (vindicator.position[0] - ravager.position[0]).abs() < 1.0
            && vindicator.position[1] > ravager.position[1] + 1.0,
        "it still rides: {:?} on {:?}",
        vindicator.position,
        ravager.position
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tamed_wolf_far_behind_its_owner_teleports_next_to_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    place_blocks(&world, &region, pen(17, 30)).await;
    let y = SKY as f64 + 1.0;
    summon(
        &region,
        EntityType::Wolf,
        Compound::new().with("Owner", Tag::uuid(77)),
        [17.5, y, 17.5],
    )
    .await;
    let (player, _heard) = PlayerHandle::new(2000);
    region
        .request(player_at(
            &player,
            [30.5, y, 30.5],
            crate::world::messages::PlayerLooks::default(),
        ))
        .await;
    let mut near = false;
    for _ in 0..40 {
        world.ticks(1).await;
        let (reply, answer) = tokio::sync::oneshot::channel();
        region
            .request(crate::world::messages::RegionRequest::Entities { reply })
            .await;
        let wolf = answer
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.kind == EntityType::Wolf)
            .unwrap();
        let dx = wolf.position[0] - 30.5;
        let dz = wolf.position[2] - 30.5;
        if dx * dx + dz * dz < 36.0 {
            near = true;
            break;
        }
    }
    assert!(
        near,
        "the wolf teleports to within a few blocks of its owner"
    );
    world.shutdown().await;
}
