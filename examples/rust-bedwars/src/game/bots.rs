// SPDX-License-Identifier: MIT
//! Practice bots taking part in matches. A bot is a fake player of the
//! optional bots plugin: Bedwars asks for one through `tuff-bots:api`,
//! and once it is online it joins, fights, dies, respawns and shops
//! through the same paths as any player. What the game wants from the
//! bots plugin is queued as [`BotCall`]s for the plugin glue to send.

use super::*;
use crate::bot::{
    self, Bot, Difficulty, Role, Seen, Shot, Stock, View, block_center, block_of, dist,
};
use crate::shop::{self, Offer};
use tuff_bots_api::control::{self, BotId};

/// The answer to bot commands while the bots plugin is not installed.
pub const NO_BOTS: &str = "Bots need the bots plugin";
/// Ticks before a walk the bots plugin finished or gave up is asked again.
const BRIDGE_COOLDOWN: u64 = 2 * SECOND;
const BRIDGE_AHEAD: f64 = 3.0;
const RESEND: u64 = 2 * SECOND;
const WOOL_STOCK: u32 = 32;
const ARROW_STOCK: u32 = 12;
const FIREBALL_STOCK: u32 = 2;
const SWORDS: [&str; 3] = ["stone_sword", "iron_sword", "diamond_sword"];
const ARMORS: [&str; 3] = ["chainmail", "iron_armor", "diamond_armor"];

/// A request for the bots plugin.
#[derive(Clone, Debug, PartialEq)]
pub enum BotCall {
    /// Spawn the bot numbered `.0`; the answer comes back through
    /// [`Game::bot_spawned`].
    Spawn(u64, control::SpawnSpec),
    Remove(BotId),
    Process(BotId, control::Process),
}

/// Why the bots plugin did not spawn a bot.
#[derive(Clone, Debug, PartialEq)]
pub enum SpawnFailure {
    /// The bots plugin is not installed.
    NoProvider,
    Refused(String),
}

const COLORS: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

/// What a bot may build bridges with and dig through: the blocks players
/// place in a match and beds, never the map.
fn policy(sprint: bool) -> control::Policy {
    let wools = || COLORS.iter().map(|c| format!("minecraft:{c}_wool"));
    let mut breakable: Vec<String> = wools().collect();
    breakable.extend(COLORS.iter().map(|c| format!("minecraft:{c}_bed")));
    breakable.extend(["minecraft:oak_planks".into(), "minecraft:end_stone".into()]);
    control::Policy {
        allow_break: true,
        allow_place: true,
        allow_sprint: sprint,
        allow_parkour: false,
        max_fall: 3,
        break_allow: breakable,
        break_deny: Vec::new(),
        throwaway_items: wools().collect(),
        avoid: Vec::new(),
        auto_eat: false,
    }
}

/// What a bot carrying `carried` saves for next: the better sword, then
/// better armor, then blocks and ranged items.
pub fn next_buy(carried: &[(u8, String)]) -> Option<&'static Offer> {
    let has = |item: &str| {
        carried
            .iter()
            .filter(|(_, s)| shop::snbt_id(s) == Some(item))
            .map(|(_, s)| shop::snbt_count(s))
            .sum::<u32>()
    };
    let tier = |keys: &[&str]| {
        keys.iter()
            .rposition(|k| shop::find(k).is_some_and(|o| has(o.item) > 0))
            .map_or(0, |i| i + 1)
    };
    let sword = tier(&SWORDS);
    if let Some(key) = SWORDS.get(sword) {
        return shop::find(key);
    }
    let worn = carried
        .iter()
        .find(|(s, _)| *s == 8)
        .and_then(|(_, s)| shop::snbt_id(s))
        .map_or(0, |id| trade::rank(id).saturating_sub(1) as usize);
    if let Some(key) = ARMORS.get(worn) {
        return shop::find(key);
    }
    let wool: u32 = carried
        .iter()
        .filter(|(_, s)| shop::snbt_id(s).is_some_and(|id| id.ends_with("_wool")))
        .map(|(_, s)| shop::snbt_count(s))
        .sum();
    if wool < WOOL_STOCK {
        return shop::find("wool");
    }
    let stocked = |key: &str, stock: u32| shop::find(key).is_some_and(|o| has(o.item) >= stock);
    if !stocked("bow", 1) {
        return shop::find("bow");
    }
    if !stocked("arrows", ARROW_STOCK) {
        return shop::find("arrows");
    }
    if !stocked("fireball", FIREBALL_STOCK) {
        return shop::find("fireball");
    }
    (!stocked("bridge_egg", 1)).then(|| shop::find("bridge_egg"))?
}

