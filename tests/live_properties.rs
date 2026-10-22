//! Every server.properties key the spec marks live changes the running
//! server when set with `/tuff config minecraft.<key>`: one `live_<key>`
//! test per key, which `every_live_key_has_a_test` checks.

extern crate tuff_dylib;

mod common;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use common::{MockSessionServer, TempDir, connect, encrypted_login, ping};
use tuff::Server;
use tuff::feature::{Feature, LoadContext};
use tuff::property_spec::{Effect, PROPERTIES};
use tuff::protocol::PROTOCOL_VERSION;
use tuff::protocol::handshake::Intent;
use tuff::protocol::login::{LoginStart, SetCompression};
use tuff::protocol::play::Serverbound;
use tuff::world::Shared;
use tuff::world::dimension::Dimensions;
use tuff::world::services::{ServiceFactory, WorldService};
use tuff_bot::{Bot, BotConfig};
use tuff_core::logging::{HubOptions, LogHub};
use tuff_core::world::player::Difficulty;
use tuff_item::load::{LoadError, Phase};
use tuff_vanilla_data::packet::configuration::Clientbound as Config;
use tuff_vanilla_data::packet::play::Clientbound as Id;
use tuff_wire::PacketReader;

const WAIT: Duration = Duration::from_secs(60);

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

struct Running {
    handle: tuff::ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    addr: std::net::SocketAddr,
    shared: Arc<Shared>,
    instance: TempDir,
}

/// The vanilla plugins without anti-cheat, whose own chat, rate and break
/// checks would act before the properties under test.
fn features(slot: &Slot) -> Vec<Box<dyn Feature>> {
    let mut features: Vec<Box<dyn Feature>> = tuff::vanilla_features::all()
        .into_iter()
        .filter(|plugin| plugin.id() != "vanilla-anticheat")
        .collect();
    features.push(Box::new(Capture(slot.clone())));
    features
}

async fn start(properties: &str) -> Running {
    start_in(TempDir::new(), properties, None, 1).await
}

async fn start_in(
    instance: TempDir,
    properties: &str,
    session: Option<&str>,
    seed: i64,
) -> Running {
    std::fs::write(instance.path().join("server.properties"), properties).unwrap();
    let slot = Slot::default();
    let server = Server::builder()
        .features(features(&slot))
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".to_string()])
        .maybe_session_server_url(session)
        .maybe_profile_api_url(session)
        .online_mode(session.is_some())
        .config_overrides(vec![("tuff.offlineOps".into(), "true".into())])
        .whitelist(false)
        .view_distance(2)
        .seed(seed)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let shared = slot.lock().unwrap().clone().expect("service started");
    Running {
        handle,
        task,
        addr,
        shared,
        instance,
    }
}

async fn stop(server: Running) -> TempDir {
    server.handle.shutdown().await.unwrap();
    server.task.await.unwrap().unwrap();
    server.instance
}

async fn run(server: &Running, line: &str) -> String {
    server
        .handle
        .run_command(line)
        .await
        .unwrap()
        .iter()
        .map(|c| c.plain())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Sets a property the way an operator does, checking it was accepted.
async fn set(server: &Running, key: &str, value: &str) -> String {
    let reply = run(server, &format!("tuff config minecraft.{key} {value}")).await;
    assert!(reply.contains(&format!("Set minecraft.{key}")), "{reply}");
    reply
}

async fn bot(server: &Running, name: &str, keep: &[Id]) -> Bot {
    let bot = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 2,
            keep: keep.iter().map(|id| id.id()).collect(),
            ..BotConfig::new(name)
        },
    )
    .await
    .unwrap();
    within(bot.wait_for(|state| state.chunks > 0))
        .await
        .unwrap();
    bot
}

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(WAIT, future).await.expect("timed out")
}

/// Waits until the bot holds a body of `id` that `check` accepts.
async fn wait_packet(bot: &Bot, id: Id, check: impl Fn(&mut PacketReader) -> bool) {
    within(bot.wait_for(|state| {
        state
            .last
            .get(&id.id())
            .is_some_and(|body| check(&mut PacketReader::new(body.clone())))
    }))
    .await
    .unwrap();
}

