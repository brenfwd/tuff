//! Runtime WebAssembly plugins: loading from the plugin directory and
//! with `/tuff plugin`, commands, events, timers and per-player state
//! through the bridges, faults, dependencies, conflicts, reloads, and that
//! nothing a plugin registered survives its unload. The guests are
//! hand-written WAT against the plugin WIT world.

use super::*;
use crate::auth::GameProfile;
use crate::config::Value;
use crate::protocol::configuration::ClientInformation;
use crate::protocol::play::Serverbound;
use crate::world::session::PlayerSession;
use crate::world::session::hooks::{PacketInterceptor, SessionHooks, Verdict};
use tuff_plugin_wasm::sandbox::Player;
use tuff_plugin_wasm::wit::bindings::types::{
    BlockPos as WitPos, ClickFilter, ClickKind, CommandNode, CommandTree, EventKinds, NodeKind,
    Parser, SetFlags, TextKind, Vec3 as WitVec3,
};
use tuff_plugin_wasm::wit::canon::Value as WitValue;
use tuff_plugin_wasm::wit::testing::{self, Data, RET, export, ok_code, ok_unit};
use tuff_plugin_wasm::wit::value::ToValue;
use tuff_plugin_wasm::{Limits, WasmHost, WasmPluginsFeature, abi, bridge, manifest};
use tuff_storage::admin::PermissionLevel;

mod combat;
mod content;
mod dimensions;
mod guard;
mod inventory;
mod items;
mod manifest_conflicts;
mod menus;
mod messaging;
mod panel;
mod payload;
mod portal_hooks;
mod relay_e2e;
mod routes;
mod settings;
mod state;
mod wasm_net;

const GREETER: &str = include_str!("../../crates/tuff-plugin-wasm/examples/greeter.wat");

fn greeter(version: &str) -> Vec<u8> {
    let text = GREETER.replace("version = 1.0.0", &format!("version = {version}"));
    wat::parse_str(text).unwrap()
}

/// Adds `cap.<group> = all` for every group whose host calls `body` makes.
fn with_group_requests(manifest: &str, body: &str) -> String {
    use tuff_plugin_wasm::caps::{Kind, classes::HOST_CALL_GROUPS};
    if manifest.contains("cap.") {
        return manifest.to_owned();
    }
    let mut out = manifest.to_owned();
    for kind in [
        Kind::WorldWrite,
        Kind::PlayerWrite,
        Kind::PlayerRead,
        Kind::CrossPlugin,
        Kind::Panel,
    ] {
        let used = HOST_CALL_GROUPS.iter().any(|(name, group)| {
            *group == Some(kind) && {
                let call = format!("call ${name}");
                body.match_indices(&call).any(|(at, _)| {
                    !body[at + call.len()..]
                        .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '-')
                })
            }
        });
        if used {
            out.push_str(&format!("\ncap.{} = all", kind.name()));
        }
    }
    out
}

fn module(manifest: &str, body: &str) -> Vec<u8> {
    let manifest = &with_group_requests(manifest, body);
    let text = testing::module(&format!(
        "{}
{body}",
        testing::TEXT
    ));
    let mut bytes = wat::parse_str(text).unwrap();
    bytes.extend(manifest::custom_section(
        manifest::SECTION,
        manifest.as_bytes(),
    ));
    bytes
}

fn plain(id: &str) -> String {
    format!("id = {id}\nversion = 1.0.0\napi = 0")
}

fn s(text: &str) -> WitValue<'_> {
    WitValue::String(text.into())
}

/// `name`, taking the rest of the line, running `handler`.
fn line_tree(name: &str, handler: u32) -> CommandTree {
    let node = |name: &str, kind, children: Vec<u16>| CommandNode {
        name: name.into(),
        kind,
        children,
        executes: Some(handler),
        redirect: None,
        permission: None,
        suggest: false,
    };
    CommandTree {
        nodes: vec![
            node(name, NodeKind::Literal, vec![1]),
            node(
                "args",
                NodeKind::Argument(Parser::Text(TextKind::Greedy)),
                vec![],
            ),
        ],
    }
}

/// A `register` that claims `commands` (handlers 0, 1, ... in order),
/// then runs `more`.
fn register(data: &mut Data, commands: &[&str], more: &str) -> String {
    let claims: String = (0..)
        .zip(commands)
        .map(|(handler, name)| {
            data.call("register-command", &[line_tree(name, handler).to_value()])
        })
        .collect();
    format!(
        "(func {} {claims} {more} {})",
        export("register"),
        ok_unit()
    )
}

/// `(i32.const ptr) (i32.const len)` of `text` placed in `data`.
fn text(data: &mut Data, text: &str) -> String {
    let (ptr, len) = data.string(text);
    format!("(i32.const {ptr}) (i32.const {len})")
}

/// Broadcasts the text built so far.
const SHOUT: &str = "(call $broadcast (i32.const 512) (call $text_len) (i32.const 256))";

/// Replies the text built so far.
const SAY: &str = "(call $reply (i32.const 512) (call $text_len) (i32.const 256))";

/// A plugin `id` owning command `command`, which traps when run.
fn crasher(id: &str, command: &str, depends: &str) -> Vec<u8> {
    let mut data = Data::new();
    let register = register(&mut data, &[command], "");
    module(
        &format!("id = {id}\nversion = 1.0.0\napi = 0\ndepends = {depends}"),
        &format!(
            "{register} (func {} unreachable) {}",
            export("run-command"),
            data.wat()
        ),
    )
}

/// A plugin `id` owning command `command` that loops for ever.
fn spinner(id: &str, command: &str) -> Vec<u8> {
    let mut data = Data::new();
    let register = register(&mut data, &[command], "");
    module(
        &plain(id),
        &format!(
            "{register} (func {} (loop $forever (br $forever)) (i32.const 0)) {}",
            export("run-command"),
            data.wat()
        ),
    )
}

fn plugins_dir(dir: &Path) -> std::path::PathBuf {
    let plugins = dir.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    plugins
}

fn install(dir: &Path, file: &str, bytes: &[u8]) {
    std::fs::write(plugins_dir(dir).join(file), bytes).unwrap();
}

/// The group grants the installed plugins asked for, as (id, grant body).
fn group_grants(dir: &Path) -> Vec<(String, String)> {
    let groups = [
        "world-write",
        "player-write",
        "player-read",
        "cross-plugin",
        "panel",
    ];
    let mut out = Vec::new();
    for entry in std::fs::read_dir(plugins_dir(dir)).unwrap().flatten() {
        let Ok(manifest) = std::fs::read(entry.path())
            .map_err(drop)
            .and_then(|bytes| manifest::read(&bytes).map_err(drop))
        else {
            continue;
        };
        let body: String = manifest
            .capabilities
            .iter()
            .map(|request| request.entry.kind.name())
            .filter(|name| groups.contains(name))
            .map(|name| format!("{name} = all\n"))
            .collect();
        if !body.is_empty() {
            out.push((manifest.id, body));
        }
    }
    out
}

/// A world whose host has loaded the plugin directory.
async fn start(dir: &Path) -> TestWorld {
    let grants = group_grants(dir);
    if !grants.is_empty() {
        let backend = open_sqlite(dir);
        for (id, body) in grants {
            backend
                .config()
                .expect("sqlite keeps the config")
                .write_plugin_grant(&id, Some(body.trim()))
                .unwrap();
        }
        return start_sqlite(dir, backend).await;
    }
    let plugin = WasmPluginsFeature {
        directory: Some(plugins_dir(dir)),
        limits: Limits {
            fuel: 2_000_000,
            ..Limits::default()
        },
    };
    let world = TestWorld::start_with_features(dir, vec![Box::new(plugin)]);
    host(&world).sync().await;
    world
}

fn host(world: &TestWorld) -> &WasmHost {
    world
        .shared
        .services
        .get::<WasmHost>()
        .expect("the host runs")
}

async fn console(world: &TestWorld, line: &str) -> Vec<String> {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(|line| line.plain())
        .collect()
}

async fn announced(world: &TestWorld) -> Vec<String> {
    host(world).sync().await;
    world
        .announced()
        .await
        .iter()
        .map(|line| line.plain())
        .collect()
}

async fn count(world: &TestWorld, text: &str) -> usize {
    announced(world)
        .await
        .iter()
        .filter(|line| *line == text)
        .count()
}

