// Shared by several test crates, each of which uses only part of it.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use bytes::Bytes;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tuff::net::connection::Connection;
use tuff::net::crypto::minecraft_hex_digest;
use tuff::protocol::PROTOCOL_VERSION;
use tuff::protocol::handshake::{Intent, Intention};
use tuff::protocol::login::{EncryptionRequest, EncryptionResponse, LoginDisconnect, LoginStart};
use tuff::protocol::status::{PingRequest, PongResponse, StatusRequest, StatusResponse};
use tuff::status::Favicon;
use tuff::{Server, ServerHandle};

pub const TEST_UUID: &str = "069a79f444e94726a5befca90e38aaf5";

/// A minimal PNG signature and IHDR chunk for a 64x64 image.
pub fn test_png() -> Vec<u8> {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&64u32.to_be_bytes());
    png.extend_from_slice(&64u32.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0]);
    png
}

/// A unique directory under the system temp dir, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "minecraft-it-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct TestServer {
    pub handle: ServerHandle,
    pub addr: SocketAddr,
    pub task: JoinHandle<Result<()>>,
    /// Kept so the instance directory lives as long as the server.
    pub _instance: TempDir,
}

pub async fn start_server(bind: &str, session_url: &str) -> TestServer {
    start_server_in(TempDir::new(), bind, session_url, Some(false)).await
}

/// Starts a server in an existing instance directory (so tests can write
/// `ops.json` and friends first); `whitelist` overrides `server.properties`.
pub async fn start_server_in(
    instance: TempDir,
    bind: &str,
    session_url: &str,
    whitelist: Option<bool>,
) -> TestServer {
    start_server_with_features(
        instance,
        bind,
        session_url,
        whitelist,
        tuff::vanilla_features::all(),
    )
    .await
}

/// Like [`start_server_in`] with `plugins` loaded instead of the vanilla set.
pub async fn start_server_with_features(
    instance: TempDir,
    bind: &str,
    session_url: &str,
    whitelist: Option<bool>,
    features: Vec<Box<dyn tuff::feature::Feature>>,
) -> TestServer {
    let server = Server::builder()
        .features(features)
        .instance_dir(instance.path().to_path_buf())
        .binds(vec![bind.to_string()])
        .session_server_url(session_url)
        .profile_api_url(session_url)
        .motd("Test MOTD")
        .favicon(Favicon::from_png(&test_png()).unwrap())
        .maybe_whitelist(whitelist)
        .build();
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    TestServer {
        handle,
        addr,
        task,
        _instance: instance,
    }
}

/// Emulates the Mojang session server: clients `join` with a server hash, and
/// `hasJoined` succeeds only for a matching username and hash.
#[derive(Clone, Default)]
pub struct MockSessionServer {
    joins: Arc<Mutex<HashMap<String, String>>>,
    /// Accounts the profile lookup knows, by lower-cased name.
    profiles: Arc<Mutex<HashMap<String, (String, String)>>>,
    lookups: Arc<Mutex<usize>>,
    /// The `ip` parameter of every `hasJoined` request, in order.
    ips: Arc<Mutex<Vec<Option<String>>>>,
}

impl MockSessionServer {
    pub async fn start() -> (Self, String) {
        let mock = Self::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let serving = mock.clone();
        tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                tokio::spawn(serving.clone().serve(socket));
            }
        });
        (mock, url)
    }

    pub fn join(&self, username: &str, server_hash: &str) {
        self.joins
            .lock()
            .unwrap()
            .insert(username.to_string(), server_hash.to_string());
    }

    /// Makes the profile lookup resolve `name` to `uuid` (undashed hex).
    pub fn add_profile(&self, name: &str, uuid: &str) {
        self.profiles
            .lock()
            .unwrap()
            .insert(name.to_lowercase(), (name.to_string(), uuid.to_string()));
    }

    /// The `ip` each `hasJoined` request carried, in order.
    pub fn joined_ips(&self) -> Vec<Option<String>> {
        self.ips.lock().unwrap().clone()
    }

    /// How many profile lookup requests have been answered.
    pub fn lookups(&self) -> usize {
        *self.lookups.lock().unwrap()
    }

    fn lookup_response(&self, body: &str) -> String {
        let names: Vec<String> = serde_json::from_str(body).unwrap();
        let profiles = self.profiles.lock().unwrap();
        let found: Vec<serde_json::Value> = names
            .iter()
            .filter_map(|name| profiles.get(&name.to_lowercase()))
            .map(|(name, id)| serde_json::json!({"id": id, "name": name}))
            .collect();
        *self.lookups.lock().unwrap() += 1;
        let body = serde_json::to_string(&found).unwrap();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    async fn serve(self, mut socket: TcpStream) {
        let mut request = Vec::new();
        let mut chunk = [0u8; 1024];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = socket.read(&mut chunk).await.unwrap();
            if n == 0 {
                return;
            }
            request.extend_from_slice(&chunk[..n]);
        }
        let head_end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let head = String::from_utf8(request[..head_end].to_vec()).unwrap();
        let length: usize = head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
            .map_or(0, |(_, value)| value.trim().parse().unwrap());
        while request.len() < head_end + length {
            let n = socket.read(&mut chunk).await.unwrap();
            if n == 0 {
                return;
            }
            request.extend_from_slice(&chunk[..n]);
        }
        let request = String::from_utf8(request).unwrap();
        if head.starts_with("POST /minecraft/profile/lookup/bulk/byname ") {
            let response = self.lookup_response(&request[head_end..]);
            socket.write_all(response.as_bytes()).await.unwrap();
            return;
        }
        let target = request.split_whitespace().nth(1).unwrap();
        if let Some(id) = target
            .strip_prefix("/session/minecraft/profile/")
            .map(|rest| rest.split('?').next().unwrap())
        {
            let found = self
                .profiles
                .lock()
                .unwrap()
                .values()
                .find(|(_, uuid)| uuid == id)
                .cloned();
            let response = match found {
                Some((name, uuid)) => {
                    let body = format!(
                        r#"{{"id":"{uuid}","name":"{name}","properties":[{{"name":"textures","value":"dGV4","signature":"c2ln"}}]}}"#
                    );
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                }
                None => "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_string(),
            };
            socket.write_all(response.as_bytes()).await.unwrap();
            return;
        }
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let params: HashMap<&str, &str> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .collect();

        if path == "/session/minecraft/hasJoined" {
            self.ips
                .lock()
                .unwrap()
                .push(params.get("ip").map(|ip| (*ip).to_owned()));
        }
        let joined = path == "/session/minecraft/hasJoined"
            && self
                .joins
                .lock()
                .unwrap()
                .get(params["username"])
                .map(String::as_str)
                == Some(params["serverId"]);
        let response = if joined {
            let body = format!(
                r#"{{"id":"{TEST_UUID}","name":"{}","properties":[]}}"#,
                params["username"]
            );
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        } else {
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_string()
        };
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
    }
}

