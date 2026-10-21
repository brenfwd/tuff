//! `/chase`: the developer tool that keeps one server's first player at the
//! position of another server's first player. `lead` listens on a TCP port
//! and broadcasts the leader's position every 100 ms as an ASCII line
//! (`t <o|n|e> x y z yRot xRot`); `follow` connects to such a server and
//! teleports its own first player to every position it hears. The socket is
//! only bound while a lead runs and is closed by `stop` or shutdown.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tuff_core::command::args::Anchor;
use tuff_core::command::{CommandError, CommandNode, CommandNodeKind, RootCommand, Value};
use tuff_core::world::Shared;
use tuff_core::world::commands::console::ConsoleRuntime;
use tuff_core::world::commands::{Actor, Source, SourceKind};
use tuff_core::world::messages::{CoordinatorMessage, PlayerSnapshot};
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_storage::admin::PermissionLevel;

use super::{Context, Executor, executor_future, text};

pub const DEFAULT_CONNECT_HOST: &str = "localhost";
pub const DEFAULT_BIND_ADDRESS: &str = "0.0.0.0";
pub const DEFAULT_PORT: u16 = 10000;
const BROADCAST_INTERVAL: Duration = Duration::from_millis(100);
const RECONNECT_SECONDS: u64 = 5;
const LISTEN_BACKLOG: u32 = 50;
/// Position lines queued per follower; a slow follower drops stale lines
/// rather than growing a queue, since each line supersedes the last.
const CLIENT_QUEUE: usize = 16;

const DIMENSIONS: [(&str, &str); 3] = [
    ("o", "minecraft:overworld"),
    ("n", "minecraft:the_nether"),
    ("e", "minecraft:the_end"),
];

const OWNERS: &str = r#"{"type":"minecraft:require","permission":{"type":"minecraft:command_level","level":"owners"}}"#;

const fn node(
    name: &'static str,
    kind: CommandNodeKind,
    children: &'static [CommandNode],
) -> CommandNode {
    CommandNode {
        name,
        kind,
        executable: true,
        redirect: None,
        permissions: None,
        children,
    }
}

