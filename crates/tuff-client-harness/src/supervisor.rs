use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::timeout;

use crate::control::{Call, CallResult, ControlListener, REQUEST_TIMEOUT};
use crate::launch::LaunchCommand;
use crate::lock::{ClientLock, pid_alive};
use crate::log::{ClientEvent, LogTail, parse_line};
use crate::protocol::{ClientState, Command as HookCommand, EntityInfo};

pub const QUIT_GRACE: Duration = Duration::from_secs(10);
pub const JOIN_TIMEOUT: Duration = Duration::from_secs(120);

enum Request {
    Shutdown(oneshot::Sender<Option<ExitStatus>>),
    HasExited(oneshot::Sender<bool>),
}

/// Handle to the supervising actor. Dropping every handle shuts the client
/// down (stdin closes, then the child is killed).
#[derive(Clone)]
pub struct ClientHandle {
    requests: mpsc::Sender<Request>,
    events: broadcast::Sender<ClientEvent>,
    tail: Arc<Mutex<LogTail>>,
    control: Option<mpsc::Sender<Call>>,
    pub pid: Option<u32>,
}

pub struct SuperviseConfig {
    pub command: LaunchCommand,
    pub log_path: PathBuf,
    pub grace: Duration,
    pub lock: ClientLock,
    pub control: Option<ControlListener>,
}

impl ClientHandle {
    /// Spawns the client with its stdin piped (the wrapper halts the JVM when
    /// it closes) and moves the lock into the actor, which releases it after
    /// the child is reaped.
    pub fn spawn(mut config: SuperviseConfig) -> Result<Self> {
        let mut child = Command::new(&config.command.program)
            .args(&config.command.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {}", config.command.program.display()))?;
        let control = config.control.take().map(ControlListener::serve);
        let pid = child.id();
        config.lock.set_client_pid(pid)?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;

        let (lines_tx, lines_rx) = mpsc::channel::<String>(1024);
        tokio::spawn(pump(BufReader::new(stdout), lines_tx.clone()));
        tokio::spawn(pump(BufReader::new(stderr), lines_tx));

        let (requests, requests_rx) = mpsc::channel(8);
        let (events, _) = broadcast::channel(1024);
        let tail = Arc::new(Mutex::new(LogTail::default()));
        let actor = Actor {
            child,
            stdin,
            lines: lines_rx,
            requests: requests_rx,
            events: events.clone(),
            tail: tail.clone(),
            log: std::fs::File::create(&config.log_path).ok(),
            grace: config.grace,
            lock: config.lock,
        };
        tokio::spawn(actor.run());
        Ok(Self {
            requests,
            events,
            tail,
            control,
            pid,
        })
    }

    /// Sends one command to the in-client hook and waits for its reply (30 s).
    pub async fn request(&self, command: HookCommand) -> Result<String> {
        let control = self
            .control
            .as_ref()
            .ok_or_else(|| anyhow!("client was launched without the control hook"))?;
        let verb = command.verb();
        let (tx, rx) = oneshot::channel();
        control
            .send(Call { command, reply: tx })
            .await
            .map_err(|_| anyhow!("control actor stopped"))?;
        let result: CallResult = match timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("control actor dropped the request".into()),
            Err(_) => Err(format!("no reply within {REQUEST_TIMEOUT:?}")),
        };
        result.map_err(|e| {
            anyhow!(
                "{verb}: {e}\n--- last client log lines ---\n{}",
                self.log_tail()
            )
        })
    }

