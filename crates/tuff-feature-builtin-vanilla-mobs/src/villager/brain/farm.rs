//! A farmer's field work: harvesting ripe crops, replanting, spreading
//! bone meal, and baking bread from wheat at its composter.

use rand::Rng;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{SoundEvent, VillagerProfession};
use tuff_vanilla_data::{Block, BlockState, Item, tag};

use super::{Villager, center, distance_sq};
use crate::MobCore;
use crate::physics::block_pos;
use tuff_core::world::entity::{Effect, TickContext};
use tuff_core::world::item_port::{PortAccess, Porter};
use tuff_core::world::region_api::{RegionTask, RegionWorld};
use tuff_item::item::ItemStack;
use tuff_world_model::BlockPos;

const SCAN_INTERVAL: u64 = 40;
const SCAN_X: i32 = 4;
const SCAN_Y: i32 = 2;
const HARVEST_TICKS: u32 = 200;
const HARVEST_TIMEOUT: u64 = 60;
const HARVEST_COOLDOWN: u64 = 40;
const HARVEST_RETARGET_DELAY: u64 = 20;
const PLANT_GROWTH_EVENT: i32 = 1505;
const BONEMEAL_TICKS: u32 = 80;
const BONEMEAL_RETRY: u64 = 160;
const BONEMEAL_STEP: u64 = 40;
const BONEMEAL_CHECK: u64 = 10;
const SPEED: f64 = 0.5;
const BREAD_CAP: u32 = 36;
const BREAD_PER_VISIT: u32 = 3;
const WHEAT_PER_BREAD: u32 = 3;
const COMPOSTED_SEEDS: [Item; 2] = [Item::WheatSeeds, Item::BeetrootSeeds];
const SEEDS_KEPT: u32 = 10;
const SEEDS_PER_VISIT: u32 = 20;
const REACH: f64 = 1.0;

#[derive(Debug, Default)]
pub struct State {
    pub fields: Vec<BlockPos>,
    next_scan: u64,
    session: Option<Session>,
    next_harvest: u64,
    last_bonemeal: u64,
}

#[derive(Debug, Clone)]
enum Session {
    Harvest {
        target: BlockPos,
        candidates: Vec<BlockPos>,
        worked: u32,
        next_ok: u64,
        until: u64,
    },
    Bonemeal {
        target: BlockPos,
        worked: u32,
        next_ok: u64,
    },
}

fn is_crop(state: BlockState) -> bool {
    state.age().is_some() && crop_max(state.block()) > 0
}

fn crop_max(block: Block) -> u8 {
    if !matches!(
        block,
        Block::Wheat | Block::Carrots | Block::Potatoes | Block::Beetroots | Block::TorchflowerCrop
    ) {
        return 0;
    }
    block
        .states()
        .filter_map(BlockState::age)
        .max()
        .unwrap_or(0)
}

fn ripe(state: BlockState) -> bool {
    is_crop(state) && state.age() == Some(crop_max(state.block()))
}

fn seed_block(item: Item) -> Option<Block> {
    Some(match item {
        Item::WheatSeeds => Block::Wheat,
        Item::Carrot => Block::Carrots,
        Item::Potato => Block::Potatoes,
        Item::BeetrootSeeds => Block::Beetroots,
        Item::TorchflowerSeeds => Block::TorchflowerCrop,
        _ => return None,
    })
}

fn is_farmland(state: Option<BlockState>) -> bool {
    state.is_some_and(|s| s.block() == Block::Farmland)
}

fn put_away_bone_meal(core: &mut MobCore) {
    core.equipment.set(EquipmentSlot::Mainhand, None);
}

impl Villager {
    pub(super) fn is_farmer(&self) -> bool {
        self.data.profession == VillagerProfession::Farmer
    }

    /// Looks for the farmland around it every so often.
    pub(super) fn scan_fields(&mut self, core: &MobCore, cx: &TickContext) {
        if cx.tick < self.brain.farm.next_scan {
            return;
        }
        self.brain.farm.next_scan = cx.tick + SCAN_INTERVAL;
        let secondary = self.data.profession.info().secondary_blocks;
        let mut found = Vec::new();
        if !secondary.is_empty() {
            let here = block_pos(core.base.position);
            for x in -SCAN_X..=SCAN_X {
                for y in -SCAN_Y..=SCAN_Y {
                    for z in -SCAN_X..=SCAN_X {
                        let pos = BlockPos::new(here.x + x, here.y + y, here.z + z);
                        if (cx.block)(pos).is_some_and(|s| secondary.contains(&s.block())) {
                            found.push(pos);
                        }
                    }
                }
            }
        }
        self.brain.farm.fields = found;
    }

