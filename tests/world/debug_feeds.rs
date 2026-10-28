//! World debug feeds: POIs, village sections, neighbor updates and game
//! events reach subscribed chunk viewers only.

use super::*;
use crate::world::debug_feeds::{DebugSubscribers, bit};
use crate::world::messages::{BlockOp, BlockOpResult, PlayerLooks};
use tuff_vanilla_data::packet::play::Clientbound as C;
use tuff_vanilla_data::registry::DebugSubscription as Feed;

const Y: i32 = 100;

async fn set_blocks(
    region: &RegionHandle,
    changes: Vec<(BlockPos, tuff_vanilla_data::BlockState)>,
) {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Set {
                changes,
                destroy: false,
                keep: false,
                strict: false,
                quiet: false,
                data: None,
            },
            reply,
        })
        .await;
    assert!(matches!(answer.await.unwrap(), BlockOpResult::Set(_)));
}

#[derive(Debug, PartialEq)]
enum Seen {
    Block {
        pos: BlockPos,
        feed: i32,
        present: bool,
    },
    Event {
        feed: i32,
        pos: Option<BlockPos>,
    },
}

fn decode(frame: &bytes::Bytes) -> Option<Seen> {
    use tuff_core::protocol::codec::ReadExt;
    let mut r = framed_payload(frame);
    let id = r.read_var_int().ok()?;
    if id == C::DebugBlockValue.id() {
        let pos = r.read_position().ok()?;
        let feed = r.read_var_int().ok()?;
        let present = r.read_bool().ok()?;
        Some(Seen::Block { pos, feed, present })
    } else if id == C::DebugEvent.id() {
        let feed = r.read_var_int().ok()?;
        let pos = (feed == Feed::NeighborUpdates as i32)
            .then(|| r.read_position().ok())
            .flatten();
        Some(Seen::Event { feed, pos })
    } else {
        None
    }
}

fn drain(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> Vec<Seen> {
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::ChunkPackets { packets, .. } = event {
            seen.extend(packets.iter().filter_map(decode));
        }
    }
    seen
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn subscribed_viewers_get_world_feeds_and_others_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (watcher, mut watched) = PlayerHandle::new(1);
    let (other, mut unwatched) = PlayerHandle::new(2);
    view_around(&world, &watcher, &mut watched, ChunkPos::new(0, 0)).await;
    view_around(&world, &other, &mut unwatched, ChunkPos::new(0, 0)).await;
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    world.ticks(2).await;
    drain(&mut watched);
    drain(&mut unwatched);

    let feeds = bit(Feed::Pois) | bit(Feed::NeighborUpdates) | bit(Feed::GameEvents);
    world
        .debug_subscribers
        .send_replace(std::sync::Arc::new(DebugSubscribers::new(vec![(
            watcher.id, feeds,
        )])));
    let bell = BlockPos::new(8, Y, 8);
    set_blocks(&region, vec![(bell, Block::Bell.default_state())]).await;
    world.ticks(3).await;
    let seen = drain(&mut watched);
    assert!(
        seen.contains(&Seen::Block {
            pos: bell,
            feed: Feed::Pois as i32,
            present: true
        }),
        "{seen:?}"
    );
    assert!(drain(&mut unwatched).is_empty());

    set_blocks(&region, vec![(bell, Block::Air.default_state())]).await;
    world.ticks(3).await;
    let seen = drain(&mut watched);
    assert!(
        seen.contains(&Seen::Block {
            pos: bell,
            feed: Feed::Pois as i32,
            present: false
        }),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .any(|s| matches!(s, Seen::Event { feed, pos: Some(_) } if *feed == Feed::NeighborUpdates as i32)),
        "{seen:?}"
    );

    world
        .debug_subscribers
        .send_replace(std::sync::Arc::new(DebugSubscribers::default()));
    world.ticks(1).await;
    drain(&mut watched);
    set_blocks(&region, vec![(bell, Block::Bell.default_state())]).await;
    world.ticks(3).await;
    assert!(drain(&mut watched).is_empty());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_subscriber_gets_existing_points_and_no_village_without_claims() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (watcher, mut watched) = PlayerHandle::new(1);
    view_around(&world, &watcher, &mut watched, ChunkPos::new(0, 0)).await;
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let bell = BlockPos::new(8, Y, 8);
    set_blocks(&region, vec![(bell, Block::Bell.default_state())]).await;
    world.ticks(2).await;
    drain(&mut watched);

    let feeds = bit(Feed::Pois) | bit(Feed::VillageSections);
    world
        .debug_subscribers
        .send_replace(std::sync::Arc::new(DebugSubscribers::new(vec![(
            watcher.id, feeds,
        )])));
    world.ticks(2).await;
    let seen = drain(&mut watched);
    assert!(
        seen.contains(&Seen::Block {
            pos: bell,
            feed: Feed::Pois as i32,
            present: true
        }),
        "{seen:?}"
    );
    assert!(
        !seen.iter().any(
            |s| matches!(s, Seen::Block { feed, .. } if *feed == Feed::VillageSections as i32)
        ),
        "an unclaimed bell makes no village: {seen:?}"
    );
    world.ticks(2).await;
    assert!(
        drain(&mut watched).is_empty(),
        "unchanged values are not resent"
    );
    world.shutdown().await;
}

