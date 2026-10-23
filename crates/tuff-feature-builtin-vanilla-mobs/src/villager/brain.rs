//! A villager's day: its schedule picks an activity (work at its job site,
//! meet at the bell, sleep in its bed, wander, play), while a core runs
//! throughout: claiming beds, job sites and bells nearby, taking up or
//! losing a profession, panicking near hostile mobs and after being hurt,
//! opening and closing doors on its way, and staying with a trading
//! player. Its memories save under the game's `Brain` keys.

use rand::Rng;
use tuff_vanilla_data::registry::{Activity, EntityType, PointOfInterestType, VillagerProfession};
use tuff_vanilla_data::{BlockState, tag};

mod bell;
mod farm;
mod gift;
mod play;
mod raid;
mod show;
mod social;
mod wander;

pub use social::{Facts, Grudges};

use super::{Villager, event};
use crate::physics::block_pos;
use crate::world::poi::Occupancy;
use crate::{MobCore, random_pos};
use tuff_core::world::entity::{Effect, Notice, TickContext};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const HOME: &str = "minecraft:home";
const JOB_SITE: &str = "minecraft:job_site";
const POTENTIAL_JOB_SITE: &str = "minecraft:potential_job_site";
const MEETING_POINT: &str = "minecraft:meeting_point";
const LAST_SLEPT: &str = "minecraft:last_slept";
const LAST_WOKEN: &str = "minecraft:last_woken";
const LAST_WORKED_AT_POI: &str = "minecraft:last_worked_at_poi";
const HEARD_BELL_TIME: &str = "minecraft:heard_bell_time";

/// Blocks within which points of interest are claimed.
const SCAN_RANGE: i32 = 48;
/// Remembered points farther than this are not checked.
const VALIDATE_RANGE: f64 = 16.0;
const SPEED: f64 = 0.5;
const STROLL_SPEED: f64 = 0.4;
const PANIC_SPEED: f64 = SPEED * 1.5;
const HURT_MEMORY_TICKS: u64 = 40;
const SAFE_DISTANCE_SQ: f64 = 36.0;
const FLEE_DISTANCE: f64 = 6.0;
const WORK_CHECK_INTERVAL: u64 = 300;
const WORK_DISTANCE: f64 = 1.73;
const POTENTIAL_JOB_SITE_TIMEOUT: u64 = 1200;
const ASSIGN_DISTANCE: f64 = 2.0;
const SLEEP_DISTANCE: f64 = 2.0;
const WOKEN_COOLDOWN: i64 = 100;
const STROLL_COOLDOWN: u64 = 180;
const STROLL_TO_SITE_COOLDOWN: u64 = 80;
const STROLL_TO_FIELD_COOLDOWN: u64 = 100;
const STROLL_TO_SITE_RANGE: f64 = 10.0;
const GOSSIP_DISTANCE_SQ: f64 = 5.0;
const GOSSIP_TOPICS: usize = 10;
const BREED_RANGE: f64 = 8.0;
const BIRTH_TICKS: u64 = 275;
const BIRTH_JITTER: u64 = 50;
const DOOR_CLOSE_DISTANCE: f64 = 3.0;
const WANTED_ITEM_RANGE: f64 = 4.0;

/// A position in a dimension, as the game remembers places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalPos {
    pub dimension: String,
    pub pos: BlockPos,
}

impl GlobalPos {
    fn from_nbt(tag: &Tag) -> Option<Self> {
        let value = tag.as_compound()?.get_compound("value")?;
        let [x, y, z] = value.get_int_array("pos")? else {
            return None;
        };
        Some(Self {
            dimension: value.get_str("dimension")?.to_owned(),
            pos: BlockPos::new(*x, *y, *z),
        })
    }

    fn to_nbt(&self) -> Tag {
        let value = Compound::new()
            .with("dimension", self.dimension.as_str())
            .with(
                "pos",
                Tag::IntArray(vec![self.pos.x, self.pos.y, self.pos.z]),
            );
        Tag::Compound(Compound::new().with("value", value))
    }
}

/// What a villager remembers across saves.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Memories {
    pub home: Option<GlobalPos>,
    pub job_site: Option<GlobalPos>,
    pub potential_job_site: Option<GlobalPos>,
    pub meeting_point: Option<GlobalPos>,
    pub last_slept: Option<i64>,
    pub last_woken: Option<i64>,
    pub last_worked_at_poi: Option<i64>,
    pub heard_bell: Option<i64>,
    /// Memories this server does not use, kept as saved.
    pub other: Compound,
}

fn time(tag: &Tag) -> Option<i64> {
    let value = tag.as_compound()?.get("value")?;
    tuff_core::world::entity::number(value).map(|v| v as i64)
}

impl Memories {
    /// Reads the saved `Brain` compound.
    pub fn load(brain: &Compound) -> Self {
        let mut memories = Self::default();
        let Some(saved) = brain.get_compound("memories") else {
            return memories;
        };
        for (key, tag) in saved.iter() {
            let known = match key {
                HOME => GlobalPos::from_nbt(tag).map(|p| memories.home = Some(p)),
                JOB_SITE => GlobalPos::from_nbt(tag).map(|p| memories.job_site = Some(p)),
                POTENTIAL_JOB_SITE => {
                    GlobalPos::from_nbt(tag).map(|p| memories.potential_job_site = Some(p))
                }
                MEETING_POINT => GlobalPos::from_nbt(tag).map(|p| memories.meeting_point = Some(p)),
                LAST_SLEPT => time(tag).map(|t| memories.last_slept = Some(t)),
                LAST_WOKEN => time(tag).map(|t| memories.last_woken = Some(t)),
                LAST_WORKED_AT_POI => time(tag).map(|t| memories.last_worked_at_poi = Some(t)),
                HEARD_BELL_TIME => time(tag).map(|t| memories.heard_bell = Some(t)),
                _ => None,
            };
            if known.is_none() {
                memories.other.insert(key, tag.clone());
            }
        }
        memories
    }