async fn loaded(world: &TestWorld) -> Vec<(String, String)> {
    host(world)
        .list()
        .await
        .into_iter()
        .map(|plugin| (plugin.id, plugin.version.to_string()))
        .collect()
}

struct Client {
    session: PlayerSession,
    frames: mpsc::UnboundedReceiver<bytes::Bytes>,
}

impl Client {
    fn new(world: &TestWorld) -> Client {
        Client::at(world, PermissionLevel::All)
    }

    fn at(world: &TestWorld, level: PermissionLevel) -> Client {
        let (outbound, frames) = mpsc::unbounded_channel();
        let (handle, events) = PlayerHandle::new(7);
        let (_packets, packets) = mpsc::channel(8);
        let session = PlayerSession::new(
            world.shared.clone(),
            7,
            GameProfile {
                id: 77,
                name: "Steve".to_owned(),
                properties: Vec::new(),
            },
            ClientInformation::default(),
            outbound,
            handle,
            events,
            packets,
            level,
        );
        Client { session, frames }
    }

    /// Runs `/line` through the bridge; the verdict and every frame sent.
    async fn run(&mut self, line: &str) -> (Verdict, Vec<bytes::Bytes>) {
        let verdict = bridge::Commands
            .intercept(&mut self.session, &Serverbound::ChatCommand(line.into()))
            .await;
        let mut frames = Vec::new();
        while let Ok(frame) = self.frames.try_recv() {
            frames.push(frame);
        }
        (verdict, frames)
    }

    /// Whether `/line`'s answer mentions each of `texts`, in order.
    async fn said(&mut self, line: &str, texts: &[&str]) -> Vec<bool> {
        let (_, frames) = self.run(line).await;
        texts
            .iter()
            .map(|text| {
                frames
                    .iter()
                    .any(|frame| frame.windows(text.len()).any(|w| w == text.as_bytes()))
            })
            .collect()
    }

