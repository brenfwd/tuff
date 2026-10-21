//! Fast break: finishing a block sooner than the game's mining time for
//! the block, the tool, its enchantments, effects, water and ground.
//! Early finishes add their shortfall (past a slack for jitter) to a
//! penalty bucket that drains with time, and late ones pay some back, so
//! a lag burst that bunches packets does not flag but a steady speed-up
//! does. The region refuses a finish below 70% of the time on its own;
//! this check is what makes repeat offenders climb the ladder.

use tuff_core::config::{ConfigStore, Key, Value};
use tuff_core::protocol::play::{PlayerActionStatus, Serverbound};
use tuff_core::world::player::{self, Miner};
use tuff_core::world::session::PlayerSession;
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;

use super::{Check, Facts, Fix, Flag};

pub const STRICT: &str = "tuff.anticheat.fastBreak.strict";
pub const OVERRIDES: &str = "tuff.anticheat.fastBreak.overrides";

/// Ticks a finish may come early for free (packet jitter).
const SLACK_TICKS: f64 = 2.0;
/// Ticks of penalty above which the check flags.
const BUCKET_LIMIT: f64 = 10.0;
/// Penalty drained per session tick.
const DRAIN: f64 = 0.05;
/// A late finish pays back at most this much penalty.
const MAX_CREDIT: f64 = 2.0;
/// The client waits this long after a mined block before mining the next.
const NEXT_DELAY_TICKS: u64 = 5;
const GRACE_TICKS: u64 = 100;
const MAX_OVERRIDES: usize = 256;

pub fn keys() -> Vec<Key> {
    vec![
        Key::new(
            STRICT,
            Value::Bool(false),
            "Expect the mining time of the ground and water state at the start of each block instead of the most lenient one",
        ),
        Key::new(
            OVERRIDES,
            Value::String(String::new()),
            "Per-block multipliers on the expected mining time, as block=factor pairs separated by commas (minecraft:stone=0.5); 0 exempts the block",
        )
        .validated(|value| match value {
            Value::String(text) => parse_overrides(text).map(|_| ()),
            _ => Err("must be text".to_owned()),
        }),
    ]
}

fn parse_overrides(text: &str) -> Result<Vec<(Block, f64)>, String> {
    let mut out = Vec::new();
    for pair in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, factor) = pair
            .split_once('=')
            .ok_or_else(|| format!("'{pair}' is not block=factor"))?;
        let name = name.trim();
        let block = Block::from_name(name)
            .or_else(|| Block::from_name(name.trim_start_matches("minecraft:")))
            .ok_or_else(|| format!("unknown block '{name}'"))?;
        let factor = factor
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && *f >= 0.0)
            .ok_or_else(|| format!("'{factor}' is not a factor of 0 or more"))?;
        if out.len() >= MAX_OVERRIDES {
            return Err(format!("at most {MAX_OVERRIDES} overrides"));
        }
        out.push((block, factor));
    }
    Ok(out)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FastBreakSettings {
    pub strict: bool,
    pub overrides: Vec<(Block, f64)>,
}

impl FastBreakSettings {
    pub fn read(store: &ConfigStore) -> Self {
        Self {
            strict: store.get_bool(STRICT).unwrap_or(false),
            overrides: store
                .get_string(OVERRIDES)
                .and_then(|text| parse_overrides(&text).ok())
                .unwrap_or_default(),
        }
    }

    fn factor(&self, block: Block) -> f64 {
        self.overrides
            .iter()
            .find(|(b, _)| *b == block)
            .map_or(1.0, |(_, f)| *f)
    }
}

/// How fast the player mines the block a start or stop names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mining {
    /// Progress per tick in the most lenient state (on the ground, eyes
    /// out of water unless that is slower).
    pub lenient: f32,
    /// Progress per tick in the player's state right now.
    pub actual: f32,
    /// Multiplier on the expected time; 0 exempts the block.
    pub factor: f64,
    pub strict: bool,
}

