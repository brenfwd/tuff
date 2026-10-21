//! The crowd benchmark's scenarios, shared by `benches/crowd.rs` and the
//! ignored quick-timing test: a pit of pigs on a ladder, and cows packed
//! in a 3x3 pen with cramming off, while a player walks up with wheat.

use std::time::Instant;

use tuff_core::config::Value;
use tuff_core::world::game_rules::RuleValue;
use tuff_test_support::*;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, Item};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    LadderPit,
    CowPen,
}

#[derive(Debug, Clone, Copy)]
pub struct Report {
    pub mean_ms: f64,
    pub p99_ms: f64,
    /// Ticks per second the server would hold: 20, or less when the mean
    /// tick takes over 50 ms.
    pub tps: f64,
    pub wall_s: f64,
}

impl Scenario {
    pub fn name(self) -> &'static str {
        match self {
            Self::LadderPit => "ladder_pit",
            Self::CowPen => "cow_pen",
        }
    }

    pub fn default_count(self) -> usize {
        match self {
            Self::LadderPit => 800,
            Self::CowPen => 5000,
        }
    }
}

const CENTER: i32 = 24;

pub async fn run(scenario: Scenario, count: usize, crowd: bool, ticks: usize) -> Report {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    world
        .shared
        .config
        .set("tuff.mobs.crowd.enabled", Value::Bool(crowd))
        .unwrap();
    world
        .shared
        .config
        .set("tuff.mobs.crowd.threshold", Value::Int(500))
        .unwrap();
    if scenario == Scenario::CowPen {
        world.game_rules.send_modify(|rules| {
            rules
                .set("max_entity_cramming", RuleValue::Int(0))
                .expect("an integer rule");
        });
    }
    let chunk = ChunkPos::new(1, 1);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(1000);
    view_around(&world, &viewer, &mut events, chunk).await;
    let (low, high, kind) = match scenario {
        Scenario::LadderPit => (CENTER, CENTER, EntityType::Pig),
        Scenario::CowPen => (CENTER - 1, CENTER + 1, EntityType::Cow),
    };
    let mut blocks = pen(low, high);
    for x in low - 1..=high + 1 {
        for z in low - 1..=high + 1 {
            if x < low || x > high || z < low || z > high {
                blocks.push((BlockPos::new(x, SKY + 2, z), Block::Stone.default_state()));
            }
        }
    }
    if scenario == Scenario::LadderPit {
        blocks.push((
            BlockPos::new(CENTER, SKY + 1, CENTER),
            Block::Ladder.default_state(),
        ));
    }
    place_blocks(&world, &region, blocks).await;
    let spot = [CENTER as f64 + 0.5, SKY as f64 + 1.0, CENTER as f64 + 0.5];
    for _ in 0..count {
        summon(&region, kind, Compound::new(), spot).await;
    }
    world.ticks(20).await;
    let mut times = Vec::with_capacity(ticks);
    let started = Instant::now();
    for tick in 0..ticks {
        let walked = (tick as f64 / ticks as f64).min(1.0);
        let player = [spot[0] + 20.0 - 17.0 * walked, SKY as f64 + 1.0, spot[2]];
        region
            .request(player_update(&viewer, player, Some(Item::Wheat)))
            .await;
        while events.try_recv().is_ok() {}
        let at = Instant::now();
        world.ticks(1).await;
        times.push(at.elapsed().as_secs_f64() * 1000.0);
    }
    let wall = started.elapsed().as_secs_f64();
    world.shutdown().await;
    times.sort_by(f64::total_cmp);
    let mean_ms = times.iter().sum::<f64>() / times.len() as f64;
    let p99_ms = times[(times.len() * 99 / 100).min(times.len() - 1)];
    Report {
        mean_ms,
        p99_ms,
        tps: (1000.0 / mean_ms).min(20.0),
        wall_s: wall,
    }
}