async fn wait_kicked(bot: &Bot, key: &str) {
    within(bot.wait_for(|state| state.disconnected.is_some()))
        .await
        .ok();
    let reason = bot.state().disconnected.unwrap_or_default();
    assert!(reason.contains(key), "{reason}");
}

async fn passes(server: &Running, line: &str) -> bool {
    run(server, line).await.contains("Test passed")
}

async fn eventually(mut check: impl AsyncFnMut() -> bool) {
    for _ in 0..1200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition never held");
}

/// An offline login up to the compression packet or the disconnect.
async fn login(server: &Running, intent: Intent) -> tuff::net::connection::Connection {
    let mut conn = connect(server.addr, PROTOCOL_VERSION, intent).await;
    conn.send(&LoginStart {
        name: "Probe".to_string(),
        uuid: 0,
    })
    .await
    .unwrap();
    conn
}

/// The log hub every test in this binary shares.
fn hub() -> &'static LogHub {
    static HUB: OnceLock<LogHub> = OnceLock::new();
    HUB.get_or_init(|| {
        let hub = LogHub::spawn(HubOptions {
            terminal: false,
            ..HubOptions::default()
        });
        tuff_core::logging::install(&hub);
        hub
    })
}

/// Every log line published after `subscription` was taken, once the hub
/// has written what is queued.
async fn logged(subscription: &mut tuff_core::logging::Subscription) -> Vec<String> {
    hub().flush().await;
    let mut lines = Vec::new();
    while let Ok(line) = subscription.live.try_recv() {
        lines.push(line.text.clone());
    }
    lines
}

fn test_name(key: &str) -> String {
    format!("async fn live_{}(", key.replace(['-', '.'], "_"))
}

#[tuff_core::watchdog]
#[test]
fn every_live_key_has_a_test_and_every_key_a_classification() {
    let source = include_str!("live_properties.rs");
    let settings = include_str!("../docs/settings.md");
    for property in PROPERTIES {
        match property.effect {
            Effect::Live => assert!(
                source.contains(&test_name(property.name)),
                "{} is live but has no live_ test",
                property.name
            ),
            Effect::Restart { reason } | Effect::Permanent { reason } => {
                assert!(!reason.is_empty(), "{}", property.name)
            }
            Effect::Unsupported { reason, todo } => assert!(
                !reason.is_empty() && std::path::Path::new(todo).is_file(),
                "{}",
                property.name
            ),
            Effect::Creation => {}
        }
        let row = format!("| `{}` | {} |", property.name, property.effect.name());
        assert!(settings.contains(&row), "docs/settings.md lacks the row {row}");
    }
    assert!(PROPERTIES.windows(2).all(|w| w[0].name < w[1].name));
}

