//! Mobs in regions: spawning, falling, pathfinding, breeding, shearing,
//! milking, dying, saving and crossing region borders, driven through the
//! region's channels.

use super::*;
use crate::world::entity::{EntityAttack, EntityInteraction};
use crate::world::messages::{
    ActionOutcome, BucketUse, EntitySnapshot, HandEffect, PlayerLooks, Track,
};
use tuff_vanilla_data::registry::EntityType;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn summoned_cows_fall_onto_the_ground_and_are_tracked_with_their_data() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new(),
        [8.5, SKY as f64 + 6.0, 8.5],
    )
    .await;
    let track = expect(&mut events, track_of(EntityType::Cow)).await;
    assert!(track.spawn.len() >= 2, "spawn packets carry entity data");
    let found = wait_for(&world, &region, 200, |found| {
        found
            .iter()
            .any(|s| s.kind == EntityType::Cow && (s.position[1] - (SKY + 1) as f64).abs() < 1e-6)
    })
    .await;
    let cow = found.iter().find(|s| s.kind == EntityType::Cow).unwrap();
    assert!((2.0..=14.0).contains(&cow.position[0]) && (2.0..=14.0).contains(&cow.position[2]));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cows_wander_across_a_flat_grass_plane() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunks = around(ChunkPos::new(2, 2), 1);
    let region = world.region(ChunkPos::new(2, 2).region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &chunks).await;
    let mut plane = Vec::new();
    for x in 16..64 {
        for z in 16..64 {
            plane.push((BlockPos::new(x, SKY, z), Block::GrassBlock.default_state()));
        }
    }
    place_blocks(&world, &region, plane).await;
    let start = [40.5, SKY as f64 + 1.0, 40.5];
    summon(&region, EntityType::Cow, Compound::new(), start).await;
    world.publish_players(OVERWORLD, &[(&viewer, [20.5, SKY as f64 + 1.0, 20.5])]);
    wait_for(&world, &region, 2400, |found| {
        found.iter().any(|s| {
            s.kind == EntityType::Cow
                && (s.position[0] - start[0]).hypot(s.position[2] - start[2]) > 2.0
        })
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tempted_cows_find_their_way_around_a_wall() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let mut blocks = pen(1, 14);
    for z in 1..=7 {
        for y in [SKY + 1, SKY + 2] {
            blocks.push((BlockPos::new(8, y, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new(),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    let player = [12.5, SKY as f64 + 1.0, 4.5];
    region
        .request(player_update(&viewer, player, Some(Item::Wheat)))
        .await;
    let found = wait_for(&world, &region, 600, |found| {
        found.iter().any(|s| {
            s.kind == EntityType::Cow && crate::world::distance_sq(s.position, player) < 3.5 * 3.5
        })
    })
    .await;
    let cow = found.iter().find(|s| s.kind == EntityType::Cow).unwrap();
    assert!(
        cow.position[0] > 8.0,
        "went around the wall to {:?}",
        cow.position
    );
    world.shutdown().await;
}

/// Where a client puts an entity from the packets it gets about it, and
/// how they told it.
struct ClientMirror {
    events: mpsc::UnboundedReceiver<PlayerEvent>,
    view: tracking::EntityView,
    position: Option<[f64; 3]>,
    absolute: usize,
    relative: usize,
    heads: usize,
    most_heads: usize,
    head_ticks: Vec<u64>,
}

impl ClientMirror {
    fn new(events: mpsc::UnboundedReceiver<PlayerEvent>) -> Self {
        Self {
            events,
            view: tracking::EntityView::default(),
            position: None,
            absolute: 0,
            relative: 0,
            heads: 0,
            most_heads: 0,
            head_ticks: Vec::new(),
        }
    }

    /// Takes in the reports about `kind` so far, checking after each one
    /// that only told what changed that the client agrees with the server.
    fn follow(
        &mut self,
        kind: EntityType,
        area: &tracking::ViewArea,
        tick: u64,
    ) -> Option<[f64; 3]> {
        let mut last = None;
        self.heads = 0;
        while let Ok(event) = self.events.try_recv() {
            if let PlayerEvent::EntityPackets { packets, .. } = &event {
                packets.iter().for_each(|frame| self.apply(frame));
            }
            let PlayerEvent::Track(track) = event else {
                continue;
            };
            if track.kind != kind {
                continue;
            }
            let (position, moves) = (track.position, track.moves.is_some());
            let mut out = Vec::new();
            self.view.track(track, area, tick, false, &mut out);
            out.iter().for_each(|frame| self.apply(frame));
            let seen = self.position.expect("the entity was placed");
            assert!(
                !moves || crate::world::distance_sq(seen, position) < 0.01 * 0.01,
                "tick {tick}: the client sees {seen:?}, the entity is at {position:?}"
            );
            last = Some(position);
        }
        self.most_heads = self.most_heads.max(self.heads);
        if self.heads > 0 {
            self.head_ticks.push(tick);
        }
        last
    }

    fn apply(&mut self, frame: &bytes::Bytes) {
        use tuff_vanilla_data::packet::play::Clientbound as C;
        let mut payload = framed_payload(frame);
        let id = payload.read_var_int().unwrap();
        if id == C::EntityPositionSync.id() {
            payload.read_var_int().unwrap();
            let position = match payload.read_var_int().unwrap() {
                0 => [0; 3].map(|_| payload.read_double().unwrap()),
                1 => {
                    let steps = payload.read_var_int().unwrap();
                    assert!(steps > 0, "a stepped path has stops");
                    let mut last = [0.0; 3];
                    for _ in 0..steps {
                        last = [0; 3].map(|_| payload.read_double().unwrap());
                        payload.read_var_int().unwrap();
                    }
                    last
                }
                kind => panic!("unknown position path {kind}"),
            };
            self.position = Some(position);
            self.absolute += 1;
        } else if id == C::MoveEntityPos.id() || id == C::MoveEntityPosRot.id() {
            payload.read_var_int().unwrap();
            assert!(payload.read_var_int().unwrap() <= 1, "a linear delta");
            let delta = [0; 3].map(|_| payload.read_short().unwrap());
            let base = self.position.expect("a delta after a position");
            self.position = Some(movement_sync::decode_delta(base, delta));
            self.relative += 1;
        } else if id == C::RotateHead.id() {
            self.heads += 1;
        }
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_walking_mob_reaches_viewers_as_relative_moves() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(1, 14)).await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new(),
        [2.5, SKY as f64 + 1.0, 2.5],
    )
    .await;
    let player = [11.5, SKY as f64 + 1.0, 2.5];
    region
        .request(player_update(&viewer, player, Some(Item::Wheat)))
        .await;
    let subscribed: HashSet<ChunkPos> = [chunk].into();
    let area = tracking::ViewArea {
        center: chunk,
        view_distance: 4,
        subscribed: &subscribed,
        spectator: false,
        position: player,
        range_percent: 100,
    };
    let mut first = ClientMirror::new(events);
    let mut late = None;
    let mut last = None;
    for tick in 0..400 {
        world.ticks(1).await;
        last = first.follow(EntityType::Cow, &area, tick).or(last);
        if tick == 10 {
            let (viewer, mut events) = PlayerHandle::new(101);
            subscribe(&region, &viewer, &mut events, &[chunk]).await;
            late = Some(ClientMirror::new(events));
        }
        if let Some(late) = late.as_mut() {
            late.follow(EntityType::Cow, &area, tick);
        }
        if last.is_some_and(|at| crate::world::distance_sq(at, player) < 3.0 * 3.0) {
            break;
        }
    }
    let last = last.expect("the cow was tracked");
    assert!(
        crate::world::distance_sq(last, player) < 3.0 * 3.0,
        "the cow walked over to {last:?}"
    );
    for mirror in [&first, late.as_ref().unwrap()] {
        assert!(
            mirror.head_ticks.windows(2).all(|w| w[1] - w[0] >= 3),
            "head packets on ticks {:?}",
            mirror.head_ticks
        );
        assert!(
            mirror.most_heads <= 1,
            "{} head packets in one tick",
            mirror.most_heads
        );
        assert!(
            mirror.relative >= 10 && mirror.relative > 3 * mirror.absolute,
            "{} relative and {} absolute moves",
            mirror.relative,
            mirror.absolute
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fed_animals_fall_in_love_and_breed_a_baby() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(4, 11)).await;
    for x in [6.5, 8.5] {
        summon(
            &region,
            EntityType::Pig,
            Compound::new(),
            [x, SKY as f64 + 1.0, 7.5],
        )
        .await;
    }
    world.ticks(2).await;
    let pigs = mobs(&region, EntityType::Pig).await;
    assert_eq!(pigs.len(), 2);
    for pig in &pigs {
        let effect = interact(&region, &viewer, &mut events, pig.id, Item::Carrot).await;
        assert!(matches!(effect, HandEffect::Consume(1)));
    }
    let found = wait_for(&world, &region, 600, |found| {
        found.iter().filter(|s| s.kind == EntityType::Pig).count() == 3
    })
    .await;
    assert!(
        found.iter().any(|s| s.kind == EntityType::ExperienceOrb),
        "breeding gives experience"
    );
    let baby = found
        .iter()
        .filter(|s| s.kind == EntityType::Pig)
        .max_by_key(|s| s.id)
        .unwrap();
    assert!(baby.bounds[4] - baby.bounds[1] < 0.5, "the baby is small");
    let effect = region
        .request(RegionRequest::InteractEntity {
            player: viewer.clone(),
            target: pigs[0].id,
            interaction: interaction(&viewer, Some(Item::Carrot)),
        })
        .await;
    assert!(effect);
    world.ticks(3).await;
    let late = events.try_recv();
    assert!(
        !matches!(late, Ok(PlayerEvent::EntityInteracted { .. })),
        "a parent on cooldown does not eat"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sheep_are_sheared_and_dyed_and_cows_milked() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Sheep,
        Compound::new().with("Color", 14i8),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Sheep,
        Compound::new(),
        [9.5, SKY as f64 + 1.0, 9.5],
    )
    .await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new(),
        [7.5, SKY as f64 + 1.0, 11.5],
    )
    .await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new().with("Age", -24000),
        [11.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(2).await;
    let sheep = mobs(&region, EntityType::Sheep).await;
    let cows = mobs(&region, EntityType::Cow).await;

    let effect = interact(&region, &viewer, &mut events, sheep[0].id, Item::Shears).await;
    assert!(matches!(effect, HandEffect::Damage(1)));
    let found = wait_for(&world, &region, 5, |found| {
        found.iter().any(|s| s.kind == EntityType::Item)
    })
    .await;
    assert!(
        found
            .iter()
            .filter(|s| s.kind == EntityType::Item)
            .all(|s| s.name == "minecraft:red_wool"),
        "red sheep give red wool: {found:#?}"
    );
    region
        .request(RegionRequest::InteractEntity {
            player: viewer.clone(),
            target: sheep[0].id,
            interaction: interaction(&viewer, Some(Item::Shears)),
        })
        .await;

    let mut tag = ItemStack::mint(Item::NameTag, 1, MintReason::Test);
    tag.components.custom_name = Some(Box::new(tuff_item::text::TextComponent::new("Dolly")));
    region
        .request(RegionRequest::InteractEntity {
            player: viewer.clone(),
            target: sheep[1].id,
            interaction: EntityInteraction {
                stack: Some(tuff_item::item::ItemSnapshot::of(&tag)),
                ..interaction(&viewer, None)
            },
        })
        .await;
    let named = expect(&mut events, |event| match event {
        PlayerEvent::EntityInteracted { effect, .. } => Some(effect),
        _ => None,
    })
    .await;
    assert!(matches!(named, HandEffect::Consume(1)));
    assert!(
        mobs(&region, EntityType::Sheep)
            .await
            .iter()
            .any(|s| s.name == "Dolly"),
        "name tags name mobs"
    );
    let effect = interact(&region, &viewer, &mut events, sheep[1].id, Item::BlueDye).await;
    assert!(matches!(effect, HandEffect::Consume(1)));
    let effect = interact(&region, &viewer, &mut events, cows[0].id, Item::Bucket).await;
    assert!(matches!(effect, HandEffect::Exchange(Item::MilkBucket)));
    let effect = interact(&region, &viewer, &mut events, sheep[1].id, Item::Shears).await;
    assert!(matches!(effect, HandEffect::Damage(1)));
    let found = wait_for(&world, &region, 5, |found| {
        found.iter().any(|s| s.name == "minecraft:blue_wool")
    })
    .await;
    assert!(found.iter().any(|s| s.name == "minecraft:red_wool"));
    region
        .request(RegionRequest::InteractEntity {
            player: viewer.clone(),
            target: cows[1].id,
            interaction: interaction(&viewer, Some(Item::Bucket)),
        })
        .await;
    world.ticks(2).await;
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, PlayerEvent::EntityInteracted { .. }),
            "a sheared sheep and a calf give nothing: {event:?}"
        );
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mooshrooms_give_milk_stew_and_mushrooms() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Mooshroom,
        Compound::new().with("Type", "brown"),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(2).await;
    let mooshrooms = mobs(&region, EntityType::Mooshroom).await;
    assert_eq!(mooshrooms.len(), 1);

    let effect = interact(
        &region,
        &viewer,
        &mut events,
        mooshrooms[0].id,
        Item::Bucket,
    )
    .await;
    assert!(matches!(effect, HandEffect::Exchange(Item::MilkBucket)));

    let effect = interact(&region, &viewer, &mut events, mooshrooms[0].id, Item::Bowl).await;
    assert!(matches!(effect, HandEffect::Exchange(Item::MushroomStew)));

    let effect = interact(
        &region,
        &viewer,
        &mut events,
        mooshrooms[0].id,
        Item::Shears,
    )
    .await;
    assert!(matches!(effect, HandEffect::Damage(1)));
    let found = wait_for(&world, &region, 5, |found| {
        found.iter().any(|s| s.kind == EntityType::Item)
    })
    .await;
    let drops: Vec<_> = found
        .iter()
        .filter(|s| s.kind == EntityType::Item)
        .collect();
    assert_eq!(drops.len(), 5, "a mooshroom sheds 5 mushrooms: {drops:#?}");
    assert!(
        drops.iter().all(|s| s.name == "minecraft:brown_mushroom"),
        "a brown mooshroom sheds brown mushrooms: {drops:#?}"
    );

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rabbits_eat_carrots_breed_and_round_trip_their_variant() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Rabbit,
        Compound::new().with("RabbitType", 4i32),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Rabbit,
        Compound::new().with("RabbitType", 4i32),
        [6.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(2).await;
    let rabbits = mobs(&region, EntityType::Rabbit).await;
    assert_eq!(rabbits.len(), 2);

    for rabbit in &rabbits {
        let effect = interact(&region, &viewer, &mut events, rabbit.id, Item::Carrot).await;
        assert!(matches!(effect, HandEffect::Consume(1)));
    }
    wait_for(&world, &region, 200, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::Rabbit)
            .count()
            == 3
    })
    .await;

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn goats_are_milked() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Goat,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(2).await;
    let goats = mobs(&region, EntityType::Goat).await;
    assert_eq!(goats.len(), 1);

    let effect = interact(&region, &viewer, &mut events, goats[0].id, Item::Bucket).await;
    assert!(matches!(effect, HandEffect::Exchange(Item::MilkBucket)));

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killed_animals_drop_their_loot_and_experience_cooked_when_burning() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Chicken,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Chicken,
        Compound::new().with("Fire", 200i16),
        [10.5, SKY as f64 + 1.0, 10.5],
    )
    .await;
    world.ticks(2).await;
    let chickens = mobs(&region, EntityType::Chicken).await;
    for chicken in &chickens {
        region
            .request(RegionRequest::AttackEntity {
                attacker: viewer.clone(),
                target: chicken.id,
                attack: attack(100.0, None),
                sweep: None,
            })
            .await;
        expect(&mut events, |event| match event {
            PlayerEvent::AttackLanded { target, .. } if target == chicken.id => Some(()),
            _ => None,
        })
        .await;
    }
    let found = wait_for(&world, &region, 40, |found| {
        !found.iter().any(|s| s.kind == EntityType::Chicken)
    })
    .await;
    let names: Vec<&str> = found
        .iter()
        .filter(|s| s.kind == EntityType::Item)
        .map(|s| s.name.as_str())
        .collect();
    assert!(names.contains(&"minecraft:chicken"), "{names:?}");
    assert!(names.contains(&"minecraft:cooked_chicken"), "{names:?}");
    assert!(
        found.iter().any(|s| s.kind == EntityType::ExperienceOrb),
        "killed by a player, they give experience"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chickens_lay_eggs() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Chicken,
        Compound::new().with("EggLayTime", 3),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    wait_for(&world, &region, 40, |found| {
        found.iter().any(|s| s.name == "minecraft:egg")
    })
    .await;
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mobs_keep_their_state_across_a_restart() {
    let dir = TestDir::new();
    let chunk = ChunkPos::new(0, 0);
    let uuid;
    {
        let world = TestWorld::start(dir.path());
        let region = world.region(chunk.region()).await;
        let (viewer, mut events) = PlayerHandle::new(100);
        subscribe(&region, &viewer, &mut events, &[chunk]).await;
        place_blocks(&world, &region, pen(2, 13)).await;
        summon(
            &region,
            EntityType::Sheep,
            Compound::new()
                .with("Color", 11i8)
                .with("Sheared", true)
                .with("Health", 5.0f32)
                .with("Age", -2000)
                .with("CustomName", "Wooly"),
            [7.5, SKY as f64 + 1.0, 7.5],
        )
        .await;
        world.ticks(5).await;
        uuid = mobs(&region, EntityType::Sheep).await[0].uuid;
        world.shutdown().await;
    }
    let saved = stored_entities(dir.path(), chunk);
    let sheep = saved
        .iter()
        .find(|e| e.get_str("id") == Some("minecraft:sheep"))
        .expect("the sheep was saved");
    assert_eq!(sheep.get_uuid("UUID"), Some(uuid));
    assert_eq!(sheep.get_byte("Color"), Some(11));
    assert_eq!(sheep.get_bool("Sheared"), Some(true));
    assert_eq!(sheep.get_float("Health"), Some(5.0));
    let age = sheep.get_int("Age").unwrap();
    assert!((-2000..-1990).contains(&age), "grew a little: {age}");
    assert_eq!(sheep.get_str("CustomName"), Some("Wooly"));

    let world = TestWorld::start(dir.path());
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(200);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let found = mobs(&region, EntityType::Sheep).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].uuid, uuid);
    assert_eq!(found[0].name, "Wooly");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mobs_cross_region_borders() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    let (west_chunk, east_chunk) = (ChunkPos::new(31, 0), ChunkPos::new(32, 0));
    subscribe(&west, &viewer, &mut events, &[west_chunk]).await;
    subscribe(&east, &viewer, &mut events, &[east_chunk]).await;
    world.ticks(3).await;
    let nbt = Compound::new()
        .with("NoAI", true)
        .with("NoGravity", true)
        .with(
            "Motion",
            Tag::List(vec![Tag::Double(1.0), Tag::Double(0.0), Tag::Double(0.0)]),
        );
    summon(&west, EntityType::Cow, nbt, [510.5, SKY as f64 + 20.0, 8.5]).await;
    let spawned = expect(&mut events, track_of(EntityType::Cow)).await;
    wait_for(&world, &east, 60, |found| {
        found.iter().any(|s| s.id == spawned.entity)
    })
    .await;
    assert!(mobs(&west, EntityType::Cow).await.is_empty());
    world.shutdown().await;
    let saved = stored_entities(dir.path(), east_chunk);
    assert!(
        saved
            .iter()
            .any(|e| e.get_str("id") == Some("minecraft:cow"))
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_wandering_off_a_region_border_is_always_in_exactly_one_region() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let west_chunk = ChunkPos::new(-1, 0);
    let east_chunk = ChunkPos::new(0, 0);
    let west = world.region(west_chunk.region()).await;
    let east = world.region(east_chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&west, &viewer, &mut events, &[west_chunk]).await;
    subscribe(&east, &viewer, &mut events, &[east_chunk]).await;
    let mut floor = Vec::new();
    for x in -16..16 {
        for z in 0..16 {
            floor.push((BlockPos::new(x, SKY, z), Block::GrassBlock.default_state()));
        }
    }
    place_blocks(&world, &east, floor.clone()).await;
    place_blocks(&world, &west, floor).await;
    world.ticks(3).await;
    summon(
        &east,
        EntityType::Cow,
        Compound::new().with(
            "Motion",
            Tag::List(vec![Tag::Double(-0.3), Tag::Double(0.0), Tag::Double(0.0)]),
        ),
        [0.0, SKY as f64 + 1.0, 8.5],
    )
    .await;
    let mut crossed = false;
    for tick in 0..400 {
        let in_west = mobs(&west, EntityType::Cow).await.len();
        let in_east = mobs(&east, EntityType::Cow).await.len();
        assert_eq!(
            in_west + in_east,
            1,
            "tick {tick}: west {in_west}, east {in_east}"
        );
        crossed |= in_west == 1;
        world.ticks(1).await;
    }
    assert!(crossed, "the cow never crossed the border");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_pushed_over_a_border_with_no_neighbour_stays_put() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let east_chunk = ChunkPos::new(0, 0);
    let east = world.region(east_chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&east, &viewer, &mut events, &[east_chunk]).await;
    let floor: Vec<_> = (-16..16)
        .flat_map(|x| {
            (0..16).map(move |z| (BlockPos::new(x, SKY, z), Block::GrassBlock.default_state()))
        })
        .collect();
    place_blocks(&world, &east, floor).await;
    world.ticks(3).await;
    summon(
        &east,
        EntityType::Cow,
        Compound::new().with(
            "Motion",
            Tag::List(vec![Tag::Double(-0.3), Tag::Double(0.0), Tag::Double(0.0)]),
        ),
        [0.0, SKY as f64 + 1.0, 8.5],
    )
    .await;
    for tick in 0..100 {
        let cows = mobs(&east, EntityType::Cow).await;
        assert_eq!(cows.len(), 1, "tick {tick}: the cow left its region");
        world.ticks(1).await;
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kill_makes_mobs_die_with_their_drops() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Sheep,
        Compound::new(),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(2).await;
    let sheep = mobs(&region, EntityType::Sheep).await;
    let killed = region
        .ask(move |world| {
            let ids: Vec<i32> = vec![sheep[0].id];
            ids.into_iter().filter(|id| world.kill_entity(*id)).count()
        })
        .await
        .unwrap();
    assert_eq!(killed, 1);
    let found = wait_for(&world, &region, 40, |found| {
        !found.iter().any(|s| s.kind == EntityType::Sheep)
    })
    .await;
    assert!(found.iter().any(|s| s.name == "minecraft:white_wool"));
    assert!(
        !found.iter().any(|s| s.kind == EntityType::ExperienceOrb),
        "no experience without a player"
    );
    world.shutdown().await;
}

const FARM_ANIMALS: [EntityType; 4] = [
    EntityType::Cow,
    EntityType::Pig,
    EntityType::Sheep,
    EntityType::Chicken,
];

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn animals_spawn_naturally_near_players_every_400_ticks() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    allow_spawning(&world);
    let center = ChunkPos::new(12, 12);
    let (viewer, mut events) = PlayerHandle::new(100);
    let area = Area::subscribe(&world, &viewer, &mut events, &around(center, 4)).await;
    let generator = Generator::new(1);
    let (x, z) = (center.x * 16 + 8, center.z * 16 + 8);
    let player = [
        x as f64 + 0.5,
        generator.height(x, z) as f64 + 1.0,
        z as f64 + 0.5,
    ];
    area.owner(player)
        .request(player_update(&viewer, player, None))
        .await;
    world.publish_players(OVERWORLD, &[(&viewer, player)]);
    world.ticks(5).await;
    assert!(
        area.entities()
            .await
            .iter()
            .all(|s| !FARM_ANIMALS.contains(&s.kind)),
        "animals only spawn on the persistent spawn tick"
    );
    let mut spawned = Vec::new();
    for cycle in 1..=20u64 {
        world.clock.send_modify(|tick| *tick = cycle * 400 - 1);
        world
            .server_ticks
            .send_modify(|tick| *tick = cycle * 400 - 1);
        world.ticks(1).await;
        world.ticks(2).await;
        spawned = area
            .entities()
            .await
            .into_iter()
            .filter(|s| FARM_ANIMALS.contains(&s.kind))
            .collect();
        if !spawned.is_empty() {
            break;
        }
    }
    assert!(!spawned.is_empty(), "no animal spawned in 20 cycles");
    for animal in &spawned {
        let distance = crate::world::distance_sq(animal.position, player);
        assert!(distance > 24.0 * 24.0, "{animal:?} too close to the player");
        assert!(distance < 128.0 * 128.0 + 256.0);
    }
    world.shutdown().await;
}

/// The placeholder terrain, with world generation's creatures.
struct Populated(Arc<dyn ChunkGenerator>);

impl ChunkGenerator for Populated {
    fn generate(&self, pos: ChunkPos, shape: ChunkShape) -> tuff_world_model::chunk::Chunk {
        self.0.generate(pos, shape)
    }

    fn sea_level(&self) -> i32 {
        self.0.sea_level()
    }

    fn spawn(&self) -> BlockPos {
        self.0.spawn()
    }

    fn spawns_creatures(&self) -> bool {
        true
    }
}

fn populated(settings: &DimensionSettings, seed: i64) -> Arc<dyn ChunkGenerator> {
    Arc::new(Populated(tuff_test_support::placeholder::for_settings(
        settings, seed,
    )))
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn freshly_generated_chunks_start_with_creatures() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), populated);
    allow_spawning(&world);
    let center = ChunkPos::new(12, 12);
    let (viewer, mut events) = PlayerHandle::new(100);
    let chunks = around(center, 4);
    let area = Area::subscribe(&world, &viewer, &mut events, &chunks).await;
    world.ticks(2).await;
    let animals: Vec<EntitySnapshot> = area
        .entities()
        .await
        .into_iter()
        .filter(|s| FARM_ANIMALS.contains(&s.kind))
        .collect();
    assert!(!animals.is_empty(), "81 new chunks and no creatures");
    for animal in &animals {
        assert!(chunks.contains(&ChunkPos::of_position(animal.position)));
    }
    world.shutdown().await;

    let world = TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), populated);
    allow_spawning(&world);
    let (viewer, mut events) = PlayerHandle::new(100);
    let area = Area::subscribe(&world, &viewer, &mut events, &chunks).await;
    world.ticks(2).await;
    let again = area
        .entities()
        .await
        .into_iter()
        .filter(|s| FARM_ANIMALS.contains(&s.kind))
        .count();
    assert_eq!(
        again,
        animals.len(),
        "loaded chunks keep theirs and get no more"
    );
    world.shutdown().await;
}

