//! Raids: a player carrying Bad Omen into a village gets Raid Omen, and
//! when that runs out a raid starts there. The region owning the raid's
//! centre runs it: waves of raiders spawn at the village edge after a
//! countdown, a boss bar tracks what is left of each wave, and once the
//! last wave falls the players who fought earn Hero of the Village.
//!
//! Raiders are tracked by UUID. Ones that wandered into a neighbouring
//! region are looked up there with tasks that answer back, and raiders
//! there report deaths and joins through their own region, so nothing here
//! waits on another actor. The view raiders and villagers read is copied
//! into the neighbours ([`Raids`]).

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

use rand::{Rng, SeedableRng};
use tokio::sync::oneshot;
use tuff_vanilla_data::Block;
use tuff_vanilla_data::registry::{CustomStat, EntityType, MobEffect, SoundEvent};

use crate::physics::block_pos;
use crate::raid::{self, Phase, Raids};
use crate::raider::{self, Report, Reports};
use crate::world::raid_store::{RaidStore, center_of};
use tuff_core::protocol::play::{self, BossAction, BossEvent};
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::messages::{
    AdvancementEvent, PlayerEvent, PlayerHandle, PlayerId, RegionInternal, RideOp, RidePassenger,
};
use tuff_core::world::player::Difficulty;
use tuff_core::world::poi::Occupancy;
use tuff_core::world::region_api::{RegionTask, RegionWorld, SeenPlayer};
use tuff_core::world::{framed, packets};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, RegionPos};

const RAID_OMEN_TICKS: i64 = 600;
const PRE_RAID_TICKS: i32 = 300;
const RAID_TIMEOUT_TICKS: i64 = 48_000;
const POST_RAID_TICKS: i32 = 40;
const CELEBRATION_TICKS: i32 = 600;
const MAX_OMEN_LEVEL: i32 = 5;
const HERO_TICKS: i64 = 48_000;
const REMOVAL_DISTANCE_SQ: f64 = 12_544.0;
const OUTSIDE_RAID_TIMEOUT: i32 = 30;
const VILLAGE_SEARCH_RADIUS: i32 = 64;
const SPAWN_IN_VILLAGE_SECONDS: i32 = 7;
const REMOTE_QUERY_TICKS: u64 = 20;
const MISSING_GRACE_TICKS: u64 = 60;
const MIRROR_EVERY: u64 = 10;
const STORE_EVERY: u64 = 20;
const BAR_RED: i32 = 2;
const BAR_NOTCHED_10: i32 = 2;

/// Raider types in spawn order, with how many come in each wave before
/// bonuses (index 0 unused).
const WAVES: [(EntityType, [i32; 8]); 5] = [
    (EntityType::Vindicator, [0, 0, 2, 0, 1, 4, 2, 5]),
    (EntityType::Evoker, [0, 0, 0, 0, 0, 1, 1, 2]),
    (EntityType::Pillager, [0, 4, 3, 3, 4, 4, 4, 2]),
    (EntityType::Witch, [0, 0, 0, 0, 3, 0, 0, 1]),
    (EntityType::Ravager, [0, 0, 0, 1, 0, 1, 0, 2]),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Ongoing,
    Victory,
    Loss,
    Stopped,
}

#[derive(Debug, Clone)]
struct Member {
    wave: i32,
    health: f32,
    seen: u64,
    position: [f64; 3],
    no_action_time: i32,
    age: u64,
    outside: i32,
}

#[derive(Debug)]
struct Raid {
    id: u32,
    center: BlockPos,
    started: bool,
    active: bool,
    ticks_active: i64,
    omen_level: i32,
    groups_spawned: i32,
    cooldown: i32,
    post_raid_ticks: i32,
    total_health: f32,
    num_groups: i32,
    status: Status,
    celebration_ticks: i32,
    wave_spawn: Option<BlockPos>,
    heroes: HashSet<u128>,
    members: HashMap<u128, Member>,
    leaders: HashMap<i32, u128>,
    bar: Bar,
    asked: u64,
    remote_search: Option<oneshot::Receiver<Option<BlockPos>>>,
    pending_spawns: u32,
    spawn_deadline: u64,
}

#[derive(Debug)]
struct Bar {
    uuid: u128,
    name: TextComponent,
    progress: f32,
    visible: bool,
    viewers: HashMap<PlayerId, PlayerHandle>,
}

struct Pending {
    uuid: u128,
    pos: BlockPos,
    taken: oneshot::Receiver<u8>,
    level: Option<u8>,
    due: u64,
}

#[derive(Default)]
struct Manager {
    raids: Vec<Raid>,
    pending: Vec<Pending>,
    adoption: Adoption,
    /// The raids the store last heard this region runs.
    stored: HashSet<u32>,
}

/// Taking over the saved raids centred in this region, once per region.
#[derive(Default)]
enum Adoption {
    #[default]
    Ask,
    Waiting(oneshot::Receiver<Vec<Compound>>),
    Done,
}

fn num_groups(difficulty: Difficulty) -> i32 {
    match difficulty {
        Difficulty::Peaceful => 0,
        Difficulty::Easy => 3,
        Difficulty::Normal => 5,
        Difficulty::Hard => 7,
    }
}

fn raid_name() -> TextComponent {
    TextComponent::translate("event.minecraft.raid", [])
}

impl Raid {
    fn new(id: u32, center: BlockPos, difficulty: Difficulty, bar_uuid: u128) -> Self {
        Self {
            id,
            center,
            started: false,
            active: true,
            ticks_active: 0,
            omen_level: 0,
            groups_spawned: 0,
            cooldown: PRE_RAID_TICKS,
            post_raid_ticks: 0,
            total_health: 0.0,
            num_groups: num_groups(difficulty),
            status: Status::Ongoing,
            celebration_ticks: 0,
            wave_spawn: None,
            heroes: HashSet::new(),
            members: HashMap::new(),
            leaders: HashMap::new(),
            bar: Bar {
                uuid: bar_uuid,
                name: raid_name(),
                progress: 0.0,
                visible: true,
                viewers: HashMap::new(),
            },
            asked: 0,
            remote_search: None,
            pending_spawns: 0,
            spawn_deadline: 0,
        }
    }

