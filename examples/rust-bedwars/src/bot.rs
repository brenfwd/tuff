// SPDX-License-Identifier: MIT
//! Bots: practice participants that are real (fake) players, spawned and
//! moved by the optional bots plugin through `tuff-bots:api`. Bedwars
//! keeps only the strategy here: a mob-style goal selector fed what the
//! bot perceives each tick, whose [`Intent`] the game turns into bots-api
//! processes. Everything here is host-free and deterministic.

pub mod goals;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use tuff_bots_api::control;

use crate::game::Id;
use crate::map::Pos;

use goals::GoalSelector;

/// Entity reach for melee, as for a survival player.
pub const REACH: f64 = 3.0;
/// Block interaction range of a survival player.
pub const BLOCK_REACH: f64 = 4.5;
pub const EYE: f64 = 1.62;
/// What every bot's name starts with.
pub const PREFIX: &str = "BOT_";

const NAMES: [&str; 16] = [
    "Ashwin", "Brakken", "Corvo", "Dagny", "Elsbeth", "Fenwick", "Grisha", "Halvard", "Ingrid",
    "Jorvik", "Kestrel", "Lorcan", "Maelis", "Nyssa", "Oswin", "Perrin",
];

/// A bot's name: one of a fixed list, numbered once the list runs out.
pub fn name(n: u64) -> String {
    let base = NAMES[(n % NAMES.len() as u64) as usize];
    match n / NAMES.len() as u64 {
        0 => format!("{PREFIX}{base}"),
        k => format!("{PREFIX}{base}{k}"),
    }
}

/// The skin model a bot wears.
pub fn model(n: u64) -> control::Model {
    if n.is_multiple_of(2) {
        control::Model::Classic
    } else {
        control::Model::Slim
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Difficulty {
    Easy,
    Normal,
    Hard,
}

impl Difficulty {
    pub const ALL: [Difficulty; 3] = [Self::Easy, Self::Normal, Self::Hard];

    pub fn name(self) -> &'static str {
        match self {
            Self::Easy => "easy",
            Self::Normal => "normal",
            Self::Hard => "hard",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.name() == text)
    }

    pub fn skill(self) -> Skill {
        match self {
            Self::Easy => Skill {
                reaction: 12,
                aim_error: 14.0,
                sprint: false,
                aggro: 10.0,
                rush_after: 90 * 20,
            },
            Self::Normal => Skill {
                reaction: 7,
                aim_error: 7.0,
                sprint: true,
                aggro: 14.0,
                rush_after: 45 * 20,
            },
            Self::Hard => Skill {
                reaction: 4,
                aim_error: 2.5,
                sprint: true,
                aggro: 18.0,
                rush_after: 15 * 20,
            },
        }
    }
}

/// How well a bot plays. No skill grants more than a player can do: the
/// knobs only slow it down or make it miss.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Skill {
    /// Ticks before the bot notices where an enemy is.
    pub reaction: u64,
    /// The largest aim error, in degrees, of each look.
    pub aim_error: f32,
    pub sprint: bool,
    /// How far away an enemy draws the bot into a fight.
    pub aggro: f64,
    /// Ticks into the match before the bot goes for enemy beds.
    pub rush_after: u64,
}

impl Skill {
    /// The same skill as the bots plugin takes it.
    pub fn api(self) -> control::Skill {
        control::Skill {
            reaction_ticks: self.reaction as u32,
            aim_error_degrees: self.aim_error,
            turn_rate: if self.sprint { 45.0 } else { 25.0 },
            click_cps_cap: if self.sprint { 10.0 } else { 6.0 },
            attack_charge: 0.9,
        }
    }
}

/// An enemy as a bot perceives it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seen {
    pub id: Id,
    pub pos: [f64; 3],
}

/// Everything a bot's goals may look at in one tick.
/// A bot's job within its team.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Role {
    /// Alone, or no particular job: rushes on its own schedule.
    #[default]
    Solo,
    /// Stays near the team bed and never goes for enemy beds.
    Defender,
    /// Goes for enemy beds early.
    Rusher,
    /// Sticks with a teammate once the bed is safe.
    Follower,
}

impl Role {
    /// Roles for the bots of one team, in order. The first bot of a
    /// group defends, the second rushes, any further ones follow.
    pub fn assign(position: usize, team_bots: usize) -> Role {
        match (team_bots, position) {
            (0 | 1, _) => Role::Solo,
            (_, 0) => Role::Defender,
            (_, 1) => Role::Rusher,
            _ => Role::Follower,
        }
    }
}

pub struct View<'a> {
    pub role: Role,
    /// A teammate to stay near, if one is alive.
    pub ally: Option<[f64; 3]>,
    pub now: u64,
    /// Ticks since the match started.
    pub elapsed: u64,
    pub me: [f64; 3],
    pub home: [f64; 3],
    /// Where the team's resources spawn.
    pub forge: [f64; 3],
    /// The bot is saving up for gear it cannot pay for yet.
    pub saving: bool,
    /// The bot's own bed (foot half), while it stands.
    pub bed: Option<Pos>,
    /// Enemy beds still standing (foot halves).
    pub enemy_beds: &'a [Pos],
    pub enemies: &'a [Seen],
    pub skill: Skill,
    pub bow: bool,
    pub arrows: u32,
    pub fireballs: u32,
}

impl View<'_> {
    pub fn nearest_enemy(&self, within: f64) -> Option<Seen> {
        self.enemies
            .iter()
            .filter(|e| dist(e.pos, self.me) <= within)
            .min_by(|a, b| dist(a.pos, self.me).total_cmp(&dist(b.pos, self.me)))
            .copied()
    }
}

