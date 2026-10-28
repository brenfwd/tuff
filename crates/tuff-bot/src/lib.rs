//! Scripted protocol clients for benchmarks and stress tests. A [`Bot`]
//! logs in, keeps its connection healthy (keep-alives, teleports, chunk
//! batches) and pursues [`Goal`]s one client tick at a time, so many bots
//! with different goals can run side by side, one task each.

mod goal;
pub mod mine;
pub mod pace;
pub mod packets;
pub mod parity;
pub mod report;
pub mod sprint;
pub mod terrain;
pub mod walk;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tuff_core::net::connection::{Connection, ConnectionReader, ConnectionWriter};
use tuff_core::protocol::codec::ReadExt;
use tuff_core::protocol::configuration::{
    self, ClientInformation, FinishConfiguration, KnownPack, KnownPacks, LoginSuccess,
};
use tuff_core::protocol::handshake::{Intent, Intention};
use tuff_core::protocol::login::{LoginAcknowledged, LoginStart, SetCompression};
use tuff_core::protocol::pack::{ConfigDisconnect, ConfigPush, PackStatus, PlayPush};
use tuff_core::protocol::play::PlayerActionStatus;
use tuff_core::protocol::play::{self, Movement};
use tuff_core::protocol::{Encode, PROTOCOL_VERSION, VERSION_NAME};
use tuff_vanilla_data::registry::EntityType;
use tuff_wire::{PacketReader, PacketWriter};
use tuff_world_model::pos::ChunkPos;

pub use goal::{FlyLine, Goal, Idle, Rng, Step, look_at};
pub use pace::{ServerClock, TickWindow};
pub use terrain::{Column, Terrain};

/// One client tick.
pub const TICK: Duration = Duration::from_millis(50);

/// The farthest a goal's move goes in one tick; a farther target is
/// approached over several ticks: vanilla refuses a server tick's moves
/// whose squared distance passes 100 per move packet, and two or three
/// client moves can land in one server tick. A lagging server lowers it
/// further ([`ServerClock::move_allowance`]).
pub const MAX_MOVE_PER_TICK: f64 = 5.0;

/// `target`, or the point `max` blocks from `from` towards it.
pub fn approach(from: [f64; 3], target: [f64; 3], max: f64) -> [f64; 3] {
    let delta: [f64; 3] = std::array::from_fn(|i| target[i] - from[i]);
    let distance = delta.iter().map(|d| d * d).sum::<f64>().sqrt();
    if distance <= max {
        return target;
    }
    std::array::from_fn(|i| from[i] + delta[i] * max / distance)
}

#[derive(Debug, Clone)]
pub struct BotConfig {
    pub name: String,
    pub view_distance: i8,
    /// Chunks per tick the bot asks the server to send.
    pub chunks_per_tick: f32,
    /// The final answer to a resource pack offer, sent after the
    /// intermediate statuses a real client reports first.
    pub pack_status: PackStatus,
    /// The answer to offers pushed in play, if different.
    pub play_pack_status: Option<PackStatus>,
    /// Keep a [`Terrain`] of the chunks held, for goals that walk or dig.
    pub terrain: bool,
    /// The lowest block y of the dimension the bot plays in.
    pub min_y: i32,
    /// Where the server's per-tick timings go: when set, the bot asks for
    /// the dedicated server tick time debug subscription on joining, which
    /// a server honours only while the bot is an operator. Unbounded so
    /// the packet reader never waits on the collector.
    pub tick_samples: Option<mpsc::UnboundedSender<TickSample>>,
    /// Ask for per-tick reports (the same subscription) to keep moves
    /// within each server tick's allowance; honoured only for operators.
    pub follow_ticks: bool,
    /// The client brand sent during configuration, as a real client does.
    pub brand: Option<String>,
    /// Play packet ids whose latest body (after the id) the bot keeps in
    /// [`BotState::last`], for tests that read fields it does not track.
    pub keep: Vec<i32>,
}

impl BotConfig {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            view_distance: 10,
            chunks_per_tick: 64.0,
            pack_status: PackStatus::Loaded,
            play_pack_status: None,
            terrain: false,
            min_y: -64,
            tick_samples: None,
            follow_ticks: false,
            brand: Some("vanilla".into()),
            keep: Vec::new(),
        }
    }
}

/// `debug_subscription` registry id of the dedicated server tick time.
pub const TICK_TIME_SUBSCRIPTION: i32 = 0;
const TICK_TIME_SAMPLE_TYPE: i32 = 0;
const MAX_SAMPLE_DIMENSIONS: i32 = 16;

/// One server tick as a Debug Sample packet reports it, in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickSample {
    pub received: Instant,
    /// From the start of this tick to the start of the next, idle included.
    pub full_ns: i64,
    /// The tick's own work, the figure tick time percentiles describe.
    pub tick_ns: i64,
    pub tasks_ns: i64,
    pub idle_ns: i64,
}

