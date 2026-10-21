//! The vanilla server jar the vanilla-data build caches, run as a bench
//! target: a child process in a throwaway instance directory under the
//! target directory, offline mode, never port 25565, driven over RCON.
//! Running it needs the Minecraft EULA accepted explicitly by whoever runs
//! the bench (`--accept-mojang-eula` or `TUFF_ACCEPT_MOJANG_EULA=1`).

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot, watch};

use crate::console::ChunkRows;

pub const EULA_URL: &str = "https://aka.ms/MinecraftEULA";
pub const EULA_ENV: &str = "TUFF_ACCEPT_MOJANG_EULA";
pub const DEFAULT_JAVA_OPTS: &str = "-Xmx4G";
const CACHE_DIR_ENV: &str = "VANILLA_CACHE_DIR";
const JAVA_ENV: &str = "VANILLA_JAVA";
const READY_LIMIT: Duration = Duration::from_secs(300);
const STOP_LIMIT: Duration = Duration::from_secs(60);
const PROFILE_LIMIT: Duration = Duration::from_secs(60);
const RCON_LOGIN: i32 = 3;
const RCON_COMMAND: i32 = 2;
const RCON_CHUNK: usize = 4096;
const RCON_TAIL_WAIT: Duration = Duration::from_millis(500);
const RCON_MAX_PACKET: usize = 1 << 20;
const RCON_MAX_COMMAND: usize = 1400;
const MAX_PROFILE_BYTES: u64 = 256 * 1024 * 1024;
/// Bytes of server output copied into `console.log`; past it lines are
/// only counted, since a flood of warnings would fill the disk twice over
/// (the server keeps its own `logs/latest.log`).
const CONSOLE_LOG_LIMIT: u64 = 64 * 1024 * 1024;
const CONSOLE_FLUSH_EVERY: Duration = Duration::from_millis(200);
const MAX_CRASH_REPORT_BYTES: u64 = 4 * 1024 * 1024;
const CRASH_FRAMES: usize = 3;
const LOG_ERRORS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VanillaOptions {
    pub accept_eula: bool,
    pub jar: Option<PathBuf>,
    pub java: Option<PathBuf>,
    pub java_opts: Vec<String>,
}

impl Default for VanillaOptions {
    fn default() -> Self {
        Self {
            accept_eula: false,
            jar: None,
            java: None,
            java_opts: DEFAULT_JAVA_OPTS
                .split_whitespace()
                .map(String::from)
                .collect(),
        }
    }
}

impl VanillaOptions {
    /// Takes `flag` (and its value, if it has one) when it is a vanilla
    /// flag, returning whether it was.
    pub fn flag(&mut self, flag: &str, value: impl FnOnce() -> Result<String>) -> Result<bool> {
        match flag {
            "--accept-mojang-eula" | "--accept_mojang_eula" => self.accept_eula = true,
            "--vanilla_jar" => self.jar = Some(value()?.into()),
            "--java" => self.java = Some(value()?.into()),
            "--java_opts" => {
                self.java_opts = value()?.split_whitespace().map(String::from).collect()
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub fn eula_accepted(&self) -> bool {
        self.accepts(std::env::var(EULA_ENV).ok().as_deref())
    }

    /// Whether the flag or `env` (the value of `TUFF_ACCEPT_MOJANG_EULA`)
    /// accepts the EULA.
    pub fn accepts(&self, env: Option<&str>) -> bool {
        self.accept_eula || env_accepts(env)
    }

    pub fn require_eula(&self) -> Result<()> {
        if !self.eula_accepted() {
            bail!(
                "running the vanilla server means agreeing to the Minecraft EULA ({EULA_URL}); \
                 read it, then pass --accept-mojang-eula or set {EULA_ENV}=1"
            );
        }
        Ok(())
    }

    pub fn to_json(&self) -> Value {
        json!({ "java_opts": self.java_opts })
    }
}

pub fn env_accepts(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim).map(str::to_ascii_lowercase).as_deref(),
        Some("1" | "true" | "yes")
    )
}

/// What the instance's `server.properties` must say.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub host: IpAddr,
    pub port: u16,
    pub rcon_port: u16,
    pub rcon_password: String,
    pub seed: i64,
    pub view_distance: i32,
    pub max_players: u32,
}

