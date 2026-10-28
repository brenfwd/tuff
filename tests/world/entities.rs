//! Region-owned entities saved with their chunks: primed TNT across chunk
//! unloads, region borders and restarts.

use super::*;
use tuff_item::text::TextComponent;
use tuff_vanilla_data::registry::EntityType;

const SKY: i32 = 150;

fn tnt_track(event: PlayerEvent) -> Option<crate::world::messages::Track> {
    match event {
        PlayerEvent::Track(track)
            if track.player.is_none()
                && spawned_entity_kind(&track) == EntityType::Tnt.id() as i32 =>
        {
            Some(track)
        }
        _ => None,
    }
}

async fn summon(region: &RegionHandle, nbt: Compound, position: [f64; 3]) {
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: nbt.with("id", "minecraft:tnt"),
            position,
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap(), "TNT loads from NBT");
}

async fn snapshots(region: &RegionHandle) -> Vec<crate::world::messages::EntitySnapshot> {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer.await.unwrap()
}

fn stored_tnt(dir: &Path, chunk: ChunkPos) -> Option<Compound> {
    let root = stored_chunk(dir, chunk, true)?;
    let entities = root.get_list("Entities")?;
    entities
        .iter()
        .filter_map(Tag::as_compound)
        .find(|entity| entity.get_str("id") == Some("minecraft:tnt"))
        .cloned()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lit_tnt_is_saved_at_shutdown_and_resumes_its_fuse_after_a_restart() {
    let dir = TestDir::new();
    let chunk = ChunkPos::new(0, 0);
    let tnt = BlockPos::new(8, SKY + 1, 8);
    let floor = BlockPos::new(8, SKY, 8);
    {
        let (player, mut events) = PlayerHandle::new(100);
        let world = TestWorld::start(dir.path());
        let region = world.region(chunk.region()).await;
        subscribe(&region, &player, &mut events, &[chunk]).await;
        let mut blocks: Vec<_> = (6..=10)
            .flat_map(|x| (6..=10).map(move |z| BlockPos::new(x, SKY, z)))
            .map(|pos| (pos, Block::Stone.default_state()))
            .collect();
        blocks.push((tnt, Block::Tnt.default_state()));
        place_blocks(&world, &region, blocks).await;
        let mut click = place(&player, tnt, Item::FlintAndSteel, 1);
        click.item = None;
        region.request(RegionRequest::Place(click)).await;
        expect(&mut events, tnt_track).await;
        world.ticks(20).await;
        world.shutdown().await;
    }

    let saved = stored_tnt(dir.path(), chunk).expect("the lit TNT was saved");
    let fuse = saved.get_short("fuse").unwrap();
    assert!((50..80).contains(&fuse), "fuse {fuse} burned down a bit");
    assert_eq!(saved.get_str("block_state"), Some("minecraft:tnt"));
    assert_eq!(saved.get_uuid("owner"), Some(100), "the igniter's UUID");
    assert!(saved.get_uuid("UUID").is_some());

    let (player, mut events) = PlayerHandle::new(200);
    let world = TestWorld::start(dir.path());
    let region = world.region(chunk.region()).await;
    region
        .request(RegionRequest::Subscribe {
            player: player.clone(),
            chunks: vec![chunk],
        })
        .await;
    let track = expect(&mut events, tnt_track).await;
    assert_eq!(track.chunk, chunk);
    assert!((track.position[1] - (SKY + 1) as f64).abs() < 1.0e-6);
    world.ticks(12).await;
    let later = expect(&mut events, |event| match event {
        PlayerEvent::Track(later) if later.entity == track.entity => Some(later),
        _ => None,
    })
    .await;
    assert!(later.seq > track.seq, "movement updates resume");
    assert_eq!(
        block_at(&region, floor).await.map(|s| s.block()),
        Some(Block::Stone)
    );
    world.ticks(fuse as u64).await;
    expect(&mut events, |event| match event {
        PlayerEvent::Untrack { entity, .. } if entity == track.entity => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(
        block_at(&region, floor).await.map(|s| s.block()),
        Some(Block::Air),
        "the reloaded TNT went off"
    );
    world.shutdown().await;
    assert!(stored_tnt(dir.path(), chunk).is_none());
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tnt_outlives_its_chunk_unloading_and_comes_back_with_it() {
    let dir = TestDir::new();
    let chunk = ChunkPos::new(2, 2);
    let world = TestWorld::start(dir.path());
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let position = [40.5, SKY as f64, 40.5];
    let nbt = Compound::new()
        .with("fuse", 1000i16)
        .with("NoGravity", true)
        .with("CustomName", "Slow");
    summon(&region, nbt, position).await;
    let first = expect(&mut events, tnt_track).await;
    world.ticks(5).await;

    region
        .request(RegionRequest::Unsubscribe {
            player: player.id,
            chunks: vec![chunk],
        })
        .await;
    world.ticks(150).await;
    assert!(
        snapshots(&region).await.is_empty(),
        "the chunk and its TNT were unloaded"
    );
    let saved = stored_tnt(dir.path(), chunk).expect("saved with its chunk");
    let fuse = saved.get_short("fuse").unwrap();
    assert!((850..1000).contains(&fuse), "fuse {fuse}");
    assert_eq!(saved.get_bool("NoGravity"), Some(true));
    assert_eq!(saved.get_str("CustomName"), Some("Slow"));

    subscribe(&region, &player, &mut events, &[chunk]).await;
    let again = expect(&mut events, tnt_track).await;
    assert_ne!(again.entity, first.entity, "a fresh network id");
    assert_eq!(again.position, position, "no gravity kept it in place");
    let found = snapshots(&region).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].kind, EntityType::Tnt);
    assert_eq!(found[0].name, "Slow");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tnt_crosses_region_borders_and_is_saved_by_its_new_region() {
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
        .with("fuse", 1000i16)
        .with("NoGravity", true)
        .with(
            "Motion",
            Tag::List(vec![Tag::Double(0.4), Tag::Double(0.0), Tag::Double(0.0)]),
        );
    summon(&west, nbt, [511.0, SKY as f64, 8.5]).await;
    let spawned = expect(&mut events, tnt_track).await;
    assert_eq!(spawned.chunk, west_chunk);
    world.ticks(20).await;
    let mut arrived = false;
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::Track(track) = event {
            arrived |= track.entity == spawned.entity && track.chunk == east_chunk;
        }
    }
    assert!(arrived, "viewers saw the TNT enter the eastern region");
    assert!(snapshots(&west).await.is_empty());
    let moved = snapshots(&east).await;
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].id, spawned.entity, "handed over, not respawned");
    world.shutdown().await;

    assert!(stored_tnt(dir.path(), east_chunk).is_some());
    assert!(stored_tnt(dir.path(), west_chunk).is_none());
}

