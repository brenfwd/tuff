//! The messaging-client example and the test provider talking through a typed interface:
//! the client soft-depends on greeter, calls it and survives its absence.

use super::*;
use tuff_plugin_wasm::sandbox::Player;
use tuff_plugin_wasm::{WasmHost, WasmPluginsFeature};
use tuff_storage::admin::PermissionLevel;

fn built(example: &str, wasm: &str) -> Vec<u8> {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    let dir = root.join(example);
    assert!(
        super::worldedit_wasm::build_example(&dir),
        "building {example} failed (is the wasm32-unknown-unknown target installed?)"
    );
    let path = dir.join("target/wasm32-unknown-unknown/release").join(wasm);
    std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

struct Pair {
    command: &'static str,
    world: TestWorld,
    plugins: std::path::PathBuf,
    greeter: Vec<u8>,
    _dir: TestDir,
}

impl Pair {
    async fn start(with_greeter: bool) -> Pair {
        let client = built("examples/rust-messaging-client", "messaging_client.wasm");
        Pair::start_with(client, "messaging-client", "greeted", with_greeter).await
    }

    async fn start_cpp(with_greeter: bool) -> Option<Pair> {
        let path = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
            .join("examples/cpp-messaging/build/messaging.wasm");
        let client = match std::fs::read(&path) {
            Ok(client) => client,
            Err(_) if std::env::var("TUFF_EXAMPLE_PLUGINS").as_deref() == Ok("1") => {
                panic!("{} is not built", path.display())
            }
            Err(_) => {
                eprintln!("skipping: {} is not built", path.display());
                return None;
            }
        };
        Some(Pair::start_with(client, "cpp-messaging", "cppgreeted", with_greeter).await)
    }

    async fn start_with(
        client: Vec<u8>,
        client_id: &str,
        command: &'static str,
        with_greeter: bool,
    ) -> Pair {
        let greeter = built(
            "crates/tuff-plugin-wasm/tests/guests/rust-messaging-provider",
            "rust_messaging_provider.wasm",
        );
        let dir = TestDir::new();
        let plugins = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(plugins.join("messaging_client.wasm"), &client).unwrap();
        if with_greeter {
            std::fs::write(plugins.join("greeter.wasm"), &greeter).unwrap();
        }
        let backend = Arc::new(
            tuff_storage::sqlite::SqliteBackend::open(dir.path().join("world.mcserver")).unwrap(),
        );
        let config = backend.config().expect("sqlite keeps the config");
        for id in [client_id, "greeter"] {
            config
                .write_plugin_grant(id, Some("cross-plugin = all"))
                .unwrap();
        }
        let world = TestWorld::start_with_features_on(
            backend,
            vec![Box::new(WasmPluginsFeature::at(plugins.clone()))],
        );
        let pair = Pair {
            command,
            world,
            plugins,
            greeter,
            _dir: dir,
        };
        pair.host().sync().await;
        pair
    }

    fn host(&self) -> impl std::ops::Deref<Target = WasmHost> {
        self.world.shared.services.get::<WasmHost>().unwrap()
    }

    async fn say(&self, line: &str) -> Vec<String> {
        let player = Player {
            uuid: 77,
            name: "Steve".to_owned(),
            level: Some(PermissionLevel::Gamemasters),
            position: None,
            items: None,
        };
        let host = self.host();
        host.command(player, line.to_owned())
            .await
            .expect("client owns its command")
            .replies
    }

    async fn greet(&self, name: &str) -> String {
        let command = self.command;
        self.say(&format!("{command} greet {name}")).await;
        self.settle().await;
        joined(&self.say(&format!("{command} answer")).await)
    }

    async fn settle(&self) {
        self.world.ticks(4).await;
        self.host().sync().await;
    }
}

fn joined(lines: &[String]) -> String {
    lines.join("\n")
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_calls_greeter_and_receives_its_event() {
    let pair = Pair::start(true).await;
    let said = pair.greet("Alex").await;
    assert!(said.contains("Hello, Alex! (greeting 1)"), "{said}");
    pair.settle().await;
    let last = pair.say("greeted last").await;
    assert!(joined(&last).contains("Last greeted Alex (1)"), "{last:?}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_falls_back_when_greeter_is_not_installed() {
    let pair = Pair::start(false).await;
    assert_eq!(pair.host().loaded(), 1);
    let said = pair.greet("Alex").await;
    assert!(said.contains("greeter plugin is not installed"), "{said}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_survives_greeter_unload_and_works_after_reload() {
    let pair = Pair::start(true).await;
    pair.host().unload("greeter").await.unwrap();
    pair.settle().await;
    assert_eq!(pair.host().loaded(), 1);
    let state = pair.say("greeted greeter").await;
    assert!(joined(&state).contains("greeter is gone"), "{state:?}");
    let said = pair.greet("Alex").await;
    assert!(said.contains("greeter plugin is not installed"), "{said}");

    std::fs::write(pair.plugins.join("greeter.wasm"), &pair.greeter).unwrap();
    pair.host().load("greeter.wasm").await.unwrap();
    pair.settle().await;
    let state = pair.say("greeted greeter").await;
    assert!(joined(&state).contains("greeter is loaded"), "{state:?}");
    let said = pair.greet("Alex").await;
    assert!(said.contains("Hello, Alex! (greeting 1)"), "{said}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cpp_client_calls_the_rust_greeter_and_serves_its_event() {
    let Some(pair) = Pair::start_cpp(true).await else {
        return;
    };
    let said = pair.greet("Alex").await;
    assert!(said.contains("Hello, Alex! (greeting 1)"), "{said}");
    let said = pair.greet("Alex").await;
    assert!(said.contains("Hello, Alex! (greeting 2)"), "{said}");
    pair.settle().await;
    let last = pair.say("cppgreeted last").await;
    assert!(joined(&last).contains("Last greeted Alex (2)"), "{last:?}");
    let state = pair.say("cppgreeted greeter").await;
    assert!(
        joined(&state).contains("greeter state unknown")
            || joined(&state).contains("greeter is loaded"),
        "{state:?}"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cpp_client_falls_back_and_follows_greeter_unload_and_reload() {
    let Some(pair) = Pair::start_cpp(false).await else {
        return;
    };
    let said = pair.greet("Alex").await;
    assert!(said.contains("greeter plugin is not installed"), "{said}");

    std::fs::write(pair.plugins.join("greeter.wasm"), &pair.greeter).unwrap();
    pair.host().load("greeter.wasm").await.unwrap();
    pair.settle().await;
    let state = pair.say("cppgreeted greeter").await;
    assert!(joined(&state).contains("greeter is loaded"), "{state:?}");
    let said = pair.greet("Alex").await;
    assert!(said.contains("Hello, Alex! (greeting 1)"), "{said}");

    pair.host().unload("greeter").await.unwrap();
    pair.settle().await;
    let state = pair.say("cppgreeted greeter").await;
    assert!(joined(&state).contains("greeter is gone"), "{state:?}");
    let said = pair.greet("Alex").await;
    assert!(said.contains("greeter plugin is not installed"), "{said}");
}
