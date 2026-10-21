//! A standard, repeatable load benchmark. The server runs in this process
//! while a fleet of bots, in a child process, plays against it:
//! spectators flying out radially (fresh chunk generation and sending),
//! creative miners digging pits, summoned mobs, and survival walkers
//! doing ordinary play (walking, building, digging, fighting, chests).
//! After a warm-up it measures for a fixed window and prints one JSON
//! report: tick time percentiles and MSPT, chunk and byte throughput per
//! player, and the server process's CPU and memory.
//!
//! ```text
//! cargo bench --bench stress
//! cargo bench --bench stress -- --preset normal --players 16 --duration 2m
//! cargo bench --bench stress -- --config tuff.antiXray.enabled=true --label hide --out hide.json
//! cargo bench --bench stress -- --compare off.json hide.json
//! ```
//!
//! Flags: `--preset synthetic|normal|mixed` (synthetic), `--players <n>`
//! (8), `--flyers`/`--miners`/`--walkers`/`--entities <n>` (from the
//! preset), `--idlers <n>` (0, survival players standing at spawn), `--sprinters <n>` (0, survival players sprint-jumping), `--entity_types <a,b,..>`, `--duration <d>` (60s),
//! `--warmup <d>` (10s), `--seed <n>` (1), `--view_distance <n>` (10),
//! `--speed <blocks per tick>` (2.0), `--mine_rate <blocks per tick>` (1),
//! `--home_radius <blocks>` (40), `--tick_lockstep <always|sprint>`
//! (always), `--config <key>=<value>` (repeatable), `--label <text>`,
//! `--out <file>`, `--instance_dir <dir>` (a fresh temporary directory),
//! `--ops <name,..>` (players made operators so a human can join and
//! watch), `--host <ip>` (127.0.0.1), `--port <n>` (0, an ephemeral port),
//! `--server tuff|vanilla|both` (tuff), and for vanilla
//! `--accept-mojang-eula`, `--vanilla_jar <path>`, `--java <path>`,
//! `--java_opts "<flags>"` (-Xmx4G). `--timings` records tuff's span
//! timings over the window and saves the report beside `--out`.

#[path = "../common/alloc.rs"]
mod alloc;
#[path = "../common/console.rs"]
mod console;
mod fleet;
mod heap;
mod linger;
#[path = "../common/memory.rs"]
mod memory;
mod options;
#[path = "../common/servers.rs"]
mod servers;
#[path = "../common/vanilla.rs"]
mod vanilla;
#[path = "../common/watchers.rs"]
mod watchers;

use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::watch;
use tuff_bot::BotConfig;
use tuff_bot::packets::Kinds;
use tuff_bot::report::{BotTotals, ProcessSample, TickSummary};
use tuff_timings::{StartOptions, Timings};

use options::{Mode, Options};
use servers::{BenchServer, Choice, Samples, Spec};

const SHUTDOWN_LIMIT: Duration = Duration::from_secs(60);
const READY_LIMIT: Duration = Duration::from_secs(300);
const SPOT_RADIUS: i32 = 24;
const SUMMON_BATCH: usize = 64;
const MIB: f64 = 1024.0 * 1024.0;
const PROBE: &str = "TickProbe";
const PROBE_VIEW_DISTANCE: i8 = 2;

fn main() -> Result<()> {
    if !std::env::args().any(|arg| arg == "--bench") {
        println!("[stress] the stress target only runs via `cargo bench --bench stress`");
        return Ok(());
    }
    let mode = options::parse()?;
    if let Mode::Run(options) = &mode
        && options.server.needs_vanilla()
    {
        options.vanilla.require_eula()?;
        if options.timings {
            anyhow::bail!("--timings records tuff only");
        }
    }
    tuff_core::threads::init_rayon();
    if let Mode::Run(options) = &mode {
        alloc::sample_every(options.heap_sample);
    }
    let runtime = tuff::allocator::runtime()?;
    let result = match mode {
        Mode::Compare(before, after) => compare(&before, &after),
        Mode::Fleet => runtime.block_on(fleet::run()),
        Mode::Run(options) if options.server == Choice::Both => {
            servers::run_both("stress", options.out.as_deref())
        }
        Mode::Run(options) => {
            let result = runtime.block_on(run((*options).clone()));
            if let (Err(error), Choice::One(kind)) = (&result, options.server) {
                servers::report_failure(
                    "stress",
                    kind,
                    options.instance_dir.as_deref(),
                    options.out.as_deref(),
                    error,
                );
            }
            result
        }
    };
    std::thread::Builder::new()
        .name("shutdown-guard".into())
        .spawn(|| {
            std::thread::sleep(SHUTDOWN_LIMIT);
            eprintln!("[stress] shutdown took longer than {SHUTDOWN_LIMIT:?}, aborting");
            std::process::abort();
        })?;
    runtime.shutdown_timeout(Duration::from_secs(10));
    result?;
    std::process::exit(0)
}

