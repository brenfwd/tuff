//! A repeatable load benchmark: one bot with a 32-chunk view flies in a
//! straight line through fresh terrain, is teleported to the nearest
//! village (which stays loaded around it), and then the server runs
//! `/tick sprint`. Prints progress, then one JSON report of the sprint:
//! tick time percentiles, sprint rate, CPU and memory.
//!
//! ```text
//! cargo bench --bench village
//! cargo bench --bench village -- --fly_chunks 16 --sprint 1d
//! cargo bench --bench village -- --server both --accept-mojang-eula --fly_chunks 8 --sprint 1d
//! cargo bench --bench village -- --ops Alice,Bob --port 25570
//! samply record cargo bench --bench village
//! ```
//!
//! Flags: `--seed <n>` (1), `--fly_chunks <n>` (640), `--speed <blocks per
//! tick>` (4.4), `--sprint <duration>` (10d), `--warmup_sprint <duration>` (none, a
//! discarded sprint before the measured one), `--simulation_distance <n>`
//! (32, the view distance), `--instance_dir <dir>` (a fresh directory that
//! is removed afterwards), `--tick_lockstep
//! <sprint|always>` (always), `--ops <name,..>` (players made operators so
//! a human can join and watch), `--host <ip>` (127.0.0.1), `--port <n>`
//! (0, an ephemeral port), `--out <file>`, `--label <text>`, `--server
//! tuff|vanilla|both` (tuff), and for vanilla `--accept-mojang-eula`,
//! `--vanilla_jar <path>`, `--java <path>`, `--java_opts "<flags>"`
//! (-Xmx4G). The address to join is printed at start-up; the server is in
//! offline mode, so ops are keyed by offline UUIDs.

#[path = "common/alloc.rs"]
mod alloc;
#[path = "common/console.rs"]
mod console;
#[path = "common/servers.rs"]
mod servers;
#[path = "common/vanilla.rs"]
mod vanilla;
#[path = "common/watchers.rs"]
mod watchers;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::sync::watch;
use tuff::world::tick_barrier::TickLockstep;
use tuff_bot::report::TickSummary;
use tuff_bot::{BotConfig, FlyLine};

use servers::{BenchServer, Choice, Kind, Spec};
use vanilla::VanillaOptions;
use watchers::Watch;

const VIEW_DISTANCE: i8 = 32;
const CRUISE_Y: f64 = 160.0;
const BOT_NAME: &str = "Bench";
const SPRINT_POLL: Duration = Duration::from_millis(500);

struct Options {
    seed: i64,
    fly_chunks: i32,
    speed: f64,
    sprint: String,
    warmup_sprint: Option<String>,
    simulation_distance: i32,
    instance_dir: Option<PathBuf>,
    tick_lockstep: TickLockstep,
    watch: Watch,
    server: Choice,
    vanilla: VanillaOptions,
    out: Option<PathBuf>,
    label: String,
}

impl Options {
    fn to_json(&self) -> Value {
        json!({
            "seed": self.seed,
            "fly_chunks": self.fly_chunks,
            "speed": self.speed,
            "sprint": self.sprint,
            "view_distance": VIEW_DISTANCE,
            "simulation_distance": self.simulation_distance,
            "warmup_sprint": self.warmup_sprint,
            "tick_lockstep": self.tick_lockstep.name(),
        })
    }
}

fn options() -> Result<Options> {
    let mut options = Options {
        seed: 1,
        fly_chunks: 640,
        speed: 4.4,
        sprint: "10d".into(),
        warmup_sprint: None,
        simulation_distance: VIEW_DISTANCE.into(),
        instance_dir: None,
        tick_lockstep: TickLockstep::Always,
        watch: Watch::default(),
        server: Choice::One(Kind::Tuff),
        vanilla: VanillaOptions::default(),
        out: None,
        label: String::new(),
    };
    let mut args = std::env::args().skip(1).filter(|arg| arg != "--bench");
    while let Some(flag) = args.next() {
        let mut value = || args.next().with_context(|| format!("{flag} needs a value"));
        if options.watch.flag(&flag, &mut value)? || options.vanilla.flag(&flag, &mut value)? {
            continue;
        }
        match flag.as_str() {
            "--seed" => options.seed = value()?.parse()?,
            "--fly_chunks" => options.fly_chunks = value()?.parse()?,
            "--speed" => options.speed = value()?.parse()?,
            "--sprint" => options.sprint = value()?,
            "--warmup_sprint" => options.warmup_sprint = Some(value()?),
            "--simulation_distance" => options.simulation_distance = value()?.parse()?,
            "--instance_dir" => options.instance_dir = Some(value()?.into()),
            "--tick_lockstep" => {
                options.tick_lockstep =
                    TickLockstep::from_name(&value()?).context("unknown lockstep mode")?
            }
            "--server" => {
                options.server =
                    Choice::from_name(&value()?).context("--server takes tuff, vanilla or both")?
            }
            "--out" => options.out = Some(value()?.into()),
            "--label" => options.label = value()?,
            _ => bail!("unknown flag {flag}"),
        }
    }
    options.watch.check_bots([BOT_NAME])?;
    Ok(options)
}

