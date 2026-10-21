//! Villagers among themselves: walking up to each other, swapping
//! gossip, handing over spare food and wanted goods, and calling an iron
//! golem when enough of them agree the village needs one.

use std::collections::HashMap;

use rand::Rng;
use tuff_vanilla_data::registry::{EntityType, VillagerProfession};
use tuff_vanilla_data::{Block, BlockState, Item, tag};

use super::{SPEED, Villager, center, distance_sq};
use crate::physics::{self, block_pos};
use crate::{MobCore, spawning};
use tuff_core::world::entity::{Effect, EntitySummary, TickContext};
use tuff_item::item::ItemStack;
use tuff_world_model::BlockPos;

const INTERACT_RANGE_SQ: f64 = 64.0;
const INTERACT_DISTANCE_SQ: f64 = 5.0;
const INTERACT_TIMEOUT: u64 = 200;
const INTERACT_CHANCE: u32 = 20;
const BELL_SOCIAL_CHANCE: u32 = 100;
const BELL_SOCIAL_SPEED: f64 = 0.3;
const BELL_SOCIAL_NEAR: f64 = 4.0;
const BELL_SOCIAL_RANGE_SQ: f64 = 32.0;
pub const GOSSIP_COOLDOWN: u64 = 1200;
const GOLEM_VILLAGERS: usize = 5;
const GOLEM_BOX: f64 = 10.0;
const GOLEM_SLEEP_WINDOW: i64 = 24000;
const GOLEM_MEMORY: u64 = 599;
const GOLEM_SCAN_INTERVAL: u64 = 200;
const GOLEM_SCAN_RANGE_SQ: f64 = 256.0;
const GOLEM_ATTEMPTS: u32 = 10;
const GOLEM_RANGE_XZ: i32 = 8;
const GOLEM_RANGE_Y: i32 = 6;
const EXCESS_FOOD: i32 = 24;
const HUNGRY: i32 = 12;
const THROW_SPEED: f64 = 0.3;
const THROW_HAND_BELOW_EYE: f64 = 0.3;
const THROW_PICKUP_DELAY: u32 = 40;
const KEEP_WHEAT: u8 = 32;
const KEEP_FOOD_STACK: u8 = 24;
const PRUNE_INTERVAL: u64 = 200;
const PRUNE_AGE: u64 = 400;

/// What one villager lets its neighbours know without a round trip:
/// others read it where vanilla reads another villager directly.
#[derive(Debug, Clone, Copy)]
pub struct Fact {
    seen: u64,
    pub food: i32,
    pub profession: VillagerProfession,
    slept: Option<i64>,
    pub xp: i32,
    pub job_site: Option<BlockPos>,
    pub potential_job_site: bool,
    golem_until: u64,
    last_gossip: Option<u64>,
}

#[derive(Default)]
pub struct Facts {
    map: HashMap<i32, Fact>,
    pruned: u64,
}

impl Facts {
    fn publish(&mut self, id: i32, tick: u64, now: Fact) {
        if tick >= self.pruned + PRUNE_INTERVAL {
            self.pruned = tick;
            self.map.retain(|_, f| f.seen + PRUNE_AGE >= tick);
        }
        let fact = self.map.entry(id).or_insert(now);
        *fact = Fact {
            golem_until: fact.golem_until,
            last_gossip: fact.last_gossip,
            ..now
        };
    }

    fn fresh(&self, id: i32, tick: u64) -> Option<&Fact> {
        self.map.get(&id).filter(|f| f.seen + 2 >= tick)
    }

    pub fn wants_golem(&self, id: i32, tick: u64) -> bool {
        self.fresh(id, tick).is_some_and(|f| {
            f.golem_until <= tick
                && f.slept
                    .is_some_and(|slept| (tick as i64) - slept < GOLEM_SLEEP_WINDOW)
        })
    }

    #[cfg(test)]
    pub fn publish_rested(&mut self, id: i32, tick: u64) {
        let now = Fact {
            seen: tick,
            food: 0,
            profession: VillagerProfession::None,
            slept: Some(tick as i64),
            xp: 0,
            job_site: None,
            potential_job_site: false,
            golem_until: 0,
            last_gossip: None,
        };
        self.publish(id, tick, now);
    }