fn compare(before: &std::path::Path, after: &std::path::Path) -> Result<()> {
    let read = |path: &std::path::Path| -> Result<Value> {
        let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
        Ok(serde_json::from_str(&text)?)
    };
    let (before, after) = (read(before)?, read(after)?);
    let differ = before["server"] != after["server"];
    if differ {
        let label = |report: &Value| report["server"].as_str().unwrap_or("tuff").to_owned();
        let (a, b) = (label(&before), label(&after));
        print!(
            "{}",
            tuff_bot::report::side_by_side(&before, &after, [&a, &b])
        );
    } else {
        print!("{}", tuff_bot::report::compare(&before, &after));
    }
    Ok(())
}

fn log(text: impl AsRef<str>) {
    eprintln!("[stress] {}", text.as_ref());
}

struct Fleet {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
}

impl Fleet {
    fn spawn() -> Result<Self> {
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args(["--bench", "--fleet"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let input = child.stdin.take().context("fleet stdin")?;
        let output = BufReader::new(child.stdout.take().context("fleet stdout")?).lines();
        Ok(Self {
            child,
            input,
            output,
        })
    }

    fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    async fn send(&mut self, value: Value) -> Result<()> {
        self.input
            .write_all(format!("{value}\n").as_bytes())
            .await?;
        self.input.flush().await?;
        Ok(())
    }

    /// The next JSON line from the fleet, skipping anything else it prints.
    async fn reply(&mut self) -> Result<Value> {
        loop {
            let line = self.output.next_line().await?.context("the fleet exited")?;
            if line.starts_with('{') {
                return Ok(serde_json::from_str(&line)?);
            }
        }
    }

    async fn ask(&mut self, value: Value) -> Result<Value> {
        self.send(value).await?;
        self.reply().await
    }

    async fn totals(&mut self, cmd: &str) -> Result<(BotTotals, Kinds)> {
        let reply = self.ask(json!({ "cmd": cmd })).await?;
        Ok((
            serde_json::from_value(reply["totals"].clone())?,
            serde_json::from_value(reply["kinds"].clone())?,
        ))
    }
}

async fn run(options: Options) -> Result<()> {
    let Choice::One(kind) = options.server else {
        anyhow::bail!("run takes one server");
    };
    let (instance, temporary) =
        servers::instance_dir("stress", kind, options.instance_dir.as_deref())?;
    log(format!(
        "{} on {}: {} flyers, {} miners, {} walkers, {} idlers, {} sprinters, {} entities, {:?} after {:?} warm-up",
        options.preset.name(),
        kind.name(),
        options.flyers,
        options.miners,
        options.walkers,
        options.idlers,
        options.sprinters,
        options.entities,
        options.duration,
        options.warmup
    ));

    let names = fleet::names(
        options.flyers,
        options.miners,
        options.walkers,
        options.idlers,
        options.sprinters,
        options.entities > 0,
    );
    options
        .watch
        .check_bots(names.iter().map(String::as_str).chain([PROBE]))?;
    let mut server = BenchServer::start(Spec {
        kind,
        instance: instance.clone(),
        watch: options.watch.clone(),
        seed: options.seed,
        view_distance: options.view_distance.into(),
        max_players: Some(options.bots() + 9),
        tick_lockstep: options.tick_lockstep,
        config: options.config.clone(),
        vanilla: options.vanilla.clone(),
    })
    .await?;
    let console = server.console();
    let addr = server.addr();
    log(options.watch.join_hint(server.bound()));
    for line in [
        "gamerule keep_inventory true",
        "gamerule advance_time false",
        "gamerule advance_weather false",
        "time set 1000",
        "weather clear",
    ] {
        console.run(line).await?;
    }
    let memory_start = linger::at_start(&server).await;
    let heap_start = heap::snapshot();
    let (probe, samples) = servers::probe(
        &server,
        BotConfig {
            view_distance: PROBE_VIEW_DISTANCE,
            ..BotConfig::new(PROBE)
        },
    )
    .await?;
    console
        .run_for_player(&format!("gamemode spectator {PROBE}"))
        .await?;

    let mut bots = Fleet::spawn()?;
    bots.send(json!({
        "addr": addr.to_string(),
        "names": names,
        "view_distance": options.view_distance,
        "speed": options.speed,
        "mine_rate": options.mine_rate,
        "home_radius": options.home_radius,
        "seed": options.seed,
    }))
    .await?;
    let joining = Instant::now();
    let ready = tokio::time::timeout(READY_LIMIT, bots.reply())
        .await
        .context("the fleet never got ready")??;
    let spawn: Vec<f64> = serde_json::from_value(ready["ready"]["spawn"].clone())?;
    log(format!(
        "{} bots joined in {:.1}s",
        names.len(),
        joining.elapsed().as_secs_f64()
    ));

    let mut miner = 0;
    for name in &names {
        let role = |prefix: &str| name.starts_with(prefix);
        if role("Fly") {
            console.run_for_player(&format!("op {name}")).await?;
            console
                .run_for_player(&format!("gamemode spectator {name}"))
                .await?;
        } else if role("Mine") || role("Anchor") {
            console.run_for_player(&format!("op {name}")).await?;
            console
                .run_for_player(&format!("gamemode creative {name}"))
                .await?;
            if role("Mine") {
                let angle = std::f64::consts::TAU * f64::from(miner) / f64::from(options.miners);
                let radius = 32.0 + 2.0 * f64::from(options.miners);
                miner += 1;
                let (x, z) = (
                    (spawn[0] + radius * angle.sin()).floor() + 0.5,
                    (spawn[2] + radius * angle.cos()).floor() + 0.5,
                );
                console.run(&format!("tp {name} {x} 200 {z}")).await?;
            }
        } else {
            console
                .run_for_player(&format!("gamemode survival {name}"))
                .await?;
            for item in ["iron_sword 1", "chest 16", "dirt 64"] {
                console
                    .run(&format!("give {name} minecraft:{item}"))
                    .await?;
            }
            console
                .run(&format!(
                    "effect give {name} minecraft:saturation infinite 0 true"
                ))
                .await?;
        }
    }

    let mut summoned = 0;
    let mut failed = 0;
    if options.entities > 0 {
        let reply = bots
            .ask(json!({"cmd": "spots", "count": options.entities, "radius": SPOT_RADIUS}))
            .await?;
        let spots: Vec<[f64; 3]> = serde_json::from_value(reply["spots"].clone())?;
        for batch in spots.chunks(SUMMON_BATCH) {
            let mut summons = tokio::task::JoinSet::new();
            for spot in batch {
                let kind = &options.entity_types[summoned % options.entity_types.len()];
                summoned += 1;
                let line = format!(
                    "summon minecraft:{kind} {} {} {} {{PersistenceRequired:1b}}",
                    spot[0], spot[1], spot[2]
                );
                let console = console.clone();
                summons.spawn(async move { console.run(&line).await });
            }
            while let Some(done) = summons.join_next().await {
                let answer = done??;
                if !console::summoned(&answer) {
                    failed += 1;
                    if failed == 1 {
                        log(format!("summon failed: {answer}"));
                    }
                }
            }
        }
        log(format!("summoned {} entities", summoned - failed));
    }

    let source = server.tick_source(samples).await?;
    let ticks_source = source.describe();
    let (done, done_rx) = watch::channel(false);
    let tick_task = tokio::spawn(servers::collect_ticks(source, done_rx.clone()));
    let sample_task = tokio::spawn(servers::sample_processes(server.pid(), bots.pid(), done_rx));
    bots.ask(json!({"cmd": "go"})).await?;
    tokio::time::sleep(options.warmup).await;
    let before = bots.totals("mark").await?;
    let timings = if options.timings {
        Some(start_timings(&server).await?)
    } else {
        None
    };
    let window_start = Instant::now();
    let watchers_start = Value::from(server.online(&options.watch.ops).await?);
    log("measuring");
    let mut problems = Vec::new();
    tokio::select! {
        _ = tokio::time::sleep(options.duration) => {}
        _ = server.stopped() => problems.push("the server stopped during the window".to_string()),
    }
    let after = bots.totals("mark").await?;
    let window_end = Instant::now();
    let _ = done.send(true);
    let timings = match &timings {
        Some(timings) => Some(save_timings(timings, options.out.as_deref()).await?),
        None => None,
    };
    let mut soft = |what: &str, result: Result<Value>| {
        result
            .inspect_err(|error| problems.push(format!("{what}: {error:#}")))
            .unwrap_or(Value::Null)
    };
    let watchers_end = soft(
        "watchers",
        server.online(&options.watch.ops).await.map(Value::from),
    );
    let tick_query = soft(
        "tick query",
        console
            .tick_query()
            .await
            .map(|q| q.map_or(Value::Null, console::TickQuery::to_json)),
    );
    let world = soft("world", server.world().await);
    let memory_end = soft("memory at end", linger::at_end(&server).await);
    let heap_end = heap::snapshot();
    bots.totals("stop").await?;
    let (ticks, lagged) = tick_task.await?;
    let samples = sample_task.await?;
    let _ = tokio::time::timeout(Duration::from_secs(10), bots.child.wait()).await;
    drop(probe);
    let after_leave = if options.linger.is_zero() {
        Value::Null
    } else {
        log(format!(
            "lingering {:?} after the bots left",
            options.linger
        ));
        soft(
            "after leave",
            linger::after_leave(&server, kind, options.linger).await,
        )
    };

    let heap_after_leave = heap::snapshot().filter(|_| !options.linger.is_zero());
    let mut heap_report = json!({});
    if let Some(start) = &heap_start {
        heap_report["at_start"] = heap::report(start, None).await;
        if let Some(end) = &heap_end {
            heap_report["at_end_growth"] = heap::report(end, Some(start)).await;
        }
        if let Some(after) = &heap_after_leave {
            heap_report["after_leave_growth"] = heap::report(after, Some(start)).await;
        }
    }
    let memory_start = soft("memory at start", memory_start);

    let mut report = report(
        &options,
        (summoned, failed),
        window_start,
        window_end,
        &ticks,
        lagged,
        &samples,
        (&after.0.since(&before.0), &after.1.since(&before.1)),
        [&watchers_start, &watchers_end],
    );
    report["server"] = kind.name().into();
    report["machine"] = servers::machine();
    report["ticks_source"] = ticks_source.into();
    report["tick_query"] = tick_query;
    report["world"] = world;
    report["memory"]["at_end"] = memory_end;
    if !after_leave.is_null() && !memory_start.is_null() {
        let ratios = linger::idle_vs_start(&memory_start, &after_leave);
        log(format!("after leaving vs start: {ratios}"));
        report["memory"]["idle_vs_start"] = ratios;
    }
    report["memory"]["at_start"] = memory_start;
    report["memory"]["after_leave"] = after_leave;
    if heap_start.is_some() {
        report["heap"] = heap_report;
    }
    if let Some(timings) = timings {
        report["timings"] = timings;
    }
    let failed = (!problems.is_empty()).then(|| anyhow::anyhow!(problems.join("; ")));
    if let Some(error) = &failed {
        report["failed"] = servers::failure(kind, &instance, error);
    }
    if kind == servers::Kind::Vanilla {
        report["vanilla"] = options.vanilla.to_json();
    }
    let text = serde_json::to_string_pretty(&report)?;
    if let Some(out) = &options.out {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(out, format!("{text}\n"))?;
        log(format!("report written to {}", out.display()));
    }
    println!("{text}");
    log(format!(
        "{}: {}",
        kind.name(),
        tuff_bot::report::verdict(&report)
    ));

    if let Some(error) = failed {
        let _ = server.shutdown().await;
        return Err(error);
    }
    let stopping = Instant::now();
    server.shutdown().await?;
    let stopped = stopping.elapsed().as_secs_f64();
    log(format!("{} shut down in {stopped:.1}s", kind.name()));
    if let Some(out) = &options.out {
        report["shutdown_s"] = stopped.into();
        std::fs::write(out, format!("{}\n", serde_json::to_string_pretty(&report)?))?;
    }
    if temporary {
        std::fs::remove_dir_all(&instance)?;
    }
    Ok(())
}

async fn start_timings(server: &BenchServer) -> Result<Timings> {
    let BenchServer::Tuff { handle, .. } = server else {
        anyhow::bail!("--timings records tuff only");
    };
    let timings = handle.timings().await?;
    timings
        .start(StartOptions {
            duration: None,
            by: "stress bench".into(),
        })
        .await?;
    Ok(timings)
}

/// Stops the recording and writes its report beside `out`
/// (`<out>.timings.json`), or under the target directory without one;
/// returns the report's summary.
async fn save_timings(timings: &Timings, out: Option<&std::path::Path>) -> Result<Value> {
    let summary = timings.stop().await?;
    let json = timings.fetch(&summary.id).await?;
    let path = match out {
        Some(out) => out.with_extension("timings.json"),
        None => vanilla::target_dir()?
            .join("stress-timings")
            .join(format!("{}.json", summary.id)),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &json)?;
    log(format!("timings written to {}", path.display()));
    Ok(json!({
        "id": summary.id,
        "file": path.display().to_string(),
        "bytes": json.len(),
        "duration_s": summary.duration.as_secs_f64(),
        "ticks": summary.ticks,
        "mspt_mean": summary.mspt_mean,
        "mspt_p99": summary.mspt_p99,
        "threads": summary.threads,
        "dropped": summary.dropped,
        "top_self_ms": summary
            .top
            .iter()
            .map(|(label, ns)| (label.clone(), Value::from(*ns as f64 / 1e6)))
            .collect::<serde_json::Map<_, _>>(),
    }))
}

fn setbacks(bots: &BotTotals) -> Value {
    let (ratio, invalid) = tuff_bot::report::setback_check(bots.setbacks, bots.moves);
    let mut setbacks = json!({
        "count": bots.setbacks,
        "moves": bots.moves,
        "ratio": ratio,
    });
    if let Some(invalid) = invalid {
        setbacks["invalid"] = invalid.into();
    }
    setbacks
}

#[allow(clippy::too_many_arguments)]
fn report(
    options: &Options,
    (summoned, failed): (usize, usize),
    start: Instant,
    end: Instant,
    ticks: &[(Instant, f64)],
    lagged: u64,
    samples: &Samples,
    (bots, kinds): (&BotTotals, &Kinds),
    watchers: [&Value; 2],
) -> Value {
    let window = end.duration_since(start).as_secs_f64();
    let in_window: Vec<f64> = ticks
        .iter()
        .filter(|(at, _)| *at >= start && *at <= end)
        .map(|(_, ms)| *ms)
        .collect();
    let server = servers::window(samples, 0, start, end);
    let fleet = servers::window(samples, 1, start, end);
    let cpu = |series: &[(Instant, ProcessSample)]| match (series.first(), series.last()) {
        (Some(first), Some(last)) if last.0 > first.0 => {
            let used = last.1.cpu_s - first.1.cpu_s;
            (used, used / last.0.duration_since(first.0).as_secs_f64())
        }
        _ => (0.0, 0.0),
    };
    let mib = |sample: Option<&(Instant, ProcessSample)>| {
        sample.map_or(0.0, |(_, s)| s.rss_bytes as f64 / MIB)
    };
    let peak =
        |series: &[(Instant, ProcessSample)]| mib(series.iter().max_by_key(|(_, s)| s.rss_bytes));
    let mean = |series: &[(Instant, ProcessSample)]| {
        series
            .iter()
            .map(|(_, s)| s.rss_bytes as f64 / MIB)
            .sum::<f64>()
            / series.len().max(1) as f64
    };
    let (server_s, server_cores) = cpu(&server);
    let (fleet_s, fleet_cores) = cpu(&fleet);
    let players = bots.players.max(1) as f64;
    let per_s = |v: u64| v as f64 / window.max(f64::EPSILON);
    json!({
        "bench": "stress",
        "schema": 2,
        "label": options.label,
        "options": options.to_json(),
        "window_s": window,
        "ticks": TickSummary::new(&in_window, window, lagged),
        "chunks": {
            "received": bots.chunks,
            "per_s": per_s(bots.chunks),
            "per_player_per_s": per_s(bots.chunks) / players,
            "bytes": bots.chunk_bytes,
            "bytes_per_chunk": bots.chunk_bytes as f64 / bots.chunks.max(1) as f64,
        },
        "network": {
            "wire_bytes": bots.wire_bytes,
            "wire_bytes_per_s": per_s(bots.wire_bytes),
            "wire_bytes_per_player": bots.wire_bytes as f64 / players,
            "wire_bytes_per_player_per_s": per_s(bots.wire_bytes) / players,
            "payload_bytes": bots.payload_bytes,
            "compression_ratio": bots.payload_bytes as f64 / bots.wire_bytes.max(1) as f64,
            "packets": bots.packets,
            "packets_per_s": per_s(bots.packets),
        },
        "cpu": {
            "server_s": server_s,
            "server_cores": server_cores,
            "fleet_s": fleet_s,
            "fleet_cores": fleet_cores,
        },
        "memory": {
            "server_rss_start_mib": mib(server.first()),
            "server_rss_end_mib": mib(server.last()),
            "server_rss_mean_mib": mean(&server),
            "server_rss_peak_mib": peak(&server),
            "fleet_rss_peak_mib": peak(&fleet),
        },
        "packets": kinds.report(bots.players, window),
        "bots": bots,
        "setbacks": setbacks(bots),
        "entities": { "summoned": summoned, "summon_failed": failed },
        "watchers": {
            "ops": options.watch.ops,
            "online_at_start": watchers[0],
            "online_at_end": watchers[1],
        },
    })
}