    fn alive(&self) -> usize {
        self.members.len() + self.pending_spawns as usize
    }

    /// The raid as the game saves it (raiders rejoin on their own).
    fn to_saved(&self) -> Compound {
        let status = match self.status {
            Status::Ongoing => "ongoing",
            Status::Victory => "victory",
            Status::Loss => "loss",
            Status::Stopped => "stopped",
        };
        let mut heroes: Vec<u128> = self.heroes.iter().copied().collect();
        heroes.sort_unstable();
        Compound::new()
            .with("started", self.started)
            .with("active", self.active)
            .with("ticks_active", Tag::Long(self.ticks_active))
            .with("raid_omen_level", self.omen_level)
            .with("groups_spawned", self.groups_spawned)
            .with("cooldown_ticks", self.cooldown)
            .with("post_raid_ticks", self.post_raid_ticks)
            .with("total_health", Tag::Float(self.total_health))
            .with("group_count", self.num_groups)
            .with("status", status)
            .with(
                "center",
                Tag::IntArray(vec![self.center.x, self.center.y, self.center.z]),
            )
            .with(
                "heroes_of_the_village",
                Tag::List(heroes.into_iter().map(Tag::uuid).collect()),
            )
    }

    fn from_saved(saved: &Compound, bar_uuid: u128) -> Option<Self> {
        let id = saved.get_int("id")? as u32;
        let center = center_of(saved)?;
        let mut raid = Self::new(id, center, Difficulty::Normal, bar_uuid);
        raid.started = saved.get_bool("started").unwrap_or(false);
        raid.active = saved.get_bool("active").unwrap_or(false);
        raid.ticks_active = saved.get_long("ticks_active").unwrap_or(0);
        raid.omen_level = saved.get_int("raid_omen_level").unwrap_or(0);
        raid.groups_spawned = saved.get_int("groups_spawned").unwrap_or(0);
        raid.cooldown = saved.get_int("cooldown_ticks").unwrap_or(0);
        raid.post_raid_ticks = saved.get_int("post_raid_ticks").unwrap_or(0);
        raid.total_health = saved.get_float("total_health").unwrap_or(0.0);
        raid.num_groups = saved.get_int("group_count").unwrap_or(raid.num_groups);
        raid.status = match saved.get_str("status") {
            Some("victory") => Status::Victory,
            Some("loss") => Status::Loss,
            Some("stopped") => Status::Stopped,
            _ => Status::Ongoing,
        };
        raid.heroes = saved
            .get_list("heroes_of_the_village")
            .unwrap_or_default()
            .iter()
            .filter_map(Tag::as_uuid)
            .collect();
        Some(raid)
    }

    fn has_bonus_wave(&self) -> bool {
        self.omen_level > 1
    }

    fn is_final_wave(&self) -> bool {
        self.groups_spawned == self.num_groups
    }

    fn has_more_waves(&self) -> bool {
        if self.has_bonus_wave() {
            self.groups_spawned <= self.num_groups
        } else {
            !self.is_final_wave()
        }
    }

    fn should_spawn_bonus_group(&self) -> bool {
        self.is_final_wave() && self.alive() == 0 && self.has_bonus_wave()
    }

    fn should_spawn_group(&self) -> bool {
        self.cooldown == 0
            && (self.groups_spawned < self.num_groups || self.should_spawn_bonus_group())
            && self.alive() == 0
    }

    fn phase(&self) -> Phase {
        match self.status {
            Status::Victory => Phase::Victory,
            Status::Loss => Phase::Loss,
            Status::Stopped => Phase::Stopped,
            Status::Ongoing if !self.started => Phase::Preparing,
            Status::Ongoing if self.alive() == 0 && self.cooldown > 0 => Phase::BetweenWaves,
            Status::Ongoing => Phase::Active,
        }
    }

    fn view(&self, home: RegionPos) -> raid::Raid {
        let mut view = raid::Raid::new(self.id, self.center, self.phase());
        view.home = Some(home);
        view.omen_level = self.omen_level;
        view.groups_spawned = self.groups_spawned;
        view.leaders = self
            .leaders
            .keys()
            .filter(|w| (0..64).contains(*w))
            .fold(0, |bits, w| bits | 1 << w);
        view
    }

    fn health_left(&self) -> f32 {
        self.members.values().map(|m| m.health).sum()
    }

    fn update_bar_progress(&mut self) {
        self.bar.progress = if self.total_health > 0.0 {
            (self.health_left() / self.total_health).clamp(0.0, 1.0)
        } else {
            0.0
        };
    }

    fn stop(&mut self) {
        self.active = false;
        self.status = Status::Stopped;
        for handle in self.bar.viewers.values() {
            handle.send(PlayerEvent::Packets(packets([framed(&BossEvent {
                uuid: self.bar.uuid,
                action: BossAction::Remove,
            })])));
        }
        self.bar.viewers.clear();
    }
}

/// The region tick's raid step: omens, every raid this region runs, and
/// the view mobs read.
pub fn tick(world: &mut dyn RegionWorld) {
    let now = world.tick();
    world.state().get::<Raids>().expire(now);
    let mut manager = std::mem::take(world.state().get::<Manager>());
    let adopted = adopt(world, &mut manager);
    deliver_reports(world, &mut manager);
    let due = check_omens(world, &mut manager);
    for raid in &mut manager.raids {
        tick_raid(world, raid);
    }
    let home = world.region_pos();
    let views: Vec<raid::Raid> = manager.raids.iter().map(|r| r.view(home)).collect();
    manager.raids.retain(|r| r.status != Status::Stopped);
    store(world, &mut manager, now);
    let mirror_now = now.is_multiple_of(MIRROR_EVERY);
    let local = world.state().get::<Manager>();
    let arrived = std::mem::take(&mut local.raids);
    manager.raids.extend(arrived);
    let pending = std::mem::take(&mut local.pending);
    manager.pending.extend(pending);
    *local = manager;
    if mirror_now || adopted || views.iter().any(|v| v.phase == Phase::Stopped) {
        mirror(world, views);
    }
    for (player, pos, level) in due {
        create_or_extend(world, player, pos, level);
    }
}

