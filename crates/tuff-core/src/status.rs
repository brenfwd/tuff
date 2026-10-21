//! Server list state: MOTD, favicon, and player counts, owned by [`StatusActor`].

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use bytes::Bytes;
use image::ImageFormat;
use image::imageops::FilterType;
use serde_json::json;
use tokio::sync::{mpsc, watch};

use crate::protocol::status::StatusResponse;
use crate::protocol::{PROTOCOL_VERSION, VERSION_NAME, encode_packet};
use tuff_item::text::TextComponent;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const FAVICON_SIZE: u32 = 64;

/// A 64x64 PNG server icon, stored pre-encoded as a data URI. Images of any
/// other size are resized on load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Favicon {
    data_uri: String,
}

impl Favicon {
    pub fn from_png(png: &[u8]) -> Result<Self> {
        ensure!(
            png.len() >= 24 && png[..8] == PNG_SIGNATURE && &png[12..16] == b"IHDR",
            "favicon is not a PNG image"
        );
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        if width == FAVICON_SIZE && height == FAVICON_SIZE {
            return Ok(Self::from_encoded(png));
        }
        let resized = image::load_from_memory_with_format(png, ImageFormat::Png)
            .context("failed to decode favicon")?
            .resize_exact(FAVICON_SIZE, FAVICON_SIZE, FilterType::Lanczos3);
        let mut encoded = Cursor::new(Vec::new());
        resized
            .write_to(&mut encoded, ImageFormat::Png)
            .context("failed to encode resized favicon")?;
        Ok(Self::from_encoded(encoded.get_ref()))
    }

    fn from_encoded(png: &[u8]) -> Self {
        Self {
            data_uri: format!("data:image/png;base64,{}", BASE64.encode(png)),
        }
    }

    pub async fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_png(&tokio::fs::read(path).await?)
    }

    /// The PNG bytes behind the data URI.
    pub fn png(&self) -> Vec<u8> {
        let encoded = self
            .data_uri
            .strip_prefix("data:image/png;base64,")
            .unwrap_or_default();
        BASE64.decode(encoded).unwrap_or_default()
    }

    pub fn data_uri(&self) -> &str {
        &self.data_uri
    }
}

/// Vanilla shows at most this many names when hovering the player count.
const MAX_SAMPLE: usize = 12;
const ANONYMOUS_NAME: &str = "Anonymous Player";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnlinePlayer {
    pub name: String,
    pub uuid: u128,
    /// The client's "Allow Server Listings" option; unlisted players are
    /// counted but shown anonymously.
    pub listed: bool,
}