    fn off_cooldown(&self, id: i32, tick: u64) -> bool {
        self.map
            .get(&id)
            .and_then(|f| f.last_gossip)
            .is_none_or(|last| tick >= last + GOSSIP_COOLDOWN)
    }

    fn talked(&mut self, id: i32, tick: u64) {
        if let Some(fact) = self.map.get_mut(&id) {
            fact.last_gossip = Some(tick);
        }
    }

    fn detected_golem(&mut self, id: i32, tick: u64) {
        if let Some(fact) = self.map.get_mut(&id) {
            fact.golem_until = tick + GOLEM_MEMORY;
        }
    }
}

/// Players each villager wants an iron golem to fight (reputation at or
/// below [`GRUDGE_REPUTATION`]), by villager id, republished every tick.
#[derive(Default)]
pub struct Grudges {
    map: HashMap<i32, (u64, Vec<u128>)>,
}

pub const GRUDGE_REPUTATION: i32 = -100;

impl Grudges {
    fn publish(&mut self, id: i32, tick: u64, players: Vec<u128>) {
        if players.is_empty() {
            self.map.remove(&id);
        } else {
            self.map.insert(id, (tick, players));
        }
    }

    /// The players villager `id` holds a grudge against, if it said so
    /// lately.
    pub fn of(&self, id: i32, tick: u64) -> &[u128] {
        self.map
            .get(&id)
            .filter(|(seen, _)| seen + 2 >= tick)
            .map_or(&[], |(_, players)| players)
    }
}

/// Who a villager walks up to and until when.
#[derive(Debug, Clone, Copy)]
pub struct Interaction {
    pub(super) target: i32,
    until: u64,
    at_bell: bool,
    cat: bool,
}

#[derive(Debug, Default)]
pub struct State {
    pub interaction: Option<Interaction>,
    next_golem_scan: u64,
}

/// Villagers asking another for what it has heard.
pub type GossipRequests = super::super::Mail<i32>;
/// A job site another villager gave up to the one it suits better.
pub type JobOffers = super::super::Mail<BlockPos>;

fn is_wanted_food(stack: &ItemStack) -> bool {
    stack.item.villager_food().is_some()
}

impl Villager {
    /// Publishes what neighbours may read, looks for golems around, and
    /// answers requests for gossip.
    pub(super) fn social_upkeep(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let id = core.base.id;
        let now = Fact {
            seen: cx.tick,
            food: self.food_in_inventory(),
            profession: self.data.profession,
            slept: self.brain.memories.last_slept,
            xp: self.xp,
            job_site: self.brain.memories.job_site.as_ref().map(|p| p.pos),
            potential_job_site: self.brain.memories.potential_job_site.is_some(),
            golem_until: 0,
            last_gossip: None,
        };
        cx.state.get::<Facts>().publish(id, cx.tick, now);
        let grudges = cx
            .players
            .iter()
            .filter(|p| self.gossips.reputation(p.uuid) <= GRUDGE_REPUTATION)
            .map(|p| p.uuid)
            .collect();
        cx.state.get::<Grudges>().publish(id, cx.tick, grudges);
        let offers = cx.state.get::<JobOffers>().take(id);
        for site in offers {
            if self.brain.memories.job_site.is_none()
                && self.brain.memories.potential_job_site.is_none()
            {
                self.brain.memories.potential_job_site = Some(super::GlobalPos {
                    dimension: super::dimension_key(cx),
                    pos: site,
                });
                self.brain.potential_since = Some(cx.tick);
                core.move_to_block(site, 1, SPEED, cx);
            }
        }
        let requests = cx.state.get::<GossipRequests>().take(id);
        for requester in requests {
            cx.state.get::<super::super::GossipMail>().send(
                cx.tick,
                requester,
                self.gossips.clone(),
            );
        }
        if self.brain.social.next_golem_scan == 0 {
            self.brain.social.next_golem_scan = cx.tick + cx.rng.gen_range(0..GOLEM_SCAN_INTERVAL);
        }
        if cx.tick >= self.brain.social.next_golem_scan {
            self.brain.social.next_golem_scan = cx.tick + GOLEM_SCAN_INTERVAL;
            let golem_near = cx.entities.iter().any(|e| {
                e.kind == EntityType::IronGolem
                    && e.living.as_ref().is_some_and(|l| l.alive)
                    && distance_sq(e.position, core.base.position) <= GOLEM_SCAN_RANGE_SQ
            });
            if golem_near {
                cx.state.get::<Facts>().detected_golem(id, cx.tick);
            }
        }
    }

