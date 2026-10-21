//! Per-player packet tracing (`/tuff tracing <player> start|stop`).
//!
//! One actor owns the open trace files and the armed-name table. Connections
//! send it records over a bounded channel with `try_send`: a connection never
//! waits on the tracer, and a full queue drops the record and counts it (the
//! actor writes a dropped-records event). Start and stop are control messages
//! on a separate unbounded channel so they are never dropped.
//!
//! The hot path costs one relaxed atomic load while nothing is armed. The
//! armed names are mirrored into a lock-protected set (written only by the
//! actor) so a connection can decide whether to record a packet at all.
//!
//! Files use the binary [`format`]: every packet of both directions as its
//! decompressed bytes, with protocol state and a monotonic timestamp. A
//! connection hands over the payload `Bytes` it already holds; clientbound
//! frames are unframed and inflated in the actor, never on the connection.
//! Login secrets and chat session keys are stored redacted, and chat packets
//! are flagged. A file past the size limit rotates to a new part. [`dump`]
//! renders a file as text lines. A trace starts at the first packet after
//! the player name is known (Login Start); packets before it (the handshake)
//! are held per connection and written retroactively if a trace was armed.

pub mod dump;
pub mod format;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{BufWriter, Read as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use format::{Header, Tag, Writer};

const RECORD_QUEUE: usize = 65_536;
const FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const MAX_DATA_BYTES: usize = 64;
const MAX_EARLY: usize = 8;
/// Compressed bytes after which a trace continues in a new part file.
pub const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ConnState {
    Handshake,
    Status,
    Login,
    Configuration,
    Play,
}

impl ConnState {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Handshake => "handshake",
            Self::Status => "status",
            Self::Login => "login",
            Self::Configuration => "configuration",
            Self::Play => "play",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Handshake,
            1 => Self::Status,
            2 => Self::Login,
            3 => Self::Configuration,
            _ => Self::Play,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Serverbound,
    Clientbound,
}

impl Direction {
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Serverbound => "sb",
            Self::Clientbound => "cb",
        }
    }
}

#[derive(Clone)]
enum Data {
    Payload(Bytes),
    Frame { frame: Bytes, compressed: bool },
}

struct Record {
    key: Arc<str>,
    conn: u64,
    at: Instant,
    state: ConnState,
    direction: Direction,
    data: Data,
}

type Reply = oneshot::Sender<Result<PathBuf, String>>;

enum Control {
    Start {
        player: String,
        reply: Reply,
    },
    Stop {
        player: String,
        reply: Reply,
    },
    #[cfg(test)]
    Panic,
}

struct Inner {
    armed: AtomicUsize,
    names: RwLock<HashSet<String>>,
    dropped: AtomicU64,
    connections: AtomicU64,
    records: Option<mpsc::Sender<Record>>,
    control: Option<mpsc::UnboundedSender<Control>>,
}

#[derive(Clone)]
pub struct Tracer(Arc<Inner>);

/// The name that arms a trace of every connection at once.
pub const GLOBAL: &str = "*";

pub fn key_of(player: &str) -> String {
    player.to_lowercase()
}

pub fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

impl Tracer {
    fn with(
        records: Option<mpsc::Sender<Record>>,
        control: Option<mpsc::UnboundedSender<Control>>,
    ) -> Self {
        Self(Arc::new(Inner {
            armed: AtomicUsize::new(0),
            names: RwLock::default(),
            dropped: AtomicU64::new(0),
            connections: AtomicU64::new(0),
            records,
            control,
        }))
    }

    /// A tracer with no actor: tracing can never be armed.
    pub fn inert() -> Self {
        Self::with(None, None)
    }

    /// Spawns the actor; trace files are created in `directory`.
    pub fn spawn(directory: PathBuf) -> Self {
        Self::spawn_with_limit(directory, MAX_FILE_BYTES)
    }