    /// The `Brain` compound to save.
    pub fn save(&self) -> Compound {
        let mut saved = Compound::new();
        let places = [
            (HOME, &self.home),
            (JOB_SITE, &self.job_site),
            (POTENTIAL_JOB_SITE, &self.potential_job_site),
            (MEETING_POINT, &self.meeting_point),
        ];
        for (key, place) in places {
            if let Some(place) = place {
                saved.insert(key, place.to_nbt());
            }
        }
        let times = [
            (LAST_SLEPT, self.last_slept),
            (LAST_WOKEN, self.last_woken),
            (LAST_WORKED_AT_POI, self.last_worked_at_poi),
            (HEARD_BELL_TIME, self.heard_bell),
        ];
        for (key, value) in times {
            if let Some(value) = value {
                saved.insert(key, Compound::new().with("value", value));
            }
        }
        for (key, tag) in self.other.iter() {
            saved.insert(key, tag.clone());
        }
        Compound::new().with("memories", saved)
    }

    /// The points of interest these memories hold a claim on.
    pub fn claims(&self) -> Vec<BlockPos> {
        [
            &self.home,
            &self.job_site,
            &self.potential_job_site,
            &self.meeting_point,
        ]
        .into_iter()
        .flatten()
        .map(|p| p.pos)
        .collect()
    }
}

/// What a villager at its job site may do next.
#[derive(Debug, Clone, Copy)]
enum Chore {
    StrollAround,
    StrollToSite,
    StrollToField,
    Harvest,
    Bonemeal,
}

/// Which remembered point of interest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Home,
    JobSite,
    PotentialJobSite,
    MeetingPoint,
}

/// The state behind a villager's behaviour that is not saved.
#[derive(Debug, Default)]
pub struct Brain {
    pub memories: Memories,
    pub activity: Option<Activity>,
    /// The hostile mob it flees, by id.
    hostile: Option<i32>,
    /// Who hurt it (by id, when an entity) and when.
    hurt_by: Option<(Option<i32>, u64)>,
    last_damage_seen: Option<u64>,
    /// When each claim search may run next: home, meeting point, job site.
    next_acquire: [u64; 3],
    /// Unreachable points it skips until due, per claim search.
    acquire_retries: [std::collections::HashMap<BlockPos, PoiRetry>; 3],
    potential_since: Option<u64>,
    last_work_check: u64,
    next_stroll: u64,
    next_stroll_to_site: u64,
    next_stroll_to_field: u64,
    /// The bed it sleeps in.
    pub sleeping: Option<BlockPos>,
    next_sleep: i64,
    /// The villager it breeds with and when the baby comes.
    breeding: Option<(i32, u64)>,
    /// Doors it opened or walked through, to shut behind it.
    doors: Vec<BlockPos>,
    last_node: Option<BlockPos>,
    node_cooldown: i32,
    social: social::State,
    bell: bell::State,
    farm: farm::State,
    play: play::State,
    raid: raid::State,
    gift: gift::State,
    show: Option<show::State>,
    wander: wander::State,
}

/// When an unreachable point may be tried again: the delay grows by 40 to 79
/// ticks per attempt, up to 400, and the record lapses after 400 idle ticks.
#[derive(Debug)]
struct PoiRetry {
    previous_attempt: u64,
    next_attempt: u64,
    delay: u64,
}

impl PoiRetry {
    fn new(now: u64, rng: &mut impl Rng) -> Self {
        let mut retry = Self {
            previous_attempt: 0,
            next_attempt: 0,
            delay: 0,
        };
        retry.mark_attempt(now, rng);
        retry
    }

    fn mark_attempt(&mut self, now: u64, rng: &mut impl Rng) {
        self.previous_attempt = now;
        self.delay = (self.delay + rng.gen_range(0..40) + 40).min(400);
        self.next_attempt = now + self.delay;
    }

    fn still_valid(&self, now: u64) -> bool {
        now - self.previous_attempt < 400
    }
}

const PANIC_GOLEM_INTERVAL: u64 = 100;
const PANIC_GOLEM_VILLAGERS: usize = 3;

impl Brain {
    fn place(&self, place: Place) -> &Option<GlobalPos> {
        let m = &self.memories;
        match place {
            Place::Home => &m.home,
            Place::JobSite => &m.job_site,
            Place::PotentialJobSite => &m.potential_job_site,
            Place::MeetingPoint => &m.meeting_point,
        }
    }

    fn place_mut(&mut self, place: Place) -> &mut Option<GlobalPos> {
        let m = &mut self.memories;
        match place {
            Place::Home => &mut m.home,
            Place::JobSite => &mut m.job_site,
            Place::PotentialJobSite => &mut m.potential_job_site,
            Place::MeetingPoint => &mut m.meeting_point,
        }
    }

    pub fn is_panicking(&self) -> bool {
        self.activity == Some(Activity::Panic)
    }
}

fn dimension_key(cx: &TickContext) -> String {
    cx.shared.dimension(cx.dimension).key.clone()
}

fn center(pos: BlockPos) -> [f64; 3] {
    pos.center()
}

fn distance_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    tuff_core::world::distance_sq(a, b)
}

fn manhattan(a: BlockPos, b: BlockPos) -> i32 {
    a.dist_manhattan(b)
}

fn can_sleep_in(state: Option<BlockState>) -> bool {
    state.is_some_and(|s| {
        tuff_datapack::tags::is(tag::block::key::VILLAGERS_CAN_SLEEP_ON_BED, s.block())
            && s.occupied() == Some(false)
    })
}