    pub(super) fn stop_farming(&mut self, core: &mut MobCore) {
        if matches!(self.brain.farm.session, Some(Session::Bonemeal { .. })) {
            put_away_bone_meal(core);
        }
        self.brain.farm.session = None;
    }

    fn cells_around(&self, core: &MobCore, keep: impl Fn(BlockPos) -> bool) -> Vec<BlockPos> {
        let here = block_pos(core.base.position);
        let mut cells = Vec::new();
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    let pos = BlockPos::new(here.x + x, here.y + y, here.z + z);
                    if keep(pos) {
                        cells.push(pos);
                    }
                }
            }
        }
        cells
    }

    fn harvestable(cx: &TickContext, pos: BlockPos) -> bool {
        match (cx.block)(pos) {
            Some(state) if ripe(state) => true,
            Some(state) if tuff_world_model::chunk::is_air(state) => {
                is_farmland((cx.block)(BlockPos::new(pos.x, pos.y - 1, pos.z)))
            }
            _ => false,
        }
    }

    /// Starts a harvest round, if the griefing rule and the field allow.
    pub(super) fn start_harvest(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        if !self.is_farmer()
            || self.brain.farm.fields.is_empty()
            || self.brain.farm.session.is_some()
            || cx.tick < self.brain.farm.next_harvest
            || !cx.shared.game_rules.borrow().bool("mob_griefing")
        {
            return false;
        }
        let candidates = self.cells_around(core, |pos| Self::harvestable(cx, pos));
        if candidates.is_empty() {
            return false;
        }
        let target = candidates[cx.rng.gen_range(0..candidates.len())];
        core.move_to_block(target, 1, SPEED, cx);
        self.brain.farm.session = Some(Session::Harvest {
            target,
            candidates,
            worked: 0,
            next_ok: cx.tick,
            until: cx.tick + HARVEST_TIMEOUT,
        });
        true
    }

    /// Starts spreading bone meal over crops that still grow.
    pub(super) fn start_bonemeal(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.brain.farm.session.is_some()
            || !cx.tick.is_multiple_of(BONEMEAL_CHECK)
            || (self.brain.farm.last_bonemeal != 0
                && self.brain.farm.last_bonemeal + BONEMEAL_RETRY > cx.tick)
            || self.count_of(Item::BoneMeal) == 0
        {
            return false;
        }
        let Some(target) = self.pick_growing_crop(core, cx) else {
            return false;
        };
        core.move_to_block(target, 1, SPEED, cx);
        core.equipment.set(
            EquipmentSlot::Mainhand,
            Some(ItemStack::new(Item::BoneMeal, 1)),
        );
        self.brain.farm.session = Some(Session::Bonemeal {
            target,
            worked: 0,
            next_ok: cx.tick,
        });
        true
    }

    fn pick_growing_crop(&self, core: &MobCore, cx: &mut TickContext) -> Option<BlockPos> {
        let crops = self.cells_around(core, |pos| {
            (cx.block)(pos).is_some_and(|s| is_crop(s) && !ripe(s))
        });
        (!crops.is_empty()).then(|| crops[cx.rng.gen_range(0..crops.len())])
    }

    /// Runs the session in progress; true while it holds the villager.
    pub(super) fn farm_step(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(session) = self.brain.farm.session.take() else {
            return false;
        };
        let (next, keep) = match session {
            Session::Harvest {
                target,
                candidates,
                worked,
                next_ok,
                until,
            } => {
                let (next, mut keep) =
                    self.harvest_step(core, cx, target, candidates, worked, next_ok);
                if keep && cx.tick > until {
                    keep = false;
                    self.brain.farm.next_harvest = cx.tick + HARVEST_COOLDOWN;
                }
                match next {
                    Session::Harvest {
                        target,
                        candidates,
                        worked,
                        next_ok,
                        ..
                    } => (
                        Session::Harvest {
                            target,
                            candidates,
                            worked,
                            next_ok,
                            until,
                        },
                        keep,
                    ),
                    other => (other, keep),
                }
            }
            Session::Bonemeal {
                target,
                worked,
                next_ok,
            } => self.bonemeal_step(core, cx, target, worked, next_ok),
        };
        let bonemeal = matches!(next, Session::Bonemeal { .. });
        self.brain.farm.session = keep.then_some(next);
        if !keep {
            if bonemeal {
                put_away_bone_meal(core);
            }
            core.navigation.stop();
        }
        true
    }

    fn harvest_step(
        &mut self,
        core: &mut MobCore,
        cx: &mut TickContext,
        mut target: BlockPos,
        mut candidates: Vec<BlockPos>,
        mut worked: u32,
        mut next_ok: u64,
    ) -> (Session, bool) {
        let done = |target, candidates, worked, next_ok| Session::Harvest {
            target,
            candidates,
            worked,
            next_ok,
            until: 0,
        };
        if worked >= HARVEST_TICKS {
            self.brain.farm.next_harvest = cx.tick + HARVEST_COOLDOWN;
            return (done(target, candidates, worked, next_ok), false);
        }
        core.look_at_point(center(target));
        if distance_sq(center(target), core.base.position) >= REACH * REACH {
            if core.navigation.is_done() {
                core.move_to_block(target, 1, SPEED, cx);
            }
            return (done(target, candidates, worked, next_ok), true);
        }
        if cx.tick > next_ok {
            let state = (cx.block)(target);
            let was_crop = state.is_some_and(is_crop);
            if state.is_some_and(ripe) {
                cx.effects.push(Effect::DestroyBlock {
                    pos: target,
                    drop: true,
                });
            }
            let below = (cx.block)(BlockPos::new(target.x, target.y - 1, target.z));
            if state.is_some_and(tuff_world_model::chunk::is_air) && is_farmland(below) {
                self.plant(core, target, cx);
            }
            if was_crop && state.is_some_and(|s| !ripe(s)) {
                candidates.retain(|c| *c != target);
                if !candidates.is_empty() {
                    target = candidates[cx.rng.gen_range(0..candidates.len())];
                    next_ok = cx.tick + HARVEST_RETARGET_DELAY;
                    core.move_to_block(target, 1, SPEED, cx);
                }
            }
        }
        worked += 1;
        (done(target, candidates, worked, next_ok), true)
    }

    fn plant(&mut self, core: &MobCore, pos: BlockPos, cx: &mut TickContext) {
        for slot in self.inventory.iter_mut() {
            let Some(stack) = slot else {
                continue;
            };
            if !tuff_datapack::tags::is(tag::item::key::VILLAGER_PLANTABLE_SEEDS, stack.item) {
                continue;
            }
            let Some(block) = seed_block(stack.item) else {
                continue;
            };
            cx.effects.push(Effect::SetBlock {
                pos,
                state: block.default_state(),
            });
            core.sound(SoundEvent::ItemCropPlant, 1.0, 1.0, cx);
            stack.count -= 1;
            if stack.count == 0 {
                *slot = None;
            }
            return;
        }
    }

    fn bonemeal_step(
        &mut self,
        core: &mut MobCore,
        cx: &mut TickContext,
        mut target: BlockPos,
        worked: u32,
        mut next_ok: u64,
    ) -> (Session, bool) {
        let session = |target, worked, next_ok| Session::Bonemeal {
            target,
            worked,
            next_ok,
        };
        if worked >= BONEMEAL_TICKS {
            self.brain.farm.last_bonemeal = cx.tick;
            return (session(target, worked, next_ok), false);
        }
        core.look_at_point(center(target));
        if cx.tick < next_ok || distance_sq(center(target), core.base.position) >= REACH * REACH {
            if core.navigation.is_done() && cx.tick >= next_ok {
                core.move_to_block(target, 1, SPEED, cx);
            }
            return (session(target, worked + 1, next_ok), true);
        }
        let grown = self.bonemeal_crop(target, cx);
        if grown {
            match self.pick_growing_crop(core, cx) {
                Some(next) => {
                    target = next;
                    core.move_to_block(target, 1, SPEED, cx);
                }
                None => {
                    self.brain.farm.last_bonemeal = cx.tick;
                    return (session(target, worked, next_ok), false);
                }
            }
            next_ok = cx.tick + BONEMEAL_STEP;
        }
        (session(target, worked + 1, next_ok), true)
    }

    fn bonemeal_crop(&mut self, pos: BlockPos, cx: &mut TickContext) -> bool {
        let Some(state) = (cx.block)(pos).filter(|s| is_crop(*s) && !ripe(*s)) else {
            return false;
        };
        let Some(slot) = self
            .inventory
            .iter_mut()
            .find(|s| s.as_ref().is_some_and(|s| s.item == Item::BoneMeal))
        else {
            return false;
        };
        let stack = slot.as_mut().expect("found a stack");
        stack.count -= 1;
        if stack.count == 0 {
            *slot = None;
        }
        let max = crop_max(state.block());
        let age = state.age().unwrap_or(0);
        let (succeeds, grow) = if state.block() == Block::Beetroots {
            (cx.rng.gen_range(0.0f32..1.0) < 0.75, 1)
        } else {
            (true, cx.rng.gen_range(2..=5))
        };
        if succeeds {
            let grown = state.with_age((age + grow).min(max)).unwrap_or(state);
            cx.effects.push(Effect::SetBlock { pos, state: grown });
        }
        crate::monster::level_event(pos, PLANT_GROWTH_EVENT, 15, cx);
        true
    }

    /// At its composter it turns wheat into bread.
    pub(super) fn bake_bread(&mut self, core: &MobCore, job_site: BlockPos, cx: &mut TickContext) {
        if !self.is_farmer()
            || (cx.block)(job_site).is_none_or(|s| s.block() != Block::Composter)
            || self.count_of(Item::Bread) > BREAD_CAP
        {
            return;
        }
        let loaves = (self.count_of(Item::Wheat) / WHEAT_PER_BREAD).min(BREAD_PER_VISIT);
        if loaves == 0 {
            return;
        }
        let mut wheat = loaves * WHEAT_PER_BREAD;
        for slot in self.inventory.iter_mut() {
            let Some(stack) = slot else {
                continue;
            };
            if stack.item != Item::Wheat {
                continue;
            }
            let used = wheat.min(stack.count as u32);
            stack.count -= used as u8;
            wheat -= used;
            if stack.count == 0 {
                *slot = None;
            }
            if wheat == 0 {
                break;
            }
        }
        if let Some(rest) = self.add_item(ItemStack::new(Item::Bread, loaves as u8)) {
            core.drop_item(rest, 0.5, cx);
        }
    }

    /// At its composter it empties ripe bone meal and composts its spare
    /// seeds (keeping ten of each, at most twenty a visit), last slot
    /// first, stopping once the composter is full.
    pub(super) fn compost_seeds(&mut self, id: i32, job_site: BlockPos, cx: &mut TickContext) {
        use tuff_vanilla_common::composter;
        let Some(expected) = (cx.block)(job_site) else {
            return;
        };
        let Some(start) = composter::level(expected) else {
            return;
        };
        let emptied = start == composter::READY;
        let before = if emptied { 0 } else { start };
        let mut level = before;
        let mut budget = SEEDS_PER_VISIT;
        let mut seen = [0u32; COMPOSTED_SEEDS.len()];
        'slots: for slot in self.inventory.iter_mut().rev() {
            if budget == 0 {
                break;
            }
            let Some(stack) = slot else {
                continue;
            };
            let Some(index) = COMPOSTED_SEEDS.iter().position(|seed| *seed == stack.item) else {
                continue;
            };
            let count = u32::from(stack.count);
            seen[index] += count;
            let using = (seen[index].saturating_sub(SEEDS_KEPT))
                .min(budget)
                .min(count);
            budget -= using;
            for _ in 0..using {
                if level < composter::FULL
                    && let Some(provider) = composter::compostable(stack)
                {
                    let state = composter::with_level(expected, level);
                    let rng = &mut *cx.rng;
                    let layers = provider.get(&mut tuff_vanilla_data::provider::Context {
                        block: Some(state),
                        random: &mut |bound| rng.gen_range(0..bound.max(1)),
                    });
                    stack.count -= 1;
                    if layers > 0 {
                        level = (i32::from(level) + layers).clamp(0, 7) as u8;
                    }
                }
                if level == composter::FULL {
                    break 'slots;
                }
            }
        }
        for slot in self.inventory.iter_mut() {
            if slot.as_ref().is_some_and(|stack| stack.count == 0) {
                *slot = None;
            }
        }
        let raised = level != before;
        cx.effects.push(Effect::Run(RegionTask::new(move |world| {
            composted(world, id, job_site, expected, emptied, level, raised);
        })));
    }
}

