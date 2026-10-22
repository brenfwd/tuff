//! The server a bench runs against: tuff in this process, or the vanilla
//! jar as a child process (`--server tuff|vanilla|both`). Both take the
//! same commands as plain text, report every tick's duration and are
//! sampled for CPU and memory by process id, so their reports line up
//! field for field. `both` runs the bench once per server in a fresh
//! process and prints the two reports side by side.

// Shared by several bench targets, each of which uses only part of it.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tuff::world::tick_barrier::TickLockstep;
use tuff::world::tick_rate::TickTime;
use tuff::{Server, ServerHandle};
use tuff_bot::{Bot, BotConfig, TickSample};

use crate::console;
use crate::vanilla::{Rcon, Settings, Vanilla, VanillaOptions};
use crate::watchers::{self, Watch};

/// A save of every chunk a long flyer run generated can take a while on a
/// loaded machine, so the wait is generous; the bench logs how long it took.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(300);
const RETRIES: usize = 200;
pub const SAMPLE_EVERY: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Tuff,
    Vanilla,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Tuff => "tuff",
            Self::Vanilla => "vanilla",
        }
    }
}

/// `--server`: one server, or each in turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    One(Kind),
    Both,
}

impl Choice {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "tuff" => Some(Self::One(Kind::Tuff)),
            "vanilla" => Some(Self::One(Kind::Vanilla)),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    pub fn needs_vanilla(self) -> bool {
        self != Self::One(Kind::Tuff)
    }
}

pub struct Spec {
    pub kind: Kind,
    pub instance: PathBuf,
    pub watch: Watch,
    pub seed: i64,
    pub view_distance: i32,
    pub max_players: Option<u32>,
    pub tick_lockstep: TickLockstep,
    pub config: Vec<(String, String)>,
    pub simulation_distance: i32,
    pub vanilla: VanillaOptions,
}

/// Where a run keeps its instance: `--instance_dir` (kept afterwards), or
/// a fresh directory under the target directory that is removed afterwards,
/// so both servers keep their worlds on the same disk (and never on a
/// tmpfs `/tmp`).
pub fn instance_dir(bench: &str, kind: Kind, chosen: Option<&Path>) -> Result<(PathBuf, bool)> {
    if let Some(dir) = chosen {
        return Ok((dir.to_path_buf(), false));
    }
    let dir = instance_path(bench, kind, None)?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok((dir, true))
}

/// Where this process's run keeps its instance, as [`instance_dir`]
/// chooses it.
pub fn instance_path(bench: &str, kind: Kind, chosen: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = chosen {
        return Ok(dir.to_path_buf());
    }
    let name = format!("tuff-{bench}-bench-{}-{}", kind.name(), std::process::id());
    Ok(crate::vanilla::target_dir()?
        .join("vanilla-bench")
        .join(name))
}

/// Why the server in `instance` died, as far as it left a trace.
pub fn diagnose(kind: Kind, instance: &Path) -> Option<String> {
    match kind {
        Kind::Tuff => None,
        Kind::Vanilla => crate::vanilla::diagnose(instance),
    }
}

/// The `failed` section of a report: the error that ended the run and,
/// when the server left a trace, why it died.
pub fn failure(kind: Kind, instance: &Path, error: &anyhow::Error) -> Value {
    let mut failed = json!({
        "error": format!("{error:#}"),
        "instance": instance.display().to_string(),
    });
    if let Some(cause) = diagnose(kind, instance) {
        failed["cause"] = cause.into();
    }
    failed
}

/// After a run fails before writing its report: prints why and writes a
/// report holding only that, so `--server both` can still show it.
pub fn report_failure(
    bench: &str,
    kind: Kind,
    chosen: Option<&Path>,
    out: Option<&Path>,
    error: &anyhow::Error,
) {
    let Ok(instance) = instance_path(bench, kind, chosen) else {
        return;
    };
    let failed = failure(kind, &instance, error);
    if let Some(cause) = failed["cause"].as_str() {
        eprintln!("[{bench}] the {} server died: {cause}", kind.name());
    }
    eprintln!("[{bench}] its instance is kept at {}", instance.display());
    let Some(out) = out.filter(|out| !out.exists()) else {
        return;
    };
    let report = json!({ "bench": bench, "server": kind.name(), "failed": failed });
    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(out, format!("{report:#}\n"));
}

/// Runs console commands, answering in plain text.
#[derive(Clone)]
pub enum Console {
    Tuff(ServerHandle),
    Vanilla(Rcon),
}

