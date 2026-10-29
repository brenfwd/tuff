// SPDX-License-Identifier: MIT
//! The bot registry, ownership, process manager and tick driver, free of
//! host calls: [`Bots`] drives bodies through a [`Host`].

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::build::Builder;
use crate::combat::{Combat, Fighter, Me, Swing, Target, line_of_sight};
use crate::exec::{Action, Drive, Executor, Face, Kit, Step, View};
use crate::farm::{Farmer, Farming};
use crate::flight::{Body as FlightBody, Fly, Run};
use crate::follow::{Explorer, Follower, Tracking};
use crate::hands::{Breaker, Carried, HOTBAR, Hands, Placer};
use crate::mine::{EXPLORE_RADIUS, Miner, Mining};
use crate::path::{Pos, Tools};
use crate::ranged::{Archer, Draw, ItemUser, Shot, Use, UseAction};
use crate::upkeep::{Eat, Eater};
use crate::world::{Blocks, name_of};
use tuff_bots_api::control::{
    AttackSpec, BotId, BotState, ControlError, Goal, LeafGoal, LookTarget, Policy, Process,
    RespawnPolicy, Skill, SpawnSpec, Vec3, XzGoal,
};

/// A body the host keeps for a bot: a fake player's uuid.
pub type Body = u128;

pub const MAX_BOTS_PER_OWNER: usize = 64;
const OWNER_CHECK_TICKS: u64 = 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Perceived {
    pub pos: Vec3,
    pub health: f32,
    pub alive: bool,
    pub yaw: f32,
    pub on_ground: bool,
    pub in_fluid: bool,
    pub food: i32,
    pub hotbar: Vec<Option<String>>,
    pub selected: u8,
}