/// Applies a farmer's visit to the composter at `pos`, which it saw as
/// `expected`: a ripe one is emptied through its port first, then it is
/// left at `fill`, with the fill event showing whether a layer went in.
/// Nothing happens if the composter changed meanwhile.
fn composted(
    world: &mut dyn RegionWorld,
    id: i32,
    pos: BlockPos,
    expected: BlockState,
    emptied: bool,
    fill: u8,
    raised: bool,
) {
    use tuff_core::world::game_event::{Context, GameEvent, Source};
    use tuff_vanilla_common::composter;
    if !world.owns(pos) || world.block(pos) != Some(expected) {
        return;
    }
    if emptied && let Some(port) = world.shared().item_ports.get(expected.block()).cloned() {
        let at = PortAccess {
            pos,
            state: expected,
            face: None,
            by: Porter::Mob(id),
        };
        port.take(world, &at);
    }
    let Some(state) = world.block(pos) else {
        return;
    };
    if composter::level(state) != Some(fill) {
        let next = composter::with_level(state, fill);
        world.set_block_flags(pos, next, tuff_core::world::redstone::UPDATE_ALL);
        let context = Context {
            source: Some(Source::entity(id)),
            affected: Some(next),
        };
        world.game_event(GameEvent::BlockChange, pos.center(), context);
    }
    world.broadcast_level_event(composter::COMPOSTER_FILL, pos, i32::from(raised));
}