    fn villager_near(
        &self,
        core: &MobCore,
        cx: &TickContext,
        limit_sq: f64,
    ) -> Option<EntitySummary> {
        cx.entities
            .iter()
            .filter(|e| e.kind == EntityType::Villager && e.id != core.base.id)
            .filter(|e| e.living.as_ref().is_some_and(|l| l.alive))
            .map(|e| (distance_sq(e.position, core.base.position), e))
            .filter(|(d, _)| *d <= limit_sq)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, e)| *e)
    }

    fn cat_near(&self, core: &MobCore, cx: &TickContext) -> Option<EntitySummary> {
        cx.entities
            .iter()
            .filter(|e| e.kind == EntityType::Cat)
            .filter(|e| e.living.as_ref().is_some_and(|l| l.alive))
            .map(|e| (distance_sq(e.position, core.base.position), e))
            .filter(|(d, _)| *d <= INTERACT_RANGE_SQ)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, e)| *e)
    }

    /// Wanders up to a villager close by now and then, and deals with the
    /// one it has walked up to.
    pub(super) fn interact_with_villagers(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if let Some(interaction) = self.brain.social.interaction {
            self.interact_step(core, cx, interaction);
            return;
        }
        if !core.navigation.is_done() || cx.rng.gen_range(0..INTERACT_CHANCE) != 0 {
            return;
        }
        if !self.start_villager_interaction(core, cx) {
            self.start_cat_interaction(core, cx);
        }
    }

    /// Continues a walk up to a villager or cat; false when there is none.
    pub(super) fn interaction_step(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(interaction) = self.brain.social.interaction else {
            return false;
        };
        self.interact_step(core, cx, interaction);
        true
    }

    pub(super) fn start_villager_interaction(
        &mut self,
        core: &mut MobCore,
        cx: &mut TickContext,
    ) -> bool {
        let Some(other) = self.villager_near(core, cx, INTERACT_RANGE_SQ) else {
            return false;
        };
        self.brain.social.interaction = Some(Interaction {
            target: other.id,
            until: cx.tick + INTERACT_TIMEOUT,
            at_bell: false,
            cat: false,
        });
        core.move_to_entity_reach(other.position, 2, SPEED, cx);
        true
    }

    pub(super) fn start_cat_interaction(
        &mut self,
        core: &mut MobCore,
        cx: &mut TickContext,
    ) -> bool {
        let Some(cat) = self.cat_near(core, cx) else {
            return false;
        };
        self.brain.social.interaction = Some(Interaction {
            target: cat.id,
            until: cx.tick + INTERACT_TIMEOUT,
            at_bell: false,
            cat: true,
        });
        core.move_to_entity_reach(cat.position, 2, SPEED, cx);
        true
    }

    /// At the bell, now and then walks up to a villager close by.
    pub(super) fn socialize_at_bell(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if let Some(interaction) = self.brain.social.interaction {
            self.interact_step(core, cx, interaction);
            return;
        }
        let Some(bell) = self.brain.memories.meeting_point.clone() else {
            return;
        };
        if cx.rng.gen_range(0..BELL_SOCIAL_CHANCE) != 0
            || distance_sq(center(bell.pos), core.base.position) >= BELL_SOCIAL_NEAR.powi(2)
        {
            return;
        }
        if let Some(other) = self.villager_near(core, cx, BELL_SOCIAL_RANGE_SQ) {
            self.brain.social.interaction = Some(Interaction {
                target: other.id,
                until: cx.tick + INTERACT_TIMEOUT,
                at_bell: true,
                cat: false,
            });
            core.look_at_point([
                other.position[0],
                other.position[1] + 1.5,
                other.position[2],
            ]);
            core.move_to_entity_reach(other.position, 1, BELL_SOCIAL_SPEED, cx);
        }
    }

    pub(super) fn end_interaction(&mut self) {
        self.brain.social.interaction = None;
    }

    fn interact_step(
        &mut self,
        core: &mut MobCore,
        cx: &mut TickContext,
        interaction: Interaction,
    ) {
        let target = cx
            .entities
            .iter()
            .find(|e| e.id == interaction.target)
            .filter(|e| {
                e.kind
                    == if interaction.cat {
                        EntityType::Cat
                    } else {
                        EntityType::Villager
                    }
            })
            .filter(|e| e.living.as_ref().is_some_and(|l| l.alive))
            .copied();
        let Some(target) = target.filter(|_| cx.tick < interaction.until) else {
            self.brain.social.interaction = None;
            return;
        };
        let speed = if interaction.at_bell {
            BELL_SOCIAL_SPEED
        } else {
            SPEED
        };
        core.look_at_point([
            target.position[0],
            target.position[1] + 1.5,
            target.position[2],
        ]);
        if distance_sq(target.position, core.base.position) > INTERACT_DISTANCE_SQ {
            if core.navigation.is_done() {
                core.move_to_entity_reach(target.position, 2, speed, cx);
            }
            return;
        }
        core.navigation.stop();
        if interaction.cat {
            return;
        }
        self.gossip_with(core, &target, cx);
        self.share_goods(core, &target, cx);
    }

    /// Asks the other for its gossip, if neither talked lately, and sees
    /// whether the village wants a golem.
    fn gossip_with(&mut self, core: &MobCore, other: &EntitySummary, cx: &mut TickContext) {
        let (id, tick) = (core.base.id, cx.tick);
        let mut facts = cx.state.get::<Facts>();
        if !facts.off_cooldown(id, tick) || !facts.off_cooldown(other.id, tick) {
            return;
        }
        facts.talked(id, tick);
        facts.talked(other.id, tick);
        drop(facts);
        cx.state.get::<GossipRequests>().send(tick, other.id, id);
        self.call_golem_if_needed(core, cx, GOLEM_VILLAGERS);
    }

    pub(super) fn call_golem_if_needed(
        &mut self,
        core: &MobCore,
        cx: &mut TickContext,
        required: usize,
    ) {
        let tick = cx.tick;
        if !cx.state.get::<Facts>().wants_golem(core.base.id, tick) {
            return;
        }
        let area = physics::inflate(core.bounds(), [GOLEM_BOX; 3]);
        let nearby: Vec<i32> = cx
            .entities
            .iter()
            .filter(|e| e.kind == EntityType::Villager && e.id != core.base.id)
            .filter(|e| e.living.as_ref().is_some_and(|l| l.alive))
            .filter(|e| physics::intersects(e.bounds, area))
            .map(|e| e.id)
            .collect();
        let facts = cx.state.get::<Facts>();
        let wanting = 1 + nearby
            .iter()
            .filter(|id| facts.wants_golem(**id, tick))
            .count();
        drop(facts);
        if wanting < required || !self.spawn_golem(core, cx) {
            return;
        }
        let mut facts = cx.state.get::<Facts>();
        facts.detected_golem(core.base.id, tick);
        for id in nearby {
            facts.detected_golem(id, tick);
        }
    }

    fn spawn_golem(&mut self, core: &MobCore, cx: &mut TickContext) -> bool {
        let start = block_pos(core.base.position);
        for _ in 0..GOLEM_ATTEMPTS {
            let dx = cx.rng.gen_range(-GOLEM_RANGE_XZ..=GOLEM_RANGE_XZ);
            let dz = cx.rng.gen_range(-GOLEM_RANGE_XZ..=GOLEM_RANGE_XZ);
            let Some(spot) = golem_spot(
                BlockPos::new(start.x + dx, start.y + GOLEM_RANGE_Y, start.z + dz),
                cx,
            ) else {
                continue;
            };
            if !golem_spawn_allowed(spot, cx) {
                continue;
            }
            let position = [spot.x as f64 + 0.5, spot.y as f64, spot.z as f64 + 0.5];
            let id = cx.shared.next_entity_id();
            let yaw = cx.rng.gen_range(0.0..360.0);
            let Some(golem) =
                cx.shared
                    .entity_types
                    .create(EntityType::IronGolem, id, position, yaw)
            else {
                continue;
            };
            if !spawning::unobstructed_for(EntityType::IronGolem, golem.bounds(), cx.block) {
                continue;
            }
            cx.effects.push(Effect::Spawn(golem));
            return true;
        }
        false
    }

    /// Throws food it can spare and goods the other one's profession asks
    /// for to the other villager.
    fn share_goods(&mut self, core: &MobCore, other: &EntitySummary, cx: &mut TickContext) {
        let Some(fact) = cx.state.get::<Facts>().fresh(other.id, cx.tick).copied() else {
            return;
        };
        let farmer = self.data.profession == VillagerProfession::Farmer;
        if self.food_in_inventory() >= EXCESS_FOOD && (farmer || fact.food < HUNGRY) {
            self.throw_half(core, other, cx, is_wanted_food, KEEP_FOOD_STACK);
        }
        if farmer && self.count_of(Item::Wheat) > KEEP_WHEAT as u32 {
            self.throw_half(core, other, cx, |s| s.item == Item::Wheat, KEEP_FOOD_STACK);
        }
        let own = self.data.profession.info().requested_items;
        let willing: Vec<Item> = fact
            .profession
            .info()
            .requested_items
            .iter()
            .copied()
            .filter(|item| !own.contains(item))
            .collect();
        if !willing.is_empty() {
            self.throw_half(
                core,
                other,
                cx,
                |s| willing.contains(&s.item),
                KEEP_FOOD_STACK,
            );
        }
    }

    pub(super) fn count_of(&self, item: Item) -> u32 {
        self.inventory
            .iter()
            .flatten()
            .filter(|s| s.item == item)
            .map(|s| s.count() as u32)
            .sum()
    }

    /// Throws half of the first matching stack (or what is over `keep`)
    /// toward `other`.
    fn throw_half(
        &mut self,
        core: &MobCore,
        other: &EntitySummary,
        cx: &mut TickContext,
        matches: impl Fn(&ItemStack) -> bool,
        keep: u8,
    ) {
        for slot in self.inventory.iter_mut() {
            let Some(stack) = slot else {
                continue;
            };
            if !matches(stack) {
                continue;
            }
            let max = stack.max_stack_size();
            let count = if stack.count() > max / 2 {
                stack.count() / 2
            } else if stack.count() > keep {
                stack.count() - keep
            } else {
                continue;
            };
            let thrown = stack.take(count);
            if stack.count() == 0 {
                *slot = None;
            }
            let from = [
                core.base.position[0],
                core.base.position[1] + core.size.eye_height as f64 - THROW_HAND_BELOW_EYE,
                core.base.position[2],
            ];
            let toward = [
                other.position[0] - core.base.position[0],
                other.position[1] - core.base.position[1],
                other.position[2] - core.base.position[2],
            ];
            let length = (toward[0] * toward[0] + toward[1] * toward[1] + toward[2] * toward[2])
                .sqrt()
                .max(1e-6);
            cx.effects.push(Effect::DropItem {
                stack: thrown,
                position: from,
                velocity: toward.map(|c| c / length * THROW_SPEED),
                pickup_delay: THROW_PICKUP_DELAY,
            });
            return;
        }
    }
}

