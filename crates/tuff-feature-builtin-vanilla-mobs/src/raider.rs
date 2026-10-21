//! What raid mobs (illagers, witches, ravagers) share: their raid and
//! patrol membership, the goals that walk them to a raid, through a
//! village and across the land in patrols, celebrating a won raid, and the
//! ominous banner a leader wears.

use rand::Rng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::{DyeColor, EquipmentSlot};
use tuff_vanilla_data::registry::{EntityType, PointOfInterestType, SoundEvent};

use super::goal::{Flags, Goal, reduced_delay};
use super::physics::block_pos;
use super::raid::{Raid, Raids};
use super::{MobCore, random_pos};
use tuff_core::world::entity::{Effect, RegionEntity, TickContext, take_bool, take_number};
use tuff_core::world::poi::{Occupancy, PoiStore};
use tuff_item::item::decor::BannerLayer;
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

/// Ticks of idling past which a raider no longer joins or stays in a raid.
pub const MAX_NO_ACTION_TIME: i32 = 2400;

/// A raider's raid and patrol membership.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RaiderState {
    pub raid: Option<u32>,
    pub wave: i32,
    pub can_join_raid: bool,
    pub patrol_leader: bool,
    pub patrolling: bool,
    pub patrol_target: Option<BlockPos>,
    pub celebrating: bool,
    pub can_be_leader: bool,
    pub celebrate_sound: Option<SoundEvent>,
    /// Set while it is spawned by a raid or a patrol, so it does not
    /// roll to lead on its own.
    pub spawned_for_group: bool,
    /// A witch's wait before it may pick another raider to heal.
    pub heal_cooldown: i32,
    /// Loaded with a raid id: how many more once-a-second checks it waits
    /// for that raid to be loaded before rejoining it, or forgetting it.
    pub rejoin: u8,
    /// Picked up a banner and still has to tell the raid it leads.
    pub announce_leader: bool,
}

/// How many seconds a raider loaded with a raid id waits for the raid.
const REJOIN_CHECKS: u8 = 15;

/// What raiders tell the raid they belong to, collected in the region
/// state and passed on to the raid's home region.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Report {
    Joined {
        raid: u32,
        uuid: u128,
        wave: i32,
        health: f32,
        leader: bool,
        /// Loaded back into a raid that already counted its health.
        rejoin: bool,
    },
    Leader {
        raid: u32,
        uuid: u128,
        wave: i32,
    },
    Died {
        raid: u32,
        uuid: u128,
        wave: i32,
        killer: Option<u128>,
    },
}

#[derive(Debug, Default)]
pub struct Reports(pub Vec<Report>);

/// Patrol leaders' current targets, by UUID, for their followers.
#[derive(Debug, Default)]
pub struct PatrolTargets(pub std::collections::HashMap<u128, (BlockPos, u64)>);

pub fn is_raider(kind: EntityType) -> bool {
    super::illager::RAIDERS.contains(&kind)
}

/// Marks a freshly configured mob as a raider.
pub fn make_raider(core: &mut MobCore, can_be_leader: bool, celebrate_sound: SoundEvent) {
    core.raider = Some(RaiderState {
        can_be_leader,
        celebrate_sound: Some(celebrate_sound),
        ..Default::default()
    });
}

/// The standard raider goals every raid mob adds after its own.
pub fn add_goals(goals: &mut super::goal::GoalSelector) {
    goals.add(1, ObtainRaidLeaderBanner);
    goals.add(3, PathfindToRaid);
    goals.add(4, MoveThroughVillage::default());
    goals.add(4, LongDistancePatrol::default());
    goals.add(5, Celebrate);
}

pub fn load(state: &mut RaiderState, nbt: &mut Compound) {
    state.wave = take_number(nbt, "Wave").map_or(0, |v| v as i32);
    state.can_join_raid = take_bool(nbt, "CanJoinRaid").unwrap_or(false);
    state.raid = take_number(nbt, "RaidId").map(|v| v as i64 as u32);
    state.rejoin = if state.raid.is_some() {
        REJOIN_CHECKS
    } else {
        0
    };
    state.patrol_leader = take_bool(nbt, "PatrolLeader").unwrap_or(false);
    state.patrolling = take_bool(nbt, "Patrolling").unwrap_or(false);
    state.patrol_target = match nbt.remove("patrol_target") {
        Some(Tag::IntArray(v)) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
        Some(other) => {
            nbt.insert("patrol_target", other);
            None
        }
        None => None,
    };
}