/// `server.properties` for a bench run. Simulation distance follows view
/// distance as tuff's does; spawn protection is off and flight allowed
/// because tuff has neither, so bots see the same rules.
pub fn properties(settings: &Settings) -> String {
    let host = match settings.host {
        ip if ip.is_unspecified() => String::new(),
        ip => ip.to_string(),
    };
    let lines = [
        ("online-mode", "false".to_string()),
        ("enforce-secure-profile", "false".into()),
        ("server-ip", host),
        ("server-port", settings.port.to_string()),
        ("enable-rcon", "true".into()),
        ("rcon.port", settings.rcon_port.to_string()),
        ("rcon.password", settings.rcon_password.clone()),
        ("broadcast-rcon-to-ops", "false".into()),
        ("level-seed", settings.seed.to_string()),
        ("view-distance", settings.view_distance.to_string()),
        ("simulation-distance", settings.view_distance.to_string()),
        ("max-players", settings.max_players.to_string()),
        ("spawn-protection", "0".into()),
        ("allow-flight", "true".into()),
        ("pause-when-empty-seconds", "0".into()),
        ("white-list", "false".into()),
        ("motd", "tuff vanilla comparison bench".into()),
    ];
    lines
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect()
}

/// `<target>`: `CARGO_TARGET_DIR`, else from this executable's path,
/// `<target>/<profile>/deps/<name>` or `<target>/<profile>/build/...`.
pub fn target_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return Ok(dir.into());
    }
    let exe = std::env::current_exe()?;
    exe.ancestors()
        .find(|dir| {
            dir.file_name()
                .is_some_and(|name| name == "deps" || name == "build")
        })
        .and_then(|deps| Some(deps.parent()?.parent()?.to_path_buf()))
        .context("cannot find the target directory from the bench executable")
}

fn cache_dir() -> Result<PathBuf> {
    match std::env::var_os(CACHE_DIR_ENV) {
        Some(dir) => Ok(dir.into()),
        None => Ok(target_dir()?.join("vanilla")),
    }
}

pub fn server_jar(options: &VanillaOptions) -> Result<PathBuf> {
    let jar = match &options.jar {
        Some(jar) => jar.clone(),
        None => cache_dir()?
            .join(tuff::protocol::VERSION_NAME)
            .join("server.jar"),
    };
    ensure!(
        jar.is_file(),
        "no vanilla server jar at {}; build once so vanilla-data fetches it, or pass --vanilla_jar",
        jar.display()
    );
    Ok(jar)
}

/// `--java`, then `VANILLA_JAVA`, then the Mojang runtime vanilla-data
/// installed in its cache.
pub fn java(options: &VanillaOptions) -> Result<PathBuf> {
    if let Some(java) = options
        .java
        .clone()
        .or_else(|| std::env::var_os(JAVA_ENV).map(Into::into))
    {
        return Ok(java);
    }
    let runtimes = cache_dir()?.join("java");
    let mut entries: Vec<_> = std::fs::read_dir(&runtimes)
        .with_context(|| format!("no Java runtime under {}; pass --java", runtimes.display()))?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .collect();
    entries.sort();
    for dir in entries {
        if let Ok(relative) = std::fs::read_to_string(dir.join(".java-path")) {
            let java = dir.join(relative.trim());
            if java.is_file() {
                return Ok(java);
            }
        }
    }
    bail!("no Java runtime under {}; pass --java", runtimes.display())
}

/// A free TCP port on `host`, found by binding port 0 and letting go.
pub fn free_port(host: IpAddr) -> Result<u16> {
    let listener = TcpListener::bind(SocketAddr::new(host, 0))?;
    Ok(listener.local_addr()?.port())
}

fn random_password() -> String {
    (0..2)
        .map(|_| format!("{:016x}", RandomState::new().build_hasher().finish()))
        .collect()
}

/// What the server's console output has shown so far.
#[derive(Debug, Clone, Default)]
struct Console {
    done: bool,
    rcon: bool,
    /// Performance profiles written, newest last, relative to the instance.
    profiles: Vec<String>,
    overloaded: u64,
    /// Moves the server refused, each logged as a warning.
    movement_warnings: u64,
    closed: bool,
}

impl Console {
    fn see(&mut self, line: &str) {
        if line.contains("]: Done (") {
            self.done = true;
        } else if line.contains("RCON running on") {
            self.rcon = true;
        } else if line.contains(" moved too quickly!") || line.contains(" moved wrongly!") {
            self.movement_warnings += 1;
        } else if line.contains("Can't keep up!") {
            self.overloaded += 1;
        } else if let Some(at) = line.find("Compressed to ") {
            self.profiles
                .push(line[at + "Compressed to ".len()..].trim().to_owned());
        }
    }
}