async fn entity_data(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().expect("the entity exists here")
}

/// `/tp` (`RegionRequest::TeleportEntities`) on a non-player entity: within
/// a region it just relocates it, but a destination in another region hands
/// it off exactly like normal movement crossing a border.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teleport_entities_moves_a_mob_and_hands_it_off_across_a_region_border() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let dimension = world.dimension(OVERWORLD);
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    let (west_chunk, east_chunk) = (ChunkPos::new(31, 0), ChunkPos::new(32, 0));
    subscribe(&west, &viewer, &mut events, &[west_chunk]).await;
    subscribe(&east, &viewer, &mut events, &[east_chunk]).await;
    world.ticks(3).await;

    let (reply, summoned) = oneshot::channel();
    west.request(RegionRequest::Summon {
        nbt: Compound::new()
            .with("id", "minecraft:pig")
            .with("NoAI", true)
            .with("NoGravity", true),
        position: [511.5, SKY as f64, 8.5],
        fresh: false,
        reply,
    })
    .await;
    assert!(summoned.await.unwrap(), "the pig loads from NBT");
    let spawned = expect(&mut events, |event| match event {
        PlayerEvent::Track(track)
            if track.player.is_none()
                && spawned_entity_kind(&track) == EntityType::Pig.id() as i32 =>
        {
            Some(track)
        }
        _ => None,
    })
    .await;
    assert_eq!(spawned.chunk, west_chunk);
    let id = spawned.entity;

    // A same-region move just relocates it, rotation included.
    let same_region = [500.5, SKY as f64 + 4.0, 8.5];
    let ids = vec![id];
    let teleport = Teleport {
        dimension,
        position: same_region,
        relative: [false; 3],
        rotation: Some([45.0, 10.0]),
        relative_rotation: [false; 2],
        look_at: None,
    };
    let moved = west
        .ask(move |world| {
            ids.into_iter()
                .filter(|id| world.teleport_entity(*id, &teleport))
                .count()
        })
        .await
        .unwrap();
    assert_eq!(moved, 1);
    let found = snapshots(&west).await;
    let entity = found
        .iter()
        .find(|e| e.id == id)
        .expect("still owned by the western region");
    assert_eq!(entity.position, same_region);
    let saved = entity_data(&west, id).await;
    let rotation = saved.get_list("Rotation").unwrap();
    assert_eq!(rotation[0].as_float(), Some(45.0));
    assert_eq!(rotation[1].as_float(), Some(10.0));

    // A destination one region over hands it off, exactly like a normal
    // move crossing the border.
    let east_position = [513.5, SKY as f64, 8.5];
    let ids = vec![id];
    let teleport = Teleport {
        dimension,
        position: east_position,
        relative: [false; 3],
        rotation: None,
        relative_rotation: [false; 2],
        look_at: None,
    };
    let moved = west
        .ask(move |world| {
            ids.into_iter()
                .filter(|id| world.teleport_entity(*id, &teleport))
                .count()
        })
        .await
        .unwrap();
    assert_eq!(moved, 1);
    assert!(
        snapshots(&west).await.iter().all(|e| e.id != id),
        "left the western region"
    );
    let east_entities = snapshots(&east).await;
    let entity = east_entities
        .iter()
        .find(|e| e.id == id)
        .expect("handed to the eastern region");
    assert_eq!(entity.position, east_position);

    world.shutdown().await;
}