pub fn save(state: &RaiderState, out: &mut Compound) {
    out.insert("Wave", state.wave);
    out.insert("CanJoinRaid", state.can_join_raid);
    if let Some(raid) = state.raid {
        out.insert("RaidId", raid as i32);
    }
    out.insert("PatrolLeader", state.patrol_leader);
    out.insert("Patrolling", state.patrolling);
    if let Some(at) = state.patrol_target {
        out.insert("patrol_target", Tag::IntArray(vec![at.x, at.y, at.z]));
    }
}

/// The banner raid and patrol leaders wear.
pub fn ominous_banner() -> ItemStack {
    let layer = |pattern: &str, color: &str| BannerLayer {
        pattern: format!("minecraft:{pattern}"),
        inline: None,
        color: DyeColor::from_name(color).expect("a dye color"),
    };
    let mut stack = ItemStack::mint(Item::WhiteBanner, 1, MintReason::Spawn);
    stack.components.banner_patterns = Some(vec![
        layer("rhombus", "cyan"),
        layer("stripe_bottom", "light_gray"),
        layer("stripe_center", "gray"),
        layer("border", "light_gray"),
        layer("stripe_middle", "black"),
        layer("half_horizontal", "light_gray"),
        layer("circle", "light_gray"),
        layer("border", "black"),
    ]);
    let other = &mut stack.components.other;
    other.insert(
        "minecraft:item_name",
        Compound::new().with("translate", "block.minecraft.ominous_banner"),
    );
    other.insert("minecraft:rarity", "uncommon");
    other.insert(
        "minecraft:tooltip_display",
        Compound::new().with(
            "hidden_components",
            Tag::List(vec![Tag::String("minecraft:banner_patterns".into())]),
        ),
    );
    stack
}

pub fn is_ominous_banner(stack: &ItemStack) -> bool {
    let banner = ominous_banner();
    stack.item == banner.item && stack.components == banner.components
}

/// A leader wears the banner, which always drops.
pub fn wear_banner(core: &mut MobCore) {
    core.equipment
        .set(EquipmentSlot::Head, Some(ominous_banner()));
    core.equipment.set_drop_chance(EquipmentSlot::Head, 2.0);
}

pub fn is_captain(core: &MobCore) -> bool {
    core.raider.as_ref().is_some_and(|r| r.patrol_leader)
        && core
            .equipment
            .get(EquipmentSlot::Head)
            .is_some_and(is_ominous_banner)
}

/// The raid it belongs to, as this region last heard of it.
pub fn current_raid(core: &MobCore, cx: &mut TickContext) -> Option<Raid> {
    let id = core.raider.as_ref()?.raid?;
    cx.state.get::<Raids>().get(id)
}

pub fn has_active_raid(core: &MobCore, cx: &mut TickContext) -> bool {
    current_raid(core, cx).is_some_and(|r| r.is_active())
}

/// Within a section of a village section.
pub fn is_village(pois: &PoiStore, pos: BlockPos) -> bool {
    pois.sections_to_village(pos) <= 1
}

/// The bottom middle of the section within `radius` sections of `pos`
/// that is closest to a village, when one is closer than `pos`'s own.
pub fn toward_village(pois: &PoiStore, pos: BlockPos, radius: i32) -> Option<[f64; 3]> {
    let here = pois.sections_to_village(pos);
    let middle = [pos.x >> 4, pos.y >> 4, pos.z >> 4];
    let mut closest: Option<(i32, [i32; 3])> = None;
    for z in middle[2] - radius..=middle[2] + radius {
        for y in middle[1] - radius..=middle[1] + radius {
            for x in middle[0] - radius..=middle[0] + radius {
                let at = BlockPos::new(x * 16 + 8, y * 16 + 8, z * 16 + 8);
                let distance = pois.sections_to_village(at);
                if distance < here && closest.is_none_or(|(best, _)| distance < best) {
                    closest = Some((distance, [x, y, z]));
                }
            }
        }
    }
    let (_, [x, y, z]) = closest?;
    Some([
        f64::from(x * 16 + 8) + 0.5,
        f64::from(y * 16 + 8),
        f64::from(z * 16 + 8) + 0.5,
    ])
}