impl Console {
    pub async fn run(&self, line: &str) -> Result<String> {
        match self {
            Self::Tuff(handle) => {
                let lines = handle.run_command(line).await?;
                Ok(lines
                    .iter()
                    .map(|line| line.plain())
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            Self::Vanilla(rcon) => rcon.command(line).await,
        }
    }

    /// Runs `line` until its target player is known to the server.
    pub async fn run_for_player(&self, line: &str) -> Result<String> {
        for _ in 0..RETRIES {
            let answer = self.run(line).await?;
            if !console::player_missing(&answer) {
                return Ok(answer);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        bail!("`{line}` never found its player")
    }

    pub async fn op(&self, name: &str) -> Result<()> {
        let answer = self.run(&format!("op {name}")).await?;
        if !console::opped(&answer) {
            bail!("could not op {name}: {answer}");
        }
        Ok(())
    }

    pub async fn players(&self) -> Result<Vec<String>> {
        let answer = self.run("list").await?;
        console::player_list(&answer).with_context(|| format!("list gave no player list: {answer}"))
    }

    pub async fn entities(&self) -> Result<u64> {
        let answer = self.run("execute if entity @e").await?;
        console::entity_count(&answer).with_context(|| format!("no entity count in {answer}"))
    }

    pub async fn tick_query(&self) -> Result<Option<console::TickQuery>> {
        Ok(console::tick_query(&self.run("tick query").await?))
    }
}

pub enum BenchServer {
    Tuff {
        handle: ServerHandle,
        task: JoinHandle<Result<()>>,
        keep_ops: JoinHandle<()>,
        bound: SocketAddr,
    },
    Vanilla(Vanilla),
}

impl BenchServer {
    pub async fn start(spec: Spec) -> Result<Self> {
        std::fs::create_dir_all(&spec.instance)?;
        match spec.kind {
            Kind::Tuff => {
                let mut overrides = vec![
                    ("pause-when-empty-seconds".to_string(), "-1".to_string()),
                    ("tuff.allowPublicOffline".to_string(), "true".to_string()),
                    (
                        "simulation-distance".to_string(),
                        spec.simulation_distance.to_string(),
                    ),
                ];
                overrides.extend(spec.config.clone());
                let server = Server::builder()
                    .features(tuff::vanilla_features::all())
                    .instance_dir(spec.instance.clone())
                    .binds(vec![spec.watch.bind()])
                    .online_mode(false)
                    .whitelist(false)
                    .view_distance(spec.view_distance)
                    .seed(spec.seed)
                    .tick_lockstep(spec.tick_lockstep)
                    .config_overrides(overrides)
                    .maybe_max_players(spec.max_players)
                    .build();
                let handle = server.handle();
                tuff::allocator::spawn_trimmer(handle.clone());
                let task = tokio::spawn(server.run());
                let bound = handle.local_addrs().await?[0];
                watchers::apply_ops(&handle, &spec.watch.ops).await?;
                let keep_ops = watchers::keep_ops(handle.clone(), spec.watch.ops.clone());
                Ok(Self::Tuff {
                    handle,
                    task,
                    keep_ops,
                    bound,
                })
            }
            Kind::Vanilla => {
                if !spec.config.is_empty() {
                    bail!("--config sets tuff config keys; vanilla has none of them");
                }
                let settings = Settings {
                    host: spec.watch.host,
                    port: spec.watch.port,
                    rcon_port: 0,
                    rcon_password: String::new(),
                    seed: spec.seed,
                    view_distance: spec.view_distance,
                    simulation_distance: spec.simulation_distance,
                    max_players: spec.max_players.unwrap_or(20),
                };
                let server =
                    Self::Vanilla(Vanilla::start(&spec.vanilla, settings, &spec.instance).await?);
                for name in &spec.watch.ops {
                    server.console().op(name).await?;
                }
                Ok(server)
            }
        }
    }

    pub fn bound(&self) -> SocketAddr {
        match self {
            Self::Tuff { bound, .. } => *bound,
            Self::Vanilla(vanilla) => vanilla.addr(),
        }
    }

    /// The address bots on this machine connect to.
    pub fn addr(&self) -> SocketAddr {
        watchers::reachable(self.bound())
    }

    /// The process whose CPU and memory are the server's.
    pub fn pid(&self) -> u32 {
        match self {
            Self::Tuff { .. } => std::process::id(),
            Self::Vanilla(vanilla) => vanilla.pid(),
        }
    }

    pub fn console(&self) -> Console {
        match self {
            Self::Tuff { handle, .. } => Console::Tuff(handle.clone()),
            Self::Vanilla(vanilla) => Console::Vanilla(vanilla.rcon()),
        }
    }

    /// Names from `names` (ignoring case) online now, as they joined.
    pub async fn online(&self, names: &[String]) -> Result<Vec<String>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        Ok(self
            .console()
            .players()
            .await?
            .into_iter()
            .filter(|player| names.iter().any(|name| name.eq_ignore_ascii_case(player)))
            .collect())
    }

    /// Where every tick's duration comes from: tuff's own tick broadcast,
    /// or the Debug Sample packets vanilla sends an operator subscribed to
    /// them (`samples`, from [`probe`]).
    pub async fn tick_source(
        &self,
        samples: mpsc::UnboundedReceiver<TickSample>,
    ) -> Result<TickSource> {
        Ok(match self {
            Self::Tuff { handle, .. } => TickSource::Tuff(handle.tick_times().await?),
            Self::Vanilla(_) => TickSource::Samples(samples),
        })
    }

    /// Server-side counts at the end of a run; `null` where the server
    /// has no way to tell.
    pub async fn world(&mut self) -> Result<Value> {
        let entities = self.console().entities().await?;
        let (holders, full, overloaded, refused) = match self {
            Self::Tuff { handle, .. } => {
                let snapshot = handle.metrics().await?.collect().await;
                (None, Some(snapshot.world.chunks), None, None)
            }
            Self::Vanilla(vanilla) => {
                let rows = vanilla.chunk_rows().await?;
                (
                    Some(rows.holders),
                    Some(rows.full),
                    Some(vanilla.overloaded()),
                    Some(vanilla.movement_warnings()),
                )
            }
        };
        Ok(json!({
            "entities": entities,
            "loaded_chunks": full,
            "chunk_holders": holders,
            "overload_warnings": overloaded,
            "movement_warnings": refused,
        }))
    }

    /// Resolves if the server stops on its own: the vanilla process
    /// exiting, or tuff's server task ending.
    pub async fn stopped(&self) {
        match self {
            Self::Tuff { task, .. } => {
                while !task.is_finished() {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
            Self::Vanilla(vanilla) => vanilla.exited().await,
        }
    }

    pub async fn shutdown(self) -> Result<()> {
        match self {
            Self::Tuff {
                handle,
                task,
                keep_ops,
                ..
            } => {
                keep_ops.abort();
                let shutdown = async {
                    handle.shutdown().await?;
                    task.await??;
                    Ok::<_, anyhow::Error>(())
                };
                tokio::time::timeout(SHUTDOWN_LIMIT, shutdown)
                    .await
                    .context("server shutdown timed out")?
            }
            Self::Vanilla(vanilla) => vanilla.stop().await,
        }
    }
}

/// A bot that asks for the server's per-tick timings; it is made an
/// operator once online (an offline name may resolve to another UUID),
/// which vanilla requires before it sends them. Every
/// bench connects one on both servers so the load stays the same.
pub async fn probe(
    server: &BenchServer,
    config: BotConfig,
) -> Result<(Bot, mpsc::UnboundedReceiver<TickSample>)> {
    let (sink, samples) = mpsc::unbounded_channel();
    let name = config.name.clone();
    let bot = Bot::connect(
        server.addr(),
        BotConfig {
            tick_samples: Some(sink),
            ..config
        },
    )
    .await?;
    server.console().op(&name).await?;
    Ok((bot, samples))
}

pub enum TickSource {
    Tuff(broadcast::Receiver<TickTime>),
    Samples(mpsc::UnboundedReceiver<TickSample>),
}

impl TickSource {
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Tuff(_) => "tuff tick broadcast: start of tick until every region finished it",
            Self::Samples(_) => {
                "vanilla debug sample (dedicated_server_tick_time): the server tick method's duration"
            }
        }
    }
}

/// Every tick's duration in milliseconds, stamped with when it arrived,
/// until `done`; and how many samples the collector missed.
pub async fn collect_ticks(
    source: TickSource,
    mut done: watch::Receiver<bool>,
) -> (Vec<(Instant, f64)>, u64) {
    let mut seen = Vec::new();
    let mut lagged = 0;
    match source {
        TickSource::Tuff(mut ticks) => loop {
            tokio::select! {
                tick = ticks.recv() => match tick {
                    Ok(tick) => seen.push((Instant::now(), tick.busy.as_secs_f64() * 1000.0)),
                    Err(broadcast::error::RecvError::Lagged(n)) => lagged += n,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = done.changed() => break,
            }
        },
        TickSource::Samples(mut samples) => loop {
            tokio::select! {
                sample = samples.recv() => match sample {
                    Some(sample) => seen.push((sample.received, sample.tick_ns as f64 / 1e6)),
                    None => break,
                },
                _ = done.changed() => break,
            }
        },
    }
    (seen, lagged)
}

/// The command-line arguments with `--server` and `--out` (and their
/// values) removed.
pub fn args_without_server(args: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut kept = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--server" || arg == "--out" {
            args.next();
        } else {
            kept.push(arg);
        }
    }
    kept
}

/// `--server both`: runs this bench once per server, each in a fresh
/// process, writes both reports and prints them side by side.
pub fn run_both(bench: &str, out: Option<&Path>) -> Result<()> {
    let base = match out {
        Some(out) => out.with_extension(""),
        None => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            crate::vanilla::target_dir()?
                .join("vanilla-bench")
                .join("reports")
                .join(format!("{bench}-{stamp}"))
        }
    };
    let args = args_without_server(std::env::args().skip(1));
    let mut reports = Vec::new();
    let mut failed = Vec::new();
    for kind in [Kind::Tuff, Kind::Vanilla] {
        let path = PathBuf::from(format!("{}-{}.json", base.display(), kind.name()));
        let _ = std::fs::remove_file(&path);
        eprintln!("[{bench}] running against {}", kind.name());
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(&args)
            .args(["--server", kind.name(), "--out"])
            .arg(&path)
            .status()?;
        let report = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok());
        if !status.success() {
            failed.push(kind.name());
        }
        let report = match report {
            Some(report) => {
                eprintln!("[{bench}] {} report: {}", kind.name(), path.display());
                report
            }
            None => json!({
                "server": kind.name(),
                "failed": { "error": format!("the run exited ({status}) without a report") },
            }),
        };
        reports.push(report);
    }
    print!("{}", both_summary(&reports[0], &reports[1]));
    if !failed.is_empty() {
        bail!("the {} run failed", failed.join(" and "));
    }
    Ok(())
}