/// Whether the remembered point still fits: the kind of point it must be.
fn fits(villager: &Villager, place: Place, kind: PointOfInterestType) -> bool {
    match place {
        Place::Home => kind == PointOfInterestType::Home,
        Place::MeetingPoint => kind == PointOfInterestType::Meeting,
        Place::JobSite => villager.data.profession.info().job_site == Some(kind),
        Place::PotentialJobSite => VillagerProfession::None
            .info()
            .acquirable_job_sites
            .contains(&kind),
    }
}

impl Villager {
    /// One step of the villager's behaviour, before it moves.
    pub(super) fn think(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if core.no_ai || !core.is_alive() {
            return;
        }
        self.note_bell(cx);
        self.update_raid_status(core, cx);
        self.social_upkeep(core, cx);
        self.scan_fields(core, cx);
        self.sense(core, cx);
        self.update_activity(core, cx);
        self.validate_places(core, cx);
        self.check_bed(core, cx);
        if !core.is_baby() {
            self.acquire(core, cx, Place::Home);
            self.acquire(core, cx, Place::MeetingPoint);
            if self.brain.memories.job_site.is_none() {
                self.acquire(core, cx, Place::PotentialJobSite);
            }
        }
        self.compete_for_job_site(core, cx);
        self.yield_job_site(core, cx);
        self.take_up_job(core, cx);
        self.reset_profession(core);
        if self.pick_up_items(core, cx) {
            return;
        }
        self.use_doors(core, cx);
        if let Some(trader) = self.trading {
            if let Some(player) = cx.players.iter().find(|p| p.id == trader.id) {
                let eye = [
                    player.position[0],
                    player.position[1] + player.eye_height,
                    player.position[2],
                ];
                core.look.set_look_at(eye, 30.0, 30.0);
            }
            core.navigation.stop();
            return;
        }
        self.show_trades_step(core, cx);
        match self.brain.activity {
            Some(Activity::Panic) => self.panic(core, cx),
            Some(Activity::Hide) => self.hide(core, cx),
            Some(Activity::PreRaid) => self.pre_raid(core, cx),
            Some(Activity::Raid) => self.raid(core, cx),
            Some(Activity::Work) => self.work(core, cx),
            Some(Activity::Meet) => self.meet(core, cx),
            Some(Activity::Rest) => self.rest(core, cx),
            Some(Activity::Play) => self.play(core, cx),
            _ => self.idle(core, cx),
        }
    }