/// The raider's own part of its AI step: it joins a raid it wanders
/// into, forgets one that is gone, and a raider in a raid fighting a
/// player or golem is never idle.
pub fn ai_step(core: &mut MobCore, cx: &mut TickContext) {
    let uuid = core.base.uuid;
    let Some(state) = core.raider.as_mut() else {
        return;
    };
    if std::mem::take(&mut state.announce_leader)
        && let Some(raid) = state.raid
    {
        let wave = state.wave;
        cx.state
            .get::<Reports>()
            .0
            .push(Report::Leader { raid, uuid, wave });
    }
    let Some(state) = core.raider.as_ref() else {
        return;
    };
    if !core.is_alive() {
        return;
    }
    if state.raid.is_none() && !state.patrol_leader && cx.tick.is_multiple_of(20) {
        follow_leader(core, cx);
    }
    let Some(state) = core.raider.as_ref() else {
        return;
    };
    match state.raid {
        Some(id) => {
            if cx.tick.is_multiple_of(20) {
                let known = cx.state.get::<Raids>().get(id).is_some();
                let waiting = state.rejoin > 0;
                match (known, waiting) {
                    (true, true) => rejoin(core, id, cx),
                    (false, true) => {
                        if let Some(state) = core.raider.as_mut() {
                            state.rejoin -= 1;
                        }
                        return;
                    }
                    (false, false) => {
                        if let Some(state) = core.raider.as_mut() {
                            state.raid = None;
                        }
                        return;
                    }
                    (true, false) => {}
                }
            }
            let fighting = super::target::view(cx, core.target.unwrap_or(i32::MIN))
                .is_some_and(|t| t.is_player() || t.kind == EntityType::IronGolem);
            if fighting {
                core.no_action_time = 0;
            }
        }
        None if state.can_join_raid && cx.tick.is_multiple_of(20) => {
            let Some(raid) = cx.state.get::<Raids>().at(block_pos(core.base.position)) else {
                return;
            };
            if core.no_action_time > MAX_NO_ACTION_TIME {
                return;
            }
            join(core, raid, raid.groups_spawned, cx);
        }
        None => {}
    }
}

/// A raider loaded with its raid id tells the raid, now loaded too, that
/// it is back.
fn rejoin(core: &mut MobCore, raid: u32, cx: &mut TickContext) {
    let uuid = core.base.uuid;
    let health = core.health;
    let Some(state) = core.raider.as_mut() else {
        return;
    };
    state.rejoin = 0;
    cx.state.get::<Reports>().0.push(Report::Joined {
        raid,
        uuid,
        wave: state.wave,
        health,
        leader: state.patrol_leader,
        rejoin: true,
    });
}

fn join(core: &mut MobCore, raid: Raid, wave: i32, cx: &mut TickContext) {
    let uuid = core.base.uuid;
    let health = core.health;
    let Some(state) = core.raider.as_mut() else {
        return;
    };
    state.raid = Some(raid.id);
    state.wave = wave;
    state.can_join_raid = true;
    let leader = state.patrol_leader;
    cx.state.get::<Reports>().0.push(Report::Joined {
        raid: raid.id,
        uuid,
        wave,
        health,
        leader,
        rejoin: false,
    });
}

/// A raider in a raid died: the raid hears of it, and of the player who
/// killed it.
pub fn died(core: &MobCore, killer: Option<u128>, cx: &mut TickContext) {
    let Some(state) = core.raider.as_ref() else {
        return;
    };
    let Some(raid) = state.raid else {
        return;
    };
    cx.state.get::<Reports>().0.push(Report::Died {
        raid,
        uuid: core.base.uuid,
        wave: state.wave,
        killer,
    });
}

/// `has_raid` and `is_captain`, as loot conditions see a raider.
pub fn loot_facts(core: &MobCore, cx: &mut TickContext) -> Option<(bool, bool)> {
    let state = core.raider.as_ref()?;
    let has_raid = state.raid.is_some()
        || cx
            .state
            .get::<Raids>()
            .at(block_pos(core.base.position))
            .is_some();
    Some((has_raid, is_captain(core)))
}

/// The odds a raider's weapon is enchanted for a raid of this omen level.
pub fn enchant_odds(omen_level: i32) -> f32 {
    match omen_level {
        2 => 0.1,
        3 => 0.25,
        4 => 0.5,
        5 => 0.75,
        _ => 0.0,
    }
}