    /// Whether `/line` was taken by a plugin that answered with `text`.
    async fn answered(&mut self, line: &str, text: &str) -> bool {
        let (verdict, frames) = self.run(line).await;
        verdict == Verdict::Cancel
            && frames
                .iter()
                .any(|frame| frame.windows(text.len()).any(|w| w == text.as_bytes()))
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plugin_loads_runs_and_unloads_without_a_trace() {
    let dir = TestDir::new();
    install(dir.path(), "greeter.wasm", &greeter("1.0.0"));
    let world = start(dir.path()).await;
    let mut steve = Client::new(&world);

    assert_eq!(loaded(&world).await, [("greeter".into(), "1.0.0".into())]);
    assert_eq!(count(&world, "greeter is ready").await, 1);
    assert!(host(&world).claims("greet"));

    assert!(steve.answered("greet", "Hello! Nice to meet you.").await);
    assert!(steve.answered("greet", "Welcome back!").await);
    assert_eq!(steve.run("time set day").await.0, Verdict::Allow);

    bridge::Sessions.joined(&mut steve.session).await;
    assert_eq!(count(&world, "Steve joined; say hello!").await, 1);

    const REMINDER: &str = "Reminder: say hi to someone today.";
    assert!(steve.answered("greet later", "remind everyone").await);
    world.ticks(99).await;
    assert_eq!(count(&world, REMINDER).await, 0);
    world.ticks(1).await;
    assert_eq!(count(&world, REMINDER).await, 1);

    assert!(steve.answered("greet later", "remind everyone").await);
    assert_eq!(
        console(&world, "tuff plugin unload greeter").await,
        ["Unloaded greeter"]
    );
    assert_eq!(count(&world, "greeter is shutting down").await, 1);
    assert!(loaded(&world).await.is_empty());
    assert!(!host(&world).claims("greet"));
    assert!(!host(&world).listens(EventKinds::JOIN));
    assert_eq!(steve.run("greet").await.0, Verdict::Allow);
    let player = Player::new(77, "Steve");
    assert_eq!(host(&world).command(player, "greet".into()).await, None);
    bridge::Sessions.joined(&mut steve.session).await;
    world.ticks(200).await;
    assert_eq!(
        count(&world, REMINDER).await,
        1,
        "a dead plugin's timer fired"
    );
    assert_eq!(count(&world, "Steve joined; say hello!").await, 1);

    assert_eq!(
        console(&world, "tuff plugin load greeter").await,
        ["Loaded greeter 1.0.0"]
    );
    assert!(
        steve.answered("greet", "Hello! Nice to meet you.").await,
        "per-player state outlived the unload"
    );
    world.ticks(200).await;
    assert_eq!(count(&world, REMINDER).await, 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn leaving_drops_a_players_state() {
    let dir = TestDir::new();
    install(dir.path(), "greeter.wasm", &greeter("1.0.0"));
    let world = start(dir.path()).await;
    let mut steve = Client::new(&world);
    assert!(steve.answered("greet", "Hello! Nice to meet you.").await);
    assert!(steve.answered("greet", "Welcome back!").await);
    bridge::Sessions.leaving(&mut steve.session).await;
    host(&world).sync().await;
    assert!(steve.answered("greet", "Hello! Nice to meet you.").await);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn faulting_plugins_are_unloaded_and_the_server_carries_on() {
    let dir = TestDir::new();
    install(dir.path(), "greeter.wasm", &greeter("1.0.0"));
    install(dir.path(), "crasher.wasm", &crasher("crasher", "boom", ""));
    install(
        dir.path(),
        "leaning.wasm",
        &crasher("leaning", "lean", "crasher"),
    );
    install(dir.path(), "spinner.wasm", &spinner("spinner", "spin"));
    let world = start(dir.path()).await;
    let mut steve = Client::new(&world);
    let ids = |plugins: Vec<(String, String)>| -> Vec<String> {
        plugins.into_iter().map(|(id, _)| id).collect()
    };
    assert_eq!(
        ids(loaded(&world).await),
        ["crasher", "greeter", "leaning", "spinner"]
    );

    assert!(
        steve
            .answered("boom", "crasher failed and was unloaded")
            .await
    );
    assert_eq!(ids(loaded(&world).await), ["greeter", "spinner"]);
    assert!(!host(&world).claims("boom") && !host(&world).claims("lean"));

    assert!(
        steve
            .answered("spin", "spinner failed and was unloaded")
            .await
    );
    assert_eq!(ids(loaded(&world).await), ["greeter"]);
    assert!(steve.answered("greet", "Hello! Nice to meet you.").await);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dependencies_versions_and_conflicts_are_checked() {
    let dir = TestDir::new();
    let world = start(dir.path()).await;
    let mut steve = Client::new(&world);
    install(
        dir.path(),
        "addon.wasm",
        &crasher("addon", "addon", "greeter >= 1.0.0"),
    );
    install(dir.path(), "greeter.wasm", &greeter("1.0.0"));

    let refused = console(&world, "tuff plugin load addon").await;
    assert!(
        refused[0].contains("addon needs greeter >= 1.0.0, which is not loaded"),
        "{refused:?}"
    );
    assert_eq!(
        console(&world, "tuff plugin load greeter.wasm").await,
        ["Loaded greeter 1.0.0"]
    );
    assert_eq!(
        console(&world, "tuff plugin load addon").await,
        ["Loaded addon 1.0.0"]
    );
    let refused = console(&world, "tuff plugin unload greeter").await;
    assert!(
        refused[0].contains("addon depend on greeter"),
        "{refused:?}"
    );

    install(dir.path(), "greeter.wasm", &greeter("0.9.0"));
    let refused = console(&world, "tuff plugin reload greeter").await;
    assert!(
        refused[0].contains("addon needs greeter >= 1.0.0"),
        "{refused:?}"
    );
    assert!(
        loaded(&world)
            .await
            .contains(&("greeter".into(), "1.0.0".into()))
    );
    assert!(steve.answered("greet", "Hello! Nice to meet you.").await);

    install(dir.path(), "greeter.wasm", &greeter("1.1.0"));
    assert_eq!(
        console(&world, "tuff plugin reload greeter").await,
        ["Reloaded greeter 1.0.0 -> 1.1.0"]
    );
    assert_eq!(count(&world, "greeter is shutting down").await, 1);
    assert_eq!(count(&world, "greeter is ready").await, 2);
    assert!(steve.answered("greet", "Hello! Nice to meet you.").await);

    for (file, bytes, why) in [
        (
            "future.wasm",
            module(
                "id = future\nversion = 1.0.0\napi = 99",
                &format!("(func {} {})", export("register"), ok_unit()),
            ),
            "future needs plugin API 99",
        ),
        (
            "timely.wasm",
            crasher("timely", "time", ""),
            "timely claims /time, which the server already has",
        ),
        (
            "copycat.wasm",
            crasher("copycat", "greet", ""),
            "copycat claims /greet, which greeter already has",
        ),
        (
            "builtin.wasm",
            crasher("vanilla-mobs", "mobby", ""),
            "vanilla-mobs is the id of a built-in plugin",
        ),
        (
            "nomanifest.wasm",
            wat::parse_str(testing::module(&format!(
                "(func {} {})",
                export("register"),
                ok_unit()
            )))
            .unwrap(),
            "no \"tuff-plugin\" custom section",
        ),
        (
            "garbage.wasm",
            b"\0asm\x01\0\0\0\x01\x05garbage".to_vec(),
            "truncated",
        ),
    ] {
        install(dir.path(), file, &bytes);
        let refused = console(&world, &format!("tuff plugin load {file}")).await;
        assert!(refused[0].contains(why), "{file}: {refused:?}");
    }
    for outside in ["../outside", "/etc/hosts", "sub/dir"] {
        let refused = host(&world).load(outside).await.unwrap_err();
        assert!(refused.contains("not a file name"), "{refused}");
    }

    assert_eq!(
        loaded(&world).await,
        [
            ("addon".into(), "1.0.0".into()),
            ("greeter".into(), "1.1.0".into())
        ]
    );
    for command in ["time", "mobby"] {
        assert!(!host(&world).claims(command), "{command}");
    }
    let listed = console(&world, "tuff plugin list").await;
    assert_eq!(listed[0], "2 runtime plugins loaded");
    assert!(listed[2].starts_with("greeter 1.1.0 /greet"), "{listed:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_setting_turns_runtime_plugins_off_and_on() {
    let dir = TestDir::new();
    install(dir.path(), "greeter.wasm", &greeter("1.0.0"));
    let world = start(dir.path()).await;
    let mut steve = Client::new(&world);
    assert_eq!(loaded(&world).await.len(), 1);

    world
        .shared
        .config
        .set(tuff_plugin_wasm::ENABLED_KEY, Value::Bool(false))
        .unwrap();
    host(&world).sync().await;
    assert!(loaded(&world).await.is_empty());
    assert_eq!(count(&world, "greeter is shutting down").await, 1);
    assert_eq!(steve.run("greet").await.0, Verdict::Allow);
    let refused = console(&world, "tuff plugin load greeter").await;
    assert!(refused[0].contains("turned off"), "{refused:?}");

    world
        .shared
        .config
        .set(tuff_plugin_wasm::ENABLED_KEY, Value::Bool(true))
        .unwrap();
    host(&world).sync().await;
    assert_eq!(loaded(&world).await.len(), 1);
    assert!(steve.answered("greet", "Hello! Nice to meet you.").await);
    world.shutdown().await;
}

async fn behaves_like_the_greeter(id: &str, bytes: &[u8]) {
    let dir = TestDir::new();
    install(dir.path(), &format!("{id}.wasm"), bytes);
    let world = start(dir.path()).await;
    let mut steve = Client::new(&world);
    assert_eq!(
        loaded(&world).await,
        [(id.to_owned(), "1.0.0".to_owned())],
        "{id}"
    );
    assert_eq!(count(&world, &format!("{id} is ready")).await, 1, "{id}");
    assert!(
        steve.answered("greet", "Hello! Nice to meet you.").await,
        "{id}"
    );
    assert!(steve.answered("greet", "Welcome back!").await, "{id}");
    bridge::Sessions.joined(&mut steve.session).await;
    assert_eq!(count(&world, "Steve joined; say hello!").await, 1, "{id}");
    assert!(
        steve.answered("greet later", "remind everyone").await,
        "{id}"
    );
    world.ticks(100).await;
    assert_eq!(
        count(&world, "Reminder: say hi to someone today.").await,
        1,
        "{id}"
    );
    host(&world).unload(id).await.unwrap();
    assert_eq!(
        count(&world, &format!("{id} is shutting down")).await,
        1,
        "{id}"
    );
    assert!(!host(&world).claims("greet"), "{id}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_wat_greeter_behaves_as_documented() {
    behaves_like_the_greeter("greeter", &greeter("1.0.0")).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_commands_are_in_the_client_tree_until_unloaded() {
    let dir = TestDir::new();
    install(dir.path(), "greeter.wasm", &greeter("1.0.0"));
    let world = start(dir.path()).await;
    let mut steve = Client::new(&world);
    let has = |frames: &[bytes::Bytes], text: &str| {
        frames.iter().any(|frame| {
            let mut payload = tuff_test_support::framed_payload(frame);
            let bytes = payload.read_bytes(payload.remaining()).unwrap();
            bytes.windows(text.len()).any(|w| w == text.as_bytes())
        })
    };

    let mut changes = world.shared.commands.client_roots().subscribe();
    steve.session.send_permission_level();
    let (_, frames) = steve.run("time set day").await;
    assert!(has(&frames, "greet"));

    changes.mark_unchanged();
    console(&world, "tuff plugin unload greeter").await;
    assert!(changes.has_changed().unwrap());
    steve.session.send_permission_level();
    let (_, frames) = steve.run("time set day").await;
    assert!(!has(&frames, "greet"));
}

/// `/perm` replies which of the nodes it declares (unless `declare` is
/// off) the sender holds.
fn perms(version: &str, declare: bool) -> Vec<u8> {
    let mut data = Data::new();
    let declarations = match declare {
        true => {
            data.call("declare-permission", &[s("perm.use"), WitValue::U8(0)])
                + &data.call("declare-permission", &[s("perm.admin"), WitValue::U8(2)])
        }
        false => String::new(),
    };
    let register = register(&mut data, &["perm"], &declarations);
    let check = |data: &mut Data, node: &str, answer: &str| {
        format!(
            "{} (if (i32.and (i32.load8_u (i32.const {RET})) (i32.load8_u (i32.const {})))
               (then (call $reply {} (i32.const {RET}))))",
            data.call("has-permission", &[s(node)]),
            RET + 1,
            text(data, answer)
        )
    };
    let body = format!(
        "{register} (func {} {} {} {}) {}",
        export("run-command"),
        check(&mut data, "perm.use", "use:yes"),
        check(&mut data, "perm.admin", "admin:yes"),
        ok_code("(i32.const 1)"),
        data.wat()
    );
    module(&format!("id = perm\nversion = {version}\napi = 0"), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declared_permissions_are_checked_and_gone_after_unload() {
    let dir = TestDir::new();
    install(dir.path(), "perm.wasm", &perms("1.0.0", true));
    let world = start(dir.path()).await;
    let mut steve = Client::new(&world);
    let mut gamemaster = Client::at(&world, PermissionLevel::Gamemasters);
    let summary = host(&world).list().await;
    assert_eq!(summary[0].permissions, ["perm.use", "perm.admin"]);

    assert_eq!(
        steve.said("perm", &["use:yes", "admin:yes"]).await,
        [true, false]
    );
    assert_eq!(
        gamemaster.said("perm", &["use:yes", "admin:yes"]).await,
        [true, true]
    );
    let permissions = &world.shared.permissions;
    permissions
        .set(77, "Steve", "perm.admin", Some(true))
        .await
        .unwrap();
    permissions
        .set(77, "Steve", "perm.use", Some(false))
        .await
        .unwrap();
    assert_eq!(
        steve.said("perm", &["use:yes", "admin:yes"]).await,
        [false, true]
    );
    permissions
        .set(77, "Steve", "perm.admin", None)
        .await
        .unwrap();
    permissions
        .set(77, "Steve", "perm.use", None)
        .await
        .unwrap();

    install(dir.path(), "perm.wasm", &perms("1.1.0", false));
    assert_eq!(
        console(&world, "tuff plugin reload perm").await,
        ["Reloaded perm 1.0.0 -> 1.1.0"]
    );
    assert!(host(&world).list().await[0].permissions.is_empty());
    assert_eq!(
        steve.said("perm", &["use:yes", "admin:yes"]).await,
        [false, false],
        "a node the old version declared outlived it"
    );
    assert_eq!(
        console(&world, "tuff plugin unload perm").await,
        ["Unloaded perm"]
    );
    world.shutdown().await;
}

/// `/count` counts in the plugin's store and replies `count = <n>`.
fn counter() -> Vec<u8> {
    let mut data = Data::new();
    let register = register(&mut data, &["count"], "");
    let get = data.call("storage-get", &[s("n")]);
    let key = data.string("n");
    let (said, _) = data.string("count = ?");
    let body = format!(
        r#"{register}
        (func {run} (local $n i32)
          {get}
          (if (i32.load8_u (i32.const {RET}))
            (then (local.set $n (i32.load8_u (i32.load (i32.const {list}))))))
          (local.set $n (i32.add (local.get $n) (i32.const 1)))
          (i32.store8 (i32.const 16) (local.get $n))
          (call $storage-set (i32.const {k}) (i32.const {k_len}) (i32.const 1) (i32.const 16) (i32.const 1) (i32.const {RET}))
          (i32.store8 (i32.const {digit}) (i32.add (i32.const 48) (local.get $n)))
          (call $reply (i32.const {said}) (i32.const 9) (i32.const {RET}))
          {code})
        {data}"#,
        run = export("run-command"),
        list = RET + 4,
        k = key.0,
        k_len = key.1,
        digit = said + 8,
        code = ok_code("(local.get $n)"),
        data = data.wat(),
    );
    module(&plain("counter"), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stored_data_outlives_unloads_and_restarts_until_purged() {
    let dir = TestDir::new();
    install(dir.path(), "counter.wasm", &counter());
    let file = plugins_dir(dir.path()).join("counter").join("storage.bin");
    let world = start(dir.path()).await;
    let mut steve = Client::new(&world);
    assert!(steve.answered("count", "count = 1").await);
    assert!(steve.answered("count", "count = 2").await);
    let stored = host(&world).stored("counter").await.unwrap();
    assert_eq!(stored.get("n"), Some(&[2][..]));

    assert_eq!(
        console(&world, "tuff plugin unload counter").await,
        ["Unloaded counter"]
    );
    assert!(file.exists(), "unloading did not write the store");
    console(&world, "tuff plugin load counter").await;
    assert!(steve.answered("count", "count = 3").await);
    world.shared.services.flush().await;
    world.shutdown().await;

    let world = start(dir.path()).await;
    let mut steve = Client::new(&world);
    assert!(steve.answered("count", "count = 4").await);
    assert_eq!(
        console(&world, "tuff plugin purge counter").await,
        ["Deleted the data counter stored"]
    );
    assert!(!file.exists());
    assert!(steve.answered("count", "count = 1").await);
    console(&world, "tuff plugin unload counter").await;
    assert!(file.exists());
    assert_eq!(
        console(&world, "tuff plugin purge counter").await,
        ["Deleted the data counter stored"]
    );
    assert!(!file.exists());
    assert_eq!(host(&world).stored("counter").await, None);
    assert_eq!(
        console(&world, "tuff plugin purge counter").await,
        ["counter has no stored data"]
    );

    std::fs::write(&file, b"not a store").unwrap();
    let refused = console(&world, "tuff plugin load counter").await;
    assert!(refused[0].contains("cannot be read"), "{refused:?}");
    assert!(loaded(&world).await.is_empty());
    world.shutdown().await;
}

fn pos(x: i32, y: i32, z: i32) -> WitValue<'static> {
    WitPos { x, y, z }.to_value().into_owned()
}

/// The world error case at `RET + 1`, if the call at `RET` failed, as an
/// `i32` expression: -1 when it succeeded.
fn world_error() -> String {
    format!(
        "(if (result i32) (i32.load8_u (i32.const {RET})) (then (i32.load8_u (i32.const {}))) (else (i32.const -1)))",
        RET + 1
    )
}

/// `/build` places blocks, `/peek` replies one, `/box` reads two; block
/// events are broadcast as `x y z dimension state`.
fn builder() -> Vec<u8> {
    let mut data = Data::new();
    let kinds = EventKinds::BLOCK_BREAK | EventKinds::BLOCK_PLACE;
    let subscribe = data.call("subscribe", &[kinds.to_value()]);
    let register = register(&mut data, &["build", "peek", "box"], &subscribe);
    let none = SetFlags::empty().to_value().into_owned();
    let quiet = SetFlags::NO_UPDATES.to_value().into_owned();
    let set_stone = data.call(
        "block-set",
        &[s("overworld"), pos(0, 100, 0), s("stone"), none.clone()],
    );
    let set_stairs = data.call(
        "block-set",
        &[
            s("minecraft:overworld"),
            pos(1, 100, 0),
            s("oak_stairs[facing=east]"),
            quiet,
        ],
    );
    let set_bad = data.call(
        "block-set",
        &[s("overworld"), pos(2, 100, 0), s("not_a_block"), none],
    );
    let peek = data.call("block-get", &[s("overworld"), pos(1, 100, 0)]);
    let peek_far = data.call("block-get", &[s("overworld"), pos(1, 100_000, 0)]);
    let boxed = data.call(
        "blocks-get",
        &[s("overworld"), pos(1, 100, 0), pos(0, 100, 0)],
    );
    let ok = format!("(if (i32.load8_u (i32.const {RET})) (then unreachable))");
    let body = format!(
        r#"{register}
        (func {run} (param $handler i32) (param i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)
          (if (i32.eqz (local.get $handler))
            (then {set_stone} {ok} {set_stairs} {ok} {set_bad} (return {build})))
          (if (i32.eq (local.get $handler) (i32.const 1))
            (then
              {peek}
              (if (i32.eqz (i32.load8_u (i32.const {RET})))
                (then (call $reply (i32.load (i32.const {p4})) (i32.load (i32.const {p8})) (i32.const {RET}))))
              {peek_far}
              (return {far})))
          {boxed} {ok}
          (call $reply
            (i32.load (i32.load (i32.const {p4})))
            (i32.load offset=4 (i32.load (i32.const {p4})))
            (i32.const 1000))
          {count})
        (func {event} (param $ev i32)
          (call $clear)
          (call $put_int (i32.load offset=40 (local.get $ev)))
          (call $put_char (i32.const 32))
          (call $put_int (i32.load offset=44 (local.get $ev)))
          (call $put_char (i32.const 32))
          (call $put_int (i32.load offset=48 (local.get $ev)))
          (call $put_char (i32.const 32))
          (call $put (i32.load offset=52 (local.get $ev)) (i32.load offset=56 (local.get $ev)))
          (call $put_char (i32.const 32))
          (call $put (i32.load offset=60 (local.get $ev)) (i32.load offset=64 (local.get $ev)))
          {SHOUT})
        {data}"#,
        run = export("run-command").replace(
            " (param i32 i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)",
            ""
        ),
        build = ok_code(&world_error()),
        far = ok_code(&format!("(i32.load8_u (i32.const {}))", RET + 4)),
        p4 = RET + 4,
        p8 = RET + 8,
        count = ok_code(&format!(
            "(i32.add (i32.mul (i32.load (i32.const {})) (i32.const 10)) (i32.load (i32.const {})))",
            RET + 8,
            RET + 16
        )),
        event = export("on-event").replace(" (param i32)", ""),
        data = data.wat(),
    );
    module(&plain("builder"), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugins_read_and_write_blocks_and_hear_block_events() {
    use crate::world::region_api::{BlockAction, PlayerBlockAction, RegionHooks};
    use tuff_vanilla_data::BlockState;
    use tuff_world_model::BlockPos;
    use tuff_world_model::pos::RegionPos;

    let dir = TestDir::new();
    install(dir.path(), "builder.wasm", &builder());
    let world = start(dir.path()).await;
    let steve = || Player::new(77, "Steve");
    let stone: BlockState = "minecraft:stone".parse().unwrap();
    let stairs: BlockState = "minecraft:oak_stairs[facing=east]".parse().unwrap();

    let built = host(&world).command(steve(), "build".into()).await.unwrap();
    assert_eq!(built.result, Ok(2), "a bad block state is refused");
    let region = world.region(RegionPos::new(0, 0)).await;
    assert_eq!(
        block_at(&region, BlockPos::new(0, 100, 0)).await,
        Some(stone)
    );
    assert_eq!(
        block_at(&region, BlockPos::new(1, 100, 0)).await,
        Some(stairs)
    );

    let peeked = host(&world).command(steve(), "peek".into()).await.unwrap();
    assert_eq!(peeked.replies, [stairs.to_string()]);
    assert_eq!(peeked.result, Ok(1), "a block outside the world");

    let boxed = host(&world).command(steve(), "box".into()).await.unwrap();
    assert_eq!(boxed.replies, ["minecraft:stone"]);
    assert_eq!(boxed.result, Ok(2 * 10 + 2), "two states, two blocks");

    let action = |state, action| PlayerBlockAction {
        player: 7,
        pos: BlockPos::new(0, 100, 0),
        state,
        action,
    };
    let fire = |action: PlayerBlockAction| {
        let region = region.clone();
        async move {
            region
                .ask(move |world| bridge::Blocks.player_block_action(world, &action))
                .await
                .unwrap();
        }
    };
    fire(action(stone, BlockAction::Broke)).await;
    fire(action(stairs, BlockAction::Placed)).await;
    fire(action(stone, BlockAction::Opened)).await;
    let heard = announced(&world).await;
    assert_eq!(
        heard,
        [
            "0 100 0 minecraft:overworld minecraft:stone".to_owned(),
            format!("0 100 0 minecraft:overworld {stairs}"),
        ]
    );

    console(&world, "tuff plugin unload builder").await;
    assert!(!host(&world).listens(EventKinds::BLOCK_BREAK));
    fire(action(stone, BlockAction::Broke)).await;
    assert_eq!(
        announced(&world).await,
        heard,
        "an unloaded plugin heard an event"
    );
    world.shutdown().await;
}

pub(crate) fn everywhere() -> tuff_plugin_wasm::wit::bindings::types::FilterScope {
    tuff_plugin_wasm::wit::bindings::types::FilterScope {
        dimension: None,
        area: None,
        bypass: None,
    }
}

/// Broadcasts `x y z dimension face state` for every click it hears.
fn wand() -> Vec<u8> {
    let mut data = Data::new();
    let right = ClickFilter {
        kind: ClickKind::Right,
        item: Some("minecraft:stick".into()),
        tag: None,
        block: None,
        scope: everywhere(),
    };
    let left = ClickFilter {
        kind: ClickKind::Left,
        item: None,
        tag: Some("wand".into()),
        block: None,
        scope: everywhere(),
    };
    let claims = data.call("intercept-click", &[right.to_value()])
        + &data.call("intercept-click", &[left.to_value()]);
    let body = format!(
        "(func {} {claims} {})
         (func {}
           (call $clear)
           (call $put_int (i32.load offset=32 (local.get 0)))
           (call $put_char (i32.const 32))
           (call $put_int (i32.load offset=36 (local.get 0)))
           (call $put_char (i32.const 32))
           (call $put_int (i32.load offset=40 (local.get 0)))
           (call $put_char (i32.const 32))
           (call $put (i32.load offset=44 (local.get 0)) (i32.load offset=48 (local.get 0)))
           (call $put_char (i32.const 32))
           (call $put_int (i32.load8_u offset=52 (local.get 0)))
           (call $put_char (i32.const 32))
           (call $put (i32.load offset=56 (local.get 0)) (i32.load offset=60 (local.get 0)))
           {SHOUT})
         {}",
        export("register"),
        ok_unit(),
        export("on-event"),
        data.wat()
    );
    module(&plain("wand"), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wand_clicks_are_cancelled_and_reported_until_unload() {
    use crate::protocol::play::PlayerActionStatus;
    use crate::world::inventory::Inventory;
    use tuff_item::item::{ItemStack, MintReason};
    use tuff_vanilla_data::Item;
    use tuff_world_model::{BlockPos, Face};

    let dir = TestDir::new();
    install(dir.path(), "wand.wasm", &wand());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let slot = Inventory::hotbar_slot(client.session.inventory.selected());
    *client.session.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::Stick, 1, MintReason::Test));
    let right = || Serverbound::UseItemOn {
        hand: 0,
        position: BlockPos::new(1, 64, 2),
        face: Face::Up,
        cursor: [0.5; 3],
        sequence: 3,
    };
    let left = || Serverbound::PlayerAction {
        status: PlayerActionStatus::StartDestroyBlock,
        position: BlockPos::new(1, 64, 2),
        face: 1,
        sequence: 4,
    };

    assert_eq!(
        bridge::Clicks
            .intercept(&mut client.session, &right())
            .await,
        Verdict::Cancel
    );
    assert_eq!(
        announced(&world).await,
        ["1 64 2 minecraft:overworld 1 minecraft:stone".to_owned()],
        "face 1 is up"
    );
    assert_eq!(
        bridge::Clicks.intercept(&mut client.session, &left()).await,
        Verdict::Allow,
        "the left click filter needs a tagged item"
    );
    *client.session.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::Stone, 1, MintReason::Test));
    assert_eq!(
        bridge::Clicks
            .intercept(&mut client.session, &right())
            .await,
        Verdict::Allow
    );
    assert_eq!(announced(&world).await.len(), 1);

    *client.session.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::Stick, 1, MintReason::Test));
    console(&world, "tuff plugin unload wand").await;
    assert_eq!(
        bridge::Clicks
            .intercept(&mut client.session, &right())
            .await,
        Verdict::Allow
    );
    assert_eq!(announced(&world).await.len(), 1);
}

/// `/ents` replies `kind x y z` of the first entity near the origin, `/held`
/// the held stack, `/inv` the first slot; entity and player events are
/// broadcast.
fn watcher() -> Vec<u8> {
    let mut data = Data::new();
    let kinds = EventKinds::ENTITY_SPAWN
        | EventKinds::ENTITY_DEATH
        | EventKinds::ENTITY_DAMAGE
        | EventKinds::PLAYER_HURT
        | EventKinds::PLAYER_DEATH;
    let subscribe = data.call("subscribe", &[kinds.to_value()]);
    let register = register(&mut data, &["ents", "held", "inv"], &subscribe);
    let corner = |x, y, z| WitVec3 { x, y, z }.to_value().into_owned();
    let entities = data.call(
        "entities-in-box",
        &[
            s("overworld"),
            corner(-10.0, 0.0, -10.0),
            corner(10.0, 200.0, 10.0),
        ],
    );
    let inventory = data.call("acting-inventory", &[]);
    let coordinate = |at: &str| {
        format!("(call $put_int (i32.trunc_f64_s {at})) (call $put_char (i32.const 32))")
    };
    let body = format!(
        r#"{register}
        (func {run} (param $handler i32) (param i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)
          (local $e i32)
          (call $clear)
          (if (i32.eqz (local.get $handler))
            (then
              {entities}
              (if (i32.load8_u (i32.const {RET})) (then unreachable))
              (if (i32.eqz (i32.load (i32.const {r8}))) (then (return {zero})))
              (local.set $e (i32.load (i32.const {r4})))
              (call $put (i32.load offset=16 (local.get $e)) (i32.load offset=20 (local.get $e)))
              (call $put_char (i32.const 32))
              {x} {y} {z}
              {SAY}
              (return {listed})))
          {inventory}
          (if (i32.eqz (i32.load8_u (i32.const {RET}))) (then (return {none})))
          (if (i32.eq (local.get $handler) (i32.const 1))
            (then
              (call $put (i32.load (i32.const {r8})) (i32.load (i32.const {r12})))
              (call $put_char (i32.const 32))
              (call $put_int (i32.load8_u (i32.const {r16})))
              {SAY}
              (return {zero})))
          (local.set $e (i32.load (i32.const {r20})))
          (call $put_int (i32.load8_u (local.get $e)))
          (call $put_char (i32.const 32))
          (call $put (i32.load offset=4 (local.get $e)) (i32.load offset=8 (local.get $e)))
          (call $put_char (i32.const 32))
          (call $put_int (i32.load8_u offset=12 (local.get $e)))
          {SAY}
          {slots})
        (func {event} (param $ev i32) (local $case i32)
          (local.set $case (i32.load8_u (local.get $ev)))
          (call $clear)
          (if (i32.lt_u (local.get $case) (i32.const 10))
            (then
              (call $put (i32.load offset=24 (local.get $ev)) (i32.load offset=28 (local.get $ev)))
              (call $put_char (i32.const 32))
              {ex} {ey} {ez}
              (call $put (i32.load offset=56 (local.get $ev)) (i32.load offset=60 (local.get $ev)))))
          (if (i32.ge_u (local.get $case) (i32.const 10))
            (then
              (call $put (i32.load offset=32 (local.get $ev)) (i32.load offset=36 (local.get $ev)))))
          (if (i32.eq (local.get $case) (i32.const 10))
            (then
              (call $put_char (i32.const 32))
              (call $put_int (i32.trunc_f32_s (f32.load offset=40 (local.get $ev))))))
          {SHOUT})
        {data}"#,
        run = export("run-command").replace(
            " (param i32 i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)",
            ""
        ),
        event = export("on-event").replace(" (param i32)", ""),
        r4 = RET + 4,
        r8 = RET + 8,
        r12 = RET + 12,
        r16 = RET + 16,
        r20 = RET + 20,
        zero = ok_code("(i32.const 0)"),
        none = ok_code("(i32.const -1)"),
        listed = ok_code(&format!("(i32.load (i32.const {}))", RET + 8)),
        slots = ok_code(&format!("(i32.load (i32.const {}))", RET + 24)),
        x = coordinate("(f64.load offset=24 (local.get $e))"),
        y = coordinate("(f64.load offset=32 (local.get $e))"),
        z = coordinate("(f64.load offset=40 (local.get $e))"),
        ex = coordinate("(f64.load offset=32 (local.get $ev))"),
        ey = coordinate("(f64.load offset=40 (local.get $ev))"),
        ez = coordinate("(f64.load offset=48 (local.get $ev))"),
        data = data.wat(),
    );
    module(&plain("watcher"), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugins_hear_entity_events_and_query_entities_and_items() {
    use crate::world::game_event::{Context, Emission, GameEvent, Source};
    use crate::world::messages::RegionRequest;
    use crate::world::region_api::RegionHooks;
    use crate::world::session::HurtResult;
    use crate::world::session::hooks::SessionHooks;
    use tuff_item::item::{ItemStack, MintReason};
    use tuff_vanilla_data::Item;
    use tuff_vanilla_data::damage_type;
    use tuff_world_model::pos::RegionPos;

    let dir = TestDir::new();
    install(dir.path(), "watcher.wasm", &watcher());
    let world = start(dir.path()).await;
    let region = world.region(RegionPos::new(0, 0)).await;
    let at = [3.5, 100.0, 3.5];
    block_at(&region, tuff_world_model::BlockPos::new(3, 100, 3)).await;
    let item = crate::world::items::ItemEntity::new(
        world.shared.next_entity_id(),
        4242,
        ItemStack::mint(Item::GoldIngot, 1, MintReason::Test),
        at,
        [0.0; 3],
        0,
    );
    let id = item.base.id;
    region
        .request(RegionRequest::SpawnEntity(Box::new(item)))
        .await;
    let mut line = None;
    for _ in 0..200 {
        line = region
            .ask(move |world| {
                let base = world.entity(id)?.entity.base();
                Some(format!("{} 3 100 3 ", base.kind.name()))
            })
            .await
            .unwrap();
        if line.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let line = line.expect("the item entered the region");
    assert_eq!(
        announced(&world).await,
        [format!("{line}minecraft:overworld")]
    );

    let steve = || Player::new(77, "Steve");
    let listed = host(&world).command(steve(), "ents".into()).await.unwrap();
    assert_eq!(listed.replies, std::slice::from_ref(&line));
    assert_eq!(listed.result, Ok(1));

    let emission = |event| Emission {
        event,
        pos: at,
        context: Context::from(Source::entity(id)),
    };
    for event in [
        GameEvent::EntityDamage,
        GameEvent::EntityDie,
        GameEvent::Step,
    ] {
        let emission = emission(event);
        region
            .ask(move |world| bridge::Entities.game_event(world, &emission))
            .await
            .unwrap();
    }
    let heard = announced(&world).await;
    assert_eq!(heard.len(), 3, "damage and death are heard, steps are not");
    assert_eq!(heard[1], heard[2]);

    let held = host(&world).command(steve(), "held".into()).await.unwrap();
    assert_eq!(held.result, Ok(-1), "no inventory without a session");
    let mut client = Client::new(&world);
    let slot = crate::world::inventory::Inventory::hotbar_slot(client.session.inventory.selected());
    *client.session.inventory.slot_mut(slot) =
        Some(ItemStack::mint(Item::Stick, 3, MintReason::Test));
    *client.session.inventory.slot_mut(0) = Some(ItemStack::mint(Item::Stone, 9, MintReason::Test));
    let items = tuff_plugin_wasm::sandbox::Items {
        held: Some(("minecraft:stick".to_owned(), 3)),
        slots: vec![(0, "minecraft:stone".to_owned(), 9)],
        ..Default::default()
    };
    let with_items = Player {
        items: Some(items),
        ..steve()
    };
    let held = host(&world)
        .command(with_items.clone(), "held".into())
        .await
        .unwrap();
    assert_eq!(held.replies, ["minecraft:stick 3"]);
    let inv = host(&world)
        .command(with_items, "inv".into())
        .await
        .unwrap();
    assert_eq!(inv.replies, ["0 minecraft:stone 9"]);
    assert_eq!(inv.result, Ok(1));

    let before = announced(&world).await.len();
    let result = HurtResult {
        landed: true,
        health_lost: 4.0,
        ..HurtResult::default()
    };
    let cactus = crate::world::session::HurtBy {
        kind: &damage_type::CACTUS,
        attacker: None,
        projectile: None,
        direct: false,
    };
    tuff_plugin_wasm::guard::Sessions.player_damaged(&mut client.session, &cactus, &result, false);
    tuff_plugin_wasm::guard::Sessions.player_damaged(&mut client.session, &cactus, &result, true);
    let heard = announced(&world).await;
    assert_eq!(
        heard[before..],
        [
            "minecraft:cactus 4".to_owned(),
            "minecraft:cactus".to_owned()
        ]
    );

    console(&world, "tuff plugin unload watcher").await;
    for kind in [
        EventKinds::ENTITY_SPAWN,
        EventKinds::ENTITY_DEATH,
        EventKinds::ENTITY_DAMAGE,
        EventKinds::PLAYER_HURT,
        EventKinds::PLAYER_DEATH,
    ] {
        assert!(!host(&world).listens(kind), "{kind:?} still heard");
    }
    let emission = emission(GameEvent::EntityDie);
    region
        .ask(move |world| bridge::Entities.game_event(world, &emission))
        .await
        .unwrap();
    tuff_plugin_wasm::guard::Sessions.player_damaged(&mut client.session, &cactus, &result, true);
    assert_eq!(announced(&world).await.len(), before + 2);
    assert!(host(&world).command(steve(), "ents".into()).await.is_none());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn killing_a_player_by_directive_reaches_plugins_as_a_death() {
    use crate::world::messages::{Directive, DirectiveSource};
    use tuff_item::text::TextComponent;

    let dir = TestDir::new();
    install(dir.path(), "watcher.wasm", &watcher());
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let before = announced(&world).await.len();
    client
        .session
        .apply_directive(
            Directive::Kill,
            DirectiveSource {
                name: TextComponent::new("Plugin"),
                player: None,
            },
        )
        .await;
    let heard = announced(&world).await;
    assert_eq!(heard[before..], ["minecraft:generic_kill".to_owned()]);
    world.shutdown().await;
}

/// `/tuff we ...` and its alias `//set` reply the line they ran as.
fn we() -> Vec<u8> {
    let mut data = Data::new();
    let claims = data.call("register-tuff-subcommand", &[line_tree("we", 0).to_value()])
        + &data.call("register-alias", &[s("/set"), s("tuff we set")]);
    let body = format!(
        "(func {} {claims} {})
         (func {} (call $reply (local.get 8) (local.get 9) (i32.const {RET})) {})
         {}",
        export("register"),
        ok_unit(),
        export("run-command"),
        ok_code("(i32.const 1)"),
        data.wat()
    );
    module(&plain("we"), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tuff_subcommands_and_aliases_reach_the_plugin_and_the_client_tree() {
    let dir = TestDir::new();
    install(dir.path(), "we.wasm", &we());
    let world = start(dir.path()).await;
    let mut owner = Client::at(&world, PermissionLevel::Owners);
    let listed = |world: &TestWorld| {
        let extras = world.shared.commands.client_roots().get();
        let named = |name: &str| {
            extras.trees.iter().any(|tree| tree.name() == name) || extras.roots.contains(&name)
        };
        (
            named("/set"),
            extras.tuff.contains(&"we") || extras.trees.iter().any(|tree| tree.name() == "we"),
        )
    };

    assert!(
        owner
            .answered("tuff we set stone", "tuff we set stone")
            .await
    );
    assert!(owner.answered("/set stone", "tuff we set stone").await);
    assert!(owner.answered("/set", "tuff we set").await);
    assert_eq!(listed(&world), (true, true));

    let out = console(&world, "tuff we set stone").await;
    assert_eq!(out, ["tuff we set stone"]);
    let out = console(&world, "//set stone").await;
    assert_eq!(out, ["tuff we set stone"]);
    let (out, ok) = crate::world::commands::console::run_as_counted(
        world.shared.clone(),
        "tuff we set air",
        "@".into(),
        PermissionLevel::Gamemasters,
    )
    .await;
    assert_eq!((out.len(), ok), (1, 1));
    assert_eq!(out[0].plain(), "tuff we set air");

    console(&world, "tuff plugin unload we").await;
    assert_eq!(listed(&world), (false, false));
    owner.session.send_permission_level();
    let (verdict, _) = owner.run("/set stone").await;
    assert_eq!(verdict, Verdict::Allow);
    assert!(!host(&world).claims("tuff we set"));
    let out = console(&world, "tuff we set stone").await;
    assert_ne!(out, ["tuff we set stone"]);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tuff_subcommand_clashing_with_a_builtin_is_refused() {
    let dir = TestDir::new();
    let mut data = Data::new();
    let claim = data.call(
        "register-tuff-subcommand",
        &[line_tree("chunk", 0).to_value()],
    );
    let body = format!(
        "(func {} {claim} {}) (func {} {}) {}",
        export("register"),
        ok_unit(),
        export("run-command"),
        ok_code("(i32.const 0)"),
        data.wat()
    );
    install(dir.path(), "clash.wasm", &module(&plain("clash"), &body));
    let world = start(dir.path()).await;
    assert!(host(&world).list().await.is_empty());
    let out = console(&world, "tuff plugin load clash").await;
    assert!(
        out.iter()
            .any(|line| line.contains("clash claims /tuff chunk, which the server already has")),
        "{out:?}"
    );
    world.shutdown().await;
}

fn open_sqlite(dir: &Path) -> std::sync::Arc<tuff_storage::sqlite::SqliteBackend> {
    std::sync::Arc::new(
        tuff_storage::sqlite::SqliteBackend::open(dir.join("world.mcserver")).unwrap(),
    )
}

async fn start_sqlite(
    dir: &Path,
    backend: std::sync::Arc<tuff_storage::sqlite::SqliteBackend>,
) -> TestWorld {
    let plugin = WasmPluginsFeature {
        directory: Some(plugins_dir(dir)),
        limits: Limits {
            fuel: 2_000_000,
            ..Limits::default()
        },
    };
    let world = TestWorld::start_with_features_on(backend, vec![Box::new(plugin)]);
    host(&world).sync().await;
    world
}

fn relay() -> Vec<u8> {
    module(
        "id = relay\nversion = 1.0.0\napi = 0\n\
         cap.http = discord.com/api/webhooks/ -- Relays chat\n\
         cap.files = 1 MiB\n\
         cap.route = /dynmap top",
        &format!("(func {} {})", export("register"), ok_unit()),
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owners_edit_grants_in_game_and_they_persist_across_restarts() {
    let dir = TestDir::new();
    install(dir.path(), "relay.wasm", &relay());
    let backend = open_sqlite(dir.path());
    let world = start_sqlite(dir.path(), backend.clone()).await;
    let shown = console(&world, "tuff plugin grants relay").await;
    assert!(shown[0].contains("requests 3 capabilities"), "{shown:?}");
    assert_eq!(
        shown
            .iter()
            .filter(|line| line.contains("NOT GRANTED"))
            .count(),
        3,
        "{shown:?}"
    );

    let mut policy = host(&world).policy("relay").await.unwrap();
    assert!(policy.borrow().check_top_route("/dynmap").is_err());
    let out = console(&world, "tuff plugin grant relay http all-requested").await;
    assert!(
        out[0].starts_with("Granted 1 requested http entries"),
        "{out:?}"
    );
    assert!(policy.has_changed().unwrap());
    policy.mark_unchanged();
    let out = console(&world, "tuff plugin grant relay route all-requested").await;
    assert!(
        out[0].starts_with(
            "Granted 0 requested route entries; grant these explicitly: route /dynmap top"
        ),
        "{out:?}"
    );
    assert!(policy.borrow().check_top_route("/dynmap").is_err());
    let out = console(&world, "tuff plugin grant relay route /dynmap top").await;
    assert!(out[0].starts_with("Granted route /dynmap top"), "{out:?}");
    assert!(
        policy
            .borrow()
            .check_top_route("/dynmap/tiles/0.png")
            .is_ok()
    );
    assert!(policy.borrow().check_route("/dynmap").is_err());
    console(&world, "tuff plugin grant relay files 1 MiB").await;
    console(
        &world,
        "tuff plugin grant relay limit.requests-per-minute 30",
    )
    .await;
    assert_eq!(policy.borrow().limits().requests_per_minute, 30);

    let denied = console(&world, "tuff plugin grant relay http nope").await;
    assert!(denied[0].contains("nope"), "{denied:?}");
    let unknown = console(&world, "tuff plugin grant relay telnet x.example:23").await;
    assert!(unknown[0].contains("not a capability"), "{unknown:?}");

    let mut steve = Client::new(&world);
    steve
        .session
        .run_command("tuff plugin revoke relay route all")
        .await;
    steve
        .session
        .run_command("tuff plugin grant relay tcp evil.example:22")
        .await;
    assert!(policy.borrow().check_top_route("/dynmap").is_ok());
    world.shared.services.flush().await;
    world.shutdown().await;
    backend.close();

    let backend = open_sqlite(dir.path());
    let world = start_sqlite(dir.path(), backend.clone()).await;
    let shown = console(&world, "tuff plugin grants relay").await;
    assert_eq!(
        shown
            .iter()
            .filter(|line| line.contains("NOT GRANTED"))
            .count(),
        0,
        "{shown:?}"
    );
    assert!(shown.iter().all(|line| !line.contains("tcp")), "{shown:?}");
    let policy = host(&world).policy("relay").await.unwrap();
    assert!(policy.borrow().check_top_route("/dynmap").is_ok());
    assert_eq!(policy.borrow().limits().requests_per_minute, 30);

    let mut owner = Client::at(&world, PermissionLevel::Owners);
    owner
        .session
        .run_command("tuff plugin revoke relay route all")
        .await;
    host(&world).sync().await;
    assert!(policy.borrow().check_top_route("/dynmap").is_err());
    let out = console(
        &world,
        "tuff plugin revoke relay http discord.com/api/webhooks/",
    )
    .await;
    assert!(
        out[0].starts_with("Revoked http https://discord.com/api/webhooks/ (requests http"),
        "{out:?}"
    );
    let again = console(
        &world,
        "tuff plugin revoke relay http discord.com/api/webhooks/",
    )
    .await;
    assert!(again[0].contains("is not granted"), "{again:?}");
    world.shutdown().await;
    backend.close();
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grants_made_offline_are_there_when_the_world_starts() {
    use tuff_plugin_wasm::grants_edit::{GrantChange, change_offline, report_offline};
    let dir = TestDir::new();
    install(dir.path(), "relay.wasm", &relay());
    let backend = open_sqlite(dir.path());
    let store = backend.config().unwrap();
    let plugins = plugins_dir(dir.path());
    let grant = |cap: &str, entry: &str| GrantChange::Grant {
        cap: cap.into(),
        entry: entry.into(),
    };
    let done = change_offline(&*store, &plugins, "relay", grant("http", "all-requested")).unwrap();
    assert!(
        done.starts_with("Granted 1 requested http entries"),
        "{done}"
    );
    let done = change_offline(&*store, &plugins, "relay", grant("route", "all-requested")).unwrap();
    assert!(done.contains("grant these explicitly"), "{done}");
    let report = report_offline(&*store, &plugins, "relay").unwrap();
    assert_eq!(report.requested.iter().filter(|r| r.2).count(), 1);
    for (id, cap) in [("ghost", "http"), ("relay", "smtp")] {
        assert!(change_offline(&*store, &plugins, id, grant(cap, "all-requested")).is_err());
    }
    let world = start_sqlite(dir.path(), backend.clone()).await;
    let shown = console(&world, "tuff plugin grants relay").await;
    assert_eq!(
        shown.iter().filter(|l| l.contains("NOT GRANTED")).count(),
        2,
        "{shown:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_folder_world_keeps_grants_in_its_tuff_database_and_needs_a_loaded_plugin() {
    let dir = TestDir::new();
    install(dir.path(), "relay.wasm", &relay());
    let world = start(dir.path()).await;
    let out = console(&world, "tuff plugin grant relay http all-requested").await;
    assert!(out[0].starts_with("Granted"), "{out:?}");
    let out = console(&world, "tuff plugin grants relay").await;
    assert!(out.iter().any(|l| l.contains("  granted: http")), "{out:?}");
    let out = console(&world, "tuff plugin grants ghost").await;
    assert_eq!(out, ["ghost is not loaded"]);
    world.shutdown().await;
}

fn filer() -> Vec<u8> {
    let mut data = Data::new();
    let register = register(&mut data, &["save"], "");
    let write = data.call(
        "files.write",
        &[
            s("notes/a.txt"),
            WitValue::Variant(0, None),
            WitValue::Bytes(b"hello".as_slice().into()),
        ],
    );
    let done = text(&mut data, "file done");
    let body = format!(
        r#"{register}
        (func {run} {write} {ok})
        (func {on_io} (call $broadcast {done} (i32.const 256)))
        {data}"#,
        run = export("run-command"),
        ok = ok_code("(i32.const 1)"),
        on_io = export("on-io"),
        data = data.wat(),
    );
    module(&format!("{}\ncap.files = 1 KiB", plain("filer")), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_files_need_a_grant_run_off_thread_and_stop_on_revoke() {
    let dir = TestDir::new();
    install(dir.path(), "filer.wasm", &filer());
    let store = plugins_dir(dir.path()).join("filer").join("files");
    let world = start_sqlite(dir.path(), open_sqlite(dir.path())).await;
    let mut steve = Client::new(&world);
    steve.run("save").await;
    host(&world).sync().await;
    assert_eq!(count(&world, "file done").await, 0, "ungranted");
    assert!(!store.exists());

    console(&world, "tuff plugin grant filer files 1 KiB").await;
    host(&world).sync().await;
    std::fs::create_dir_all(store.join("notes")).unwrap();
    steve.run("save").await;
    let mut tries = 0;
    while count(&world, "file done").await == 0 && tries < 200 {
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        tries += 1;
    }
    assert_eq!(count(&world, "file done").await, 1);
    assert_eq!(std::fs::read(store.join("notes/a.txt")).unwrap(), b"hello");

    console(&world, "tuff plugin revoke filer files 1 KiB").await;
    host(&world).sync().await;
    std::fs::remove_file(store.join("notes/a.txt")).unwrap();
    steve.run("save").await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(count(&world, "file done").await, 1, "revoked");
    assert!(!store.join("notes/a.txt").exists());
    world.shutdown().await;
}

fn line_filters(manifest: &str) -> Vec<u8> {
    use tuff_plugin_wasm::wit::bindings::types::{ChatFilter, CommandFilter, LineAction};
    let mut data = Data::new();
    let filters = [
        data.call(
            "intercept-command",
            &[CommandFilter {
                prefix: "gamemode".into(),
                action: LineAction::Cancel(None),
            }
            .to_value()],
        ),
        data.call(
            "intercept-command",
            &[CommandFilter {
                prefix: "w".into(),
                action: LineAction::Rewrite("msg".into()),
            }
            .to_value()],
        ),
        data.call(
            "intercept-chat",
            &[ChatFilter {
                prefix: None,
                contains: Some("spam".into()),
                action: LineAction::Cancel(None),
            }
            .to_value()],
        ),
        data.call(
            "intercept-chat",
            &[ChatFilter {
                prefix: Some("!".into()),
                contains: None,
                action: LineAction::Rewrite("shouted".into()),
            }
            .to_value()],
        ),
    ]
    .concat();
    let body = format!(
        "(func {} {filters} {})
         {}",
        export("register"),
        ok_unit(),
        data.wat()
    );
    module(manifest, &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rewrite_filters_do_nothing_without_the_player_write_grant() {
    let dir = TestDir::new();
    install(dir.path(), "filters.wasm", &line_filters(&plain("filters")));
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let command = |line: &str| Serverbound::ChatCommand(line.to_owned());
    let chat = |line: &str| Serverbound::Chat(line.to_owned());
    assert_eq!(
        bridge::Commands
            .intercept(&mut client.session, &command("gamemode creative"))
            .await,
        Verdict::Cancel
    );
    assert_eq!(
        bridge::Commands
            .intercept(&mut client.session, &command("w Bob hi"))
            .await,
        Verdict::Allow
    );
    assert_eq!(
        bridge::Commands
            .intercept(&mut client.session, &chat("!hey"))
            .await,
        Verdict::Allow
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn command_and_chat_filters_cancel_and_rewrite_until_unload() {
    let dir = TestDir::new();
    install(
        dir.path(),
        "filters.wasm",
        &line_filters(&format!("{}\ncap.player-write = all", plain("filters"))),
    );
    let world = start(dir.path()).await;
    let mut client = Client::new(&world);
    let command = |line: &str| Serverbound::ChatCommand(line.to_owned());
    let chat = |line: &str| Serverbound::Chat(line.to_owned());

    assert_eq!(
        bridge::Commands
            .intercept(&mut client.session, &command("gamemode creative"))
            .await,
        Verdict::Cancel
    );
    assert_eq!(
        bridge::Commands
            .intercept(&mut client.session, &command("w Bob hi"))
            .await,
        Verdict::Correct(Box::new(command("msg Bob hi")))
    );
    assert_eq!(
        bridge::Commands
            .intercept(&mut client.session, &command("say hi"))
            .await,
        Verdict::Allow
    );
    assert_eq!(
        bridge::Commands
            .intercept(&mut client.session, &chat("buy SPAM"))
            .await,
        Verdict::Cancel
    );
    assert_eq!(
        bridge::Commands
            .intercept(&mut client.session, &chat("!hey"))
            .await,
        Verdict::Correct(Box::new(chat("shouted")))
    );
    assert_eq!(
        bridge::Commands
            .intercept(&mut client.session, &chat("hello"))
            .await,
        Verdict::Allow
    );

    console(&world, "tuff plugin unload filters").await;
    assert_eq!(
        bridge::Commands
            .intercept(&mut client.session, &command("gamemode creative"))
            .await,
        Verdict::Allow
    );
    world.shutdown().await;
}

fn suggester() -> Vec<u8> {
    let mut data = Data::new();
    let node = |name: &str, kind, children: Vec<u16>, suggest| CommandNode {
        name: name.into(),
        kind,
        children,
        executes: Some(0),
        redirect: None,
        permission: None,
        suggest,
    };
    let tree = CommandTree {
        nodes: vec![
            node("pick", NodeKind::Literal, vec![1], false),
            node(
                "word",
                NodeKind::Argument(Parser::Text(TextKind::Word)),
                vec![],
                true,
            ),
        ],
    };
    let list = vec!["alpha".to_owned(), "beta".to_owned()];
    let answer = data.export_result("suggest", &list.to_value());
    let body = format!(
        "(func {register} {reg} {unit})
         (func {run} {code})
         (func {suggest} (i32.const {answer}))
         {data}",
        run = export("run-command"),
        code = ok_code("(i32.const 0)"),
        register = export("register"),
        reg = data.call("register-command", &[tree.to_value()]),
        unit = ok_unit(),
        suggest = export("suggest"),
        data = data.wat(),
    );
    module(&plain("suggester"), &body)
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_suggestions_reach_the_console_and_a_session() {
    let dir = TestDir::new();
    install(dir.path(), "suggester.wasm", &suggester());
    let world = start(dir.path()).await;

    let found = crate::world::commands::console::suggest(
        &world.shared,
        "pick al",
        crate::permission::Subject::Server,
    )
    .await;
    assert_eq!(found.matches, vec!["alpha".to_owned()]);
    assert_eq!((found.start, found.length), (5, 2));

    let mut client = Client::new(&world);
    client.session.suggest(3, "/pick b").await;
    let mut frames = Vec::new();
    while let Ok(frame) = client.frames.try_recv() {
        frames.push(frame);
    }
    assert!(
        frames.iter().any(|f| f.windows(4).any(|w| w == b"beta")),
        "session completion carries the plugin's match"
    );
}
