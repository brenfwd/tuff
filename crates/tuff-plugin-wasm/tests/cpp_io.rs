//! `examples/cpp-io`, on the C++ SDK's awaitable capability calls and
//! world handles: a real file store and a local HTTP server complete the
//! plugin's coroutines through `on-io`. Skipped when the example is not
//! built, unless `TUFF_EXAMPLE_PLUGINS=1` requires it.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};
use tuff_plugin_wasm::caps::resolve::SystemResolver;
use tuff_plugin_wasm::caps::{Grant, Policy, Request};
use tuff_plugin_wasm::files::{Access, Bounds, Done, Files};
use tuff_plugin_wasm::net::Net;
use tuff_plugin_wasm::sandbox::{Limits, Player, Position, Sandbox, engine};
use tuff_plugin_wasm::wit::bindings::capability_types::{FileDone, IoEvent};

fn built() -> Option<Vec<u8>> {
    let path: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/cpp-io/build/io.wasm");
    match std::fs::read(&path) {
        Ok(wasm) => Some(wasm),
        Err(_) if std::env::var("TUFF_EXAMPLE_PLUGINS").as_deref() == Ok("1") => {
            panic!("{} is not built", path.display())
        }
        Err(_) => {
            eprintln!("skipping: {} is not built", path.display());
            None
        }
    }
}

fn alex() -> Player {
    let mut alex = Player::new(0xa1e8, "Alex");
    alex.position = Some(Position {
        dimension: "minecraft:overworld".into(),
        at: [0.0, 64.0, 0.0],
        look: None,
        eye_height: 1.62,
    });
    alex
}

fn plain(text: &str) -> String {
    text.replace('\u{a7}', "&")
}

struct Store {
    files: Arc<Files>,
    done: Receiver<Done>,
    dir: PathBuf,
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn store() -> Store {
    let dir = std::env::temp_dir().join(format!("tuff-cpp-io-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("files")).unwrap();
    let (tx, done) = channel();
    let tx = Mutex::new(tx);
    let files = Files::new(
        dir.join("files"),
        Bounds {
            entries: 16,
            read: 4096,
            pending: 8,
        },
        move |d| {
            let _ = tx.lock().unwrap().send(d);
        },
    );
    files.set_access(Access::Quota(1 << 20));
    Store {
        files: Arc::new(files),
        done,
        dir,
    }
}

fn sandbox(wasm: &[u8]) -> Sandbox {
    let mut plugin = Sandbox::new(&engine(), wasm, "io", Limits::default()).unwrap();
    plugin.register().unwrap();
    plugin.enable().unwrap();
    plugin
}

#[tuff_core::watchdog]
#[test]
fn files_resume_coroutines() {
    let Some(wasm) = built() else { return };
    let mut plugin = sandbox(&wasm);
    let store = store();
    plugin.set_files(store.files.clone());

    let (result, mut effects) = plugin.command(alex(), "io save hello world").unwrap();
    assert_eq!(result, Ok(1));
    let mut replies = Vec::new();
    replies.append(&mut effects.replies);
    let told = |effects: tuff_plugin_wasm::sandbox::Effects| -> Vec<String> {
        effects.tells.into_iter().map(|(_, text)| text).collect()
    };
    for _ in 0..4 {
        let Ok((operation, outcome)) = store.done.recv_timeout(Duration::from_millis(500)) else {
            break;
        };
        let effects = plugin
            .io(&IoEvent::File(FileDone { operation, outcome }))
            .unwrap();
        replies.append(&mut told(effects));
    }
    assert_eq!(
        replies,
        ["read back: hello world", "1 file(s)", "missing: not found"]
    );
}

#[tuff_core::watchdog]
#[test]
fn files_refused_without_a_store_answer_at_once() {
    let Some(wasm) = built() else { return };
    let mut plugin = sandbox(&wasm);
    let (_, effects) = plugin.command(alex(), "io save x").unwrap();
    assert_eq!(effects.replies.len(), 1);
    assert!(effects.replies[0].contains("write failed"), "{effects:?}");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn http_fetch_resumes_a_coroutine() {
    let Some(wasm) = built() else { return };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\npong",
                    )
                    .await;
                let _ = stream.shutdown().await;
            });
        }
    });

    let host = format!("http://127.0.0.1:{}", addr.port());
    let requests = Request::parse_line("cap.http", &format!("{host} -- test"))
        .unwrap()
        .unwrap();
    let grant = Grant::parse(&format!("http = {host} private\n")).unwrap();
    let (_policy, watcher) = watch::channel(Arc::new(Policy::new(requests, Some(&grant))));
    let (queue, mut events) = mpsc::unbounded_channel();
    let net = Net::new(watcher, Arc::new(SystemResolver), "io", 1, queue);

    let mut plugin = sandbox(&wasm);
    plugin.attach_net(net);
    let (result, effects) = plugin
        .command(alex(), &format!("io get {host}/ping"))
        .unwrap();
    assert_eq!(result, Ok(1));
    assert!(effects.replies.is_empty());
    let delivery = tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .unwrap()
        .unwrap();
    let effects = plugin.io(&delivery.event).unwrap();
    let told: Vec<&str> = effects.tells.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(told, ["status 200, 4 bytes: pong"]);
}