impl Mining {
    pub fn probe(
        session: &PlayerSession,
        packet: &Serverbound,
        settings: &FastBreakSettings,
    ) -> Option<Mining> {
        let Serverbound::PlayerAction {
            status, position, ..
        } = packet
        else {
            return None;
        };
        if !matches!(
            status,
            PlayerActionStatus::StartDestroyBlock | PlayerActionStatus::StopDestroyBlock
        ) {
            return None;
        }
        let world = session.client_world();
        let state: BlockState = world.block_at(*position)?;
        let hardness = state.destroy_speed();
        let tool = session.inventory.hand(false);
        let miner = session.miner();
        let wet = player::eye_in_water(session.eye(), |pos| world.block_at(pos));
        let rate = |miner: &Miner, wet: bool| {
            player::destroy_progress_at(state, hardness, tool, miner, wet)
        };
        let grounded = Miner {
            on_ground: true,
            ..miner
        };
        Some(Mining {
            lenient: rate(&grounded, false).max(rate(&grounded, true)),
            actual: rate(&miner, wet),
            factor: settings.factor(state.block()),
            strict: settings.strict,
        })
    }

    fn rate(&self) -> f32 {
        if self.strict {
            self.actual
        } else {
            self.lenient
        }
    }
}

fn undo(position: BlockPos, sequence: i32, weight: f64) -> Option<Flag> {
    Some(Flag {
        check: "fast_break",
        weight,
        fix: Fix::Correct(Box::new(Serverbound::PlayerAction {
            status: PlayerActionStatus::AbortDestroyBlock,
            position,
            face: 0,
            sequence,
        })),
    })
}

#[derive(Default)]
pub struct FastBreak {
    first: Option<u64>,
    started: Option<(BlockPos, u64, f32)>,
    finished: Option<u64>,
    bucket: f64,
    at: u64,
}

impl FastBreak {
    fn drain(&mut self, tick: u64) {
        self.bucket = (self.bucket - tick.saturating_sub(self.at) as f64 * DRAIN).max(0.0);
        self.at = self.at.max(tick);
    }

    fn penalise(&mut self, facts: &Facts, ticks: f64) -> bool {
        self.bucket += ticks;
        let grace = facts.tick.saturating_sub(self.first.unwrap_or(facts.tick)) < GRACE_TICKS;
        if self.bucket > BUCKET_LIMIT && !grace {
            self.bucket /= 2.0;
            return true;
        }
        false
    }

    fn start(
        &mut self,
        facts: &Facts,
        mining: Mining,
        position: BlockPos,
        seq: i32,
    ) -> Option<Flag> {
        let rate = mining.rate();
        if rate >= 1.0 || rate <= 0.0 || mining.factor <= 0.0 {
            self.started = None;
            return None;
        }
        self.started = Some((position, facts.tick, rate));
        let gap = self.finished.take().map(|at| facts.tick.saturating_sub(at));
        match gap {
            Some(gap) if gap < NEXT_DELAY_TICKS => {
                let early = (NEXT_DELAY_TICKS - gap) as f64 / 2.0;
                self.penalise(facts, early)
                    .then(|| undo(position, seq, 1.0))?
            }
            _ => None,
        }
    }

    fn stop(
        &mut self,
        facts: &Facts,
        mining: Mining,
        position: BlockPos,
        seq: i32,
    ) -> Option<Flag> {
        let (start, at, rate) = self.started.take()?;
        if start != position || mining.factor <= 0.0 {
            return None;
        }
        let rate = rate.max(mining.rate());
        if rate >= 1.0 {
            return None;
        }
        self.finished = Some(facts.tick);
        let needed = ((1.0 / f64::from(rate)).ceil() - 1.0) * mining.factor;
        let took = facts.tick.saturating_sub(at) as f64;
        let early = needed - took - SLACK_TICKS;
        if early <= 0.0 {
            self.bucket = (self.bucket - (took - needed).clamp(0.0, MAX_CREDIT)).max(0.0);
            return None;
        }
        if self.penalise(facts, early) {
            return undo(position, seq, (early / 5.0).clamp(1.0, 4.0));
        }
        None
    }
}