    /// What it notices: hostile mobs close by and who hurt it.
    fn sense(&mut self, core: &MobCore, cx: &TickContext) {
        let position = core.base.position;
        self.brain.hostile = cx
            .entities
            .iter()
            .filter(|e| e.living.as_ref().is_some_and(|l| l.alive))
            .filter_map(|e| {
                let limit = tuff_vanilla_data::villager::hostile_distance(e.kind)? as f64;
                let d = distance_sq(e.position, position);
                (d <= limit * limit
                    && core.can_see_point(
                        [
                            e.position[0],
                            e.position[1] + f64::from(e.living.as_ref()?.eye_height),
                            e.position[2],
                        ],
                        cx,
                    ))
                .then_some((d, e.id))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id);
        if let Some((damage, tick)) = &core.last_damage
            && self.brain.last_damage_seen != Some(*tick)
        {
            self.brain.last_damage_seen = Some(*tick);
            self.brain.hurt_by = Some((damage.attacker.map(|(id, _)| id), *tick));
        }
    }

    fn scared(&self, core: &MobCore, cx: &TickContext) -> bool {
        if self.brain.hostile.is_some() {
            return true;
        }
        let Some((attacker, tick)) = self.brain.hurt_by else {
            return false;
        };
        if cx.tick <= tick + HURT_MEMORY_TICKS {
            return true;
        }
        attacker
            .and_then(|id| crate::target::view(cx, id))
            .is_some_and(|t| distance_sq(t.position, core.base.position) <= SAFE_DISTANCE_SQ)
    }

    fn update_activity(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let scared = self.scared(core, cx);
        let activity = if self.is_hiding() && self.raid_activity().is_none() {
            Activity::Hide
        } else if scared {
            Activity::Panic
        } else if let Some(activity) = self.raid_activity() {
            activity
        } else {
            if self.brain.activity == Some(Activity::Panic) {
                self.brain.hurt_by = None;
            }
            let dimension = cx.shared.dimension(cx.dimension);
            let clocks = cx.shared.clocks.borrow();
            let scheduled = dimension.environment().villager_activity(
                |clock| {
                    tuff_core::world::clock::clock_id(clock)
                        .map_or(0, |id| clocks.get(id).total_ticks)
                },
                core.is_baby(),
            );
            match scheduled {
                Activity::Work if self.brain.memories.job_site.is_none() => Activity::Idle,
                Activity::Meet if self.brain.memories.meeting_point.is_none() => Activity::Idle,
                other => other,
            }
        };
        if self.brain.activity != Some(activity) {
            if self.brain.activity == Some(Activity::Rest)
                || activity == Activity::Panic
                || activity == Activity::Hide
            {
                self.wake_up(core, cx);
            }
            if activity == Activity::Panic || activity == Activity::Hide {
                core.navigation.stop();
                self.brain.breeding = None;
            }
            self.end_interaction();
            self.stop_farming(core);
            self.brain.activity = Some(activity);
        }
    }

    /// Forgets points of interest that are gone, and a bed someone else
    /// sleeps in.
    fn validate_places(&mut self, core: &MobCore, cx: &TickContext) {
        let dimension = dimension_key(cx);
        for place in [
            Place::Home,
            Place::JobSite,
            Place::PotentialJobSite,
            Place::MeetingPoint,
        ] {
            let Some(remembered) = self.brain.place(place).clone() else {
                continue;
            };
            if remembered.dimension != dimension
                || distance_sq(center(remembered.pos), core.base.position)
                    >= VALIDATE_RANGE * VALIDATE_RANGE
            {
                continue;
            }
            let Some(state) = (cx.block)(remembered.pos) else {
                continue;
            };
            let kind = PointOfInterestType::of_state(state);
            let gone = !kind.is_some_and(|kind| fits(self, place, kind));
            let taken_bed = place == Place::Home
                && self.brain.sleeping != Some(remembered.pos)
                && tuff_datapack::tags::is(
                    tag::block::key::VILLAGERS_CAN_SLEEP_ON_BED,
                    state.block(),
                )
                && state.occupied() == Some(true);
            if gone || taken_bed {
                *self.brain.place_mut(place) = None;
            }
        }
    }

    /// Claims the closest free point of the kind it lacks that it can walk
    /// to, now and then.
    fn acquire(&mut self, core: &mut MobCore, cx: &mut TickContext, place: Place) {
        if self.brain.place(place).is_some() {
            return;
        }
        let slot = match place {
            Place::Home => 0,
            Place::MeetingPoint => 1,
            _ => 2,
        };
        let next = &mut self.brain.next_acquire[slot];
        if *next == 0 {
            *next = cx.tick + cx.rng.gen_range(0..20);
            return;
        }
        if cx.tick < *next {
            return;
        }
        *next = cx.tick + 20 + cx.rng.gen_range(0..20);
        let acquirable = self.data.profession.info().acquirable_job_sites;
        let accept = |kind: PointOfInterestType| match place {
            Place::Home => kind == PointOfInterestType::Home,
            Place::MeetingPoint => kind == PointOfInterestType::Meeting,
            _ => acquirable.contains(&kind),
        };
        let origin = block_pos(core.base.position);
        let now = cx.tick;
        let retries = &mut self.brain.acquire_retries[slot];
        retries.retain(|_, retry| retry.still_valid(now));
        let candidates: Vec<(BlockPos, PointOfInterestType)> = cx
            .pois
            .closest_first(origin, SCAN_RANGE, Occupancy::HasSpace, accept)
            .into_iter()
            .filter(|(pos, _)| match retries.get_mut(pos) {
                None => true,
                Some(retry) if now >= retry.next_attempt => {
                    retry.mark_attempt(now, cx.rng);
                    true
                }
                Some(_) => false,
            })
            .filter(|(pos, _)| place != Place::Home || can_sleep_in((cx.block)(*pos)))
            .take(5)
            .collect();
        if candidates.is_empty() {
            return;
        }
        let reach = candidates
            .iter()
            .map(|(_, kind)| kind.info().valid_range as i32)
            .max()
            .unwrap_or(1)
            .max(1);
        let targets: Vec<BlockPos> = candidates.iter().map(|(pos, _)| *pos).collect();
        let Some(path) = core.path_to_any(&targets, reach, cx) else {
            for target in targets {
                self.brain.acquire_retries[slot]
                    .entry(target)
                    .or_insert_with(|| PoiRetry::new(now, cx.rng));
            }
            return;
        };
        self.brain.acquire_retries[slot].clear();
        let pos = path.target;
        *self.brain.place_mut(place) = Some(GlobalPos {
            dimension: dimension_key(cx),
            pos,
        });
        if place == Place::PotentialJobSite {
            self.brain.potential_since = Some(cx.tick);
        } else {
            core.entity_event(event::HAPPY, cx);
        }
    }

    /// Walks to a job site it may take and takes it up when there.
    fn take_up_job(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let Some(potential) = self.brain.memories.potential_job_site.clone() else {
            return;
        };
        if distance_sq(center(potential.pos), core.base.position) < ASSIGN_DISTANCE.powi(2) {
            self.brain.memories.potential_job_site = None;
            self.brain.potential_since = None;
            let kind = cx.pois.kind_at(potential.pos);
            self.brain.memories.job_site = Some(potential);
            core.entity_event(event::HAPPY, cx);
            if self.data.profession == VillagerProfession::None
                && let Some(kind) = kind
                && let Some(profession) = VillagerProfession::ALL
                    .iter()
                    .copied()
                    .find(|p| p.info().job_site == Some(kind))
            {
                let data = super::VillagerData {
                    profession,
                    ..self.data
                };
                self.set_data(data, core);
            }
            return;
        }
        let walking = matches!(
            self.brain.activity,
            None | Some(Activity::Idle) | Some(Activity::Work) | Some(Activity::Play)
        );
        if self
            .brain
            .potential_since
            .is_some_and(|since| cx.tick > since + POTENTIAL_JOB_SITE_TIMEOUT)
        {
            self.brain.memories.potential_job_site = None;
            self.brain.potential_since = None;
            return;
        }
        if walking && core.navigation.is_done() {
            core.move_to_block(potential.pos, 1, SPEED, cx);
        }
    }

    /// A novice who never traded loses a profession without a job site.
    fn reset_profession(&mut self, core: &mut MobCore) {
        let profession = self.data.profession;
        let fireable =
            profession != VillagerProfession::None && profession != VillagerProfession::Nitwit;
        if fireable
            && self.brain.memories.job_site.is_none()
            && self.xp == 0
            && self.data.level <= 1
        {
            let data = super::VillagerData {
                profession: VillagerProfession::None,
                ..self.data
            };
            self.set_data(data, core);
        }
    }

    /// Takes food and wanted items lying within reach, and walks to those a
    /// little farther. True while it heads for one.
    fn pick_up_items(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        if !core.can_pick_up_loot || !cx.shared.game_rules.borrow().bool("mob_griefing") {
            return false;
        }
        let reach = crate::physics::inflate(core.bounds(), [1.0, 0.0, 1.0]);
        let wanted = |item: tuff_vanilla_data::Item, count: u8| {
            let stack = tuff_item::item::ItemPicture::new(item, count);
            self.wants(&stack)
                .then(|| self.room_for(&stack).min(count as u32) as u8)
        };
        let mut nearest: Option<(f64, [f64; 3])> = None;
        for entity in cx.entities {
            let Some((item, count, takeable)) = entity.item else {
                continue;
            };
            if !takeable {
                continue;
            }
            let Some(room) = wanted(item, count).filter(|room| *room > 0) else {
                continue;
            };
            if crate::physics::intersects(entity.bounds, reach) {
                let stack = tuff_item::item::ItemStack::mint(
                    item,
                    room,
                    tuff_item::item::MintReason::Spawn,
                );
                match self.add_item(stack) {
                    None => cx.effects.push(Effect::TakeItem {
                        item: entity.id,
                        count: room,
                    }),
                    Some(rest) => rest.burn(tuff_item::item::BurnReason::Destroyed),
                }
                return false;
            }
            let d = distance_sq(entity.position, core.base.position);
            if d <= WANTED_ITEM_RANGE * WANTED_ITEM_RANGE && nearest.is_none_or(|(n, _)| d < n) {
                nearest = Some((d, entity.position));
            }
        }
        let Some((_, position)) = nearest else {
            return false;
        };
        if self.trading.is_none() && self.brain.sleeping.is_none() {
            core.move_to_position(position, SPEED, cx);
            return true;
        }
        false
    }

    /// Opens the doors on its path and shuts those it passed.
    fn use_doors(&mut self, core: &MobCore, cx: &mut TickContext) {
        let Some(path) = core.navigation.path.as_ref().filter(|p| !p.is_done()) else {
            self.close_doors(core, None, None, cx);
            return;
        };
        if path.next == 0 {
            return;
        }
        let next = path.next_node().pos();
        if self.brain.last_node == Some(next) {
            self.brain.node_cooldown = 20;
        } else {
            self.brain.node_cooldown -= 1;
            if self.brain.node_cooldown > 0 {
                return;
            }
        }
        self.brain.last_node = Some(next);
        let from = path.nodes[path.next - 1].pos();
        for (pos, remember_anyway) in [(from, true), (next, false)] {
            let Some(state) = (cx.block)(pos) else {
                continue;
            };
            if !tuff_datapack::tags::is(tag::block::key::MOB_INTERACTABLE_DOORS, state.block()) {
                continue;
            }
            let closed = state.open() == Some(false);
            if closed {
                cx.effects.push(Effect::UseDoor { pos, open: true });
            }
            if (closed || remember_anyway) && !self.brain.doors.contains(&pos) {
                self.brain.doors.push(pos);
            }
        }
        self.close_doors(core, Some(from), Some(next), cx);
    }

    fn close_doors(
        &mut self,
        core: &MobCore,
        from: Option<BlockPos>,
        to: Option<BlockPos>,
        cx: &mut TickContext,
    ) {
        let position = core.base.position;
        let others: Vec<[f64; 3]> = cx
            .entities
            .iter()
            .filter(|e| e.kind == EntityType::Villager && e.id != core.base.id)
            .map(|e| e.position)
            .collect();
        self.brain.doors.retain(|pos| {
            if Some(*pos) == from || Some(*pos) == to {
                return true;
            }
            if distance_sq(center(*pos), position) >= DOOR_CLOSE_DISTANCE.powi(2) {
                return false;
            }
            let Some(state) = (cx.block)(*pos) else {
                return false;
            };
            if !tuff_datapack::tags::is(tag::block::key::MOB_INTERACTABLE_DOORS, state.block())
                || state.open() != Some(true)
            {
                return false;
            }
            let others_near = others.iter().any(|p| distance_sq(center(*pos), *p) < 4.0);
            if !others_near {
                cx.effects.push(Effect::UseDoor {
                    pos: *pos,
                    open: false,
                });
            }
            false
        });
    }

    fn stroll(&mut self, core: &mut MobCore, cx: &mut TickContext, speed: f64) {
        if !core.navigation.is_done() || cx.rng.gen_range(0..60) != 0 {
            return;
        }
        wander::village_stroll(core, cx, speed, 10, 7);
    }

    /// Walks to a remembered place when farther than `close` blocks
    /// (Manhattan), or toward it when it is out of `far`.
    fn go_to(
        &mut self,
        core: &mut MobCore,
        cx: &mut TickContext,
        pos: BlockPos,
        close: i32,
        far: i32,
    ) -> bool {
        let here = block_pos(core.base.position);
        let distance = manhattan(pos, here);
        if distance <= close {
            return false;
        }
        if distance <= far {
            core.walk_to_target(pos, close, SPEED, cx);
            return true;
        }
        if !core.navigation.is_done() {
            return true;
        }
        if let Some(toward) = random_pos::pos_towards(core, 15, 7, center(pos), cx) {
            core.move_to_position(toward, SPEED, cx);
        }
        true
    }

    fn stroll_around(
        &mut self,
        core: &mut MobCore,
        cx: &mut TickContext,
        pos: BlockPos,
        within: f64,
    ) {
        if distance_sq(center(pos), core.base.position) >= within * within
            || cx.tick <= self.brain.next_stroll
        {
            return;
        }
        self.brain.next_stroll = cx.tick + STROLL_COOLDOWN;
        if let Some(target) = random_pos::land_pos(core, 8, 6, cx) {
            core.move_to_position(target, STROLL_SPEED, cx);
        }
    }

    fn stroll_to_site(&mut self, core: &mut MobCore, cx: &mut TickContext, site: &GlobalPos) {
        let near = distance_sq(center(site.pos), core.base.position)
            < STROLL_TO_SITE_RANGE * STROLL_TO_SITE_RANGE;
        if near && site.dimension == dimension_key(cx) && cx.tick > self.brain.next_stroll_to_site {
            self.brain.next_stroll_to_site = cx.tick + STROLL_TO_SITE_COOLDOWN;
            core.move_to_block(site.pos, 1, STROLL_SPEED, cx);
        }
    }

    fn stroll_to_field(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.brain.farm.fields.is_empty() {
            return;
        }
        if cx.tick > self.brain.next_stroll_to_field {
            let fields = &self.brain.farm.fields;
            let field = fields[cx.rng.gen_range(0..fields.len())];
            self.brain.next_stroll_to_field = cx.tick + STROLL_TO_FIELD_COOLDOWN;
            core.move_to_block(field, 1, SPEED, cx);
        }
    }

    fn work(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.gift_step(core, cx) {
            return;
        }
        let Some(site) = self.brain.memories.job_site.clone() else {
            return;
        };
        if self.farm_step(core, cx) {
            return;
        }
        if self.go_to(core, cx, site.pos, 9, 100) {
            return;
        }
        if cx.tick >= self.brain.last_work_check + WORK_CHECK_INTERVAL && cx.rng.r#gen::<bool>() {
            self.brain.last_work_check = cx.tick;
            if distance_sq(center(site.pos), core.base.position) < WORK_DISTANCE.powi(2) {
                self.brain.memories.last_worked_at_poi = Some(cx.tick as i64);
                core.look_at_point(center(site.pos));
                if let Some(sound) = self.data.profession.info().work_sound {
                    core.sound(sound, 1.0, core.voice_pitch(cx.rng), cx);
                }
                self.bake_bread(core, site.pos, cx);
                if self.is_farmer() {
                    self.compost_seeds(core.base.id, site.pos, cx);
                }
                if self.should_restock(core, cx) {
                    self.restock(core, cx);
                }
                return;
            }
        }
        if !core.navigation.is_done() {
            return;
        }
        let fields = !self.brain.farm.fields.is_empty();
        let near_site = distance_sq(center(site.pos), core.base.position) < 36.0;
        let farmer = self.is_farmer();
        let mut options: Vec<(u32, Chore)> =
            vec![(2, Chore::StrollAround), (5, Chore::StrollToSite)];
        if fields && near_site {
            options.push((5, Chore::StrollToField));
        }
        if fields {
            options.push((if farmer { 2 } else { 5 }, Chore::Harvest));
        }
        options.push((if farmer { 4 } else { 7 }, Chore::Bonemeal));
        let total: u32 = options.iter().map(|(w, _)| w).sum();
        let mut roll = cx.rng.gen_range(0..total);
        let chore = options
            .iter()
            .find(|(weight, _)| {
                if roll < *weight {
                    true
                } else {
                    roll -= weight;
                    false
                }
            })
            .map(|(_, chore)| *chore)
            .unwrap_or(Chore::StrollAround);
        match chore {
            Chore::StrollAround => self.stroll_around(core, cx, site.pos, 4.0),
            Chore::StrollToSite => self.stroll_to_site(core, cx, &site),
            Chore::StrollToField => self.stroll_to_field(core, cx),
            Chore::Harvest => {
                self.start_harvest(core, cx);
            }
            Chore::Bonemeal => {
                self.start_bonemeal(core, cx);
            }
        }
    }