pub async fn connect(addr: SocketAddr, protocol_version: i32, intent: Intent) -> Connection {
    let mut conn = Connection::new(TcpStream::connect(addr).await.unwrap());
    conn.send(&Intention {
        protocol_version,
        server_address: addr.ip().to_string(),
        server_port: addr.port(),
        intent,
    })
    .await
    .unwrap();
    conn
}

/// Performs a server list ping, returning the parsed status JSON.
pub async fn ping(addr: SocketAddr) -> serde_json::Value {
    let mut conn = connect(addr, PROTOCOL_VERSION, Intent::Status).await;
    conn.send(&StatusRequest).await.unwrap();
    let status: StatusResponse = conn.expect().await.unwrap();
    conn.send(&PingRequest {
        timestamp: 0x1234_5678,
    })
    .await
    .unwrap();
    let pong: PongResponse = conn.expect().await.unwrap();
    assert_eq!(pong.timestamp, 0x1234_5678);
    serde_json::from_str(&status.json).unwrap()
}

/// Performs the client side of an online-mode login and returns the disconnect reason JSON.
/// When `session` is given, the client registers its join like a vanilla client would.
pub async fn login(
    addr: SocketAddr,
    username: &str,
    session: Option<&MockSessionServer>,
) -> serde_json::Value {
    let mut conn = encrypted_login(addr, username, session).await;
    read_disconnect(&mut conn).await
}

/// Runs the client side of an online-mode login up to and including enabling
/// encryption, returning the connection for the caller to continue.
pub async fn encrypted_login(
    addr: SocketAddr,
    username: &str,
    session: Option<&MockSessionServer>,
) -> Connection {
    let mut conn = connect(addr, PROTOCOL_VERSION, Intent::Login).await;
    conn.send(&LoginStart {
        name: username.to_string(),
        uuid: 0,
    })
    .await
    .unwrap();

    let request: EncryptionRequest = conn.expect().await.unwrap();
    assert!(request.should_authenticate);
    let public_key = RsaPublicKey::from_public_key_der(&request.public_key).unwrap();
    let shared_secret: [u8; 16] = rand::random();
    if let Some(session) = session {
        session.join(
            username,
            &minecraft_hex_digest(&request.server_id, &shared_secret, &request.public_key),
        );
    }

    let mut rng = rand::rngs::OsRng;
    conn.send(&EncryptionResponse {
        shared_secret: Bytes::from(
            public_key
                .encrypt(&mut rng, Pkcs1v15Encrypt, &shared_secret)
                .unwrap(),
        ),
        verify_token: Bytes::from(
            public_key
                .encrypt(&mut rng, Pkcs1v15Encrypt, &request.verify_token)
                .unwrap(),
        ),
    })
    .await
    .unwrap();
    conn.enable_encryption(&shared_secret);
    conn
}

pub async fn read_disconnect(conn: &mut Connection) -> serde_json::Value {
    let disconnect: LoginDisconnect = conn.expect().await.unwrap();
    serde_json::from_str(&disconnect.reason_json).unwrap()
}

/// Concatenates all `text` fields of a text component tree.
pub fn plain_text(component: &serde_json::Value) -> String {
    let mut out = component["text"].as_str().unwrap_or_default().to_string();
    for child in component["extra"].as_array().into_iter().flatten() {
        out.push_str(&plain_text(child));
    }
    out
}