impl Perceived {
    pub fn standing(pos: Vec3) -> Self {
        Self {
            pos,
            health: 20.0,
            alive: true,
            yaw: 0.0,
            on_ground: true,
            in_fluid: false,
            food: 20,
            hotbar: Vec::new(),
            selected: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Spawned,
    Died(String),
    Respawned,
    ProcessDone,
    ProcessFailed(String),
    Removed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    Gone,
}

/// What a target lookup found.
pub enum Lookup {
    Found(Target),
    Gone,
    Unknown,
}

impl Lookup {
    pub fn found(self) -> Option<Target> {
        match self {
            Lookup::Found(target) => Some(target),
            Lookup::Gone | Lookup::Unknown => None,
        }
    }
}

pub trait Host {
    fn spawn(&mut self, spec: &SpawnSpec) -> Result<Body, ControlError>;
    fn remove(&mut self, body: Body);
    /// `None` when the body no longer exists.
    fn perceive(&mut self, body: Body) -> Option<Perceived>;
    fn respawn(&mut self, body: Body);
    /// Holds these keys and this look until the next call.
    fn drive(&mut self, body: Body, drive: Drive);
    /// The blocks around the body, `None` when they cannot be read.
    fn blocks(&mut self, body: Body) -> Option<&mut dyn Blocks>;
    /// Starts digging the block at this position.
    fn dig(&mut self, _body: Body, _pos: (i32, i32, i32), _from: &Vec3) {}
    /// Selects a hotbar slot (0 to 8).
    fn select_slot(&mut self, _body: Body, _slot: u8) {}
    /// Uses the held item on a face of a block.
    fn place(&mut self, _body: Body, _against: Pos, _face: Face) {}
    /// Uses the held item on a side face at a cursor height (0 to 1).
    fn place_at(&mut self, body: Body, against: Pos, face: Face, _cursor_y: f32) {
        self.place(body, against, face);
    }
    /// What the body carries: menu slot, item and count.
    fn inventory(&mut self, _body: Body) -> Vec<Carried> {
        Vec::new()
    }
    /// How the body breaks blocks with its hotbar.
    fn tools(&mut self, _body: Body) -> Option<Rc<RefCell<dyn Tools>>> {
        None
    }
    /// Where an online player is, `None` when they are gone or in
    /// another dimension than the body.
    fn locate(&mut self, body: Body, target: &str) -> Option<Vec3>;
    /// The player or entity with this uuid (32 hex digits): `Gone` when it
    /// is gone or in another dimension than the body, `Unknown` when the
    /// server could not tell right now.
    fn target(&mut self, _body: Body, _uuid: &str) -> Lookup {
        Lookup::Gone
    }
    /// Swings at the target, hitting it when the server finds it in reach.
    fn attack(&mut self, _body: Body, _uuid: &str) {}
    /// Swings at the air.
    fn swing(&mut self, _body: Body) {}
    fn select(&mut self, _body: Body, _slot: u8) {}
    /// Turns the held look, yaw and pitch in degrees.
    fn look(&mut self, _body: Body, _yaw: f32, _pitch: f32) {}
    /// Starts using the held item (eating).
    fn use_held(&mut self, _body: Body) {}
    /// Lets go of the use key.
    fn stop_using(&mut self, _body: Body) {}
    fn owner_loaded(&self, owner: &str) -> bool;
    fn notify(&mut self, owner: &str, bot: BotId, event: Event) -> Delivery;
}

struct Bot {
    owner: String,
    body: Body,
    skill: Skill,
    policy: Policy,
    respawn: RespawnPolicy,
    process: Process,
    last: Perceived,
    dead_ticks: u32,
    exec: Option<Executor>,
    exec_goal: Option<Goal>,
    follower: Follower,
    explorer: Explorer,
    miner: Option<Miner>,
    farmer: Option<Farmer>,
    hands: Option<HandWork>,
    fighter: Option<Fighter>,
    archer: Option<Archer>,
    item_user: Option<ItemUser>,
    flight: Option<Run>,
    columns: BTreeMap<(i32, i32), i32>,
    eater: Eater,
    held: Option<Drive>,
}

enum HandWork {
    Break(Breaker),
    Place(Placer),
    Build(Builder),
}

fn me_of(seen: &Perceived, policy: &Policy) -> Me {
    Me {
        pos: seen.pos.clone(),
        yaw: seen.yaw,
        on_ground: seen.on_ground,
        health: seen.health,
        food: seen.food,
        allow_sprint: policy.allow_sprint,
        hotbar: seen.hotbar.clone(),
        selected: seen.selected,
    }
}

/// The throwaway blocks a policy lets the bot use from its hotbar.
pub fn kit_for(policy: &Policy, carried: &[Carried]) -> Kit {
    let mut kit = Kit::default();
    for (slot, item, count) in carried {
        let usable = HOTBAR.contains(slot)
            && policy
                .throwaway_items
                .iter()
                .any(|t| name_of(t) == name_of(item));
        if usable && *count > 0 {
            kit.throwaway += count;
            kit.throwaway_slot.get_or_insert(slot - HOTBAR.start());
        }
    }
    kit
}

const CHEST_SLOT: u8 = 6;

fn perform(host: &mut impl Host, body: Body, at: &Vec3, actions: Vec<Action>) {
    for action in actions {
        match action {
            Action::Select(slot) => host.select_slot(body, slot),
            Action::Dig(pos) => host.dig(body, pos, at),
            Action::Place { against, face } => host.place(body, against, face),
            Action::PlaceAt {
                against,
                face,
                cursor_y,
            } => host.place_at(body, against, face, cursor_y),
            Action::Look { yaw, pitch } => host.look(body, yaw, pitch),
        }
    }
}

#[derive(Default)]
pub struct Bots {
    next: BotId,
    bots: BTreeMap<BotId, Bot>,
    ticks: u64,
}

fn miner_for(process: &Process, at: &Vec3) -> Miner {
    match process {
        Process::Mine(s) => Miner::new(&s.blocks, s.count, s.radius, true, at),
        Process::GetToBlock(s) => Miner::new(&s.blocks, 1, s.radius, false, at),
        _ => unreachable!("only mining processes make a miner"),
    }
}

fn floor(v: f64) -> i32 {
    v.floor() as i32
}

pub fn goal_reached(goal: &Goal, pos: &Vec3) -> bool {
    match goal {
        Goal::Leaf(leaf) => leaf_reached(leaf, pos),
        Goal::AnyOf(leaves) => leaves.iter().any(|leaf| leaf_reached(leaf, pos)),
    }
}

fn leaf_reached(goal: &LeafGoal, pos: &Vec3) -> bool {
    let (bx, by, bz) = (floor(pos.x), floor(pos.y), floor(pos.z));
    match goal {
        LeafGoal::Block(p) => (bx, by, bz) == (p.x, p.y, p.z),
        LeafGoal::Near(n) => {
            let dx = pos.x - (f64::from(n.pos.x) + 0.5);
            let dy = pos.y - f64::from(n.pos.y);
            let dz = pos.z - (f64::from(n.pos.z) + 0.5);
            dx * dx + dy * dy + dz * dz <= n.radius * n.radius
        }
        LeafGoal::Xz(g) => (bx, bz) == (g.x, g.z),
        LeafGoal::YLevel(y) => by == *y,
        LeafGoal::GetToBlock(p) => (bx - p.x).abs() + (by - p.y).abs() + (bz - p.z).abs() == 1,
        LeafGoal::RunAway(r) => {
            let dx = pos.x - (f64::from(r.origin.x) + 0.5);
            let dz = pos.z - (f64::from(r.origin.z) + 0.5);
            dx * dx + dz * dz >= r.distance * r.distance
        }
    }
}

impl Bots {
    pub fn len(&self) -> usize {
        self.bots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bots.is_empty()
    }

    fn owned(&mut self, owner: &str, bot: BotId) -> Result<&mut Bot, ControlError> {
        match self.bots.get_mut(&bot) {
            None => Err(ControlError::UnknownBot),
            Some(b) if b.owner != owner => Err(ControlError::NotOwner),
            Some(b) => Ok(b),
        }
    }

    pub fn spawn(
        &mut self,
        host: &mut impl Host,
        owner: &str,
        spec: SpawnSpec,
    ) -> Result<BotId, ControlError> {
        if self.bots.values().filter(|b| b.owner == owner).count() >= MAX_BOTS_PER_OWNER {
            return Err(ControlError::LimitReached);
        }
        let body = host.spawn(&spec)?;
        self.next += 1;
        let id = self.next;
        self.bots.insert(
            id,
            Bot {
                owner: owner.to_owned(),
                body,
                skill: spec.skill,
                policy: spec.policy,
                respawn: spec.respawn,
                process: Process::Idle,
                last: Perceived::standing(spec.location.pos),
                dead_ticks: 0,
                exec: None,
                exec_goal: None,
                follower: Follower::default(),
                explorer: Explorer::default(),
                miner: None,
                farmer: None,
                hands: None,
                fighter: None,
                archer: None,
                item_user: None,
                flight: None,
                columns: BTreeMap::new(),
                eater: Eater::default(),
                held: None,
            },
        );
        self.send(host, owner, id, Event::Spawned);
        Ok(id)
    }

    pub fn remove(
        &mut self,
        host: &mut impl Host,
        owner: &str,
        bot: BotId,
    ) -> Result<(), ControlError> {
        self.owned(owner, bot)?;
        let gone = self.bots.remove(&bot).expect("checked above");
        host.remove(gone.body);
        self.send(host, owner, bot, Event::Removed);
        Ok(())
    }

    pub fn list(&self, owner: &str) -> Vec<BotId> {
        self.bots
            .iter()
            .filter(|(_, b)| b.owner == owner)
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn state(&mut self, owner: &str, bot: BotId) -> Result<BotState, ControlError> {
        let b = self.owned(owner, bot)?;
        Ok(BotState {
            position: b.last.pos.clone(),
            health: b.last.health,
            alive: b.last.alive,
            process: b.process.clone(),
            inventory: Vec::new(),
        })
    }

    pub fn set_skill(&mut self, owner: &str, bot: BotId, skill: Skill) -> Result<(), ControlError> {
        self.owned(owner, bot)?.skill = skill;
        Ok(())
    }

    pub fn skill(&mut self, owner: &str, bot: BotId) -> Result<Skill, ControlError> {
        Ok(self.owned(owner, bot)?.skill.clone())
    }

    pub fn set_policy(
        &mut self,
        owner: &str,
        bot: BotId,
        policy: Policy,
    ) -> Result<(), ControlError> {
        let b = self.owned(owner, bot)?;
        b.policy = policy;
        b.exec = None;
        Ok(())
    }

    pub fn policy(&mut self, owner: &str, bot: BotId) -> Result<Policy, ControlError> {
        Ok(self.owned(owner, bot)?.policy.clone())
    }

    /// Replaces the active process; the replaced one reports nothing.
    pub fn set_process(
        &mut self,
        owner: &str,
        bot: BotId,
        process: Process,
    ) -> Result<(), ControlError> {
        let b = self.owned(owner, bot)?;
        b.process = process;
        b.exec = None;
        b.exec_goal = None;
        b.follower = Follower::default();
        b.explorer.forget_target();
        b.miner = None;
        b.farmer = None;
        b.hands = None;
        b.fighter = None;
        b.archer = None;
        b.item_user = None;
        b.flight = None;
        b.columns.clear();
        Ok(())
    }

    /// Blocks changed somewhere: every running goto checks its path ahead.
    pub fn mark_dirty(&mut self) {
        for exec in self.bots.values_mut().filter_map(|b| b.exec.as_mut()) {
            exec.mark_dirty();
        }
    }

    pub fn stop(&mut self, owner: &str, bot: BotId) -> Result<(), ControlError> {
        self.set_process(owner, bot, Process::Idle)
    }

    pub fn respawn(
        &mut self,
        host: &mut impl Host,
        owner: &str,
        bot: BotId,
    ) -> Result<(), ControlError> {
        let b = self.owned(owner, bot)?;
        if b.last.alive {
            return Err(ControlError::Invalid("the bot is alive".into()));
        }
        host.respawn(b.body);
        Ok(())
    }

    fn send(&mut self, host: &mut impl Host, owner: &str, bot: BotId, event: Event) {
        if host.notify(owner, bot, event) == Delivery::Gone && !host.owner_loaded(owner) {
            self.drop_owner(host, owner);
        }
    }

    fn drop_owner(&mut self, host: &mut impl Host, owner: &str) {
        let ids = self.list(owner);
        for id in ids {
            if let Some(gone) = self.bots.remove(&id) {
                host.remove(gone.body);
            }
        }
    }

    /// One server tick: owner checks once per second, then per bot
    /// perceive, life-cycle events and the active process.
    pub fn tick(&mut self, host: &mut impl Host) {
        self.ticks += 1;
        if self.ticks.is_multiple_of(OWNER_CHECK_TICKS) {
            let owners: BTreeSet<String> = self.bots.values().map(|b| b.owner.clone()).collect();
            for owner in owners {
                if !host.owner_loaded(&owner) {
                    self.drop_owner(host, &owner);
                }
            }
        }
        let ids: Vec<BotId> = self.bots.keys().copied().collect();
        for id in ids {
            self.tick_bot(host, id);
        }
    }

    fn tick_bot(&mut self, host: &mut impl Host, id: BotId) {
        let Some(bot) = self.bots.get_mut(&id) else {
            return;
        };
        let owner = bot.owner.clone();
        let Some(seen) = host.perceive(bot.body) else {
            self.bots.remove(&id);
            self.send(host, &owner, id, Event::Removed);
            return;
        };
        let was_alive = bot.last.alive;
        bot.last = seen.clone();
        if !seen.alive {
            bot.dead_ticks += 1;
            bot.exec = None;
            bot.held = None;
            let failed = !matches!(bot.process, Process::Idle);
            if failed {
                bot.process = Process::Idle;
            }
            let auto = matches!(bot.respawn, RespawnPolicy::Auto(n) if bot.dead_ticks >= n);
            let body = bot.body;
            if auto {
                host.respawn(body);
            }
            if was_alive {
                self.send(host, &owner, id, Event::Died("unknown".into()));
                if failed {
                    self.send(host, &owner, id, Event::ProcessFailed("died".into()));
                }
            }
            return;
        }
        bot.dead_ticks = 0;
        let mut outcome = None;
        let mut goal = None;
        let mut dig = None;
        let mut acts = Vec::new();
        let placing = matches!(bot.process, Process::Place(_) | Process::Build(_));
        let breaking =
            bot.policy.allow_break || matches!(bot.process, Process::Break(_) | Process::Build(_));
        let farming = matches!(bot.process, Process::Farm(_));
        let flying = matches!(bot.process, Process::Fly(_));
        let carried = if bot.policy.allow_place
            || placing
            || farming
            || flying
            || breaking
            || bot.policy.auto_eat
        {
            host.inventory(bot.body)
        } else {
            Vec::new()
        };
        let kit = kit_for(&bot.policy, &carried);
        let fighting = matches!(bot.process, Process::Attack(_));
        let eat_enabled = bot.policy.auto_eat && !fighting;
        match bot.eater.tick(eat_enabled, seen.food, &carried) {
            Some(Eat::Start(slot)) => {
                host.select_slot(bot.body, slot);
                host.use_held(bot.body);
            }
            Some(Eat::Stop) => host.stop_using(bot.body),
            None => {}
        }
        let tools = if breaking { host.tools(bot.body) } else { None };
        let mut direct = None;
        let mut swing = None;
        let mut select = None;
        let mut look = None;
        let mut draw = Draw::None;
        let mut use_action = UseAction::None;
        let mut fight = match &bot.process {
            Process::Attack(spec) => Some(spec.clone()),
            _ => None,
        };
        match bot.process.clone() {
            Process::Shoot(spec) => 'hold: {
                let target = match host.target(bot.body, &spec.target) {
                    Lookup::Found(target) => Some(target),
                    Lookup::Gone => None,
                    Lookup::Unknown => break 'hold,
                };
                let visible = match (&target, host.blocks(bot.body)) {
                    (Some(t), Some(world)) => line_of_sight(world, &seen.pos, t),
                    (Some(_), None) => true,
                    (None, _) => false,
                };
                let me = me_of(&seen, &bot.policy);
                let ammo = host
                    .inventory(bot.body)
                    .iter()
                    .any(|(_, item, count)| *count > 0 && name_of(item).ends_with("arrow"));
                let archer = bot.archer.get_or_insert_with(|| Archer::new(u64::from(id)));
                match archer.tick(&me, target.as_ref(), visible, &bot.skill, &spec, ammo) {
                    Shot::Done => outcome = Some(Event::ProcessDone),
                    Shot::Failed(reason) => outcome = Some(Event::ProcessFailed(reason)),
                    Shot::Melee { release } => {
                        if release {
                            host.stop_using(bot.body);
                        }
                        fight = Some(AttackSpec {
                            target: spec.target.clone(),
                            until: spec.until.clone(),
                            retreat_health: 0.0,
                        });
                    }
                    Shot::Act {
                        yaw,
                        pitch,
                        draw: d,
                        select: slot,
                    } => {
                        direct = Some(Drive {
                            yaw,
                            ..Drive::default()
                        });
                        look = Some((yaw, pitch));
                        draw = d;
                        select = slot;
                    }
                }
            }
            Process::UseItem(spec) => {
                let still = Vec3 {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                };
                let aim = match &spec.look_at {
                    None => None,
                    Some(LookTarget::Position(p)) => Some((p.clone(), still)),
                    Some(LookTarget::Entity(uuid)) => {
                        host.target(bot.body, uuid).found().map(|t| {
                            let mut p = t.pos;
                            p.y += t.height * 0.6;
                            (p, still)
                        })
                    }
                };
                let me = me_of(&seen, &bot.policy);
                let user = bot.item_user.get_or_insert_with(ItemUser::new);
                match user.tick(&me, &spec, aim, &bot.skill) {
                    Use::Done => outcome = Some(Event::ProcessDone),
                    Use::Failed(reason) => outcome = Some(Event::ProcessFailed(reason)),
                    Use::Act {
                        look: l,
                        action,
                        select: slot,
                    } => {
                        look = l;
                        use_action = action;
                        select = slot;
                        direct = Some(Drive {
                            yaw: l.map_or(seen.yaw, |(yaw, _)| yaw),
                            ..Drive::default()
                        });
                    }
                }
            }
            _ => {}
        }
        match bot.process.clone() {
            Process::Idle => {}
            Process::Goto(g)
                if (seen.on_ground || seen.in_fluid) && goal_reached(&g, &seen.pos) =>
            {
                outcome = Some(Event::ProcessDone)
            }
            Process::Goto(g) => goal = Some(g),
            Process::Follow(spec) => match host.locate(bot.body, &spec.target) {
                None => outcome = Some(Event::ProcessFailed("target gone".into())),
                Some(target) => {
                    if let Tracking::Chase(g) = bot.follower.track(&seen.pos, &target, spec.range) {
                        goal = Some(g);
                    }
                }
            },
            p @ (Process::Mine(_) | Process::GetToBlock(_)) => {
                let miner = bot.miner.get_or_insert_with(|| miner_for(&p, &seen.pos));
                let step = match host.blocks(bot.body) {
                    Some(world) => miner.tick(&seen.pos, world),
                    None => Mining::Wait,
                };
                match step {
                    Mining::Done => outcome = Some(Event::ProcessDone),
                    Mining::Goal(g) => goal = Some(g),
                    Mining::Dig(pos) => dig = Some(pos),
                    Mining::Wait => {}
                    Mining::Explore => {
                        match bot.explorer.next(&seen.pos, miner.origin(), EXPLORE_RADIUS) {
                            None => outcome = Some(Event::ProcessFailed("none found".into())),
                            Some(g) => goal = Some(g),
                        }
                    }
                }
            }
            Process::Farm(spec) => {
                let farmer = bot.farmer.get_or_insert_with(|| Farmer::new(&spec));
                let step = match host.blocks(bot.body) {
                    Some(world) => farmer.tick(&seen.pos, world, &carried),
                    None => Farming::Wait,
                };
                match step {
                    Farming::Done => outcome = Some(Event::ProcessDone),
                    Farming::Failed(reason) => outcome = Some(Event::ProcessFailed(reason)),
                    Farming::Goal(g) => goal = Some(g),
                    Farming::Dig(pos) => dig = Some(pos),
                    Farming::Act(actions) => acts = actions,
                    Farming::Wait => {}
                }
            }
            p @ (Process::Break(_) | Process::Place(_) | Process::Build(_)) => {
                let hands = bot.hands.get_or_insert_with(|| match &p {
                    Process::Place(spec) => HandWork::Place(Placer::new(&spec.pos, &spec.item)),
                    Process::Break(targets) => HandWork::Break(Breaker::new(targets)),
                    Process::Build(spec) => HandWork::Build(Builder::new(spec)),
                    _ => unreachable!("only hand processes"),
                });
                let step = match host.blocks(bot.body) {
                    Some(world) => match hands {
                        HandWork::Break(b) => b.tick(&seen.pos, world, tools.as_ref()),
                        HandWork::Place(p) => {
                            p.grounded = seen.on_ground || seen.in_fluid;
                            p.tick(&seen.pos, world, &carried)
                        }
                        HandWork::Build(b) => {
                            b.grounded = seen.on_ground || seen.in_fluid;
                            b.tick(
                            &seen.pos,
                            world,
                            &carried,
                            tools.as_ref(),
                            bot.policy.allow_break,
                            &bot.policy.throwaway_items,
                            )
                        }
                    },
                    None => Hands::Wait,
                };
                match step {
                    Hands::Done => outcome = Some(Event::ProcessDone),
                    Hands::Failed(reason) => outcome = Some(Event::ProcessFailed(reason)),
                    Hands::Goal(g) => goal = Some(g),
                    Hands::Act(actions) => acts = actions,
                    Hands::Wait => {}
                }
            }
            Process::Attack(_) | Process::Shoot(_) | Process::UseItem(_) => 'hold: {
                if let Some(spec) = fight.take() {
                    let target = match host.target(bot.body, &spec.target) {
                        Lookup::Found(target) => Some(target),
                        Lookup::Gone => None,
                        Lookup::Unknown => break 'hold,
                    };
                    let visible = match (&target, host.blocks(bot.body)) {
                        (Some(t), Some(world)) => line_of_sight(world, &seen.pos, t),
                        (Some(_), None) => true,
                        (None, _) => false,
                    };
                    let me = me_of(&seen, &bot.policy);
                    let fighter = bot
                        .fighter
                        .get_or_insert_with(|| Fighter::new(u64::from(id)));
                    match fighter.tick(&me, target.as_ref(), visible, &bot.skill, &spec) {
                        Combat::Done => outcome = Some(Event::ProcessDone),
                        Combat::Failed(reason) => outcome = Some(Event::ProcessFailed(reason)),
                        Combat::Goal(g) => goal = Some(g),
                        Combat::Act {
                            drive,
                            swing: s,
                            select: slot,
                        } => {
                            direct = Some(drive);
                            swing = s.map(|s| (s, spec.target.clone()));
                            select = slot;
                        }
                    }
                }
            }
            Process::Fly(spec) => {
                let run = bot.flight.get_or_insert_with(|| {
                    let held: Vec<(u8, u32)> = carried
                        .iter()
                        .filter(|(_, item, _)| name_of(item) == "firework_rocket")
                        .map(|(slot, _, count)| (*slot, *count))
                        .collect();
                    let glider = carried
                        .iter()
                        .any(|(slot, item, _)| *slot == CHEST_SLOT && name_of(item) == "elytra");
                    let rocket = held
                        .iter()
                        .find(|(slot, _)| HOTBAR.contains(slot))
                        .map(|(slot, _)| slot - HOTBAR.start());
                    let owned = held
                        .iter()
                        .filter(|(slot, _)| HOTBAR.contains(slot))
                        .map(|(_, count)| count)
                        .sum::<u32>();
                    Run::new((spec.x, spec.z), spec.fireworks.min(owned), glider, rocket)
                });
                let body = FlightBody {
                    pos: [seen.pos.x, seen.pos.y, seen.pos.z],
                    velocity: [0.0; 3],
                    on_ground: seen.on_ground,
                    gliding: false,
                };
                let top = seen.pos.y as i32 + 16;
                let mut cache = std::mem::take(&mut bot.columns);
                let step = match host.blocks(bot.body) {
                    Some(world) => {
                        let mut ground = |x: i32, z: i32| {
                            if let Some(known) = cache.get(&(x, z)) {
                                return Some(*known);
                            }
                            for y in (top - 320..=top).rev() {
                                match world.class_at(x, y, z) {
                                    Ok(class) if !class.passable() => {
                                        cache.insert((x, z), y + 1);
                                        return Some(y + 1);
                                    }
                                    Ok(_) => {}
                                    Err(_) => return None,
                                }
                            }
                            None
                        };
                        run.tick(&body, seen.selected, &mut ground)
                    }
                    None => Fly::Act {
                        yaw: seen.yaw,
                        pitch: 0.0,
                        jump: false,
                        select: None,
                        fire: false,
                    },
                };
                bot.columns = cache;
                match step {
                    Fly::Failed(reason) => outcome = Some(Event::ProcessFailed(reason)),
                    Fly::Land => {
                        let to = Goal::Leaf(LeafGoal::Xz(XzGoal {
                            x: spec.x,
                            z: spec.z,
                        }));
                        bot.flight = None;
                        bot.process = Process::Goto(to.clone());
                        goal = Some(to);
                    }
                    Fly::Act {
                        yaw,
                        pitch,
                        jump,
                        select: slot,
                        fire,
                    } => {
                        direct = Some(Drive {
                            jump,
                            yaw,
                            pitch,
                            ..Drive::default()
                        });
                        select = slot;
                        if fire {
                            use_action = UseAction::Press;
                        }
                    }
                }
            }
            Process::Explore(spec) => {
                match bot.explorer.next(&seen.pos, &spec.origin, spec.radius) {
                    None => outcome = Some(Event::ProcessDone),
                    Some(g) => goal = Some(g),
                }
            }
        }
        let active = goal.is_some() || direct.is_some();
        if let Some(slot) = select {
            host.select(bot.body, slot);
        }
        if let Some(drive) = direct
            && bot.held != Some(drive)
        {
            host.drive(bot.body, drive);
            bot.held = Some(drive);
        }
        if let Some((yaw, pitch)) = look {
            host.look(bot.body, yaw, pitch);
        }
        match (draw, use_action) {
            (Draw::Start, _) | (_, UseAction::Press) => host.use_held(bot.body),
            (Draw::Release, _) | (_, UseAction::Release) => host.stop_using(bot.body),
            _ => {}
        }
        match swing {
            Some((Swing::Hit, target)) => host.attack(bot.body, &target),
            Some((Swing::Miss, _)) => host.swing(bot.body),
            None => {}
        }
        if let Some(goal) = goal {
            if bot.exec_goal.as_ref() != Some(&goal) {
                bot.exec = None;
                bot.exec_goal = Some(goal.clone());
            }
            let exec = bot
                .exec
                .get_or_insert_with(|| Executor::new(goal, &bot.policy, tools.clone(), kit));
            exec.set_kit(kit);
            let view = View {
                pos: seen.pos.clone(),
                yaw: seen.yaw,
                on_ground: seen.on_ground,
                in_fluid: seen.in_fluid,
                food: seen.food,
            };
            if let Some(world) = host.blocks(bot.body) {
                let step = exec.tick(&view, &bot.skill, world);
                acts.extend(exec.take_actions());
                let goto = matches!(bot.process, Process::Goto(_));
                match step {
                    Step::Drive(drive) => {
                        if bot.held != Some(drive) {
                            host.drive(bot.body, drive);
                            bot.held = Some(drive);
                        }
                    }
                    Step::Done if goto => outcome = Some(Event::ProcessDone),
                    Step::Failed(reason)
                        if !matches!(
                            bot.process,
                            Process::Explore(_)
                                | Process::Mine(_)
                                | Process::GetToBlock(_)
                                | Process::Farm(_)
                                | Process::Attack(_)
                        ) =>
                    {
                        outcome = Some(Event::ProcessFailed(reason));
                    }
                    Step::Failed(_) => {
                        bot.explorer.skip_current();
                        if let Some(miner) = bot.miner.as_mut() {
                            miner.skip_current();
                        }
                        if let Some(farmer) = bot.farmer.as_mut() {
                            farmer.skip_current();
                        }
                        bot.exec = None;
                        bot.exec_goal = None;
                    }
                    Step::Done => {
                        bot.exec = None;
                        bot.exec_goal = None;
                    }
                }
            }
        } else {
            bot.exec = None;
            bot.exec_goal = None;
        }
        if outcome.is_some() {
            if bot.archer.as_ref().is_some_and(Archer::drawing) {
                host.stop_using(bot.body);
            }
            bot.fighter = None;
            bot.archer = None;
            bot.item_user = None;
            bot.flight = None;
            bot.process = Process::Idle;
            bot.exec = None;
            bot.exec_goal = None;
        }
        if let Some(pos) = dig {
            host.dig(bot.body, pos, &seen.pos);
        }
        perform(host, bot.body, &seen.pos, acts);
        if (!active || matches!(bot.process, Process::Idle))
            && let Some(held) = bot.held.take()
        {
            host.drive(bot.body, held.released());
        }
        if !was_alive {
            self.send(host, &owner, id, Event::Respawned);
        }
        if let Some(event) = outcome
            && self.bots.contains_key(&id)
        {
            self.send(host, &owner, id, event);
        }
    }
}
