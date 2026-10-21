//! Messages a test sends a region before bumping the clock are handled
//! before that tick, even while other traffic keeps the region polling.

use super::*;

fn chatter(region: &RegionHandle) -> tokio::task::JoinHandle<()> {
    let region = region.clone();
    tokio::spawn(async move {
        loop {
            let (reply, answer) = oneshot::channel();
            if !region.request(RegionRequest::Entities { reply }).await || answer.await.is_err() {
                return;
            }
        }
    })
}

fn offer(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> Option<(i32, u64)> {
    let mut found = None;
    while let Ok(event) = events.try_recv() {
        if let PlayerEvent::OfferItem { item, offer, .. } = event {
            found = Some((item, offer));
        }
    }
    found
}

async fn pickup_world(
    dir: &TestDir,
) -> (
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
    [f64; 3],
) {
    let world = TestWorld::start(dir.path());
    let pos = ChunkPos::new(2, 2);
    let region = world.region(pos.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[pos]).await;
    let y = Generator::new(1).height(40, 40) + 1;
    let at = [40.5, y as f64, 40.5];
    region.request(update(&viewer, 1, at)).await;
    (world, region, viewer, events, at)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_internal_message_sent_before_a_tick_is_handled_first() {
    let dir = TestDir::new();
    let (world, region, viewer, mut events, at) = pickup_world(&dir).await;
    region
        .request(RegionRequest::SpawnItem {
            stack: ItemStack::mint(Item::Stone, 3, MintReason::Test),
            position: at,
            velocity: [0.0; 3],
            pickup_delay: 0,
        })
        .await;
    let noise = chatter(&region);
    world.ticks(1).await;
    for round in 0..300 {
        let (item, offer) =
            offer(&mut events).unwrap_or_else(|| panic!("no offer after refusal {round}"));
        region.internal(RegionInternal::ItemTaken {
            item,
            offer,
            player: viewer.clone(),
            taken: 0,
        });
        world.ticks(1).await;
    }
    noise.abort();
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_request_sent_before_a_tick_is_handled_first() {
    let dir = TestDir::new();
    let (world, region, viewer, mut events, at) = pickup_world(&dir).await;
    let noise = chatter(&region);
    for round in 0..300 {
        region
            .request(RegionRequest::SpawnItem {
                stack: ItemStack::mint(Item::Stone, 1, MintReason::Test),
                position: at,
                velocity: [0.0; 3],
                pickup_delay: 0,
            })
            .await;
        world.ticks(1).await;
        let (item, offer) = offer(&mut events)
            .unwrap_or_else(|| panic!("item {round} not offered on its first tick"));
        region.internal(RegionInternal::ItemTaken {
            item,
            offer,
            player: viewer.clone(),
            taken: 1,
        });
    }
    noise.abort();
    world.shutdown().await;
}
