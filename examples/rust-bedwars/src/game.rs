// SPDX-License-Identifier: MIT
//! The game itself, free of host calls: arenas with their queue and
//! match, driven by commands, events and one `tick` per server tick, and
//! acting through a [`Host`]. Everything is deterministic, so tests drive
//! it with a recording host.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use tuff_plugin_sdk::text;
use tuff_plugin_sdk::text::{Color, Styled};

use crate::economy::{self, Resource, SECOND, Stage, TIMELINE};
use crate::map::{Map, Pos, Team};

pub type Id = u128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Who {
    pub id: Id,
    pub name: String,
}

impl Who {
    pub fn new(id: Id, name: &str) -> Self {
        Self {
            id,
            name: name.to_owned(),
        }
    }
}

/// What the game asks of the server.
pub trait Host {
    fn tell(&mut self, to: Id, text: &str);
    fn set_block(&mut self, dimension: &str, at: Pos, state: &str);
    fn teleport(&mut self, who: Id, dimension: &str, at: [f64; 3], yaw: f32);
    fn spawn_item(&mut self, dimension: &str, at: [f64; 3], item: &str);
    /// How many of `item` lie around `at`.
    fn items_near(&mut self, dimension: &str, at: [f64; 3], item: &str) -> u32;
    /// Removes dropped `item`s around `at`.
    fn sweep(&mut self, dimension: &str, at: Pos, item: &str);
    /// Gives the player the current callback acts for `item` back.
    fn give_back(&mut self, item: &str);
    /// Saves where the acting player `who` is (and what they carry) so
    /// [`Host::restore_state`] can put them back.
    fn save_state(&mut self, who: Id);
    /// Puts `who` back as saved, once; nothing when nothing is saved.
    fn restore_state(&mut self, who: Id);
    /// Whether the arena holds a game's leftovers until it is rebuilt.
    fn mark_dirty(&mut self, arena: usize, dirty: bool);

    fn open_menu(&mut self, _who: Id, _menu: &str, _rows: u8, _title: &str, _items: &[MenuSlot]) {}
    /// The occupied slots of the player the current callback acts for, as
    /// `(slot, stack SNBT)`.
    fn carried(&mut self) -> Vec<(u8, String)> {
        Vec::new()
    }
    fn take(&mut self, _who: Id, _item: &str, _count: u32) {}
    fn give(&mut self, _who: Id, _item: &str, _count: u32) {}
    fn set_slot(&mut self, _who: Id, _slot: u8, _stack: Option<&str>) {}
    fn clear(&mut self, _who: Id) {}
    /// Whether `who` keeps their inventory on death.
    fn keep_inventory(&mut self, _who: Id, _keep: bool) {}
    fn effect(&mut self, _who: Id, _effect: &str, _ticks: i32, _amplifier: u8) {}
    fn position(&mut self, _who: Id) -> Option<[f64; 3]> {
        None
    }
    fn title(&mut self, _who: Id, _title: &str, _subtitle: &str) {}
    fn action_bar(&mut self, _who: Id, _text: &str) {}
    /// Shows the shared sidebar, or removes it.
    fn sidebar(&mut self, _lines: Option<(&str, &[String])>) {}
    /// Moves a dead player out of the death screen and into spectating
    /// (`true`), or back into survival at full health.
    fn spectate(&mut self, _who: Id, _on: bool) {}
    /// Spawns an entity with SNBT `snbt`, returning its id.
    fn launch(&mut self, _dimension: &str, _kind: &str, _at: [f64; 3], _snbt: &str) -> Option<Id> {
        None
    }
    /// Where a loaded entity is.
    fn entity_at(&mut self, _entity: Id) -> Option<[f64; 3]> {
        None
    }
    /// The block state at `at`.
    fn block_at(&mut self, _dimension: &str, _at: Pos) -> Option<String> {
        None
    }
    /// Has the server spare the box of arena `arena` from explosions
    /// except for blocks players place, until [`Host::unprotect`].
    fn protect(&mut self, _arena: usize, _dimension: &str, _bounds: [Pos; 2], _beds: &[String]) {}
    fn unprotect(&mut self, _arena: usize) {}
    /// Removes a non-player entity without death effects.
    fn remove_entity(&mut self, _entity: Id) {}
    /// Removes every shopkeeper (by tag) inside the box, such as ones
    /// saved with the world by an earlier run.
    fn clear_shopkeepers(&mut self, _dimension: &str, _bounds: [Pos; 2]) {}
    /// Where `who` looks from: dimension, eye position and unit direction.
    fn gaze(&mut self, _who: Id) -> Option<(String, [f64; 3], [f64; 3])> {
        None
    }
    /// The uuid of the online player named `name`.
    fn player_named(&mut self, _name: &str) -> Option<Id> {
        None
    }
}

