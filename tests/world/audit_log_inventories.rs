//! Rolling back `+item` (pickup) rows takes the items back from the player
//! who picked them up, online or offline, and restoring gives them again,
//! all or nothing, with logins ordered against offline edits.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use super::fake_players::common::{TempDir, TestServer};
use tokio::sync::oneshot;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::storage::player_data;
use tuff_core::world::Shared;
use tuff_core::world::dimension::{DimensionId, Dimensions};
use tuff_core::world::inventory_edit::InventoryEdit;
use tuff_core::world::messages::{CoordinatorMessage, PlayerEvent, SessionTask};
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_feature_builtin_audit_log::AuditLogFeature;
use tuff_feature_builtin_audit_log::record::{Kind, Record, Who};
use tuff_item::item::{ItemStack, MintReason};
use tuff_item::load::{LoadError, Phase};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::Item;

type Slot = Arc<OnceLock<Arc<Shared>>>;

struct Capture(Slot);

impl WorldService for Capture {
    fn start(&self, shared: &Arc<Shared>) {
        let _ = self.0.set(shared.clone());
    }
}

struct CaptureFactory(Slot);

impl ServiceFactory for CaptureFactory {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(Capture(self.0.clone()))
    }
}

struct CaptureFeature(Slot);

impl Feature for CaptureFeature {
    fn id(&self) -> &'static str {
        "test-capture-shared"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries()
                .services
                .push(Arc::new(CaptureFactory(self.0.clone())));
        }
        Ok(())
    }
}

async fn start() -> (TestServer, Arc<Shared>) {
    let instance = TempDir::new();
    let slot = Slot::default();
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(AuditLogFeature::at(
        instance.path().join("audit.sqlite"),
    )));
    features.push(Box::new(CaptureFeature(slot.clone())));
    let server = Server::builder()
        .features(features)
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let shared = slot.get().expect("the world started").clone();
    (
        TestServer {
            handle,
            addr,
            task,
            _instance: instance,
        },
        shared,
    )
}

async fn stop(server: TestServer) {
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
}

async fn say(server: &TestServer, line: &str) -> Vec<String> {
    server
        .handle
        .run_command(line)
        .await
        .unwrap()
        .iter()
        .map(|line| line.plain())
        .collect()
}

fn diamond() -> ItemStack {
    ItemStack::mint(Item::Diamond, 1, MintReason::Test)
}

/// Logs that `name` picked up `count` diamonds.
async fn log_pickup(shared: &Shared, name: &str, count: i32) {
    let log = shared
        .services
        .get::<tuff_feature_builtin_audit_log::AuditLog>()
        .unwrap();
    let who = Who::Player {
        uuid: tuff_core::auth::offline_uuid(name),
        name: Arc::from(name),
    };
    let mut record = Record::new(
        tuff_feature_builtin_audit_log::record::now_ms(),
        who,
        Kind::ItemAdd,
        DimensionId::default(),
        [0, 64, 0],
    );
    record.material = Some("minecraft:diamond".into());
    record.amount = count;
    record.data = Some(diamond().to_nbt());
    log.log(record);
    log.sync().await.unwrap();
}

async fn write_stored(shared: &Shared, uuid: u128, diamonds: u8) {
    let backend = shared.backend.clone();
    tokio::task::spawn_blocking(move || {
        let mut stack = ItemStack::mint(Item::Diamond, diamonds, MintReason::Test).to_nbt();
        stack.insert("Slot", Tag::Byte(0));
        let root = Compound::new().with("Inventory", Tag::from(vec![stack]));
        player_data::save(backend.documents(), uuid, root).unwrap();
    })
    .await
    .unwrap();
}

async fn stored_diamonds(shared: &Shared, uuid: u128) -> u32 {
    let backend = shared.backend.clone();
    tokio::task::spawn_blocking(move || {
        let mut raw = player_data::load(backend.documents(), uuid)
            .unwrap()
            .unwrap();
        let mut held = 0;
        player_data::edit_inventory(&mut raw, |inventory| {
            held = count(inventory.slots());
            false
        });
        held
    })
    .await
    .unwrap()
}

fn count(slots: &[Option<ItemStack>]) -> u32 {
    slots
        .iter()
        .flatten()
        .filter(|stack| stack.item == Item::Diamond)
        .map(|stack| u32::from(stack.count()))
        .sum()
}

async fn online(shared: &Shared, uuid: u128) -> bool {
    let (reply, players) = oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::Players { reply })
        .await
        .unwrap();
    players.await.unwrap().iter().any(|p| p.uuid == uuid)
}

async fn held_diamonds(shared: &Shared, uuid: u128) -> u32 {
    let (reply, players) = oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::Players { reply })
        .await
        .unwrap();
    let player = players
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.uuid == uuid)
        .expect("online");
    let (reply, held) = oneshot::channel();
    player
        .handle
        .send(PlayerEvent::Run(SessionTask::new(move |session| {
            Box::pin(async move {
                let _ = reply.send(count(session.inventory().slots()));
            })
        })));
    held.await.unwrap()
}