/// Reads a Debug Sample payload after its id; `None` for a sample type
/// other than tick time.
pub fn read_tick_sample(r: &mut PacketReader, received: Instant) -> Result<Option<TickSample>> {
    let count = r.read_var_int()?;
    anyhow::ensure!(
        (0..=MAX_SAMPLE_DIMENSIONS).contains(&count),
        "bad debug sample length {count}"
    );
    let mut values = [0i64; 4];
    for index in 0..count as usize {
        let value = r.read_long()?;
        if let Some(slot) = values.get_mut(index) {
            *slot = value;
        }
    }
    if r.read_var_int()? != TICK_TIME_SAMPLE_TYPE {
        return Ok(None);
    }
    let [full_ns, tick_ns, tasks_ns, idle_ns] = values;
    Ok(Some(TickSample {
        received,
        full_ns,
        tick_ns,
        tasks_ns,
        idle_ns,
    }))
}

/// The statuses a client sends on its way to `last`.
pub fn pack_answers(last: PackStatus) -> Vec<PackStatus> {
    match last {
        PackStatus::Loaded | PackStatus::FailedReload => {
            vec![PackStatus::Accepted, PackStatus::Downloaded, last]
        }
        PackStatus::FailedDownload | PackStatus::Downloaded => vec![PackStatus::Accepted, last],
        _ => vec![last],
    }
}

/// What the bot knows about itself, updated as packets arrive.
#[derive(Debug, Clone, Default)]
pub struct BotState {
    /// Times the server took the bot back to configuration and out again.
    pub reconfigurations: u64,
    /// Between Start Configuration and Finish Configuration.
    pub configuring: bool,
    pub position: [f64; 3],
    pub rotation: (f32, f32),
    /// Server-driven position changes seen so far (spawn, `/tp`, ...).
    pub teleports: u64,
    /// Chunks received in total, counted per batch.
    pub chunks: u64,
    pub last_chunks: Option<Instant>,
    /// Chunks the bot holds now.
    pub loaded: HashSet<ChunkPos>,
    /// Why the connection ended, once it has.
    pub disconnected: Option<String>,
    /// How many packets of each id arrived.
    pub received: HashMap<i32, u64>,
    /// The latest body of each packet id the config asked to keep.
    pub last: HashMap<i32, bytes::Bytes>,
    /// Every kept packet in arrival order, with its body.
    pub kept_order: Vec<(i32, bytes::Bytes)>,
    /// Every resource pack offer, configuration phase first.
    pub pack_offers: Vec<PackSeen>,
    /// The id of every configuration packet the server sent, in order.
    pub configuration: Vec<i32>,
    /// The bot's own entity id.
    pub entity_id: i32,
    pub health: f32,
    pub deaths: u64,
    /// Bytes read off the socket, as compressed on the wire.
    pub bytes: u64,
    /// Bytes of every packet after decompression.
    pub payload_bytes: u64,
    /// Decompressed bytes of chunk data packets alone.
    pub chunk_bytes: u64,
    pub packets: u64,
    /// Play packets received by kind, with their wire and decompressed size.
    pub tally: packets::PacketTally,
    /// The ground of the chunks held, when the config asks for it.
    pub terrain: Option<Terrain>,
    /// Entities the server tracks for the bot, by id.
    pub entities: HashMap<i32, Seen>,
    /// The container window open now.
    pub window: Option<i32>,
    pub windows_opened: u64,
    pub sent: Sent,
    /// A move with a position went out since the last Client Tick End; a
    /// server may refuse a second one in the same client tick.
    pub position_sent: bool,
    /// Moves with a position sent while pursuing goals.
    pub moves: u64,
    /// Teleports that arrived while pursuing a goal: the server refusing
    /// a move and putting the bot back, or otherwise moving it.
    pub setbacks: u64,
    /// The server's tick rate, from its time syncs.
    pub server_clock: ServerClock,
    /// Where the bot was at the server's last tick reports.
    pub tick_window: TickWindow,
    /// A goal is being pursued, so teleports are confirmed on the bot's
    /// next tick rather than at once.
    pub pursuing: bool,
    /// The newest teleport not confirmed yet.
    pub pending_teleport: Option<i32>,
    /// Every locator bar packet received, in order.
    pub waypoints: Vec<tuff_core::world::waypoints::TrackedWaypoint>,
}

/// An entity as the bot last saw it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seen {
    pub kind: Option<EntityType>,
    pub position: [f64; 3],
}

impl Seen {
    /// A mob that fights or flees, rather than an item, projectile,
    /// vehicle or player.
    pub fn is_mob(self) -> bool {
        use tuff_vanilla_data::entity::MobCategory;
        self.kind.is_some_and(|kind| {
            kind != EntityType::Player
                && matches!(
                    kind.category(),
                    MobCategory::Monster | MobCategory::Creature
                )
        })
    }
}

