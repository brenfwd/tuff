//! Anti-xray as clients see it: ores nobody can see reach them as stone in
//! chunk packets and the session's mirror, and the true blocks follow when
//! something opens them up or a player starts mining next to them.

use std::collections::HashMap;

use super::*;
use crate::config::Value;
use crate::world::anti_xray;
use crate::world::session::PlayerSession;
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::chunk::Chunk;

struct Viewer {
    session: PlayerSession,
    handle: PlayerHandle,
    events: mpsc::UnboundedReceiver<PlayerEvent>,
    _frames: mpsc::UnboundedReceiver<bytes::Bytes>,
    sent: HashMap<ChunkPos, (bytes::Bytes, Option<Arc<Chunk>>)>,
}

fn viewer(world: &TestWorld, id: i32) -> Viewer {
    let (session, frames) = session_with_outbound(world, id);
    let (handle, events) = PlayerHandle::new(id);
    Viewer {
        session,
        handle,
        events,
        _frames: frames,
        sent: HashMap::new(),
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
            if let PlayerEvent::Chunk {
                pos,
                packet,
                snapshot,
                ..
            } = &event
            {
                self.sent.insert(*pos, (packet.clone(), snapshot.clone()));
                waiting -= 1;
            }
            self.session.on_event(event).await;
        }
        while !self.session.view.queue.is_empty() {
            self.session.view.unacked_batches = 0;
            self.session.send_chunk_batch();
        }
    }

    async fn pump(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.session.on_event(event).await;
        }
    }

    fn sees(&self, at: BlockPos) -> Option<BlockState> {
        self.session.client_world().block_at(at)
    }

    /// The chunk packet holds exactly the blocks the mirror was given, and
    /// no block entities.
    fn packet_matches_mirror(&self, pos: ChunkPos) {
        let (packet, snapshot) = &self.sent[&pos];
        let snapshot = snapshot.as_ref().expect("a snapshot beside the packet");
        let expected = framed(&protocol::play::LevelChunkWithLight(snapshot.encode(
            pos.x,
            pos.z,
            &[],
        )));
        assert_eq!(packet, &expected);
    }
}

fn around(center: ChunkPos) -> Vec<ChunkPos> {
    (-1..=1)
        .flat_map(|dx| (-1..=1).map(move |dz| ChunkPos::new(center.x + dx, center.z + dz)))
        .collect()
}

fn enable(world: &TestWorld, on: bool) {
    world
        .shared
        .config
        .set(anti_xray::ENABLED, Value::Bool(on))
        .unwrap();
}

fn stone() -> BlockState {
    Block::Stone.default_state()
}

fn diamond() -> BlockState {
    Block::DiamondOre.default_state()
}

fn offset(at: BlockPos, dx: i32, dy: i32, dz: i32) -> BlockPos {
    BlockPos::new(at.x + dx, at.y + dy, at.z + dz)
}

