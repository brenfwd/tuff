//! A plugin claiming a projectile block hit suppresses the core reaction.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use tuff_core::feature::{Feature, LoadContext};
use tuff_core::world::entity::ProjectileImpact;
use tuff_core::world::messages::RegionRequest;
use tuff_core::world::raycast::Hit;
use tuff_core::world::region_api::{RegionHooks, RegionTask, RegionWorld};
use tuff_item::load::{LoadError, Phase};
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, BlockState};

struct Claim {
    claiming: Arc<AtomicBool>,
    seen: Arc<AtomicUsize>,
}

impl RegionHooks for Claim {
    fn projectile_hit_block(
        &self,
        _world: &mut dyn RegionWorld,
        _hit: Hit,
        _impact: &ProjectileImpact,
        state: BlockState,
    ) -> bool {
        if state.block() != Block::BigDripleaf {
            return false;
        }
        self.seen.fetch_add(1, Ordering::SeqCst);
        self.claiming.load(Ordering::SeqCst)
    }
}

struct ClaimFeature(Arc<AtomicBool>, Arc<AtomicUsize>);

impl Feature for ClaimFeature {
    fn id(&self) -> &'static str {
        "test-projectile-claim"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries().region_hooks.push(Arc::new(Claim {
                claiming: self.0.clone(),
                seen: self.1.clone(),
            }));
        }
        Ok(())
    }
}

async fn shoot(region: &RegionHandle, pos: BlockPos) {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: pos,
            max: pos,
            run: RegionTask::new(move |world| {
                let impact = ProjectileImpact {
                    kind: EntityType::Arrow,
                    arrow: true,
                    on_fire: false,
                    velocity: [0.0, 0.0, 1.0],
                    owner: None,
                };
                let hit = Hit {
                    pos,
                    face: Facing::North,
                    location: [0.5, 0.5, 0.0],
                };
                world.projectile_hit_block(hit, &impact);
                let _ = reply.send(());
            }),
        })
        .await;
    answer.await.unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claimed_projectile_hit_skips_the_core_reaction() {
    let dir = TestDir::new();
    let claiming = Arc::new(AtomicBool::new(true));
    let seen = Arc::new(AtomicUsize::new(0));
    let world = TestWorld::start_with_features(
        dir.path(),
        vec![Box::new(ClaimFeature(claiming.clone(), seen.clone()))],
    );
    let (viewer, mut events) = PlayerHandle::new(1000);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &viewer, &mut events, center).await;
    let region = world.region(center.region()).await;
    let pos = BlockPos::new(84, 100, 84);
    place_blocks(&world, &region, [(pos, Block::BigDripleaf.default_state())]).await;

    shoot(&region, pos).await;
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    let power = |s: Option<BlockState>| s.and_then(|s| s.get("tilt").map(str::to_owned));
    assert_eq!(power(block_at(&region, pos).await).as_deref(), Some("none"));

    claiming.store(false, Ordering::SeqCst);
    shoot(&region, pos).await;
    assert_ne!(power(block_at(&region, pos).await).as_deref(), Some("none"));
}