/// Takes over the saved raids centred here, with the raiders already
/// loaded here that belong to them; true once some arrived.
fn adopt(world: &mut dyn RegionWorld, manager: &mut Manager) -> bool {
    let shared = world.shared().clone();
    let Some(store) = shared.services.get::<RaidStore>() else {
        return false;
    };
    let saved = match std::mem::take(&mut manager.adoption) {
        Adoption::Done => {
            manager.adoption = Adoption::Done;
            return false;
        }
        Adoption::Ask => {
            let dimension = world.dimension_info().key.clone();
            manager.adoption = match store.adopt(&dimension, world.region_pos()) {
                Some(answer) => Adoption::Waiting(answer),
                None => Adoption::Ask,
            };
            return false;
        }
        Adoption::Waiting(mut answer) => match answer.try_recv() {
            Ok(saved) => saved,
            Err(oneshot::error::TryRecvError::Empty) => {
                manager.adoption = Adoption::Waiting(answer);
                return false;
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                manager.adoption = Adoption::Done;
                return false;
            }
        },
    };
    manager.adoption = Adoption::Done;
    let mut any = false;
    for saved in saved {
        let bar = world.rng().r#gen::<u128>();
        let Some(mut raid) = Raid::from_saved(&saved, bar) else {
            continue;
        };
        if manager.raids.iter().any(|r| r.id == raid.id) {
            continue;
        }
        claim_loaded_raiders(world, &mut raid);
        manager.stored.insert(raid.id);
        manager.raids.push(raid);
        any = true;
    }
    any
}

fn claim_loaded_raiders(world: &mut dyn RegionWorld, raid: &mut Raid) {
    let now = world.tick();
    for entity in world.entity_ids() {
        let Some(tracked) = world.entity_mut(entity) else {
            continue;
        };
        let uuid = tracked.entity.base().uuid;
        let Some(core) = raider::core_mut(tracked.entity.as_mut()) else {
            continue;
        };
        let (health, position, alive) = (core.health, core.base.position, core.is_alive());
        let Some(state) = core.raider.as_mut().filter(|s| s.raid == Some(raid.id)) else {
            continue;
        };
        if !alive {
            continue;
        }
        state.rejoin = 0;
        let (wave, leader) = (state.wave, state.patrol_leader);
        raid.members.insert(
            uuid,
            Member {
                wave,
                health,
                seen: now,
                position,
                no_action_time: 0,
                age: 0,
                outside: 0,
            },
        );
        if leader {
            raid.leaders.entry(wave).or_insert(uuid);
        }
    }
}

/// Hands the store a snapshot of the raids this region runs, every
/// second.
fn store(world: &mut dyn RegionWorld, manager: &mut Manager, now: u64) {
    if !now.is_multiple_of(STORE_EVERY) || !matches!(manager.adoption, Adoption::Done) {
        return;
    }
    if manager.raids.is_empty() && manager.stored.is_empty() {
        return;
    }
    let shared = world.shared().clone();
    let Some(store) = shared.services.get::<RaidStore>() else {
        return;
    };
    let current: HashSet<u32> = manager.raids.iter().map(|r| r.id).collect();
    let gone: Vec<u32> = manager.stored.difference(&current).copied().collect();
    let raids = manager.raids.iter().map(|r| (r.id, r.to_saved())).collect();
    let dimension = world.dimension_info().key.clone();
    if store.update(&dimension, world.region_pos(), raids, gone) {
        manager.stored = current;
    }
}

fn mirror(world: &mut dyn RegionWorld, views: Vec<raid::Raid>) {
    if views.is_empty() {
        return;
    }
    let now = world.tick();
    let raids = world.state().get::<Raids>();
    for view in &views {
        raids.mirror(*view, now);
    }
    for handle in neighbours(world) {
        let views = views.clone();
        handle.internal(RegionInternal::Run(RegionTask::new(move |other| {
            let now = other.tick();
            let raids = other.state().get::<Raids>();
            for view in views {
                raids.mirror(view, now);
            }
        })));
    }
}

fn neighbours(world: &dyn RegionWorld) -> Vec<tuff_core::world::messages::RegionHandle> {
    let here = world.region_pos();
    let mut found = Vec::new();
    for dx in -1..=1 {
        for dz in -1..=1 {
            if (dx, dz) == (0, 0) {
                continue;
            }
            if let Some(handle) = world.neighbor(RegionPos::new(here.x + dx, here.z + dz)) {
                found.push(handle);
            }
        }
    }
    found
}

/// Runs `run` on the raid's home region: here, or a neighbour.
fn at_home(
    world: &mut dyn RegionWorld,
    home: RegionPos,
    run: impl FnOnce(&mut dyn RegionWorld) + Send + 'static,
) {
    if home == world.region_pos() {
        run(world);
    } else if let Some(handle) = world.neighbor(home) {
        handle.internal(RegionInternal::Run(RegionTask::new(run)));
    }
}

fn with_raid(
    world: &mut dyn RegionWorld,
    id: u32,
    run: impl FnOnce(&mut Raid, &mut dyn RegionWorld),
) {
    let mut manager = std::mem::take(world.state().get::<Manager>());
    if let Some(raid) = manager.raids.iter_mut().find(|r| r.id == id) {
        run(raid, world);
    }
    let local = world.state().get::<Manager>();
    let arrived = std::mem::take(&mut local.raids);
    manager.raids.extend(arrived);
    let pending = std::mem::take(&mut local.pending);
    manager.pending.extend(pending);
    *local = manager;
}