/// Four ores along x one above `at`'s level and one at it, deep in stone:
/// `at` and the three at `at + (1..=3, 1, 0)`.
fn ores(at: BlockPos) -> Vec<(BlockPos, BlockState)> {
    let mut ores = vec![(at, diamond())];
    for dx in 1..=3 {
        ores.push((offset(at, dx, 1, 0), Block::GoldOre.default_state()));
    }
    ores
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enclosed_ores_are_sent_as_stone_until_a_neighbour_opens() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    enable(&world, true);
    let region = world.region(RegionPos::new(1, 1)).await;
    let center = ChunkPos::new(40, 40);
    let at = BlockPos::new(center.x * 16 + 6, 20, center.z * 16 + 6);
    let mut loader = viewer(&world, 1000);
    loader.subscribe(&region, &around(center)).await;
    let exposed = BlockPos::new(at.x, 20, at.z + 4);
    let mut blocks = ores(at);
    blocks.push((exposed, diamond()));
    blocks.push((offset(exposed, 0, 1, 0), Block::Air.default_state()));
    place_blocks(&world, &region, blocks).await;

    let mut v = viewer(&world, 1001);
    v.subscribe(&region, &around(center)).await;
    v.packet_matches_mirror(center);
    for (pos, _) in ores(at) {
        assert_eq!(v.sees(pos), Some(stone()), "{pos:?}");
    }
    assert_eq!(v.sees(exposed), Some(diamond()), "an ore by air shows");
    assert_eq!(block_at(&region, at).await, Some(diamond()));

    let opened = offset(at, 0, 1, 0);
    place_blocks(&world, &region, [(opened, Block::Air.default_state())]).await;
    v.pump().await;
    assert_eq!(v.sees(at), Some(diamond()));
    assert_eq!(
        v.sees(offset(at, 1, 1, 0)),
        Some(Block::GoldOre.default_state())
    );
    assert_eq!(
        v.sees(offset(at, 2, 1, 0)),
        Some(Block::GoldOre.default_state()),
        "within the update radius"
    );
    assert_eq!(v.sees(offset(at, 3, 1, 0)), Some(stone()), "beyond it");

    let mut late = viewer(&world, 1002);
    late.subscribe(&region, &around(center)).await;
    late.packet_matches_mirror(center);
    assert_eq!(late.sees(at), Some(diamond()), "exposed now");
    assert_eq!(late.sees(offset(at, 2, 1, 0)), Some(stone()));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn starting_to_mine_reveals_what_is_behind_the_block() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    enable(&world, true);
    let region = world.region(RegionPos::new(1, 1)).await;
    let center = ChunkPos::new(44, 44);
    let at = BlockPos::new(center.x * 16 + 8, 25, center.z * 16 + 8);
    let mut loader = viewer(&world, 1000);
    loader.subscribe(&region, &around(center)).await;
    place_blocks(&world, &region, ores(at)).await;
    let mut v = viewer(&world, 1001);
    v.subscribe(&region, &around(center)).await;
    assert_eq!(v.sees(at), Some(stone()));

    let (miner, mut answers) = PlayerHandle::new(2000);
    let face = offset(at, -1, 0, 0);
    region
        .request(RegionRequest::Break {
            player: miner,
            pos: face,
            tool: None,
            mode: BreakMode::Start {
                eye: [
                    face.x as f64 - 2.0,
                    face.y as f64 + 0.5,
                    face.z as f64 + 0.5,
                ],
                miner: player::Miner::plain(true),
            },
            sequence: 1,
        })
        .await;
    expect(&mut answers, |event| match event {
        PlayerEvent::ActionResult { outcome, .. } => Some(outcome),
        _ => None,
    })
    .await;
    world.ticks(1).await;
    v.pump().await;
    assert_eq!(v.sees(at), Some(diamond()));
    assert_eq!(v.sees(face), Some(stone()));
    assert_eq!(v.sees(offset(at, 3, 1, 0)), Some(stone()));
    assert_eq!(
        block_at(&region, face).await,
        Some(stone()),
        "still unbroken"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_a_block_across_a_region_border_reveals_the_ore_behind_it() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    enable(&world, true);
    let west = world.region(RegionPos::new(0, 1)).await;
    let east = world.region(RegionPos::new(1, 1)).await;
    let center = ChunkPos::new(32, 40);
    let (west_chunks, east_chunks): (Vec<_>, Vec<_>) =
        around(center).into_iter().partition(|c| c.x < 32);
    let mut loader = viewer(&world, 1000);
    loader.subscribe(&west, &west_chunks).await;
    loader.subscribe(&east, &east_chunks).await;
    let at = BlockPos::new(center.x * 16, 20, center.z * 16 + 5);
    place_blocks(&world, &east, [(at, diamond())]).await;
    world.ticks(2).await;

    let mut v = viewer(&world, 1001);
    v.subscribe(&west, &west_chunks).await;
    v.subscribe(&east, &east_chunks).await;
    assert_eq!(v.sees(at), Some(stone()), "hidden by the block across");

    place_blocks(
        &world,
        &west,
        [(offset(at, -1, 0, 0), Block::Air.default_state())],
    )
    .await;
    for _ in 0..10 {
        world.ticks(1).await;
        v.pump().await;
        if v.sees(at) == Some(diamond()) {
            break;
        }
    }
    assert_eq!(v.sees(at), Some(diamond()));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn settings_apply_live_to_later_sends() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(1, 1)).await;
    let center = ChunkPos::new(48, 40);
    let at = BlockPos::new(center.x * 16 + 7, 15, center.z * 16 + 7);
    let mut loader = viewer(&world, 1000);
    loader.subscribe(&region, &around(center)).await;
    place_blocks(&world, &region, [(at, diamond())]).await;
    let mut off = viewer(&world, 1001);
    off.subscribe(&region, &around(center)).await;
    assert_eq!(off.sees(at), Some(diamond()), "off by default");

    enable(&world, true);
    world.ticks(1).await;
    let mut on = viewer(&world, 1002);
    on.subscribe(&region, &around(center)).await;
    assert_eq!(on.sees(at), Some(stone()));

    world
        .shared
        .config
        .set_text(anti_xray::MODE, "fill")
        .unwrap();
    world.ticks(1).await;
    let mut fill = viewer(&world, 1003);
    fill.subscribe(&region, &around(center)).await;
    fill.packet_matches_mirror(center);
    let stand_in = offset(at, 0, 1, 1);
    assert_eq!(block_at(&region, stand_in).await, Some(stone()));
    let shown = (0..16)
        .map(|dz| fill.sees(offset(at, 0, 1, dz - 7)).unwrap())
        .filter(|state| *state != stone())
        .count();
    assert!(shown > 8, "{shown} of 16 enclosed stones shown as ores");

    world
        .shared
        .config
        .set_text(&anti_xray::hidden_key("overworld"), "")
        .unwrap();
    world.ticks(1).await;
    let mut emptied = viewer(&world, 1004);
    emptied.subscribe(&region, &around(center)).await;
    assert_eq!(emptied.sees(at), Some(diamond()), "nothing listed");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn buried_chests_are_hidden_with_their_block_entity() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    enable(&world, true);
    let region = world.region(RegionPos::new(1, 1)).await;
    let center = ChunkPos::new(52, 52);
    let at = BlockPos::new(center.x * 16 + 4, 30, center.z * 16 + 9);
    let chest = Block::Chest.default_state();
    let mut loader = viewer(&world, 1000);
    loader.subscribe(&region, &around(center)).await;
    place_blocks(&world, &region, [(at, chest)]).await;
    let mut v = viewer(&world, 1001);
    v.subscribe(&region, &around(center)).await;
    assert_eq!(v.sees(at), Some(stone()));
    v.packet_matches_mirror(center);

    place_blocks(
        &world,
        &region,
        [(offset(at, 0, 0, 1), Block::Air.default_state())],
    )
    .await;
    v.pump().await;
    assert_eq!(v.sees(at), Some(chest));
    world.shutdown().await;
}

