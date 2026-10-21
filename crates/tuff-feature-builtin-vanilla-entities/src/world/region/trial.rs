//! Trial spawners.
//!
//! A trial spawner stays saved block entity NBT. A region finds the ones
//! among its chunks every second and ticks each every tick: it waits for a
//! player in range, sends waves of mobs sized by how many players it saw,
//! and once the wave is dead ejects rewards one player at a time before
//! cooling down. Players carrying Bad Omen or Trial Omen make it ominous:
//! it swaps its wave for the ominous one and rains items on the fight.
//!
//! Mobs it sent are tracked by UUID. One that left the region is looked up
//! in the neighbouring regions, which answer back with a task, so nothing
//! here waits on another actor.

use std::collections::HashMap;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tuff_vanilla_data::registry::{EntityType, MobEffect, SoundEvent};
use tuff_vanilla_data::trial_spawner::{SpawnPotential, TrialSpawnerConfig};
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::world::entity::SpawnReason;
use tuff_core::world::entity::rule_holds;
use tuff_core::world::messages::{PlayerEvent, RegionInternal};
use tuff_core::world::physics;
use tuff_core::world::raycast;
use tuff_core::world::region_api::{RegionTask, RegionWorld, SeenPlayer};
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::RegionPos;

use crate::world::ominous_item_spawner::OminousItemSpawner;

pub const SPAWNER_ID: &str = "minecraft:trial_spawner";

const PLAYER_SCAN_TICKS: i64 = 20;
const DETECT_BUFFER_TICKS: i64 = 40;
const MAX_TRACKING_DISTANCE_SQ: i64 = 47 * 47;
const SHUTTER_DELAY_TICKS: i64 = 40;
const EJECTION_INTERVAL_TICKS: i64 = 30;
const ITEM_SPAWNER_INTERVAL_TICKS: i64 = 160;
const TRIAL_OMEN_TICKS_PER_LEVEL: i64 = 18_000;
const DEFAULT_COOLDOWN: i32 = 36_000;
const DEFAULT_PLAYER_RANGE: i32 = 14;
const REMOTE_QUERY_TICKS: u64 = 10;
const REMOTE_KEEP_TICKS: u64 = 60;
const ABSENT_GRACE_TICKS: u64 = 40;

const EVENT_SPAWN: i32 = 3011;
const EVENT_SPAWN_MOB_AT: i32 = 3012;
const EVENT_DETECT_PLAYER: i32 = 3013;
const EVENT_EJECT_ITEM: i32 = 3014;
const EVENT_DETECT_PLAYER_OMINOUS: i32 = 3019;
const EVENT_BECOME_OMINOUS: i32 = 3020;

const STATE: &str = "trial_spawner_state";
const OMINOUS: &str = "ominous";

#[derive(Default)]
struct Spawners(Vec<BlockPos>, Option<u64>);

const SETTING_KEYS: [&str; 4] = [
    "normal_config",
    "ominous_config",
    "target_cooldown_length",
    "required_player_range",
];

#[derive(Default)]
struct SettingsCache(HashMap<BlockPos, (Vec<Option<Tag>>, Settings)>);

fn cached_settings(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    key: Vec<Option<Tag>>,
) -> Option<Settings> {
    let cache = &mut world.state().get::<SettingsCache>().0;
    if let Some((cached, settings)) = cache.get(&pos)
        && *cached == key
    {
        return Some(settings.clone());
    }
    let settings = Settings::read(world.raw_block_entity(pos)?);
    world
        .state()
        .get::<SettingsCache>()
        .0
        .insert(pos, (key, settings.clone()));
    Some(settings)
}

#[derive(Default)]
struct Remote {
    seen: HashMap<(BlockPos, u128), u64>,
    asked: HashMap<BlockPos, u64>,
    absent: HashMap<(BlockPos, u128), u64>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Inactive,
    WaitingForPlayers,
    Active,
    WaitingForRewardEjection,
    EjectingReward,
    Cooldown,
}

impl State {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "inactive" => Self::Inactive,
            "waiting_for_players" => Self::WaitingForPlayers,
            "active" => Self::Active,
            "waiting_for_reward_ejection" => Self::WaitingForRewardEjection,
            "ejecting_reward" => Self::EjectingReward,
            "cooldown" => Self::Cooldown,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::WaitingForPlayers => "waiting_for_players",
            Self::Active => "active",
            Self::WaitingForRewardEjection => "waiting_for_reward_ejection",
            Self::EjectingReward => "ejecting_reward",
            Self::Cooldown => "cooldown",
        }
    }
}

#[derive(Default, Clone, PartialEq)]
struct Data {
    detected: Vec<u128>,
    current: Vec<u128>,
    cooldown_ends_at: i64,
    next_mob_spawns_at: i64,
    total_spawned: i32,
    next_spawn_data: Option<Compound>,
    ejecting: Option<String>,
}

fn uuids(nbt: &Compound, key: &str) -> Vec<u128> {
    nbt.get_list(key)
        .into_iter()
        .flatten()
        .filter_map(Tag::as_uuid)
        .collect()
}

fn number(nbt: &Compound, key: &str) -> i64 {
    match nbt.get(key) {
        Some(Tag::Long(v)) => *v,
        Some(Tag::Int(v)) => i64::from(*v),
        Some(Tag::Short(v)) => i64::from(*v),
        Some(Tag::Byte(v)) => i64::from(*v),
        _ => 0,
    }
}