/// Arms a raider joining a raid's wave: pillagers' crossbows and
/// vindicators' axes may be enchanted the later the wave.
pub fn apply_raid_buffs(core: &mut MobCore, wave: i32, omen_level: i32, rng: &mut impl Rng) {
    let enchant = rng.r#gen::<f32>() <= enchant_odds(omen_level);
    match core.kind() {
        EntityType::Pillager => {
            let provider = if wave > 5 {
                Some("minecraft:raid/pillager_post_wave_5")
            } else if wave > 3 {
                Some("minecraft:raid/pillager_post_wave_3")
            } else {
                None
            };
            if enchant && let Some(provider) = provider {
                let crossbow = tuff_item::item::enchanting::enchant_from_provider(
                    ItemStack::mint(Item::Crossbow, 1, MintReason::Spawn),
                    provider,
                    0.0,
                    rng,
                );
                core.equipment.set(EquipmentSlot::Mainhand, Some(crossbow));
            }
        }
        EntityType::Vindicator => {
            let mut axe = ItemStack::mint(Item::IronAxe, 1, MintReason::Spawn);
            if enchant {
                let provider = if wave > 5 {
                    "minecraft:raid/vindicator_post_wave_5"
                } else {
                    "minecraft:raid/vindicator"
                };
                axe = tuff_item::item::enchanting::enchant_from_provider(axe, provider, 0.0, rng);
            }
            core.equipment.set(EquipmentSlot::Mainhand, Some(axe));
        }
        _ => {}
    }
}

/// A natural spawn may make a patrol leader of it (6%), and witches that
/// spawn naturally never join raids.
pub fn finalize_spawn(core: &mut MobCore, cx: &mut TickContext) {
    let kind = core.kind();
    let natural = super::spawning::spawning_naturally(cx);
    let Some(state) = core.raider.as_mut() else {
        return;
    };
    state.can_join_raid = !(kind == EntityType::Witch && natural);
    if !state.spawned_for_group && cx.rng.r#gen::<f32>() < 0.06 && state.can_be_leader {
        state.patrol_leader = true;
    }
    if state.patrol_leader {
        wear_banner(core);
    }
}

/// Mob cores of raid mobs among region entities, for the raid that runs
/// them.
pub fn core_mut(entity: &mut dyn RegionEntity) -> Option<&mut MobCore> {
    use super::Mob;
    macro_rules! try_species {
        ($($s:ty),*) => {
            $(
                if entity.downcast_ref::<Mob<$s>>().is_some() {
                    return entity.downcast_mut::<Mob<$s>>().map(|m| &mut m.core);
                }
            )*
        };
    }
    try_species!(
        super::pillager::Pillager,
        super::vindicator::Vindicator,
        super::evoker::Evoker,
        super::witch::Witch,
        super::ravager::Ravager,
        super::illusioner::Illusioner
    );
    None
}

pub fn core_of(entity: &dyn RegionEntity) -> Option<&MobCore> {
    use super::Mob;
    macro_rules! try_species {
        ($($s:ty),*) => {
            $(
                if let Some(mob) = entity.downcast_ref::<Mob<$s>>() {
                    return Some(&mob.core);
                }
            )*
        };
    }
    try_species!(
        super::pillager::Pillager,
        super::vindicator::Vindicator,
        super::evoker::Evoker,
        super::witch::Witch,
        super::ravager::Ravager,
        super::illusioner::Illusioner
    );
    None
}

fn raid_ongoing(mob: &MobCore, cx: &mut TickContext) -> Option<Raid> {
    current_raid(mob, cx).filter(|r| r.is_active() && !r.is_over())
}

/// Walks a raider outside the village towards its raid's centre.
#[derive(Debug)]
struct PathfindToRaid;

impl Goal for PathfindToRaid {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target.is_none()
            && mob.base.riders.is_empty()
            && raid_ongoing(mob, cx).is_some()
            && !is_village(cx.pois, block_pos(mob.base.position))
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        raid_ongoing(mob, cx).is_some() && !is_village(cx.pois, block_pos(mob.base.position))
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(raid) = raid_ongoing(mob, cx) else {
            return;
        };
        if !mob.navigation.is_done() {
            return;
        }
        let c = raid.center;
        let center = [f64::from(c.x) + 0.5, f64::from(c.y), f64::from(c.z) + 0.5];
        if let Some(to) =
            random_pos::pos_towards_within(mob, 15, 4, center, std::f64::consts::FRAC_PI_2, cx)
        {
            mob.move_to_position(to, 1.0, cx);
        }
    }
}