fn dashed_uuid(uuid: u128) -> String {
    let hex = format!("{uuid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[derive(Debug, Clone)]
pub struct ServerListInfo {
    pub motd: TextComponent,
    pub favicon: Option<Favicon>,
    pub max_players: u32,
    pub players: Vec<OnlinePlayer>,
    pub hide_players: bool,
    pub enforces_secure_chat: bool,
}

impl ServerListInfo {
    pub fn to_json(&self) -> String {
        let sample: Vec<_> = self
            .players
            .iter()
            .take(MAX_SAMPLE)
            .map(|player| {
                if player.listed {
                    json!({ "name": player.name, "id": dashed_uuid(player.uuid) })
                } else {
                    json!({ "name": ANONYMOUS_NAME, "id": dashed_uuid(0) })
                }
            })
            .collect();
        let mut status = json!({
            "version": { "name": VERSION_NAME, "protocol": PROTOCOL_VERSION },
            "players": { "max": self.max_players, "online": self.players.len(), "sample": sample },
            "description": self.motd,
            "enforcesSecureChat": self.enforces_secure_chat,
        });
        if self.hide_players {
            status["players"] = json!({ "max": self.max_players, "online": self.players.len() });
        }
        if let Some(favicon) = &self.favicon {
            status["favicon"] = favicon.data_uri().into();
        }
        status.to_string()
    }

    /// The full framed Status Response packet, ready to write to an unencrypted socket.
    fn to_framed_response(&self) -> Bytes {
        encode_packet(&StatusResponse {
            json: self.to_json(),
        })
        .finish()
    }
}

#[derive(Debug)]
pub enum StatusCommand {
    SetMotd(TextComponent),
    SetFavicon(Option<Favicon>),
    SetMaxPlayers(u32),
    SetPlayers(Vec<OnlinePlayer>),
    SetPolicy {
        hide_players: bool,
        enforces_secure_chat: bool,
    },
    /// `hide-online-players` changed.
    SetHidePlayers(bool),
    #[cfg(test)]
    Panic,
}

/// Owns [`ServerListInfo`]. Mutations arrive over MPSC; after each one the
/// actor publishes a pre-framed Status Response through a `watch` channel so
/// that connections can answer pings without contacting the actor.
pub struct StatusActor {
    info: ServerListInfo,
    commands: mpsc::Receiver<StatusCommand>,
    snapshot: watch::Sender<Bytes>,
    info_watch: watch::Sender<Arc<ServerListInfo>>,
}

impl StatusActor {
    pub fn new(
        info: ServerListInfo,
        commands: mpsc::Receiver<StatusCommand>,
    ) -> (Self, watch::Receiver<Bytes>) {
        let (snapshot, snapshot_rx) = watch::channel(info.to_framed_response());
        let (info_watch, _) = watch::channel(Arc::new(info.clone()));
        let actor = Self {
            info,
            commands,
            snapshot,
            info_watch,
        };
        (actor, snapshot_rx)
    }

    /// The current list info, for protocols other than the status ping.
    pub fn info_receiver(&self) -> watch::Receiver<Arc<ServerListInfo>> {
        self.info_watch.subscribe()
    }

    pub async fn run(&mut self) {
        while let Some(command) = self.commands.recv().await {
            match command {
                #[cfg(test)]
                StatusCommand::Panic => panic!("test panic"),
                StatusCommand::SetMotd(motd) => self.info.motd = motd,
                StatusCommand::SetFavicon(favicon) => self.info.favicon = favicon,
                StatusCommand::SetMaxPlayers(max) => self.info.max_players = max,
                StatusCommand::SetPlayers(players) => self.info.players = players,
                StatusCommand::SetPolicy {
                    hide_players,
                    enforces_secure_chat,
                } => {
                    self.info.hide_players = hide_players;
                    self.info.enforces_secure_chat = enforces_secure_chat;
                }
                StatusCommand::SetHidePlayers(hide) => self.info.hide_players = hide,
            }
            self.snapshot.send_replace(self.info.to_framed_response());
            self.info_watch.send_replace(Arc::new(self.info.clone()));
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut png = PNG_SIGNATURE.to_vec();
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png
    }

    #[tuff_core::watchdog]
    #[test]
    fn favicon_accepts_64x64_png() {
        let favicon = Favicon::from_png(&png_header(64, 64)).unwrap();
        assert!(
            favicon
                .data_uri()
                .starts_with("data:image/png;base64,iVBORw0KGgo")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn favicon_resizes_other_sizes_to_64x64() {
        for (width, height) in [(500, 500), (16, 16), (128, 32)] {
            let mut png = Cursor::new(Vec::new());
            image::RgbaImage::new(width, height)
                .write_to(&mut png, ImageFormat::Png)
                .unwrap();
            let favicon = Favicon::from_png(png.get_ref()).unwrap();
            let encoded = BASE64
                .decode(
                    favicon
                        .data_uri()
                        .strip_prefix("data:image/png;base64,")
                        .unwrap(),
                )
                .unwrap();
            let decoded = image::load_from_memory_with_format(&encoded, ImageFormat::Png).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (64, 64));
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn favicon_rejects_invalid_images() {
        assert!(Favicon::from_png(&png_header(32, 32)).is_err());
        assert!(Favicon::from_png(b"GIF89a not a png at all!").is_err());
        assert!(Favicon::from_png(&[]).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn status_json_shape() {
        let info = ServerListInfo {
            motd: TextComponent::new("Hello").color("gold"),
            favicon: Some(Favicon::from_png(&png_header(64, 64)).unwrap()),
            max_players: 20,
            hide_players: false,
            enforces_secure_chat: false,
            players: vec![
                OnlinePlayer {
                    name: "Notch".into(),
                    uuid: 0x069a79f444e94726a5befca90e38aaf5,
                    listed: true,
                },
                OnlinePlayer {
                    name: "Hidden".into(),
                    uuid: 1,
                    listed: false,
                },
            ],
        };
        let value: serde_json::Value = serde_json::from_str(&info.to_json()).unwrap();
        assert_eq!(value["version"]["protocol"], PROTOCOL_VERSION);
        assert_eq!(value["version"]["name"], VERSION_NAME);
        assert_eq!(value["players"]["max"], 20);
        assert_eq!(value["players"]["online"], 2);
        assert_eq!(value["players"]["sample"][0]["name"], "Notch");
        assert_eq!(
            value["players"]["sample"][0]["id"],
            "069a79f4-44e9-4726-a5be-fca90e38aaf5"
        );
        assert_eq!(value["players"]["sample"][1]["name"], ANONYMOUS_NAME);
        assert_eq!(value["description"]["text"], "Hello");
        assert_eq!(value["description"]["color"], "gold");
        assert!(
            value["favicon"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png")
        );
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn actor_publishes_updates() {
        let (tx, rx) = mpsc::channel(4);
        let info = ServerListInfo {
            motd: "before".into(),
            favicon: None,
            max_players: 1,
            hide_players: false,
            enforces_secure_chat: false,
            players: Vec::new(),
        };
        let (mut actor, mut snapshot) = StatusActor::new(info, rx);
        tokio::spawn(async move { actor.run().await });
        assert!(contains(&snapshot.borrow_and_update(), "before"));

        tx.send(StatusCommand::SetMotd("after".into()))
            .await
            .unwrap();
        snapshot.changed().await.unwrap();
        assert!(contains(&snapshot.borrow_and_update(), "after"));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panic_restarts_the_actor_with_its_info() {
        let (tx, rx) = mpsc::channel(4);
        let info = ServerListInfo {
            motd: "before".into(),
            favicon: None,
            max_players: 1,
            hide_players: false,
            enforces_secure_chat: false,
            players: Vec::new(),
        };
        let (actor, mut snapshot) = StatusActor::new(info, rx);
        tokio::spawn(tuff_supervise::restarting(
            "the status actor",
            actor,
            |actor| Box::pin(actor.run()),
        ));
        tx.send(StatusCommand::SetMotd("kept".into()))
            .await
            .unwrap();
        snapshot.changed().await.unwrap();
        tx.send(StatusCommand::Panic).await.unwrap();
        tx.send(StatusCommand::SetMaxPlayers(9)).await.unwrap();
        snapshot.changed().await.unwrap();
        let bytes = snapshot.borrow_and_update().clone();
        assert!(contains(&bytes, "kept"));
        assert!(contains(&bytes, "\"max\":9"));
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
    }
}