    fn meet(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.gift_step(core, cx) {
            return;
        }
        let Some(bell) = self.brain.memories.meeting_point.clone() else {
            return;
        };
        if self.go_to(core, cx, bell.pos, 6, 100) {
            return;
        }
        self.socialize_at_bell(core, cx);
        if self.brain.social.interaction.is_none() {
            self.stroll_around(core, cx, bell.pos, 40.0);
        }
    }

    /// Hears a neighbour's gossip, weakened.
    pub fn hear_gossip(
        &mut self,
        core: &MobCore,
        gossips: &super::gossip::Gossips,
        cx: &mut TickContext,
    ) {
        let heard = self.gossips.transfer_from(gossips, cx.rng, GOSSIP_TOPICS);
        if heard > 0 {
            self.update_special_prices(core, cx);
        }
    }

    fn rest(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let Some(home) = self.brain.memories.home.clone() else {
            if self.brain.sleeping.is_none() {
                self.homeless_night(core, cx);
            }
            return;
        };
        if let Some(bed) = self.brain.sleeping {
            if bed != home.pos {
                self.wake_up(core, cx);
            }
            return;
        }
        if self.go_to(core, cx, home.pos, 1, 150) {
            return;
        }
        let woken = self
            .brain
            .memories
            .last_woken
            .map(|t| cx.tick as i64 - t)
            .is_some_and(|since| since > 0 && since < WOKEN_COOLDOWN);
        if woken
            || (cx.tick as i64) < self.brain.next_sleep
            || distance_sq(center(home.pos), core.base.position) >= SLEEP_DISTANCE.powi(2)
            || !can_sleep_in((cx.block)(home.pos))
        {
            return;
        }
        self.start_sleeping(core, home.pos, cx);
    }