/// What a bot wants to do this tick.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Intent {
    /// Where to walk, by path.
    pub goto: Option<[f64; 3]>,
    /// An enemy bed to get to by the bots plugin's block goal.
    pub rush: Option<Pos>,
    /// A point to look at.
    pub look: Option<[f64; 3]>,
    /// The enemy to chase and fight by an attack process.
    pub fight: Option<Id>,
    pub dig: Option<Pos>,
    pub sprint: bool,
    /// Release the drawn bow or throw a fire charge along the look.
    pub shoot: Option<Shot>,
    /// The enemy and weapon of a ranged attack is aimed at.
    pub aim: Option<(Id, Shot)>,
}

/// The ranged gear a bot is known to carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stock {
    pub bow: bool,
    pub arrows: u32,
    pub fireballs: u32,
    pub eggs: u32,
}

impl Stock {
    /// The stock a bot really carries, from its slot snapshots.
    pub fn from_carried(carried: &[(u8, String)]) -> Self {
        let mut stock = Self::default();
        for (_, snbt) in carried {
            if let Some(id) = crate::shop::snbt_id(snbt) {
                stock.bought(id, crate::shop::snbt_count(snbt));
            }
        }
        stock
    }

    /// Counts a purchase of `count` of `item`.
    pub fn bought(&mut self, item: &str, count: u32) {
        match item {
            "minecraft:bow" => self.bow = true,
            "minecraft:arrow" => self.arrows += count,
            "minecraft:fire_charge" => self.fireballs += count,
            "minecraft:egg" => self.eggs += count,
            _ => {}
        }
    }

    /// Counts a shot spent.
    pub fn spent(&mut self, shot: Shot) {
        match shot {
            Shot::Arrow => self.arrows = self.arrows.saturating_sub(1),
            Shot::Fireball => self.fireballs = self.fireballs.saturating_sub(1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shot {
    Arrow,
    Fireball,
}

pub fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

pub fn block_center(at: Pos) -> [f64; 3] {
    [
        f64::from(at[0]) + 0.5,
        f64::from(at[1]) + 0.5,
        f64::from(at[2]) + 0.5,
    ]
}

pub fn block_of(at: [f64; 3]) -> Pos {
    [
        at[0].floor() as i32,
        at[1].floor() as i32,
        at[2].floor() as i32,
    ]
}

/// Where a bot was last sent to walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Heading {
    /// Close to a block.
    Near(Pos),
    /// Next to or onto a block, bridging and digging as needed.
    Block(Pos),
}

impl Heading {
    pub fn pos(self) -> Pos {
        match self {
            Heading::Near(at) | Heading::Block(at) => at,
        }
    }
}

/// One bot: who it is, how it plays and its brain. It is a player once
/// the bots plugin spawned it and the game found its uuid.
#[derive(Debug)]
pub struct Bot {
    pub number: u64,
    pub name: String,
    pub difficulty: Difficulty,
    /// The arena the bot was added to.
    pub arena: usize,
    /// The bots plugin's id for it, once spawned.
    pub handle: Option<control::BotId>,
    /// Its player uuid, once it is online.
    pub id: Option<Id>,
    brain: GoalSelector,
    /// Recent positions of each enemy, oldest first, for reaction delay.
    memory: BTreeMap<Id, VecDeque<(u64, [f64; 3])>>,
    /// The walk the bot was last sent on.
    pub heading: Option<Heading>,
    /// A bed the bots plugin could not reach, left alone until the tick.
    pub unreachable: Option<(Pos, u64)>,
    /// The enemy an attack process was last sent for.
    pub fighting: Option<Id>,
    /// The tick the walk was sent.
    pub sent_at: u64,
    /// The tick a bridge egg was last thrown.
    pub egg_at: Option<u64>,
    /// The bots plugin finished or gave up the walk.
    pub arrived: bool,
    /// The block a break process was last sent for.
    pub breaking: Option<Pos>,
    /// Whether the bot still has something to save up for, as of its
    /// last look at its inventory.
    pub saving: bool,
    pub stock: Stock,
    /// The ranged process the bot is running, by target and kind.
    pub ranged: Option<(Id, Shot)>,
}

impl Bot {
    pub fn new(number: u64, difficulty: Difficulty, arena: usize) -> Self {
        Self {
            number,
            name: name(number),
            difficulty,
            arena,
            handle: None,
            id: None,
            brain: goals::bedwars(),
            memory: BTreeMap::new(),
            heading: None,
            unreachable: None,
            fighting: None,
            sent_at: 0,
            egg_at: None,
            arrived: false,
            breaking: None,
            saving: true,
            stock: Stock::default(),
            ranged: None,
        }
    }

    pub fn skill(&self) -> Skill {
        self.difficulty.skill()
    }

    /// Records where enemies are now and returns where the bot believes
    /// they are: their position `reaction` ticks ago, and nothing for an
    /// enemy seen for less than that.
    pub fn perceive(&mut self, now: u64, live: &[Seen]) -> Vec<Seen> {
        let delay = self.skill().reaction;
        let ids: BTreeSet<Id> = live.iter().map(|s| s.id).collect();
        self.memory.retain(|id, _| ids.contains(id));
        let mut out = Vec::new();
        for seen in live {
            let trail = self.memory.entry(seen.id).or_default();
            trail.push_back((now, seen.pos));
            while trail.len() > 1 && trail[1].0 + delay <= now {
                trail.pop_front();
            }
            if let Some(&(at, pos)) = trail.front()
                && at + delay <= now
            {
                out.push(Seen { id: seen.id, pos });
            }
        }
        out
    }

    pub fn think(&mut self, view: &View) -> Intent {
        let mut intent = self.brain.tick(view);
        intent.sprint &= view.skill.sprint;
        intent
    }

    pub fn goal_names(&self) -> Vec<&'static str> {
        self.brain.running()
    }
}