const fn string_arg(name: &'static str, children: &'static [CommandNode]) -> CommandNode {
    node(
        name,
        CommandNodeKind::Argument {
            parser: "brigadier:string",
            properties: Some(r#"{"type":"phrase"}"#),
        },
        children,
    )
}

const FOLLOW_PORT: CommandNode = node(
    "port",
    CommandNodeKind::Argument {
        parser: "brigadier:integer",
        properties: Some(r#"{"min":1,"max":65535}"#),
    },
    &[],
);
const LEAD_PORT: CommandNode = node(
    "port",
    CommandNodeKind::Argument {
        parser: "brigadier:integer",
        properties: Some(r#"{"min":1024,"max":65535}"#),
    },
    &[],
);
const FOLLOW: CommandNode = node(
    "follow",
    CommandNodeKind::Literal,
    &[string_arg("host", &[FOLLOW_PORT])],
);
const LEAD: CommandNode = node(
    "lead",
    CommandNodeKind::Literal,
    &[string_arg("bind_address", &[LEAD_PORT])],
);
const STOP: CommandNode = node("stop", CommandNodeKind::Literal, &[]);
pub const CHASE: CommandNode = CommandNode {
    name: "chase",
    kind: CommandNodeKind::Literal,
    executable: false,
    redirect: None,
    permissions: Some(OWNERS),
    children: &[FOLLOW, LEAD, STOP],
};

pub fn root() -> RootCommand {
    RootCommand::custom("chase", &CHASE, &[])
}

pub struct Chase;

impl Executor for Chase {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(chase) = ChaseService::of(ctx.shared()) else {
                return Ok(0);
            };
            let literals: Vec<&str> = ctx.literals();
            let host = match ctx.arg("host").or_else(|| ctx.arg("bind_address")) {
                Some(Value::Str(host)) => Some(host.clone()),
                _ => None,
            };
            let port = match ctx.arg("port") {
                Some(Value::Int(port)) => *port as u16,
                _ => DEFAULT_PORT,
            };
            match literals.get(1).copied() {
                Some("stop") => {
                    let (client, server) = chase.stop().await;
                    if client {
                        ctx.success(text("You have now stopped chasing"), false)
                            .await;
                    }
                    if server {
                        ctx.success(text("You are no longer being chased"), false)
                            .await;
                    }
                }
                Some("lead") => {
                    let bind = host.unwrap_or_else(|| DEFAULT_BIND_ADDRESS.to_owned());
                    match chase.lead(bind, port).await {
                        Started::Yes => {
                            ctx.success(
                                text(format!(
                                    "Chase server is now running on port {port}. Clients can follow you using /chase follow <ip> <port>"
                                )),
                                false,
                            )
                            .await;
                        }
                        Started::Failed => {
                            ctx.fail(text(format!("Failed to start chase server on port {port}")))
                        }
                        Started::Running(running) => ctx.fail(text(running.message())),
                    }
                }
                Some("follow") => {
                    let host = host.unwrap_or_else(|| DEFAULT_CONNECT_HOST.to_owned());
                    match chase.follow(host.clone(), port).await {
                        Started::Running(running) => ctx.fail(text(running.message())),
                        _ => {
                            ctx.success(
                                text(format!(
                                    "You are now chasing {host}:{port}. If that server does '/chase lead' then you will automatically go to the same position. Use '/chase stop' to stop chasing."
                                )),
                                false,
                            )
                            .await;
                        }
                    }
                }
                _ => return Err(CommandError::simple("command.unknown.command")),
            }
            Ok(0)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Running {
    Server,
    Client,
}

impl Running {
    fn message(self) -> &'static str {
        match self {
            Self::Server => "Chase server is already running. Stop it using /chase stop",
            Self::Client => "You are already chasing someone. Stop it using /chase stop",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    Yes,
    Failed,
    Running(Running),
}

enum Message {
    Lead {
        bind: String,
        port: u16,
        reply: oneshot::Sender<Started>,
    },
    Follow {
        host: String,
        port: u16,
        reply: oneshot::Sender<Started>,
    },
    Stop {
        reply: oneshot::Sender<(bool, bool)>,
    },
    /// The address the running lead listens on, for tests.
    LeadAddress {
        reply: oneshot::Sender<Option<SocketAddr>>,
    },
}

/// A running task that ends when its stop sender is dropped.
struct Task {
    _stop: oneshot::Sender<()>,
}

struct Lead {
    _task: Task,
    address: SocketAddr,
}

#[derive(Default)]
struct State {
    lead: Option<Lead>,
    follow: Option<Task>,
}

impl State {
    fn running(&self) -> Option<Running> {
        if self.lead.is_some() {
            Some(Running::Server)
        } else if self.follow.is_some() {
            Some(Running::Client)
        } else {
            None
        }
    }
}

/// Handle to the chase actor of one world.
#[derive(Clone)]
pub struct ChaseHandle {
    tx: mpsc::Sender<Message>,
}

impl ChaseHandle {
    async fn ask<T>(&self, message: impl FnOnce(oneshot::Sender<T>) -> Message) -> Option<T> {
        let (reply, answer) = oneshot::channel();
        self.tx.send(message(reply)).await.ok()?;
        answer.await.ok()
    }

    pub async fn lead(&self, bind: String, port: u16) -> Started {
        self.ask(|reply| Message::Lead { bind, port, reply })
            .await
            .unwrap_or(Started::Failed)
    }

    pub async fn follow(&self, host: String, port: u16) -> Started {
        self.ask(|reply| Message::Follow { host, port, reply })
            .await
            .unwrap_or(Started::Failed)
    }

    /// Stops both sides; reports whether a follow and a lead were running.
    pub async fn stop(&self) -> (bool, bool) {
        self.ask(|reply| Message::Stop { reply })
            .await
            .unwrap_or_default()
    }

    pub async fn lead_address(&self) -> Option<SocketAddr> {
        self.ask(|reply| Message::LeadAddress { reply })
            .await
            .flatten()
    }
}

fn start(shared: Arc<Shared>) -> ChaseHandle {
    // Commands wait for each reply, so a small queue never fills.
    let (tx, mut rx) = mpsc::channel(8);
    let weak = Arc::downgrade(&shared);
    drop(shared);
    tokio::spawn(async move {
        let mut state = State::default();
        while let Some(message) = rx.recv().await {
            match message {
                Message::Lead { bind, port, reply } => {
                    let outcome = if let Some(running) = state.running() {
                        Started::Running(running)
                    } else {
                        match bind_listener(&bind, port).await {
                            Ok(listener) => {
                                let address = listener
                                    .local_addr()
                                    .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], port)));
                                let (stop, stopped) = oneshot::channel();
                                tokio::spawn(run_lead(weak.clone(), listener, port, stopped));
                                state.lead = Some(Lead {
                                    _task: Task { _stop: stop },
                                    address,
                                });
                                Started::Yes
                            }
                            Err(err) => {
                                log::error!("Failed to start chase server: {err}");
                                Started::Failed
                            }
                        }
                    };
                    let _ = reply.send(outcome);
                }
                Message::Follow { host, port, reply } => {
                    let outcome = if let Some(running) = state.running() {
                        Started::Running(running)
                    } else {
                        let (stop, stopped) = oneshot::channel();
                        tokio::spawn(run_follow(weak.clone(), host, port, stopped));
                        state.follow = Some(Task { _stop: stop });
                        Started::Yes
                    };
                    let _ = reply.send(outcome);
                }
                Message::Stop { reply } => {
                    let client = state.follow.take().is_some();
                    let server = state.lead.take().is_some();
                    let _ = reply.send((client, server));
                }
                Message::LeadAddress { reply } => {
                    let _ = reply.send(state.lead.as_ref().map(|l| l.address));
                }
            }
        }
    });
    ChaseHandle { tx }
}

async fn bind_listener(bind: &str, port: u16) -> io::Result<TcpListener> {
    let address = tokio::net::lookup_host((bind, port))
        .await?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("unknown host {bind}")))?;
    let socket = if address.is_ipv6() {
        TcpSocket::new_v6()?
    } else {
        TcpSocket::new_v4()?
    };
    socket.set_reuseaddr(true)?;
    socket.bind(address)?;
    socket.listen(LISTEN_BACKLOG)
}