/// Runs a raider from house to house (beds) in the village it raids.
#[derive(Debug, Default)]
struct MoveThroughVillage {
    poi: Option<BlockPos>,
    visited: Vec<BlockPos>,
    stuck: bool,
}

const VILLAGE_SPEED: f64 = 1.049_999_952_316_284;
const POI_DISTANCE: f64 = 1.0;

impl MoveThroughVillage {
    fn close_to(&self, mob: &MobCore, distance: f64) -> bool {
        self.poi.is_some_and(|p| {
            let center = [
                f64::from(p.x) + 0.5,
                f64::from(p.y) + 0.5,
                f64::from(p.z) + 0.5,
            ];
            mob.distance_sq(center) < distance * distance
        })
    }
}

impl Goal for MoveThroughVillage {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.visited.len() > 2 {
            self.visited.remove(0);
        }
        if raid_ongoing(mob, cx).is_none() || mob.target.is_some() {
            return false;
        }
        let homes: Vec<BlockPos> = cx
            .pois
            .closest_first(block_pos(mob.base.position), 48, Occupancy::Any, |k| {
                k == PointOfInterestType::Home
            })
            .into_iter()
            .map(|(pos, _)| pos)
            .filter(|pos| !self.visited.contains(pos))
            .collect();
        if homes.is_empty() {
            return false;
        }
        self.poi = Some(homes[cx.rng.gen_range(0..homes.len())]);
        true
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !(mob.navigation.is_done()
            || mob.target.is_some()
            || self.close_to(mob, f64::from(mob.size.width) + POI_DISTANCE)
            || self.stuck)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.no_action_time = 0;
        self.stuck = false;
        if let Some(p) = self.poi {
            mob.move_to_position(
                [f64::from(p.x), f64::from(p.y), f64::from(p.z)],
                VILLAGE_SPEED,
                cx,
            );
        }
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        if self.close_to(mob, POI_DISTANCE)
            && let Some(p) = self.poi
        {
            self.visited.push(p);
        }
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(p) = self.poi else {
            return;
        };
        if !mob.navigation.is_done() {
            return;
        }
        let target = [f64::from(p.x) + 0.5, f64::from(p.y), f64::from(p.z) + 0.5];
        let next =
            random_pos::pos_towards_within(mob, 16, 7, target, std::f64::consts::PI / 10.0, cx)
                .or_else(|| {
                    random_pos::pos_towards_within(
                        mob,
                        8,
                        7,
                        target,
                        std::f64::consts::FRAC_PI_2,
                        cx,
                    )
                });
        match next {
            Some(to) => {
                mob.move_to_position(to, VILLAGE_SPEED, cx);
            }
            None => self.stuck = true,
        }
    }
}

/// A raider of a wave that has no leader walks to a dropped ominous
/// banner and puts it on.
#[derive(Debug)]
struct ObtainRaidLeaderBanner;

impl ObtainRaidLeaderBanner {
    fn banner(mob: &MobCore, cx: &mut TickContext) -> Option<(i32, [f64; 3], [f64; 6])> {
        let state = mob.raider.as_ref()?;
        let raid = current_raid(mob, cx)?;
        if !raid.is_active()
            || !state.can_be_leader
            || raid.has_leader(state.wave)
            || mob
                .equipment
                .get(EquipmentSlot::Head)
                .is_some_and(is_ominous_banner)
        {
            return None;
        }
        let area = super::physics::inflate(mob.bounds(), [16.0, 8.0, 16.0]);
        cx.entities
            .iter()
            .find(|e| {
                e.ominous_banner
                    && e.item.is_some_and(|(_, _, free)| free)
                    && super::physics::intersects(area, e.bounds)
            })
            .map(|e| (e.id, e.position, e.bounds))
    }
}