    /// Like [`Tracer::spawn`], handing the actor to `spawn` so it can be restarted after a panic.
    pub fn spawn_using(directory: PathBuf, spawn: impl FnOnce(Runner)) -> Self {
        Self::spawn_inner(directory, MAX_FILE_BYTES, spawn)
    }

    /// Like [`Tracer::spawn`], rotating files past `max_file_bytes`.
    pub fn spawn_with_limit(directory: PathBuf, max_file_bytes: u64) -> Self {
        Self::spawn_inner(directory, max_file_bytes, |mut runner| {
            tokio::spawn(async move { runner.run().await });
        })
    }

    fn spawn_inner(directory: PathBuf, max_file_bytes: u64, spawn: impl FnOnce(Runner)) -> Self {
        let (records_tx, records_rx) = mpsc::channel(RECORD_QUEUE);
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let tracer = Self::with(Some(records_tx), Some(control_tx));
        let actor = Actor {
            directory,
            max_file_bytes,
            shared: Arc::downgrade(&tracer.0),
            files: HashMap::new(),
            reported_drops: 0,
        };
        spawn(Runner {
            actor,
            records: records_rx,
            control: control_rx,
        });
        tracer
    }

    /// True while any trace is armed.
    #[inline]
    pub fn enabled(&self) -> bool {
        self.0.armed.load(Ordering::Relaxed) > 0
    }

    pub fn is_armed(&self, key: &str) -> bool {
        self.enabled() && self.0.names.read().is_ok_and(|names| names.contains(key))
    }

    pub fn armed_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .0
            .names
            .read()
            .map(|names| names.iter().cloned().collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    pub fn dropped(&self) -> u64 {
        self.0.dropped.load(Ordering::Relaxed)
    }

    fn push(&self, record: Record) {
        let Some(records) = &self.0.records else {
            return;
        };
        if records.try_send(record).is_err() {
            self.0.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    fn panic_the_actor(&self) {
        if let Some(control) = &self.0.control {
            let _ = control.send(Control::Panic);
        }
    }

    /// Arms a trace for `player` and creates its file; returns the path.
    pub async fn start(&self, player: &str) -> Result<PathBuf, String> {
        self.control(|reply| Control::Start {
            player: player.to_owned(),
            reply,
        })
        .await
    }

    /// Flushes and closes the trace of `player`; returns the path of the
    /// last part.
    pub async fn stop(&self, player: &str) -> Result<PathBuf, String> {
        self.control(|reply| Control::Stop {
            player: player.to_owned(),
            reply,
        })
        .await
    }

    async fn control(&self, make: impl FnOnce(Reply) -> Control) -> Result<PathBuf, String> {
        const GONE: &str = "packet tracing is unavailable";
        let control = self.0.control.as_ref().ok_or(GONE)?;
        let (reply, answer) = oneshot::channel();
        control.send(make(reply)).map_err(|_| GONE.to_string())?;
        answer.await.map_err(|_| GONE.to_string())?
    }
}

fn sanitize(player: &str) -> String {
    player
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub use tuff_timings::file_stamp;

/// The file name stem (without extension) for a trace started at
/// `unix_seconds`.
pub fn file_stem(unix_seconds: i64, player: &str) -> String {
    format!(
        "tuff-trace-{}-{}",
        file_stamp(unix_seconds),
        sanitize(player)
    )
}

pub fn file_name(unix_seconds: i64, player: &str) -> String {
    format!("{}.{}", file_stem(unix_seconds, player), format::EXTENSION)
}

/// One text trace line including the trailing newline; raw line breaks in
/// the text are escaped.
pub fn format_line(ns: u128, state: ConnState, direction: Direction, text: &str) -> String {
    let mut line = String::with_capacity(text.len() + 48);
    let _ = write!(line, "{ns} {} {} ", state.name(), direction.tag());
    push_escaped(&mut line, text);
    line.push('\n');
    line
}

fn push_escaped(line: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '\n' => line.push_str("\\n"),
            '\r' => line.push_str("\\r"),
            c => line.push(c),
        }
    }
}

/// The packet payloads (id and body) in a run of clientbound frames, inflated
/// with each payload capped at [`format::MAX_RECORD`]: a larger one keeps
/// only its id. Stops at the first malformed frame.
pub fn unframe(frames: &Bytes, compressed: bool) -> Vec<Bytes> {
    let mut out = Vec::new();
    let mut rest = frames.clone();
    while !rest.is_empty() {
        let Some((len, size)) = var_int(&rest) else {
            break;
        };
        let Some(end) = size.checked_add(len).filter(|end| *end <= rest.len()) else {
            break;
        };
        let inner = rest.slice(size..end);
        rest = rest.slice(end..);
        let payload = if compressed {
            let Some((data_len, size)) = var_int(&inner) else {
                break;
            };
            let body = &inner[size..];
            if data_len == 0 {
                inner.slice(size..)
            } else {
                let cap = if data_len > format::MAX_RECORD {
                    5
                } else {
                    data_len
                };
                let mut raw = Vec::new();
                let read = flate2::read::ZlibDecoder::new(body)
                    .take(cap as u64)
                    .read_to_end(&mut raw);
                if read.is_err() || raw.is_empty() {
                    break;
                }
                if data_len > format::MAX_RECORD {
                    let id = var_int(&raw).map_or(0, |(_, size)| size);
                    raw.truncate(id);
                }
                Bytes::from(raw)
            }
        } else {
            inner
        };
        out.push(payload);
    }
    out
}

fn var_int(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut value = 0u32;
    for (i, byte) in bytes.iter().take(5).enumerate() {
        value |= u32::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value as usize, i + 1));
        }
    }
    None
}