fn deliver_reports(world: &mut dyn RegionWorld, manager: &mut Manager) {
    let reports = std::mem::take(&mut world.state().get::<Reports>().0);
    for report in reports {
        let id = match report {
            Report::Joined { raid, .. }
            | Report::Leader { raid, .. }
            | Report::Died { raid, .. } => raid,
        };
        if let Some(raid) = manager.raids.iter_mut().find(|r| r.id == id) {
            apply_report(raid, report, world.tick());
            continue;
        }
        let Some(home) = world.state().get::<Raids>().get(id).and_then(|r| r.home) else {
            continue;
        };
        at_home(world, home, move |w| {
            with_raid(w, id, |raid, w| apply_report(raid, report, w.tick()));
        });
    }
}

fn apply_report(raid: &mut Raid, report: Report, now: u64) {
    match report {
        Report::Joined {
            uuid,
            wave,
            health,
            leader,
            rejoin,
            ..
        } => {
            if raid.members.contains_key(&uuid) {
                return;
            }
            raid.members.insert(
                uuid,
                Member {
                    wave,
                    health,
                    seen: now,
                    position: [0.0; 3],
                    no_action_time: 0,
                    age: 0,
                    outside: 0,
                },
            );
            if leader {
                raid.leaders.entry(wave).or_insert(uuid);
            }
            if !rejoin {
                raid.total_health += health;
            }
            raid.update_bar_progress();
        }
        Report::Leader { uuid, wave, .. } => {
            raid.leaders.entry(wave).or_insert(uuid);
        }
        Report::Died {
            uuid, wave, killer, ..
        } => {
            if raid.leaders.get(&wave) == Some(&uuid) {
                raid.leaders.remove(&wave);
            }
            if let Some(killer) = killer {
                raid.heroes.insert(killer);
            }
            if raid.members.remove(&uuid).is_some() {
                raid.update_bar_progress();
            }
        }
    }
}

fn is_village(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    raider::is_village(world.pois(), pos)
}

fn can_start_raid(world: &dyn RegionWorld, pos: BlockPos) -> bool {
    let by_biome = world
        .biome_name(pos)
        .and_then(tuff_vanilla_data::biome::Biome::get)
        .and_then(|b| b.attribute("minecraft:gameplay/can_start_raid"))
        .and_then(|v| v.as_bool());
    by_biome.unwrap_or(world.dimension_info().environment().can_start_raid)
}

fn check_omens(
    world: &mut dyn RegionWorld,
    manager: &mut Manager,
) -> Vec<(SeenPlayer, BlockPos, u8)> {
    let now = world.tick();
    let players = world.seen_player_details();
    let peaceful = *world.shared().difficulty.borrow() == Difficulty::Peaceful;
    if !peaceful {
        for player in &players {
            let pos = block_pos(player.position);
            if !player.looks.has_effect(MobEffect::BadOmen)
                || !world.owns(pos)
                || manager.pending.iter().any(|p| p.uuid == player.uuid)
                || !is_village(world, pos)
            {
                continue;
            }
            let full = world
                .state()
                .get::<Raids>()
                .at(pos)
                .is_some_and(|r| r.omen_level >= MAX_OMEN_LEVEL);
            if full {
                continue;
            }
            let (tx, rx) = oneshot::channel();
            player.handle.send(PlayerEvent::SwapEffect {
                from: MobEffect::BadOmen,
                to: MobEffect::RaidOmen,
                duration: RAID_OMEN_TICKS,
                taken: Some(tx),
            });
            manager.pending.push(Pending {
                uuid: player.uuid,
                pos,
                taken: rx,
                level: None,
                due: 0,
            });
        }
    }
    let mut due = Vec::new();
    manager.pending.retain_mut(|pending| {
        if pending.level.is_none() {
            match pending.taken.try_recv() {
                Ok(level) => {
                    pending.level = Some(level);
                    pending.due = now + RAID_OMEN_TICKS as u64 - 1;
                }
                Err(oneshot::error::TryRecvError::Empty) => return true,
                Err(oneshot::error::TryRecvError::Closed) => return false,
            }
        }
        if now < pending.due {
            return true;
        }
        due.push((pending.uuid, pending.pos, pending.level.unwrap_or(0)));
        false
    });
    due.into_iter()
        .filter_map(|(uuid, pos, level)| {
            players
                .iter()
                .find(|p| p.uuid == uuid && p.looks.has_effect(MobEffect::RaidOmen))
                .map(|player| (player.clone(), pos, level))
        })
        .collect()
}

/// The heart of the village around `pos`: the average of its occupied
/// village points of interest, or `pos` itself with none.
fn village_center(world: &dyn RegionWorld, pos: BlockPos) -> BlockPos {
    let pois = world
        .pois()
        .closest_first(pos, VILLAGE_SEARCH_RADIUS, Occupancy::IsOccupied, |k| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::point_of_interest_type::key::VILLAGE,
                k,
            )
        });
    if pois.is_empty() {
        pos
    } else {
        let n = pois.len() as f64;
        let sum = pois.iter().fold([0.0f64; 3], |s, (p, _)| {
            [
                s[0] + f64::from(p.x),
                s[1] + f64::from(p.y),
                s[2] + f64::from(p.z),
            ]
        });
        block_pos([sum[0] / n, sum[1] / n, sum[2] / n])
    }
}

/// A Raid Omen ran out at `pos`: the raid there grows, or a new one starts
/// at the heart of the village around it.
fn create_or_extend(world: &mut dyn RegionWorld, player: SeenPlayer, pos: BlockPos, level: u8) {
    if !world.shared().rule_bool("raids") || !can_start_raid(world, pos) {
        return;
    }
    let center = village_center(world, pos);
    let known = world.state().get::<Raids>().at(center);
    let home = match known.and_then(|r| r.home) {
        Some(home) => home,
        None => ChunkPos::of_block(center).region(),
    };
    let existing = known.map(|r| r.id);
    at_home(world, home, move |w| {
        absorb(w, existing, center, player, level)
    });
}

