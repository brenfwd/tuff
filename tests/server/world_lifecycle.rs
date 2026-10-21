use crate::common;

use std::path::Path;

use std::sync::Arc;
use std::time::Duration;

use common::TempDir;
use tuff::Server;
use tuff::feature::{Feature, LoadContext};
use tuff::world::Shared;
use tuff::world::dimension::Dimensions;
use tuff::world::messages::CoordinatorMessage;
use tuff::world::services::{ServiceFactory, WorldService};
use tuff_item::load::{LoadError, Phase};

fn server(bind: &str, instance: &Path) -> Server {
    Server::builder()
        .binds(vec![bind.to_string()])
        .instance_dir(instance.to_path_buf())
        .seed(7)
        .build()
}

#[tuff_core::watchdog]
#[tokio::test]
async fn creates_vanilla_layout_and_locks_the_world() {
    let instance = TempDir::new();
    let first = server("127.0.0.1:0", instance.path());
    let handle = first.handle();
    let task = tokio::spawn(first.run());
    handle.local_addrs().await.unwrap();

    let world = instance.path().join("world");
    assert!(world.join("level.dat").is_file());
    assert!(world.join("session.lock").is_file());
    assert!(
        world
            .join("data/minecraft/world_gen_settings.dat")
            .is_file()
    );
    assert!(
        world
            .join("dimensions/minecraft/overworld/data/minecraft/world_border.dat")
            .is_file()
    );

    let second = server("127.0.0.1:0", instance.path()).run().await;
    let err = format!("{:#}", second.unwrap_err());
    assert!(err.contains("already in use"), "{err}");

    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    assert!(world.join("data/minecraft/world_clocks.dat").is_file());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn failing_to_bind_releases_the_world() {
    let instance = TempDir::new();
    let occupier = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let taken = occupier.local_addr().unwrap().to_string();
    assert!(server(&taken, instance.path()).run().await.is_err());

    let retry = server("127.0.0.1:0", instance.path());
    let handle = retry.handle();
    let task = tokio::spawn(retry.run());
    handle.local_addrs().await.unwrap();
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}

/// A service that panics the coordinator as soon as the world starts.
struct CoordinatorFuse;

impl Feature for CoordinatorFuse {
    fn id(&self) -> &'static str {
        "test-coordinator-fuse"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries().services.push(Arc::new(CoordinatorFuse));
        }
        Ok(())
    }
}

impl ServiceFactory for CoordinatorFuse {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(CoordinatorFuse)
    }
}

impl WorldService for CoordinatorFuse {
    fn start(&self, shared: &Arc<Shared>) {
        let coordinator = shared.coordinator.clone();
        tokio::spawn(async move {
            let _ = coordinator
                .send(CoordinatorMessage::Panic("coordinator test panic"))
                .await;
        });
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_panicking_coordinator_stops_the_server_and_saves_the_world() {
    let instance = TempDir::new();
    let fused = Server::builder()
        .binds(vec!["127.0.0.1:0".to_string()])
        .instance_dir(instance.path().to_path_buf())
        .seed(7)
        .features(vec![Box::new(CoordinatorFuse)])
        .build();
    let before = tuff::metrics::actor_panics().get();
    tokio::time::timeout(Duration::from_secs(60), fused.run())
        .await
        .expect("the server stops on its own")
        .unwrap();
    assert!(tuff::metrics::actor_panics().get() > before);
    let world = instance.path().join("world");
    assert!(world.join("data/minecraft/world_clocks.dat").is_file());

    let again = server("127.0.0.1:0", instance.path());
    let handle = again.handle();
    let task = tokio::spawn(again.run());
    handle
        .local_addrs()
        .await
        .expect("the world lock was released");
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}