impl Data {
    fn read(nbt: &Compound) -> Self {
        Self {
            detected: uuids(nbt, "registered_players"),
            current: uuids(nbt, "current_mobs"),
            cooldown_ends_at: number(nbt, "cooldown_ends_at"),
            next_mob_spawns_at: number(nbt, "next_mob_spawns_at"),
            total_spawned: number(nbt, "total_mobs_spawned") as i32,
            next_spawn_data: nbt.get_compound("spawn_data").cloned(),
            ejecting: nbt.get_str("ejecting_loot_table").map(str::to_owned),
        }
    }

    fn write(&self, nbt: &mut Compound) {
        let list = |ids: &[u128]| Tag::List(ids.iter().map(|id| Tag::uuid(*id)).collect());
        let mut put = |key: &str, keep: bool, value: Tag| {
            if keep {
                nbt.insert(key, value);
            } else {
                nbt.remove(key);
            }
        };
        put(
            "registered_players",
            !self.detected.is_empty(),
            list(&self.detected),
        );
        put(
            "current_mobs",
            !self.current.is_empty(),
            list(&self.current),
        );
        put(
            "cooldown_ends_at",
            self.cooldown_ends_at != 0,
            Tag::Long(self.cooldown_ends_at),
        );
        put(
            "next_mob_spawns_at",
            self.next_mob_spawns_at != 0,
            Tag::Long(self.next_mob_spawns_at),
        );
        put(
            "total_mobs_spawned",
            self.total_spawned != 0,
            Tag::Int(self.total_spawned),
        );
        put(
            "spawn_data",
            self.next_spawn_data.is_some(),
            self.next_spawn_data
                .clone()
                .map_or(Tag::Compound(Compound::new()), Tag::Compound),
        );
        put(
            "ejecting_loot_table",
            self.ejecting.is_some(),
            Tag::String(self.ejecting.clone().unwrap_or_default()),
        );
    }

    fn reset_statistics(&mut self) {
        self.detected.clear();
        self.total_spawned = 0;
        self.next_mob_spawns_at = 0;
        self.cooldown_ends_at = 0;
    }

    fn reset(&mut self) {
        self.current.clear();
        self.next_spawn_data = None;
        self.reset_statistics();
    }
}