impl Goal for ObtainRaidLeaderBanner {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.is_alive() && Self::banner(mob, cx).is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.can_use(mob, cx)
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some((id, position, bounds)) = Self::banner(mob, cx) else {
            return;
        };
        if super::physics::intersects(
            super::physics::inflate(mob.bounds(), [1.0, 0.0, 1.0]),
            bounds,
        ) {
            cx.effects.push(Effect::TakeItem { item: id, count: 1 });
            mob.navigation.stop();
        } else {
            mob.move_to_entity(position, 1.15, cx);
        }
    }
}

/// A banner it picked up: an ominous one makes it the wave's leader.
pub fn took_banner(core: &mut MobCore, stack: &ItemStack) -> bool {
    if !is_ominous_banner(stack) {
        return false;
    }
    let Some(state) = core.raider.as_mut() else {
        return false;
    };
    state.patrol_leader = true;
    state.announce_leader = true;
    wear_banner(core);
    core.persistence_required = true;
    true
}

/// Raiders of a raid the village lost cheer and hop about.
#[derive(Debug)]
struct Celebrate;

fn set_celebrating(mob: &mut MobCore, celebrating: bool) {
    if let Some(state) = mob.raider.as_mut()
        && state.celebrating != celebrating
    {
        state.celebrating = celebrating;
        mob.changed(super::illager::CELEBRATING);
    }
}

impl Goal for Celebrate {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.is_alive()
            && mob.target.is_none()
            && current_raid(mob, cx).is_some_and(|r| r.phase == super::raid::Phase::Loss)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        set_celebrating(mob, true);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        set_celebrating(mob, false);
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let sound = mob.raider.as_ref().and_then(|r| r.celebrate_sound);
        if !mob.base.silent
            && let Some(sound) = sound
            && cx.rng.gen_range(0..reduced_delay(100)) == 0
        {
            let pitch = mob.voice_pitch(cx.rng);
            mob.sound(sound, 1.0, pitch, cx);
        }
        if cx.rng.gen_range(0..reduced_delay(50)) == 0 {
            mob.jump.jump = true;
        }
    }
}

/// A patrol member takes up the target of the leader it walks with.
fn follow_leader(mob: &mut MobCore, cx: &mut TickContext) {
    let [x, y, z] = mob.base.position;
    let leaders = cx.state.get::<PatrolTargets>();
    let target = cx
        .entities
        .iter()
        .filter(|e| {
            (e.position[0] - x).abs() <= 17.0
                && (e.position[1] - y).abs() <= 18.0
                && (e.position[2] - z).abs() <= 17.0
        })
        .filter_map(|e| leaders.0.get(&e.uuid))
        .max_by_key(|(_, at)| *at)
        .map(|(target, _)| *target);
    drop(leaders);
    if let (Some(target), Some(state)) = (target, mob.raider.as_mut()) {
        state.patrol_target = Some(target);
        state.patrolling = true;
    }
}

/// A patrol walking across the land: the leader heads for its far target
/// and its companions follow wherever it leads.
#[derive(Debug, Default)]
struct LongDistancePatrol {
    cooldown_until: u64,
}

const PATROL_SPEED: f64 = 0.7;
const LEADER_SPEED: f64 = 0.595;
const NAVIGATION_FAILED_COOLDOWN: u64 = 200;

impl LongDistancePatrol {
    fn companions(mob: &MobCore, cx: &TickContext) -> usize {
        let [x, y, z] = mob.base.position;
        cx.entities
            .iter()
            .filter(|e| {
                e.id != mob.base.id
                    && is_raider(e.kind)
                    && e.living.is_some_and(|l| l.alive)
                    && (e.position[0] - x).abs() <= 16.0 + 1.0
                    && (e.position[1] - y).abs() <= 16.0 + 2.0
                    && (e.position[2] - z).abs() <= 16.0 + 1.0
            })
            .count()
    }

    fn height(mob: &MobCore, x: i32, z: i32, cx: &TickContext) -> i32 {
        super::spawning::heightmap_top(
            tuff_vanilla_data::entity::Heightmap::MotionBlockingNoLeaves,
            x,
            z,
            mob.max_y,
            mob.min_y,
            cx.block,
        )
    }
}