#[tuff_core::watchdog]
#[tokio::test]
async fn live_accepts_transfers() {
    let server = start("").await;
    let mut conn = login(&server, Intent::Transfer).await;
    let reason = common::read_disconnect(&mut conn).await;
    assert_eq!(
        reason["translate"],
        "multiplayer.disconnect.transfers_disabled"
    );
    set(&server, "accepts-transfers", "true").await;
    let mut conn = login(&server, Intent::Transfer).await;
    let _: SetCompression = conn.expect().await.unwrap();
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_allow_flight() {
    let server = start("").await;
    let bot = bot(&server, "Flyer", &[]).await;
    set(&server, "allow-flight", "true").await;
    let [x, y, z] = bot.state().position;
    let hover = async |ticks: u32| {
        for _ in 0..ticks {
            bot.send(Serverbound::Move(tuff::protocol::play::Movement {
                position: Some([x, y + 4.0, z]),
                rotation: None,
                on_ground: false,
                horizontal_collision: false,
            }));
            bot.send(Serverbound::ClientTickEnd);
            tokio::time::sleep(Duration::from_millis(50)).await;
            if bot.state().disconnected.is_some() {
                return;
            }
        }
    };
    hover(120).await;
    assert_eq!(bot.state().disconnected, None);
    set(&server, "allow-flight", "false").await;
    hover(300).await;
    wait_kicked(&bot, "multiplayer.disconnect.flying").await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_bug_report_link() {
    let server = start("").await;
    let links = Config::ServerLinks.id();
    let before = bot(&server, "Before", &[]).await;
    assert!(!before.state().configuration.contains(&links));
    set(&server, "bug-report-link", "https://bugs.example/report").await;
    let after = bot(&server, "After", &[]).await;
    assert!(after.state().configuration.contains(&links));
    stop(server).await;
}

async fn spam(server: &Running, key: &str, packet: fn() -> Serverbound) {
    let bot = bot(server, "Spammer", &[]).await;
    for _ in 0..3 {
        bot.send(packet());
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(bot.state().disconnected, None);
    set(server, key, "1").await;
    for _ in 0..3 {
        bot.send(packet());
    }
    wait_kicked(&bot, "disconnect.spam").await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_chat_spam_threshold_seconds() {
    let server = start("").await;
    spam(&server, "chat-spam-threshold-seconds", || {
        Serverbound::Chat("hello".into())
    })
    .await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_command_spam_threshold_seconds() {
    let server = start("").await;
    spam(&server, "command-spam-threshold-seconds", || {
        Serverbound::ChatCommand("help".into())
    })
    .await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_difficulty() {
    let server = start("difficulty=easy\n").await;
    let bot = bot(&server, "Watcher", &[Id::ChangeDifficulty]).await;
    wait_packet(&bot, Id::ChangeDifficulty, |r| r.read_byte().unwrap() == 1).await;
    set(&server, "difficulty", "hard").await;
    wait_packet(&bot, Id::ChangeDifficulty, |r| r.read_byte().unwrap() == 3).await;
    let query = run(&server, "difficulty").await;
    assert!(query.to_lowercase().contains("hard"), "{query}");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_enable_code_of_conduct() {
    let server = start("").await;
    let refused = run(&server, "tuff config minecraft.enable-code-of-conduct true").await;
    assert!(refused.contains("Invalid value"), "{refused}");
    let folder = server.instance.path().join("codeofconduct");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("en_us.txt"), "Be kind.").unwrap();
    let conduct = Config::CodeOfConduct.id();
    let before = bot(&server, "Before", &[]).await;
    assert!(!before.state().configuration.contains(&conduct));
    set(&server, "enable-code-of-conduct", "true").await;
    let mut seen = false;
    for attempt in 0..50 {
        let after = bot(&server, &format!("After{attempt}"), &[]).await;
        if after.state().configuration.contains(&conduct) {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(seen, "the code of conduct was never shown");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn live_enable_status() {
    let server = start("").await;
    assert!(ping(server.addr).await["version"].is_object());
    set(&server, "enable-status", "false").await;
    let pinged =
        tokio::time::timeout(Duration::from_secs(5), tokio::spawn(ping(server.addr))).await;
    assert!(!matches!(pinged, Ok(Ok(_))), "status was served");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_enforce_whitelist() {
    let server = start("").await;
    let bot = bot(&server, "Outsider", &[]).await;
    set(&server, "white-list", "true").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(bot.state().disconnected, None);
    set(&server, "enforce-whitelist", "true").await;
    wait_kicked(&bot, "multiplayer.disconnect.not_whitelisted").await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_entity_broadcast_range_percentage() {
    let server = start("").await;
    let bot = bot(&server, "Watcher", &[]).await;
    let [x, y, z] = bot.state().position;
    let east = x + 20.0;
    run(
        &server,
        &format!("summon armor_stand {east} {} {z} {{NoGravity:1b}}", y + 2.0),
    )
    .await;
    let stand = || {
        bot.state()
            .entities
            .values()
            .any(|seen| seen.kind == Some(tuff_vanilla_data::registry::EntityType::ArmorStand))
    };
    eventually(async || stand()).await;
    set(&server, "entity-broadcast-range-percentage", "10").await;
    eventually(async || !stand()).await;
    set(&server, "entity-broadcast-range-percentage", "100").await;
    eventually(async || stand()).await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_max_chained_neighbor_updates() {
    let server = start("").await;
    assert_eq!(server.shared.live.max_chained_neighbor_updates(), 1_000_000);
    set(&server, "max-chained-neighbor-updates", "5").await;
    assert_eq!(server.shared.live.max_chained_neighbor_updates(), 5);
    set(&server, "max-chained-neighbor-updates", "-1").await;
    assert_eq!(server.shared.live.max_chained_neighbor_updates(), -1);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_force_gamemode() {
    let server = start("").await;
    let _bot = bot(&server, "Forced", &[]).await;
    set(&server, "gamemode", "creative").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        passes(
            &server,
            "execute if entity @a[name=Forced,gamemode=survival]"
        )
        .await
    );
    set(&server, "force-gamemode", "true").await;
    eventually(async || {
        passes(
            &server,
            "execute if entity @a[name=Forced,gamemode=creative]",
        )
        .await
    })
    .await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn defaultgamemode_command_sets_new_players_and_forces_when_enabled() {
    let server = start("").await;
    let _first = bot(&server, "First", &[]).await;
    run(&server, "defaultgamemode creative").await;
    let _second = bot(&server, "Second", &[]).await;
    assert!(
        passes(
            &server,
            "execute if entity @a[name=Second,gamemode=creative]"
        )
        .await
    );
    assert!(
        passes(
            &server,
            "execute if entity @a[name=First,gamemode=survival]"
        )
        .await
    );
    set(&server, "force-gamemode", "true").await;
    run(&server, "defaultgamemode adventure").await;
    eventually(async || {
        passes(
            &server,
            "execute if entity @a[name=First,gamemode=adventure]",
        )
        .await
    })
    .await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_function_permission_level() {
    let instance = TempDir::new();
    let pack = instance.path().join("world/datapacks/perm");
    std::fs::create_dir_all(pack.join("data/perm/function")).unwrap();
    let v = tuff_vanilla_data::version::DATA_PACK_VERSION;
    std::fs::write(
        pack.join("pack.mcmeta"),
        format!(
            r#"{{"pack":{{"description":"perm","min_format":[{0},{1}],"max_format":[{0},{1}]}}}}"#,
            v.major, v.minor
        ),
    )
    .unwrap();
    std::fs::write(
        pack.join("data/perm/function/strict.mcfunction"),
        "whitelist add Someone\n",
    )
    .unwrap();
    let server = start_in(instance, "", None, 1).await;
    let before = run(&server, "function perm:strict").await;
    assert!(before.contains("Unknown function"), "{before}");
    set(&server, "function-permission-level", "3").await;
    eventually(async || {
        !run(&server, "function perm:strict")
            .await
            .contains("Unknown function")
    })
    .await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_gamemode() {
    let server = start("").await;
    let _first = bot(&server, "First", &[]).await;
    assert!(
        passes(
            &server,
            "execute if entity @a[name=First,gamemode=survival]"
        )
        .await
    );
    set(&server, "gamemode", "creative").await;
    let _second = bot(&server, "Second", &[]).await;
    assert!(
        passes(
            &server,
            "execute if entity @a[name=Second,gamemode=creative]"
        )
        .await
    );
    assert!(
        passes(
            &server,
            "execute if entity @a[name=First,gamemode=survival]"
        )
        .await
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lock_difficulty_needs_an_operator_and_tells_everyone() {
    let server = start("difficulty=easy\n").await;
    let bot = bot(&server, "Locker", &[Id::ChangeDifficulty]).await;
    wait_packet(&bot, Id::ChangeDifficulty, |r| {
        r.read_byte().unwrap() == 1 && !r.read_bool().unwrap()
    })
    .await;
    bot.send(Serverbound::LockDifficulty(true));
    run(&server, "op Locker").await;
    let locked = wait_packet(&bot, Id::ChangeDifficulty, |r| {
        r.read_byte().unwrap() == 1 && r.read_bool().unwrap()
    });
    tokio::pin!(locked);
    loop {
        tokio::select! {
            () = &mut locked => break,
            () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                bot.send(Serverbound::LockDifficulty(true));
            }
        }
    }
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn change_difficulty_needs_an_operator() {
    let server = start("difficulty=easy\n").await;
    let bot = bot(&server, "Changer", &[Id::ChangeDifficulty]).await;
    wait_packet(&bot, Id::ChangeDifficulty, |r| r.read_byte().unwrap() == 1).await;
    bot.send(Serverbound::ChangeDifficulty(Difficulty::Hard));
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        run(&server, "difficulty")
            .await
            .to_lowercase()
            .contains("easy")
    );
    run(&server, "op Changer").await;
    let changed = wait_packet(&bot, Id::ChangeDifficulty, |r| r.read_byte().unwrap() == 3);
    tokio::pin!(changed);
    loop {
        tokio::select! {
            () = &mut changed => break,
            () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                bot.send(Serverbound::ChangeDifficulty(Difficulty::Hard));
            }
        }
    }
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_hardcore() {
    let server = start("").await;
    let bot = bot(&server, "Mortal", &[Id::ChangeDifficulty]).await;
    set(&server, "hardcore", "true").await;
    wait_packet(&bot, Id::ChangeDifficulty, |r| {
        r.read_byte().unwrap() == 3 && r.read_bool().unwrap()
    })
    .await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn live_hide_online_players() {
    let server = start("").await;
    assert!(ping(server.addr).await["players"].get("sample").is_some());
    set(&server, "hide-online-players", "true").await;
    eventually(async || ping(server.addr).await["players"].get("sample").is_none()).await;
    stop(server).await;
}

/// Opens a connection and drops it mid-handshake, returning its local port.
async fn abandoned_connection(server: &Running) -> u16 {
    let socket = tokio::net::TcpStream::connect(server.addr).await.unwrap();
    let port = socket.local_addr().unwrap().port();
    drop(socket);
    port
}

#[tuff_core::watchdog]
#[tokio::test]
async fn live_log_ips() {
    let mut logs = hub().subscribe().await.unwrap();
    let server = start("").await;
    let port = abandoned_connection(&server).await;
    let mut lines = Vec::new();
    eventually(async || {
        lines.extend(logged(&mut logs).await);
        lines
            .iter()
            .any(|l| l.contains(&format!("127.0.0.1:{port} disconnected")))
    })
    .await;
    set(&server, "log-ips", "false").await;
    let port = abandoned_connection(&server).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let lines = logged(&mut logs).await;
    assert!(
        !lines
            .iter()
            .any(|l| l.contains(&format!(":{port} disconnected"))),
        "{lines:?}"
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_max_players() {
    let server = start("").await;
    set(&server, "max-players", "0").await;
    eventually(async || ping(server.addr).await["players"]["max"] == 0).await;
    assert!(
        Bot::connect(server.addr, BotConfig::new("Late"))
            .await
            .is_err()
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_max_world_size() {
    let server = start("").await;
    let bot = bot(&server, "Edge", &[Id::InitializeBorder]).await;
    set(&server, "max-world-size", "1000").await;
    wait_packet(&bot, Id::InitializeBorder, |r| {
        for _ in 0..4 {
            r.read_double().unwrap();
        }
        r.read_var_long().unwrap();
        r.read_var_int().unwrap() == 1000
    })
    .await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn live_motd() {
    let server = start("motd=Before\n").await;
    eventually(async || ping(server.addr).await["description"]["text"] == "Before").await;
    set(&server, "motd", "After").await;
    eventually(async || ping(server.addr).await["description"]["text"] == "After").await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn live_network_compression_threshold() {
    let server = start("").await;
    set(&server, "network-compression-threshold", "64").await;
    let mut conn = login(&server, Intent::Login).await;
    let compression: SetCompression = conn.expect().await.unwrap();
    assert_eq!(compression.threshold, 64);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn live_op_permission_level() {
    let server = start("").await;
    set(&server, "op-permission-level", "2").await;
    run(&server, "op Helper").await;
    let file = server.instance.path().join("ops.json");
    eventually(async || {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        text.contains("Helper") && text.contains("\"level\": 2")
    })
    .await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn live_pause_when_empty_seconds() {
    let server = start("").await;
    let time = async || {
        let reply = run(&server, "time query gametime").await;
        reply
            .split_whitespace()
            .filter_map(|word| word.parse::<i64>().ok())
            .next_back()
            .unwrap()
    };
    set(&server, "pause-when-empty-seconds", "1").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let paused = time().await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(time().await, paused);
    set(&server, "pause-when-empty-seconds", "-1").await;
    eventually(async || time().await > paused).await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_player_idle_timeout() {
    let server = start("").await;
    let _bot = bot(&server, "Idler", &[]).await;
    assert_eq!(server.shared.live.network().idle_timeout, None);
    set(&server, "player-idle-timeout", "1").await;
    assert_eq!(
        server.shared.live.network().idle_timeout,
        Some(Duration::from_secs(60))
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn live_prevent_proxy_connections() {
    let (session, url) = MockSessionServer::start().await;
    let server = start_in(TempDir::new(), "", Some(&url), 1).await;
    let _ = encrypted_login(server.addr, "Notch", Some(&session)).await;
    eventually(async || session.joined_ips().len() == 1).await;
    set(&server, "prevent-proxy-connections", "true").await;
    let _ = encrypted_login(server.addr, "Notch", Some(&session)).await;
    eventually(async || session.joined_ips().len() == 2).await;
    assert_eq!(
        session.joined_ips(),
        vec![None, Some("127.0.0.1".to_owned())]
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn live_pvp() {
    let server = start("").await;
    assert!(run(&server, "gamerule pvp").await.contains("true"));
    set(&server, "pvp", "false").await;
    eventually(async || run(&server, "gamerule pvp").await.contains("false")).await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_rate_limit() {
    let server = start("").await;
    let bot = bot(&server, "Chatty", &[]).await;
    set(&server, "rate-limit", "5").await;
    for _ in 0..40 {
        bot.send(Serverbound::ClientTickEnd);
    }
    tokio::time::sleep(Duration::from_millis(1100)).await;
    bot.send(Serverbound::ClientTickEnd);
    wait_kicked(&bot, "disconnect.exceeded_packet_rate").await;
    stop(server).await;
}

/// Sets the pack keys one by one, `key` last, and returns what a player
/// joining after each step is offered at configuration.
async fn pack_offer(key: &str) -> (Option<tuff_bot::PackSeen>, tuff_bot::PackSeen) {
    let server = start("").await;
    let pack = [
        ("resource-pack", "http://packs.example/pack.zip"),
        ("resource-pack-id", "00000000-0000-0000-0000-000000000007"),
        (
            "resource-pack-sha1",
            "0123456789abcdef0123456789abcdef01234567",
        ),
        ("resource-pack-prompt", "{\"text\":\"Please\"}"),
        ("require-resource-pack", "true"),
    ];
    let mut before = None;
    for (name, value) in pack.iter().filter(|(name, _)| *name != key) {
        set(&server, name, value).await;
    }
    if key != "resource-pack" {
        let joined = bot(&server, "Before", &[]).await;
        before = joined.state().pack_offers.first().cloned();
    }
    let value = pack.iter().find(|(name, _)| *name == key).unwrap().1;
    set(&server, key, value).await;
    let joined = bot(&server, "After", &[]).await;
    let after = joined.state().pack_offers.first().cloned().unwrap();
    stop(server).await;
    (before, after)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_resource_pack() {
    let (before, after) = pack_offer("resource-pack").await;
    assert_eq!(before, None);
    assert_eq!(after.url, "http://packs.example/pack.zip");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_resource_pack_id() {
    let (before, after) = pack_offer("resource-pack-id").await;
    assert_ne!(before.unwrap().id, 7);
    assert_eq!(after.id, 7);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_resource_pack_sha1() {
    let (before, after) = pack_offer("resource-pack-sha1").await;
    assert_eq!(before.unwrap().hash, "");
    assert_eq!(after.hash, "0123456789abcdef0123456789abcdef01234567");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_resource_pack_prompt() {
    let (before, after) = pack_offer("resource-pack-prompt").await;
    assert!(!before.unwrap().prompted);
    assert!(after.prompted);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_require_resource_pack() {
    let (before, after) = pack_offer("require-resource-pack").await;
    assert!(!before.unwrap().required);
    assert!(after.required);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_simulation_distance() {
    let server = start("").await;
    let bot = bot(&server, "Ticker", &[Id::SetSimulationDistance]).await;
    set(&server, "simulation-distance", "6").await;
    wait_packet(&bot, Id::SetSimulationDistance, |r| {
        r.read_var_int().unwrap() == 6
    })
    .await;
    assert_eq!(server.shared.live.simulation_distance(), 6);
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_spawn_protection() {
    let server = start("").await;
    let bot = bot(&server, "Builder", &[]).await;
    run(&server, "op Owner").await;
    run(&server, "gamemode creative Builder").await;
    let [x, y, z] = bot.state().position;
    let below =
        tuff_world_model::BlockPos::new(x.floor() as i32, y.floor() as i32 - 1, z.floor() as i32);
    let present = format!(
        "execute unless block {} {} {} minecraft:air",
        below.x, below.y, below.z
    );
    assert!(passes(&server, &present).await);
    let dig = |sequence| {
        bot.send(Serverbound::PlayerAction {
            status: tuff::protocol::play::PlayerActionStatus::StartDestroyBlock,
            position: below,
            face: 1,
            sequence,
        });
    };
    dig(1);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(passes(&server, &present).await, "broke a protected block");
    set(&server, "spawn-protection", "0").await;
    dig(2);
    eventually(async || !passes(&server, &present).await).await;
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_view_distance() {
    let server = start("").await;
    let bot = Bot::connect(
        server.addr,
        BotConfig {
            view_distance: 10,
            keep: vec![Id::SetChunkCacheRadius.id()],
            ..BotConfig::new("Viewer")
        },
    )
    .await
    .unwrap();
    within(bot.wait_for(|state| state.chunks > 0))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let near = bot.state().loaded.len();
    set(&server, "view-distance", "4").await;
    wait_packet(&bot, Id::SetChunkCacheRadius, |r| {
        r.read_var_int().unwrap() == 4
    })
    .await;
    within(bot.wait_for(|state| state.loaded.len() > near))
        .await
        .unwrap();
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_white_list() {
    let server = start("").await;
    let _member = bot(&server, "Member", &[]).await;
    set(&server, "white-list", "true").await;
    assert!(
        Bot::connect(server.addr, BotConfig::new("Stranger"))
            .await
            .is_err()
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn creation_keys_note_an_existing_world_but_not_a_new_one() {
    let mut logs = hub().subscribe().await.unwrap();
    let note = "the current world keeps seed 777001";
    let server = start_in(TempDir::new(), "level-seed=12345\n", None, 777_001).await;
    assert_eq!(server.shared.seed, 777_001);
    assert!(
        !logged(&mut logs).await.iter().any(|l| l.contains(note)),
        "a new world got the creation note"
    );
    let reply = set(&server, "level-seed", "999").await;
    assert!(reply.contains(note), "{reply}");
    let reply = set(&server, "generate-structures", "false").await;
    assert!(
        reply.contains("only applies when a new world is created"),
        "{reply}"
    );
    let reply = set(&server, "motd", "x").await;
    assert!(!reply.contains("new world"), "{reply}");
    let instance = stop(server).await;
    let server = start_in(instance, "level-seed=12345\n", None, 1).await;
    assert_eq!(server.shared.seed, 777_001);
    assert!(
        logged(&mut logs).await.iter().any(|l| l.contains(note)),
        "the existing world got no creation note"
    );
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn restart_and_unsupported_keys_say_so() {
    let server = start("").await;
    let reply = set(&server, "server-port", "25599").await;
    assert!(
        reply.contains("needs a restart: the game listener is bound"),
        "{reply}"
    );
    let reply = set(&server, "enable-rcon", "true").await;
    assert!(reply.contains("not planned"), "{reply}");
    let reply = run(&server, "tuff config minecraft.online-mode").await;
    assert!(reply.contains("needs a restart"), "{reply}");
    stop(server).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_broadcast_console_to_ops() {
    let server = start("").await;
    let op = bot(&server, "Helper", &[]).await;
    run(&server, "op Helper").await;
    let chats = |b: &Bot| {
        b.state()
            .received
            .get(&Id::SystemChat.id())
            .copied()
            .unwrap_or(0)
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = chats(&op);
    run(&server, "time set day").await;
    within(op.wait_for(|s| s.received.get(&Id::SystemChat.id()).copied().unwrap_or(0) > before))
        .await
        .unwrap();
    set(&server, "broadcast-console-to-ops", "false").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = chats(&op);
    run(&server, "time set night").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(chats(&op), before);
    stop(server).await;
}