    /// A bed that is gone wakes the sleeper.
    fn check_bed(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let Some(bed) = self.brain.sleeping else {
            return;
        };
        let missing = (cx.block)(bed).is_some_and(|s| {
            !tuff_datapack::tags::is(tag::block::key::VILLAGERS_CAN_SLEEP_ON_BED, s.block())
        });
        if missing {
            self.wake_up(core, cx);
        }
    }

    fn start_sleeping(&mut self, core: &mut MobCore, bed: BlockPos, cx: &mut TickContext) {
        core.navigation.stop();
        self.close_doors(core, None, None, cx);
        self.brain.sleeping = Some(bed);
        self.brain.memories.last_slept = Some(cx.tick as i64);
        core.base.position = [
            bed.x as f64 + 0.5,
            bed.y as f64 + 0.6875,
            bed.z as f64 + 0.5,
        ];
        core.base.velocity = [0.0; 3];
        cx.effects.push(Effect::OccupyBed {
            pos: bed,
            occupied: true,
        });
        core.changed(tuff_core::protocol::play::metadata::POSE);
        core.changed(tuff_core::protocol::play::metadata::SLEEPING_POS);
    }

    /// Gets out of bed, if in one.
    pub fn wake_up(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let Some(bed) = self.brain.sleeping.take() else {
            return;
        };
        self.brain.memories.last_woken = Some(cx.tick as i64);
        self.brain.next_sleep = cx.tick as i64 + 40;
        cx.effects.push(Effect::OccupyBed {
            pos: bed,
            occupied: false,
        });
        let block = cx.block;
        let level = |pos| block(pos).unwrap_or(tuff_vanilla_data::Block::Stone.default_state());
        let stand = (cx.block)(bed)
            .and_then(|state| state.facing())
            .and_then(|facing| {
                tuff_vanilla_common::dismount::stand_up_position(bed, facing, core.base.yaw, &level)
            });
        core.base.position =
            stand.unwrap_or([bed.x as f64 + 0.5, bed.y as f64 + 1.0, bed.z as f64 + 0.5]);
        core.changed(tuff_core::protocol::play::metadata::POSE);
        core.changed(tuff_core::protocol::play::metadata::SLEEPING_POS);
    }

