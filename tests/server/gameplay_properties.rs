use crate::common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::TempDir;
use tuff::Server;
use tuff::feature::{Feature, LoadContext};
use tuff::world::Shared;
use tuff::world::dimension::Dimensions;
use tuff::world::messages::CoordinatorMessage;
use tuff::world::player::Difficulty;
use tuff::world::services::{ServiceFactory, WorldService};
use tuff_item::load::{LoadError, Phase};

type Slot = Arc<Mutex<Option<Arc<Shared>>>>;

struct Capture(Slot);

impl Feature for Capture {
    fn id(&self) -> &'static str {
        "test-capture-shared"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries()
                .services
                .push(Arc::new(Capture(self.0.clone())));
        }
        Ok(())
    }
}

impl ServiceFactory for Capture {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(Capture(self.0.clone()))
    }
}

impl WorldService for Capture {
    fn start(&self, shared: &Arc<Shared>) {
        *self.0.lock().unwrap() = Some(shared.clone());
    }
}

struct Run {
    shared: Arc<Shared>,
    handle: tuff::ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

async fn start(instance: &TempDir) -> Run {
    let slot = Slot::default();
    let server = Server::builder()
        .binds(vec!["127.0.0.1:0".to_string()])
        .instance_dir(instance.path().to_path_buf())
        .seed(7)
        .features(vec![Box::new(Capture(slot.clone()))])
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();
    let shared = slot.lock().unwrap().clone().expect("service started");
    Run {
        shared,
        handle,
        task,
    }
}

async fn stop(run: Run) {
    run.handle.shutdown().await.unwrap();
    run.task.await.unwrap().unwrap();
}

async fn set(shared: &Shared, key: &str, value: &str) {
    let (reply, rx) = tokio::sync::oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::ServerProperties {
            change: Some((key.to_owned(), value.to_owned())),
            reply,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), rx)
        .await
        .unwrap()
        .unwrap();
}

fn hardcore(shared: &Shared) -> bool {
    shared.hardcore.load(std::sync::atomic::Ordering::Relaxed)
}

#[tuff_core::watchdog]
#[tokio::test]
async fn pvp_and_hardcore_change_on_a_running_world_and_persist() {
    let instance = TempDir::new();
    let run = start(&instance).await;
    assert!(run.shared.rule_bool("pvp"));
    assert!(!hardcore(&run.shared));

    set(&run.shared, "pvp", "false").await;
    assert!(!run.shared.rule_bool("pvp"));
    set(&run.shared, "hardcore", "true").await;
    assert!(hardcore(&run.shared));
    assert_eq!(run.shared.difficulty(), Difficulty::Hard);
    stop(run).await;

    let run = start(&instance).await;
    assert!(!run.shared.rule_bool("pvp"));
    assert!(hardcore(&run.shared));
    assert_eq!(run.shared.difficulty(), Difficulty::Hard);
    set(&run.shared, "hardcore", "false").await;
    assert!(!hardcore(&run.shared));
    assert_eq!(run.shared.difficulty(), Difficulty::Hard);
    stop(run).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn properties_changed_while_stopped_apply_to_the_existing_world() {
    let instance = TempDir::new();
    stop(start(&instance).await).await;
    std::fs::write(
        instance.path().join("server.properties"),
        "pvp=false\nhardcore=true\n",
    )
    .unwrap();
    let run = start(&instance).await;
    assert!(!run.shared.rule_bool("pvp"));
    assert!(hardcore(&run.shared));
    assert_eq!(run.shared.difficulty(), Difficulty::Hard);
    stop(run).await;
}