/// Where to throw a bridge egg: a point ahead on the way to `goal` when the
/// ground there is open air and the goal is further than a hop away.
fn bridge_point(host: &mut dyn Host, dimension: &str, me: [f64; 3], goal: Pos) -> Option<[f64; 3]> {
    let to = block_center(goal);
    let (dx, dz) = (to[0] - me[0], to[2] - me[2]);
    let len = dx.hypot(dz);
    if len < 2.0 * BRIDGE_AHEAD {
        return None;
    }
    let ahead = [
        me[0] + dx / len * BRIDGE_AHEAD,
        me[1] - 1.0,
        me[2] + dz / len * BRIDGE_AHEAD,
    ];
    let under = block_of(ahead);
    let open = host.block_at(dimension, under)? == "minecraft:air"
        && host.block_at(dimension, [under[0], under[1] - 1, under[2]])? == "minecraft:air";
    open.then_some([ahead[0], me[1], ahead[2]])
}

fn goto(at: Pos) -> control::Process {
    control::Process::Goto(control::Goal::Leaf(control::LeafGoal::Near(
        control::NearGoal {
            pos: control::BlockPos {
                x: at[0],
                y: at[1],
                z: at[2],
            },
            radius: 1.0,
        },
    )))
}

fn attack(target: Id) -> control::Process {
    control::Process::Attack(control::AttackSpec {
        target: format!("{target:032x}"),
        until: control::AttackUntil::Death,
        retreat_health: 0.0,
    })
}

impl Game {
    fn bot_by_id(&self, id: Id) -> Option<&Bot> {
        self.bots.values().find(|b| b.id == Some(id))
    }

    fn bot_by_id_mut(&mut self, id: Id) -> Option<&mut Bot> {
        self.bots.values_mut().find(|b| b.id == Some(id))
    }

    /// Bots of arena `number` still being spawned.
    fn pending_bots(&self, number: usize) -> usize {
        self.bots
            .values()
            .filter(|b| b.arena == number && b.id.is_none())
            .count()
    }

    /// Free places of arena `i` for players and bots still coming.
    fn room(&self, i: usize) -> usize {
        let arena = &self.arenas[i];
        let taken = arena.queue.len() + self.pending_bots(arena.number);
        if matches!(arena.phase, Phase::Building | Phase::Waiting { .. }) {
            arena.map.capacity().saturating_sub(taken)
        } else {
            0
        }
    }

    fn bot_arena(&self, number: Option<usize>) -> Result<usize, String> {
        let i = match number {
            Some(n) => self.index(n)?,
            None => (0..self.arenas.len())
                .filter(|&i| self.room(i) > 0)
                .max_by_key(|&i| (self.arenas[i].queue.len(), usize::MAX - i))
                .ok_or("No arena has room; try again after the next game")?,
        };
        let arena = &self.arenas[i];
        if !matches!(arena.phase, Phase::Building | Phase::Waiting { .. }) {
            return Err(format!("A game is running in {}", arena.label()));
        }
        if self.room(i) == 0 {
            return Err(format!("{} is full", arena.label()));
        }
        Ok(i)
    }