/// One slot of a shop menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuSlot {
    pub slot: u8,
    pub item: String,
    pub count: u8,
    pub name: String,
    pub lore: Vec<String>,
    pub glint: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Queued players needed to start the countdown.
    pub min_players: usize,
    pub countdown: u64,
    pub respawn: u64,
    /// Blocks an arena build writes per tick.
    pub writes_per_tick: usize,
    /// How long the winners celebrate before the arena resets.
    pub end_delay: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            min_players: 2,
            countdown: 20 * SECOND,
            respawn: 5 * SECOND,
            writes_per_tick: 4096,
            end_delay: 10 * SECOND,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Life {
    Alive,
    Respawning { at: u64 },
    Out,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub name: String,
    pub team: usize,
    pub life: Life,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamState {
    pub bed: bool,
    /// Bought forge tiers.
    pub forge: usize,
    /// Bought tiers by upgrade key.
    pub upgrades: BTreeMap<String, usize>,
    /// Enemies already caught by the bought trap.
    pub trapped: bool,
}

impl TeamState {
    pub fn tier(&self, key: &str) -> usize {
        self.upgrades.get(key).copied().unwrap_or(0)
    }

    pub fn gear(&self) -> crate::shop::Gear {
        crate::shop::Gear {
            sharpness: self.tier("sharpness"),
            protection: self.tier("protection"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Generator {
    resource: Resource,
    at: [f64; 3],
    team: Option<usize>,
    next: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub started: u64,
    pub teams: Vec<TeamState>,
    pub members: BTreeMap<Id, Member>,
    /// Blocks players placed, the only ones they may break.
    pub placed: BTreeSet<Pos>,
    generators: Vec<Generator>,
    /// Timeline stages that happened.
    pub passed: usize,
    /// What each team keeps in its chest; lasts the game.
    pub chests: Vec<Stash>,
    /// Each player's ender chest in this game, apart from the real one.
    pub ender: BTreeMap<Id, Stash>,
}

impl Match {
    fn alive_teams(&self) -> BTreeSet<usize> {
        self.members
            .values()
            .filter(|m| m.life != Life::Out)
            .map(|m| m.team)
            .collect()
    }

    fn team_size(&self, team: usize) -> usize {
        self.members.values().filter(|m| m.team == team).count()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Writing the map; queued players wait where they are.
    Building,
    Waiting {
        start_at: Option<u64>,
    },
    Running(Box<Match>),
    /// The game is decided; the arena resets at `until`.
    Ending {
        until: u64,
        game: Box<Match>,
    },
}

#[derive(Clone, Debug, PartialEq)]
enum Job {
    /// Air over the whole box, from linear index `next`.
    Clear { lo: Pos, hi: Pos, next: u64 },
    Write {
        blocks: Vec<(Pos, String)>,
        next: usize,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Arena {
    pub number: usize,
    pub dimension: String,
    pub map: Map,
    pub queue: Vec<Who>,
    pub phase: Phase,
    jobs: VecDeque<Job>,
    shops: Vec<Id>,
}

pub const SHOPKEEPER_KIND: &str = "minecraft:villager";
pub const SHOPKEEPER_TAG: &str = "bedwars_shop";

impl Arena {
    fn new(number: usize, dimension: String, map: Map, dirty: bool) -> Self {
        let mut jobs = VecDeque::new();
        if dirty {
            let [lo, hi] = map.bounds;
            jobs.push_back(Job::Clear { lo, hi, next: 0 });
        }
        jobs.push_back(Job::Write {
            blocks: map.blocks(),
            next: 0,
        });
        Self {
            number,
            dimension,
            map,
            queue: Vec::new(),
            phase: Phase::Building,
            jobs,
            shops: Vec::new(),
        }
    }

    fn game(&self) -> Option<&Match> {
        match &self.phase {
            Phase::Running(game) | Phase::Ending { game, .. } => Some(game),
            _ => None,
        }
    }

    /// Everyone taking part: the queue and players not out.
    pub fn audience(&self) -> Vec<Id> {
        let mut out: Vec<Id> = self.queue.iter().map(|w| w.id).collect();
        if let Some(game) = self.game() {
            out.extend(
                game.members
                    .iter()
                    .filter(|(_, m)| m.life != Life::Out)
                    .map(|(id, _)| *id),
            );
        }
        out
    }

    fn holds(&self, id: Id) -> bool {
        self.audience().contains(&id)
    }

    fn tell_all(&self, host: &mut dyn Host, line: &str) {
        for id in self.audience() {
            host.tell(id, line);
        }
    }

    fn team_name(&self, team: usize) -> String {
        let t = &self.map.teams[team];
        text!(t.name.as_str().color(t.chat_color()))
    }

    fn waiting_spot(&self) -> [f64; 3] {
        let [x, y, z] = self.map.waiting;
        [x as f64 + 0.5, y as f64, z as f64 + 0.5]
    }

    fn label(&self) -> String {
        format!("arena {} ({})", self.number, self.map.name)
    }

    /// Writes up to `budget` blocks of the queued jobs; true when done.
    /// Whether the waiting platform and the beds still stand; a forced
    /// chunk regeneration wipes them.
    fn intact(&self, host: &mut dyn Host) -> bool {
        let bare = |state: &str| state.split('[').next().unwrap_or(state).to_owned();
        let center = [
            self.map.waiting[0],
            self.map.waiting[1] - 1,
            self.map.waiting[2],
        ];
        std::iter::once((center, "minecraft:glass".to_owned()))
            .chain(self.map.teams.iter().map(|t| {
                let (at, state) = &t.bed_blocks()[0];
                (*at, bare(state))
            }))
            .all(|(at, want)| {
                host.block_at(&self.dimension, at)
                    .is_none_or(|have| bare(&have) == want)
            })
    }

    fn rebuild(&mut self, stale_shops: &mut Vec<Id>) {
        stale_shops.append(&mut self.shops);
        self.jobs.push_back(Job::Write {
            blocks: self.map.blocks(),
            next: 0,
        });
        self.phase = Phase::Building;
    }

    fn work(&mut self, host: &mut dyn Host, mut budget: usize) -> bool {
        while budget > 0 {
            let Some(job) = self.jobs.front_mut() else {
                return true;
            };
            let done = match job {
                Job::Clear { lo, hi, next } => {
                    let w = (hi[0] - lo[0] + 1) as u64;
                    let d = (hi[2] - lo[2] + 1) as u64;
                    let total = w * d * (hi[1] - lo[1] + 1) as u64;
                    while *next < total && budget > 0 {
                        let i = *next;
                        let at = [
                            lo[0] + (i % w) as i32,
                            lo[1] + (i / (w * d)) as i32,
                            lo[2] + (i / w % d) as i32,
                        ];
                        host.set_block(&self.dimension, at, "minecraft:air");
                        *next += 1;
                        budget -= 1;
                    }
                    *next >= total
                }
                Job::Write { blocks, next } => {
                    while *next < blocks.len() && budget > 0 {
                        let (at, state) = &blocks[*next];
                        host.set_block(&self.dimension, *at, state);
                        *next += 1;
                        budget -= 1;
                    }
                    *next >= blocks.len()
                }
            };
            if done {
                self.jobs.pop_front();
            }
        }
        self.jobs.is_empty()
    }
}

mod explosives;
pub use explosives::{BLAST_PROOF, EGG, FIRE_CHARGE};
mod bots;
mod chests;
pub use chests::Stash;
mod trade;
pub use bots::{BotCall, NO_BOTS, SpawnFailure};

/// Every arena and the clock.
#[derive(Debug, Default)]
pub struct Game {
    pub arenas: Vec<Arena>,
    pub now: u64,
    pub settings: Settings,
    /// Players to put back once their death screen is likely gone.
    restores: Vec<(u64, Id)>,
    /// What each player paid this tick, not yet out of their inventory.
    spent: BTreeMap<Id, BTreeMap<Resource, u32>>,
    /// What each player put in a chest this tick, not yet out of their
    /// inventory.
    deposited: BTreeMap<Id, Stash>,
    bridges: Vec<explosives::Bridge>,
    /// Practice bots by number, spawned or still being spawned.
    pub bots: BTreeMap<u64, crate::bot::Bot>,
    /// Bots made so far, for their names.
    made_bots: u64,
    /// Requests for the bots plugin, sent by the plugin glue.
    pub bot_calls: Vec<BotCall>,
    /// Whether the bots plugin is installed.
    pub bots_present: bool,
    stale_shops: Vec<Id>,
}

fn item_of(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

fn center([x, y, z]: Pos) -> [f64; 3] {
    [x as f64 + 0.5, y as f64, z as f64 + 0.5]
}

impl Game {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            ..Self::default()
        }
    }

    fn index(&self, number: usize) -> Result<usize, String> {
        self.arenas
            .iter()
            .position(|a| a.number == number)
            .ok_or_else(|| format!("There is no arena {number}"))
    }

    pub fn arena(&self, number: usize) -> Option<&Arena> {
        self.arenas.iter().find(|a| a.number == number)
    }

    /// The arena `id` takes part in.
    pub fn arena_of(&self, id: Id) -> Option<&Arena> {
        self.arenas.iter().find(|a| a.holds(id))
    }

    fn in_dimension(&self, dimension: &str) -> Option<usize> {
        self.arenas.iter().position(|a| a.dimension == dimension)
    }

    /// Sets up arena `number` in `dimension` with `map`, building it (after
    /// clearing its whole box when `dirty`). Refused while anyone plays
    /// there.
    pub fn add_arena(
        &mut self,
        number: usize,
        dimension: &str,
        map: Map,
        dirty: bool,
    ) -> Result<(), String> {
        let arena = Arena::new(number, dimension.to_owned(), map, dirty);
        match self.arenas.iter().position(|a| a.number == number) {
            Some(i) if !self.arenas[i].audience().is_empty() => {
                Err(format!("Players are in arena {number}"))
            }
            Some(i) => {
                self.stale_shops.append(&mut self.arenas[i].shops);
                self.arenas[i] = arena;
                Ok(())
            }
            None => {
                self.arenas.push(arena);
                self.arenas.sort_by_key(|a| a.number);
                Ok(())
            }
        }
    }

    /// Ends whatever runs in arena `number`, puts its players back and
    /// drops it; its blocks stay until it is set up again.
    pub fn remove_arena(&mut self, host: &mut dyn Host, number: usize) -> Result<(), String> {
        let i = self.index(number)?;
        for id in self.arenas[i].audience() {
            host.restore_state(id);
        }
        for id in std::mem::take(&mut self.arenas[i].shops) {
            host.remove_entity(id);
        }
        self.arenas.remove(i);
        Ok(())
    }

    pub fn join(
        &mut self,
        host: &mut dyn Host,
        who: Who,
        number: Option<usize>,
    ) -> Result<String, String> {
        if let Some(arena) = self.arena_of(who.id) {
            return Err(format!(
                "You are already in {}; leave it with /bedwars leave",
                arena.label()
            ));
        }
        if self.restores.iter().any(|(_, id)| *id == who.id) {
            return Err("You can join again in a moment".into());
        }
        let open = |a: &Arena| {
            matches!(a.phase, Phase::Building | Phase::Waiting { .. })
                && a.queue.len() < a.map.capacity()
        };
        let i = match number {
            Some(n) => self.index(n)?,
            None => self
                .arenas
                .iter()
                .enumerate()
                .filter(|(_, a)| open(a))
                .max_by_key(|(i, a)| (a.queue.len(), usize::MAX - i))
                .map(|(i, _)| i)
                .ok_or("No arena has room; try again after the next game")?,
        };
        let arena = &mut self.arenas[i];
        if !matches!(arena.phase, Phase::Building | Phase::Waiting { .. }) {
            return Err(format!("A game is running in {}", arena.label()));
        }
        if arena.queue.len() >= arena.map.capacity() {
            return Err(format!("{} is full", arena.label()));
        }
        host.save_state(who.id);
        let line = text!(
            who.name.as_str().gray(),
            " has joined (".yellow(),
            (arena.queue.len() + 1).to_string().aqua(),
            "/".yellow(),
            arena.map.capacity().to_string().aqua(),
            ")!".yellow()
        );
        arena.queue.push(who.clone());
        if matches!(arena.phase, Phase::Waiting { .. }) {
            host.teleport(who.id, &arena.dimension, arena.waiting_spot(), 0.0);
        }
        arena.tell_all(host, &line);
        Ok(format!("You joined {}", arena.label()))
    }

    /// `id` leaves their queue or game; `online` false when they
    /// disconnected (they are put back when they return).
    pub fn leave(&mut self, host: &mut dyn Host, id: Id, online: bool) -> Result<String, String> {
        let i = self
            .arenas
            .iter()
            .position(|a| a.holds(id))
            .ok_or("You are not in a game")?;
        let arena = &mut self.arenas[i];
        let label = arena.label();
        if let Some(at) = arena.queue.iter().position(|w| w.id == id) {
            let who = arena.queue.remove(at);
            let line = text!(who.name.as_str().gray(), " has quit!".yellow());
            arena.tell_all(host, &line);
        } else if let Phase::Running(game) | Phase::Ending { game, .. } = &mut arena.phase {
            let member = game.members.get_mut(&id).expect("held");
            member.life = Life::Out;
            let line = text!(member.name.as_str().gray(), " left the game.".gray());
            let team = member.team;
            arena.tell_all(host, &line);
            if matches!(arena.phase, Phase::Running(_)) {
                self.team_check(host, i, team);
            }
        }
        self.restores.retain(|(_, r)| *r != id);
        if online {
            host.restore_state(id);
        }
        Ok(format!("You left {label}"))
    }

    /// `id` left the server: they forfeit, and are put back when they
    /// return rather than now.
    pub fn disconnected(&mut self, host: &mut dyn Host, id: Id) {
        self.restores.retain(|(_, r)| *r != id);
        let _ = self.leave(host, id, false);
    }

    /// Starts arena `number` now, whatever the countdown says.
    pub fn start(&mut self, host: &mut dyn Host, number: usize) -> Result<String, String> {
        let i = self.index(number)?;
        let arena = &self.arenas[i];
        if !matches!(arena.phase, Phase::Waiting { .. }) {
            return Err(format!("{} is not waiting for players", arena.label()));
        }
        if arena.queue.len() < 2 {
            return Err("A game needs at least 2 players".into());
        }
        self.begin(host, i);
        Ok(format!("Started {}", self.arenas[i].label()))
    }

    /// Ends the game in arena `number` with no winner and resets it.
    pub fn stop(&mut self, host: &mut dyn Host, number: usize) -> Result<String, String> {
        let i = self.index(number)?;
        if !matches!(self.arenas[i].phase, Phase::Running(_)) {
            return Err(format!("No game runs in {}", self.arenas[i].label()));
        }
        self.arenas[i].tell_all(host, &text!("The game was stopped.".red()));
        self.finish(host, i, None);
        Ok(format!("Stopped {}", self.arenas[i].label()))
    }

    /// Puts every player back, as when the plugin is unloaded.
    pub fn shutdown(&mut self, host: &mut dyn Host) {
        for id in std::mem::take(&mut self.stale_shops) {
            host.remove_entity(id);
        }
        for arena in &mut self.arenas {
            for id in std::mem::take(&mut arena.shops) {
                host.remove_entity(id);
            }
        }
        for arena in &self.arenas {
            host.unprotect(arena.number);
        }
        for arena in &self.arenas {
            for id in arena.audience() {
                host.restore_state(id);
            }
        }
        for (_, id) in std::mem::take(&mut self.restores) {
            host.restore_state(id);
        }
    }

    fn begin(&mut self, host: &mut dyn Host, i: usize) {
        let now = self.now;
        let arena = &mut self.arenas[i];
        let teams = arena.map.teams.len();
        let mut game = Match {
            started: now,
            teams: vec![
                TeamState {
                    bed: true,
                    forge: 0,
                    upgrades: BTreeMap::new(),
                    trapped: false,
                };
                teams
            ],
            members: BTreeMap::new(),
            placed: BTreeSet::new(),
            generators: Vec::new(),
            passed: 0,
            chests: vec![Stash::new(); teams],
            ender: BTreeMap::new(),
        };
        for who in std::mem::take(&mut arena.queue) {
            let team = (0..teams)
                .min_by_key(|&t| (game.team_size(t), t))
                .expect("maps have teams");
            game.members.insert(
                who.id,
                Member {
                    name: who.name,
                    team,
                    life: Life::Alive,
                },
            );
        }
        for at in arena.map.platform() {
            host.set_block(&arena.dimension, at, "minecraft:air");
        }
        for (t, team) in arena.map.teams.iter().enumerate() {
            if game.team_size(t) == 0 {
                game.teams[t].bed = false;
                for (at, _) in team.bed_blocks() {
                    host.set_block(&arena.dimension, at, "minecraft:air");
                }
                continue;
            }
            for resource in [Resource::Iron, Resource::Gold, Resource::Emerald] {
                game.generators.push(Generator {
                    resource,
                    at: center(team.forge),
                    team: Some(t),
                    next: now,
                });
            }
        }
        for (resource, points) in [
            (Resource::Diamond, &arena.map.diamonds),
            (Resource::Emerald, &arena.map.emeralds),
        ] {
            for &at in points {
                game.generators.push(Generator {
                    resource,
                    at: center(at),
                    team: None,
                    next: now + economy::generator_interval(resource, 0),
                });
            }
        }
        for (id, member) in &game.members {
            let team = &arena.map.teams[member.team];
            host.teleport(*id, &arena.dimension, center(team.spawn), team.yaw);
            let line = text!(
                "You are on the ".yellow(),
                team.name.as_str().color(team.chat_color()).bold(),
                " team. Protect your bed and destroy the others!".yellow()
            );
            host.tell(*id, &line);
            host.keep_inventory(*id, true);
            trade::kit(host, *id, &team.color, game.teams[member.team].gear());
        }
        host.mark_dirty(arena.number, true);
        let beds: Vec<String> = arena.map.teams.iter().map(Team::bed_item).collect();
        host.protect(arena.number, &arena.dimension, arena.map.bounds, &beds);
        arena.phase = Phase::Running(Box::new(game));
    }

    /// Advances every arena by one server tick.
    pub fn tick(&mut self, host: &mut dyn Host) {
        self.now += 1;
        let now = self.now;
        for id in std::mem::take(&mut self.stale_shops) {
            host.remove_entity(id);
        }
        self.tick_bridges(host);
        self.spent.clear();
        self.deposited.clear();
        let due: Vec<Id> = self
            .restores
            .iter()
            .filter(|(at, _)| *at <= now)
            .map(|(_, id)| *id)
            .collect();
        self.restores.retain(|(at, _)| *at > now);
        for id in due {
            host.restore_state(id);
        }
        for i in 0..self.arenas.len() {
            self.tick_arena(host, i);
        }
        self.tick_bots(host);
        if now.is_multiple_of(SECOND) {
            self.sidebar(host);
        }
    }

    fn tick_arena(&mut self, host: &mut dyn Host, i: usize) {
        let now = self.now;
        let settings = self.settings.clone();
        let arena = &mut self.arenas[i];
        if matches!(arena.phase, Phase::Waiting { .. })
            && now.is_multiple_of(SECOND)
            && !arena.intact(host)
        {
            arena.rebuild(&mut self.stale_shops);
            arena.tell_all(host, &text!("The arena was reset; rebuilding it.".yellow()));
            return;
        }
        match &mut arena.phase {
            Phase::Building => {
                if arena.work(host, settings.writes_per_tick) {
                    arena.phase = Phase::Waiting { start_at: None };
                    host.mark_dirty(arena.number, false);
                    host.clear_shopkeepers(&arena.dimension, arena.map.bounds);
                    for at in arena.map.shop_spots() {
                        let snbt = format!(
                            "{{NoAI:1b,Invulnerable:1b,Silent:1b,Tags:[\"{SHOPKEEPER_TAG}\"],\
                             CustomName:'\"Item Shop\"',CustomNameVisible:1b}}"
                        );
                        if let Some(id) = host.launch(&arena.dimension, SHOPKEEPER_KIND, at, &snbt)
                        {
                            arena.shops.push(id);
                        }
                    }
                    let spot = arena.waiting_spot();
                    for who in &arena.queue {
                        host.teleport(who.id, &arena.dimension, spot, 0.0);
                    }
                }
            }
            Phase::Waiting { start_at } => {
                let queued = arena.queue.len();
                let full = queued >= arena.map.capacity();
                match *start_at {
                    None if queued >= settings.min_players.max(2) => {
                        let wait = if full {
                            settings.countdown.min(10 * SECOND)
                        } else {
                            settings.countdown
                        };
                        *start_at = Some(now + wait);
                        let line = text!(
                            "The game starts in ".yellow(),
                            (wait / SECOND).to_string().red(),
                            " seconds!".yellow()
                        );
                        arena.tell_all(host, &line);
                    }
                    Some(_) if queued < settings.min_players.max(2) => {
                        *start_at = None;
                        arena.tell_all(
                            host,
                            &text!("Not enough players; the countdown stopped.".red()),
                        );
                    }
                    Some(at) if now >= at => self.begin(host, i),
                    Some(at) => {
                        let left = at - now;
                        if full && left > 10 * SECOND {
                            *start_at = Some(now + 10 * SECOND);
                        }
                        let seconds = left / SECOND;
                        if left.is_multiple_of(SECOND)
                            && (seconds == 10 || (1..=5).contains(&seconds))
                        {
                            let line = text!(
                                "The game starts in ".yellow(),
                                seconds.to_string().red(),
                                " seconds!".yellow()
                            );
                            arena.tell_all(host, &line);
                        }
                    }
                    None => {}
                }
            }
            Phase::Running(_) => self.tick_game(host, i),
            Phase::Ending { until, game } => {
                if now >= *until {
                    for id in game.members.keys() {
                        host.restore_state(*id);
                    }
                    let mut placed: Vec<(Pos, String)> = game
                        .placed
                        .iter()
                        .map(|&at| (at, "minecraft:air".to_owned()))
                        .collect();
                    placed.sort();
                    arena.jobs.push_back(Job::Write {
                        blocks: placed,
                        next: 0,
                    });
                    arena.jobs.push_back(Job::Write {
                        blocks: arena.map.blocks(),
                        next: 0,
                    });
                    arena.phase = Phase::Building;
                }
            }
        }
    }

    fn tick_game(&mut self, host: &mut dyn Host, i: usize) {
        let now = self.now;
        let arena = &mut self.arenas[i];
        let Phase::Running(game) = &mut arena.phase else {
            return;
        };
        let mut stages = Vec::new();
        while let Some(&(at, stage)) = TIMELINE.get(game.passed) {
            if now - game.started < at {
                break;
            }
            game.passed += 1;
            stages.push(stage);
        }
        let dimension = arena.dimension.clone();
        for g in &mut game.generators {
            if now < g.next {
                continue;
            }
            let interval = match g.team {
                Some(t) => economy::forge_interval(g.resource, game.teams[t].forge),
                None => Some(economy::generator_interval(
                    g.resource,
                    economy::tier(g.resource, game.passed),
                )),
            };
            let Some(interval) = interval else {
                g.next = now + SECOND;
                continue;
            };
            g.next = now + interval;
            if host.items_near(&dimension, g.at, g.resource.item()) < g.resource.cap() {
                host.spawn_item(&dimension, g.at, g.resource.item());
            }
        }
        let mut back = Vec::new();
        for (id, m) in &mut game.members {
            if let Life::Respawning { at } = m.life
                && now >= at
            {
                m.life = Life::Alive;
                back.push((*id, m.team));
            }
        }
        for (id, m) in &game.members {
            if let Life::Respawning { at } = m.life
                && (at - now).is_multiple_of(SECOND)
            {
                let line = text!(
                    "Respawning in ".yellow(),
                    ((at - now) / SECOND).to_string().red(),
                    "...".yellow()
                );
                host.action_bar(*id, &line);
            }
        }
        for (id, team) in back {
            let t = &arena.map.teams[team];
            host.spectate(id, false);
            host.teleport(id, &dimension, center(t.spawn), t.yaw);
            host.title(id, &text!("RESPAWNED!".green().bold()), "");
            host.tell(id, &text!("You have respawned!".yellow()));
        }
        for stage in stages {
            self.stage(host, i, stage);
        }
        self.upkeep(host, i);
    }

    fn stage(&mut self, host: &mut dyn Host, i: usize, stage: Stage) {
        let arena = &mut self.arenas[i];
        let Phase::Running(game) = &mut arena.phase else {
            return;
        };
        let line = match stage {
            Stage::DiamondII | Stage::DiamondIII => text!(
                "Diamond Generators".aqua(),
                " have been upgraded to Tier ".yellow(),
                if stage == Stage::DiamondII {
                    "II"
                } else {
                    "III"
                }
                .red()
            ),
            Stage::EmeraldII | Stage::EmeraldIII => text!(
                "Emerald Generators".dark_green(),
                " have been upgraded to Tier ".yellow(),
                if stage == Stage::EmeraldII {
                    "II"
                } else {
                    "III"
                }
                .red()
            ),
            Stage::BedGone => {
                for (t, team) in arena.map.teams.iter().enumerate() {
                    if std::mem::replace(&mut game.teams[t].bed, false) {
                        for (at, _) in team.bed_blocks() {
                            host.set_block(&arena.dimension, at, "minecraft:air");
                        }
                    }
                }
                text!("All beds have been destroyed!".red().bold())
            }
            Stage::SuddenDeath => text!("Sudden Death!".red().bold()),
            Stage::GameEnd => {
                arena.tell_all(
                    host,
                    &text!("Time is up: the game ends in a draw.".yellow()),
                );
                self.finish(host, i, None);
                return;
            }
        };
        arena.tell_all(host, &line);
    }

    /// Team `team` may be out of players; then the game may be decided.
    fn team_check(&mut self, host: &mut dyn Host, i: usize, team: usize) {
        let arena = &mut self.arenas[i];
        let Phase::Running(game) = &mut arena.phase else {
            return;
        };
        let alive = game.alive_teams();
        if !alive.contains(&team) {
            let line = text!(
                "TEAM ELIMINATED > ".bold(),
                arena.team_name(team),
                " has been eliminated!".red()
            );
            arena.tell_all(host, &line);
        }
        if alive.len() <= 1 {
            let winner = alive.into_iter().next();
            self.finish(host, i, winner);
        }
    }

    fn finish(&mut self, host: &mut dyn Host, i: usize, winner: Option<usize>) {
        let until = self.now + self.settings.end_delay;
        let arena = &mut self.arenas[i];
        let Phase::Running(game) = std::mem::replace(&mut arena.phase, Phase::Building) else {
            return;
        };
        let line = match winner {
            Some(t) => {
                let names: Vec<&str> = game
                    .members
                    .values()
                    .filter(|m| m.team == t)
                    .map(|m| m.name.as_str())
                    .collect();
                text!(
                    "VICTORY! ".gold().bold(),
                    arena.team_name(t),
                    " wins: ".yellow(),
                    names.join(", ").gray()
                )
            }
            None => text!("GAME OVER: nobody wins.".yellow()),
        };
        for (id, m) in &game.members {
            let (title, sub) = match winner {
                Some(t) if m.team == t => (text!("VICTORY!".gold().bold()), String::new()),
                Some(_) => (text!("GAME OVER".red().bold()), String::new()),
                None => (text!("DRAW".yellow().bold()), String::new()),
            };
            host.title(*id, &title, &sub);
        }
        host.unprotect(arena.number);
        let mut game = game;
        game.ender.clear();
        game.chests.iter_mut().for_each(Stash::clear);
        arena.phase = Phase::Ending { until, game };
        arena.tell_all(host, &line);
    }

    /// The shared sidebar for the first arena with a game, or none.
    fn sidebar(&self, host: &mut dyn Host) {
        let shown = self
            .arenas
            .iter()
            .find_map(|a| a.game().map(|game| (a, game)));
        let Some((arena, game)) = shown else {
            host.sidebar(None);
            return;
        };
        let elapsed = self.now - game.started;
        let mut lines = Vec::new();
        if let Some((at, stage)) = TIMELINE.get(game.passed) {
            lines.push(text!(
                stage.title().white(),
                " in ".white(),
                economy::clock(at.saturating_sub(elapsed)).green()
            ));
        }
        lines.push(String::new());
        for (t, team) in arena.map.teams.iter().enumerate() {
            let alive = game
                .members
                .values()
                .filter(|m| m.team == t && m.life != Life::Out)
                .count();
            let status = if game.teams[t].bed {
                text!("Y".green())
            } else if alive > 0 {
                text!(alive.to_string().green())
            } else {
                text!("X".red())
            };
            lines.push(text!(
                team.name.as_str().color(team.chat_color()),
                ": ".white(),
                status
            ));
        }
        host.sidebar(Some((&text!("BED WARS".yellow().bold()), &lines)));
    }

    /// A player died in a dimension.
    pub fn died(&mut self, host: &mut dyn Host, id: Id) {
        let now = self.now;
        let respawn = self.settings.respawn;
        let Some(i) = self.arenas.iter().position(|a| {
            matches!(&a.phase, Phase::Running(g)
                if g.members.get(&id).is_some_and(|m| m.life == Life::Alive))
        }) else {
            return;
        };
        let arena = &mut self.arenas[i];
        let Phase::Running(game) = &mut arena.phase else {
            return;
        };
        let member = game.members.get_mut(&id).expect("found");
        let team = member.team;
        let name = member.name.clone();
        if game.teams[team].bed {
            member.life = Life::Respawning { at: now + respawn };
            arena.tell_all(host, &text!(name.as_str().gray(), " died.".gray()));
            let line = text!(
                "You will respawn in ".yellow(),
                (respawn / SECOND).to_string().red(),
                " seconds!".yellow()
            );
            host.tell(id, &line);
            let sub = text!(
                "You will respawn in ".yellow(),
                (respawn / SECOND).to_string().red(),
                " seconds".yellow()
            );
            host.spectate(id, true);
            host.title(id, &text!("YOU DIED!".red().bold()), &sub);
            return;
        }
        let line = text!(
            name.as_str().gray(),
            " died. ".gray(),
            "FINAL KILL!".aqua().bold()
        );
        member.life = Life::Out;
        arena.tell_all(host, &line);
        host.tell(id, &text!("You have been eliminated!".red()));
        host.spectate(id, true);
        host.title(
            id,
            &text!("YOU DIED!".red().bold()),
            &text!("You have been eliminated".gray()),
        );
        self.restores.push((now + respawn, id));
        self.team_check(host, i, team);
    }

    /// A player broke a block (`state` is what it was).
    pub fn broken(
        &mut self,
        host: &mut dyn Host,
        who: Option<&Who>,
        dimension: &str,
        at: Pos,
        state: &str,
    ) {
        let Some(i) = self.in_dimension(dimension) else {
            return;
        };
        let arena = &mut self.arenas[i];
        let dim = arena.dimension.clone();
        let restore = |host: &mut dyn Host, why: &str| {
            host.set_block(&dim, at, state);
            host.sweep(&dim, at, item_of(state));
            if let Some(who) = who {
                host.tell(who.id, &text!(why.red()));
            }
        };
        let Phase::Running(game) = &mut arena.phase else {
            restore(host, "You can't break blocks here!");
            return;
        };
        if let Some(t) = arena.map.bed_owner(at) {
            if !game.teams[t].bed {
                return;
            }
            let team = &arena.map.teams[t];
            let breaker = who.and_then(|w| game.members.get(&w.id).map(|m| (w, m)));
            match breaker {
                Some((w, m)) if m.life == Life::Alive && m.team != t => {
                    game.teams[t].bed = false;
                    for (half, _) in team.bed_blocks() {
                        host.set_block(&dim, half, "minecraft:air");
                    }
                    host.sweep(&dim, at, &team.bed_item());
                    let line = text!(
                        "BED DESTRUCTION > ".bold(),
                        team.name.as_str().color(team.chat_color()),
                        " Bed".color(team.chat_color()),
                        " was destroyed by ".gray(),
                        w.name.as_str().color(arena.map.teams[m.team].chat_color())
                    );
                    let victims: Vec<Id> = game
                        .members
                        .iter()
                        .filter(|(_, v)| v.team == t && v.life != Life::Out)
                        .map(|(id, _)| *id)
                        .collect();
                    arena.tell_all(host, &line);
                    for id in victims {
                        host.title(
                            id,
                            &text!("BED DESTROYED!".red().bold()),
                            &text!("You will no longer respawn!".white()),
                        );
                        host.tell(
                            id,
                            &text!("Your bed was destroyed! You will no longer respawn!".red()),
                        );
                    }
                }
                _ => {
                    for (half, bed) in team.bed_blocks() {
                        host.set_block(&dim, half, &bed);
                    }
                    host.sweep(&dim, at, &team.bed_item());
                    if let Some(w) = who {
                        host.tell(w.id, &text!("You can't break your own bed!".red()));
                    }
                }
            }
            return;
        }
        if !game.placed.remove(&at) {
            restore(host, "You can only break blocks placed by a player!");
        }
    }

    /// A player placed a block (`state` is the block placed).
    pub fn placed(
        &mut self,
        host: &mut dyn Host,
        who: Option<&Who>,
        dimension: &str,
        at: Pos,
        state: &str,
    ) {
        let Some(i) = self.in_dimension(dimension) else {
            return;
        };
        let arena = &mut self.arenas[i];
        let playing = |game: &Match| {
            who.and_then(|w| game.members.get(&w.id))
                .is_some_and(|m| m.life == Life::Alive)
        };
        let why = match &mut arena.phase {
            Phase::Running(game)
                if playing(game) && arena.map.buildable(at) && !chests::is_chest(state) =>
            {
                if item_of(state) == "minecraft:tnt" {
                    explosives::prime(host, &arena.dimension, at, who.map_or(0, |w| w.id));
                } else {
                    game.placed.insert(at);
                }
                return;
            }
            Phase::Running(game) if playing(game) => "You can't place blocks here!",
            _ => "You can't build here!",
        };
        host.set_block(&arena.dimension, at, "minecraft:air");
        if let Some(w) = who {
            host.give_back(item_of(state));
            host.tell(w.id, &text!(why.red()));
        }
    }

    /// One line per arena.
    pub fn list(&self) -> Vec<String> {
        self.arenas.iter().map(|a| self.summary(a)).collect()
    }

    fn summary(&self, a: &Arena) -> String {
        let state = match &a.phase {
            Phase::Building => "building".gold(),
            Phase::Waiting { start_at: None } => "waiting".green(),
            Phase::Waiting { start_at: Some(at) } => format!(
                "starting in {}s",
                at.saturating_sub(self.now).div_ceil(SECOND)
            )
            .green(),
            Phase::Running(g) => format!("running {}", economy::clock(self.now - g.started)).red(),
            Phase::Ending { .. } => "ending".gray(),
        };
        let players = match a.game() {
            Some(g) => g.members.values().filter(|m| m.life != Life::Out).count(),
            None => a.queue.len(),
        };
        text!(
            "Arena ".yellow(),
            a.number.to_string().aqua(),
            " ",
            a.map.name.as_str().white(),
            ": ".yellow(),
            state,
            format!(" {players}/{}", a.map.capacity()).gray()
        )
    }

    /// Details of arena `number`.
    pub fn status(&self, number: usize) -> Result<Vec<String>, String> {
        let a = self
            .arena(number)
            .ok_or(format!("There is no arena {number}"))?;
        let mut out = vec![self.summary(a)];
        if let Some(game) = a.game() {
            for (t, team) in a.map.teams.iter().enumerate() {
                let members: Vec<&Member> = game.members.values().filter(|m| m.team == t).collect();
                if members.is_empty() {
                    continue;
                }
                let bed = if game.teams[t].bed {
                    "bed".green()
                } else {
                    "no bed".red()
                };
                let names: Vec<String> = members
                    .iter()
                    .map(|m| match m.life {
                        Life::Out => text!(m.name.as_str().dark_gray().strikethrough()),
                        _ => m.name.clone(),
                    })
                    .collect();
                out.push(text!(
                    " ",
                    team.name.as_str().color(team.chat_color()),
                    " (",
                    bed,
                    "): ",
                    names.join(", ")
                ));
            }
            if let Some(&(at, stage)) = TIMELINE.get(game.passed) {
                let left = (game.started + at).saturating_sub(self.now);
                out.push(text!(
                    " Next: ".gray(),
                    stage.title().white(),
                    " in ".gray(),
                    economy::clock(left).white()
                ));
            }
        } else if !a.queue.is_empty() {
            let names: Vec<&str> = a.queue.iter().map(|w| w.name.as_str()).collect();
            out.push(text!(" Queue: ".gray(), names.join(", ")));
        }
        Ok(out)
    }

    /// The color of `id`'s team, for chat.
    pub fn team_color(&self, id: Id) -> Option<Color> {
        let arena = self.arena_of(id)?;
        let member = arena.game()?.members.get(&id)?;
        Some(arena.map.teams[member.team].chat_color())
    }
}

#[cfg(test)]
mod tests;