#[derive(Debug, Clone, PartialEq)]
struct LeaderPosition {
    dimension: &'static str,
    position: [f64; 3],
    rotation: [f32; 2],
}

impl LeaderPosition {
    fn line(&self) -> String {
        let [x, y, z] = self.position;
        let [y_rot, x_rot] = self.rotation;
        format!(
            "t {} {} {} {} {} {}\n",
            self.dimension,
            fixed(x, 2),
            fixed(y, 2),
            fixed(z, 2),
            fixed(y_rot as f64, 2),
            fixed(x_rot as f64, 2)
        )
    }
}

async fn online(shared: &Shared) -> Vec<PlayerSnapshot> {
    let (reply, players) = oneshot::channel();
    if shared
        .coordinator
        .send(CoordinatorMessage::Players { reply })
        .await
        .is_err()
    {
        return Vec::new();
    }
    let mut players = players.await.unwrap_or_default();
    players.sort_by_key(|p| p.id);
    players
}

async fn leader_position(shared: &Shared) -> Option<LeaderPosition> {
    let first = online(shared).await.into_iter().next()?;
    let key = &shared.dimensions.get(first.state.dimension).key;
    let short = DIMENSIONS.iter().find(|(_, k)| k == key)?.0;
    Some(LeaderPosition {
        dimension: short,
        position: first.state.position,
        rotation: first.state.rotation,
    })
}