/// Copies one output stream of the server into the instance's log and
/// publishes what it shows.
async fn follow(
    stream: impl AsyncRead + Unpin,
    log: PathBuf,
    console: watch::Sender<Console>,
) -> Result<()> {
    let mut file = BufWriter::new(
        tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .await?,
    );
    let mut written = 0u64;
    let mut flushed = std::time::Instant::now();
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        if written < CONSOLE_LOG_LIMIT {
            written += line.len() as u64 + 1;
            file.write_all(line.as_bytes()).await?;
            file.write_all(b"\n").await?;
            if written >= CONSOLE_LOG_LIMIT {
                file.write_all(b"[bench] console.log full; see logs/latest.log\n")
                    .await?;
            }
            if flushed.elapsed() >= CONSOLE_FLUSH_EVERY {
                file.flush().await?;
                flushed = std::time::Instant::now();
            }
        }
        console.send_modify(|console| console.see(&line));
    }
    file.flush().await?;
    console.send_modify(|console| console.closed = true);
    Ok(())
}

/// The parts of a server crash report that say what went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashReport {
    pub description: String,
    /// The exception line, such as the watchdog's error.
    pub error: String,
    /// The innermost game frames of the failing stack, as
    /// `Type.method`, innermost first.
    pub frames: Vec<String>,
}

pub fn parse_crash_report(text: &str) -> Option<CrashReport> {
    let mut lines = text.lines().map(str::trim);
    let description = lines
        .by_ref()
        .find_map(|line| line.strip_prefix("Description:"))?
        .trim()
        .to_owned();
    let error = lines.by_ref().find(|line| !line.is_empty())?.to_owned();
    let frames = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.strip_prefix("at "))
        .filter(|frame| frame.starts_with("net.minecraft."))
        .filter_map(|frame| {
            let call = frame.split('(').next()?;
            let mut parts = call.rsplitn(3, '.');
            let method = parts.next()?;
            let owner = parts.next()?;
            Some(format!("{owner}.{method}"))
        })
        .take(CRASH_FRAMES)
        .collect();
    Some(CrashReport {
        description,
        error,
        frames,
    })
}

/// The message of a server log line at level ERROR.
pub fn log_error(line: &str) -> Option<&str> {
    Some(line.split_once("/ERROR]: ")?.1.trim())
}

/// One line on why the server died, from its crash report and the
/// errors it logged; `None` when neither says anything.
pub fn crash_cause(report: Option<&CrashReport>, errors: &[String]) -> Option<String> {
    if report.is_none() && errors.is_empty() {
        return None;
    }
    let watchdog = report.is_some_and(|r| r.error.contains("Watchdog"))
        || errors.iter().any(|e| e.contains("single server tick took"));
    let kind = if watchdog {
        "watchdog"
    } else if report.is_some() {
        "crash"
    } else {
        "error"
    };
    let headline = errors
        .first()
        .cloned()
        .or_else(|| report.map(|r| format!("{}: {}", r.description, r.error)))
        .unwrap_or_default();
    let mut cause = format!("{kind}: {headline}");
    if let Some(report) = report.filter(|r| !r.frames.is_empty()) {
        cause.push_str("; at ");
        cause.push_str(&report.frames.join(" < "));
    }
    Some(cause)
}

/// Why the vanilla server in `instance` died, from its newest crash report
/// and the first errors in its log.
pub fn diagnose(instance: &Path) -> Option<String> {
    use std::io::{BufRead, Read};
    let newest = std::fs::read_dir(instance.join("crash-reports"))
        .ok()
        .and_then(|dir| {
            dir.filter_map(|entry| Some(entry.ok()?.path()))
                .filter(|path| path.extension().is_some_and(|e| e == "txt"))
                .max()
        });
    let report = newest.and_then(|path| {
        let mut text = String::new();
        std::fs::File::open(&path)
            .ok()?
            .take(MAX_CRASH_REPORT_BYTES)
            .read_to_string(&mut text)
            .ok()?;
        parse_crash_report(&text)
    });
    let mut errors = Vec::new();
    if let Ok(log) = std::fs::File::open(instance.join("logs").join("latest.log")) {
        let mut reader = std::io::BufReader::new(log);
        let mut line = Vec::new();
        while errors.len() < LOG_ERRORS
            && matches!(reader.read_until(b'\n', &mut line), Ok(n) if n > 0)
        {
            if let Some(message) = log_error(&String::from_utf8_lossy(&line)) {
                errors.push(message.to_owned());
            }
            line.clear();
        }
    }
    crash_cause(report.as_ref(), &errors)
}