/// The tag a stored packet gets: login secrets and chat session keys are
/// redacted (the payload is cut to the id) and chat is flagged.
fn classify(state: ConnState, direction: Direction, payload: &Bytes) -> (Tag, Bytes) {
    use tuff_vanilla_data::packet as p;
    let tag = Tag::packet(state, direction);
    let Some((id, id_len)) = var_int(payload) else {
        return (tag, payload.clone());
    };
    let id = id as i32;
    let serverbound = direction == Direction::Serverbound;
    let redact = match state {
        ConnState::Login => serverbound && id == p::login::Serverbound::Key.id(),
        ConnState::Play => serverbound && id == p::play::Serverbound::ChatSessionUpdate.id(),
        _ => false,
    };
    if redact {
        return (tag.redacted(), payload.slice(..id_len));
    }
    let chat = state == ConnState::Play
        && if serverbound {
            [
                p::play::Serverbound::Chat,
                p::play::Serverbound::ChatCommand,
                p::play::Serverbound::ChatCommandSigned,
            ]
            .iter()
            .any(|k| k.id() == id)
        } else {
            [
                p::play::Clientbound::PlayerChat,
                p::play::Clientbound::SystemChat,
                p::play::Clientbound::DisguisedChat,
            ]
            .iter()
            .any(|k| k.id() == id)
        };
    (if chat { tag.chat() } else { tag }, payload.clone())
}

struct OpenTrace {
    player: String,
    stem: String,
    part: u32,
    path: PathBuf,
    start: Instant,
    conn: Option<u64>,
    out: Writer<BufWriter<std::fs::File>>,
}

impl OpenTrace {
    fn ns(&self, at: Instant) -> i64 {
        i64::try_from(at.saturating_duration_since(self.start).as_nanos()).unwrap_or(i64::MAX)
    }
}

struct Actor {
    directory: PathBuf,
    max_file_bytes: u64,
    shared: std::sync::Weak<Inner>,
    files: HashMap<String, OpenTrace>,
    reported_drops: u64,
}