fn spoils_golem_spawn(state: BlockState) -> bool {
    let block = state.block();
    let name = block.name();
    matches!(
        block,
        Block::Cobweb
            | Block::Cactus
            | Block::Conduit
            | Block::Ice
            | Block::Tnt
            | Block::Glowstone
            | Block::Beacon
            | Block::SeaLantern
            | Block::FrostedIce
            | Block::TintedGlass
            | Block::Glass
    ) || name.ends_with("glass_pane")
        || name.ends_with("stained_glass")
        || tuff_datapack::tags::is(tag::block::key::LEAVES, block)
}

/// Where a golem called near `above` stands: the first solid block below
/// (within the vertical range) with open air above it.
fn golem_spawn_allowed(spot: BlockPos, cx: &TickContext) -> bool {
    let in_border = cx
        .shared
        .dimension(cx.dimension)
        .border()
        .contains_block(spot);
    in_border
        && (0..3).all(|up| {
            let at = BlockPos::new(spot.x, spot.y + up, spot.z);
            let state = (cx.block)(at);
            if up == 0 {
                state.is_some_and(|state| {
                    !state.is_collision_full_block()
                        && !state.is_signal_source()
                        && !state.is_dangerous_to(EntityType::IronGolem)
                })
            } else {
                spawning::valid_empty_spawn_block(state, EntityType::IronGolem)
            }
        })
}

