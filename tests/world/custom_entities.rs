//! Custom entities: the stand-in is tagged with the custom id and its
//! display and interaction parts board it as passengers.

use super::*;
use tuff_core::custom::entities::EntityPart;
use tuff_core::custom::{CustomEntityDef, entity_id};
use tuff_core::feature::{Feature, LoadContext};
use tuff_item::load::{LoadError, Phase};
use tuff_nbt::Compound;
use tuff_vanilla_data::registry::EntityType;

async fn watch(
    region: &RegionHandle,
    pos: ChunkPos,
) -> (PlayerHandle, mpsc::UnboundedReceiver<PlayerEvent>) {
    let (watcher, mut events) = PlayerHandle::new(900);
    subscribe(region, &watcher, &mut events, &[pos]).await;
    (watcher, events)
}

const GOLEM: &str = "test:golem";

struct Golems;

impl Feature for Golems {
    fn id(&self) -> &'static str {
        "test-custom-entities"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() != Phase::Content {
            return Ok(());
        }
        let def = CustomEntityDef::new(GOLEM, EntityType::IronGolem)
            .with_part(EntityPart::new(EntityType::ItemDisplay, Compound::new()))
            .with_part(EntityPart::hitbox(1.5, 3.0));
        cx.registries().custom.register_entity(self.id(), def)
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_spawned_custom_entity_carries_its_identity_and_parts() {
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(Golems)]);
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let _watcher = watch(&region, pos).await;
    let at = [8.5, 100.0, 8.5];
    let mut nbt = world
        .shared
        .custom
        .spawn_entity_nbt(GOLEM, at, 0.0)
        .expect("registered");
    nbt.insert("NoAI", true);
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt,
            position: at,
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    world.ticks(3).await;
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let all = answer.await.unwrap();
    let golem = all
        .iter()
        .find(|e| e.kind == EntityType::IronGolem)
        .expect("the stand-in");
    assert_eq!(entity_id(&golem.tags), Some(GOLEM));
    for kind in [EntityType::ItemDisplay, EntityType::Interaction] {
        let part = all.iter().find(|e| e.kind == kind).expect("a part");
        for axis in 0..3 {
            assert!((part.position[axis] - golem.position[axis]).abs() < 3.0);
        }
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_parts_go_away_with_a_dead_custom_entity() {
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), vec![Box::new(Golems)]);
    let pos = ChunkPos::new(0, 0);
    let region = world.region(pos.region()).await;
    let _watcher = watch(&region, pos).await;
    let at = [8.5, 100.0, 8.5];
    let mut nbt = world
        .shared
        .custom
        .spawn_entity_nbt(GOLEM, at, 0.0)
        .expect("registered");
    nbt.insert("NoAI", true);
    nbt.insert("Health", 0.0f32);
    let (reply, summoned) = oneshot::channel();
    region
        .request(RegionRequest::Summon {
            nbt,
            position: at,
            fresh: false,
            reply,
        })
        .await;
    assert!(summoned.await.unwrap());
    world.ticks(30).await;
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    let all = answer.await.unwrap();
    for kind in [
        EntityType::IronGolem,
        EntityType::ItemDisplay,
        EntityType::Interaction,
    ] {
        assert!(all.iter().all(|e| e.kind != kind), "{kind:?} is left");
    }
    world.shutdown().await;
}