fn subscribe_feeds(world: &TestWorld, player: &PlayerHandle, feeds: u32) {
    world
        .debug_subscribers
        .send_replace(std::sync::Arc::new(DebugSubscribers::new(vec![(
            player.id, feeds,
        )])));
}

/// Rewrites the stored chunk at `pos` with `edit`, as another program
/// saving it would.
fn rewrite_stored(dir: &Path, pos: ChunkPos, edit: impl FnOnce(&mut tuff_nbt::Compound)) {
    use tuff_nbt::NamedTag;
    use tuff_storage::region_file::{RawChunk, RegionFile};
    use tuff_storage::vanilla::VanillaBackend;
    let mut root = stored_chunk(dir, pos, false).expect("chunk saved");
    edit(&mut root);
    let mut bytes = Vec::new();
    NamedTag::new("", root).write(&mut bytes).unwrap();
    let folder = VanillaBackend::new(dir)
        .paths()
        .region_dir(crate::world::dimension::OVERWORLD);
    let region = pos.file_region();
    let mut file = RegionFile::open(&folder, region.x, region.z, false)
        .unwrap()
        .unwrap();
    file.write(pos, &RawChunk::compress(bytes).unwrap())
        .unwrap();
}

/// The boxes of each structure in a Debug Chunk Value for structures.
fn structure_boxes(frame: &bytes::Bytes) -> Option<(i64, Vec<[i32; 6]>)> {
    use tuff_core::protocol::codec::ReadExt;
    let mut r = framed_payload(frame);
    if r.read_var_int().ok()? != C::DebugChunkValue.id() {
        return None;
    }
    let chunk = r.read_long().ok()?;
    if r.read_var_int().ok()? != Feed::Structures as i32 || !r.read_bool().ok()? {
        return None;
    }
    let read_box = |r: &mut _| -> Option<[i32; 6]> {
        let min: BlockPos = ReadExt::read_position(r).ok()?;
        let max: BlockPos = ReadExt::read_position(r).ok()?;
        Some([min.x, min.y, min.z, max.x, max.y, max.z])
    };
    let mut boxes = Vec::new();
    for _ in 0..r.read_var_int().ok()? {
        boxes.push(read_box(&mut r)?);
        for _ in 0..r.read_var_int().ok()? {
            boxes.push(read_box(&mut r)?);
            r.read_bool().ok()?;
        }
    }
    Some((chunk, boxes))
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn structure_starts_reach_subscribers_as_chunk_values() {
    let dir = TestDir::new();
    let pos = ChunkPos::new(0, 0);
    {
        let world = TestWorld::start(dir.path());
        let (viewer, mut events) = PlayerHandle::new(1);
        view_around(&world, &viewer, &mut events, pos).await;
        world.ticks(2).await;
        world.shutdown().await;
    }
    rewrite_stored(dir.path(), pos, |root| {
        let piece = |bb: Vec<i32>| tuff_nbt::Tag::Compound(Compound::new().with("BB", bb));
        let start = Compound::new()
            .with("id", "minecraft:village_plains")
            .with("ChunkX", 0)
            .with("ChunkZ", 0)
            .with("references", 0)
            .with(
                "Children",
                tuff_nbt::Tag::List(vec![
                    piece(vec![0, 60, 0, 9, 70, 9]),
                    piece(vec![-5, 62, 4, 3, 66, 20]),
                ]),
            );
        root.insert(
            "structures",
            Compound::new().with("References", Compound::new()).with(
                "starts",
                Compound::new().with("minecraft:village_plains", start),
            ),
        );
    });
    let world = TestWorld::start(dir.path());
    let (watcher, mut watched) = PlayerHandle::new(1);
    let (other, mut unwatched) = PlayerHandle::new(2);
    subscribe_feeds(&world, &watcher, bit(Feed::Structures));
    view_around(&world, &watcher, &mut watched, pos).await;
    view_around(&world, &other, &mut unwatched, pos).await;
    world.ticks(3).await;
    let mut found = Vec::new();
    while let Ok(event) = watched.try_recv() {
        if let PlayerEvent::ChunkPackets { packets, .. } = event {
            found.extend(packets.iter().filter_map(structure_boxes));
        }
    }
    assert_eq!(
        found,
        vec![(
            0,
            vec![
                [-17, 48, -12, 21, 82, 32],
                [0, 60, 0, 9, 70, 9],
                [-5, 62, 4, 3, 66, 20],
            ]
        )],
        "one start, its box grown for terrain adaptation, then its pieces"
    );
    while let Ok(event) = unwatched.try_recv() {
        if let PlayerEvent::ChunkPackets { packets, .. } = event {
            assert!(packets.iter().all(|p| structure_boxes(p).is_none()));
        }
    }
    world.ticks(2).await;
    while let Ok(event) = watched.try_recv() {
        if let PlayerEvent::ChunkPackets { packets, .. } = event {
            assert!(
                packets.iter().all(|p| structure_boxes(p).is_none()),
                "unchanged starts are not resent"
            );
        }
    }
    world.shutdown().await;
}

/// The feed and whether a value is present in a Debug Entity Value.
fn entity_value(packet: &bytes::Bytes) -> Option<(i32, i32, bool)> {
    use tuff_core::protocol::codec::ReadExt;
    let mut r = framed_payload(packet);
    if r.read_var_int().ok()? != C::DebugEntityValue.id() {
        return None;
    }
    let entity = r.read_var_int().ok()?;
    let feed = r.read_var_int().ok()?;
    Some((entity, feed, r.read_bool().ok()?))
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_walking_mob_reports_its_goals_and_paths_to_subscribers() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (watcher, mut watched) = PlayerHandle::new(1);
    let (other, mut unwatched) = PlayerHandle::new(2);
    let pos = ChunkPos::new(0, 0);
    view_around(&world, &watcher, &mut watched, pos).await;
    view_around(&world, &other, &mut unwatched, pos).await;
    let region = world.region(pos.region()).await;
    let mut floor = Vec::new();
    for x in 0..16 {
        for z in 0..16 {
            floor.push((BlockPos::new(x, Y - 1, z), Block::Stone.default_state()));
        }
    }
    set_blocks(&region, floor).await;
    region
        .request(player_at(
            &watcher,
            [12.5, f64::from(Y), 12.5],
            PlayerLooks::default(),
        ))
        .await;
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt: Compound::new().with("id", "minecraft:zombie"),
            position: [3.5, f64::from(Y), 3.5],
            fresh: true,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    world.ticks(2).await;
    subscribe_feeds(
        &world,
        &watcher,
        bit(Feed::EntityPaths) | bit(Feed::GoalSelectors),
    );
    let mut goals = false;
    let mut path = None;
    for _ in 0..60 {
        world.ticks(20).await;
        while let Ok(event) = watched.try_recv() {
            if let PlayerEvent::DebugEntityValue { packet, feed, .. } = event {
                let (entity, wire_feed, present) = entity_value(&packet).expect("decodes");
                assert_eq!(i32::from(feed), wire_feed);
                if wire_feed == Feed::GoalSelectors as i32 {
                    goals = true;
                }
                if wire_feed == Feed::EntityPaths as i32 && present {
                    path = Some((entity, packet));
                }
            }
        }
        if goals && path.is_some() {
            break;
        }
    }
    assert!(goals, "a mob's goals are reported");
    let (_, packet) = path.expect("a stroll reports its path");
    let mut r = framed_payload(&packet);
    let payload = r.read_bytes(r.remaining()).unwrap();
    let text = tuff_core::trace_decode::typed_text(tuff_trace::ConnState::Play, &payload)
        .expect("the path decodes cleanly");
    assert!(text.contains("entity_paths"), "{text}");
    while let Ok(event) = unwatched.try_recv() {
        assert!(!matches!(event, PlayerEvent::DebugEntityValue { .. }));
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn brain_mobs_report_brains_and_goal_mobs_do_not() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (watcher, mut watched) = PlayerHandle::new(1);
    let pos = ChunkPos::new(0, 0);
    view_around(&world, &watcher, &mut watched, pos).await;
    let region = world.region(pos.region()).await;
    let mut floor = Vec::new();
    for x in 0..16 {
        for z in 0..16 {
            floor.push((BlockPos::new(x, Y - 1, z), Block::Stone.default_state()));
        }
    }
    set_blocks(&region, floor).await;
    region
        .request(player_at(
            &watcher,
            [12.5, f64::from(Y), 12.5],
            PlayerLooks::default(),
        ))
        .await;
    for (id, x) in [("minecraft:goat", 3.5), ("minecraft:zombie", 8.5)] {
        let (reply, summoned) = oneshot::channel();
        region
            .request(RegionRequest::Summon {
                nbt: Compound::new().with("id", id).with("NoAI", true),
                position: [x, f64::from(Y), 3.5],
                fresh: true,
                reply,
            })
            .await;
        assert!(summoned.await.unwrap());
    }
    world.ticks(2).await;
    subscribe_feeds(&world, &watcher, bit(Feed::Brains));
    let mut brains = Vec::new();
    for _ in 0..10 {
        world.ticks(5).await;
        while let Ok(event) = watched.try_recv() {
            if let PlayerEvent::DebugEntityValue { packet, .. } = event {
                let (entity, feed, present) = entity_value(&packet).expect("decodes");
                if feed == Feed::Brains as i32 && present {
                    let mut r = framed_payload(&packet);
                    let payload = r.read_bytes(r.remaining()).unwrap();
                    let text =
                        tuff_core::trace_decode::typed_text(tuff_trace::ConnState::Play, &payload)
                            .expect("the brain decodes cleanly");
                    brains.push((entity, text));
                }
            }
        }
        if !brains.is_empty() {
            break;
        }
    }
    let goat = brains[0].0;
    assert!(
        brains.iter().all(|(entity, _)| *entity == goat),
        "only the goat has a brain: {brains:?}"
    );
    assert!(brains[0].1.contains("idle"), "{}", brains[0].1);
    assert!(brains[0].1.contains("attack_target: -"), "{}", brains[0].1);
    assert!(
        brains[0].1.contains("ram_cooldown_ticks"),
        "{}",
        brains[0].1
    );
    world.shutdown().await;
}