/// What `--server both` prints: a verdict line per server, whether the
/// two ran the same workload, then the reports (with per-move figures)
/// side by side.
pub fn both_summary(tuff: &Value, vanilla: &Value) -> String {
    use tuff_bot::report::{side_by_side, verdict, with_per_move, workload_check};
    let labels = ["tuff", "vanilla"];
    let workload = match workload_check(tuff, vanilla, labels) {
        Some(reason) => format!("INVALID: workload mismatch, {reason}"),
        None => "comparable".into(),
    };
    format!(
        "tuff     {}\nvanilla  {}\nworkload {workload}\n{}",
        verdict(tuff),
        verdict(vanilla),
        side_by_side(&with_per_move(tuff), &with_per_move(vanilla), labels)
    )
}

/// The CPU and memory figures a report gives for one process's samples
/// over the window, `samples` in time order.
pub fn process_figures(samples: &[(Instant, tuff_bot::report::ProcessSample)]) -> Value {
    const MIB: f64 = 1024.0 * 1024.0;
    let (cpu_s, cores) = match (samples.first(), samples.last()) {
        (Some(first), Some(last)) if last.0 > first.0 => {
            let used = last.1.cpu_s - first.1.cpu_s;
            (used, used / last.0.duration_since(first.0).as_secs_f64())
        }
        _ => (0.0, 0.0),
    };
    let rss: Vec<f64> = samples
        .iter()
        .map(|(_, s)| s.rss_bytes as f64 / MIB)
        .collect();
    let mean = if rss.is_empty() {
        0.0
    } else {
        rss.iter().sum::<f64>() / rss.len() as f64
    };
    json!({
        "cpu_s": cpu_s,
        "cores": cores,
        "rss_start_mib": rss.first().copied().unwrap_or(0.0),
        "rss_end_mib": rss.last().copied().unwrap_or(0.0),
        "rss_mean_mib": mean,
        "rss_peak_mib": rss.iter().copied().fold(0.0, f64::max),
        "samples": samples.len(),
    })
}