type RaidRun = Box<dyn FnOnce(&mut Raid, &mut dyn RegionWorld) -> DebugReply + Send>;

/// A request of the developer `/raid` command.
#[derive(Debug, Clone)]
pub enum DebugAction {
    Start(i32),
    Stop,
    Check,
    SetOmen(i32),
    Glow,
    SpawnLeader([f64; 3]),
    LocalSound([f64; 3]),
}

/// What a [`DebugAction`] came to: lines to show as success, a failure to
/// show, and the command's result.
#[derive(Debug, Default)]
pub struct DebugReply {
    pub messages: Vec<String>,
    pub failure: Option<String>,
    pub result: i32,
}

impl DebugReply {
    fn ok(message: impl Into<String>) -> Self {
        Self {
            messages: vec![message.into()],
            failure: None,
            result: 1,
        }
    }

    fn fail(message: impl Into<String>, result: i32) -> Self {
        Self {
            messages: Vec::new(),
            failure: Some(message.into()),
            result,
        }
    }
}

/// Runs a `/raid` request for the player at `pos`, in the region holding
/// them; the raid itself is reached at its home region.
pub fn debug(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    action: DebugAction,
    reply: oneshot::Sender<DebugReply>,
) {
    let known = world.state().get::<Raids>().at(pos);
    let on_raid =
        |world: &mut dyn RegionWorld, reply: oneshot::Sender<DebugReply>, run: RaidRun| {
            let Some(view) = known else {
                return;
            };
            let id = view.id;
            let home = view.home.unwrap_or_else(|| world.region_pos());
            at_home(world, home, move |w| {
                with_raid(w, id, |raid, w| {
                    let _ = reply.send(run(raid, w));
                });
            });
        };
    match action {
        DebugAction::Start(level) => {
            if known.is_some() {
                let _ = reply.send(DebugReply::fail("Raid already started close by", -1));
                return;
            }
            if !world.shared().rule_bool("raids") || !can_start_raid(world, pos) {
                let _ = reply.send(DebugReply::fail(
                    "Failed to create a raid in your local village",
                    1,
                ));
                return;
            }
            let center = village_center(world, pos);
            let home = ChunkPos::of_block(center).region();
            at_home(world, home, move |w| {
                let difficulty = *w.shared().difficulty.borrow();
                let id = w.rng().r#gen::<u32>().max(1);
                let bar = w.rng().r#gen::<u128>();
                let mut raid = Raid::new(id, center, difficulty, bar);
                raid.omen_level = level;
                let view = raid.view(w.region_pos());
                w.state().get::<Manager>().raids.push(raid);
                mirror(w, vec![view]);
                let _ = reply.send(DebugReply::ok("Created a raid in your local village"));
            });
        }
        DebugAction::Stop => {
            if known.is_none() {
                let _ = reply.send(DebugReply::fail("No raid here", -1));
                return;
            }
            on_raid(
                world,
                reply,
                Box::new(|raid, _| {
                    raid.stop();
                    DebugReply::ok("Stopped raid")
                }),
            );
        }
        DebugAction::Check => {
            if known.is_none() {
                let _ = reply.send(DebugReply::fail("Found no started raids", 0));
                return;
            }
            on_raid(
                world,
                reply,
                Box::new(|raid, _| DebugReply {
                    messages: vec![
                        "Found a started raid!".to_owned(),
                        format!(
                            "Num groups spawned: {} Raid omen level: {} Num mobs: {} Raid health: {:?} / {:?}",
                            raid.groups_spawned,
                            raid.omen_level,
                            raid.alive(),
                            raid.health_left(),
                            raid.total_health,
                        ),
                    ],
                    failure: None,
                    result: 1,
                }),
            );
        }
        DebugAction::SetOmen(level) => {
            if known.is_none() {
                let _ = reply.send(DebugReply::fail("No raid found here", 1));
                return;
            }
            on_raid(
                world,
                reply,
                Box::new(move |raid, _| {
                    if level > MAX_OMEN_LEVEL {
                        return DebugReply::fail(
                            format!(
                                "Sorry, the max raid omen level you can set is {MAX_OMEN_LEVEL}"
                            ),
                            1,
                        );
                    }
                    let before = raid.omen_level;
                    raid.omen_level = level;
                    DebugReply::ok(format!(
                        "Changed village's raid omen level from {before} to {level}"
                    ))
                }),
            );
        }
        DebugAction::Glow => {
            if known.is_none() {
                let _ = reply.send(DebugReply {
                    result: 1,
                    ..DebugReply::default()
                });
                return;
            }
            on_raid(
                world,
                reply,
                Box::new(|raid, w| {
                    let wanted: HashSet<u128> = raid.members.keys().copied().collect();
                    glow_members(w, &wanted);
                    for handle in neighbours(w) {
                        let wanted = wanted.clone();
                        handle.internal(RegionInternal::Run(RegionTask::new(move |other| {
                            glow_members(other, &wanted);
                        })));
                    }
                    DebugReply {
                        result: 1,
                        ..DebugReply::default()
                    }
                }),
            );
        }
        DebugAction::SpawnLeader(at) => {
            let id = world.shared().next_entity_id();
            let created = world
                .shared()
                .entity_types
                .create(EntityType::Pillager, id, at, 0.0);
            let Some(mut entity) = created else {
                let _ = reply.send(DebugReply::fail("Pillager failed to spawn", 0));
                return;
            };
            if let Some(core) = raider::core_mut(entity.as_mut()) {
                if let Some(state) = core.raider.as_mut() {
                    state.patrol_leader = true;
                }
                raider::wear_banner(core);
            }
            world.finalize_spawn(&mut entity, 0);
            world.add_entity(entity);
            let _ = reply.send(DebugReply::ok("Spawned a raid captain"));
        }
        DebugAction::LocalSound(from) => {
            let seed = world.rng().r#gen::<i64>();
            let at = [from[0] + 5.0, from[1], from[2]];
            for player in world.seen_player_details() {
                let [px, py, pz] = player.position;
                let near = (px - at[0]).powi(2) + (py - at[1]).powi(2) + (pz - at[2]).powi(2)
                    < 32.0 * 32.0;
                if near {
                    player
                        .handle
                        .send(PlayerEvent::Packets(packets([framed(&play::Sound {
                            sound_event: SoundEvent::EventRaidHorn.id(),
                            category: play::sound_category::NEUTRAL,
                            position: at,
                            volume: 2.0,
                            pitch: 1.0,
                            seed,
                        })])));
                }
            }
            let _ = reply.send(DebugReply {
                result: 1,
                ..DebugReply::default()
            });
        }
    }
}

