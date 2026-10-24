//! One owner per player: logins, a session dying before its final save and
//! offline writers are ordered through the player's lease.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use super::fake_players::common::{TempDir, TestServer};
use tokio::sync::oneshot;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::storage::player_data;
use tuff_core::world::Shared;
use tuff_core::world::dimension::Dimensions;
use tuff_core::world::inventory_edit::InventoryEdit;
use tuff_core::world::messages::{CoordinatorMessage, PlayerEvent, SessionTask};
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{HookFuture, SessionHooks};
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

struct PanicWhileLeaving;

impl SessionHooks for PanicWhileLeaving {
    fn leaving<'a>(&'a self, session: &'a mut PlayerSession) -> HookFuture<'a> {
        let doomed = session.profile.name == "pam";
        Box::pin(async move {
            if doomed {
                panic!("a hook failed while the player was leaving");
            }
        })
    }
}

struct LeaseFeature(Slot);

impl Feature for LeaseFeature {
    fn id(&self) -> &'static str {
        "test-player-lease"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries()
                .services
                .push(Arc::new(CaptureFactory(self.0.clone())));
            cx.registries()
                .session_hooks
                .push(Arc::new(PanicWhileLeaving));
        }
        Ok(())
    }
}

async fn start() -> (TestServer, Arc<Shared>) {
    let instance = TempDir::new();
    let slot = Slot::default();
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(LeaseFeature(slot.clone())));
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

async fn say(server: &TestServer, line: &str) {
    server.handle.run_command(line).await.unwrap();
}

async fn join(server: &TestServer, name: &str) -> Bot {
    let bot = Bot::connect(server.addr, BotConfig::new(name))
        .await
        .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    bot
}

fn diamond() -> ItemStack {
    ItemStack::mint(Item::Diamond, 1, MintReason::Test)
}

fn count(slots: &[Option<ItemStack>]) -> u32 {
    slots
        .iter()
        .flatten()
        .filter(|stack| stack.item() == Item::Diamond)
        .map(|stack| u32::from(stack.count()))
        .sum()
}

async fn newest(shared: &Shared, uuid: u128) -> Option<tuff_core::world::messages::PlayerHandle> {
    let (reply, players) = oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::Players { reply })
        .await
        .unwrap();
    players
        .await
        .unwrap()
        .into_iter()
        .filter(|p| p.uuid == uuid)
        .max_by_key(|p| p.id)
        .map(|p| p.handle)
}

async fn held(shared: &Shared, uuid: u128) -> u32 {
    let handle = newest(shared, uuid).await.expect("online");
    let (reply, held) = oneshot::channel();
    handle.send(PlayerEvent::Run(SessionTask::new(move |session| {
        Box::pin(async move {
            let _ = reply.send(count(session.inventory().slots()));
        })
    })));
    held.await.unwrap()
}

async fn wait_offline(shared: &Shared, uuid: u128) {
    for _ in 0..1000 {
        if newest(shared, uuid).await.is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the player never left");
}

async fn stored(shared: &Shared, uuid: u128) -> u32 {
    let backend = shared.backend.clone();
    tokio::task::spawn_blocking(move || {
        let Some(mut raw) = player_data::load(backend.documents(), uuid).unwrap() else {
            return 0;
        };
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

async fn write_stored(shared: &Shared, uuid: u128, diamonds: u8) {
    let backend = shared.backend.clone();
    tokio::task::spawn_blocking(move || {
        let mut stack = tuff_item::item::ItemPicture::new(Item::Diamond, diamonds).to_nbt();
        stack.insert("Slot", Tag::Byte(0));
        let root = Compound::new().with("Inventory", Tag::from(vec![stack]));
        player_data::save(backend.documents(), uuid, root).unwrap();
    })
    .await
    .unwrap();
}

fn edit(kind: fn(tuff_item::item::ItemSnapshot, u32) -> InventoryEdit, n: u32) -> InventoryEdit {
    kind(tuff_item::item::ItemSnapshot::of(&diamond()), n)
}

fn give(stack: tuff_item::item::ItemSnapshot, count: u32) -> InventoryEdit {
    InventoryEdit::Give { stack, count }
}

fn take(stack: tuff_item::item::ItemSnapshot, count: u32) -> InventoryEdit {
    InventoryEdit::Take { stack, count }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_login_sees_what_the_first_session_moved_before_it_left() {
    let (server, shared) = start().await;
    let uuid = tuff_core::auth::offline_uuid("alex");
    let first = join(&server, "alex").await;
    say(&server, "give alex minecraft:diamond 5").await;
    assert_eq!(held(&shared, uuid).await, 5);
    let command = {
        let handle = server.handle.clone();
        tokio::spawn(async move {
            handle
                .run_command("give alex minecraft:diamond 3")
                .await
                .unwrap()
        })
    };
    let second = join(&server, "alex").await;
    command.await.unwrap();
    first.wait_for(|_| true).await.ok();
    let total = held(&shared, uuid).await;
    assert!(
        total == 5 || total == 8,
        "the give either landed before the save or on the old session: {total}"
    );
    drop((first, second));
    wait_offline(&shared, uuid).await;
    assert_eq!(
        stored(&shared, uuid).await,
        total,
        "nothing duplicated or lost"
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_dying_before_its_final_save_hands_the_next_login_the_last_save() {
    let (server, shared) = start().await;
    let uuid = tuff_core::auth::offline_uuid("pam");
    write_stored(&shared, uuid, 4).await;
    let first = join(&server, "pam").await;
    assert_eq!(held(&shared, uuid).await, 4);
    say(&server, "give pam minecraft:diamond 6").await;
    assert_eq!(held(&shared, uuid).await, 10);
    let dirty = tuff_core::world::player_lease::dirty_returns();
    drop(first);
    wait_offline(&shared, uuid).await;
    assert!(
        tuff_core::world::player_lease::dirty_returns() > dirty,
        "the lease came back dirty"
    );
    assert_eq!(
        stored(&shared, uuid).await,
        4,
        "the final save never ran, so the last consistent save stays"
    );
    let second = join(&server, "pam").await;
    assert_eq!(held(&shared, uuid).await, 4);
    drop(second);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn offline_edits_racing_a_login_apply_once_and_in_order() {
    let (server, shared) = start().await;
    let uuid = tuff_core::auth::offline_uuid("sam");
    write_stored(&shared, uuid, 10).await;
    let mut results = Vec::new();
    for _ in 0..3 {
        let (reply, applied) = oneshot::channel();
        shared
            .coordinator
            .send(CoordinatorMessage::EditInventory {
                uuid,
                edit: edit(take, 2),
                reply,
            })
            .await
            .unwrap();
        results.push(applied);
    }
    let bot = join(&server, "sam").await;
    let (reply, applied) = oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::EditInventory {
            uuid,
            edit: edit(give, 1),
            reply,
        })
        .await
        .unwrap();
    let mut applied_edits = 0;
    for result in results {
        applied_edits += u32::from(result.await.unwrap());
    }
    let online_give = applied.await.unwrap();
    assert!(online_give);
    assert_eq!(
        held(&shared, uuid).await,
        10 - 2 * applied_edits + 1,
        "every edit landed exactly once, offline or on the session"
    );
    drop(bot);
    wait_offline(&shared, uuid).await;
    assert_eq!(stored(&shared, uuid).await, 10 - 2 * applied_edits + 1);
    stop(server).await;
}