fn golem_spot(mut search: BlockPos, cx: &TickContext) -> Option<BlockPos> {
    let mut above_state = (cx.block)(search)?;
    for _ in 0..=(2 * GOLEM_RANGE_Y) {
        search = BlockPos::new(search.x, search.y - 1, search.z);
        let current = (cx.block)(search)?;
        let open = tuff_world_model::chunk::is_air(above_state) || above_state.is_liquid();
        let ground = current.is_solid() || current.block() == Block::PowderSnow;
        if !spoils_golem_spawn(current) && open && ground {
            return Some(BlockPos::new(search.x, search.y + 1, search.z));
        }
        above_state = current;
    }
    None
}

impl Villager {
    /// Gives up a job site another villager with the same trade holds and
    /// has more experience at.
    pub(super) fn compete_for_job_site(&mut self, core: &MobCore, cx: &mut TickContext) {
        let Some(site) = self.brain.memories.job_site.clone() else {
            return;
        };
        let held = self.data.profession.info().job_site;
        let loses = cx.entities.iter().any(|e| {
            e.kind == EntityType::Villager
                && e.id != core.base.id
                && e.living.as_ref().is_some_and(|l| l.alive)
                && cx
                    .state
                    .get::<Facts>()
                    .fresh(e.id, cx.tick)
                    .is_some_and(|f| {
                        f.job_site == Some(site.pos)
                            && f.profession.info().job_site == held
                            && f.xp >= self.xp
                    })
        });
        if loses {
            self.brain.memories.job_site = None;
        }
    }