impl Viewer {
    /// Waits for `count` chunk packets, hands them to the session and
    /// returns how many are queued behind the send budget.
    async fn receive_chunks(&mut self, count: usize) -> usize {
        let mut waiting = count;
        while waiting > 0 {
            let event = self.events.recv().await.expect("region events");
            if let PlayerEvent::Chunk {
                pos,
                packet,
                snapshot,
                ..
            } = &event
            {
                self.sent.insert(*pos, (packet.clone(), snapshot.clone()));
                waiting -= 1;
            }
            self.session.on_event(event).await;
        }
        self.session.view.queue.len()
    }

    fn drain_queue(&mut self) {
        while !self.session.view.queue.is_empty() {
            self.session.view.unacked_batches = 0;
            self.session.send_chunk_batch();
        }
    }

    fn chunks_received(&mut self) -> usize {
        let mut count = 0;
        while let Ok(event) = self.events.try_recv() {
            if matches!(event, PlayerEvent::Chunk { .. }) {
                count += 1;
            }
        }
        count
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changed_settings_resend_every_chunk_in_view_within_the_budget() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let region = world.region(RegionPos::new(1, 1)).await;
    let center = ChunkPos::new(56, 40);
    let at = BlockPos::new(center.x * 16 + 7, 15, center.z * 16 + 7);
    let mut loader = viewer(&world, 1000);
    loader.subscribe(&region, &around(center)).await;
    place_blocks(&world, &region, [(at, diamond())]).await;
    let mut v = viewer(&world, 1001);
    v.subscribe(&region, &around(center)).await;
    assert_eq!(v.sees(at), Some(diamond()));
    assert_eq!(v.session.view.sent.len(), 9);

    enable(&world, true);
    world.ticks(1).await;
    assert_eq!(v.receive_chunks(9).await, 9, "resends wait for the budget");
    assert_eq!(v.sees(at), Some(diamond()), "not yet resent");
    v.session.view.chunks_per_tick = 2.0;
    v.session.view.unacked_batches = 0;
    v.session.send_chunk_batch();
    assert_eq!(v.session.view.queue.len(), 7);
    v.drain_queue();
    assert_eq!(v.session.view.sent.len(), 9);
    assert_eq!(v.sees(at), Some(stone()));
    v.packet_matches_mirror(center);

    enable(&world, false);
    world.ticks(1).await;
    v.receive_chunks(9).await;
    v.drain_queue();
    assert_eq!(v.sees(at), Some(diamond()));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unchanged_setting_resends_nothing() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    enable(&world, true);
    let region = world.region(RegionPos::new(1, 1)).await;
    let center = ChunkPos::new(60, 40);
    let mut v = viewer(&world, 1001);
    v.subscribe(&region, &around(center)).await;

    enable(&world, true);
    let Some(Value::String(mode)) = world.shared.config.get(anti_xray::MODE) else {
        panic!("a mode");
    };
    world
        .shared
        .config
        .set_text(anti_xray::MODE, &mode)
        .unwrap();
    world.ticks(5).await;
    world
        .shared
        .config
        .set_text(
            anti_xray::MODE,
            if mode == "fill" { "hide" } else { "fill" },
        )
        .unwrap();
    world.ticks(1).await;
    v.receive_chunks(9).await;
    world.ticks(5).await;
    assert_eq!(v.chunks_received(), 0, "only the real change resent");
    world.shutdown().await;
}