    fn panic(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if cx.tick.is_multiple_of(PANIC_GOLEM_INTERVAL) {
            self.call_golem_if_needed(core, cx, PANIC_GOLEM_VILLAGERS);
        }
        let threat = self
            .brain
            .hostile
            .or(self.brain.hurt_by.and_then(|(id, _)| id))
            .and_then(|id| crate::target::view(cx, id))
            .map(|t| t.position);
        if let Some(threat) = threat
            && distance_sq(threat, core.base.position) < FLEE_DISTANCE * FLEE_DISTANCE
        {
            let heading_away = core.navigation.path.as_ref().is_some_and(|p| {
                let end = p.target.center();
                let current = [
                    end[0] - core.base.position[0],
                    end[2] - core.base.position[2],
                ];
                let avoid = [
                    threat[0] - core.base.position[0],
                    threat[2] - core.base.position[2],
                ];
                !p.is_done() && current[0] * avoid[0] + current[1] * avoid[1] < 0.0
            });
            if !heading_away {
                for _ in 0..10 {
                    if let Some(target) = random_pos::land_pos_away(core, 16, 7, threat, cx) {
                        core.move_to_position(target, PANIC_SPEED, cx);
                        break;
                    }
                }
            }
            return;
        }
        if core.navigation.is_done() {
            wander::village_stroll(core, cx, PANIC_SPEED, 2, 2);
        }
    }

    fn idle(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.gift_step(core, cx) {
            return;
        }
        if self.breed_step(core, cx) {
            return;
        }
        if self.bounce_step(core, cx) || self.interaction_step(core, cx) {
            return;
        }
        self.idle_run_one(core, cx);
    }

    /// Courting: finds a willing villager nearby, walks up to it, and after
    /// a while a baby comes if a bed is free.
    fn breed_step(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        if core.is_baby() || !self.can_breed(core) {
            self.brain.breeding = None;
            core.breedable = false;
            return false;
        }
        core.breedable = true;
        let partner = match self.brain.breeding {
            Some((partner, _)) => cx.entities.iter().find(|e| e.id == partner),
            None => {
                let found = cx
                    .entities
                    .iter()
                    .filter(|e| e.kind == EntityType::Villager && e.id > core.base.id)
                    .filter(|e| {
                        e.living
                            .as_ref()
                            .is_some_and(|l| l.alive && l.in_love && l.age == 0)
                    })
                    .find(|e| distance_sq(e.position, core.base.position) <= BREED_RANGE.powi(2));
                if let Some(found) = found {
                    let birth = cx.tick + BIRTH_TICKS + cx.rng.gen_range(0..BIRTH_JITTER);
                    self.brain.breeding = Some((found.id, birth));
                    core.entity_event(crate::event::IN_LOVE, cx);
                    cx.effects.push(Effect::Notify {
                        target: found.id,
                        notice: Notice::Courted { by: core.base.id },
                    });
                }
                found
            }
        };
        let Some(partner) =
            partner.filter(|p| p.living.as_ref().is_some_and(|l| l.alive && l.in_love))
        else {
            self.brain.breeding = None;
            return false;
        };
        let (id, position) = (partner.id, partner.position);
        let birth = self.brain.breeding.map_or(u64::MAX, |(_, b)| b);
        core.look_at_point([position[0], position[1] + 1.5, position[2]]);
        if distance_sq(position, core.base.position) > GOSSIP_DISTANCE_SQ {
            if core.navigation.is_done() {
                core.move_to_entity_reach(position, 2, SPEED, cx);
            }
            return true;
        }
        if cx.tick >= birth {
            self.brain.breeding = None;
            cx.effects.push(Effect::Breed { partner: id });
        } else if cx.rng.gen_range(0..35) == 0 {
            core.entity_event(event::HEARTS, cx);
        }
        true
    }

