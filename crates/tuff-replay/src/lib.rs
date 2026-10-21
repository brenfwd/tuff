//! Replays a packet recording made by `/tuff tracing` to any number of
//! viewers. Each viewer logs in (offline mode), is greeted with the recorded
//! configuration phase and then watches the recorded play stream in
//! spectator mode with its own playhead, controlled by `/replay`. No world
//! is loaded: the recording is the world.

pub mod controls;
pub mod corpus;
pub mod playhead;
pub mod reduce;
pub mod script;
pub mod shared;
pub mod timeline;
pub mod transform;
pub mod viewer;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tuff_core::auth::{GameProfile, offline_uuid};
use tuff_core::net::connection::{Connection, ConnectionReader};
use tuff_core::protocol::configuration::{self, FinishConfiguration, LoginSuccess};
use tuff_core::protocol::handshake::{Intent, Intention};
use tuff_core::protocol::login::{LoginAcknowledged, LoginStart, SetCompression};
use tuff_core::protocol::play::{self, BossAction, BossEvent, KeepAlive, SystemChat};
use tuff_core::protocol::play::{CommandSuggestions, Commands};
use tuff_core::protocol::status::{PingRequest, PongResponse, StatusRequest, StatusResponse};
use tuff_item::text::TextComponent;

pub use script::Script;
use transform::{encode, frame};
use viewer::{Viewer, clock};

const COMPRESSION_THRESHOLD: usize = 256;
const TICK: Duration = Duration::from_millis(50);
const KEEP_ALIVE_TICKS: u64 = 200;
const BAR_TICKS: u64 = 10;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(30);
const INBOUND_QUEUE: usize = 256;
/// Total decompressed packet bytes a replay will load.
pub const MAX_RECORDING_BYTES: usize = 2 * 1024 * 1024 * 1024;
const BAR_UUID: u128 = 0x7475_6666_7265_706c_6179_0000_0000_0001;