    /// Asks the client to exit through its own shutdown path (options and
    /// logs flushed), waits for it, then releases the lock.
    pub async fn quit(&self) -> Option<ExitStatus> {
        let _ = self.request(HookCommand::Quit).await;
        let deadline = tokio::time::Instant::now() + QUIT_GRACE * 2;
        while tokio::time::Instant::now() < deadline && !self.has_exited().await {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.shutdown().await
    }

    pub async fn state(&self) -> Result<ClientState> {
        ClientState::parse(&self.request(HookCommand::State).await?)
    }

    pub async fn block(&self, x: i32, y: i32, z: i32) -> Result<String> {
        crate::protocol::parse_block(&self.request(HookCommand::Block { x, y, z }).await?)
    }

    pub async fn light(&self, x: i32, y: i32, z: i32) -> Result<(u8, u8)> {
        crate::protocol::parse_light(&self.request(HookCommand::Light { x, y, z }).await?)
    }

    pub async fn entities(&self, radius: f64) -> Result<Vec<EntityInfo>> {
        crate::protocol::parse_entities(&self.request(HookCommand::Entities { radius }).await?)
    }

    /// Reads a value by reflection path from the `mc` root, e.g. `mc.player.getHealth()`.
    pub async fn eval(&self, path: &str) -> Result<String> {
        self.request(HookCommand::Eval(path.to_owned())).await
    }

    /// Writes a field by reflection path, e.g. `mc.options.renderDistance`.
    pub async fn set(&self, path: &str, value: &str) -> Result<()> {
        self.request(HookCommand::Set {
            path: path.to_owned(),
            value: value.to_owned(),
        })
        .await
        .map(drop)
    }

    /// Instruments `method` (all overloads) of `class` to count calls.
    pub async fn hook(&self, class: &str, method: &str) -> Result<()> {
        self.request(HookCommand::Hook {
            class: class.to_owned(),
            method: method.to_owned(),
        })
        .await
        .map(drop)
    }

    /// Hooks `method` and alters it: `skip`, `return=<value>` or `arg<N>=<value>`.
    pub async fn hook_alter(&self, class: &str, method: &str, spec: &str) -> Result<()> {
        self.request(HookCommand::HookAlter {
            class: class.to_owned(),
            method: method.to_owned(),
            spec: spec.to_owned(),
        })
        .await
        .map(drop)
    }

    /// `(entries, returns, exceptional exits)` seen by a hooked method since the hook was set.
    pub async fn hook_hits(&self, class: &str, method: &str) -> Result<(u64, u64, u64)> {
        crate::protocol::parse_hook_hits(
            &self
                .request(HookCommand::HookHits {
                    class: class.to_owned(),
                    method: method.to_owned(),
                })
                .await?,
        )
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ClientEvent> {
        self.events.subscribe()
    }

    pub fn log_tail(&self) -> String {
        self.tail.lock().map(|t| t.render()).unwrap_or_default()
    }

    pub async fn has_exited(&self) -> bool {
        let (tx, rx) = oneshot::channel();
        if self.requests.send(Request::HasExited(tx)).await.is_err() {
            return true;
        }
        rx.await.unwrap_or(true)
    }

    /// Waits for an event matching `pred`; on timeout or client exit the
    /// error carries the last 50 client log lines.
    pub async fn wait_event(
        &self,
        rx: &mut broadcast::Receiver<ClientEvent>,
        limit: Duration,
        pred: impl Fn(&ClientEvent) -> bool,
    ) -> Result<ClientEvent> {
        let wait = async {
            loop {
                match rx.recv().await {
                    Ok(event) if pred(&event) => return Ok(event),
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(anyhow!("client exited before the event"));
                    }
                }
            }
        };
        match timeout(limit, wait).await {
            Ok(Ok(event)) => Ok(event),
            Ok(Err(e)) => bail!("{e}\n--- last client log lines ---\n{}", self.log_tail()),
            Err(_) => bail!(
                "timed out after {limit:?}\n--- last client log lines ---\n{}",
                self.log_tail()
            ),
        }
    }

    /// Closes stdin, waits the grace period, kills if needed, reaps, and only
    /// then releases the lock.
    pub async fn shutdown(&self) -> Option<ExitStatus> {
        let (tx, rx) = oneshot::channel();
        self.requests.send(Request::Shutdown(tx)).await.ok()?;
        rx.await.ok().flatten()
    }
}

async fn pump(reader: impl AsyncBufReadExt + Unpin, tx: mpsc::Sender<String>) {
    let mut lines = reader.lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if tx.send(line).await.is_err() {
            break;
        }
    }
}

struct Actor {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<String>,
    requests: mpsc::Receiver<Request>,
    events: broadcast::Sender<ClientEvent>,
    tail: Arc<Mutex<LogTail>>,
    log: Option<std::fs::File>,
    grace: Duration,
    lock: ClientLock,
}