async fn run_lead(
    shared: std::sync::Weak<Shared>,
    listener: TcpListener,
    port: u16,
    mut stopped: oneshot::Receiver<()>,
) {
    let mut clients: Vec<mpsc::Sender<Arc<[u8]>>> = Vec::new();
    let mut last: Option<LeaderPosition> = None;
    let mut interval = tokio::time::interval(BROADCAST_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    log::info!("Remote control server is listening for connections on port {port}");
    loop {
        tokio::select! {
            _ = &mut stopped => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    log::info!(
                        "Remote control server received client connection on port {}",
                        peer.port()
                    );
                    clients.push(spawn_writer(stream));
                    log::info!("Remote control server is listening for connections on port {port}");
                }
                Err(err) => {
                    log::error!("Remote control server closed because of an IO exception: {err}");
                    break;
                }
            },
            _ = interval.tick() => {
                clients.retain(|c| !c.is_closed());
                if clients.is_empty() {
                    continue;
                }
                let Some(shared) = shared.upgrade() else { break };
                let Some(position) = leader_position(&shared).await else { continue };
                if last.as_ref() == Some(&position) {
                    continue;
                }
                let line: Arc<[u8]> = position.line().into_bytes().into();
                last = Some(position);
                for client in &clients {
                    let _ = client.try_send(line.clone());
                }
            }
        }
    }
    log::info!("Remote control server is now stopped");
}

fn spawn_writer(mut stream: TcpStream) -> mpsc::Sender<Arc<[u8]>> {
    let (tx, mut rx) = mpsc::channel::<Arc<[u8]>>(CLIENT_QUEUE);
    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if let Err(err) = stream.write_all(&line).await {
                log::info!(
                    "Remote control client socket got an IO exception and will be closed: {err}"
                );
                break;
            }
        }
    });
    tx
}

async fn run_follow(
    shared: std::sync::Weak<Shared>,
    host: String,
    port: u16,
    mut stopped: oneshot::Receiver<()>,
) {
    let address = format!("{host}:{port}");
    loop {
        log::info!("Connecting to remote control server {address}");
        let connected = tokio::select! {
            _ = &mut stopped => return,
            connected = TcpStream::connect((host.as_str(), port)) => connected,
        };
        match connected {
            Ok(stream) => {
                log::info!(
                    "Connected to remote control server! Will continuously execute the command broadcasted by that server."
                );
                let mut lines = BufReader::new(stream).lines();
                loop {
                    let line = tokio::select! {
                        _ = &mut stopped => return,
                        line = lines.next_line() => line,
                    };
                    match line {
                        Ok(Some(line)) => {
                            let Some(shared) = shared.upgrade() else {
                                return;
                            };
                            handle_line(&shared, &line).await;
                        }
                        Ok(None) | Err(_) => {
                            log::warn!(
                                "Lost connection to remote control server {address}. Will retry in {RECONNECT_SECONDS}s."
                            );
                            break;
                        }
                    }
                }
            }
            Err(_) => log::warn!(
                "Failed to connect to remote control server {address}. Will retry in {RECONNECT_SECONDS}s."
            ),
        }
        tokio::select! {
            _ = &mut stopped => return,
            _ = tokio::time::sleep(Duration::from_secs(RECONNECT_SECONDS)) => {}
        }
    }
}

#[derive(Debug, PartialEq)]
enum Parsed {
    Teleport(String),
    Ignored,
    Unknown(String),
    Malformed,
}

/// What one broadcast line asks for: the command to run, if any.
fn parse_line(line: &str) -> Parsed {
    let mut words = line.split_whitespace();
    let Some(head) = words.next() else {
        return Parsed::Malformed;
    };
    if head != "t" {
        return Parsed::Unknown(head.to_owned());
    }
    let Some(name) = words.next() else {
        return Parsed::Malformed;
    };
    let Some((_, key)) = DIMENSIONS.iter().find(|(short, _)| *short == name) else {
        return Parsed::Ignored;
    };
    let mut values = [0f32; 5];
    for value in &mut values {
        match words.next().map(str::parse::<f32>) {
            Some(Ok(v)) => *value = v,
            _ => return Parsed::Malformed,
        }
    }
    let [x, y, z, y_rot, x_rot] = values.map(|v| fixed(v as f64, 3));
    Parsed::Teleport(format!(
        "execute in {key} run tp @s {x} {y} {z} {y_rot} {x_rot}"
    ))
}