    /// Another villager courts this one: it walks up to it, and the
    /// courting one decides when the baby comes.
    pub fn courted(&mut self, by: i32) {
        if self.brain.breeding.is_none() {
            self.brain.breeding = Some((by, u64::MAX));
        }
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;
    use crate::Species;
    use crate::villager::Villager;

    fn site(core: &MobCore, dx: f64, cx: &TickContext) -> GlobalPos {
        GlobalPos {
            dimension: dimension_key(cx),
            pos: BlockPos::new((core.base.position[0] + dx) as i32, 64, 0),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_idle_villager_greets_a_nearby_cat_or_villager_from_its_weighted_pick() {
        let mut cat = crate::target::tests::zombie(7, [3.5, 64.0, 0.5]);
        cat.kind = EntityType::Cat;
        let mut other = crate::target::tests::zombie(8, [-3.5, 64.0, 0.5]);
        other.kind = EntityType::Villager;
        crate::tests::with_entities(&[cat, other], |core, cx| {
            let mut villager = Villager::create(&mut StdRng::seed_from_u64(1));
            Villager::configure(core);
            core.base.position = [0.5, 64.0, 0.5];
            core.base.on_ground = true;
            let (mut cats, mut villagers) = (0, 0);
            for attempt in 0..120 {
                cx.tick = 10_000 + attempt * 100;
                core.navigation.stop();
                villager.end_interaction();
                villager.brain.wander.idle_until = 0;
                villager.idle_run_one(core, cx);
                match villager.brain.social.interaction.map(|i| i.target) {
                    Some(7) => cats += 1,
                    Some(8) => villagers += 1,
                    _ => {}
                }
            }
            assert!(
                cats > 5 && villagers > cats,
                "cat {cats}, villager {villagers}"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_idle_villager_sometimes_walks_to_what_it_looks_at() {
        crate::tests::with_context(|core, cx| {
            let mut villager = Villager::create(&mut StdRng::seed_from_u64(1));
            Villager::configure(core);
            core.base.position = [0.5, 64.0, 0.5];
            core.base.on_ground = true;
            let target = [6.5, 64.0, 0.5];
            let mut walked = 0;
            for attempt in 0..80 {
                cx.tick = 10_000 + attempt * 100;
                core.navigation.stop();
                villager.brain.wander.idle_until = 0;
                core.look.set_look_at(target, 30.0, 30.0);
                villager.idle_run_one(core, cx);
                if core
                    .navigation
                    .path
                    .as_ref()
                    .is_some_and(|p| distance_sq(p.target.center(), target) < 9.0)
                {
                    walked += 1;
                }
            }
            assert!(
                walked > 5,
                "{walked} of 80 idle picks walked to the look target"
            );
            core.look = Default::default();
            core.navigation.stop();
            villager.brain.wander.idle_until = 0;
            for attempt in 0..40 {
                cx.tick = 50_000 + attempt * 100;
                villager.idle_run_one(core, cx);
            }
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn strolling_to_a_secondary_site_waits_100_ticks() {
        crate::tests::with_context(|core, cx| {
            let mut villager = Villager::create(&mut StdRng::seed_from_u64(1));
            villager.brain.farm.fields = vec![BlockPos::new(3, 64, 3)];
            cx.tick = 1000;
            villager.stroll_to_field(core, cx);
            assert_eq!(villager.brain.next_stroll_to_field, 1100);
            cx.tick = 1050;
            villager.stroll_to_field(core, cx);
            assert_eq!(villager.brain.next_stroll_to_field, 1100);
            cx.tick = 1101;
            villager.stroll_to_field(core, cx);
            assert_eq!(villager.brain.next_stroll_to_field, 1201);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn strolling_to_the_job_site_needs_it_within_ten_blocks_and_waits_80_ticks() {
        crate::tests::with_context(|core, cx| {
            let mut villager = Villager::create(&mut StdRng::seed_from_u64(1));
            core.base.position = [0.5, 64.0, 0.5];
            let far = site(core, 20.0, cx);
            villager.stroll_to_site(core, cx, &far);
            assert!(core.navigation.is_done(), "too far to walk");
            assert_eq!(villager.brain.next_stroll_to_site, 0);

            let near = site(core, 6.0, cx);
            cx.tick = 1000;
            villager.stroll_to_site(core, cx, &near);
            assert_eq!(
                villager.brain.next_stroll_to_site,
                1000 + STROLL_TO_SITE_COOLDOWN
            );
            cx.tick = 1040;
            villager.stroll_to_site(core, cx, &near);
            assert_eq!(
                villager.brain.next_stroll_to_site,
                1000 + STROLL_TO_SITE_COOLDOWN
            );
            cx.tick = 1081;
            villager.stroll_to_site(core, cx, &near);
            assert_eq!(
                villager.brain.next_stroll_to_site,
                1081 + STROLL_TO_SITE_COOLDOWN
            );

            let mut other = near.clone();
            other.dimension = "minecraft:the_nether".to_owned();
            cx.tick = 5000;
            villager.stroll_to_site(core, cx, &other);
            assert_eq!(
                villager.brain.next_stroll_to_site,
                1081 + STROLL_TO_SITE_COOLDOWN
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_retry_delay_grows_by_40_to_79_up_to_400_and_lapses_when_idle() {
        let mut rng = StdRng::seed_from_u64(7);
        let mut retry = PoiRetry::new(1000, &mut rng);
        assert!((40..80).contains(&retry.delay));
        assert_eq!(retry.next_attempt, 1000 + retry.delay);
        let mut previous = retry.delay;
        for _ in 0..20 {
            retry.mark_attempt(2000, &mut rng);
            assert!(retry.delay >= (previous + 40).min(400));
            assert!(retry.delay <= (previous + 79).min(400));
            previous = retry.delay;
        }
        assert_eq!(retry.delay, 400);
        assert!(retry.still_valid(2399));
        assert!(!retry.still_valid(2400));
    }
}