/// Gameplay packets a bot has sent, by kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sent {
    pub digs_started: u64,
    pub digs_finished: u64,
    pub uses_on_block: u64,
    pub uses: u64,
    pub attacks: u64,
    pub swings: u64,
    pub commands: u64,
    pub closes: u64,
}

impl Sent {
    fn count(&mut self, packet: &play::Serverbound) {
        use play::Serverbound as S;
        let counter = match packet {
            S::PlayerAction {
                status: PlayerActionStatus::StartDestroyBlock,
                ..
            } => &mut self.digs_started,
            S::PlayerAction {
                status: PlayerActionStatus::StopDestroyBlock,
                ..
            } => &mut self.digs_finished,
            S::UseItemOn { .. } => &mut self.uses_on_block,
            S::UseItem { .. } => &mut self.uses,
            S::Attack(_) => &mut self.attacks,
            S::Punch => &mut self.swings,
            S::ChatCommand(_) => &mut self.commands,
            S::ContainerClose(_) => &mut self.closes,
            _ => return,
        };
        *counter += 1;
    }
}

/// One resource pack push as the bot received it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSeen {
    pub id: u128,
    pub url: String,
    pub hash: String,
    pub required: bool,
    /// The offer came with a prompt message.
    pub prompted: bool,
    /// Pushed in play rather than in the configuration phase.
    pub in_play: bool,
}

fn read_push(r: &mut PacketReader, in_play: bool) -> Result<PackSeen> {
    Ok(PackSeen {
        id: r.read_uuid()?,
        url: r.read_string()?,
        hash: r.read_string()?,
        required: r.read_bool()?,
        prompted: r.read_bool()?,
        in_play,
    })
}

pub struct Bot {
    config: BotConfig,
    state: Arc<Mutex<BotState>>,
    out: mpsc::UnboundedSender<play::Serverbound>,
    tasks: [JoinHandle<()>; 2],
}

impl Bot {
    /// Logs in (offline mode) and waits until the server has placed the bot.
    pub async fn connect(addr: SocketAddr, config: BotConfig) -> Result<Self> {
        let (conn, pack_offers, configuration) = join(addr, &config).await?;
        let compression = conn.compression();
        let (reader, writer) = conn.split();
        // Unbounded like a real client's send path: the bot never waits on
        // the server to queue its own replies.
        let (out, outbox) = mpsc::unbounded_channel();
        // Configuration replies after the server sends the bot back there.
        let (raw, raw_outbox) = mpsc::unbounded_channel();
        let state = Arc::new(Mutex::new(BotState {
            pack_offers,
            configuration,
            health: 20.0,
            terrain: config.terrain.then(|| Terrain::new(config.min_y)),
            ..BotState::default()
        }));
        let tasks = [
            tokio::spawn(write(writer, outbox, raw_outbox, compression)),
            tokio::spawn(listen(
                reader,
                out.clone(),
                state.clone(),
                Replies {
                    chunks_per_tick: config.chunks_per_tick,
                    pack_status: config.play_pack_status.unwrap_or(config.pack_status),
                    tick_samples: config.tick_samples.clone(),
                    keep: config.keep.clone(),
                    raw,
                    compression,
                },
            )),
        ];
        let _ = out.send(play::Serverbound::PlayerLoaded);
        if config.tick_samples.is_some() || config.follow_ticks {
            let _ = out.send(play::Serverbound::DebugSubscriptionRequest(vec![
                TICK_TIME_SUBSCRIPTION,
            ]));
        }
        let bot = Self {
            config,
            state,
            out,
            tasks,
        };
        bot.wait_for(|state| state.teleports > 0).await?;
        Ok(bot)
    }

    /// Stops reading from the socket while keeping it open, like a client
    /// that has hung.
    pub fn stop_reading(&self) {
        self.tasks[1].abort();
    }