pub struct Vanilla {
    child: Child,
    pid: u32,
    addr: SocketAddr,
    rcon: Rcon,
    console: watch::Receiver<Console>,
    instance: PathBuf,
}

impl Vanilla {
    /// Starts the server in `instance` (created, and expected to be
    /// fresh) and waits until it accepts players and RCON.
    pub async fn start(
        options: &VanillaOptions,
        mut settings: Settings,
        instance: &Path,
    ) -> Result<Self> {
        options.require_eula()?;
        let java = java(options)?;
        let jar = server_jar(options)?;
        std::fs::create_dir_all(instance)?;
        if settings.port == 0 {
            settings.port = free_port(settings.host)?;
        }
        let rcon_host = if settings.host.is_unspecified() {
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        } else {
            settings.host
        };
        if settings.rcon_port == 0 {
            settings.rcon_port = free_port(rcon_host)?;
        }
        if settings.rcon_password.is_empty() {
            settings.rcon_password = random_password();
        }
        std::fs::write(
            instance.join("eula.txt"),
            format!(
                "# accepted for this bench run by the person running it ({EULA_URL})\neula=true\n"
            ),
        )?;
        std::fs::write(instance.join("server.properties"), properties(&settings))?;

        let mut child = tokio::process::Command::new(&java)
            .args(&options.java_opts)
            .arg("-jar")
            .arg(&jar)
            .arg("nogui")
            .current_dir(instance)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {}", java.display()))?;
        let pid = child.id().context("the vanilla server exited at once")?;
        let (console_tx, mut console) = watch::channel(Console::default());
        let log = instance.join("console.log");
        tokio::spawn(follow(
            child.stdout.take().context("server stdout")?,
            log.clone(),
            console_tx.clone(),
        ));
        tokio::spawn(follow(
            child.stderr.take().context("server stderr")?,
            log.clone(),
            console_tx,
        ));
        let up = match tokio::time::timeout(
            READY_LIMIT,
            console.wait_for(|c| (c.done && c.rcon) || c.closed),
        )
        .await
        {
            Ok(Ok(c)) => c.done && c.rcon,
            _ => false,
        };
        if !up {
            let _ = child.start_kill();
            let _ = child.wait().await;
            bail!("the vanilla server did not come up; see {}", log.display());
        }
        let rcon = Rcon::connect(
            SocketAddr::new(rcon_host, settings.rcon_port),
            &settings.rcon_password,
        )
        .await?;
        Ok(Self {
            child,
            pid,
            addr: SocketAddr::new(settings.host, settings.port),
            rcon,
            console,
            instance: instance.to_path_buf(),
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn rcon(&self) -> Rcon {
        self.rcon.clone()
    }

    /// "Can't keep up!" warnings logged so far.
    pub fn overloaded(&self) -> u64 {
        self.console.borrow().overloaded
    }

    /// Refused moves logged so far.
    pub fn movement_warnings(&self) -> u64 {
        self.console.borrow().movement_warnings
    }

    /// Resolves once the server's output has ended, which it does when
    /// the process exits.
    pub async fn exited(&self) {
        let mut console = self.console.clone();
        let _ = console.wait_for(|c| c.closed).await;
    }

    /// Chunk holders across every level, from a performance profile taken
    /// now (`/perf start` then `/perf stop`).
    pub async fn chunk_rows(&mut self) -> Result<ChunkRows> {
        let before = self.console.borrow().profiles.len();
        self.rcon.command("perf start").await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.rcon.command("perf stop").await?;
        let console = tokio::time::timeout(
            PROFILE_LIMIT,
            self.console
                .wait_for(|c| c.profiles.len() > before || c.closed),
        )
        .await
        .context("no performance profile was written")??
        .clone();
        let relative = console
            .profiles
            .last()
            .filter(|_| console.profiles.len() > before)
            .context("the server stopped before writing a profile")?;
        let path = self.instance.join(relative);
        ensure!(
            std::fs::metadata(&path)?.len() <= MAX_PROFILE_BYTES,
            "profile {} is too large",
            path.display()
        );
        let data = std::fs::read(&path)?;
        let files = tuff::custom_pack::zip::read(&data, Default::default())?;
        let mut rows = ChunkRows::default();
        for (name, body) in files {
            if name.starts_with("server/levels/") && name.ends_with("/chunks.csv") {
                rows.add(std::str::from_utf8(&body)?);
            }
        }
        Ok(rows)
    }

    /// Stops the server through `/stop`, killing the process it started
    /// if it does not exit in time.
    pub async fn stop(mut self) -> Result<()> {
        let _ = tokio::time::timeout(Duration::from_secs(5), self.rcon.command("stop")).await;
        match tokio::time::timeout(STOP_LIMIT, self.child.wait()).await {
            Ok(status) => {
                status?;
            }
            Err(_) => {
                self.child.start_kill()?;
                self.child.wait().await?;
                bail!("the vanilla server did not stop in {STOP_LIMIT:?} and was killed");
            }
        }
        Ok(())
    }
}

type RconRequest = (String, oneshot::Sender<Result<String>>);

/// An RCON connection, owned by one task that runs commands one at a
/// time; clones queue on a small buffered channel, so callers wait their
/// turn rather than piling up unbounded work.
#[derive(Clone)]
pub struct Rcon {
    requests: mpsc::Sender<RconRequest>,
}

impl Rcon {
    pub async fn connect(addr: SocketAddr, password: &str) -> Result<Self> {
        let mut stream = TcpStream::connect(addr).await?;
        stream
            .write_all(&encode_rcon(1, RCON_LOGIN, password))
            .await?;
        let (id, _, _) = read_rcon(&mut stream).await?;
        ensure!(id == 1, "RCON refused the password");
        let (requests, inbox) = mpsc::channel(64);
        tokio::spawn(serve_rcon(stream, inbox));
        Ok(Self { requests })
    }

    pub async fn command(&self, line: impl Into<String>) -> Result<String> {
        let line = line.into();
        ensure!(line.len() <= RCON_MAX_COMMAND, "command too long for RCON");
        let (reply, answer) = oneshot::channel();
        self.requests
            .send((line, reply))
            .await
            .map_err(|_| anyhow::anyhow!("RCON connection closed"))?;
        answer.await?
    }
}

async fn serve_rcon(mut stream: TcpStream, mut inbox: mpsc::Receiver<RconRequest>) {
    let mut next = 2;
    while let Some((line, reply)) = inbox.recv().await {
        let id = next;
        next = if next == i32::MAX { 2 } else { next + 1 };
        let result = exchange(&mut stream, id, &line).await;
        let failed = result.is_err();
        let _ = reply.send(result);
        if failed {
            break;
        }
    }
}

/// Sends one command and joins its answer. The server reads one packet
/// per socket read, so nothing else may be in flight, and it splits an
/// answer into packets of [`RCON_CHUNK`] UTF-16 units: a shorter packet
/// is the last, and after a full one the next is waited for briefly.
async fn exchange(stream: &mut TcpStream, id: i32, line: &str) -> Result<String> {
    stream
        .write_all(&encode_rcon(id, RCON_COMMAND, line))
        .await?;
    let mut body = String::new();
    let mut first = true;
    loop {
        let packet = if first {
            read_rcon(stream).await?
        } else {
            match tokio::time::timeout(RCON_TAIL_WAIT, read_rcon(stream)).await {
                Ok(packet) => packet?,
                Err(_) => return Ok(body),
            }
        };
        first = false;
        let (got, _, text) = packet;
        ensure!(got == id, "RCON answered request {got}, expected {id}");
        let full = text.encode_utf16().count() >= RCON_CHUNK;
        body.push_str(&text);
        if !full {
            return Ok(body);
        }
    }
}

pub fn encode_rcon(id: i32, kind: i32, body: &str) -> Vec<u8> {
    let length = 4 + 4 + body.len() + 2;
    let mut out = Vec::with_capacity(4 + length);
    out.extend((length as i32).to_le_bytes());
    out.extend(id.to_le_bytes());
    out.extend(kind.to_le_bytes());
    out.extend(body.as_bytes());
    out.extend([0, 0]);
    out
}

/// One packet's id, type and body, from the bytes after its length.
pub fn decode_rcon(packet: &[u8]) -> Result<(i32, i32, String)> {
    ensure!(packet.len() >= 10, "RCON packet too short");
    let int = |at: usize| i32::from_le_bytes(packet[at..at + 4].try_into().unwrap());
    let body = &packet[8..packet.len() - 2];
    Ok((int(0), int(4), String::from_utf8_lossy(body).into_owned()))
}

async fn read_rcon(stream: &mut TcpStream) -> Result<(i32, i32, String)> {
    let length = stream.read_i32_le().await?;
    let length = usize::try_from(length).unwrap_or(0);
    ensure!(
        (10..=RCON_MAX_PACKET).contains(&length),
        "bad RCON packet length {length}"
    );
    let mut packet = vec![0; length];
    stream.read_exact(&mut packet).await?;
    decode_rcon(&packet)
}