pub type Samples = Vec<(
    Instant,
    Option<tuff_bot::report::ProcessSample>,
    Option<tuff_bot::report::ProcessSample>,
)>;

/// Samples the server's process and, if given, another one (a bot
/// fleet) every [`SAMPLE_EVERY`] until `done`.
pub async fn sample_processes(
    server: u32,
    other: Option<u32>,
    mut done: watch::Receiver<bool>,
) -> Samples {
    use tuff_bot::report::sample_process;
    let mut samples = Vec::new();
    let mut every = tokio::time::interval(SAMPLE_EVERY);
    loop {
        tokio::select! {
            _ = every.tick() => {
                let pair = tokio::task::spawn_blocking(move || {
                    (sample_process(server), other.and_then(sample_process))
                })
                .await
                .unwrap_or((None, None));
                samples.push((Instant::now(), pair.0, pair.1));
            }
            _ = done.changed() => break,
        }
    }
    samples
}

/// The samples of the server (`which` 0) or the other process (1) taken
/// between `start` and `end`.
pub fn window(
    samples: &Samples,
    which: usize,
    start: Instant,
    end: Instant,
) -> Vec<(Instant, tuff_bot::report::ProcessSample)> {
    samples
        .iter()
        .filter(|(at, _, _)| *at >= start && *at <= end)
        .filter_map(|(at, server, other)| Some((*at, if which == 0 { *server } else { *other }?)))
        .collect()
}

pub fn machine() -> Value {
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cpus": std::thread::available_parallelism().map_or(0, |n| n.get()),
    })
}
