//! The rust-discord-relay example against an in-process webhook server:
//! posts for chat, joins and leaves, 429 handling and mention suppression.

use super::*;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tuff_plugin_wasm::wit::bindings::types::{ChatMessage, Event};

const BUILD: &str =
    "examples/rust-discord-relay/target/wasm32-unknown-unknown/release/discord_relay.wasm";

fn built() -> Vec<u8> {
    let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
    assert!(
        crate::worldedit_wasm::build_example(&root.join("examples/rust-discord-relay")),
        "building discord_relay.wasm failed"
    );
    std::fs::read(root.join(BUILD)).unwrap()
}

fn leb(mut n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

fn read_leb(bytes: &[u8], at: &mut usize) -> usize {
    let (mut value, mut shift) = (0usize, 0);
    loop {
        let byte = bytes[*at];
        *at += 1;
        value |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
        shift += 7;
    }
}

/// Rewrites the manifest's http request to the local webhook server.
fn retarget(module: &[u8], base: &str) -> Vec<u8> {
    let mut out = module[..8].to_vec();
    let mut at = 8;
    while at < module.len() {
        let id = module[at];
        at += 1;
        let size = read_leb(module, &mut at);
        let body = &module[at..at + size];
        at += size;
        let mut inner = 0;
        let name_len = read_leb(body, &mut inner);
        let named = &body[inner..inner + name_len];
        if id == 0 && named == b"tuff-plugin" {
            let text = String::from_utf8(body[inner + name_len..].to_vec()).unwrap();
            let text = text.replace(
                "discord.com/api/webhooks/",
                &format!("{base}/api/webhooks/ private"),
            );
            let mut new_body = body[..inner + name_len].to_vec();
            new_body.extend_from_slice(text.as_bytes());
            out.push(0);
            out.extend(leb(new_body.len()));
            out.extend(new_body);
        } else {
            out.push(id);
            out.extend(leb(size));
            out.extend_from_slice(body);
        }
    }
    out
}

type Bodies = Arc<Mutex<Vec<String>>>;

async fn webhook(statuses: Vec<(u16, &'static str)>) -> (SocketAddr, Bodies) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let bodies: Bodies = Arc::default();
    let seen = bodies.clone();
    let script = Arc::new(Mutex::new(statuses.into_iter()));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let seen = seen.clone();
            let script = script.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 2048];
                let body = loop {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).into_owned();
                    let Some((head, rest)) = text.split_once("\r\n\r\n") else {
                        continue;
                    };
                    let length = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if rest.len() >= length {
                        break rest.to_owned();
                    }
                };
                seen.lock().unwrap().push(body);
                let (status, extra) = script.lock().unwrap().next().unwrap_or((204, ""));
                let reply = format!(
                    "HTTP/1.1 {status} X\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (addr, bodies)
}

async fn posted(world: &TestWorld, bodies: &Bodies, want: usize) -> Vec<String> {
    for _ in 0..500 {
        world.ticks(2).await;
        host(world).sync().await;
        if bodies.lock().unwrap().len() >= want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let seen = bodies.lock().unwrap().clone();
    assert!(seen.len() >= want, "wanted {want} posts, got {seen:?}");
    seen
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_posts_chat_joins_and_leaves_through_a_429() {
    let dir = TestDir::new();
    let (addr, bodies) = webhook(vec![(204, ""), (429, "Retry-After: 0.05\r\n"), (204, "")]).await;
    let base = format!("http://127.0.0.1:{}", addr.port());
    let files = plugins_dir(dir.path()).join("discord-relay").join("files");
    std::fs::create_dir_all(&files).unwrap();
    install(dir.path(), "discord-relay.wasm", &retarget(&built(), &base));
    let world = start_sqlite(dir.path(), open_sqlite(dir.path())).await;
    let url = format!("{base}/api/webhooks/1/x");
    world
        .shared
        .config
        .load_saved([("plugins.discord-relay.webhook", url.as_str())]);
    console(&world, "tuff plugin grant discord-relay files 1 MiB").await;
    console(
        &world,
        &format!("tuff plugin grant discord-relay http {base}/api/webhooks/ private"),
    )
    .await;
    console(&world, "tuff plugin reload discord-relay").await;

    let seen = posted(&world, &bodies, 1).await;
    assert!(seen[0].contains("Server relay online."), "{seen:?}");

    let steve = Player::new(5, "Steve");
    let chat = Event::Chat(ChatMessage {
        player: steve.to_wit(),
        message: "hi @everyone *x*".into(),
    });
    host(&world).event(chat, Some(steve.clone()));
    host(&world).event(Event::Join(steve.to_wit()), Some(steve.clone()));
    host(&world).left(steve.clone()).await;

    let seen = posted(&world, &bodies, 5).await;
    assert!(
        seen[1].contains("**Steve**: hi @\u{200b}everyone"),
        "{seen:?}"
    );
    assert_eq!(seen[1], seen[2], "the 429 is retried with the same body");
    assert!(seen[3].contains("Steve joined the game"), "{seen:?}");
    assert!(seen[4].contains("Steve left the game"), "{seen:?}");
    for body in &seen {
        assert!(
            body.contains("\"allowed_mentions\":{\"parse\":[]}"),
            "{body}"
        );
        assert!(!body.contains("@everyone"), "{body}");
    }
    world.shutdown().await;
}