impl Goal for LongDistancePatrol {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(state) = mob.raider.as_ref() else {
            return false;
        };
        state.patrolling
            && mob.target.is_none()
            && mob.base.riders.is_empty()
            && state.patrol_target.is_some()
            && cx.tick >= self.cooldown_until
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if !mob.navigation.is_done() {
            return;
        }
        let Some(state) = mob.raider.clone() else {
            return;
        };
        let leader = state.patrol_leader;
        if Self::companions(mob, cx) == 0 {
            if let Some(state) = mob.raider.as_mut() {
                state.patrolling = false;
            }
            return;
        }
        let Some(target) = state.patrol_target else {
            return;
        };
        let target = [
            f64::from(target.x) + 0.5,
            f64::from(target.y),
            f64::from(target.z) + 0.5,
        ];
        let here = mob.base.position;
        let arrived = (0..3).map(|i| (target[i] - here[i]).powi(2)).sum::<f64>() < 100.0;
        if leader && arrived {
            let pos = block_pos(here);
            let target = BlockPos::new(
                pos.x - 500 + cx.rng.gen_range(0..1000),
                pos.y,
                pos.z - 500 + cx.rng.gen_range(0..1000),
            );
            if let Some(state) = mob.raider.as_mut() {
                state.patrol_target = Some(target);
                state.patrolling = true;
            }
            return;
        }
        let away = [
            here[0] - target[0],
            here[1] - target[1],
            here[2] - target[2],
        ];
        let (sin, cos) = (
            90.0f32.to_radians().sin() as f64,
            90.0f32.to_radians().cos() as f64,
        );
        let rotated = [
            away[0] * cos + away[2] * sin,
            away[1],
            away[2] * cos - away[0] * sin,
        ];
        let mut step = [
            rotated[0] * 0.4 + target[0] - here[0],
            rotated[1] * 0.4 + target[1] - here[1],
            rotated[2] * 0.4 + target[2] - here[2],
        ];
        let length = step.iter().map(|v| v * v).sum::<f64>().sqrt();
        if length > 1.0e-5 {
            step = step.map(|v| v / length * 10.0);
        }
        let to = block_pos([here[0] + step[0], here[1] + step[1], here[2] + step[2]]);
        let y = Self::height(mob, to.x, to.z, cx);
        let speed = if leader { LEADER_SPEED } else { PATROL_SPEED };
        let moved =
            mob.move_to_position([f64::from(to.x), f64::from(y), f64::from(to.z)], speed, cx);
        if !moved {
            let pos = block_pos(here);
            let x = pos.x - 8 + cx.rng.gen_range(0..16);
            let z = pos.z - 8 + cx.rng.gen_range(0..16);
            let y = Self::height(mob, x, z, cx);
            mob.move_to_position([f64::from(x), f64::from(y), f64::from(z)], PATROL_SPEED, cx);
            self.cooldown_until = cx.tick + NAVIGATION_FAILED_COOLDOWN;
        } else if leader {
            let at = BlockPos::new(to.x, y, to.z);
            cx.state
                .get::<PatrolTargets>()
                .0
                .insert(mob.base.uuid, (at, cx.tick));
        }
    }
}

/// A patrolling illager that spots a target first stands its ground and
/// calls the patrol to it, turning on it once it comes close.
#[derive(Debug)]
pub struct HoldGroundAttack {
    hostile_radius_sq: f64,
}

impl HoldGroundAttack {
    pub fn new(hostile_radius: f32) -> Self {
        Self {
            hostile_radius_sq: f64::from(hostile_radius * hostile_radius),
        }
    }

    fn shout(mob: &MobCore, cx: &mut TickContext, target: i32) {
        let [x, y, z] = mob.base.position;
        let nearby: Vec<i32> = cx
            .entities
            .iter()
            .filter(|e| {
                e.id != mob.base.id
                    && is_raider(e.kind)
                    && e.living.is_some_and(|l| l.alive)
                    && (e.position[0] - x).abs() <= 8.0
                    && (e.position[1] - y).abs() <= 8.0
                    && (e.position[2] - z).abs() <= 8.0
            })
            .map(|e| e.id)
            .collect();
        for id in nearby {
            cx.effects.push(tuff_core::world::entity::Effect::Notify {
                target: id,
                notice: tuff_core::world::entity::Notice::Alert { attacker: target },
            });
        }
    }
}