fn create_new(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

impl Actor {
    async fn run(
        &mut self,
        records: &mut mpsc::Receiver<Record>,
        control: &mut mpsc::UnboundedReceiver<Control>,
    ) {
        let mut flush = tokio::time::interval(FLUSH_INTERVAL);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                message = control.recv() => match message {
                    Some(message) => self.control(message, records),
                    None => break,
                },
                record = records.recv() => match record {
                    Some(record) => self.write(record),
                    None => break,
                },
                _ = flush.tick() => self.flush(),
            }
        }
        for (_, open) in self.files.drain() {
            let _ = open.out.finish();
        }
    }

    fn control(&mut self, message: Control, records: &mut mpsc::Receiver<Record>) {
        match message {
            #[cfg(test)]
            Control::Panic => panic!("test panic"),
            Control::Start { player, reply } => {
                let _ = reply.send(self.start(&player));
            }
            Control::Stop { player, reply } => {
                while let Ok(record) = records.try_recv() {
                    self.write(record);
                }
                let _ = reply.send(self.stop(&player));
            }
        }
    }

    fn open(&self, player: &str, stem: String, part: u32) -> std::io::Result<OpenTrace> {
        let mut attempt = 1u32;
        loop {
            let mut name = stem.clone();
            if part > 1 {
                let _ = write!(name, "-part{part}");
            }
            if attempt > 1 {
                let _ = write!(name, "-{attempt}");
            }
            let path = self.directory.join(format!("{name}.{}", format::EXTENSION));
            match create_new(&path) {
                Ok(file) => {
                    let start_unix = unix_nanos() as u64;
                    let mut header = Header::current(player, start_unix);
                    if player == GLOBAL {
                        header.flags |= format::FLAG_DEDUPE;
                    }
                    let out = Writer::new(BufWriter::new(file), &header)?;
                    return Ok(OpenTrace {
                        player: player.to_owned(),
                        stem,
                        part,
                        path,
                        start: Instant::now(),
                        conn: None,
                        out,
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => attempt += 1,
                Err(err) => return Err(err),
            }
        }
    }

    fn start(&mut self, player: &str) -> Result<PathBuf, String> {
        let key = key_of(player);
        if let Some(open) = self.files.get(&key) {
            return Err(format!(
                "Already tracing {player} to {}",
                open.path.display()
            ));
        }
        let inner = self.shared.upgrade().ok_or("packet tracing has stopped")?;
        let seconds = (unix_nanos() / 1_000_000_000) as i64;
        let open = self
            .open(player, file_stem(seconds, player), 1)
            .map_err(|err| format!("Cannot create the trace file: {err}"))?;
        let path = open.path.clone();
        self.files.insert(key.clone(), open);
        if let Ok(mut names) = inner.names.write() {
            names.insert(key);
        }
        inner.armed.fetch_add(1, Ordering::Relaxed);
        Ok(path)
    }

    fn stop(&mut self, player: &str) -> Result<PathBuf, String> {
        let key = key_of(player);
        let Some(open) = self.files.remove(&key) else {
            return Err(format!("Not tracing {player}"));
        };
        if let Some(inner) = self.shared.upgrade() {
            if let Ok(mut names) = inner.names.write() {
                names.remove(&key);
            }
            inner.armed.fetch_sub(1, Ordering::Relaxed);
        }
        let path = open.path.clone();
        open.out
            .finish()
            .map_err(|err| format!("Cannot finish the trace file: {err}"))?;
        Ok(path)
    }

    fn write(&mut self, record: Record) {
        let dropped = self
            .shared
            .upgrade()
            .map_or(0, |i| i.dropped.load(Ordering::Relaxed));
        let Some(open) = self.files.get_mut(&*record.key) else {
            return;
        };
        let ns = open.ns(record.at);
        let mut event = Vec::with_capacity(11);
        if dropped > self.reported_drops {
            event.push(format::EVENT_DROPPED);
            format::put_varint(&mut event, dropped - self.reported_drops);
            let _ = open.out.push(ns, Tag::event(), &event);
            self.reported_drops = dropped;
        }
        if open.conn != Some(record.conn) {
            event.clear();
            event.push(format::EVENT_CONNECTION);
            format::put_varint(&mut event, record.conn);
            let _ = open.out.push(ns, Tag::event(), &event);
            open.conn = Some(record.conn);
        }
        let payloads = match record.data {
            Data::Payload(payload) => vec![payload],
            Data::Frame { frame, compressed } => unframe(&frame, compressed),
        };
        for payload in payloads {
            let (tag, payload) = classify(record.state, record.direction, &payload);
            let _ = open.out.push(ns, tag, &payload);
        }
        if open.out.written() >= self.max_file_bytes {
            self.rotate(&record.key);
        }
    }

    fn rotate(&mut self, key: &str) {
        let Some(open) = self.files.remove(key) else {
            return;
        };
        let next = self.open(&open.player, open.stem.clone(), open.part + 1);
        let _ = open.out.finish();
        match next {
            Ok(next) => {
                self.files.insert(key.to_owned(), next);
            }
            Err(err) => {
                log::warn!("packet trace of {} stopped: {err}", open.player);
                if let Some(inner) = self.shared.upgrade() {
                    if let Ok(mut names) = inner.names.write() {
                        names.remove(key);
                    }
                    inner.armed.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
    }

    fn flush(&mut self) {
        for open in self.files.values_mut() {
            let _ = open.out.flush();
        }
    }
}

struct Early {
    at: Instant,
    state: ConnState,
    direction: Direction,
    data: Data,
}

/// A connection's view of the tracer, shared by its reader and writer.
pub struct ConnTrace {
    tracer: Tracer,
    conn: u64,
    key: OnceLock<Arc<str>>,
    state: AtomicU8,
    early: Mutex<Vec<Early>>,
}

impl ConnTrace {
    pub fn new(tracer: Tracer) -> Arc<Self> {
        let conn = tracer.0.connections.fetch_add(1, Ordering::Relaxed);
        Arc::new(Self {
            tracer,
            conn,
            key: OnceLock::new(),
            state: AtomicU8::new(ConnState::Handshake as u8),
            early: Mutex::new(Vec::new()),
        })
    }

    pub fn state(&self) -> ConnState {
        ConnState::from_u8(self.state.load(Ordering::Relaxed))
    }

    pub fn set_state(&self, state: ConnState) {
        self.state.store(state as u8, Ordering::Relaxed);
    }

    #[inline]
    pub fn enabled(&self) -> bool {
        self.tracer.enabled()
    }

    /// Records the player name; packets from now on are traced when armed,
    /// and any packets held from before are written first.
    pub fn set_player(&self, name: &str) {
        let key: Arc<str> = key_of(name).into();
        if self.key.set(key.clone()).is_err() {
            return;
        }
        let held = self
            .early
            .lock()
            .map(|mut early| std::mem::take(&mut *early))
            .unwrap_or_default();
        for early in held {
            self.push_armed(&key, early.at, early.state, early.direction, || early.data);
        }
    }

    fn push_armed(
        &self,
        key: &Arc<str>,
        at: Instant,
        state: ConnState,
        direction: Direction,
        data: impl FnOnce() -> Data,
    ) {
        let player = self.tracer.is_armed(key);
        let global = self.tracer.is_armed(GLOBAL);
        if !player && !global {
            return;
        }
        let data = data();
        let make = |key: Arc<str>, data: Data| Record {
            key,
            conn: self.conn,
            at,
            state,
            direction,
            data,
        };
        if player && global {
            self.tracer.push(make(key.clone(), data.clone()));
            self.tracer.push(make(GLOBAL.into(), data));
        } else if player {
            self.tracer.push(make(key.clone(), data));
        } else {
            self.tracer.push(make(GLOBAL.into(), data));
        }
    }

    fn record(&self, direction: Direction, data: impl FnOnce() -> Data) {
        if !self.tracer.enabled() {
            return;
        }
        let state = self.state();
        match self.key.get() {
            Some(key) => self.push_armed(key, Instant::now(), state, direction, data),
            None => {
                if let Ok(mut early) = self.early.lock()
                    && early.len() < MAX_EARLY
                {
                    early.push(Early {
                        at: Instant::now(),
                        state,
                        direction,
                        data: data(),
                    });
                }
            }
        }
    }

    /// Records clientbound frames as written to the socket.
    pub fn clientbound_frame(&self, frame: &Bytes, compression: Option<usize>) {
        self.record(Direction::Clientbound, || Data::Frame {
            frame: frame.clone(),
            compressed: compression.is_some(),
        });
    }

    /// Records a serverbound packet payload (id and body) as read.
    pub fn serverbound(&self, payload: &Bytes) {
        self.record(Direction::Serverbound, || Data::Payload(payload.clone()));
    }
}

fn packet_name(state: ConnState, direction: Direction, id: i32) -> Option<&'static str> {
    use tuff_vanilla_data::packet as p;
    let serverbound = direction == Direction::Serverbound;
    match (state, serverbound) {
        (ConnState::Handshake, true) => p::handshake::Serverbound::from_id(id).map(|v| v.name()),
        (ConnState::Handshake, false) => None,
        (ConnState::Status, true) => p::status::Serverbound::from_id(id).map(|v| v.name()),
        (ConnState::Status, false) => p::status::Clientbound::from_id(id).map(|v| v.name()),
        (ConnState::Login, true) => p::login::Serverbound::from_id(id).map(|v| v.name()),
        (ConnState::Login, false) => p::login::Clientbound::from_id(id).map(|v| v.name()),
        (ConnState::Configuration, true) => {
            p::configuration::Serverbound::from_id(id).map(|v| v.name())
        }
        (ConnState::Configuration, false) => {
            p::configuration::Clientbound::from_id(id).map(|v| v.name())
        }
        (ConnState::Play, true) => p::play::Serverbound::from_id(id).map(|v| v.name()),
        (ConnState::Play, false) => p::play::Clientbound::from_id(id).map(|v| v.name()),
    }
}

fn read_var_int(bytes: &[u8]) -> Option<(i32, usize)> {
    let mut value: u32 = 0;
    for (i, byte) in bytes.iter().take(5).enumerate() {
        value |= u32::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((value as i32, i + 1));
        }
    }
    None
}

/// The tracer actor with its channels, handed to a custom spawner. `run`
/// can be called again after a panic and resumes with the open traces.
pub struct Runner {
    actor: Actor,
    records: mpsc::Receiver<Record>,
    control: mpsc::UnboundedReceiver<Control>,
}

impl Runner {
    pub async fn run(&mut self) {
        self.actor.run(&mut self.records, &mut self.control).await;
    }
}

/// `Packet { name, id, len, data }` for a payload that starts with the id.
pub fn raw_text(state: ConnState, direction: Direction, payload: Bytes) -> String {
    let total = payload.len();
    let Some((id, used)) = read_var_int(&payload) else {
        return format!("Malformed {{ len: {total} }}");
    };
    let body = total - used;
    let shown = body.min(MAX_DATA_BYTES);
    let mut hex = String::with_capacity(shown * 2 + 3);
    for byte in &payload[used..used + shown] {
        let _ = write!(hex, "{byte:02x}");
    }
    if body > shown {
        hex.push_str("...");
    }
    let name = packet_name(state, direction, id).unwrap_or("unknown");
    format!("Packet {{ name: {name:?}, id: {id:#04x}, len: {body}, data: {hex:?} }}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn file_name_is_filename_safe_and_utc() {
        assert_eq!(
            file_name(1_700_000_000, "Steve"),
            "tuff-trace-20231114-221320-Steve.tuffrec"
        );
        assert_eq!(
            file_name(0, "a/b c"),
            "tuff-trace-19700101-000000-a_b_c.tuffrec"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn lines_escape_breaks_and_carry_the_header() {
        let line = format_line(42, ConnState::Play, Direction::Serverbound, "a\nb\rc");
        assert_eq!(line, "42 play sb a\\nb\\rc\n");
    }

    #[tuff_core::watchdog]
    #[test]
    fn raw_text_names_the_packet_and_caps_the_data() {
        let mut payload = vec![0x00];
        payload.extend(std::iter::repeat_n(0xab, 100));
        let text = raw_text(ConnState::Login, Direction::Clientbound, payload.into());
        assert!(
            text.starts_with("Packet { name: \"minecraft:login_disconnect\", id: 0x00, len: 100")
        );
        assert!(text.ends_with("...\" }"));
    }

    fn compressed_frame(payload: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(payload).unwrap();
        let packed = encoder.finish().unwrap();
        let mut inner = Vec::new();
        format::put_varint(&mut inner, payload.len() as u64);
        inner.extend_from_slice(&packed);
        let mut frame = Vec::new();
        format::put_varint(&mut frame, inner.len() as u64);
        frame.extend(inner);
        frame
    }

    #[tuff_core::watchdog]
    #[test]
    fn unframe_splits_runs_and_inflates() {
        let mut frames = vec![3, 0, 0x10, 0x20];
        frames.extend(compressed_frame(&[0x05; 300]));
        frames.extend([2, 0]);
        let payloads = unframe(&Bytes::from(frames), true);
        assert_eq!(payloads.len(), 2);
        assert_eq!(&payloads[0][..], &[0x10, 0x20]);
        assert_eq!(payloads[1].len(), 300);

        let plain = unframe(&Bytes::from_static(&[2, 0x07, 0x01, 1, 0x08]), false);
        assert_eq!(
            plain,
            vec![
                Bytes::from_static(&[0x07, 0x01]),
                Bytes::from_static(&[0x08])
            ]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn unframe_keeps_only_the_id_of_an_oversized_packet() {
        let mut big = vec![0x27];
        big.resize(format::MAX_RECORD + 1, 0);
        let payloads = unframe(&Bytes::from(compressed_frame(&big)), true);
        assert_eq!(payloads, vec![Bytes::from_static(&[0x27])]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn secrets_are_redacted_and_chat_is_flagged() {
        use tuff_vanilla_data::packet as p;
        let key = Bytes::from(vec![p::login::Serverbound::Key.id() as u8, 9, 9, 9]);
        let (tag, stored) = classify(ConnState::Login, Direction::Serverbound, &key);
        assert!(tag.is_redacted());
        assert_eq!(stored.len(), 1);
        let chat = Bytes::from(vec![p::play::Clientbound::SystemChat.id() as u8, 1]);
        let (tag, stored) = classify(ConnState::Play, Direction::Clientbound, &chat);
        assert!(tag.is_chat() && !tag.is_redacted());
        assert_eq!(stored, chat);
        let other = Bytes::from(vec![p::login::Serverbound::Key.id() as u8, 1]);
        let (tag, _) = classify(ConnState::Play, Direction::Clientbound, &other);
        assert!(!tag.is_chat() && !tag.is_redacted());
    }

    fn scratch_dir() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "tuff-trace-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panic_restarts_the_actor_keeping_open_traces() {
        let dir = scratch_dir();
        let tracer = Tracer::spawn_using(dir.clone(), |runner| {
            tokio::spawn(tuff_supervise::restarting(
                "the packet tracer",
                runner,
                |runner| Box::pin(runner.run()),
            ));
        });
        let first = tracer.start("Alex").await.unwrap();
        tracer.panic_the_actor();
        assert_eq!(tracer.stop("Alex").await.unwrap(), first);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_full_file_rotates_into_complete_parts() {
        let dir = scratch_dir();
        let tracer = Tracer::spawn_with_limit(dir.clone(), 4096);
        let first = tracer.start("Alex").await.unwrap();
        let conn = ConnTrace::new(tracer.clone());
        conn.serverbound(&Bytes::from_static(&[0x00, 0x01]));
        conn.set_player("Alex");
        conn.set_state(ConnState::Play);
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for i in 0..400u32 {
            let noise: Vec<u8> = (0..200)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as u8
                })
                .collect();
            conn.serverbound(&Bytes::from(noise));
            if i % 50 == 0 {
                tokio::task::yield_now().await;
            }
        }
        let last = tracer.stop("alex").await.unwrap();
        assert_ne!(first, last);
        let mut parts: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        parts.sort_by_key(|p| p.to_string_lossy().contains("-part"));
        assert!(parts.len() >= 2, "{parts:?}");
        let mut total = 0;
        for (i, part) in parts.iter().enumerate() {
            let recording =
                format::read_all(std::fs::File::open(part).unwrap(), usize::MAX).unwrap();
            assert!(!recording.truncated, "{part:?}");
            assert_eq!(recording.header.player, "Alex");
            assert!(
                recording.records[0].tag.is_event(),
                "part {i} opens with its connection"
            );
            if i == 0 {
                assert_eq!(recording.records[1].tag.state(), ConnState::Handshake);
            }
            total += recording
                .records
                .iter()
                .filter(|r| !r.tag.is_event())
                .count();
        }
        assert_eq!(total + tracer.dropped() as usize, 401);
        assert!(
            parts
                .iter()
                .any(|p| p.to_string_lossy().contains("-part2.tuffrec"))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_global_trace_holds_every_connection_including_late_joiners() {
        let dir = scratch_dir();
        let tracer = Tracer::spawn(dir.clone());
        let path = tracer.start(GLOBAL).await.unwrap();
        let early = ConnTrace::new(tracer.clone());
        early.serverbound(&Bytes::from_static(&[0x00, 0x01]));
        early.set_player("Alex");
        early.set_state(ConnState::Play);
        early.serverbound(&Bytes::from_static(&[0x05]));
        let late = ConnTrace::new(tracer.clone());
        late.serverbound(&Bytes::from_static(&[0x00, 0x02]));
        late.set_player("Steve");
        late.set_state(ConnState::Play);
        late.serverbound(&Bytes::from_static(&[0x06]));
        tracer.stop(GLOBAL).await.unwrap();
        let recording = format::read_all(std::fs::File::open(path).unwrap(), usize::MAX).unwrap();
        assert_eq!(recording.header.player, GLOBAL);
        let conns = recording
            .records
            .iter()
            .filter(|r| r.tag.is_event())
            .count();
        assert!(conns >= 2);
        let payloads: Vec<&[u8]> = recording
            .records
            .iter()
            .filter(|r| !r.tag.is_event())
            .map(|r| &r.payload[..])
            .collect();
        assert_eq!(payloads.len(), 4, "{payloads:?}");
        assert!(payloads.contains(&&[0x05][..]) && payloads.contains(&&[0x06][..]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_global_trace_stores_a_packet_sent_to_two_players_once() {
        let dir = scratch_dir();
        let tracer = Tracer::spawn(dir.clone());
        let path = tracer.start(GLOBAL).await.unwrap();
        let shared = Bytes::from(vec![0x2c; 200]);
        for name in ["Alex", "Steve"] {
            let conn = ConnTrace::new(tracer.clone());
            conn.set_player(name);
            conn.set_state(ConnState::Play);
            let mut frame = Vec::new();
            format::put_varint(&mut frame, shared.len() as u64);
            frame.extend_from_slice(&shared);
            conn.clientbound_frame(&Bytes::from(frame), None);
        }
        tracer.stop(GLOBAL).await.unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        let recording = format::read_all(std::fs::File::open(path).unwrap(), usize::MAX).unwrap();
        assert_eq!(recording.header.flags & format::FLAG_DEDUPE, 1);
        let full: Vec<_> = recording
            .records
            .iter()
            .filter(|r| !r.tag.is_event() && r.payload == shared)
            .collect();
        assert_eq!(full.len(), 2);
        assert!(size > 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
