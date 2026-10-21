//! A session keeps a read-only mirror of the chunks its client was sent,
//! patched by the block changes delivered to it.

use super::*;
use crate::world::session::PlayerSession;
use tuff_vanilla_data::Block;

struct Viewer {
    session: PlayerSession,
    handle: PlayerHandle,
    events: mpsc::UnboundedReceiver<PlayerEvent>,
    _frames: mpsc::UnboundedReceiver<bytes::Bytes>,
}

fn viewer(world: &TestWorld) -> Viewer {
    let (session, frames) = session_with_outbound(world, 1000);
    let (handle, events) = PlayerHandle::new(1000);
    Viewer {
        session,
        handle,
        events,
        _frames: frames,
    }
}

impl Viewer {
    async fn subscribe(&mut self, region: &RegionHandle, chunks: &[ChunkPos]) {
        self.session.view.subscribed.extend(chunks.iter().copied());
        region
            .request(RegionRequest::Subscribe {
                player: self.handle.clone(),
                chunks: chunks.to_vec(),
            })
            .await;
        let mut waiting = chunks.len();
        while waiting > 0 {
            let event = self.events.recv().await.expect("region events");
            if matches!(event, PlayerEvent::Chunk { .. }) {
                waiting -= 1;
            }
            self.session.on_event(event).await;
        }
    }

    async fn pump(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.session.on_event(event).await;
        }
    }
}

fn around(center: ChunkPos) -> Vec<ChunkPos> {
    (-1..=1)
        .flat_map(|dx| (-1..=1).map(move |dz| ChunkPos::new(center.x + dx, center.z + dz)))
        .collect()
}

fn drain(session: &mut PlayerSession) {
    while !session.view.queue.is_empty() {
        session.view.unacked_batches = 0;
        session.send_chunk_batch();
    }
}

fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, y, z)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_mirror_follows_chunks_and_block_changes_that_were_delivered() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(1, 1)).await;
    let base = ChunkPos::new(40, 40);
    let near = base;
    let mut v = viewer(&world);
    let at = pos(base.x * 16 + 3, 100, base.z * 16 + 4);

    assert_eq!(v.session.client_world().block_at(at), None);
    assert!(!v.session.client_world().in_loaded_chunk(at));

    v.subscribe(&region, &around(near)).await;
    assert!(v.session.client_world().is_empty());
    drain(&mut v.session);
    assert_eq!(v.session.client_world().len(), 9);
    assert!(v.session.client_world().in_loaded_chunk(at));
    let truth = block_at(&region, at).await;
    assert_eq!(v.session.client_world().block_at(at), truth);
    assert_eq!(
        v.session.client_world().block_at(pos(at.x, 5000, at.z)),
        None
    );

    place_blocks(&world, &region, [(at, Block::Stone.default_state())]).await;
    v.pump().await;
    assert_eq!(
        v.session.client_world().block_at(at),
        Some(Block::Stone.default_state())
    );
    let area = [
        at.x as f64,
        at.y as f64,
        at.z as f64,
        at.x as f64 + 1.0,
        at.y as f64 + 1.0,
        at.z as f64 + 1.0,
    ];
    assert_eq!(v.session.client_world().colliders(area).len(), 1);

    let many: Vec<_> = (0..5)
        .map(|i| {
            (
                pos(at.x + i % 2, at.y + 1 + i / 2, at.z),
                Block::Dirt.default_state(),
            )
        })
        .collect();
    place_blocks(&world, &region, many.clone()).await;
    v.pump().await;
    for (p, state) in many {
        assert_eq!(v.session.client_world().block_at(p), Some(state));
    }

    v.session.detach_from_regions().await;
    assert!(v.session.client_world().is_empty());
    assert_eq!(v.session.client_world().block_at(at), None);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changes_in_chunks_the_client_has_not_been_sent_are_ignored_and_queued_ones_are_patched() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(1, 1)).await;
    let base = ChunkPos::new(48, 48);
    let mut v = viewer(&world);
    let at = pos(base.x * 16 + 1, 100, base.z * 16 + 1);

    v.subscribe(&region, &around(base)).await;
    place_blocks(&world, &region, [(at, Block::Stone.default_state())]).await;
    v.pump().await;
    assert_eq!(v.session.client_world().block_at(at), None);
    drain(&mut v.session);
    assert_eq!(
        v.session.client_world().block_at(at),
        Some(Block::Stone.default_state())
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forgetting_a_chunk_drops_it_from_the_mirror() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(1, 1)).await;
    let base = ChunkPos::new(56, 56);
    let mut v = viewer(&world);
    v.subscribe(&region, &around(base)).await;
    drain(&mut v.session);
    assert_eq!(v.session.client_world().len(), 9);
    v.session.pose.position = [0.0, 100.0, 0.0];
    v.session.update_view().await;
    assert!(v.session.client_world().chunk(base).is_none());
    assert!(v.session.client_world().len() <= 25);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_client_is_sent_a_chunk_only_once_its_quota_accumulates() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(1, 1)).await;
    let mut v = viewer(&world);
    v.subscribe(&region, &around(ChunkPos::new(40, 40))).await;
    let queued = v.session.view.queue.len();
    assert_eq!(queued, 9);

    v.session.view.chunks_per_tick = 0.5;
    v.session.view.batch_quota = 0.0;
    v.session.send_chunk_batch();
    assert_eq!(v.session.view.queue.len(), queued);
    v.session.send_chunk_batch();
    assert_eq!(v.session.view.queue.len(), queued - 1);
    assert!(v.session.view.batch_quota < 1.0);

    v.session.view.chunks_per_tick = 4.0;
    v.session.view.batch_quota = 0.0;
    v.session.view.unacked_batches = 0;
    v.session.send_chunk_batch();
    assert_eq!(v.session.view.queue.len(), queued - 5);
}