    /// Asks the bots plugin for a bot of `difficulty` that joins arena
    /// `number`'s queue, or the fullest open arena's, once it is online.
    pub fn add_bot(
        &mut self,
        number: Option<usize>,
        difficulty: Difficulty,
    ) -> Result<String, String> {
        if !self.bots_present {
            return Err(NO_BOTS.into());
        }
        let i = self.bot_arena(number)?;
        let arena = &self.arenas[i];
        let (dimension, at) = match arena.phase {
            Phase::Waiting { .. } => (arena.dimension.clone(), arena.waiting_spot()),
            _ => (crate::LOBBY.to_owned(), center(crate::LOBBY_SPAWN)),
        };
        let bot = Bot::new(self.made_bots, difficulty, arena.number);
        self.made_bots += 1;
        let skill = difficulty.skill();
        let spec = control::SpawnSpec {
            name: bot.name.clone(),
            model: bot::model(bot.number),
            location: control::Location {
                dimension: Some(dimension),
                pos: control::Vec3 {
                    x: at[0],
                    y: at[1],
                    z: at[2],
                },
                yaw: None,
                pitch: None,
            },
            skill: skill.api(),
            policy: policy(skill.sprint),
            respawn: control::RespawnPolicy::Owner,
        };
        let line = format!(
            "Adding {} ({}) to {}",
            bot.name,
            difficulty.name(),
            arena.label()
        );
        self.bot_calls.push(BotCall::Spawn(bot.number, spec));
        self.bots.insert(bot.number, bot);
        Ok(line)
    }

    /// Asks for bots for every free place of arena `number`; how many.
    pub fn fill_bots(&mut self, number: usize, difficulty: Difficulty) -> Result<usize, String> {
        if !self.bots_present {
            return Err(NO_BOTS.into());
        }
        let i = self.bot_arena(Some(number))?;
        let free = self.room(i);
        for _ in 0..free {
            self.add_bot(Some(number), difficulty)?;
        }
        Ok(free)
    }

    /// Takes every bot out of arena `number` as if it left; how many.
    pub fn remove_bots(&mut self, host: &mut dyn Host, number: usize) -> Result<usize, String> {
        if !self.bots_present && self.bots.is_empty() {
            return Err(NO_BOTS.into());
        }
        self.index(number)?;
        let mine: Vec<u64> = self
            .bots
            .values()
            .filter(|b| b.arena == number)
            .map(|b| b.number)
            .collect();
        for n in &mine {
            self.drop_bot(host, *n, true);
        }
        Ok(mine.len())
    }

    /// Forgets bot `n`: it leaves its arena like a player, and the bots
    /// plugin removes it when `remove`.
    fn drop_bot(&mut self, host: &mut dyn Host, n: u64, remove: bool) {
        let Some(bot) = self.bots.remove(&n) else {
            return;
        };
        if let Some(id) = bot.id {
            let _ = self.leave(host, id, false);
        }
        if remove && let Some(handle) = bot.handle {
            self.bot_calls.push(BotCall::Remove(handle));
        }
    }

    /// The bots plugin answered the spawn of bot `n`.
    pub fn bot_spawned(
        &mut self,
        host: &mut dyn Host,
        n: u64,
        result: Result<BotId, SpawnFailure>,
    ) {
        match result {
            Ok(handle) => match self.bots.get_mut(&n) {
                Some(bot) => {
                    bot.handle = Some(handle);
                    self.seat_bots(host);
                }
                None => self.bot_calls.push(BotCall::Remove(handle)),
            },
            Err(failure) => {
                if failure == SpawnFailure::NoProvider {
                    self.bots_present = false;
                }
                self.bots.remove(&n);
            }
        }
    }

    /// The bots plugin unloaded: its bots are gone, as players who left.
    pub fn bots_unloaded(&mut self, host: &mut dyn Host) {
        self.bots_present = false;
        let all: Vec<u64> = self.bots.keys().copied().collect();
        for n in all {
            self.drop_bot(host, n, false);
        }
    }

    /// The bots plugin removed bot `handle` on its own.
    pub fn bot_removed(&mut self, host: &mut dyn Host, handle: BotId) {
        let found = self.bots.values().find(|b| b.handle == Some(handle));
        if let Some(n) = found.map(|b| b.number) {
            self.drop_bot(host, n, false);
        }
    }

    /// Bot `handle` finished or gave up its walk.
    pub fn bot_walk_ended(&mut self, handle: BotId) {
        if let Some(bot) = self.bots.values_mut().find(|b| b.handle == Some(handle)) {
            bot.arrived = true;
        }
    }