fn glow_members(world: &mut dyn RegionWorld, wanted: &HashSet<u128>) {
    for id in world.entity_ids() {
        if let Some(tracked) = world.entity_mut(id)
            && wanted.contains(&tracked.entity.base().uuid)
        {
            tracked
                .entity
                .add_effects(&[ActiveEffect::timed(MobEffect::Glowing, 1000, 1)]);
        }
    }
}

fn absorb(
    world: &mut dyn RegionWorld,
    existing: Option<u32>,
    center: BlockPos,
    player: SeenPlayer,
    level: u8,
) {
    let found = existing.is_some_and(|id| {
        world
            .state()
            .get::<Manager>()
            .raids
            .iter()
            .any(|r| r.id == id)
    });
    let id = match (existing, found) {
        (Some(id), true) => id,
        _ => {
            let difficulty = *world.shared().difficulty.borrow();
            let id = world.rng().r#gen::<u32>().max(1);
            let bar = world.rng().r#gen::<u128>();
            let raid = Raid::new(id, center, difficulty, bar);
            world.state().get::<Manager>().raids.push(raid);
            id
        }
    };
    let home = world.region_pos();
    let mut view = None;
    with_raid(world, id, |raid, _| {
        view = Some(raid.view(home));
        if raid.started && raid.omen_level >= MAX_OMEN_LEVEL {
            return;
        }
        raid.omen_level = (raid.omen_level + i32::from(level) + 1).clamp(0, MAX_OMEN_LEVEL);
        if raid.groups_spawned == 0 {
            player
                .handle
                .send(PlayerEvent::AwardStat(CustomStat::RaidTrigger, 1));
            player.handle.send(PlayerEvent::Advancement(Box::new(
                AdvancementEvent::RaidOmen,
            )));
        }
        view = Some(raid.view(home));
    });
    if let Some(view) = view {
        mirror(world, vec![view]);
    }
}

fn tick_raid(world: &mut dyn RegionWorld, raid: &mut Raid) {
    if raid.status != Status::Stopped {
        let _ = step_raid(world, raid);
    }
}

/// One tick of a live raid; `Break` once the raid needs nothing more this
/// tick (it ended, or is between waves).
fn step_raid(world: &mut dyn RegionWorld, raid: &mut Raid) -> ControlFlow<()> {
    end_if(raid, !world.shared().rule_bool("raids"))?;
    if raid.status != Status::Ongoing {
        return wind_down(world, raid);
    }
    hold_ground(world, raid)?;
    raid.ticks_active += 1;
    end_if(raid, raid.ticks_active >= RAID_TIMEOUT_TICKS)?;
    poll_remote_search(raid);
    if raid.pending_spawns > 0 && world.tick() > raid.spawn_deadline {
        raid.pending_spawns = 0;
    }
    track_members(world, raid);
    if raid.alive() == 0 && raid.has_more_waves() {
        count_down(world, raid)?;
    }
    if raid.ticks_active % 20 == 0 {
        update_players(world, raid);
        update_members(world, raid);
        raid.bar.name = bar_title(raid.alive());
    }
    launch_waves(world, raid);
    if raid.started && !raid.has_more_waves() && raid.alive() == 0 {
        if raid.post_raid_ticks >= POST_RAID_TICKS {
            raid.status = Status::Victory;
            reward_heroes(world, raid);
        } else {
            raid.post_raid_ticks += 1;
        }
    }
    send_bar_progress(raid);
    ControlFlow::Continue(())
}

fn end_if(raid: &mut Raid, cond: bool) -> ControlFlow<()> {
    if cond {
        raid.stop();
        ControlFlow::Break(())
    } else {
        ControlFlow::Continue(())
    }
}

/// The won or lost raid's bar lingers, refreshed once a second, until the
/// celebration runs out.
fn wind_down(world: &mut dyn RegionWorld, raid: &mut Raid) -> ControlFlow<()> {
    raid.celebration_ticks += 1;
    end_if(raid, raid.celebration_ticks >= CELEBRATION_TICKS)?;
    if raid.celebration_ticks % 20 == 0 {
        let won = raid.status == Status::Victory;
        if won {
            raid.bar.progress = 0.0;
        }
        let key = if won {
            "event.minecraft.raid.victory.full"
        } else {
            "event.minecraft.raid.defeat.full"
        };
        raid.bar.name = TextComponent::translate(key, []);
        raid.bar.visible = true;
        update_players(world, raid);
    }
    ControlFlow::Break(())
}

