use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tuff::world::tick_barrier::TickLockstep;

use crate::servers::{Choice, Kind};
use crate::vanilla::VanillaOptions;
use crate::watchers::Watch;

pub const DEFAULT_ENTITY_TYPES: &str = "cow,pig,sheep,chicken,husk,spider,villager,rabbit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Synthetic,
    Normal,
    Mixed,
}

impl Preset {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "synthetic" => Some(Self::Synthetic),
            "normal" => Some(Self::Normal),
            "mixed" => Some(Self::Mixed),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Synthetic => "synthetic",
            Self::Normal => "normal",
            Self::Mixed => "mixed",
        }
    }

    /// Flyers, miners, walkers and entities for `players` players.
    fn mix(self, players: u32) -> [u32; 4] {
        match self {
            Self::Synthetic => [players, (players / 2).max(1), 0, players * 100],
            Self::Normal => [0, 0, players, players * 4],
            Self::Mixed => {
                let flyers = players / 4;
                let miners = players / 4;
                [flyers, miners, players - flyers - miners, players * 25]
            }
        }
    }
}

pub enum Mode {
    Run(Box<Options>),
    Fleet,
    Compare(PathBuf, PathBuf),
}

#[derive(Debug, Clone)]
pub struct Options {
    pub preset: Preset,
    pub players: u32,
    pub flyers: u32,
    pub miners: u32,
    pub walkers: u32,
    /// Survival players standing still at spawn.
    pub idlers: u32,
    /// Survival players sprint-jumping around spawn.
    pub sprinters: u32,
    pub entities: u32,
    pub entity_types: Vec<String>,
    pub duration: Duration,
    pub warmup: Duration,
    pub seed: i64,
    pub view_distance: i8,
    pub simulation_distance: i8,
    pub speed: f64,
    pub mine_rate: u32,
    pub home_radius: f64,
    pub tick_lockstep: TickLockstep,
    pub config: Vec<(String, String)>,
    pub label: String,
    pub out: Option<PathBuf>,
    pub instance_dir: Option<PathBuf>,
    pub watch: Watch,
    pub server: Choice,
    pub vanilla: VanillaOptions,
    pub timings: bool,
    pub linger: Duration,
    /// Sample about one allocation per this many bytes for heap sites (0
    /// is off).
    pub heap_sample: usize,
}

impl Options {
    pub fn bots(&self) -> u32 {
        self.flyers
            + self.miners
            + self.walkers
            + self.idlers
            + self.sprinters
            + u32::from(self.entities > 0)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "preset": self.preset.name(),
            "players": self.players,
            "flyers": self.flyers,
            "miners": self.miners,
            "walkers": self.walkers,
            "idlers": self.idlers,
            "sprinters": self.sprinters,
            "entities": self.entities,
            "entity_types": self.entity_types,
            "duration_s": self.duration.as_secs_f64(),
            "warmup_s": self.warmup.as_secs_f64(),
            "seed": self.seed,
            "view_distance": self.view_distance,
            "simulation_distance": self.simulation_distance,
            "speed": self.speed,
            "mine_rate": self.mine_rate,
            "home_radius": self.home_radius,
            "tick_lockstep": self.tick_lockstep.name(),
            "timings": self.timings,
            "linger_s": self.linger.as_secs_f64(),
            "heap_sample": self.heap_sample,
            "config": self.config.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect::<serde_json::Map<_, _>>(),
        })
    }
}

/// `90`, `90s`, `500ms`, `2m` or `1h`.
pub fn duration(text: &str) -> Result<Duration> {
    let split = text
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let value: f64 = number
        .parse()
        .with_context(|| format!("bad duration {text}"))?;
    let seconds = match unit {
        "" | "s" => value,
        "ms" => value / 1000.0,
        "m" => value * 60.0,
        "h" => value * 3600.0,
        _ => bail!("bad duration unit in {text}"),
    };
    Ok(Duration::from_secs_f64(seconds))
}

pub fn parse() -> Result<Mode> {
    parse_from(std::env::args().skip(1))
}