/// An id this region does not own, and a destination in a different
/// dimension (that move is `TakeEntities`), both move nothing and are
/// counted as such.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teleport_entities_ignores_unowned_ids_and_other_dimensions() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let overworld = world.dimension(OVERWORLD);
    let nether = world.dimension(THE_NETHER);
    let region = world.region(RegionPos::new(0, 0)).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(0, 0);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    world.ticks(3).await;

    let start = [8.5, SKY as f64, 8.5];
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Dirt, 1, MintReason::Test),
            position: start,
            velocity: [0.0; 3],
            pickup_delay: 10_000,
        })
        .await;
    let spawned = expect(&mut events, |event| match event {
        PlayerEvent::Track(track) => Some(track),
        _ => None,
    })
    .await;
    let id = spawned.entity;

    let teleport = |dimension| Teleport {
        dimension,
        position: [20.5, SKY as f64, 20.5],
        relative: [false; 3],
        rotation: None,
        relative_rotation: [false; 2],
        look_at: None,
    };

    let ids = vec![id + 1000];
    let there = teleport(overworld);
    let moved = region
        .ask(move |world| {
            ids.into_iter()
                .filter(|id| world.teleport_entity(*id, &there))
                .count()
        })
        .await
        .unwrap();
    assert_eq!(moved, 0, "no such entity here");

    let ids = vec![id];
    let teleport = teleport(nether);
    let moved = region
        .ask(move |world| {
            ids.into_iter()
                .filter(|id| world.teleport_entity(*id, &teleport))
                .count()
        })
        .await
        .unwrap();
    assert_eq!(moved, 0, "another dimension is not a move");
    let found = snapshots(&region).await;
    let entity = found.iter().find(|e| e.id == id).expect("still here");
    assert_eq!(entity.position, start, "left in place by the refused move");

    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn items_and_orbs_dropped_onto_cactus_are_destroyed() {
    let dir = TestDir::new();
    let (player, mut events) = PlayerHandle::new(100);
    let world = TestWorld::start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    view_around(&world, &player, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let mut blocks: Vec<_> = (0..16)
        .flat_map(|x| (0..16).map(move |z| BlockPos::new(x, SKY, z)))
        .map(|pos| (pos, Block::Stone.default_state()))
        .collect();
    for x in [2, 6, 10] {
        blocks.push((BlockPos::new(x, SKY + 1, 8), Block::Cactus.default_state()));
    }
    place_blocks(&world, &region, blocks).await;
    let item = |id: &str| {
        Compound::new()
            .with("id", "minecraft:item")
            .with("Item", Compound::new().with("id", id).with("count", 1i32))
    };
    let spawn = [
        (item("minecraft:dirt"), 2.5),
        (item("minecraft:netherite_ingot"), 6.5),
        (
            Compound::new()
                .with("id", "minecraft:experience_orb")
                .with("Value", 3i16),
            10.5,
        ),
        (item("minecraft:stone"), 14.5),
    ];
    for (nbt, x) in spawn {
        let (reply, summoned) = oneshot::channel();
        region
            .request(RegionRequest::Summon {
                nbt,
                position: [x, (SKY + 3) as f64, 8.5],
                fresh: false,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
    }
    let dropped = async || {
        let mut all = snapshots(&region).await;
        all.retain(|e| matches!(e.kind, EntityType::Item | EntityType::ExperienceOrb));
        all
    };
    assert_eq!(dropped().await.len(), 4);
    world.ticks(60).await;
    for x in [2, 6, 10] {
        assert_eq!(
            block_at(&region, BlockPos::new(x, SKY + 1, 8))
                .await
                .map(|s| s.block()),
            Some(Block::Cactus)
        );
    }
    let left = dropped().await;
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(
        left[0].position[0] > 14.0,
        "only the item on stone survives"
    );
    world.shutdown().await;
}

fn rotation(nbt: &Compound) -> [f32; 2] {
    let rotation = nbt.get_list("Rotation").unwrap();
    [
        rotation[0].as_float().unwrap(),
        rotation[1].as_float().unwrap(),
    ]
}

async fn console(world: &TestWorld, line: &str) -> Vec<TextComponent> {
    crate::world::commands::console::run(world.shared.clone(), line).await
}

fn translation(lines: &[TextComponent]) -> String {
    lines[0].to_value()["translate"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// `/tp ... facing` turns a non-player from the source's anchor (feet
/// unless `execute anchored eyes`) towards the point, and `facing entity
/// ... eyes` aims at the target's own eye height.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teleported_entities_face_from_the_source_anchor() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[ChunkPos::new(0, 0)]).await;
    world.ticks(3).await;
    for (kind, x) in [("minecraft:pig", 8.5), ("minecraft:cow", 12.5)] {
        let (reply, summoned) = oneshot::channel();
        region
            .request(RegionRequest::Summon {
                nbt: Compound::new()
                    .with("id", kind)
                    .with("NoAI", true)
                    .with("NoGravity", true),
                position: [x, SKY as f64, 8.5],
                fresh: false,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
    }
    let pig = snapshots(&region)
        .await
        .into_iter()
        .find(|e| e.kind == EntityType::Pig)
        .unwrap()
        .id;
    let pig_eye = f64::from(EntityType::Pig.size().eye_height);
    let cow_eye = f64::from(EntityType::Cow.size().eye_height);
    let level_with_eyes = SKY as f64 + pig_eye;

    let feet = console(
        &world,
        &format!("tp @e[type=pig] 8.5 {SKY} 8.5 facing 18.5 {level_with_eyes} 8.5"),
    )
    .await;
    assert_eq!(
        translation(&feet),
        "commands.teleport.success.location.single"
    );
    let [yaw, pitch] = rotation(&entity_data(&region, pig).await);
    assert_eq!(yaw, -90.0);
    assert!(pitch < -1.0, "from its feet it looks up: {pitch}");

    console(
        &world,
        &format!(
            "execute anchored eyes run tp @e[type=pig] 8.5 {SKY} 8.5 facing 18.5 {level_with_eyes} 8.5"
        ),
    )
    .await;
    let [yaw, pitch] = rotation(&entity_data(&region, pig).await);
    assert_eq!(yaw, -90.0);
    assert!(pitch.abs() < 1e-3, "from its eyes it looks level: {pitch}");

    console(
        &world,
        &format!(
            "execute anchored eyes run tp @e[type=pig] 8.5 {SKY} 8.5 facing entity @e[type=cow,limit=1] eyes"
        ),
    )
    .await;
    let [_, pitch] = rotation(&entity_data(&region, pig).await);
    let expected = -((cow_eye - pig_eye).atan2(4.0).to_degrees()) as f32;
    assert!(
        (pitch - expected).abs() < 1e-3,
        "aims at the cow's eyes: {pitch} vs {expected}"
    );
    world.shutdown().await;
}

/// `/tp` into another dimension takes a non-player out of its region and
/// summons it, same UUID, at the destination.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teleport_moves_entities_into_another_dimension() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(0, 0)).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[ChunkPos::new(0, 0)]).await;
    world.ticks(3).await;
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new()
                .with("id", "minecraft:pig")
                .with("NoAI", true)
                .with("NoGravity", true),
            position: [8.5, SKY as f64, 8.5],
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    let pig = snapshots(&region)
        .await
        .into_iter()
        .find(|e| e.kind == EntityType::Pig)
        .unwrap();

    let moved = console(
        &world,
        "execute in minecraft:the_nether run tp @e[type=pig] 40.5 100 40.5 90 0",
    )
    .await;
    assert_eq!(
        translation(&moved),
        "commands.teleport.success.location.single"
    );
    assert!(
        snapshots(&region).await.iter().all(|e| e.uuid != pig.uuid),
        "it left the overworld"
    );
    let nether = world
        .region_in(THE_NETHER, ChunkPos::new(2, 2).region())
        .await;
    world.ticks(2).await;
    let arrived = snapshots(&nether)
        .await
        .into_iter()
        .find(|e| e.uuid == pig.uuid)
        .expect("the pig arrived in the nether");
    assert_eq!(arrived.kind, EntityType::Pig);
    assert_eq!(arrived.position, [40.5, 100.0, 40.5]);
    assert_eq!(
        rotation(&entity_data(&nether, arrived.id).await),
        [90.0, 0.0]
    );
    world.shutdown().await;
}

/// A destination in a region this one has no link to is refused: the mob
/// stays where it was and is not counted as moved.
#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn teleport_entities_refuses_an_unlinked_destination_and_keeps_the_mob() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let dimension = world.dimension(OVERWORLD);
    let west = world.region(ChunkPos::new(0, 0).region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&west, &viewer, &mut events, &[ChunkPos::new(0, 0)]).await;
    world.ticks(3).await;
    let (reply, summoned) = oneshot::channel();
    west.request(RegionRequest::Summon {
        nbt: Compound::new()
            .with("id", "minecraft:pig")
            .with("NoAI", true)
            .with("NoGravity", true),
        position: [8.5, SKY as f64, 8.5],
        fresh: false,
        reply,
    })
    .await;
    assert!(summoned.await.unwrap());
    let id = snapshots(&west).await[0].id;
    let teleport = Teleport {
        dimension,
        position: [5000.5, SKY as f64, 5000.5],
        relative: [false; 3],
        rotation: None,
        relative_rotation: [false; 2],
        look_at: None,
    };
    let moved = west
        .ask(move |world| world.teleport_entity(id, &teleport))
        .await
        .unwrap();
    assert!(!moved);
    let found = snapshots(&west).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].position, [8.5, SKY as f64, 8.5]);
    world.shutdown().await;
}