impl Check for FastBreak {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        self.first.get_or_insert(facts.tick);
        let Serverbound::PlayerAction {
            status,
            position,
            sequence,
            ..
        } = packet
        else {
            return None;
        };
        self.drain(facts.tick);
        if facts.instabuild {
            self.started = None;
            return None;
        }
        match status {
            PlayerActionStatus::StartDestroyBlock => {
                self.start(facts, facts.mining?, *position, *sequence)
            }
            PlayerActionStatus::StopDestroyBlock => {
                let mining = facts.mining?;
                self.stop(facts, mining, *position, *sequence)
            }
            PlayerActionStatus::AbortDestroyBlock => {
                self.started = None;
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;

    fn action(status: PlayerActionStatus) -> Serverbound {
        Serverbound::PlayerAction {
            status,
            position: BlockPos::new(0, 63, 0),
            face: 1,
            sequence: 3,
        }
    }

    /// Stone by hand-ish: 0.1 progress a tick, 9 ticks needed.
    fn stone() -> Facts {
        let mut f = facts();
        f.mining = Some(Mining {
            lenient: 0.1,
            actual: 0.1,
            factor: 1.0,
            strict: false,
        });
        f
    }

    fn mine(check: &mut FastBreak, f: &mut Facts, took: u64) -> Option<Flag> {
        let start = check.examine(f, &action(PlayerActionStatus::StartDestroyBlock));
        f.tick += took;
        let stop = check.examine(f, &action(PlayerActionStatus::StopDestroyBlock));
        f.tick += NEXT_DELAY_TICKS;
        start.or(stop)
    }

    fn warmed() -> FastBreak {
        FastBreak {
            first: Some(0),
            ..FastBreak::default()
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn honest_mining_and_lag_bursts_never_flag() {
        let mut check = warmed();
        let mut f = stone();
        f.tick = 1000;
        for n in 0..200 {
            let took = match n % 3 {
                0 => 7,
                1 => 11,
                _ => 9,
            };
            assert_eq!(mine(&mut check, &mut f, took), None, "break {n}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn steady_early_finishes_flag_after_the_bucket_fills() {
        let mut check = warmed();
        let mut f = stone();
        f.tick = 1000;
        let flagged = (0..10)
            .filter_map(|_| mine(&mut check, &mut f, 3))
            .collect::<Vec<_>>();
        assert!(!flagged.is_empty());
        assert!(flagged.iter().all(|flag| flag.check == "fast_break"));
        assert!(matches!(flagged[0].fix, Fix::Correct(_)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn grace_creative_instant_and_exempt_blocks_pass() {
        let mut check = FastBreak::default();
        let mut f = stone();
        for _ in 0..4 {
            assert_eq!(mine(&mut check, &mut f, 1), None, "session grace");
        }
        let mut f = stone();
        f.tick = 1000;
        f.instabuild = true;
        let mut check = FastBreak::default();
        assert!((0..20).all(|_| mine(&mut check, &mut f, 0).is_none()));
        let mut f = stone();
        f.tick = 1000;
        f.mining = Some(Mining {
            lenient: 1.5,
            actual: 1.5,
            factor: 1.0,
            strict: false,
        });
        assert!((0..20).all(|_| mine(&mut check, &mut f, 0).is_none()));
        let mut f = stone();
        f.tick = 1000;
        if let Some(m) = &mut f.mining {
            m.factor = 0.0;
        }
        assert!((0..20).all(|_| mine(&mut check, &mut f, 0).is_none()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn strict_mode_uses_the_state_at_the_start() {
        let mut f = stone();
        f.tick = 1000;
        f.mining = Some(Mining {
            lenient: 0.1,
            actual: 0.02,
            factor: 1.0,
            strict: true,
        });
        let mut check = warmed();
        assert!((0..5).any(|_| mine(&mut check, &mut f, 12).is_some()));
        if let Some(m) = &mut f.mining {
            m.strict = false;
        }
        let mut check = warmed();
        assert!((0..20).all(|_| mine(&mut check, &mut f, 12).is_none()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn overrides_parse_and_reject_garbage() {
        let parsed = parse_overrides("minecraft:stone=0.5, dirt=0").unwrap();
        assert_eq!(parsed, vec![(Block::Stone, 0.5), (Block::Dirt, 0.0)]);
        assert!(parse_overrides("stone").is_err());
        assert!(parse_overrides("nonsense=1").is_err());
        assert!(parse_overrides("stone=-1").is_err());
        assert_eq!(parse_overrides("").unwrap(), vec![]);
    }
}