    /// `id` picked something up; a bot spends its inventory at the shop
    /// (one purchase at a time) as a player would.
    pub fn bot_picked_up(&mut self, host: &mut dyn Host, id: Id) {
        if self.bot_by_id(id).is_none() {
            return;
        }
        let carried = host.carried();
        if let Some(bot) = self.bot_by_id_mut(id) {
            bot.stock = Stock::from_carried(&carried);
        }
        let Some(offer) = next_buy(&carried) else {
            if let Some(bot) = self.bot_by_id_mut(id) {
                bot.saving = false;
            }
            return;
        };
        let bought = self.buy(host, id, offer).is_ok();
        if let Some(bot) = self.bot_by_id_mut(id) {
            if bought {
                bot.stock.bought(offer.item, offer.count);
            }
            bot.saving = !bought;
        }
    }

    /// Bots the bots plugin spawned join their arena once they are online.
    fn seat_bots(&mut self, host: &mut dyn Host) {
        let waiting: Vec<u64> = self
            .bots
            .values()
            .filter(|b| b.handle.is_some() && b.id.is_none())
            .map(|b| b.number)
            .collect();
        for n in waiting {
            let bot = &self.bots[&n];
            let Some(id) = host.player_named(&bot.name) else {
                continue;
            };
            let (name, arena) = (bot.name.clone(), bot.arena);
            self.bots.get_mut(&n).expect("listed").id = Some(id);
            if self.join(host, Who::new(id, &name), Some(arena)).is_err() {
                self.drop_bot(host, n, true);
            }
        }
    }

    pub(super) fn tick_bots(&mut self, host: &mut dyn Host) {
        if self.bots.is_empty() {
            return;
        }
        self.seat_bots(host);
        let held: BTreeSet<Id> = self
            .arenas
            .iter()
            .flat_map(|a| {
                let mut ids = a.audience();
                if let Some(game) = a.game() {
                    ids.extend(game.members.keys());
                }
                ids
            })
            .collect();
        let gone: Vec<u64> = self
            .bots
            .values()
            .filter(|b| b.id.is_some_and(|id| !held.contains(&id)))
            .map(|b| b.number)
            .collect();
        for n in gone {
            self.drop_bot(host, n, true);
        }
        for i in 0..self.arenas.len() {
            self.steer(host, i);
        }
    }