async fn handle_line(shared: &Arc<Shared>, line: &str) {
    match parse_line(line) {
        Parsed::Teleport(command) => run_as_first_player(shared, &command).await,
        Parsed::Ignored => {}
        Parsed::Unknown(head) => log::warn!("Unknown message type '{head}'"),
        Parsed::Malformed => log::warn!("Could not parse message '{line}', ignoring"),
    }
}

/// Runs `command` as the first online player with owner permission, from
/// the world spawn, the way the follower applies a broadcast position.
async fn run_as_first_player(shared: &Arc<Shared>, command: &str) {
    let Some(player) = online(shared).await.into_iter().next() else {
        return;
    };
    let spawn = shared.spawn();
    let source = Source {
        kind: SourceKind::Player {
            id: player.id,
            uuid: player.uuid,
            handle: player.handle.clone(),
        },
        name: player.name.clone(),
        permission: PermissionLevel::Owners,
        dimension: shared.dimensions.default_id(),
        position: [spawn.pos.x as f64, spawn.pos.y as f64, spawn.pos.z as f64],
        rotation: [0.0, 0.0],
        anchor: Anchor::Feet,
        entity: Some(player.id),
        silent: false,
    };
    let (feedback, _lines) = mpsc::unbounded_channel();
    let mut runtime = ConsoleRuntime::new(shared.clone(), feedback);
    tuff_core::world::commands::run(Actor::Console(&mut runtime), source, command).await;
    runtime.finish().await;
}

/// `value` with `places` decimals, rounding the shortest decimal form of
/// the number half up, as the game's fixed-point formatting does.
fn fixed(value: f64, places: usize) -> String {
    if value.is_nan() {
        return "NaN".into();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let sci = format!("{:e}", value.abs());
    let (mantissa, exponent) = sci.split_once('e').expect("scientific form");
    let exponent: i64 = exponent.parse().expect("exponent");
    let digits: Vec<u8> = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    let digit_at = |power: i64| -> u8 {
        let index = exponent - power;
        if index < 0 {
            0
        } else {
            digits.get(index as usize).copied().unwrap_or(0)
        }
    };
    let top = exponent.max(0);
    let places_i = places as i64;
    let mut all: Vec<u8> = (-places_i..=top).rev().map(digit_at).collect();
    if digit_at(-places_i - 1) >= 5 {
        let mut i = all.len();
        loop {
            if i == 0 {
                all.insert(0, 1);
                break;
            }
            i -= 1;
            if all[i] == 9 {
                all[i] = 0;
            } else {
                all[i] += 1;
                break;
            }
        }
    }
    let split = all.len() - places;
    let mut out = String::new();
    if value.is_sign_negative() {
        out.push('-');
    }
    let int_part: String = all[..split].iter().map(|d| (b'0' + d) as char).collect();
    let int_part = int_part.trim_start_matches('0');
    out.push_str(if int_part.is_empty() { "0" } else { int_part });
    if places > 0 {
        out.push('.');
        out.extend(all[split..].iter().map(|d| (b'0' + d) as char));
    }
    out
}

/// The feature id: `tuff.features.chase-command.enabled = true` registers
/// `/chase`; it is off by default, as in the game's release builds.
pub const ID: &str = "chase-command";

/// Registers `/chase` and its service.
pub struct ChaseFeature;

impl tuff_core::feature::Feature for ChaseFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            cx.registries().commands.register(root(), Chase);
            cx.registries().services.push(Arc::new(ChaseFactory));
        }
        Ok(())
    }
}