    /// Sends everything queued, then closes the connection.
    pub async fn close(&mut self) {
        self.tasks[1].abort();
        let (closed, _) = mpsc::unbounded_channel();
        drop(std::mem::replace(&mut self.out, closed));
        let _ = (&mut self.tasks[1]).await;
        let _ = (&mut self.tasks[0]).await;
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// A copy of everything the bot knows; [`Bot::with_state`] avoids
    /// copying the terrain and entity tables.
    pub fn state(&self) -> BotState {
        self.state.lock().unwrap().clone()
    }

    pub fn with_state<R>(&self, read: impl FnOnce(&BotState) -> R) -> R {
        read(&self.state.lock().unwrap())
    }

    pub fn send(&self, packet: play::Serverbound) {
        let _ = self.out.send(packet);
    }

    /// Polls until `done` holds, failing if the connection ends first.
    pub async fn wait_for(&self, mut done: impl FnMut(&BotState) -> bool) -> Result<()> {
        loop {
            {
                let state = self.state.lock().unwrap();
                if done(&state) {
                    return Ok(());
                }
                if let Some(reason) = &state.disconnected {
                    bail!("{} disconnected: {reason}", self.config.name);
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until the bot holds every chunk the server sends a player
    /// with `view_distance` at its current position.
    pub async fn wait_loaded(&self, view_distance: i32) -> Result<()> {
        self.wait_for(|state| {
            ChunkPos::of_position(state.position)
                .view(view_distance)
                .iter()
                .all(|chunk| state.loaded.contains(chunk))
        })
        .await
    }

    /// Runs `goal` one client tick at a time until it is done. Teleports
    /// arriving meanwhile are confirmed at the start of the next tick, as
    /// a client handles packets between its ticks, and the goal goes on
    /// from where the server put the bot.
    pub async fn pursue(&self, goal: &mut dyn Goal) -> Result<()> {
        let _pursuing = Pursuing::start(&self.state, &self.out);
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let mut state = self.state.lock().unwrap();
            if let Some(reason) = &state.disconnected {
                bail!("{} disconnected: {reason}", self.config.name);
            }
            let (packets, done) = client_tick(&mut state, goal, Instant::now());
            for packet in packets {
                self.send(packet);
            }
            drop(state);
            if done {
                return Ok(());
            }
        }
    }
}

/// One client tick of `goal`: the packets to send, in order, and whether
/// the goal is done. A teleport waiting is confirmed first, and the move
/// stays within what the server accepts now.
pub fn client_tick(
    state: &mut BotState,
    goal: &mut dyn Goal,
    now: Instant,
) -> (Vec<play::Serverbound>, bool) {
    let mut packets = Vec::new();
    if let Some(id) = state.pending_teleport.take() {
        packets.push(accept(state, id));
    }
    let mut step = goal.tick(state);
    let mut held = false;
    if let Some(target) = step.position {
        let limit = state
            .server_clock
            .move_allowance(now)
            .min(state.tick_window.allowance(state.position, now))
            .min(MAX_MOVE_PER_TICK);
        held = limit <= 0.0 && target != state.position;
        let position = approach(state.position, target, limit);
        step.position = Some(position);
        state.position = position;
    }
    if let Some(rotation) = step.rotation {
        state.rotation = rotation;
    }
    for action in &step.actions {
        state.sent.count(action);
    }
    let position = step.position.filter(|_| !state.position_sent);
    state.position_sent = false;
    state.moves += u64::from(position.is_some() && !held);
    // A held bot sends no move at all: even an unchanged one costs a
    // lagging server its per-move work while it drains its queue.
    if !held {
        packets.push(play::Serverbound::Move(Movement {
            horizontal_collision: false,
            position,
            rotation: step.rotation,
            on_ground: step.on_ground,
        }));
    }
    packets.extend(step.actions);
    packets.push(play::Serverbound::ClientTickEnd);
    (packets, step.done)
}

impl Drop for Bot {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn accept(state: &BotState, id: i32) -> play::Serverbound {
    play::Serverbound::AcceptTeleportation {
        id,
        position: state.position,
        rotation: state.rotation,
    }
}

/// Marks a goal as running for as long as it lives; on the way out,
/// however the pursuit ends, a teleport still waiting is confirmed.
struct Pursuing<'a> {
    state: &'a Mutex<BotState>,
    out: &'a mpsc::UnboundedSender<play::Serverbound>,
}

impl<'a> Pursuing<'a> {
    fn start(
        state: &'a Mutex<BotState>,
        out: &'a mpsc::UnboundedSender<play::Serverbound>,
    ) -> Self {
        state.lock().unwrap().pursuing = true;
        Self { state, out }
    }
}

impl Drop for Pursuing<'_> {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pursuing = false;
        if let Some(id) = state.pending_teleport.take() {
            confirm_teleport(&mut state, self.out, id);
        }
    }
}

/// Confirms teleport `id` at once, with a move to the new position as a
/// client sends after one.
fn confirm_teleport(state: &mut BotState, out: &mpsc::UnboundedSender<play::Serverbound>, id: i32) {
    let _ = out.send(accept(state, id));
    let _ = out.send(play::Serverbound::Move(Movement {
        horizontal_collision: false,
        position: (!state.position_sent).then_some(state.position),
        rotation: Some(state.rotation),
        on_ground: true,
    }));
    let _ = out.send(play::Serverbound::ClientTickEnd);
    state.position_sent = true;
}

fn frame(w: PacketWriter, compression: Option<usize>) -> Bytes {
    match compression {
        None => w.finish(),
        Some(threshold) => w
            .finish_compressed(threshold)
            .expect("in-memory zlib compression is infallible"),
    }
}

async fn join(
    addr: SocketAddr,
    config: &BotConfig,
) -> Result<(Connection, Vec<PackSeen>, Vec<i32>)> {
    use tuff_vanilla_data::packet::configuration::Clientbound as Config;
    let mut offers = Vec::new();
    let mut seen = Vec::new();
    let mut conn = Connection::new(TcpStream::connect(addr).await?);
    conn.send(&Intention {
        protocol_version: PROTOCOL_VERSION,
        server_address: addr.ip().to_string(),
        server_port: addr.port(),
        intent: Intent::Login,
    })
    .await?;
    conn.send(&LoginStart {
        name: config.name.clone(),
        uuid: 0,
    })
    .await?;
    let compression: SetCompression = conn.expect().await?;
    conn.enable_compression(compression.threshold as usize);
    let id = next_id(&mut conn).await?;
    anyhow::ensure!(
        id == LoginSuccess::ID,
        "expected login success, got {id:#04x}"
    );
    conn.send(&LoginAcknowledged).await?;

    let compression = conn.compression();
    let info = configuration::Serverbound::ClientInformation(
        ClientInformation {
            view_distance: config.view_distance,
            ..ClientInformation::default()
        },
        "en_us".to_owned(),
    );
    conn.send_raw(&frame(info.encode().unwrap(), compression))
        .await?;
    if let Some(brand) = &config.brand {
        let brand = configuration::Serverbound::Brand(brand.clone());
        conn.send_raw(&frame(brand.encode().unwrap(), compression))
            .await?;
    }
    loop {
        let payload = conn.read_packet().await?.context("server closed")?;
        let mut reader = PacketReader::new(payload);
        let id = reader.read_var_int()?;
        seen.push(id);
        if id == ConfigDisconnect::ID {
            anyhow::bail!("disconnected during configuration");
        }
        let reply = if id == Config::CodeOfConduct.id() {
            configuration::Serverbound::AcceptCodeOfConduct
        } else if id == ConfigPush::ID {
            let seen = read_push(&mut reader, false)?;
            let pack = seen.id;
            offers.push(seen);
            for status in pack_answers(config.pack_status) {
                let answer = configuration::Serverbound::ResourcePack {
                    id: pack,
                    status: Some(status),
                };
                conn.send_raw(&frame(answer.encode().unwrap(), compression))
                    .await?;
            }
            continue;
        } else if id == KnownPacks::ID {
            configuration::Serverbound::KnownPacks(vec![KnownPack {
                namespace: "minecraft".into(),
                id: "core".into(),
                version: VERSION_NAME.into(),
            }])
        } else if id == FinishConfiguration::ID {
            configuration::Serverbound::AcknowledgeFinish
        } else {
            continue;
        };
        conn.send_raw(&frame(reply.encode().unwrap(), compression))
            .await?;
        if id == FinishConfiguration::ID {
            return Ok((conn, offers, seen));
        }
    }
}

async fn next_id(conn: &mut Connection) -> Result<i32> {
    let payload = conn.read_packet().await?.context("server closed")?;
    Ok(PacketReader::new(payload).read_var_int()?)
}

async fn write(
    mut writer: ConnectionWriter,
    mut outbox: mpsc::UnboundedReceiver<play::Serverbound>,
    mut raw: mpsc::UnboundedReceiver<Bytes>,
    compression: Option<usize>,
) {
    loop {
        let framed = tokio::select! {
            biased;
            Some(framed) = raw.recv() => framed,
            packet = outbox.recv() => {
                let Some(packet) = packet else { break };
                let Some(w) = packet.encode() else {
                    continue;
                };
                frame(w, compression)
            }
        };
        if writer.send_raw(&framed).await.is_err() {
            break;
        }
    }
}

/// How the bot answers the server on its own.
#[derive(Clone)]
struct Replies {
    chunks_per_tick: f32,
    pack_status: PackStatus,
    tick_samples: Option<mpsc::UnboundedSender<TickSample>>,
    keep: Vec<i32>,
    raw: mpsc::UnboundedSender<Bytes>,
    compression: Option<usize>,
}

const PERFORM_RESPAWN: i32 = 0;

async fn listen(
    reader: ConnectionReader,
    out: mpsc::UnboundedSender<play::Serverbound>,
    state: Arc<Mutex<BotState>>,
    replies: Replies,
) {
    let reason = match respond(reader, &out, &state, replies).await {
        Ok(()) => "connection closed".to_string(),
        Err(error) => format!("{error:#}"),
    };
    state.lock().unwrap().disconnected = Some(reason);
}

async fn respond(
    mut reader: ConnectionReader,
    out: &mpsc::UnboundedSender<play::Serverbound>,
    state: &Mutex<BotState>,
    replies: Replies,
) -> Result<()> {
    while let Some(payload) = reader.read_packet().await? {
        let size = payload.len() as u64;
        let mut r = PacketReader::new(payload.clone());
        let id = r.read_var_int()?;
        let mut state = state.lock().unwrap();
        if state.configuring {
            configure_again(id, &mut r, out, &mut state, &replies)?;
            continue;
        }
        if replies.keep.contains(&id) {
            let body = payload.slice(payload.len() - r.remaining()..);
            state.kept_order.push((id, body.clone()));
            state.last.insert(id, body);
        }
        state.bytes = reader.bytes_read();
        state.payload_bytes += size;
        state.packets += 1;
        *state.received.entry(id).or_default() += 1;
        state
            .tally
            .record(id, reader.last_frame_len(), size as usize);
        handle(id, &mut r, out, &mut state, &replies)?;
    }
    Ok(())
}

/// Answers a configuration phase the server started from play.
fn configure_again(
    id: i32,
    r: &mut PacketReader,
    out: &mpsc::UnboundedSender<play::Serverbound>,
    state: &mut BotState,
    replies: &Replies,
) -> Result<()> {
    state.configuration.push(id);
    let reply = if id == KnownPacks::ID {
        configuration::Serverbound::KnownPacks(vec![KnownPack {
            namespace: "minecraft".into(),
            id: "core".into(),
            version: VERSION_NAME.into(),
        }])
    } else if id == configuration::KeepAlive::ID {
        configuration::Serverbound::KeepAlive(r.read_long()?)
    } else if id == FinishConfiguration::ID {
        configuration::Serverbound::AcknowledgeFinish
    } else if id == ConfigDisconnect::ID {
        bail!("disconnected during configuration");
    } else {
        return Ok(());
    };
    let _ = replies
        .raw
        .send(frame(reply.encode().unwrap(), replies.compression));
    if id == FinishConfiguration::ID {
        state.configuring = false;
        state.reconfigurations += 1;
        let _ = out.send(play::Serverbound::PlayerLoaded);
    }
    Ok(())
}

fn handle(
    id: i32,
    r: &mut PacketReader,
    out: &mpsc::UnboundedSender<play::Serverbound>,
    state: &mut BotState,
    replies: &Replies,
) -> Result<()> {
    use tuff_vanilla_data::packet::play::Clientbound as C;
    if id == play::KeepAlive::ID {
        let _ = out.send(play::Serverbound::KeepAlive(r.read_long()?));
    } else if id == play::StartConfiguration::ID {
        state.configuring = true;
        let _ = out.send(play::Serverbound::ConfigurationAcknowledged);
    } else if id == play::LevelChunkWithLight::ID {
        state.chunk_bytes += r.remaining() as u64;
        let chunk = ChunkPos::new(r.read_int()?, r.read_int()?);
        state.loaded.insert(chunk);
        if let Some(terrain) = &mut state.terrain {
            terrain.read_chunk(chunk, r)?;
        }
    } else if id == play::ForgetLevelChunk::ID {
        let z = r.read_int()?;
        let chunk = ChunkPos::new(r.read_int()?, z);
        state.loaded.remove(&chunk);
        if let Some(terrain) = &mut state.terrain {
            terrain.forget(chunk);
        }
    } else if id == play::ChunkBatchFinished::ID {
        let count = r.read_var_int()?;
        state.chunks += count as u64;
        state.last_chunks = Some(Instant::now());
        let _ = out.send(play::Serverbound::ChunkBatchReceived(
            replies.chunks_per_tick,
        ));
    } else if id == play::PlayerPosition::ID {
        let teleport = r.read_var_int()?;
        let given = [r.read_double()?, r.read_double()?, r.read_double()?];
        for _ in 0..3 {
            r.read_double()?;
        }
        r.read_float()?;
        r.read_float()?;
        let relative = r.read_int()?;
        let position: [f64; 3] = std::array::from_fn(|axis| {
            given[axis]
                + if relative & (1 << axis) != 0 {
                    state.position[axis]
                } else {
                    0.0
                }
        });
        state.position = position;
        state.teleports += 1;
        if state.pursuing {
            state.setbacks += 1;
            state.pending_teleport = Some(teleport);
        } else {
            confirm_teleport(state, out, teleport);
        }
    } else if id == play::SetTime::ID {
        state.server_clock.sync(r.read_long()?, Instant::now());
    } else if id == PlayPush::ID {
        let seen = read_push(r, true)?;
        for status in pack_answers(replies.pack_status) {
            let _ = out.send(play::Serverbound::ResourcePack {
                id: seen.id,
                status: Some(status),
            });
        }
        state.pack_offers.push(seen);
    } else if id == play::Disconnect::ID {
        let n = r.remaining();
        let raw = r.read_bytes(n)?;
        bail!("kicked: {}", String::from_utf8_lossy(&raw));
    } else if id == C::Login.id() {
        state.entity_id = r.read_int()?;
    } else if id == C::SetHealth.id() {
        let health = r.read_float()?;
        if health <= 0.0 && state.health > 0.0 {
            state.deaths += 1;
            let _ = out.send(play::Serverbound::ClientCommand(PERFORM_RESPAWN));
        }
        state.health = health;
    } else if id == C::Waypoint.id() {
        state.waypoints.push(read_waypoint(r)?);
    } else if id == C::AddEntity.id() {
        let entity = r.read_var_int()?;
        r.read_uuid()?;
        let kind = EntityType::from_id(r.read_var_int()? as u32);
        let position = [r.read_double()?, r.read_double()?, r.read_double()?];
        state.entities.insert(entity, Seen { kind, position });
    } else if id == C::EntityPositionSync.id() {
        let entity = r.read_var_int()?;
        if r.read_var_int()? == 0
            && let Some(seen) = state.entities.get_mut(&entity)
        {
            seen.position = [r.read_double()?, r.read_double()?, r.read_double()?];
        }
    } else if id == C::RemoveEntities.id() {
        for _ in 0..r.read_var_int()? {
            let entity = r.read_var_int()?;
            state.entities.remove(&entity);
        }
    } else if id == C::BlockUpdate.id() {
        let position = r.read_position()?;
        let block = r.read_var_int()? as u32;
        if let Some(terrain) = &mut state.terrain {
            terrain.set_block(position, block);
        }
    } else if id == C::SectionBlocksUpdate.id() {
        if let Some(terrain) = &mut state.terrain {
            terrain.read_section_update(r)?;
        }
    } else if id == C::DebugSample.id() {
        if let Some(sample) = read_tick_sample(r, Instant::now())? {
            state.tick_window.report(state.position, sample.received);
            if let Some(sink) = &replies.tick_samples {
                let _ = sink.send(sample);
            }
        }
    } else if id == C::OpenScreen.id() || id == C::MountScreenOpen.id() {
        state.window = Some(r.read_var_int()?);
        state.windows_opened += 1;
    } else if id == C::ContainerClose.id() {
        state.window = None;
    }
    Ok(())
}

fn read_waypoint(r: &mut PacketReader) -> Result<tuff_core::world::waypoints::TrackedWaypoint> {
    use tuff_core::world::waypoints::{Contents, Icon, Operation, TrackedWaypoint};
    let operation = match r.read_var_int()? {
        0 => Operation::Track,
        1 => Operation::Untrack,
        2 => Operation::Update,
        other => bail!("unknown waypoint operation {other}"),
    };
    let uuid = if r.read_bool()? {
        r.read_uuid()?
    } else {
        r.read_string()?;
        0
    };
    let style = r.read_string()?;
    let color = if r.read_bool()? {
        Some([
            r.read_unsigned_byte()?,
            r.read_unsigned_byte()?,
            r.read_unsigned_byte()?,
        ])
    } else {
        None
    };
    let contents = match r.read_var_int()? {
        0 => Contents::Empty,
        1 => Contents::Position([r.read_var_int()?, r.read_var_int()?, r.read_var_int()?]),
        2 => Contents::Chunk([r.read_var_int()?, r.read_var_int()?]),
        3 => Contents::Azimuth(r.read_float()?),
        other => bail!("unknown waypoint data {other}"),
    };
    Ok(TrackedWaypoint {
        operation,
        uuid,
        icon: Icon {
            style: Some(style),
            color,
        },
        contents,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload(values: &[i64], kind: i32) -> PacketReader {
        let mut w = PacketWriter::new();
        w.write_var_int(values.len() as i32);
        values.iter().for_each(|v| w.write_long(*v));
        w.write_var_int(kind);
        PacketReader::new(w.into_bytes())
    }

    fn teleport_payload(id: i32, position: [f64; 3], relative: i32) -> PacketReader {
        let mut w = PacketWriter::new();
        w.write_var_int(id);
        position.iter().for_each(|v| w.write_double(*v));
        (0..3).for_each(|_| w.write_double(0.0));
        w.write_float(0.0);
        w.write_float(0.0);
        w.write_int(relative);
        PacketReader::new(w.into_bytes())
    }

    fn receive_teleport(
        state: &mut BotState,
        id: i32,
        position: [f64; 3],
        relative: i32,
    ) -> Vec<play::Serverbound> {
        let (out, mut sent) = mpsc::unbounded_channel();
        let replies = Replies {
            chunks_per_tick: 1.0,
            pack_status: PackStatus::Loaded,
            tick_samples: None,
            keep: Vec::new(),
            raw: mpsc::unbounded_channel().0,
            compression: None,
        };
        let mut payload = teleport_payload(id, position, relative);
        handle(
            play::PlayerPosition::ID,
            &mut payload,
            &out,
            state,
            &replies,
        )
        .unwrap();
        std::iter::from_fn(|| sent.try_recv().ok()).collect()
    }

    fn moved_to(packets: &[play::Serverbound]) -> Option<[f64; 3]> {
        packets.iter().find_map(|packet| match packet {
            play::Serverbound::Move(movement) => movement.position,
            _ => None,
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_teleport_outside_a_goal_is_confirmed_at_once() {
        let mut state = BotState::default();
        let sent = receive_teleport(&mut state, 3, [1.0, 70.0, 2.0], 0);
        assert_eq!(
            sent[0],
            play::Serverbound::AcceptTeleportation {
                id: 3,
                position: [1.0, 70.0, 2.0],
                rotation: (0.0, 0.0),
            }
        );
        assert_eq!(moved_to(&sent), Some([1.0, 70.0, 2.0]));
        assert_eq!((state.teleports, state.setbacks), (1, 0));
        let sent = receive_teleport(&mut state, 4, [1.0, 0.0, -2.0], 0b101);
        assert_eq!(state.position, [2.0, 0.0, 0.0]);
        assert_eq!(moved_to(&sent), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_setback_is_adopted_confirmed_next_tick_and_flown_on_from() {
        let mut state = BotState {
            position: [0.0, 100.0, 0.0],
            pursuing: true,
            ..BotState::default()
        };
        let mut goal = FlyLine::new([0.0, 1.0], 100.0, f64::INFINITY, 2.0);
        let now = Instant::now();
        for _ in 0..3 {
            client_tick(&mut state, &mut goal, now);
        }
        assert_eq!(state.position, [0.0, 100.0, 6.0]);
        assert!(receive_teleport(&mut state, 9, [0.0, 100.0, 2.0], 0).is_empty());
        assert!(receive_teleport(&mut state, 10, [0.0, 100.0, 1.0], 0).is_empty());
        assert_eq!(state.position, [0.0, 100.0, 1.0]);
        assert_eq!((state.setbacks, state.pending_teleport), (2, Some(10)));

        let (packets, _) = client_tick(&mut state, &mut goal, now);
        assert_eq!(
            packets[0],
            play::Serverbound::AcceptTeleportation {
                id: 10,
                position: [0.0, 100.0, 1.0],
                rotation: (0.0, 0.0),
            }
        );
        assert_eq!(moved_to(&packets), Some([0.0, 100.0, 3.0]));
        assert_eq!(packets.last(), Some(&play::Serverbound::ClientTickEnd));
        assert_eq!((state.pending_teleport, state.moves), (None, 4));
    }

    #[tuff_core::watchdog]
    #[test]
    fn moves_shrink_while_the_server_lags_and_stop_while_it_stalls() {
        let start = Instant::now();
        let at = |s: f64| start + Duration::from_secs_f64(s);
        let mut state = BotState {
            position: [0.0, 100.0, 0.0],
            pursuing: true,
            ..BotState::default()
        };
        state.server_clock.sync(0, at(0.0));
        state.server_clock.sync(20, at(4.0));
        let mut goal = FlyLine::new([1.0, 0.0], 100.0, f64::INFINITY, 4.0);
        let (packets, _) = client_tick(&mut state, &mut goal, at(4.5));
        let [x, _, _] = moved_to(&packets).unwrap();
        assert!((x - pace::MOVE_PER_SERVER_TICK / 4.0).abs() < 1e-9, "{x}");
        let (packets, _) = client_tick(&mut state, &mut goal, at(30.0));
        assert_eq!(packets, [play::Serverbound::ClientTickEnd]);
        assert_eq!(state.position, [x, 100.0, 0.0]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn moves_wait_for_the_next_tick_report_once_the_window_is_used() {
        let now = Instant::now();
        let mut state = BotState {
            position: [0.0, 100.0, 0.0],
            pursuing: true,
            ..BotState::default()
        };
        state.tick_window.report(state.position, now);
        let mut goal = FlyLine::new([1.0, 0.0], 100.0, f64::INFINITY, 4.0);
        let flown: Vec<Option<f64>> = (0..4)
            .map(|_| moved_to(&client_tick(&mut state, &mut goal, now).0).map(|p| p[0]))
            .collect();
        assert_eq!(flown, [Some(4.0), Some(8.0), Some(9.0), None]);
        assert_eq!(state.moves, 3);
        state.tick_window.report(state.position, now);
        client_tick(&mut state, &mut goal, now);
        assert_eq!(state.position[0], 9.0);
        state.tick_window.report(state.position, now);
        client_tick(&mut state, &mut goal, now);
        assert_eq!(state.position[0], 13.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn approach_caps_one_move() {
        assert_eq!(approach([0.0; 3], [3.0, 4.0, 0.0], 8.0), [3.0, 4.0, 0.0]);
        assert_eq!(
            approach([1.0, 0.0, 0.0], [1.0, 100.0, 0.0], 8.0),
            [1.0, 8.0, 0.0]
        );
        let [x, y, _] = approach([0.0; 3], [30.0, 40.0, 0.0], 5.0);
        assert!((x - 3.0).abs() < 1e-9 && (y - 4.0).abs() < 1e-9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn tick_samples_decode_and_bound_their_length() {
        let at = Instant::now();
        let sample = read_tick_sample(&mut sample_payload(&[50, 7, 2, 41], 0), at)
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                sample.full_ns,
                sample.tick_ns,
                sample.tasks_ns,
                sample.idle_ns
            ),
            (50, 7, 2, 41)
        );
        assert!(
            read_tick_sample(&mut sample_payload(&[1], 3), at)
                .unwrap()
                .is_none()
        );
        let mut huge = PacketWriter::new();
        huge.write_var_int(MAX_SAMPLE_DIMENSIONS + 1);
        assert!(read_tick_sample(&mut PacketReader::new(huge.into_bytes()), at).is_err());
    }
}