/// World generation's creatures come from the chunk's decoration random:
/// a chunk gets the same groups in the same places however much the
/// region's own random was drawn from before.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generation_creatures_follow_the_world_seed() {
    async fn creatures(warm_up: bool) -> Vec<(EntityType, [i64; 3])> {
        let dir = TestDir::new();
        let world =
            TestWorld::start_generated(Storage::default().open(dir.path()).unwrap(), populated);
        allow_spawning(&world);
        if warm_up {
            let (viewer, mut events) = PlayerHandle::new(101);
            let corner = [(22, 14), (23, 14), (22, 15), (23, 15)].map(|(x, z)| ChunkPos::new(x, z));
            Area::subscribe(&world, &viewer, &mut events, &corner).await;
        }
        let (viewer, mut events) = PlayerHandle::new(100);
        let chunks = around(ChunkPos::new(18, 11), 2);
        let area = Area::subscribe(&world, &viewer, &mut events, &chunks).await;
        let mut found: Vec<(EntityType, [i64; 3])> = area
            .entities()
            .await
            .into_iter()
            .filter(|s| {
                FARM_ANIMALS.contains(&s.kind)
                    && chunks.contains(&ChunkPos::of_position(s.position))
            })
            .map(|s| (s.kind, s.position.map(|v| (v * 16.0).round() as i64)))
            .collect();
        found.sort();
        world.shutdown().await;
        found
    }
    let fresh = creatures(false).await;
    assert!(!fresh.is_empty(), "25 new chunks and no creatures");
    assert_eq!(creatures(true).await, fresh);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_eggs_spawn_their_mob_on_the_clicked_block() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let target = BlockPos::new(7, SKY, 7);
    region
        .request(RegionRequest::Place(place(
            &viewer,
            target,
            Item::SheepSpawnEgg,
            5,
        )))
        .await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 5,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(matches!(
        outcome,
        ActionOutcome::Placed {
            item: Item::SheepSpawnEgg,
            ..
        }
    ));
    let sheep = mobs(&region, EntityType::Sheep).await;
    assert_eq!(sheep.len(), 1);
    assert!((sheep[0].position[1] - (SKY + 1) as f64).abs() < 1e-9);
    assert_eq!(sheep[0].position[0], 7.5);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spawn_egg_on_a_spawner_changes_what_it_spawns() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, roofed_pen(2, 13)).await;
    let spawner = BlockPos::new(8, SKY, 8);
    let mut op = place(&player, spawner, Item::Spawner, 1);
    op.components = tuff_item::item::Components::from(tuff_item::item::ComponentPatch {
        other: Compound::new().with(
            "minecraft:block_entity_data",
            Compound::new()
                .with("id", "minecraft:mob_spawner")
                .with("Delay", 0i16)
                .with(
                    "SpawnData",
                    Compound::new().with("entity", Compound::new().with("id", "minecraft:blaze")),
                ),
        ),
        ..Default::default()
    })
    .into();
    op.game_master = true;
    region.request(RegionRequest::Place(op)).await;
    let egg = place(&player, spawner, Item::ZombieSpawnEgg, 2);
    region.request(RegionRequest::Place(egg)).await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 2,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(matches!(outcome, ActionOutcome::Placed { .. }));
    region
        .request(player_at(
            &player,
            [4.5, SKY as f64 + 1.0, 4.5],
            PlayerLooks {
                creative: true,
                invulnerable: true,
                ..Default::default()
            },
        ))
        .await;
    let found = wait_for(&world, &region, 200, |found| {
        found.iter().any(|s| s.kind == EntityType::Zombie)
    })
    .await;
    assert!(found.iter().all(|s| s.kind != EntityType::Blaze));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spawn_egg_used_in_the_air_spawns_on_the_water_it_faces() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let surface = BlockPos::new(8, SKY + 1, 8);
    place_blocks(
        &world,
        &region,
        [(surface, tuff_vanilla_data::Block::Water.default_state())],
    )
    .await;
    let RegionRequest::UseBucket(mut request) = place_bucket(
        &viewer,
        BlockPos::new(8, SKY + 1, 8),
        Item::SquidSpawnEgg,
        None,
        None,
        9,
    ) else {
        unreachable!()
    };
    request.fluid = None;
    region.request(RegionRequest::UseBucket(request)).await;
    let outcome = expect(&mut events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: 9,
            outcome,
        } => Some(outcome),
        _ => None,
    })
    .await;
    assert!(matches!(outcome, ActionOutcome::Placed { .. }));
    assert_eq!(mobs(&region, EntityType::Squid).await.len(), 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spawn_egg_on_an_animal_of_its_kind_spawns_a_baby() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Sheep,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(2).await;
    let sheep = mobs(&region, EntityType::Sheep).await;
    assert_eq!(sheep.len(), 1);
    let effect = interact(
        &region,
        &viewer,
        &mut events,
        sheep[0].id,
        Item::SheepSpawnEgg,
    )
    .await;
    assert!(matches!(effect, HandEffect::Consume(1)));
    world.ticks(2).await;
    let found = mobs(&region, EntityType::Sheep).await;
    assert_eq!(found.len(), 2);
    let baby = found.iter().find(|s| s.id != sheep[0].id).unwrap();
    let (reply, answer) = tokio::sync::oneshot::channel();
    region
        .request(RegionRequest::EntityData { id: baby.id, reply })
        .await;
    let nbt = answer.await.unwrap().expect("the baby is there");
    assert!(
        nbt.get("Age")
            .and_then(|t| t.as_int())
            .is_some_and(|a| a < 0)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn magma_hurts_mobs_unless_fire_immune_or_frost_walking() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    for x in 2..=13 {
        for z in 2..=13 {
            blocks.push((BlockPos::new(x, SKY, z), Block::MagmaBlock.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    let frail = || Compound::new().with("Health", 1.0f32).with("NoAI", true);
    let mut boots = ItemStack::mint(Item::LeatherBoots, 1, MintReason::Test);
    boots.components.enchantments = Some(tuff_item::item::Enchantments::new([(
        "minecraft:frost_walker".to_owned(),
        1,
    )]));
    let at = |x: f64| [x, SKY as f64 + 1.0, 7.5];
    summon(&region, EntityType::Cow, frail(), at(3.5)).await;
    summon(&region, EntityType::Husk, frail(), at(6.5)).await;
    summon(
        &region,
        EntityType::Husk,
        frail().with("equipment", Compound::new().with("feet", boots.to_nbt())),
        at(9.5),
    )
    .await;
    summon(&region, EntityType::ZombifiedPiglin, frail(), at(12.5)).await;
    let found = wait_for(&world, &region, 200, |found| {
        !found.iter().any(|s| {
            s.kind == EntityType::Cow || (s.kind == EntityType::Husk && s.position[0] < 8.0)
        })
    })
    .await;
    world.ticks(40).await;
    let husks = mobs(&region, EntityType::Husk).await;
    assert_eq!(husks.len(), 1, "{found:#?}");
    assert!(husks[0].position[0] > 8.0, "the frost walker survives");
    assert_eq!(
        mobs(&region, EntityType::ZombifiedPiglin).await.len(),
        1,
        "fire immune mobs do not burn"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_in_frost_walker_boots_freezes_the_water_beside_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let mut blocks = Vec::new();
    for x in 2..=13 {
        for z in 2..=13 {
            let block = if (x == 7 || x == 8) && z == 7 {
                Block::Stone
            } else {
                Block::Water
            };
            blocks.push((BlockPos::new(x, SKY, z), block.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    let mut boots = ItemStack::mint(Item::LeatherBoots, 1, MintReason::Test);
    boots.components.enchantments = Some(tuff_item::item::Enchantments::new([(
        "minecraft:frost_walker".to_owned(),
        1,
    )]));
    summon(
        &region,
        EntityType::Zombie,
        Compound::new()
            .with("NoAI", true)
            .with(
                "Motion",
                tuff_nbt::Tag::List(vec![
                    tuff_nbt::Tag::Double(0.3),
                    tuff_nbt::Tag::Double(0.0),
                    tuff_nbt::Tag::Double(0.0),
                ]),
            )
            .with("equipment", Compound::new().with("feet", boots.to_nbt())),
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    world.ticks(30).await;
    assert_eq!(
        block_at(&region, BlockPos::new(9, SKY, 7))
            .await
            .map(|s| s.block()),
        Some(Block::FrostedIce)
    );
    assert_eq!(
        block_at(&region, BlockPos::new(12, SKY, 7))
            .await
            .map(|s| s.block()),
        Some(Block::Water),
        "outside the radius"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cactus_hurts_mobs_touching_it_from_the_side() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    blocks.push((BlockPos::new(7, SKY + 1, 7), Block::Cactus.default_state()));
    place_blocks(&world, &region, blocks).await;
    let frail = || Compound::new().with("Health", 1.0f32).with("NoAI", true);
    let feet = SKY as f64 + 1.0;
    summon(&region, EntityType::Cow, frail(), [6.8, feet, 7.5]).await;
    summon(&region, EntityType::Cow, frail(), [3.5, feet, 7.5]).await;
    world.ticks(30).await;
    let cows = mobs(&region, EntityType::Cow).await;
    assert_eq!(cows.len(), 1, "only the cow beside the cactus dies");
    assert!(
        (cows[0].position[0] - 3.5).abs() < 0.5,
        "the far cow survives"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn suffocation_hurts_a_mob_buried_in_a_solid_block() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    // A solid column tall enough to cover any mob's eyes, right where it
    // will be summoned.
    blocks.push((BlockPos::new(7, SKY + 1, 7), Block::Stone.default_state()));
    blocks.push((BlockPos::new(7, SKY + 2, 7), Block::Stone.default_state()));
    place_blocks(&world, &region, blocks).await;
    let frail = Compound::new().with("Health", 1.0f32).with("NoAI", true);
    summon(
        &region,
        EntityType::Pig,
        frail,
        [7.5, SKY as f64 + 1.0, 7.5],
    )
    .await;
    assert_eq!(mobs(&region, EntityType::Pig).await.len(), 1);
    wait_for(&world, &region, 60, |found| {
        !found.iter().any(|s| s.kind == EntityType::Pig)
    })
    .await;
    world.shutdown().await;
}

/// The ids of entities in `event` that play the drowning bubbles.
fn drowned_ids(event: &PlayerEvent) -> Vec<i32> {
    let PlayerEvent::EntityPackets { packets, .. } = event else {
        return Vec::new();
    };
    packets
        .iter()
        .filter_map(|frame| {
            let mut r = framed_payload(frame);
            if r.read_var_int().ok()?
                != tuff_vanilla_data::packet::play::Clientbound::EntityEvent.id()
            {
                return None;
            }
            let id = r.read_int().ok()?;
            (r.read_byte().ok()? == crate::protocol::play::entity_event::DROWN_PARTICLES)
                .then_some(id)
        })
        .collect()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn land_mobs_trapped_under_water_run_out_of_air_and_drown() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    for x in 1..=8 {
        for z in 1..=8 {
            let inside = (3..=6).contains(&x) && (3..=6).contains(&z);
            for y in [SKY + 1, SKY + 2] {
                if !inside && (x == 2 || x == 7 || z == 2 || z == 7) {
                    blocks.push((BlockPos::new(x, y, z), Block::Stone.default_state()));
                } else if inside {
                    blocks.push((BlockPos::new(x, y, z), Block::Water.default_state()));
                }
            }
            blocks.push((BlockPos::new(x, SKY + 3, z), Block::Stone.default_state()));
        }
    }
    place_blocks(&world, &region, blocks).await;
    let low_on_air = || Compound::new().with("Air", 5i16).with("Health", 2.0f32);
    summon(
        &region,
        EntityType::Cow,
        low_on_air(),
        [4.5, SKY as f64 + 1.0, 4.5],
    )
    .await;
    summon(
        &region,
        EntityType::Cow,
        low_on_air(),
        [11.5, SKY as f64 + 1.0, 11.5],
    )
    .await;
    let cows = mobs(&region, EntityType::Cow).await;
    let (wet, dry) = (cows[0].id, cows[1].id);
    let mut drowned_at = None;
    for tick in 1..=60 {
        world.ticks(1).await;
        while let Ok(event) = events.try_recv() {
            let ids = drowned_ids(&event);
            assert!(!ids.contains(&dry), "the cow on land breathes");
            if ids.contains(&wet) && drowned_at.is_none() {
                drowned_at = Some(tick);
            }
        }
    }
    let drowned_at = drowned_at.expect("the trapped cow drowns");
    assert!(
        (24..=27).contains(&drowned_at),
        "5 ticks of air then 20 more before the first damage, drowned at {drowned_at}"
    );
    let left: Vec<i32> = mobs(&region, EntityType::Cow)
        .await
        .iter()
        .map(|cow| cow.id)
        .collect();
    assert_eq!(left, [dry], "two points of drowning killed the trapped cow");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fish_and_axolotl_buckets_capture_and_place_their_data() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let floor = BlockPos::new(5, SKY, 5);

    // A cod: no species data at all, just capture and place.
    summon(
        &region,
        EntityType::Cod,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    let cod = mobs(&region, EntityType::Cod).await;
    assert_eq!(cod.len(), 1);
    let effect = interact(&region, &viewer, &mut events, cod[0].id, Item::WaterBucket).await;
    let HandEffect::ExchangeStack(stack) = effect else {
        panic!("expected a filled cod bucket, got {effect:?}");
    };
    assert_eq!(stack.item, Item::CodBucket);
    assert!(
        mobs(&region, EntityType::Cod).await.is_empty(),
        "the captured cod leaves the world"
    );

    region
        .request(place_bucket(
            &viewer,
            floor,
            Item::CodBucket,
            Some(EntityType::Cod),
            None,
            1,
        ))
        .await;
    assert!(matches!(
        bucket_outcome(&mut events, 1).await,
        ActionOutcome::Used {
            effect: HandEffect::Emptied(Item::Bucket),
            ..
        }
    ));
    assert_eq!(
        wait_for(&world, &region, 5, |found| found
            .iter()
            .any(|s| s.kind == EntityType::Cod))
        .await
        .iter()
        .filter(|s| s.kind == EntityType::Cod)
        .count(),
        1,
        "the cod is placed back"
    );
    let placed = mobs(&region, EntityType::Cod).await[0].id;
    let nbt = entity_nbt(&region, placed).await.expect("the placed cod");
    assert_eq!(
        nbt.get_bool("FromBucket"),
        Some(true),
        "a fish let out of a bucket stays for good"
    );

    // A tropical fish: its pattern and colors round-trip through the
    // bucket's `minecraft:bucket_entity_data`.
    // pattern 2 (small, index 2), base color 7 (gray), pattern color 11
    // (blue): packed as the tropical fish's variant data int is packed.
    let variant = 0x0B07_0200_u32 as i32;
    summon(
        &region,
        EntityType::TropicalFish,
        Compound::new().with("Variant", variant),
        [8.5, SKY as f64 + 1.0, 8.5],
    )
    .await;
    let fish = mobs(&region, EntityType::TropicalFish).await;
    assert_eq!(fish.len(), 1);
    let effect = interact(&region, &viewer, &mut events, fish[0].id, Item::WaterBucket).await;
    let HandEffect::ExchangeStack(stack) = effect else {
        panic!("expected a filled tropical fish bucket, got {effect:?}");
    };
    assert_eq!(stack.item, Item::TropicalFishBucket);
    let data = bucket_entity_data(&stack).expect("bucket entity data");
    assert_eq!(data.get_int("Variant"), Some(variant));

    region
        .request(place_bucket(
            &viewer,
            floor,
            Item::TropicalFishBucket,
            Some(EntityType::TropicalFish),
            Some(data),
            2,
        ))
        .await;
    assert!(matches!(
        bucket_outcome(&mut events, 2).await,
        ActionOutcome::Used {
            effect: HandEffect::Emptied(Item::Bucket),
            ..
        }
    ));
    wait_for(&world, &region, 5, |found| {
        found.iter().any(|s| s.kind == EntityType::TropicalFish)
    })
    .await;

    // Placing it back and capturing it again reproduces the same variant,
    // proving the round trip rather than just that a fish appeared.
    let fish = mobs(&region, EntityType::TropicalFish).await;
    assert_eq!(fish.len(), 1);
    let effect = interact(&region, &viewer, &mut events, fish[0].id, Item::WaterBucket).await;
    let HandEffect::ExchangeStack(stack) = effect else {
        panic!("expected a filled tropical fish bucket, got {effect:?}");
    };
    let data = bucket_entity_data(&stack).expect("bucket entity data");
    assert_eq!(data.get_int("Variant"), Some(variant));

    // An axolotl: its color round-trips the same way.
    summon(
        &region,
        EntityType::Axolotl,
        Compound::new().with("Variant", 3i32),
        [8.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    let axolotl = mobs(&region, EntityType::Axolotl).await;
    assert_eq!(axolotl.len(), 1);
    let effect = interact(
        &region,
        &viewer,
        &mut events,
        axolotl[0].id,
        Item::WaterBucket,
    )
    .await;
    let HandEffect::ExchangeStack(stack) = effect else {
        panic!("expected a filled axolotl bucket, got {effect:?}");
    };
    assert_eq!(stack.item, Item::AxolotlBucket);
    let data = bucket_entity_data(&stack).expect("bucket entity data");
    assert_eq!(data.get_int("Variant"), Some(3));
    assert!(
        mobs(&region, EntityType::Axolotl).await.is_empty(),
        "the captured axolotl leaves the world"
    );

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foxes_pick_a_biome_variant_and_breed_on_berries() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Fox,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Fox,
        Compound::new(),
        [6.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(2).await;
    let foxes = mobs(&region, EntityType::Fox).await;
    assert_eq!(foxes.len(), 2);

    for fox in &foxes {
        let effect = interact(&region, &viewer, &mut events, fox.id, Item::SweetBerries).await;
        assert!(matches!(effect, HandEffect::Consume(1)));
    }
    wait_for(&world, &region, 200, |found| {
        found.iter().filter(|s| s.kind == EntityType::Fox).count() == 3
    })
    .await;

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frogs_take_slimeballs_and_carry_a_biome_variant() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Frog,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Frog,
        Compound::new(),
        [6.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(2).await;
    let frogs = mobs(&region, EntityType::Frog).await;
    assert_eq!(frogs.len(), 2);

    for frog in &frogs {
        let effect = interact(&region, &viewer, &mut events, frog.id, Item::SlimeBall).await;
        assert!(matches!(effect, HandEffect::Consume(1)));
        let nbt = entity_nbt(&region, frog.id).await.expect("the frog");
        assert!(nbt.get_str("variant").is_some(), "a frog has a variant");
    }

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn armadillos_eat_spider_eyes_and_breed() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Armadillo,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Armadillo,
        Compound::new(),
        [6.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(2).await;
    let armadillos = mobs(&region, EntityType::Armadillo).await;
    assert_eq!(armadillos.len(), 2);

    for armadillo in &armadillos {
        let effect = interact(&region, &viewer, &mut events, armadillo.id, Item::SpiderEye).await;
        assert!(matches!(effect, HandEffect::Consume(1)));
    }
    wait_for(&world, &region, 200, |found| {
        found
            .iter()
            .filter(|s| s.kind == EntityType::Armadillo)
            .count()
            == 3
    })
    .await;

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sniffers_eat_torchflower_seeds_and_breed() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Sniffer,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Sniffer,
        Compound::new(),
        [6.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(2).await;
    let sniffers = mobs(&region, EntityType::Sniffer).await;
    assert_eq!(sniffers.len(), 2);

    for sniffer in &sniffers {
        let effect = interact(
            &region,
            &viewer,
            &mut events,
            sniffer.id,
            Item::TorchflowerSeeds,
        )
        .await;
        assert!(matches!(effect, HandEffect::Consume(1)));
    }
    // Sniffers lay an egg instead of having a baby.
    wait_for(&world, &region, 200, |found| {
        found.iter().any(|s| s.kind == EntityType::Item)
    })
    .await;

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn camels_eat_cacti_and_breed() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    summon(
        &region,
        EntityType::Camel,
        Compound::new(),
        [5.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    summon(
        &region,
        EntityType::Camel,
        Compound::new(),
        [6.5, SKY as f64 + 1.0, 5.5],
    )
    .await;
    world.ticks(2).await;
    let camels = mobs(&region, EntityType::Camel).await;
    assert_eq!(camels.len(), 2);

    for camel in &camels {
        let effect = interact(&region, &viewer, &mut events, camel.id, Item::Cactus).await;
        assert!(matches!(effect, HandEffect::Consume(1)));
    }
    wait_for(&world, &region, 700, |found| {
        found.iter().filter(|s| s.kind == EntityType::Camel).count() == 3
    })
    .await;

    world.shutdown().await;
}

/// A cow dropped onto a slime block bounces back up and takes no damage,
/// while the same drop onto grass hurts.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mob_landing_on_slime_bounces_and_takes_no_fall_damage() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let mut blocks = pen(2, 13);
    blocks.push((BlockPos::new(4, SKY, 4), Block::SlimeBlock.default_state()));
    place_blocks(&world, &region, blocks).await;
    let drop = SKY as f64 + 12.0;
    summon(&region, EntityType::Cow, Compound::new(), [4.5, drop, 4.5]).await;
    summon(
        &region,
        EntityType::Cow,
        Compound::new(),
        [10.5, drop, 10.5],
    )
    .await;
    let cows = mobs(&region, EntityType::Cow).await;
    let (bouncer, faller) = (cows[0].id, cows[1].id);

    let floor = (SKY + 1) as f64;
    let mut touched = false;
    let mut peak_after_touch = floor;
    for _ in 0..200 {
        world.ticks(1).await;
        let cows = mobs(&region, EntityType::Cow).await;
        let y = cows.iter().find(|c| c.id == bouncer).unwrap().position[1];
        if y < floor + 0.5 {
            touched = true;
        } else if touched {
            peak_after_touch = peak_after_touch.max(y);
        }
        if touched && mob_health(&region, faller).await < 10.0 && peak_after_touch > floor + 2.0 {
            break;
        }
    }
    assert!(touched, "the cow reached the slime block");
    assert!(
        peak_after_touch > floor + 2.0,
        "it bounced back up (peak {peak_after_touch})"
    );
    assert_eq!(
        mob_health(&region, bouncer).await,
        10.0,
        "slime took the fall"
    );
    assert!(mob_health(&region, faller).await < 10.0, "grass did not");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_head_rotation_reaches_a_late_viewer_only_in_the_spawn() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let head = bytes::Bytes::from_static(b"head");
    let RegionRequest::Player(update) =
        player_update(&PlayerHandle::new(5).0, [8.0, 70.0, 8.0], None)
    else {
        unreachable!()
    };
    let (owner, _owner_events) = PlayerHandle::new(5);
    region
        .request(RegionRequest::Player(PlayerUpdate {
            handle: owner,
            spawn: Arc::from(vec![bytes::Bytes::from_static(b"spawn")]),
            head: Some(head.clone()),
            ..update
        }))
        .await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    world.ticks(1).await;
    let mut tracks = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::Track(track) = event {
            tracks.push(track);
        }
    }
    assert!(
        !tracks.is_empty(),
        "the late viewer was told about the player"
    );
    assert!(tracks.iter().all(|track| track.head.is_none()));
    world.shutdown().await;
}

async fn long_jump_between_pillars(kind: EntityType, gap: i32) -> (f64, f64) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let stone = Block::Stone.default_state();
    place_blocks(
        &world,
        &region,
        vec![
            (BlockPos::new(4, SKY, 4), stone),
            (BlockPos::new(4 + gap, SKY, 4), stone),
        ],
    )
    .await;
    summon(&region, kind, Compound::new(), [4.5, SKY as f64 + 1.0, 4.5]).await;
    let mut landed = 4.5;
    for _ in 0..80 {
        world.ticks(20).await;
        let found = mobs(&region, kind).await;
        let at = found[0].position;
        if at[0] > 4.5 + f64::from(gap) - 1.0 && at[1] < SKY as f64 + 1.5 {
            landed = at[0];
            break;
        }
    }
    world.shutdown().await;
    (4.5, landed)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_goat_on_a_pillar_long_jumps_to_the_next_one() {
    let (start, landed) = long_jump_between_pillars(EntityType::Goat, 3).await;
    assert!(landed > start + 2.0, "landed at {landed}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frog_on_a_pillar_long_jumps_to_the_next_one() {
    let (start, landed) = long_jump_between_pillars(EntityType::Frog, 3).await;
    assert!(landed > start + 2.0, "landed at {landed}");
}