pub fn parse_from(args: impl IntoIterator<Item = String>) -> Result<Mode> {
    let mut args = args.into_iter().filter(|arg| arg != "--bench");
    let mut preset = Preset::Synthetic;
    let mut players = 8;
    let mut overrides: [Option<u32>; 4] = [None; 4];
    let mut options = Options {
        preset,
        players,
        flyers: 0,
        miners: 0,
        walkers: 0,
        idlers: 0,
        sprinters: 0,
        entities: 0,
        entity_types: DEFAULT_ENTITY_TYPES.split(',').map(String::from).collect(),
        duration: Duration::from_secs(60),
        warmup: Duration::from_secs(10),
        seed: 1,
        view_distance: 10,
        simulation_distance: 0,
        speed: 2.0,
        mine_rate: 1,
        home_radius: 40.0,
        tick_lockstep: TickLockstep::Always,
        config: Vec::new(),
        label: String::new(),
        out: None,
        instance_dir: None,
        watch: Watch::default(),
        server: Choice::One(Kind::Tuff),
        vanilla: VanillaOptions::default(),
        timings: false,
        linger: Duration::ZERO,
        heap_sample: 0,
    };
    while let Some(flag) = args.next() {
        if flag == "--fleet" {
            return Ok(Mode::Fleet);
        }
        let mut value = || args.next().with_context(|| format!("{flag} needs a value"));
        if options.watch.flag(&flag, &mut value)? || options.vanilla.flag(&flag, &mut value)? {
            continue;
        }
        match flag.as_str() {
            "--compare" => {
                let before = value()?.into();
                let after = value()?.into();
                return Ok(Mode::Compare(before, after));
            }
            "--preset" => {
                preset = Preset::from_name(&value()?).context("unknown preset")?;
            }
            "--players" => players = value()?.parse()?,
            "--flyers" => overrides[0] = Some(value()?.parse()?),
            "--miners" => overrides[1] = Some(value()?.parse()?),
            "--walkers" => overrides[2] = Some(value()?.parse()?),
            "--entities" => overrides[3] = Some(value()?.parse()?),
            "--idlers" => options.idlers = value()?.parse()?,
            "--sprinters" => options.sprinters = value()?.parse()?,
            "--entity_types" => {
                options.entity_types = value()?.split(',').map(String::from).collect()
            }
            "--duration" => options.duration = duration(&value()?)?,
            "--warmup" => options.warmup = duration(&value()?)?,
            "--linger" => options.linger = duration(&value()?)?,
            "--heap_sample" => options.heap_sample = value()?.parse()?,
            "--seed" => options.seed = value()?.parse()?,
            "--view_distance" => options.view_distance = value()?.parse()?,
            "--simulation_distance" => options.simulation_distance = value()?.parse()?,
            "--speed" => options.speed = value()?.parse()?,
            "--mine_rate" => options.mine_rate = value()?.parse()?,
            "--home_radius" => options.home_radius = value()?.parse()?,
            "--tick_lockstep" => {
                options.tick_lockstep =
                    TickLockstep::from_name(&value()?).context("unknown lockstep mode")?
            }
            "--config" => {
                let pair = value()?;
                let (key, val) = pair.split_once('=').context("--config takes key=value")?;
                options.config.push((key.into(), val.into()));
            }
            "--server" => {
                options.server =
                    Choice::from_name(&value()?).context("--server takes tuff, vanilla or both")?
            }
            "--timings" => options.timings = true,
            "--label" => options.label = value()?,
            "--out" => options.out = Some(value()?.into()),
            "--instance_dir" => options.instance_dir = Some(value()?.into()),
            _ => bail!("unknown flag {flag}"),
        }
    }
    let mix = preset.mix(players);
    let [flyers, miners, walkers, entities] =
        std::array::from_fn(|i| overrides[i].unwrap_or(mix[i]));
    if options.simulation_distance == 0 {
        options.simulation_distance = options.view_distance;
    }
    options.preset = preset;
    options.players = players;
    options.flyers = flyers;
    options.miners = miners;
    options.walkers = walkers;
    options.entities = entities;
    Ok(Mode::Run(Box::new(options)))
}