/// Tracks whether the center is loaded and still a village (moving to a
/// neighbouring one when not); `Break` when the raid paused or ended.
fn hold_ground(world: &mut dyn RegionWorld, raid: &mut Raid) -> ControlFlow<()> {
    let loaded = world.block(raid.center).is_some();
    let toggled = loaded != raid.active;
    raid.active = loaded;
    end_if(
        raid,
        *world.shared().difficulty.borrow() == Difficulty::Peaceful,
    )?;
    if toggled {
        raid.bar.visible = loaded;
    }
    if !loaded {
        return ControlFlow::Break(());
    }
    let mut in_village = is_village(world, raid.center);
    if !in_village {
        move_to_nearby_village(world, raid);
        in_village = is_village(world, raid.center);
    }
    if !in_village {
        end_if(raid, raid.groups_spawned == 0)?;
        raid.status = Status::Loss;
    }
    ControlFlow::Continue(())
}

/// The quiet stretch before the next wave: keeps a spawn point ready and
/// fills the bar; `Break` on the tick a new countdown begins.
fn count_down(world: &mut dyn RegionWorld, raid: &mut Raid) -> ControlFlow<()> {
    if raid.cooldown <= 0 {
        if raid.groups_spawned == 0 {
            return ControlFlow::Continue(());
        }
        raid.cooldown = PRE_RAID_TICKS;
        raid.bar.name = raid_name();
        return ControlFlow::Break(());
    }
    let refind = match raid.wave_spawn {
        Some(spot) => world.owns(spot) && world.block(spot).is_none(),
        None => raid.cooldown % 5 == 0,
    };
    if refind {
        raid.wave_spawn = find_spawn_pos(world, raid, 8);
    }
    if raid.cooldown % 20 == 0 || raid.cooldown == PRE_RAID_TICKS {
        update_players(world, raid);
    }
    raid.cooldown -= 1;
    let elapsed = (PRE_RAID_TICKS - raid.cooldown) as f32;
    raid.bar.progress = (elapsed / PRE_RAID_TICKS as f32).clamp(0.0, 1.0);
    ControlFlow::Continue(())
}

fn bar_title(alive: usize) -> TextComponent {
    let title = raid_name();
    if !(1..=2).contains(&alive) {
        return title;
    }
    let left = TextComponent::translate(
        "event.minecraft.raid.raiders_remaining",
        [TextComponent::new(alive.to_string())],
    );
    title.append(TextComponent::new(" - ")).append(left)
}

/// Spawns every group that is due, sounding the horn once at the first;
/// gives up on the raid after six spots in a row cannot be found.
fn launch_waves(world: &mut dyn RegionWorld, raid: &mut Raid) {
    let mut horn_sounded = false;
    let mut misses = 0;
    while raid.should_spawn_group() {
        let spot = match raid.wave_spawn {
            Some(spot) => Some(spot),
            None if raid.remote_search.is_some() => None,
            None => find_spawn_pos(world, raid, 20),
        };
        let Some(spot) = spot else {
            if raid.remote_search.is_some() {
                return;
            }
            misses += 1;
            if misses > 5 {
                raid.stop();
                return;
            }
            continue;
        };
        raid.started = true;
        if world.owns(spot) {
            spawn_group(world, raid, spot);
        } else {
            spawn_group_remotely(world, raid, spot);
        }
        if !horn_sounded {
            horn_sounded = true;
            play_horn(world, raid, spot);
        }
    }
}

fn move_to_nearby_village(world: &dyn RegionWorld, raid: &mut Raid) {
    let section = |v: i32| v >> 4;
    let (sx, sy, sz) = (
        section(raid.center.x),
        section(raid.center.y),
        section(raid.center.z),
    );
    let mut best: Option<(i64, BlockPos)> = None;
    for x in sx - 2..=sx + 2 {
        for y in sy - 2..=sy + 2 {
            for z in sz - 2..=sz + 2 {
                let center = BlockPos::new((x << 4) + 8, (y << 4) + 8, (z << 4) + 8);
                if !is_village(world, center) {
                    continue;
                }
                let d = raid::distance_sq(center, raid.center);
                if best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, center));
                }
            }
        }
    }
    if let Some((_, center)) = best {
        raid.center = center;
    }
}

/// Refreshes what this region sees of the raid's raiders.
fn track_members(world: &mut dyn RegionWorld, raid: &mut Raid) {
    if raid.members.is_empty() {
        return;
    }
    let now = world.tick();
    for id in world.entity_ids() {
        let Some(tracked) = world.entity(id) else {
            continue;
        };
        let uuid = tracked.entity.base().uuid;
        let Some(member) = raid.members.get_mut(&uuid) else {
            continue;
        };
        let Some(core) = raider::core_of(tracked.entity.as_ref()) else {
            continue;
        };
        if !core.is_alive() || core.raider.as_ref().and_then(|r| r.raid) != Some(raid.id) {
            continue;
        }
        member.health = core.health;
        member.position = core.base.position;
        member.no_action_time = core.no_action_time;
        member.age = core.tick_count;
        member.seen = now;
    }
    let missing: Vec<u128> = raid
        .members
        .iter()
        .filter(|(_, m)| m.seen < now)
        .map(|(u, _)| *u)
        .collect();
    if !missing.is_empty() && now.saturating_sub(raid.asked) >= REMOTE_QUERY_TICKS {
        raid.asked = now;
        ask_neighbours(world, raid.id, missing);
    }
}

fn ask_neighbours(world: &mut dyn RegionWorld, id: u32, wanted: Vec<u128>) {
    let home = world.handle();
    for handle in neighbours(world) {
        let wanted = wanted.clone();
        let home = home.clone();
        handle.internal(RegionInternal::Run(RegionTask::new(move |other| {
            let mut found = Vec::new();
            for entity in other.entity_ids() {
                let Some(tracked) = other.entity(entity) else {
                    continue;
                };
                let uuid = tracked.entity.base().uuid;
                if !wanted.contains(&uuid) {
                    continue;
                }
                let Some(core) = raider::core_of(tracked.entity.as_ref()) else {
                    continue;
                };
                if core.is_alive() && core.raider.as_ref().and_then(|r| r.raid) == Some(id) {
                    found.push((
                        uuid,
                        core.health,
                        core.base.position,
                        core.no_action_time,
                        core.tick_count,
                    ));
                }
            }
            if found.is_empty() {
                return;
            }
            home.internal(RegionInternal::Run(RegionTask::new(move |here| {
                let now = here.tick();
                with_raid(here, id, |raid, _| {
                    for (uuid, health, position, no_action_time, age) in found {
                        if let Some(member) = raid.members.get_mut(&uuid) {
                            member.health = health;
                            member.position = position;
                            member.no_action_time = no_action_time;
                            member.age = age;
                            member.seen = now;
                        }
                    }
                });
            })));
        })));
    }
}

