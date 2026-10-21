//! The bot side of the stress bench, run as a child process so the
//! server process's CPU and memory are the server's alone. It speaks JSON
//! lines: a setup line arrives on stdin, `ready` goes out once every bot
//! is in, and each later command gets one reply line.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tuff_bot::mine::Miner;
use tuff_bot::packets::Kinds;
use tuff_bot::report::BotTotals;
use tuff_bot::sprint::SprintJump;
use tuff_bot::walk::Wander;
use tuff_bot::{Bot, BotConfig, FlyLine, Goal, Idle, Rng};

pub const CRUISE_Y: f64 = 160.0;
const CONNECT_BATCH: usize = 8;
const MINER_HALF_WIDTH: i32 = 2;
const MINER_FLOOR_MARGIN: i32 = 6;
const SPOT_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Flyer,
    Miner,
    Walker,
    Idler,
    Sprinter,
    Anchor,
}

impl Role {
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Flyer => "Fly",
            Self::Miner => "Mine",
            Self::Walker => "Walk",
            Self::Idler => "Idle",
            Self::Sprinter => "Sprint",
            Self::Anchor => "Anchor",
        }
    }

    fn from_prefix(name: &str) -> Option<Self> {
        [
            Self::Anchor,
            Self::Flyer,
            Self::Miner,
            Self::Walker,
            Self::Idler,
            Self::Sprinter,
        ]
        .into_iter()
        .find(|role| name.starts_with(role.prefix()))
    }
}

/// Every bot name, in the order the fleet connects them.
pub fn names(
    flyers: u32,
    miners: u32,
    walkers: u32,
    idlers: u32,
    sprinters: u32,
    anchor: bool,
) -> Vec<String> {
    let mut names = Vec::new();
    if anchor {
        names.push(Role::Anchor.prefix().to_string());
    }
    for (role, count) in [
        (Role::Flyer, flyers),
        (Role::Miner, miners),
        (Role::Walker, walkers),
        (Role::Idler, idlers),
        (Role::Sprinter, sprinters),
    ] {
        names.extend((0..count).map(|i| format!("{}{i}", role.prefix())));
    }
    names
}

struct Setup {
    addr: SocketAddr,
    names: Vec<String>,
    view_distance: i8,
    speed: f64,
    mine_rate: u32,
    home_radius: f64,
    seed: u64,
}

fn setup(line: &str) -> Result<Setup> {
    let v: Value = serde_json::from_str(line)?;
    let field = |name: &str| v.get(name).with_context(|| format!("setup lacks {name}"));
    Ok(Setup {
        addr: field("addr")?.as_str().context("addr")?.parse()?,
        names: field("names")?
            .as_array()
            .context("names")?
            .iter()
            .filter_map(|n| n.as_str().map(String::from))
            .collect(),
        view_distance: field("view_distance")?.as_i64().context("view_distance")? as i8,
        speed: field("speed")?.as_f64().context("speed")?,
        mine_rate: field("mine_rate")?.as_u64().context("mine_rate")? as u32,
        home_radius: field("home_radius")?.as_f64().context("home_radius")?,
        seed: field("seed")?.as_i64().context("seed")? as u64,
    })
}

async fn say(out: &mut tokio::io::Stdout, value: Value) -> Result<()> {
    out.write_all(format!("{value}\n").as_bytes()).await?;
    out.flush().await?;
    Ok(())
}

fn totals(bots: &[Arc<Bot>]) -> Value {
    let mut totals = BotTotals::default();
    let mut kinds = Kinds::default();
    for bot in bots {
        bot.with_state(|state| {
            totals.add(state);
            kinds.add(&state.tally);
        });
    }
    json!({"totals": totals, "kinds": kinds})
}