/// Serves `script` on every listener until the task is dropped.
pub async fn serve(listeners: Vec<TcpListener>, script: Arc<Script>) {
    let next_id = Arc::new(AtomicI32::new(i32::MAX));
    let shared = shared::new();
    let mut tasks = tokio::task::JoinSet::new();
    for listener in listeners {
        let script = script.clone();
        let next_id = next_id.clone();
        let shared = shared.clone();
        tasks.spawn(async move {
            loop {
                let Ok((socket, addr)) = listener.accept().await else {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                let script = script.clone();
                let id = next_id.fetch_sub(1, Ordering::Relaxed);
                let shared = shared.clone();
                tokio::spawn(async move {
                    if let Err(err) = viewer_connection(socket, script, id, shared).await {
                        log::info!("replay viewer {addr} left: {err:#}");
                    }
                });
            }
        });
    }
    while tasks.join_next().await.is_some() {}
}

/// Binds `binds` and serves; returns the bound addresses and the task.
pub async fn start(
    binds: &[String],
    script: Arc<Script>,
) -> Result<(Vec<SocketAddr>, tokio::task::JoinHandle<()>)> {
    let listeners = tuff_core::net::tcp_server::bind_all(binds).await?;
    let addrs = listeners
        .iter()
        .map(|l| l.local_addr())
        .collect::<std::io::Result<Vec<_>>>()?;
    for addr in addrs.iter().filter(|a| !a.ip().is_loopback()) {
        log::warn!(
            "replay bound to non-loopback {addr}: anyone who can reach it can join under any name and see the recording, including chat"
        );
    }
    Ok((addrs, tokio::spawn(serve(listeners, script))))
}

async fn viewer_connection(
    socket: TcpStream,
    script: Arc<Script>,
    viewer_id: i32,
    shared: shared::Shared,
) -> Result<()> {
    socket.set_nodelay(true)?;
    let conn = tokio::time::timeout(LOGIN_TIMEOUT, greet(Connection::new(socket), &script))
        .await
        .context("timed out")??;
    let Some((conn, name)) = conn else {
        return Ok(());
    };
    log::info!("{name} is watching the replay of {}", script.player);
    watch(conn, script, viewer_id, shared).await
}

async fn greet(mut conn: Connection, script: &Script) -> Result<Option<(Connection, String)>> {
    let intention: Intention = conn.expect().await?;
    if intention.intent == Intent::Status {
        conn.expect::<StatusRequest>().await?;
        conn.send(&StatusResponse {
            json: controls::status_json(&script.player, 0),
        })
        .await?;
        let ping: PingRequest = conn.expect().await?;
        conn.send(&PongResponse {
            timestamp: ping.timestamp,
        })
        .await?;
        return Ok(None);
    }
    if intention.intent != Intent::Login {
        return Ok(None);
    }
    let start: LoginStart = conn.expect().await?;
    conn.send(&SetCompression {
        threshold: COMPRESSION_THRESHOLD as i32,
    })
    .await?;
    conn.enable_compression(COMPRESSION_THRESHOLD);
    let profile = GameProfile {
        id: offline_uuid(&start.name),
        name: start.name.clone(),
        properties: Vec::new(),
    };
    conn.send(&LoginSuccess {
        profile: &profile,
        session_id: rand::random(),
    })
    .await?;
    conn.expect::<LoginAcknowledged>().await?;
    let compression = conn.compression();
    for payload in &script.config {
        conn.send_raw(&frame(payload, compression)).await?;
    }
    conn.send(&FinishConfiguration).await?;
    loop {
        let payload = conn
            .read_packet()
            .await?
            .context("connection closed during configuration")?;
        if configuration::Serverbound::decode(payload)?
            == configuration::Serverbound::AcknowledgeFinish
        {
            return Ok(Some((conn, start.name)));
        }
    }
}

const SNEAK: u8 = 0x20;

async fn read_inbound(mut reader: ConnectionReader, inbound: mpsc::Sender<play::Serverbound>) {
    while let Ok(Some(payload)) = reader.read_packet().await {
        let Ok(packet) = play::Serverbound::decode(payload) else {
            continue;
        };
        if inbound.send(packet).await.is_err() {
            break;
        }
    }
}

fn bar(viewer: &Viewer, add: bool, out: &mut Vec<Bytes>) {
    let title = TextComponent::new(format!(
        "{} {} / {}",
        if viewer.head.paused() {
            "Paused"
        } else {
            "Replay"
        },
        clock(viewer.head.position()),
        clock(viewer.head.duration())
    ));
    let duration = viewer.head.duration().max(1) as f32;
    let progress = (viewer.head.position() as f32 / duration).clamp(0.0, 1.0);
    if add {
        out.push(encode(&BossEvent {
            uuid: BAR_UUID,
            action: BossAction::Add {
                title: &title,
                progress,
                color: 1,
                overlay: 0,
                flags: 0,
            },
        }));
        return;
    }
    out.push(encode(&BossEvent {
        uuid: BAR_UUID,
        action: BossAction::Title(&title),
    }));
    out.push(encode(&BossEvent {
        uuid: BAR_UUID,
        action: BossAction::Progress(progress),
    }));
}

async fn watch(
    conn: Connection,
    script: Arc<Script>,
    viewer_id: i32,
    shared: shared::Shared,
) -> Result<()> {
    let compression = conn.compression();
    let (reader, mut writer) = conn.split();
    let (inbound_tx, mut inbound) = mpsc::channel(INBOUND_QUEUE);
    let reading = tokio::spawn(read_inbound(reader, inbound_tx));
    let mut viewer = Viewer::new(script, viewer_id);
    viewer.attach(shared);
    let mut out = Vec::new();
    let mut ticks = 0u64;
    let mut interval = tokio::time::interval(TICK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = tokio::time::Instant::now();
    viewer.step(Duration::ZERO, &mut out);
    out.push(encode(&Commands {
        nodes: &controls::command_tree(),
        root: 0,
    }));
    let result = loop {
        tokio::select! {
            _ = interval.tick() => {
                let now = tokio::time::Instant::now();
                viewer.tick(now - last, &mut out);
                last = now;
                if ticks.is_multiple_of(KEEP_ALIVE_TICKS) {
                    out.push(encode(&KeepAlive(ticks as i64)));
                }
                if ticks.is_multiple_of(BAR_TICKS) {
                    bar(&viewer, ticks == 0, &mut out);
                }
                ticks += 1;
            }
            packet = inbound.recv() => match packet {
                Some(play::Serverbound::ChatCommand(line)) => {
                    let reply = viewer.command(&line, &mut out);
                    out.push(encode(&SystemChat {
                        content: &controls::chat_controls(reply),
                        overlay: false,
                    }));
                }
                Some(play::Serverbound::CommandSuggestion { id, text }) => {
                    if let Some((start, length, matches)) = viewer.suggest(&text) {
                        out.push(encode(&CommandSuggestions {
                            id,
                            start: start as i32,
                            length: length as i32,
                            matches: &matches,
                            tooltips: &[],
                        }));
                    }
                }
                Some(play::Serverbound::Spectate(Some(entity))) => viewer.spectate(entity, &mut out),
                Some(play::Serverbound::PlayerInput(flags)) if flags & SNEAK != 0 => {
                    viewer.release_camera(&mut out)
                }
                Some(play::Serverbound::SetCarriedItem(slot)) => {
                    if let Some(reply) = viewer.hotbar_select(slot, &mut out) {
                        out.push(controls::action_bar(reply));
                    }
                }
                Some(play::Serverbound::UseItem { .. }) => {
                    if let Some(reply) = viewer.hotbar_use(&mut out) {
                        out.push(controls::action_bar(reply));
                    }
                }
                Some(play::Serverbound::SetCreativeModeSlot { .. }) if viewer.hotbar() => {
                    viewer.send_hotbar(&mut out)
                }
                Some(_) => continue,
                None => break Ok(()),
            },
        }
        if out.is_empty() {
            continue;
        }
        let frames: Vec<Bytes> = out.drain(..).map(|p| frame(&p, compression)).collect();
        if let Err(err) = writer.send_batch(&frames).await {
            break Err(err);
        }
    };
    viewer.leave();
    reading.abort();
    result
}