#[derive(Clone)]
enum Config {
    Named(&'static TrialSpawnerConfig),
    Inline(std::sync::Arc<Inline>),
}

struct Inline {
    spawn_range: i64,
    total_mobs: f32,
    simultaneous_mobs: f32,
    total_per_player: f32,
    simultaneous_per_player: f32,
    ticks_between_spawn: i64,
    potentials: Vec<(u64, Compound)>,
    loot_tables: Vec<(u64, String)>,
    items_to_drop_when_ominous: String,
}

fn weight_of(entry: &Compound) -> u64 {
    entry
        .get("weight")
        .map_or(1, |_| number(entry, "weight").max(0) as u64)
}

impl Inline {
    fn read(nbt: &Compound) -> Self {
        let d = TrialSpawnerConfig::DEFAULT;
        let float = |key: &str, default: f32| nbt.get(key).and_then(float_of).unwrap_or(default);
        let potentials = nbt
            .get_list("spawn_potentials")
            .map(|list| {
                list.iter()
                    .filter_map(|tag| match tag {
                        Tag::Compound(c) => {
                            let data = c.get_compound("data")?.clone();
                            Some((weight_of(c), data))
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let loot_tables = match nbt.get_list("loot_tables_to_eject") {
            Some(list) => list
                .iter()
                .filter_map(|tag| match tag {
                    Tag::Compound(c) => Some((weight_of(c), c.get_str("data")?.to_owned())),
                    _ => None,
                })
                .collect(),
            None => d
                .loot_tables_to_eject
                .iter()
                .map(|(w, n)| (*w, (*n).to_owned()))
                .collect(),
        };
        Self {
            spawn_range: nbt
                .get("spawn_range")
                .map_or(d.spawn_range, |_| number(nbt, "spawn_range").clamp(1, 128)),
            total_mobs: float("total_mobs", d.total_mobs),
            simultaneous_mobs: float("simultaneous_mobs", d.simultaneous_mobs),
            total_per_player: float("total_mobs_added_per_player", d.total_mobs_added_per_player),
            simultaneous_per_player: float(
                "simultaneous_mobs_added_per_player",
                d.simultaneous_mobs_added_per_player,
            ),
            ticks_between_spawn: nbt
                .get("ticks_between_spawn")
                .map_or(d.ticks_between_spawn, |_| {
                    number(nbt, "ticks_between_spawn").max(0)
                }),
            potentials,
            loot_tables,
            items_to_drop_when_ominous: nbt
                .get_str("items_to_drop_when_ominous")
                .unwrap_or(d.items_to_drop_when_ominous)
                .to_owned(),
        }
    }
}

impl Config {
    fn spawn_range(&self) -> i64 {
        match self {
            Self::Named(c) => c.spawn_range,
            Self::Inline(c) => c.spawn_range,
        }
    }

    fn ticks_between_spawn(&self) -> i64 {
        match self {
            Self::Named(c) => c.ticks_between_spawn,
            Self::Inline(c) => c.ticks_between_spawn,
        }
    }

    fn target_total_mobs(&self, additional: usize) -> usize {
        match self {
            Self::Named(c) => c.target_total_mobs(additional),
            Self::Inline(c) => {
                (c.total_mobs + c.total_per_player * additional as f32).floor() as usize
            }
        }
    }

    fn target_simultaneous_mobs(&self, additional: usize) -> usize {
        match self {
            Self::Named(c) => c.target_simultaneous_mobs(additional),
            Self::Inline(c) => {
                (c.simultaneous_mobs + c.simultaneous_per_player * additional as f32).floor()
                    as usize
            }
        }
    }

    fn potentials(&self) -> Vec<(u64, Compound)> {
        match self {
            Self::Named(c) => c
                .spawn_potentials
                .iter()
                .map(|p| (p.weight, spawn_data_of(p)))
                .collect(),
            Self::Inline(c) => c.potentials.clone(),
        }
    }

    fn has_potentials(&self) -> bool {
        match self {
            Self::Named(c) => !c.spawn_potentials.is_empty(),
            Self::Inline(c) => !c.potentials.is_empty(),
        }
    }

    fn loot_tables(&self) -> Vec<(u64, String)> {
        match self {
            Self::Named(c) => c
                .loot_tables_to_eject
                .iter()
                .map(|(w, n)| (*w, (*n).to_owned()))
                .collect(),
            Self::Inline(c) => c.loot_tables.clone(),
        }
    }

    fn ominous_items_table(&self) -> &str {
        match self {
            Self::Named(c) => c.items_to_drop_when_ominous,
            Self::Inline(c) => &c.items_to_drop_when_ominous,
        }
    }
}

#[derive(Clone)]
struct Settings {
    normal: Config,
    ominous: Config,
    cooldown: i64,
    range: i32,
}

fn config_named(nbt: &Compound, key: &str) -> Config {
    match nbt.get(key) {
        Some(Tag::Compound(inline)) => Config::Inline(std::sync::Arc::new(Inline::read(inline))),
        _ => Config::Named(
            nbt.get_str(key)
                .and_then(TrialSpawnerConfig::get)
                .unwrap_or(&TrialSpawnerConfig::DEFAULT),
        ),
    }
}

impl Settings {
    fn read(nbt: &Compound) -> Self {
        let cooldown = match nbt.get("target_cooldown_length") {
            Some(_) => number(nbt, "target_cooldown_length").max(0),
            None => i64::from(DEFAULT_COOLDOWN),
        };
        let range = match nbt.get("required_player_range") {
            Some(_) => number(nbt, "required_player_range").clamp(1, 128) as i32,
            None => DEFAULT_PLAYER_RANGE,
        };
        Self {
            normal: config_named(nbt, "normal_config"),
            ominous: config_named(nbt, "ominous_config"),
            cooldown,
            range,
        }
    }
}

fn is_spawner(nbt: &Compound) -> bool {
    nbt.get_str("id") == Some(SPAWNER_ID)
}

fn pos_long(pos: BlockPos) -> i64 {
    ((i64::from(pos.x) & 0x3FF_FFFF) << 38)
        | ((i64::from(pos.z) & 0x3FF_FFFF) << 12)
        | (i64::from(pos.y) & 0xFFF)
}

fn center(pos: BlockPos) -> [f64; 3] {
    [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    ]
}

fn dist_sq_blocks(a: BlockPos, b: BlockPos) -> i64 {
    let (dx, dy, dz) = (
        i64::from(a.x - b.x),
        i64::from(a.y - b.y),
        i64::from(a.z - b.z),
    );
    dx * dx + dy * dy + dz * dz
}

fn lookup(world: &dyn RegionWorld) -> impl Fn(BlockPos) -> BlockState + '_ {
    |pos| world.block(pos).unwrap_or(Block::Air.default_state())
}

fn in_line_of_sight(world: &dyn RegionWorld, spawner: BlockPos, from: [f64; 3]) -> bool {
    let level = lookup(world);
    match raycast::cast_colliders(from, center(spawner), &level) {
        None => true,
        Some(hit) => hit.pos == spawner,
    }
}

/// The players close enough to the spawner, optionally with a clear line
/// from their eyes to it.
fn detect(
    world: &dyn RegionWorld,
    players: &[SeenPlayer],
    pos: BlockPos,
    range: i32,
    line_of_sight: bool,
) -> Vec<u128> {
    let limit = i64::from(range) * i64::from(range);
    players
        .iter()
        .filter(|p| !p.looks.creative)
        .filter(|p| dist_sq_blocks(physics::block_pos(p.position), pos) < limit)
        .filter(|p| {
            !line_of_sight
                || in_line_of_sight(
                    world,
                    pos,
                    [
                        p.position[0],
                        p.position[1] + p.looks.eye_height,
                        p.position[2],
                    ],
                )
        })
        .map(|p| p.uuid)
        .collect()
}

#[derive(Clone, Copy)]
struct Mob {
    id: i32,
    position: [f64; 3],
    height: f64,
    alive: bool,
}

fn local_mobs(world: &dyn RegionWorld, wanted: &[u128]) -> HashMap<u128, Mob> {
    let mut found = HashMap::new();
    if wanted.is_empty() {
        return found;
    }
    for id in world.entity_ids() {
        let Some(tracked) = world.entity(id) else {
            continue;
        };
        let base = tracked.entity.base();
        if !wanted.contains(&base.uuid) {
            continue;
        }
        let bounds = tracked.entity.bounds();
        found.insert(
            base.uuid,
            Mob {
                id,
                position: base.position,
                height: bounds[4] - bounds[1],
                alive: tracked.entity.living().is_none_or(|l| l.alive),
            },
        );
    }
    found
}

struct Tick<'a> {
    world: &'a mut dyn RegionWorld,
    pos: BlockPos,
    now: i64,
    settings: Settings,
    ominous: bool,
    state: State,
    data: Data,
    players: Vec<SeenPlayer>,
    mobs: HashMap<u128, Mob>,
    client_dirty: bool,
}

impl Tick<'_> {
    fn config(&self) -> Config {
        if self.ominous {
            self.settings.ominous.clone()
        } else {
            self.settings.normal.clone()
        }
    }

    fn can_spawn(&self) -> bool {
        let shared = self.world.shared();
        if !shared.game_rules.borrow().bool("spawner_blocks_work") {
            return false;
        }
        *shared.difficulty.borrow() != tuff_core::world::player::Difficulty::Peaceful
            && shared.game_rules.borrow().bool("spawn_mobs")
    }

    fn pick_spawn_data(&mut self) -> Option<Compound> {
        let potentials = self.config().potentials();
        let total: u64 = potentials.iter().map(|p| p.0).sum();
        if total == 0 {
            return None;
        }
        let mut roll = self.world.rng().gen_range(0..total);
        let chosen = potentials.into_iter().find(|p| {
            if roll < p.0 {
                true
            } else {
                roll -= p.0;
                false
            }
        })?;
        Some(chosen.1)
    }

    fn next_spawn_data(&mut self) -> Compound {
        if let Some(data) = &self.data.next_spawn_data {
            return data.clone();
        }
        let chosen = self.pick_spawn_data().unwrap_or_default();
        self.data.next_spawn_data = Some(chosen.clone());
        self.client_dirty = true;
        chosen
    }

    fn has_mob_to_spawn(&mut self) -> bool {
        let data = self.next_spawn_data();
        let has_id = data
            .get_compound("entity")
            .and_then(|e| e.get_str("id"))
            .is_some();
        has_id || self.config().has_potentials()
    }

    fn display_entity_exists(&mut self) -> bool {
        let data = self.next_spawn_data();
        data.get_compound("entity")
            .and_then(|e| e.get_str("id"))
            .is_some_and(|id| EntityType::from_name(id).is_some())
    }

    fn level_event(&mut self, event: i32, pos: BlockPos, data: i32) {
        self.world.broadcast_level_event(event, pos, data);
    }

    fn sound(&mut self, sound: SoundEvent) {
        self.world.play_sound(sound, self.pos, 1.0, 1.0, None);
    }

    fn untrack_gone_mobs(&mut self) -> bool {
        let pos = self.pos;
        let now = self.now as u64;
        let current = self.data.current.clone();
        let local = local_mobs(self.world, &current);
        self.mobs = local.clone();
        let mut remote_needed = false;
        let mut kept = Vec::with_capacity(current.len());
        for uuid in &current {
            let alive_here = local.get(uuid).copied().filter(|m| m.alive);
            let keep = match (local.get(uuid), alive_here) {
                (Some(_), Some(mob)) => {
                    self.world
                        .state()
                        .get::<Remote>()
                        .absent
                        .remove(&(pos, *uuid));
                    dist_sq_blocks(physics::block_pos(mob.position), pos)
                        <= MAX_TRACKING_DISTANCE_SQ
                }
                (Some(_), None) => false,
                (None, _) => {
                    remote_needed = true;
                    let remote = self.world.state().get::<Remote>();
                    let seen = remote.seen.get(&(pos, *uuid)).copied();
                    if seen.is_some_and(|at| now.saturating_sub(at) <= REMOTE_KEEP_TICKS) {
                        remote.absent.remove(&(pos, *uuid));
                        true
                    } else {
                        let first = *remote.absent.entry((pos, *uuid)).or_insert(now);
                        now.saturating_sub(first) < ABSENT_GRACE_TICKS
                    }
                }
            };
            if keep {
                kept.push(*uuid);
            } else {
                let remote = self.world.state().get::<Remote>();
                remote.seen.remove(&(pos, *uuid));
                remote.absent.remove(&(pos, *uuid));
            }
        }
        if remote_needed {
            ask_neighbours(self.world, pos, &current);
        }
        let removed = kept.len() != current.len();
        self.data.current = kept;
        removed
    }

    fn try_detect_players(&mut self) {
        if (pos_long(self.pos) + self.now).rem_euclid(PLAYER_SCAN_TICKS) != 0 {
            return;
        }
        if self.state == State::Cooldown && self.ominous {
            return;
        }
        let range = self.settings.range;
        let in_sight = detect(self.world, &self.players, self.pos, range, true);
        let mut became_ominous = false;
        if !self.ominous && !in_sight.is_empty() {
            let carrier = omen_carrier(&self.players, &in_sight);
            if let Some((player, effect)) = carrier {
                if effect == MobEffect::BadOmen {
                    player.handle.send(PlayerEvent::ConvertEffect {
                        from: MobEffect::BadOmen,
                        to: MobEffect::TrialOmen,
                        ticks_per_level: TRIAL_OMEN_TICKS_PER_LEVEL,
                    });
                }
                let eye = physics::block_pos([
                    player.position[0],
                    player.position[1] + player.looks.eye_height,
                    player.position[2],
                ]);
                self.level_event(EVENT_BECOME_OMINOUS, eye, 0);
                self.apply_ominous();
                became_ominous = true;
            }
        }
        if self.state == State::Cooldown && !became_ominous {
            return;
        }
        let found = if self.data.detected.is_empty() {
            in_sight
        } else {
            detect(self.world, &self.players, self.pos, range, false)
        };
        let before = self.data.detected.len();
        for uuid in found {
            if !self.data.detected.contains(&uuid) {
                self.data.detected.push(uuid);
            }
        }
        if self.data.detected.len() != before {
            self.data.next_mob_spawns_at = self
                .data
                .next_mob_spawns_at
                .max(self.now + DETECT_BUFFER_TICKS);
            if !became_ominous {
                let event = if self.ominous {
                    EVENT_DETECT_PLAYER_OMINOUS
                } else {
                    EVENT_DETECT_PLAYER
                };
                let count = self.data.detected.len() as i32;
                self.level_event(event, self.pos, count);
            }
        }
    }

    fn apply_ominous(&mut self) {
        if let Some(state) = self.world.block(self.pos)
            && let Some(next) = state.with(OMINOUS, "true")
        {
            self.world.put_block(self.pos, next);
        }
        self.level_event(EVENT_BECOME_OMINOUS, self.pos, 1);
        self.ominous = true;
        let current = std::mem::take(&mut self.data.current);
        let mut remote = Vec::new();
        for uuid in &current {
            match self.mobs.get(uuid) {
                Some(mob) => {
                    let at = physics::block_pos(mob.position);
                    self.world.remove_entity(mob.id);
                    self.level_event(EVENT_SPAWN_MOB_AT, at, 0);
                }
                None => remote.push(*uuid),
            }
        }
        if !remote.is_empty() {
            discard_remote(self.world, remote);
        }
        let ominous = self.settings.ominous.clone();
        if ominous.has_potentials() {
            self.data.next_spawn_data = None;
        }
        self.data.total_spawned = 0;
        self.data.next_mob_spawns_at = self.now + ominous.ticks_between_spawn();
        self.data.cooldown_ends_at = self.now + ITEM_SPAWNER_INTERVAL_TICKS;
        self.client_dirty = true;
    }

    fn remove_ominous(&mut self) {
        if let Some(state) = self.world.block(self.pos)
            && let Some(next) = state.with(OMINOUS, "false")
        {
            self.world.put_block(self.pos, next);
        }
        self.ominous = false;
    }

    fn step(&mut self) -> State {
        let config = self.config();
        match self.state {
            State::Inactive => {
                if self.display_entity_exists() {
                    State::WaitingForPlayers
                } else {
                    State::Inactive
                }
            }
            State::WaitingForPlayers => {
                if !self.can_spawn() {
                    self.data.reset_statistics();
                    return State::WaitingForPlayers;
                }
                if !self.has_mob_to_spawn() {
                    return State::Inactive;
                }
                self.try_detect_players();
                if self.data.detected.is_empty() {
                    State::WaitingForPlayers
                } else {
                    State::Active
                }
            }
            State::Active => self.step_active(config),
            State::WaitingForRewardEjection => {
                let started = self.data.cooldown_ends_at - self.settings.cooldown;
                if self.now >= started + SHUTTER_DELAY_TICKS {
                    self.sound(SoundEvent::BlockTrialSpawnerOpenShutter);
                    State::EjectingReward
                } else {
                    State::WaitingForRewardEjection
                }
            }
            State::EjectingReward => self.step_ejecting(config),
            State::Cooldown => {
                self.try_detect_players();
                if !self.data.detected.is_empty() {
                    self.data.total_spawned = 0;
                    self.data.next_mob_spawns_at = 0;
                    return State::Active;
                }
                if self.now >= self.data.cooldown_ends_at {
                    self.remove_ominous();
                    self.data.reset();
                    State::WaitingForPlayers
                } else {
                    State::Cooldown
                }
            }
        }
    }

    fn step_active(&mut self, config: Config) -> State {
        if !self.can_spawn() {
            self.data.reset_statistics();
            return State::WaitingForPlayers;
        }
        if !self.has_mob_to_spawn() {
            return State::Inactive;
        }
        let additional = self.data.detected.len().saturating_sub(1);
        self.try_detect_players();
        if self.ominous {
            self.spawn_item_spawner();
        }
        if self.data.total_spawned as usize >= config.target_total_mobs(additional) {
            if self.data.current.is_empty() {
                self.data.cooldown_ends_at = self.now + self.settings.cooldown;
                self.data.total_spawned = 0;
                self.data.next_mob_spawns_at = 0;
                return State::WaitingForRewardEjection;
            }
        } else if self.now >= self.data.next_mob_spawns_at
            && self.data.current.len() < config.target_simultaneous_mobs(additional)
            && let Some(uuid) = self.spawn_mob()
        {
            self.data.current.push(uuid);
            self.data.total_spawned += 1;
            self.data.next_mob_spawns_at = self.now + config.ticks_between_spawn();
            if let Some(next) = self.pick_spawn_data() {
                self.data.next_spawn_data = Some(next);
                self.client_dirty = true;
            }
        }
        State::Active
    }

    fn step_ejecting(&mut self, config: Config) -> State {
        let started = self.data.cooldown_ends_at - self.settings.cooldown;
        if (self.now - started) as f32 % EJECTION_INTERVAL_TICKS as f32 != 0.0 {
            return State::EjectingReward;
        }
        if self.data.detected.is_empty() {
            self.sound(SoundEvent::BlockTrialSpawnerCloseShutter);
            self.data.ejecting = None;
            return State::Cooldown;
        }
        if self.data.ejecting.is_none() {
            self.data.ejecting = pick_weighted(&config.loot_tables(), self.world.rng());
        }
        if let Some(table) = self.data.ejecting.clone() {
            self.eject_reward(&table);
        }
        self.data.detected.remove(0);
        State::EjectingReward
    }

    fn eject_reward(&mut self, table: &str) {
        let drops = self
            .world
            .shared()
            .block_loot()
            .roll(table, self.world.rng());
        if drops.is_empty() {
            return;
        }
        let origin = [
            f64::from(self.pos.x) + 0.5,
            f64::from(self.pos.y) + 1.2 - 0.125,
            f64::from(self.pos.z) + 0.5,
        ];
        for stack in drops {
            eject_item(self.world, stack, origin);
        }
        self.level_event(EVENT_EJECT_ITEM, self.pos, 0);
    }

    fn spawn_mob(&mut self) -> Option<u128> {
        let pos = self.pos;
        let spawn_data = self.next_spawn_data();
        let mut saved = spawn_data.get_compound("entity")?.clone();
        let kind = saved.get_str("id").and_then(EntityType::from_name)?;
        let range = f64::from(self.config().spawn_range() as i32);
        let at = match saved.get_list("Pos").and_then(numbers3) {
            Some(at) => at,
            None => {
                let rng = self.world.rng();
                [
                    f64::from(pos.x) + (rng.r#gen::<f64>() - rng.r#gen::<f64>()) * range + 0.5,
                    f64::from(pos.y + rng.gen_range(0..3) - 1),
                    f64::from(pos.z) + (rng.r#gen::<f64>() - rng.r#gen::<f64>()) * range + 0.5,
                ]
            }
        };
        if !physics::no_collision(physics::spawn_box(kind, at), &|p| self.world.block(p)) {
            return None;
        }
        if !in_line_of_sight(self.world, pos, at) {
            return None;
        }
        let spawn_block = physics::block_pos(at);
        if let Some(rule) = self.world.shared().entity_types.spawn_rule(kind) {
            let seed = self.world.rng().r#gen();
            if !rule_holds(
                self.world,
                seed,
                kind,
                spawn_block,
                rule,
                SpawnReason::TrialSpawner,
            ) {
                return None;
            }
        }
        if let Some(rules) = spawn_data.get_compound("custom_spawn_rules")
            && !light_rules_hold(self.world, rules, spawn_block)
        {
            return None;
        }
        let fresh = saved.len() == 1;
        let yaw = self.world.rng().r#gen::<f32>() * 360.0;
        saved.insert("Pos", Tag::List(at.map(Tag::Double).to_vec()));
        saved.insert(
            "Rotation",
            Tag::List(vec![Tag::Float(yaw), Tag::Float(0.0)]),
        );
        saved.insert("PersistenceRequired", true);
        if let Some(equipment) = spawn_data.get_compound("equipment") {
            tuff_vanilla_common::spawn_equipment::equip(self.world, equipment, &mut saved);
        }
        let id = self.world.shared().next_entity_id();
        let mut entity = self.world.shared().entity_types.load(&saved, id)?;
        if !physics::unobstructed_for(entity.base().kind, entity.bounds(), &|p| {
            self.world.block(p)
        }) {
            return None;
        }
        if fresh {
            self.world.finalize_spawn(&mut entity, 0);
        }
        let uuid = entity.base().uuid;
        self.world.add_entity(entity);
        let flame = i32::from(self.ominous);
        self.level_event(EVENT_SPAWN, pos, flame);
        self.level_event(EVENT_SPAWN_MOB_AT, spawn_block, flame);
        Some(uuid)
    }

    fn spawn_item_spawner(&mut self) {
        let config = self.config();
        let items = dispensing_items(self.world, &config, self.pos);
        let total: u64 = items.iter().map(|(_, w)| *w).sum();
        if total == 0 || self.now < self.data.cooldown_ends_at {
            return;
        }
        let mut roll = self.world.rng().gen_range(0..total);
        let Some((stack, _)) = items.iter().find(|(_, w)| {
            if roll < *w {
                true
            } else {
                roll -= *w;
                false
            }
        }) else {
            return;
        };
        let Some(at) = self.item_spawner_position() else {
            return;
        };
        let id = self.world.shared().next_entity_id();
        let delay = self.world.rng().gen_range(60..=120);
        let uuid = self.world.rng().r#gen();
        let mut entity = OminousItemSpawner::new(id, uuid, at, stack.clone(), delay);
        entity.base.position = at;
        self.world.add_entity(Box::new(entity));
        let rng = self.world.rng();
        let pitch = (rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.2 + 1.0;
        self.world.play_sound(
            SoundEvent::BlockTrialSpawnerSpawnItemBegin,
            physics::block_pos(at),
            1.0,
            pitch,
            None,
        );
        self.data.cooldown_ends_at = self.now + ITEM_SPAWNER_INTERVAL_TICKS;
    }

    fn item_spawner_position(&mut self) -> Option<[f64; 3]> {
        let range_sq = f64::from(self.settings.range) * f64::from(self.settings.range);
        let middle = center(self.pos);
        let nearby: Vec<(&SeenPlayer, f64)> = self
            .players
            .iter()
            .filter(|p| self.data.detected.contains(&p.uuid) && !p.looks.creative)
            .filter(|p| tuff_core::world::distance_sq(p.position, middle) <= range_sq)
            .map(|p| (p, p.looks.height))
            .collect();
        if nearby.is_empty() {
            return None;
        }
        let mob_positions: Vec<([f64; 3], f64)> = self
            .data
            .current
            .iter()
            .filter_map(|uuid| self.mobs.get(uuid))
            .filter(|m| m.alive && tuff_core::world::distance_sq(m.position, middle) <= range_sq)
            .map(|m| (m.position, m.height))
            .collect();
        let choose_mobs = self.world.rng().r#gen::<bool>();
        let eligible: Vec<([f64; 3], f64)> = if choose_mobs {
            mob_positions
        } else {
            nearby.iter().map(|(p, h)| (p.position, *h)).collect()
        };
        if eligible.is_empty() {
            return None;
        }
        let pick = if eligible.len() == 1 {
            0
        } else {
            self.world.rng().gen_range(0..eligible.len())
        };
        let (from, height) = eligible[pick];
        let lift = height + 2.0 + f64::from(self.world.rng().gen_range(0..4));
        let to = [from[0], from[1] + lift, from[2]];
        let level = lookup(self.world);
        let landed = raycast::cast_colliders(from, to, &level)
            .map(|hit| hit.pos)
            .unwrap_or_else(|| physics::block_pos(to));
        let down = [
            f64::from(landed.x) + 0.5,
            f64::from(landed.y) + 0.5 - 1.0,
            f64::from(landed.z) + 0.5,
        ];
        let below = physics::block_pos(down);
        let state = self.world.block(below)?;
        state.collision_boxes().is_empty().then_some(down)
    }
}

fn omen_carrier<'a>(
    players: &'a [SeenPlayer],
    in_sight: &[u128],
) -> Option<(&'a SeenPlayer, MobEffect)> {
    let mut bad = None;
    for uuid in in_sight {
        let Some(player) = players.iter().find(|p| p.uuid == *uuid) else {
            continue;
        };
        if player.looks.has_effect(MobEffect::TrialOmen) {
            return Some((player, MobEffect::TrialOmen));
        }
        if player.looks.has_effect(MobEffect::BadOmen) {
            bad = Some(player);
        }
    }
    bad.map(|p| (p, MobEffect::BadOmen))
}

fn numbers3(list: &[Tag]) -> Option<[f64; 3]> {
    let value = |tag: &Tag| match tag {
        Tag::Double(v) => Some(*v),
        Tag::Float(v) => Some(f64::from(*v)),
        Tag::Int(v) => Some(f64::from(*v)),
        _ => None,
    };
    Some([
        value(list.first()?)?,
        value(list.get(1)?)?,
        value(list.get(2)?)?,
    ])
}

fn pick_weighted(entries: &[(u64, String)], rng: &mut StdRng) -> Option<String> {
    let total: u64 = entries.iter().map(|(w, _)| *w).sum();
    if total == 0 {
        return None;
    }
    let mut roll = rng.gen_range(0..total);
    entries.iter().find_map(|(weight, name)| {
        if roll < *weight {
            Some(name.clone())
        } else {
            roll -= *weight;
            None
        }
    })
}

fn json_tag(text: &str) -> Option<Tag> {
    Tag::from_snbt(text).ok()
}

fn spawn_data_of(potential: &SpawnPotential) -> Compound {
    let mut out = Compound::new();
    if let Some(Tag::Compound(entity)) = json_tag(potential.entity) {
        out.insert("entity", entity);
    }
    if let Some(Tag::Compound(rules)) = potential.custom_spawn_rules.and_then(json_tag) {
        out.insert("custom_spawn_rules", rules);
    }
    if let Some(Tag::Compound(equipment)) = potential.equipment.and_then(json_tag) {
        out.insert("equipment", equipment);
    }
    out
}

fn light_rules_hold(world: &dyn RegionWorld, rules: &Compound, pos: BlockPos) -> bool {
    let within = |tag: Option<&Tag>, value: i32| match tag {
        Some(Tag::Compound(range)) => {
            let low = range.get_int("min_inclusive").unwrap_or(0);
            let high = range.get_int("max_inclusive").unwrap_or(15);
            (low..=high).contains(&value)
        }
        Some(Tag::Int(exact)) => *exact == value,
        _ => true,
    };
    let (sky, block) = world.engine_light(pos).unwrap_or((0, 0));
    let darken = tuff_core::world::sky_darken(world.shared(), world.dimension_info());
    let effective_sky = i32::from(sky) - i32::from(darken);
    within(rules.get("block_light_limit"), i32::from(block))
        && within(rules.get("sky_light_limit"), effective_sky.max(0))
}

fn float_of(tag: &Tag) -> Option<f32> {
    match tag {
        Tag::Float(v) => Some(*v),
        Tag::Double(v) => Some(*v as f32),
        Tag::Int(v) => Some(*v as f32),
        _ => None,
    }
}

const DISPENSE_SPREAD: f64 = 0.017_227_5 * 2.0;

/// Drops `stack` out of the top of a block as a dispenser would, moving
/// upward.
pub fn eject_item(world: &mut dyn RegionWorld, stack: ItemStack, origin: [f64; 3]) {
    let rng = world.rng();
    let velocity = [
        physics::triangle(rng, 0.0, DISPENSE_SPREAD),
        physics::triangle(rng, 0.2, DISPENSE_SPREAD),
        physics::triangle(rng, 0.0, DISPENSE_SPREAD),
    ];
    world.spawn_item_at(stack, origin, velocity, 0);
}

fn low_resolution_seed(world: &dyn RegionWorld, pos: BlockPos) -> u64 {
    let x = (pos.x as f32 / 30.0).floor() as i32;
    let y = (pos.y as f32 / 20.0).floor() as i32;
    let z = (pos.z as f32 / 30.0).floor() as i32;
    world
        .shared()
        .seed
        .wrapping_add(pos_long(BlockPos::new(x, y, z))) as u64
}

/// The ominous item pool: the config's table rolled once with a seed from
/// the spawner's coarse position, each drop weighted by its count.
fn dispensing_items(
    world: &dyn RegionWorld,
    config: &Config,
    pos: BlockPos,
) -> Vec<(ItemStack, u64)> {
    let mut rng = StdRng::seed_from_u64(low_resolution_seed(world, pos));
    world
        .shared()
        .block_loot()
        .roll(config.ominous_items_table(), &mut rng)
        .into_iter()
        .map(|stack| {
            let weight = u64::from(stack.count);
            (stack.with_count(1), weight)
        })
        .collect()
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

fn ask_neighbours(world: &mut dyn RegionWorld, spawner: BlockPos, wanted: &[u128]) {
    let now = world.tick();
    let remote = world.state().get::<Remote>();
    if remote
        .asked
        .get(&spawner)
        .is_some_and(|at| now.saturating_sub(*at) < REMOTE_QUERY_TICKS)
    {
        return;
    }
    remote.asked.insert(spawner, now);
    let home = world.handle();
    for handle in neighbours(world) {
        let wanted = wanted.to_vec();
        let home = home.clone();
        handle.internal(RegionInternal::Run(RegionTask::new(move |other| {
            let alive: Vec<u128> = other
                .entity_ids()
                .into_iter()
                .filter_map(|id| other.entity(id))
                .filter(|t| wanted.contains(&t.entity.base().uuid))
                .filter(|t| t.entity.living().is_none_or(|l| l.alive))
                .filter(|t| {
                    dist_sq_blocks(physics::block_pos(t.entity.base().position), spawner)
                        <= MAX_TRACKING_DISTANCE_SQ
                })
                .map(|t| t.entity.base().uuid)
                .collect();
            let at = other.tick();
            home.internal(RegionInternal::Run(RegionTask::new(move |here| {
                let remote = here.state().get::<Remote>();
                for uuid in alive {
                    remote.seen.insert((spawner, uuid), at);
                }
            })));
        })));
    }
}

fn discard_remote(world: &mut dyn RegionWorld, wanted: Vec<u128>) {
    for handle in neighbours(world) {
        let wanted = wanted.clone();
        handle.internal(RegionInternal::Run(RegionTask::new(move |other| {
            let ids: Vec<i32> = other
                .entity_ids()
                .into_iter()
                .filter(|id| {
                    other
                        .entity(*id)
                        .is_some_and(|t| wanted.contains(&t.entity.base().uuid))
                })
                .collect();
            for id in ids {
                other.remove_entity(id);
            }
        })));
    }
}

pub fn tick(world: &mut dyn RegionWorld) {
    let generation = world.block_entity_generation();
    if world.state().get::<Spawners>().1 != Some(generation) {
        let mut found = world.raw_block_entities_where(&is_spawner);
        found.sort_unstable_by_key(|p| (p.y, p.z, p.x));
        let live: std::collections::HashSet<BlockPos> = found.iter().copied().collect();
        world
            .state()
            .get::<SettingsCache>()
            .0
            .retain(|p, _| live.contains(p));
        let cache = world.state().get::<Spawners>();
        cache.0 = found;
        cache.1 = Some(generation);
    }
    let positions = std::mem::take(&mut world.state().get::<Spawners>().0);
    for &pos in &positions {
        tick_spawner(world, pos);
    }
    world.state().get::<Spawners>().0 = positions;
}

fn tick_spawner(world: &mut dyn RegionWorld, pos: BlockPos) {
    let Some(block) = world.block(pos) else {
        return;
    };
    if block.block() != Block::TrialSpawner {
        return;
    }
    let Some(nbt) = world.raw_block_entity(pos).filter(|n| is_spawner(n)) else {
        return;
    };
    let state = block
        .get(STATE)
        .and_then(State::parse)
        .unwrap_or(State::Inactive);
    let data = Data::read(nbt);
    let initial = data.clone();
    let key: Vec<Option<Tag>> = SETTING_KEYS.iter().map(|k| nbt.get(k).cloned()).collect();
    let Some(settings) = cached_settings(world, pos, key) else {
        return;
    };
    let ominous = block.get(OMINOUS) == Some("true");
    let players = world.seen_player_details();
    let now = world.tick() as i64;
    let mut tick = Tick {
        world,
        pos,
        now,
        settings,
        ominous,
        state,
        data,
        players,
        mobs: HashMap::new(),
        client_dirty: false,
    };
    if tick.untrack_gone_mobs() {
        tick.data.next_mob_spawns_at = now + tick.config().ticks_between_spawn();
    }
    let before = tick.data.clone();
    let next = tick.step();
    let Tick {
        world,
        data,
        client_dirty,
        ..
    } = tick;
    let state_changed = next != state;
    if state_changed
        && let Some(current) = world.block(pos)
        && let Some(updated) = current.with(STATE, next.name())
    {
        world.put_block(pos, updated);
    }
    let Some(nbt) = world.raw_block_entity_mut(pos) else {
        return;
    };
    if data == initial {
        return;
    }
    data.write(nbt);
    let spin_changed =
        next == State::Active && data.next_mob_spawns_at != before.next_mob_spawns_at;
    if state_changed || client_dirty || spin_changed {
        world.block_entity_changed(pos);
    } else {
        world.mark_blocks_dirty(pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn inline_config_overrides_defaults() {
        let Ok(Tag::Compound(nbt)) = Tag::from_snbt(
            "{normal_config: {total_mobs: 3.0f, simultaneous_mobs: 1.0f, ticks_between_spawn: 10, \
             spawn_potentials: [{weight: 2, data: {entity: {id: \"minecraft:zombie\"}}}], \
             loot_tables_to_eject: [{weight: 3, data: \"minecraft:x\"}]}}",
        ) else {
            panic!("snbt")
        };
        let settings = Settings::read(&nbt);
        let config = settings.normal;
        assert_eq!(config.target_total_mobs(1), 5);
        assert_eq!(config.target_simultaneous_mobs(0), 1);
        assert_eq!(config.ticks_between_spawn(), 10);
        assert_eq!(config.spawn_range(), 4);
        assert_eq!(config.potentials()[0].0, 2);
        assert_eq!(config.loot_tables(), vec![(3, "minecraft:x".to_owned())]);
    }
}