    /// Runs the brains of arena `i`'s bots and sends their walks.
    fn steer(&mut self, host: &mut dyn Host, i: usize) {
        let now = self.now;
        let arena = &self.arenas[i];
        let Phase::Running(game) = &arena.phase else {
            return;
        };
        let mine: Vec<(Id, u64, usize, Life)> = game
            .members
            .iter()
            .filter_map(|(id, m)| {
                let bot = self.bots.values().find(|b| b.id == Some(*id))?;
                Some((*id, bot.number, m.team, m.life))
            })
            .collect();
        if mine.is_empty() {
            return;
        }
        let live: Vec<(Id, usize, [f64; 3])> = game
            .members
            .iter()
            .filter(|(_, m)| m.life == Life::Alive)
            .filter_map(|(id, m)| Some((*id, m.team, host.position(*id)?)))
            .collect();
        let mut mine_teams: Vec<(u64, usize)> = mine.iter().map(|m| (m.1, m.2)).collect();
        mine_teams.sort();
        for (id, n, team, life) in mine {
            let bot = self.bots.get_mut(&n).expect("listed");
            let me = live.iter().find(|(v, _, _)| *v == id).map(|l| l.2);
            let (Life::Alive, Some(me)) = (life, me) else {
                bot.breaking = None;
                bot.fighting = None;
                bot.stock = Default::default();
                bot.ranged = None;
                if bot.heading.take().is_some()
                    && let Some(handle) = bot.handle
                {
                    self.bot_calls
                        .push(BotCall::Process(handle, control::Process::Idle));
                }
                continue;
            };
            let seen: Vec<Seen> = live
                .iter()
                .filter(|(_, t, _)| *t != team)
                .map(|&(id, _, pos)| Seen { id, pos })
                .collect();
            let enemies = bot.perceive(now, &seen);
            let enemy_beds: Vec<Pos> = (0..arena.map.teams.len())
                .filter(|&t| t != team && game.teams[t].bed)
                .map(|t| arena.map.teams[t].bed)
                .collect();
            let mates: Vec<u64> = mine_teams
                .iter()
                .filter(|(_, t)| *t == team)
                .map(|(n, _)| *n)
                .collect();
            let role = Role::assign(mates.iter().position(|m| *m == n).unwrap_or(0), mates.len());
            let ally = live
                .iter()
                .filter(|(v, t, _)| *t == team && *v != id)
                .map(|l| l.2)
                .min_by(|a, b| dist(*a, me).total_cmp(&dist(*b, me)));
            let view = View {
                role,
                ally,
                now,
                elapsed: now - game.started,
                me,
                home: center(arena.map.teams[team].spawn),
                forge: center(arena.map.teams[team].forge),
                saving: bot.saving,
                bed: game.teams[team].bed.then_some(arena.map.teams[team].bed),
                enemy_beds: &enemy_beds,
                enemies: &enemies,
                skill: bot.skill(),
                bow: bot.stock.bow,
                arrows: bot.stock.arrows,
                fireballs: bot.stock.fireballs,
            };
            let intent = bot.think(&view);
            let fight = intent.fight.filter(|_| intent.aim.is_none());
            let target = intent.goto.map(block_of).filter(|_| fight.is_none());
            if intent.aim != bot.ranged {
                bot.ranged = intent.aim;
                match intent.aim {
                    Some((enemy, Shot::Arrow)) => {
                        let process = control::Process::Shoot(control::ShootSpec {
                            target: format!("{enemy:032x}"),
                            until: control::AttackUntil::Death,
                            melee_range: 3.0,
                        });
                        if let Some(handle) = bot.handle {
                            self.bot_calls.push(BotCall::Process(handle, process));
                        }
                    }
                    Some((_, Shot::Fireball)) => {}
                    None => bot.heading = None,
                }
            }
            if let (Some((enemy, Shot::Fireball)), Some(Shot::Fireball), Some(handle)) =
                (intent.aim, intent.shoot, bot.handle)
            {
                bot.stock.spent(Shot::Fireball);
                let process = control::Process::UseItem(control::UseItemSpec {
                    item: "minecraft:fire_charge".into(),
                    look_at: Some(control::LookTarget::Entity(format!("{enemy:032x}"))),
                    hold_ticks: 0,
                });
                self.bot_calls.push(BotCall::Process(handle, process));
            }
            if let (Some(goal), Some(handle), None, None) = (target, bot.handle, bot.ranged, fight)
                && bot.stock.eggs > 0
                && bot.egg_at.is_none_or(|t| now >= t + BRIDGE_COOLDOWN)
                && let Some(ahead) = bridge_point(host, &arena.dimension, me, goal)
            {
                bot.stock.eggs -= 1;
                bot.egg_at = Some(now);
                let process = control::Process::UseItem(control::UseItemSpec {
                    item: "minecraft:egg".into(),
                    look_at: Some(control::LookTarget::Position(control::Vec3 {
                        x: ahead[0],
                        y: ahead[1],
                        z: ahead[2],
                    })),
                    hold_ticks: 0,
                });
                self.bot_calls.push(BotCall::Process(handle, process));
            }
            if intent.shoot == Some(Shot::Arrow) {
                bot.stock.spent(Shot::Arrow);
            }
            let target = if bot.ranged.is_some() {
                bot.heading
            } else {
                target
            };
            let retry = bot.breaking.is_some() && bot.arrived && now >= bot.sent_at + RESEND;
            if intent.dig != bot.breaking || retry {
                bot.breaking = intent.dig;
                if let (Some(at), Some(handle)) = (intent.dig, bot.handle) {
                    let block = control::BlockPos {
                        x: at[0],
                        y: at[1],
                        z: at[2],
                    };
                    self.bot_calls.push(BotCall::Process(
                        handle,
                        control::Process::Break(vec![block]),
                    ));
                    bot.heading = None;
                    bot.fighting = None;
                    bot.sent_at = now;
                    bot.arrived = false;
                }
            }
            if bot.breaking.is_some() {
                continue;
            }
            let again = bot.ranged.is_none()
                && bot.arrived
                && target.is_some()
                && now >= bot.sent_at + RESEND;
            if (target != bot.heading || fight != bot.fighting || again)
                && let Some(handle) = bot.handle
            {
                let process = match fight {
                    Some(id) => attack(id),
                    None => target.map_or(control::Process::Idle, goto),
                };
                self.bot_calls.push(BotCall::Process(handle, process));
                bot.heading = target;
                bot.fighting = fight;
                bot.sent_at = now;
                bot.arrived = false;
            }
        }
    }
}