impl Actor {
    async fn run(mut self) {
        let mut exited: Option<ExitStatus> = None;
        let mut reply: Option<oneshot::Sender<Option<ExitStatus>>> = None;
        loop {
            tokio::select! {
                Some(line) = self.lines.recv() => self.on_line(line),
                request = self.requests.recv() => match request {
                    Some(Request::HasExited(tx)) => {
                        let _ = tx.send(exited.is_some());
                    }
                    Some(Request::Shutdown(tx)) => {
                        reply = Some(tx);
                        break;
                    }
                    None => break,
                },
                status = self.child.wait(), if exited.is_none() => {
                    exited = status.ok();
                    if exited.is_none() {
                        break;
                    }
                }
            }
        }
        let status = match exited {
            Some(status) => Some(status),
            None => self.stop().await,
        };
        while let Ok(line) = self.lines.try_recv() {
            self.on_line(line);
        }
        let _ = self.lock.set_client_pid(None);
        drop(self.lock);
        if let Some(tx) = reply {
            let _ = tx.send(status);
        }
    }

    async fn stop(&mut self) -> Option<ExitStatus> {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.shutdown().await;
        }
        match timeout(self.grace, self.child.wait()).await {
            Ok(status) => status.ok(),
            Err(_) => {
                let _ = self.child.kill().await;
                self.child.wait().await.ok()
            }
        }
    }

    fn on_line(&mut self, line: String) {
        use std::io::Write;
        if let Some(log) = &mut self.log {
            let _ = writeln!(log, "{line}");
        }
        if let Some(event) = parse_line(&line) {
            let _ = self.events.send(event);
        }
        if let Ok(mut tail) = self.tail.lock() {
            tail.push(line);
        }
    }
}

/// Takes one of the machine-wide client slots (waiting up to 15 minutes,
/// printing the holder), waits out a still-running previous client, writes the game dir
/// and spawns the supervised client.
pub async fn launch(
    spec: &crate::launch::LaunchSpec<'_>,
    test: &str,
    lock_path: PathBuf,
) -> Result<ClientHandle> {
    let owner = crate::lock::LockOwner::current(test);
    let lock = tokio::task::spawn_blocking(move || {
        let lock = ClientLock::acquire_slot(
            &lock_path,
            crate::lock::CLIENT_SLOTS,
            &owner,
            crate::lock::LOCK_WAIT,
            crate::lock::LOCK_POLL,
            |holders| {
                let held: Vec<_> = holders.iter().flatten().map(|h| h.describe()).collect();
                log::info!(
                    "waiting for a vanilla client slot held by {}",
                    held.join("; ")
                );
            },
        )?;
        lock.wait_previous_client(crate::lock::PREVIOUS_CLIENT_WAIT, pid_alive)?;
        Ok::<_, anyhow::Error>(lock)
    })
    .await??;
    crate::launch::write_game_dir(
        spec.run,
        spec.address,
        tuff_vanilla_data::version::DATA_VERSION,
    )?;
    let control = ControlListener::bind().await?;
    let mut hook_args = control.jvm_args();
    let java = spec.java.to_path_buf();
    let agent_dir = spec.run.root.join("agent");
    let agent = tokio::task::spawn_blocking(move || crate::launch::build_agent(&java, &agent_dir))
        .await??;
    hook_args.push(format!("-javaagent:{}", agent.display()));
    let java = spec.java.to_path_buf();
    let classpath = spec.files.classpath.clone();
    let cache = crate::launch::default_run_base().join("hook-cache");
    let hook_dir = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&cache)?;
        crate::launch::build_hook(&java, &classpath, &cache)
    })
    .await??;
    let command = crate::launch::build_command(&crate::launch::LaunchSpec {
        hook_args: &hook_args,
        hook_dir: Some(&hook_dir),
        ..*spec
    })?;
    ClientHandle::spawn(SuperviseConfig {
        command,
        log_path: spec.run.client_log(),
        grace: QUIT_GRACE,
        lock,
        control: Some(control),
    })
}