/// The world service owning the chase actor.
pub struct ChaseService {
    handle: OnceLock<ChaseHandle>,
}

impl ChaseService {
    pub fn of(shared: &Shared) -> Option<ChaseHandle> {
        shared.services.get::<Self>()?.handle.get().cloned()
    }
}

pub struct ChaseFactory;

impl ServiceFactory for ChaseFactory {
    fn create(
        &self,
        _dimensions: &tuff_core::world::dimension::Dimensions,
    ) -> Arc<dyn WorldService> {
        Arc::new(ChaseService {
            handle: OnceLock::new(),
        })
    }
}

impl WorldService for ChaseService {
    fn start(&self, shared: &Arc<Shared>) {
        self.handle.get_or_init(|| start(shared.clone()));
    }

    fn flush(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if let Some(handle) = self.handle.get() {
                handle.stop().await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn fixed_rounds_the_shortest_form_half_up() {
        assert_eq!(fixed(0.125, 2), "0.13");
        assert_eq!(fixed(1.005, 2), "1.01");
        assert_eq!(fixed(-0.125, 2), "-0.13");
        assert_eq!(fixed(64.0, 2), "64.00");
        assert_eq!(fixed(-0.001, 2), "-0.00");
        assert_eq!(fixed(9.999, 2), "10.00");
        assert_eq!(fixed(0.0004, 3), "0.000");
        assert_eq!(fixed(0.0005, 3), "0.001");
        assert_eq!(fixed(123456.789, 2), "123456.79");
        assert_eq!(fixed(1e-10, 2), "0.00");
        assert_eq!(fixed(1.5e20, 2), "150000000000000000000.00");
        assert_eq!(fixed(-90.0f32 as f64, 3), "-90.000");
    }

    #[tuff_core::watchdog]
    #[test]
    fn leader_lines_use_the_broadcast_format() {
        let position = LeaderPosition {
            dimension: "n",
            position: [1.0, 64.5, -3.333],
            rotation: [90.0, -12.5],
        };
        assert_eq!(position.line(), "t n 1.00 64.50 -3.33 90.00 -12.50\n");
    }

    #[tuff_core::watchdog]
    #[test]
    fn follower_lines_become_teleports() {
        assert_eq!(
            parse_line("t o 1.00 64.50 -3.33 90.00 -12.50"),
            Parsed::Teleport(
                "execute in minecraft:overworld run tp @s 1.000 64.500 -3.330 90.000 -12.500"
                    .into()
            )
        );
        assert_eq!(parse_line("t x 1 2 3 4 5"), Parsed::Ignored);
        assert_eq!(parse_line("q o 1 2 3 4 5"), Parsed::Unknown("q".into()));
        assert_eq!(parse_line("t e 1 2 3"), Parsed::Malformed);
        assert_eq!(parse_line("t e 1 2 3 4 five"), Parsed::Malformed);
        assert_eq!(parse_line(""), Parsed::Malformed);
    }

    #[tuff_core::watchdog]
    #[test]
    fn chase_requires_owner_permission() {
        use tuff_core::command::allowed;
        assert!(!allowed(&CHASE, &PermissionLevel::Admins));
        assert!(allowed(&CHASE, &PermissionLevel::Owners));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn lead_listens_on_ipv4_and_ipv6_and_closes_on_drop() {
        for bind in ["127.0.0.1", "::1"] {
            let Ok(listener) = bind_listener(bind, 0).await else {
                assert_eq!(bind, "::1", "ipv4 loopback must bind");
                continue;
            };
            let address = listener.local_addr().unwrap();
            let (stop, stopped) = oneshot::channel();
            let task = tokio::spawn(run_lead(std::sync::Weak::new(), listener, 0, stopped));
            TcpStream::connect(address).await.unwrap();
            drop(stop);
            task.await.unwrap();
            assert!(TcpStream::connect(address).await.is_err());
        }
    }
}