pub async fn run() -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    let first = lines.next_line().await?.context("no setup line")?;
    let setup = setup(&first)?;
    let started = Instant::now();
    let mut bots: Vec<Arc<Bot>> = Vec::new();
    for batch in setup.names.chunks(CONNECT_BATCH) {
        let mut joins = JoinSet::new();
        for (offset, name) in batch.iter().enumerate() {
            let role = Role::from_prefix(name).context("unknown bot role")?;
            let config = BotConfig {
                view_distance: setup.view_distance,
                terrain: role != Role::Flyer,
                follow_ticks: role == Role::Flyer,
                ..BotConfig::new(name.clone())
            };
            let addr = setup.addr;
            joins.spawn(async move { (offset, Bot::connect(addr, config).await) });
        }
        let mut connected: Vec<Option<Bot>> = (0..batch.len()).map(|_| None).collect();
        while let Some(joined) = joins.join_next().await {
            let (offset, bot) = joined?;
            connected[offset] = Some(bot.with_context(|| format!("{} failed", batch[offset]))?);
        }
        bots.extend(connected.into_iter().flatten().map(Arc::new));
    }
    eprintln!(
        "[fleet] {} bots in after {:.1}s",
        bots.len(),
        started.elapsed().as_secs_f64()
    );
    let spawn = bots.first().map(|bot| bot.with_state(|s| s.position));
    say(&mut out, json!({"ready": {"spawn": spawn}})).await?;

    let (stop, stopped) = watch::channel(false);
    let mut running = JoinSet::new();
    while let Some(line) = lines.next_line().await? {
        let command: Value = serde_json::from_str(&line)?;
        match command["cmd"].as_str() {
            Some("spots") => {
                let count = command["count"].as_u64().unwrap_or(0) as usize;
                let radius = command["radius"].as_i64().unwrap_or(16) as i32;
                let anchor = bots.first().context("no bots")?;
                let spots = spots(anchor, count, radius, setup.seed).await;
                say(&mut out, json!({"spots": spots})).await?;
            }
            Some("go") => {
                let home = spawn.map_or([0.0, 0.0], |p| [p[0], p[2]]);
                let flyers = bots
                    .iter()
                    .filter(|b| Role::from_prefix(b.name()) == Some(Role::Flyer))
                    .count()
                    .max(1);
                let mut flyer = 0;
                for (index, bot) in bots.iter().enumerate() {
                    let role = Role::from_prefix(bot.name()).context("role")?;
                    let goal: Box<dyn Goal> = match role {
                        Role::Flyer => {
                            let angle = std::f64::consts::TAU * flyer as f64 / flyers as f64;
                            flyer += 1;
                            Box::new(FlyLine::new(
                                [angle.sin(), angle.cos()],
                                CRUISE_Y,
                                f64::INFINITY,
                                setup.speed,
                            ))
                        }
                        Role::Miner => Box::new(Miner::new(
                            setup.mine_rate,
                            MINER_HALF_WIDTH,
                            -64 + MINER_FLOOR_MARGIN,
                        )),
                        Role::Walker => Box::new(Wander::new(
                            home,
                            setup.home_radius,
                            setup.seed.wrapping_add(index as u64),
                        )),
                        Role::Sprinter => Box::new(SprintJump::new(
                            home,
                            setup.home_radius,
                            setup.seed.wrapping_add(index as u64),
                        )),
                        Role::Anchor | Role::Idler => Box::new(Idle { ticks: u64::MAX }),
                    };
                    running.spawn(pursue(bot.clone(), goal, stopped.clone()));
                }
                say(&mut out, json!({"ok": true})).await?;
            }
            Some("mark") => {
                say(&mut out, totals(&bots)).await?;
            }
            Some("stop") => {
                let _ = stop.send(true);
                while running.join_next().await.is_some() {}
                say(&mut out, totals(&bots)).await?;
                return Ok(());
            }
            other => bail!("unknown fleet command {other:?}"),
        }
    }
    Ok(())
}

async fn pursue(bot: Arc<Bot>, mut goal: Box<dyn Goal>, mut stopped: watch::Receiver<bool>) {
    tokio::select! {
        result = bot.pursue(goal.as_mut()) => {
            if let Err(error) = result {
                eprintln!("[fleet] {error:#}");
            }
        }
        _ = stopped.wait_for(|stop| *stop) => {}
    }
}

/// `count` places on flat dry ground within `radius` of the anchor, for
/// mobs to be summoned at; chosen with repeats once every candidate is
/// taken.
async fn spots(anchor: &Bot, count: usize, radius: i32, seed: u64) -> Vec<[f64; 3]> {
    let deadline = Instant::now() + SPOT_WAIT;
    let candidates = loop {
        let candidates = anchor.with_state(|state| {
            let [x, _, z] = state.position.map(|v| v.floor() as i32);
            let Some(terrain) = &state.terrain else {
                return Vec::new();
            };
            let mut found = Vec::new();
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    if let Some(column) = terrain.column(x + dx, z + dz)
                        && column.is_flat_ground()
                    {
                        found.push([
                            f64::from(x + dx) + 0.5,
                            f64::from(column.floor),
                            f64::from(z + dz) + 0.5,
                        ]);
                    }
                }
            }
            found
        });
        let area = ((2 * radius + 1) * (2 * radius + 1)) as usize;
        if candidates.len() * 2 >= area || Instant::now() > deadline {
            break candidates;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    if candidates.is_empty() {
        return Vec::new();
    }
    let mut rng = Rng::new(seed);
    let mut used = HashSet::new();
    (0..count)
        .map(|_| {
            let mut index = rng.next_u64() as usize % candidates.len();
            if used.len() < candidates.len() {
                while !used.insert(index) {
                    index = (index + 1) % candidates.len();
                }
            }
            candidates[index]
        })
        .collect()
}