/// Drops raiders that are gone, too far away, or idle outside the
/// village too long.
fn update_members(world: &mut dyn RegionWorld, raid: &mut Raid) {
    let now = world.tick();
    let center = [
        f64::from(raid.center.x),
        f64::from(raid.center.y),
        f64::from(raid.center.z),
    ];
    let mut gone = Vec::new();
    for (uuid, member) in &mut raid.members {
        let missing = now.saturating_sub(member.seen) > MISSING_GRACE_TICKS;
        let seen_once = member.age > 0 || member.position != [0.0; 3];
        let far = seen_once
            && (0..3)
                .map(|i| (member.position[i] - center[i]).powi(2))
                .sum::<f64>()
                >= REMOVAL_DISTANCE_SQ;
        if missing || far {
            gone.push(*uuid);
            continue;
        }
        if member.age > 600 {
            if !raider::is_village(world.pois(), block_pos(member.position))
                && member.no_action_time > raider::MAX_NO_ACTION_TIME
            {
                member.outside += 1;
            }
            if member.outside >= OUTSIDE_RAID_TIMEOUT {
                gone.push(*uuid);
            }
        }
    }
    for uuid in gone {
        if let Some(member) = raid.members.remove(&uuid) {
            raid.total_health -= member.health;
            if raid.leaders.get(&member.wave) == Some(&uuid) {
                raid.leaders.remove(&member.wave);
            }
        }
        release(world, raid.id, uuid);
    }
    raid.update_bar_progress();
}

/// Tells a raider the raid let it go.
fn release(world: &mut dyn RegionWorld, id: u32, uuid: u128) {
    let clear = move |w: &mut dyn RegionWorld| {
        for entity in w.entity_ids() {
            let Some(tracked) = w.entity_mut(entity) else {
                continue;
            };
            if tracked.entity.base().uuid != uuid {
                continue;
            }
            if let Some(state) = raider::core_mut(tracked.entity.as_mut())
                .and_then(|core| core.raider.as_mut())
                .filter(|s| s.raid == Some(id))
            {
                state.raid = None;
            }
        }
    };
    clear(world);
    for handle in neighbours(world) {
        handle.internal(RegionInternal::Run(RegionTask::new(clear)));
    }
}

/// Shows the bar to living players this raid is the closest one to, and
/// takes it from those it no longer reaches.
fn update_players(world: &mut dyn RegionWorld, raid: &mut Raid) {
    let players = world.seen_player_details();
    let raids = world.state().get::<Raids>();
    let inside: HashMap<PlayerId, PlayerHandle> = if raid.bar.visible {
        players
            .iter()
            .filter(|p| {
                raids
                    .at(block_pos(p.position))
                    .is_some_and(|r| r.id == raid.id)
            })
            .map(|p| (p.handle.id, p.handle.clone()))
            .collect()
    } else {
        HashMap::new()
    };
    for (id, handle) in &inside {
        if !raid.bar.viewers.contains_key(id) {
            handle.send(PlayerEvent::Packets(packets([framed(&BossEvent {
                uuid: raid.bar.uuid,
                action: BossAction::Add {
                    title: &raid.bar.name,
                    progress: raid.bar.progress,
                    color: BAR_RED,
                    overlay: BAR_NOTCHED_10,
                    flags: 0,
                },
            })])));
        } else {
            handle.send(PlayerEvent::Packets(packets([
                framed(&BossEvent {
                    uuid: raid.bar.uuid,
                    action: BossAction::Title(&raid.bar.name),
                }),
                framed(&BossEvent {
                    uuid: raid.bar.uuid,
                    action: BossAction::Progress(raid.bar.progress),
                }),
            ])));
        }
    }
    for (id, handle) in &raid.bar.viewers {
        if !inside.contains_key(id) {
            handle.send(PlayerEvent::Packets(packets([framed(&BossEvent {
                uuid: raid.bar.uuid,
                action: BossAction::Remove,
            })])));
        }
    }
    raid.bar.viewers = inside;
}

fn send_bar_progress(raid: &mut Raid) {
    if raid.bar.viewers.is_empty() {
        return;
    }
    let progress = raid.bar.progress;
    for handle in raid.bar.viewers.values() {
        handle.send(PlayerEvent::Packets(packets([framed(&BossEvent {
            uuid: raid.bar.uuid,
            action: BossAction::Progress(progress),
        })])));
    }
}

fn reward_heroes(world: &mut dyn RegionWorld, raid: &Raid) {
    let players = world.seen_player_details();
    for hero in &raid.heroes {
        let Some(player) = players.iter().find(|p| p.uuid == *hero) else {
            continue;
        };
        let mut effect = ActiveEffect::timed(
            MobEffect::HeroOfTheVillage,
            HERO_TICKS,
            (raid.omen_level - 1).clamp(0, 255) as u8,
        );
        effect.show_particles = false;
        player.handle.send(PlayerEvent::AddEffect(effect));
        player
            .handle
            .send(PlayerEvent::AwardStat(CustomStat::RaidWin, 1));
        player.handle.send(PlayerEvent::Advancement(Box::new(
            AdvancementEvent::RaidWin,
        )));
    }
}

mod horn;
mod spawning;
use horn::play_horn;
use spawning::*;

#[cfg(test)]
#[path = "raids_tests.rs"]
mod tests;
