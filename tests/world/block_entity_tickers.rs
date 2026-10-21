//! The block entity ticker table: a registered ticker replaces the
//! built-in tick of its container kind.

use tuff_core::world::block_entity::{BlockEntityTicker, ContainerKind, TickOutcome, TickView};

use super::*;
use std::sync::Mutex;
use tuff_core::feature::{Feature, LoadContext};
use tuff_item::load::{LoadError, Phase};

struct Recorder(Arc<Mutex<Vec<BlockPos>>>);

impl BlockEntityTicker for Recorder {
    fn tick(&self, view: &mut TickView) -> TickOutcome {
        self.0.lock().unwrap().push(view.pos);
        TickOutcome {
            set_state: view.state.with("enabled", "false"),
            ..TickOutcome::default()
        }
    }
}

struct Plug(Arc<Mutex<Vec<BlockPos>>>);

impl Feature for Plug {
    fn id(&self) -> &'static str {
        "test-hopper-ticker"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries().block_entity_tickers.register(
                self.id(),
                ContainerKind::Hopper,
                Arc::new(Recorder(self.0.clone())),
            )?;
        }
        Ok(())
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registered_ticker_runs_each_tick_in_position_order_and_its_outcome_applies() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(Plug(seen.clone()))]);
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let (alice, mut alice_events) = PlayerHandle::new(100);
    subscribe(&region, &alice, &mut alice_events, &[pos]).await;
    let y = Generator::new(1).height(1, 1) + 1;
    let late = BlockPos::new(1, y, 2);
    let early = BlockPos::new(3, y, 1);
    place_blocks(
        &world,
        &region,
        [
            (late, Block::Hopper.default_state()),
            (early, Block::Hopper.default_state()),
        ],
    )
    .await;
    seen.lock().unwrap().clear();
    world.ticks(1).await;
    let first_tick: Vec<BlockPos> = seen.lock().unwrap().clone();
    assert_eq!(first_tick, vec![early, late]);
    assert_eq!(
        block_at(&region, late).await.and_then(|s| s.get("enabled")),
        Some("false")
    );
}