impl Goal for HoldGroundAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(state) = mob.raider.as_ref() else {
            return false;
        };
        let hurt_by_player = mob
            .last_hurt_by_mob
            .and_then(|(id, _)| super::target::view(cx, id))
            .is_some_and(|t| t.is_player());
        state.raid.is_none()
            && state.patrolling
            && mob.target.is_some()
            && !mob.aggressive
            && !hurt_by_player
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.navigation.stop();
        if let Some(target) = mob.target {
            Self::shout(mob, cx, target);
        }
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(target) = mob.target {
            Self::shout(mob, cx, target);
            mob.set_aggressive(true);
        }
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        if mob.distance_sq(target.position) > self.hostile_radius_sq {
            mob.look.set_look_at(target.eye, 30.0, 30.0);
        } else {
            mob.set_aggressive(true);
        }
    }
}

/// A goal a raider only runs while its raid is on: with `roll`, it also
/// only tries one step in five (a vindicator breaking down doors).
#[derive(Debug)]
pub struct RaidOnly<G: Goal> {
    goal: G,
    roll: bool,
}

impl<G: Goal> RaidOnly<G> {
    pub fn new(goal: G) -> Self {
        Self { goal, roll: false }
    }

    pub fn rolled(goal: G) -> Self {
        Self { goal, roll: true }
    }
}

impl<G: Goal> Goal for RaidOnly<G> {
    fn flags(&self) -> Flags {
        if self.roll {
            Flags::MOVE
        } else {
            self.goal.flags()
        }
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        has_active_raid(mob, cx)
            && (!self.roll || cx.rng.gen_range(0..reduced_delay(10)) == 0)
            && self.goal.can_use(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        (!self.roll || has_active_raid(mob, cx)) && self.goal.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if self.roll {
            mob.no_action_time = 0;
        }
        self.goal.start(mob, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.goal.stop(mob, cx);
    }

    fn every_tick(&self) -> bool {
        self.goal.every_tick()
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.goal.tick(mob, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn raider_state_round_trips_through_its_saved_keys() {
        let state = RaiderState {
            raid: Some(3_000_000_000),
            wave: 4,
            can_join_raid: true,
            patrol_leader: true,
            patrolling: true,
            patrol_target: Some(BlockPos::new(-5, 70, 12)),
            ..Default::default()
        };
        let mut out = Compound::new();
        save(&state, &mut out);
        let mut loaded = RaiderState::default();
        load(&mut loaded, &mut out);
        assert_eq!(
            loaded.rejoin, REJOIN_CHECKS,
            "it waits for its raid to load"
        );
        assert_eq!(
            loaded,
            RaiderState {
                rejoin: REJOIN_CHECKS,
                ..state
            }
        );
        assert!(out.is_empty(), "every key is taken: {out:?}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_naturally_spawned_witches_stay_out_of_raids() {
        for (reason, joins) in [
            (Some(crate::spawning::SpawnReason::Natural), false),
            (Some(crate::spawning::SpawnReason::Spawner), true),
            (None, true),
        ] {
            crate::tests::with_context(|core, cx| {
                core.base.kind = EntityType::Witch;
                core.raider = Some(RaiderState::default());
                cx.state.get::<crate::spawning::SettingUp>().0 = reason;
                finalize_spawn(core, cx);
                assert_eq!(core.raider.as_ref().unwrap().can_join_raid, joins);
            });
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_ominous_banner_is_a_white_banner_with_eight_layers() {
        let banner = ominous_banner();
        assert_eq!(banner.item, Item::WhiteBanner);
        assert_eq!(banner.components.banner_patterns.as_ref().unwrap().len(), 8);
        assert!(
            banner
                .components
                .banner_patterns
                .as_ref()
                .unwrap()
                .iter()
                .all(BannerLayer::is_sendable)
        );
        assert!(is_ominous_banner(&banner));
        assert!(!is_ominous_banner(&ItemStack::mint(
            Item::WhiteBanner,
            1,
            MintReason::Test
        )));
    }

    #[tuff_core::watchdog]
    #[test]
    fn raid_weapons_enchant_more_often_with_a_stronger_omen() {
        assert_eq!(enchant_odds(1), 0.0);
        assert_eq!(enchant_odds(5), 0.75);
        assert!(enchant_odds(3) < enchant_odds(4));
    }
}

#[cfg(test)]
mod banner_round_trip {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_ominous_banner_survives_saving() {
        let banner = ominous_banner();
        let back = ItemStack::mint_from_nbt(&banner.to_nbt(), MintReason::Test).unwrap();
        assert_eq!(back.components, banner.components);
    }
}