fn say(text: impl AsRef<str>) {
    println!("[bench] {}", text.as_ref());
}

fn secs(since: Instant) -> f64 {
    since.elapsed().as_secs_f64()
}

const SHUTDOWN_LIMIT: Duration = Duration::from_secs(60);

fn main() -> Result<()> {
    if !std::env::args().any(|arg| arg == "--bench") {
        println!("[bench] the village target only runs via `cargo bench --bench village`");
        return Ok(());
    }
    let options = options()?;
    if options.server.needs_vanilla() {
        options.vanilla.require_eula()?;
    }
    tuff_core::threads::init_rayon();
    let runtime = tuff::allocator::runtime()?;
    let result = match options.server {
        Choice::Both => servers::run_both("village", options.out.as_deref()),
        Choice::One(kind) => {
            let (chosen, out) = (options.instance_dir.clone(), options.out.clone());
            let result = runtime.block_on(run(options, kind));
            if let Err(error) = &result {
                servers::report_failure("village", kind, chosen.as_deref(), out.as_deref(), error);
            }
            result
        }
    };
    std::thread::Builder::new()
        .name("shutdown-guard".into())
        .spawn(|| {
            std::thread::sleep(SHUTDOWN_LIMIT);
            eprintln!("[bench] shutdown took longer than {SHUTDOWN_LIMIT:?}, aborting");
            std::process::abort();
        })?;
    runtime.shutdown_timeout(Duration::from_secs(10));
    result?;
    std::process::exit(0)
}