async fn join(server: &TestServer, name: &str) -> Bot {
    let bot = Bot::connect(server.addr, BotConfig::new(name))
        .await
        .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    bot
}

async fn leave(shared: &Shared, bot: Bot, uuid: u128) {
    drop(bot);
    for _ in 0..1000 {
        if !online(shared, uuid).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the player never left");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_online_players_pickup_is_taken_back_restored_and_undone() {
    let (server, shared) = start().await;
    let uuid = tuff_core::auth::offline_uuid("alex");
    let bot = join(&server, "alex").await;
    say(&server, "give alex minecraft:diamond 5").await;
    assert_eq!(held_diamonds(&shared, uuid).await, 5);
    log_pickup(&shared, "alex", 3).await;
    log_pickup(&shared, "alex", 40).await;

    let rolled = say(&server, "tuff audit rollback t:1h r:#global a:+item").await;
    assert_eq!(
        rolled[0], "Rolled back 0 blocks and 3 items from 2 rows in 0 regions",
        "{rolled:?}"
    );
    assert!(
        rolled
            .iter()
            .any(|l| l.starts_with("1 item rows could not")),
        "{rolled:?}"
    );
    assert_eq!(
        held_diamonds(&shared, uuid).await,
        2,
        "the 40 not held are not taken at all"
    );

    let undone = say(&server, "tuff audit undo").await;
    assert!(undone[0].starts_with("Restored"), "{undone:?}");
    assert_eq!(held_diamonds(&shared, uuid).await, 5);

    say(&server, "tuff audit rollback t:1h r:#global a:+item").await;
    assert_eq!(held_diamonds(&shared, uuid).await, 2);
    let restored = say(&server, "tuff audit restore t:1h r:#global a:+item").await;
    assert!(restored[0].contains("3 items"), "{restored:?}");
    assert_eq!(held_diamonds(&shared, uuid).await, 5);
    let again = say(&server, "tuff audit restore t:1h r:#global a:+item").await;
    assert!(
        !again[0].contains("items"),
        "a restore applies once: {again:?}"
    );
    assert_eq!(held_diamonds(&shared, uuid).await, 5);

    leave(&shared, bot, uuid).await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_offline_players_pickup_is_taken_from_stored_data_and_kept_at_login() {
    let (server, shared) = start().await;
    let uuid = tuff_core::auth::offline_uuid("steve");
    write_stored(&shared, uuid, 5).await;
    log_pickup(&shared, "steve", 3).await;

    let rolled = say(&server, "tuff audit rollback t:1h r:#global a:+item").await;
    assert!(rolled[0].contains("3 items"), "{rolled:?}");
    assert_eq!(stored_diamonds(&shared, uuid).await, 2);

    let bot = join(&server, "steve").await;
    assert_eq!(held_diamonds(&shared, uuid).await, 2);
    let restored = say(&server, "tuff audit restore t:1h r:#global a:+item").await;
    assert!(restored[0].contains("3 items"), "{restored:?}");
    assert_eq!(held_diamonds(&shared, uuid).await, 5);
    leave(&shared, bot, uuid).await;
    assert_eq!(stored_diamonds(&shared, uuid).await, 5);

    let nobody = tuff_core::auth::offline_uuid("nobody");
    log_pickup(&shared, "nobody", 1).await;
    let rolled = say(&server, "tuff audit rollback t:1h r:#global u:nobody").await;
    assert!(
        rolled
            .iter()
            .any(|l| l.starts_with("1 item rows could not")),
        "a player never seen has no inventory: {rolled:?}"
    );
    assert!(!online(&shared, nobody).await);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_login_waits_for_an_offline_edit_and_a_logout_save_comes_first() {
    let (server, shared) = start().await;
    let uuid = tuff_core::auth::offline_uuid("sam");
    write_stored(&shared, uuid, 5).await;

    let (reply, applied) = oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::EditInventory {
            uuid,
            edit: InventoryEdit::Take {
                stack: tuff_item::item::ItemSnapshot::of(&diamond()),
                count: 3,
            },
            reply,
        })
        .await
        .unwrap();
    let bot = join(&server, "sam").await;
    assert!(applied.await.unwrap());
    assert_eq!(
        held_diamonds(&shared, uuid).await,
        2,
        "the session loaded after the edit was saved"
    );

    drop(bot);
    let (reply, applied) = oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::EditInventory {
            uuid,
            edit: InventoryEdit::Take {
                stack: tuff_item::item::ItemSnapshot::of(&diamond()),
                count: 2,
            },
            reply,
        })
        .await
        .unwrap();
    let applied = applied.await.unwrap();
    for _ in 0..1000 {
        if !online(&shared, uuid).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let (reply, settled) = oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::EditInventory {
            uuid,
            edit: InventoryEdit::Give {
                stack: tuff_item::item::ItemSnapshot::of(&diamond()),
                count: 1,
            },
            reply,
        })
        .await
        .unwrap();
    assert!(settled.await.unwrap());
    let expected = if applied { 1 } else { 3 };
    assert_eq!(
        stored_diamonds(&shared, uuid).await,
        expected,
        "an edit during logout is either applied and saved, or refused"
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_edit_queued_behind_a_logout_is_applied_once_after_the_logout_save() {
    let (server, shared) = start().await;
    let uuid = tuff_core::auth::offline_uuid("sam");
    write_stored(&shared, uuid, 5).await;
    let _bot = join(&server, "sam").await;

    let (reply, players) = oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::Players { reply })
        .await
        .unwrap();
    let player = players
        .await
        .unwrap()
        .into_iter()
        .find(|p| p.uuid == uuid)
        .unwrap();
    let (held, hold) = oneshot::channel::<()>();
    let (entered, holding) = oneshot::channel();
    player
        .handle
        .send(PlayerEvent::Run(SessionTask::new(move |session| {
            Box::pin(async move {
                let _ = entered.send(());
                let _ = hold.await;
                session.disconnected = true;
            })
        })));
    holding.await.unwrap();

    let (reply, applied) = oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::EditInventory {
            uuid,
            edit: InventoryEdit::Take {
                stack: tuff_item::item::ItemSnapshot::of(&diamond()),
                count: 2,
            },
            reply,
        })
        .await
        .unwrap();
    assert!(online(&shared, uuid).await, "the edit went to the session");
    held.send(()).unwrap();

    assert!(applied.await.unwrap(), "applied offline after the logout");
    assert!(!online(&shared, uuid).await);
    assert_eq!(
        stored_diamonds(&shared, uuid).await,
        3,
        "applied exactly once"
    );
    stop(server).await;
}

async fn log_container_take(shared: &Shared, name: &str, count: i32) {
    let log = shared
        .services
        .get::<tuff_feature_builtin_audit_log::AuditLog>()
        .unwrap();
    let who = Who::Player {
        uuid: tuff_core::auth::offline_uuid(name),
        name: Arc::from(name),
    };
    let mut record = Record::new(
        tuff_feature_builtin_audit_log::record::now_ms(),
        who,
        Kind::ContainerRemove,
        DimensionId::default(),
        [0, 100, 0],
    );
    record.material = Some("minecraft:diamond".into());
    record.amount = count;
    record.text = Some("slot=0".into());
    record.data = Some(diamond().to_nbt());
    log.log(record);
    log.sync().await.unwrap();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_strict_rollback_takes_container_items_from_the_player_all_or_nothing() {
    let (server, shared) = start().await;
    let steve = tuff_core::auth::offline_uuid("steve");
    let sam = tuff_core::auth::offline_uuid("sam");
    write_stored(&shared, steve, 5).await;
    write_stored(&shared, sam, 1).await;
    say(&server, "forceload add 0 0").await;
    let placed = say(&server, "setblock 0 100 0 minecraft:chest").await;
    assert!(placed[0].contains("Changed"), "{placed:?}");
    log_container_take(&shared, "steve", 3).await;
    log_container_take(&shared, "sam", 4).await;

    let rolled = say(
        &server,
        "tuff audit rollback t:1h r:#global a:-container #strict",
    )
    .await;
    assert!(rolled[0].contains("3 items"), "{rolled:?}");
    assert!(
        rolled.iter().any(|l| l.starts_with("1 container rows")),
        "{rolled:?}"
    );
    assert_eq!(stored_diamonds(&shared, steve).await, 2);
    assert_eq!(stored_diamonds(&shared, sam).await, 1, "all or nothing");
    let chest = say(&server, "data get block 0 100 0 Items").await;
    assert!(
        chest.join(" ").contains("count: 3"),
        "only steve's: {chest:?}"
    );

    let undone = say(&server, "tuff audit undo").await;
    assert!(undone[0].starts_with("Restored"), "{undone:?}");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plain_rollback_leaves_the_players_inventory_alone() {
    let (server, shared) = start().await;
    let steve = tuff_core::auth::offline_uuid("steve");
    write_stored(&shared, steve, 5).await;
    say(&server, "forceload add 0 0").await;
    let placed = say(&server, "setblock 0 100 0 minecraft:chest").await;
    assert!(placed[0].contains("Changed"), "{placed:?}");
    log_container_take(&shared, "steve", 3).await;
    let rolled = say(&server, "tuff audit rollback t:1h r:#global a:-container").await;
    assert!(rolled[0].contains("3 items"), "{rolled:?}");
    assert_eq!(stored_diamonds(&shared, steve).await, 5);
    stop(server).await;
}