    /// A jobless villager steps aside for a nearby one whose trade fits
    /// the site it is heading for.
    pub(super) fn yield_job_site(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let Some(potential) = self.brain.memories.potential_job_site.clone() else {
            return;
        };
        if core.is_baby()
            || self.data.profession != VillagerProfession::None
            || self.brain.memories.job_site.is_some()
        {
            return;
        }
        let Some(kind) = cx.pois.kind_at(potential.pos) else {
            return;
        };
        let taker = cx.entities.iter().find(|e| {
            e.kind == EntityType::Villager
                && e.id != core.base.id
                && e.living.as_ref().is_some_and(|l| l.alive)
                && cx
                    .state
                    .get::<Facts>()
                    .fresh(e.id, cx.tick)
                    .is_some_and(|f| {
                        !f.potential_job_site
                            && f.profession.info().job_site == Some(kind)
                            && f.job_site.is_none_or(|site| site == potential.pos)
                    })
        });
        let Some(taker) = taker else {
            return;
        };
        let (id, has_site) = (
            taker.id,
            cx.state
                .get::<Facts>()
                .fresh(taker.id, cx.tick)
                .is_some_and(|f| f.job_site.is_some()),
        );
        core.navigation.stop();
        self.brain.memories.potential_job_site = None;
        self.brain.potential_since = None;
        if !has_site {
            cx.state.get::<JobOffers>().send(cx.tick, id, potential.pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn golem_spawn_spots_need_empty_blocks_above_the_ground() {
        let spot = BlockPos::new(0, 70, 0);
        let world = |pos: BlockPos| {
            Some(if pos.y < 70 {
                Block::Stone.default_state()
            } else if pos.y == 71 && pos.x == 5 {
                Block::Water.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        crate::tests::with_world(&world, &[], |_, cx| {
            assert!(golem_spawn_allowed(spot, cx));
            assert!(!golem_spawn_allowed(BlockPos::new(5, 70, 0), cx));
            assert!(!golem_spawn_allowed(BlockPos::new(0, 69, 0), cx));
        });
    }
}