#[tuff_core::watchdog]
#[test]
fn fetch_without_a_net_is_denied() {
    let Some(wasm) = built() else { return };
    let mut plugin = sandbox(&wasm);
    let (_, effects) = plugin.command(alex(), "io get http://127.0.0.1/").unwrap();
    assert_eq!(effects.replies.len(), 1);
    assert!(
        plain(&effects.replies[0]).starts_with("&cfetch failed"),
        "{effects:?}"
    );
}

#[tuff_core::watchdog]
#[test]
fn world_handles_reach_the_host() {
    let Some(wasm) = built() else { return };
    let mut plugin = sandbox(&wasm);
    let requests = Request::parse_line("cap.player-write", "all")
        .unwrap()
        .unwrap();
    let grant = Grant::parse("player-write = all\n").unwrap();
    let (_policy, watcher) = watch::channel(Arc::new(Policy::new(requests, Some(&grant))));
    plugin.set_policy(watcher);
    let (result, effects) = plugin.command(alex(), "io probe").unwrap();
    assert_eq!(result, Ok(1));
    assert_eq!(
        effects.replies,
        [
            "in minecraft:overworld",
            "io.use: no",
            "give: ok",
            "teleport: refused",
            "block: unavailable",
        ]
    );
    assert_eq!(effects.inventory_ops.len(), 1);
    assert!(matches!(
        effects.inventory_ops[0].1,
        tuff_core::world::session::plugin_inventory::InventoryOp::Give { count: 3, .. }
    ));
    assert!(effects.teleports.is_empty());
    let (result, _) = plugin.command(Player::new(1, "NoPos"), "io probe").unwrap();
    assert!(result.is_err());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn tcp_socket_callbacks_run_from_io_events() {
    let Some(wasm) = built() else { return };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 16];
                while let Ok(n) = stream.read(&mut buf).await {
                    if n == 0 || stream.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    let target = format!("127.0.0.1:{port}");
    let requests = Request::parse_line("cap.tcp", &target).unwrap().unwrap();
    let grant = Grant::parse(&format!("tcp = {target} private\n")).unwrap();
    let (_policy, watcher) = watch::channel(Arc::new(Policy::new(requests, Some(&grant))));
    let (queue, mut events) = mpsc::unbounded_channel();
    let net = Net::new(watcher, Arc::new(SystemResolver), "io", 1, queue);

    let mut plugin = sandbox(&wasm);
    plugin.attach_net(net);
    let (result, _) = plugin.command(alex(), &format!("io echo {port}")).unwrap();
    assert_eq!(result, Ok(1));
    let mut told = Vec::new();
    for _ in 0..4 {
        let Ok(Some(delivery)) = tokio::time::timeout(Duration::from_secs(10), events.recv()).await
        else {
            break;
        };
        let effects = plugin.io(&delivery.event).unwrap();
        told.extend(effects.tells.into_iter().map(|(_, t)| t));
        if !told.is_empty() {
            break;
        }
    }
    assert_eq!(told, ["echo: ping"]);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_background_task_resumes_a_coroutine() {
    let Some(wasm) = built() else { return };
    let (queue, mut events) = mpsc::unbounded_channel();
    let mut plugin = sandbox(&wasm);
    plugin.attach_background(tuff_plugin_wasm::net::Events::new("io", 1, 8, queue));
    let (result, effects) = plugin.command(alex(), "io upper shout it").unwrap();
    assert_eq!(result, Ok(1));
    assert!(effects.replies.is_empty());
    let delivery = tokio::time::timeout(Duration::from_secs(10), events.recv())
        .await
        .unwrap()
        .unwrap();
    let effects = plugin.io(&delivery.event).unwrap();
    let told: Vec<&str> = effects.tells.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(told, ["upper: SHOUT IT"]);
}

#[tuff_core::watchdog]
#[test]
fn background_without_a_host_is_refused_at_once() {
    let Some(wasm) = built() else { return };
    let mut plugin = sandbox(&wasm);
    let (_, effects) = plugin.command(alex(), "io upper x").unwrap();
    assert_eq!(effects.replies.len(), 1);
    assert!(
        plain(&effects.replies[0]).starts_with("&cbackground failed"),
        "{effects:?}"
    );
}