async fn run(options: Options, kind: Kind) -> Result<()> {
    let (instance, temporary) =
        servers::instance_dir("village", kind, options.instance_dir.as_deref())?;
    say(format!("{} with seed {}", kind.name(), options.seed));

    let mut server = BenchServer::start(Spec {
        kind,
        instance: instance.clone(),
        watch: options.watch.clone(),
        seed: options.seed,
        view_distance: VIEW_DISTANCE.into(),
        max_players: None,
        tick_lockstep: options.tick_lockstep,
        config: Vec::new(),
        simulation_distance: options.simulation_distance,
        vanilla: options.vanilla.clone(),
    })
    .await?;
    let console = server.console();
    say(options.watch.join_hint(server.bound()));

    let started = Instant::now();
    let (bot, samples) = servers::probe(
        &server,
        BotConfig {
            view_distance: VIEW_DISTANCE,
            ..BotConfig::new(BOT_NAME)
        },
    )
    .await?;
    say(format!("spawned after {:.1}s", secs(started)));
    console
        .run_for_player(&format!("gamemode spectator {BOT_NAME}"))
        .await?;

    let flight = Instant::now();
    let distance = f64::from(options.fly_chunks * 16);
    let mut fly = FlyLine::new([1.0, 0.0], CRUISE_Y, distance, options.speed);
    bot.pursue(&mut fly).await?;
    let flight_s = secs(flight);
    let flight_chunks = bot.with_state(|state| state.chunks);
    say(format!(
        "flew {distance:.0} blocks in {flight_s:.1}s, {flight_chunks} chunks received"
    ));

    let answer = console
        .run(&format!(
            "execute at {BOT_NAME} run locate structure #minecraft:village"
        ))
        .await?;
    say(&answer);
    let (x, z) = console::located(&answer).context("no village found")?;
    let teleports = bot.with_state(|state| state.teleports);
    let arrival = Instant::now();
    console
        .run(&format!("tp {BOT_NAME} {x} {CRUISE_Y} {z}"))
        .await?;
    bot.wait_for(|state| state.teleports > teleports).await?;
    bot.wait_loaded(VIEW_DISTANCE.into()).await?;
    let load_s = secs(arrival);
    let village_chunks = bot.with_state(|state| state.loaded.len());
    say(format!(
        "village view of {village_chunks} chunks loaded in {load_s:.1}s"
    ));

    if let Some(warmup) = &options.warmup_sprint {
        let warm = Instant::now();
        say(console.run(&format!("tick sprint {warmup}")).await?);
        loop {
            tokio::time::sleep(SPRINT_POLL).await;
            if !console::sprinting(&console.run("tick query").await?) {
                break;
            }
        }
        say(format!("warm-up sprint took {:.1}s wall clock", secs(warm)));
    }

    let source = server.tick_source(samples).await?;
    let ticks_source = source.describe();
    let (done, done_rx) = watch::channel(false);
    let tick_task = tokio::spawn(servers::collect_ticks(source, done_rx.clone()));
    let sample_task = tokio::spawn(servers::sample_processes(server.pid(), None, done_rx));

    let state_before = world_state(&console).await;
    let sprint = Instant::now();
    say(console
        .run(&format!("tick sprint {}", options.sprint))
        .await?);
    let tick_query = loop {
        tokio::time::sleep(SPRINT_POLL).await;
        let answer = console.run("tick query").await?;
        if !console::sprinting(&answer) {
            break console::tick_query(&answer);
        }
    };
    let sprint_end = Instant::now();
    let state_after = world_state(&console).await;
    let wall_s = sprint_end.duration_since(sprint).as_secs_f64();
    say(format!("sprint took {wall_s:.1}s wall clock"));
    if !options.watch.ops.is_empty() {
        let online = server.online(&options.watch.ops).await?;
        say(format!("watchers online at the end: {online:?}"));
    }
    let _ = done.send(true);
    let (ticks, lagged) = tick_task.await?;
    let samples = sample_task.await?;
    let world = server.world().await?;
    drop(bot);

    let in_sprint: Vec<f64> = ticks
        .iter()
        .filter(|(at, _)| *at >= sprint && *at <= sprint_end)
        .map(|(_, ms)| *ms)
        .collect();
    let process = servers::process_figures(&servers::window(&samples, 0, sprint, sprint_end));
    let mut report = json!({
        "bench": "village",
        "schema": 1,
        "label": options.label,
        "server": kind.name(),
        "machine": servers::machine(),
        "options": options.to_json(),
        "state": {"before": state_before, "after": state_after},
        "flight": {
            "blocks": distance,
            "seconds": flight_s,
            "chunks": flight_chunks,
        },
        "village": {
            "x": x,
            "z": z,
            "load_s": load_s,
            "chunks": village_chunks,
        },
        "sprint": {
            "wall_s": wall_s,
            "ticks_per_s": in_sprint.len() as f64 / wall_s.max(f64::EPSILON),
        },
        "ticks_source": ticks_source,
        "ticks": TickSummary::new(&in_sprint, wall_s, lagged),
        "tick_query": tick_query.map_or(Value::Null, console::TickQuery::to_json),
        "cpu": {
            "server_s": process["cpu_s"],
            "server_cores": process["cores"],
        },
        "memory": {
            "server_rss_start_mib": process["rss_start_mib"],
            "server_rss_end_mib": process["rss_end_mib"],
            "server_rss_mean_mib": process["rss_mean_mib"],
            "server_rss_peak_mib": process["rss_peak_mib"],
        },
        "world": world,
    });
    if kind == Kind::Vanilla {
        report["vanilla"] = options.vanilla.to_json();
    }
    let text = serde_json::to_string_pretty(&report)?;
    if let Some(out) = &options.out {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(out, format!("{text}\n"))?;
        say(format!("report written to {}", out.display()));
    }
    println!("{text}");

    server.shutdown().await?;
    if temporary {
        std::fs::remove_dir_all(&instance)?;
    }
    Ok(())
}

async fn world_state(console: &servers::Console) -> Value {
    let mut state = serde_json::Map::new();
    for (key, selector) in [
        ("entities", "@e"),
        ("villagers", "@e[type=villager]"),
        ("iron_golems", "@e[type=iron_golem]"),
        ("items", "@e[type=item]"),
    ] {
        let answer = console
            .run(&format!("execute if entity {selector}"))
            .await
            .unwrap_or_else(|error| format!("error: {error}"));
        state.insert(key.to_string(), json!(answer));
    }
    Value::Object(state)
}
