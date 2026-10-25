//! Falling blocks (sand, gravel, anvils, dragon eggs, ...): the block turns
//! into a [`FallingBlock`] entity while its old spot loses support, falls
//! under gravity, then either places itself back or breaks into an item.
//!
//! Scaffolding and speleothems start falling through their own rules (see
//! `world::scaffolding` and `world::speleothem`) but fall as this same
//! entity. Carrying a block entity's data through the fall is out of scope.
//!
//! Blocks in the `minecraft:anvil` tag hurt living things they land on and
//! may degrade (or break apart) from the impact; a falling stalactite tip
//! is set to hurt with [`FallingBlock::hurts_entities`].

use rand::Rng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, BlockState, Item};

use tuff_core::protocol::play;
use tuff_core::world::block_ledger::{BlockBurn, HeldBlock};
use tuff_core::world::entity::{
    Effect, EntityBase, EntityHit, RegionEntity, Tick, TickContext, take, take_bool, take_number,
};
use tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_common::placement;
use tuff_world_model::BlockPos;

const GRAVITY: f64 = 0.04;
const AIR_DRAG: f64 = 0.98;
/// Ticks a falling block keeps trying to land before it gives up and drops
/// (or vanishes, with no drop, if it never got a chance to).
const MAX_LIFETIME: u32 = 600;
const OUT_OF_WORLD_TIMEOUT: u32 = 100;
/// Damage per block fallen and its cap that an anvil starts falling with;
/// game code only, not in any data report.
const ANVIL_HURT_AMOUNT: f32 = 2.0;
const DEFAULT_HURT_MAX: i32 = 40;
/// How far below its feet the block it lands on is looked up.
const LANDED_ON_DEPTH: f64 = 0.2;
/// The chance of an anvil degrading after it dealt damage is this plus
/// this again per whole block fallen.
const DEGRADE_CHANCE: f32 = 0.05;

/// How long a falling block that came down inside an End portal waits to
/// be taken through it before it settles where it is.
const PORTAL_WAIT_TICKS: u32 = 40;

/// A block, no longer in the world, falling under gravity. It owns the
/// block it was made from until it lands, breaks or is removed.
#[derive(Debug)]
pub struct FallingBlock {
    pub base: EntityBase,
    /// What the block looks like now (an anvil degrades as it lands).
    pub state: BlockState,
    block: Option<HeldBlock>,
    /// The copy of this block that settled where it entered an End portal
    /// has been made (see [`FallingBlock::settles_in_end_portal`]).
    portal_copy_made: bool,
    portal_wait: u32,
    pub time: u32,
    /// Whether landing hurts living things under it, how much per block
    /// fallen, and the cap.
    pub hurt_entities: bool,
    pub fall_hurt_amount: f32,
    pub fall_hurt_max: i32,
    /// Set when the block broke apart on impact: it vanishes on landing.
    pub cancel_drop: bool,
    /// Whether a block that times out or falls out of the world drops as an item.
    pub drop_item: bool,
    /// Block entity data merged into the block entity it lands as.
    pub block_data: Option<Compound>,
}

fn is_anvil(state: BlockState) -> bool {
    tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::ANVIL, state.block())
}

/// The next worse anvil, keeping its facing, or `None` once it breaks
/// apart. The chain is game code, not data.
pub fn degraded_anvil(state: BlockState) -> Option<BlockState> {
    let next = match state.block() {
        Block::Anvil => Block::ChippedAnvil,
        Block::ChippedAnvil => Block::DamagedAnvil,
        _ => return None,
    };
    let facing = state.get("facing")?;
    next.default_state().with("facing", facing)
}

/// The damage a landing deals after falling `fall_distance` blocks, and
/// the whole blocks fallen the degrade chance uses; `None` if too short a
/// fall to hurt at all.
fn impact_damage(fall_distance: f64, amount: f32, max: i32) -> Option<(f32, i32)> {
    let blocks = (fall_distance - 1.0).ceil() as i32;
    (blocks >= 0).then(|| (((blocks as f32 * amount).floor()).min(max as f32), blocks))
}

/// The block the game falls back to for a missing or unreadable saved
/// state.
fn default_falling_state() -> BlockState {
    Block::Sand.default_state()
}

#[expect(
    clippy::disallowed_methods,
    reason = "held block loaded from a save (load)"
)]
pub fn load(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let state = take(nbt, "BlockState", |tag| {
        let name = match tag {
            Tag::String(name) => Some(name.as_str()),
            Tag::Compound(state) => state.get_str("id").or_else(|| state.get_str("Name")),
            _ => None,
        };
        Block::from_name(name?)?;
        Some(tuff_core::storage::chunk_nbt::decode_block_state(tag))
    })
    .unwrap_or_else(default_falling_state);
    let time = take_number(nbt, "Time").map_or(0, |v| v.max(0.0) as u32);
    let hurt_entities = take_bool(nbt, "HurtEntities").unwrap_or_else(|| is_anvil(state));
    let fall_hurt_amount = take_number(nbt, "FallHurtAmount").map_or(0.0, |v| v as f32);
    let fall_hurt_max = take_number(nbt, "FallHurtMax").map_or(DEFAULT_HURT_MAX, |v| v as i32);
    let cancel_drop = take_bool(nbt, "CancelDrop").unwrap_or(false);
    let drop_item = take_bool(nbt, "DropItem").unwrap_or(true);
    let block_data = take(nbt, "TileEntityData", |tag| tag.as_compound().cloned());
    Some(Box::new(FallingBlock {
        base,
        state,
        block: Some(HeldBlock::from_storage(state)),
        portal_copy_made: false,
        portal_wait: 0,
        time,
        hurt_entities,
        fall_hurt_amount,
        fall_hurt_max,
        cancel_drop,
        drop_item,
        block_data,
    }))
}

/// Whether a falling block above `state` would keep falling into it: air,
/// fire, liquid, or anything else replaceable.
pub fn is_free(state: BlockState) -> bool {
    tuff_world_model::chunk::is_air(state)
        || tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::FIRE, state.block())
        || state.is_liquid()
        || state.is_replaceable()
}

/// Whether a landed falling block may overwrite `state`, asked the way an
/// empty-handed placement from above would.
fn may_replace_landing(state: BlockState, pos: BlockPos) -> bool {
    if tuff_world_model::chunk::is_air(state) {
        return true;
    }
    let context = placement::Context {
        clicked: pos,
        face: Facing::Up,
        cursor: [0.5; 3],
        yaw: 0.0,
        pitch: 0.0,
        sneaking: false,
        item: Item::Air,
        pos,
        replace_clicked: true,
        shape: tuff_world_model::chunk::ChunkShape {
            min_y: i32::MIN / 2,
            height: i32::MAX,
            overworld: false,
        },
        blocked: false,
    };
    placement::can_be_replaced(state, &context)
}

/// What a tick of falling settles into.
enum StepOutcome {
    Falling,
    Land(BlockState, BlockPos),
    Drop,
    /// Broke apart on impact: gone, no item.
    Broken(BlockPos),
}

impl FallingBlock {
    /// The frogspawn the block's box overlaps, which a falling block
    /// destroys without a drop.
    fn frogspawn_inside(&self, level: &impl Fn(BlockPos) -> Option<BlockState>) -> Vec<BlockPos> {
        let bounds = self.bounds();
        let min = [0, 1, 2].map(|i| (bounds[i] + 1.0e-7).floor() as i32);
        let max = [0, 1, 2].map(|i| (bounds[i + 3] - 1.0e-7).floor() as i32);
        let mut found = Vec::new();
        for x in min[0]..=max[0] {
            for y in min[1]..=max[1] {
                for z in min[2]..=max[2] {
                    let pos = BlockPos::new(x, y, z);
                    if level(pos).is_some_and(|state| state.block() == Block::Frogspawn) {
                        found.push(pos);
                    }
                }
            }
        }
        found
    }

    pub fn new(id: i32, uuid: u128, pos: BlockPos, mut block: HeldBlock) -> Self {
        let state = block
            .state()
            .with_waterlogged(false)
            .unwrap_or(block.state());
        block.become_state(state);
        let anvil = is_anvil(state);
        Self {
            base: EntityBase::new(
                EntityType::FallingBlock,
                id,
                uuid,
                [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5],
                [0.0; 3],
            ),
            state,
            block: Some(block),
            portal_copy_made: false,
            portal_wait: 0,
            time: 0,
            hurt_entities: anvil,
            fall_hurt_amount: if anvil { ANVIL_HURT_AMOUNT } else { 0.0 },
            fall_hurt_max: DEFAULT_HURT_MAX,
            cancel_drop: false,
            drop_item: true,
            block_data: None,
        }
    }

    /// Makes the landing hurt living things under it, `amount` per block
    /// fallen up to `max`.
    pub fn hurts_entities(&mut self, amount: f32, max: i32) {
        self.hurt_entities = true;
        self.fall_hurt_amount = amount;
        self.fall_hurt_max = max;
    }

    /// The landing's damage to whatever is under it, if it hurts at all.
    fn landing_damage(&self, fall_distance: f64) -> Option<(f32, i32)> {
        if !self.hurt_entities {
            return None;
        }
        impact_damage(fall_distance, self.fall_hurt_amount, self.fall_hurt_max)
    }

    /// After a landing that dealt `damage` having fallen `blocks` whole
    /// blocks, rolls whether an anvil degrades; `roll` is a uniform draw in
    /// `[0, 1)`.
    fn degrade(&mut self, damage: f32, blocks: i32, roll: f32) {
        if !is_anvil(self.state)
            || damage <= 0.0
            || roll >= DEGRADE_CHANCE + blocks as f32 * DEGRADE_CHANCE
        {
            return;
        }
        match degraded_anvil(self.state) {
            Some(next) => self.state = next,
            None => self.cancel_drop = true,
        }
    }

    pub fn bounds(&self) -> [f64; 6] {
        self.base.bounds()
    }

    /// Moves one tick and settles: `level` reads a block, `None` where the
    /// region cannot see it (treated as solid for collision, and as "do not
    /// decide yet" for the landing check, so a falling block never lands or
    /// drops based on missing information).
    /// `impact` runs when it hits the ground, with the distance fallen and
    /// the block it came down on, before the landing is decided.
    #[cfg(test)]
    fn step_tick(
        &mut self,
        level: impl Fn(BlockPos) -> Option<BlockState>,
        impact: impl FnOnce(&mut Self, f64, Option<BlockState>),
    ) -> StepOutcome {
        self.step_tick_among(level, &[], impact)
    }

    /// [`Self::step_tick`] also stopping at the `pistons` boxes of blocks
    /// pistons are moving.
    fn step_tick_among(
        &mut self,
        level: impl Fn(BlockPos) -> Option<BlockState>,
        pistons: &[tuff_core::world::physics::Aabb],
        impact: impl FnOnce(&mut Self, f64, Option<BlockState>),
    ) -> StepOutcome {
        self.time += 1;
        if !self.base.no_gravity {
            self.base.velocity[1] -= GRAVITY;
        }
        let solid = |x: i32, y: i32, z: i32| {
            level(BlockPos::new(x, y, z)).is_none_or(|state| !state.collision_boxes().is_empty())
        };
        let start = self.base.position;
        let start_y = start[1];
        self.step(&solid);
        tuff_core::world::physics::stop_at_moving_blocks(&mut self.base, start, pistons);
        let dy = self.base.position[1] - start_y;
        let bubbled = tuff_core::world::block_contact::bubble_column_push(
            self.base.bounds(),
            &mut self.base.velocity,
            &level,
        );
        let in_water = level(self.landing_pos()).is_some_and(|s| s.block() == Block::Water);
        if in_water || bubbled {
            self.base.fall_distance = 0.0;
        } else if dy < 0.0 {
            self.base.fall_distance -= dy;
        }
        if self.base.on_ground && self.base.fall_distance > 0.0 {
            let fallen = self.base.fall_distance;
            self.base.fall_distance = 0.0;
            let [x, y, z] = self.base.position;
            let below = level(BlockPos::containing([x, y - LANDED_ON_DEPTH, z]));
            impact(self, fallen, below);
        }
        if self.base.on_ground {
            self.base.velocity[0] *= 0.7;
            self.base.velocity[1] *= -0.5;
            self.base.velocity[2] *= 0.7;
        }
        for v in &mut self.base.velocity {
            *v *= AIR_DRAG;
        }
        if self.base.on_ground {
            return self.try_land(&level);
        }
        if self.time > MAX_LIFETIME {
            StepOutcome::Drop
        } else {
            StepOutcome::Falling
        }
    }

    /// Moves by the velocity, stopping at collision boxes (treated as full
    /// blocks) along each axis in the game's order: y, then x and z.
    fn step(&mut self, solid: &impl Fn(i32, i32, i32) -> bool) {
        let blocked = |bounds: [f64; 6]| {
            let min = [0, 1, 2].map(|i| (bounds[i] + 1.0e-7).floor() as i32);
            let max = [0, 1, 2].map(|i| (bounds[i + 3] - 1.0e-7).floor() as i32);
            (min[0]..=max[0])
                .any(|x| (min[1]..=max[1]).any(|y| (min[2]..=max[2]).any(|z| solid(x, y, z))))
        };
        let dy = self.base.velocity[1];
        let mut moved = self.bounds();
        moved[1] += dy;
        moved[4] += dy;
        self.base.on_ground = false;
        if blocked(moved) {
            if dy < 0.0 {
                self.base.position[1] = (self.base.position[1] + dy).floor() + 1.0;
                self.base.on_ground = true;
            }
            self.base.velocity[1] = 0.0;
        } else {
            self.base.position[1] += dy;
        }
        for axis in [0, 2] {
            let mut moved = self.bounds();
            moved[axis] += self.base.velocity[axis];
            moved[axis + 3] += self.base.velocity[axis];
            if blocked(moved) {
                self.base.velocity[axis] = 0.0;
            } else {
                self.base.position[axis] += self.base.velocity[axis];
            }
        }
    }

    /// The block position a landed falling block would try to occupy: the
    /// cell its box rests in.
    fn out_of_world(&self, min_y: i32, max_y: i32) -> bool {
        let y = self.landing_pos().y;
        self.time > OUT_OF_WORLD_TIMEOUT && (y <= min_y || y > max_y)
    }

    fn landing_pos(&self) -> BlockPos {
        let [x, y, z] = self.base.position;
        BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32)
    }

    fn try_land(&self, level: &impl Fn(BlockPos) -> Option<BlockState>) -> StepOutcome {
        let pos = self.landing_pos();
        let Some(current) = level(pos) else {
            return StepOutcome::Falling;
        };
        if self.cancel_drop {
            return StepOutcome::Broken(pos);
        }
        let Some(below) = level(BlockPos::new(pos.x, pos.y - 1, pos.z)) else {
            return StepOutcome::Falling;
        };
        let may_replace = may_replace_landing(current, pos);
        let would_continue_falling = is_free(below);
        let survive_level = |p: BlockPos| level(p).unwrap_or(Block::Air.default_state());
        let would_survive =
            placement::can_survive(self.state, pos, &survive_level) && !would_continue_falling;
        if !may_replace || !would_survive {
            return StepOutcome::Drop;
        }
        let mut state = self.state;
        if let Some(concrete) = placement::concrete_from_powder(state.block())
            && placement::touches_water(pos, &survive_level)
        {
            state = concrete.default_state();
        } else if current.block() == Block::Water
            && let Some(new_state) = state.with_waterlogged(true)
        {
            state = new_state;
        }
        StepOutcome::Land(state, pos)
    }
}

impl RegionEntity for FallingBlock {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        out.insert(
            "BlockState",
            tuff_core::storage::chunk_nbt::encode_block_state(self.state),
        );
        out.insert("Time", self.time as i32);
        out.insert("HurtEntities", self.hurt_entities);
        out.insert("FallHurtAmount", self.fall_hurt_amount);
        out.insert("FallHurtMax", self.fall_hurt_max);
        out.insert("CancelDrop", self.cancel_drop);
        out.insert("DropItem", self.drop_item);
        if let Some(data) = &self.block_data {
            out.insert("TileEntityData", data.clone());
        }
    }

    fn display_name(&self) -> String {
        format!("Falling {:?}", self.state.block())
    }

    fn spawn_data(&self) -> i32 {
        self.state.id() as i32
    }

    fn take_held(&mut self) -> Option<HeldBlock> {
        self.block.take()
    }

    fn used_portal(&mut self) {
        self.portal_copy_made = false;
        self.portal_wait = 0;
    }

    fn tick(&mut self, context: &mut TickContext) -> Tick {
        let block = context.block;
        let pistons = context.pistons;
        let mut outcome = self.step_tick_among(block, pistons, |me, fallen, below| {
            me.hurt_below(fallen, below, context)
        });
        if matches!(outcome, StepOutcome::Falling)
            && self.out_of_world(context.min_y, context.max_y)
        {
            outcome = StepOutcome::Drop;
        }
        for pos in self.frogspawn_inside(&block) {
            context
                .effects
                .push(Effect::DestroyBlock { pos, drop: false });
        }
        let settling = !matches!(outcome, StepOutcome::Falling);
        let mut token = None;
        let mut waiting = false;
        if settling && self.settles_in_end_portal(context) {
            if !self.portal_copy_made {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "vanilla duplication glitch (tick)"
                )]
                let minted = tuff_core::world::block_ledger::duplicate_held(
                    &context.shared.config,
                    self.state,
                );
                token = minted;
                self.portal_copy_made = token.is_some();
            }
            if self.portal_copy_made {
                self.portal_wait += 1;
                waiting = token.is_none() && self.portal_wait < PORTAL_WAIT_TICKS;
            }
        }
        if waiting {
            return Tick::Alive;
        }
        let hopping = token.is_some();
        if !settling {
            return Tick::Alive;
        }
        let token = token.or_else(|| self.block.take()).map(|mut block| {
            block.become_state(self.state);
            block
        });
        let end = if hopping { Tick::Alive } else { Tick::Discard };
        match outcome {
            StepOutcome::Falling => Tick::Alive,
            StepOutcome::Land(state, pos) => {
                if let Some(mut block) = token {
                    block.become_state(state);
                    context.effects.push(Effect::PlaceFallenBlock {
                        pos,
                        block,
                        data: self.block_data.clone(),
                    });
                }
                if is_anvil(state) {
                    self.level_event(context, play::level_event::SOUND_ANVIL_LAND, pos);
                }
                end
            }
            StepOutcome::Broken(pos) => {
                self.broken_after_fall(context, pos);
                if let Some(block) = token {
                    block.burn(BlockBurn::Discarded);
                }
                end
            }
            StepOutcome::Drop => {
                let drops = self.drop_item && context.shared.rule_bool("entity_drops");
                if drops {
                    self.broken_after_fall(context, self.landing_pos());
                }
                if self.base.on_ground
                    && let Some(event) =
                        tuff_vanilla_common::speleothem::landing_event(self.state.block())
                {
                    self.level_event(context, event, self.landing_pos());
                }
                if let Some(block) = token {
                    if !drops {
                        block.burn(BlockBurn::Discarded);
                    } else if let Some(stack) = block.into_broken(None).into_item() {
                        let jitter = |rng: &mut rand::rngs::StdRng| rng.gen_range(-0.1..0.1);
                        context.effects.push(Effect::DropItem {
                            stack,
                            position: self.base.position,
                            velocity: [jitter(context.rng), 0.2, jitter(context.rng)],
                            pickup_delay: BLOCK_DROP_PICKUP_DELAY,
                        });
                    }
                }
                end
            }
        }
    }
}

impl FallingBlock {
    /// Whether the block is about to settle while touching an End portal it
    /// is free to enter: the game then takes it through while the copy
    /// that stays behind finishes the tick, settling in the old dimension
    /// (see `block_ledger::duplicate_held`).
    fn settles_in_end_portal(&self, context: &TickContext) -> bool {
        if self.portal_copy_made {
            return true;
        }
        if self.base.portal_cooldown > 0 {
            return false;
        }
        let bounds = self.bounds();
        let inset = 1.0e-5;
        let min = [0, 1, 2].map(|i| bounds[i] + inset);
        let max = [0, 1, 2].map(|i| bounds[i + 3] - inset);
        let cells = |axis: usize| min[axis].floor() as i32..=(max[axis].ceil() as i32 - 1);
        cells(1).any(|y| {
            let (low, high) = (f64::from(y) + 6.0 / 16.0, f64::from(y) + 12.0 / 16.0);
            max[1] > low
                && min[1] < high
                && cells(2).any(|z| {
                    cells(0).any(|x| {
                        (context.block)(BlockPos::new(x, y, z))
                            .is_some_and(|state| state.block() == Block::EndPortal)
                    })
                })
        })
    }

    /// Hurts every living, non-creative thing in its box after a fall of
    /// `fallen` blocks onto `below`, then rolls an anvil's degrade. The
    /// block only changes the distance (a bed halves it, a stalagmite tip
    /// adds to it, powder snow swallows the landing); a damage
    /// multiplier such as hay's or slime's does not apply to a falling
    /// block, which is why it counts as never bouncing.
    fn hurt_below(&mut self, fallen: f64, below: Option<BlockState>, context: &mut TickContext) {
        let Some(landing) = tuff_core::world::fall::impact(below, fallen, true) else {
            return;
        };
        let Some((damage, blocks)) = self.landing_damage(landing.distance) else {
            return;
        };
        let bounds = self.bounds();
        let overlaps =
            |other: [f64; 6]| (0..3).all(|i| other[i] < bounds[i + 3] && other[i + 3] > bounds[i]);
        let kind = if is_anvil(self.state) {
            &tuff_vanilla_data::damage_type::FALLING_ANVIL
        } else if tuff_vanilla_common::speleothem::is_speleothem(self.state.block()) {
            &tuff_vanilla_data::damage_type::FALLING_STALACTITE
        } else {
            &tuff_vanilla_data::damage_type::FALLING_BLOCK
        };
        let hit = || {
            let mut hit = EntityHit::new(damage, kind);
            hit.position = Some(self.base.position);
            Box::new(hit)
        };
        let mut targets: Vec<i32> = context
            .players
            .iter()
            .filter(|p| !p.creative && overlaps(p.bounds()))
            .map(|p| p.id)
            .collect();
        targets.extend(
            context
                .entities
                .iter()
                .filter(|e| e.id != self.base.id)
                .filter(|e| e.living.as_ref().is_some_and(|l| l.alive) && overlaps(e.bounds))
                .map(|e| e.id),
        );
        for target in targets {
            context.effects.push(Effect::Hurt { target, hit: hit() });
        }
        let roll = context.rng.r#gen::<f32>();
        self.degrade(damage, blocks, roll);
    }

    /// What breaking apart after a fall shows: an anvil's crash, or a
    /// brushable block's break particles and its destroy game event.
    fn broken_after_fall(&self, context: &mut TickContext, pos: BlockPos) {
        if is_anvil(self.state) {
            self.level_event(context, play::level_event::SOUND_ANVIL_BROKEN, pos);
        } else if tuff_vanilla_common::brushable::is_brushable(self.state.block()) {
            let b = self.bounds();
            let center = [0, 1, 2].map(|i| (b[i] + b[i + 3]) / 2.0);
            let packet = tuff_core::world::framed(&play::LevelEvent {
                event: play::level_event::BLOCK_BREAK,
                position: BlockPos::containing(center),
                data: self.state.id() as i32,
                global: false,
            });
            context.effects.push(Effect::NearbyPackets {
                center,
                range: tuff_core::world::region_api::LEVEL_EVENT_RANGE,
                packets: tuff_core::world::packets([packet]),
            });
            context.effects.push(Effect::GameEvent {
                event: tuff_core::world::game_event::GameEvent::BlockDestroy,
                position: center,
                source: Some(self.base.id),
            });
        }
    }

    fn level_event(&self, context: &mut TickContext, event: i32, pos: BlockPos) {
        if self.base.silent {
            return;
        }
        let packet = tuff_core::world::framed(&play::LevelEvent {
            event,
            position: pos,
            data: 0,
            global: false,
        });
        context.effects.push(Effect::NearbyPackets {
            center: pos.center(),
            range: tuff_core::world::region_api::LEVEL_EVENT_RANGE,
            packets: tuff_core::world::packets([packet]),
        });
    }
}

#[cfg(test)]
mod tests {
    use tuff_vanilla_data::Block;

    use super::*;

    impl Drop for FallingBlock {
        fn drop(&mut self) {
            if let Some(block) = self.block.take() {
                block.burn(BlockBurn::Discarded);
            }
        }
    }

    fn new_falling(id: i32, uuid: u128, pos: BlockPos, state: BlockState) -> FallingBlock {
        FallingBlock::new(id, uuid, pos, HeldBlock::test(state))
    }

    /// A flat stone floor at `y`, air above, for every column.
    fn flat_floor(y: i32) -> impl Fn(BlockPos) -> Option<BlockState> {
        move |pos| {
            Some(if pos.y < y {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn landing_replaces_one_snow_layer_but_not_thicker() {
        let pos = BlockPos::new(0, 64, 0);
        let snow = Block::Snow.default_state();
        assert!(may_replace_landing(snow, pos));
        let thick = snow.with_layers(3).unwrap();
        assert!(!may_replace_landing(thick, pos));
        assert!(may_replace_landing(Block::Air.default_state(), pos));
        assert!(!may_replace_landing(Block::Stone.default_state(), pos));
    }

    fn falls_to_ground(state: BlockState) -> Option<(BlockState, BlockPos)> {
        let mut block = new_falling(1, 1, BlockPos::new(0, 70, 0), state);
        let level = flat_floor(64);
        for _ in 0..200 {
            match block.step_tick(&level, |_, _, _| {}) {
                StepOutcome::Falling => {}
                StepOutcome::Land(state, pos) => return Some((state, pos)),
                StepOutcome::Drop | StepOutcome::Broken(_) => return None,
            }
        }
        panic!("never settled");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_falling_block_is_held_up_by_an_upward_bubble_column() {
        let column = Block::BubbleColumn
            .default_state()
            .with("drag", "false")
            .unwrap();
        let level = move |pos: BlockPos| {
            Some(if pos.y < 64 {
                Block::Stone.default_state()
            } else {
                column
            })
        };
        let mut block = new_falling(1, 1, BlockPos::new(0, 70, 0), Block::Sand.default_state());
        for _ in 0..40 {
            assert!(matches!(
                block.step_tick(level, |_, _, _| {}),
                StepOutcome::Falling
            ));
        }
        assert!(
            block.base.position[1] > 70.0,
            "rose to {}",
            block.base.position[1]
        );
        assert_eq!(block.base.fall_distance, 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_falling_block_overlapping_frogspawn_finds_it() {
        let block = new_falling(1, 1, BlockPos::new(0, 64, 0), Block::Sand.default_state());
        let spawn = Block::Frogspawn.default_state();
        let level = |pos: BlockPos| {
            Some(if pos == BlockPos::new(0, 64, 0) {
                spawn
            } else {
                Block::Air.default_state()
            })
        };
        assert_eq!(
            block.frogspawn_inside(&level),
            vec![BlockPos::new(0, 64, 0)]
        );
        let empty = |_: BlockPos| Some(Block::Air.default_state());
        assert!(block.frogspawn_inside(&empty).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_falling_block_lands_on_solid_ground() {
        let (state, pos) = falls_to_ground(Block::Gravel.default_state()).expect("gravel lands");
        assert_eq!(state.block(), Block::Gravel);
        assert_eq!(pos, BlockPos::new(0, 64, 0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_falling_block_that_cannot_be_replaced_drops_instead_of_landing() {
        // The entity is resting on top of solid ground (below is stone, not
        // free), but something already occupies the exact cell it would
        // place into (e.g. a block placed there while it was falling).
        let mut block = new_falling(1, 1, BlockPos::new(0, 64, 0), Block::Sand.default_state());
        block.base.on_ground = true;
        let level = |pos: BlockPos| {
            Some(
                if pos == BlockPos::new(0, 64, 0) || pos == BlockPos::new(0, 63, 0) {
                    Block::Stone.default_state()
                } else {
                    Block::Air.default_state()
                },
            )
        };
        assert!(matches!(block.try_land(&level), StepOutcome::Drop));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_falling_block_lands_in_lava_and_replaces_it() {
        // Liquids are replaceable, so a block falling into a lava pool with
        // solid ground beneath it settles into the lava's spot.
        let mut block = new_falling(1, 1, BlockPos::new(0, 64, 0), Block::Sand.default_state());
        block.base.on_ground = true;
        let level = |pos: BlockPos| {
            Some(if pos == BlockPos::new(0, 64, 0) {
                Block::Lava.default_state()
            } else if pos == BlockPos::new(0, 63, 0) {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        assert!(matches!(block.try_land(&level), StepOutcome::Land(_, _)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_falling_block_does_not_settle_where_it_would_only_keep_falling() {
        // If the block below the landing spot is also free, the landing
        // spot itself is not a stable resting place, so it drops rather
        // than placing a block that would immediately need to fall again.
        let mut block = new_falling(1, 1, BlockPos::new(0, 64, 0), Block::Sand.default_state());
        block.base.on_ground = true;
        let level = |_: BlockPos| Some(Block::Air.default_state());
        assert!(matches!(block.try_land(&level), StepOutcome::Drop));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_falling_concrete_powder_solidifies_when_it_lands_in_water() {
        let mut block = new_falling(
            1,
            1,
            BlockPos::new(0, 64, 0),
            Block::WhiteConcretePowder.default_state(),
        );
        block.base.on_ground = true;
        let level = |pos: BlockPos| {
            Some(if pos == BlockPos::new(0, 64, 0) {
                Block::Water.default_state()
            } else if pos == BlockPos::new(0, 63, 0) {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        match block.try_land(&level) {
            StepOutcome::Land(state, pos) => {
                assert_eq!(state.block(), Block::WhiteConcrete);
                assert_eq!(pos, BlockPos::new(0, 64, 0));
            }
            _ => panic!("expected the powder to land and solidify"),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_falling_concrete_powder_solidifies_when_it_lands_touching_water() {
        let mut block = new_falling(
            1,
            1,
            BlockPos::new(0, 64, 0),
            Block::WhiteConcretePowder.default_state(),
        );
        block.base.on_ground = true;
        // The landing cell itself is air; water is only beside it.
        let level = |pos: BlockPos| {
            Some(if pos == BlockPos::new(1, 64, 0) {
                Block::Water.default_state()
            } else if pos == BlockPos::new(0, 63, 0) {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        match block.try_land(&level) {
            StepOutcome::Land(state, _) => assert_eq!(state.block(), Block::WhiteConcrete),
            _ => panic!("expected the powder to land and solidify"),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_falling_concrete_powder_away_from_water_lands_as_powder() {
        let mut block = new_falling(
            1,
            1,
            BlockPos::new(0, 64, 0),
            Block::WhiteConcretePowder.default_state(),
        );
        block.base.on_ground = true;
        let level = flat_floor(64);
        match block.try_land(&level) {
            StepOutcome::Land(state, _) => assert_eq!(state.block(), Block::WhiteConcretePowder),
            _ => panic!("expected the powder to land as powder"),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_block_below_the_world_for_over_a_hundred_ticks_is_out_of_world() {
        let mut block = new_falling(1, 1, BlockPos::new(0, -70, 0), Block::Sand.default_state());
        assert!(!block.out_of_world(-64, 319));
        block.time = 101;
        assert!(block.out_of_world(-64, 319));
        block.base.position[1] = 70.0;
        assert!(!block.out_of_world(-64, 319));
    }

    #[tuff_core::watchdog]
    #[test]
    fn drop_item_is_saved_and_defaults_to_true() {
        let mut block = new_falling(1, 1, BlockPos::new(0, 70, 0), Block::Sand.default_state());
        assert!(block.drop_item);
        block.drop_item = false;
        let mut out = Compound::new();
        block.save(&mut out);
        assert_eq!(take_bool(&mut out, "DropItem"), Some(false));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_stalled_falling_block_eventually_gives_up() {
        let mut block = new_falling(1, 1, BlockPos::new(0, 70, 0), Block::Sand.default_state());
        block.base.no_gravity = true;
        let level = |_: BlockPos| Some(Block::Air.default_state());
        let mut outcome = StepOutcome::Falling;
        for _ in 0..=MAX_LIFETIME {
            outcome = block.step_tick(level, |_, _, _| {});
        }
        assert!(matches!(outcome, StepOutcome::Drop));
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_anvil_sees_the_block_it_lands_on_and_a_bed_halves_its_fall() {
        let mut anvil = new_falling(1, 1, BlockPos::new(0, 75, 0), Block::Anvil.default_state());
        let level = |pos: BlockPos| {
            Some(if pos.y < 64 {
                Block::RedBed.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let mut landed = None;
        for _ in 0..200 {
            if !matches!(
                anvil.step_tick(level, |_, fallen, below| landed = Some((fallen, below))),
                StepOutcome::Falling
            ) {
                break;
            }
        }
        let (fallen, below) = landed.expect("it lands");
        assert_eq!(below.map(|s| s.block()), Some(Block::RedBed));
        let halved = tuff_core::world::fall::impact(below, fallen, true).unwrap();
        assert_eq!(halved.distance, fallen * 0.5);
        let full = anvil.landing_damage(fallen).unwrap().0;
        assert!(anvil.landing_damage(halved.distance).unwrap().0 < full);
        let snow = Some(Block::PowderSnow.default_state());
        assert!(tuff_core::world::fall::impact(snow, fallen, true).is_none());
        let slime = Some(Block::SlimeBlock.default_state());
        assert_eq!(
            tuff_core::world::fall::impact(slime, fallen, true)
                .unwrap()
                .distance,
            fallen
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_anvils_hurt_what_they_land_on() {
        let anvil = new_falling(1, 1, BlockPos::new(0, 70, 0), Block::Anvil.default_state());
        let sand = new_falling(1, 1, BlockPos::new(0, 70, 0), Block::Sand.default_state());
        assert_eq!(anvil.landing_damage(5.0), Some((8.0, 4)));
        assert_eq!(sand.landing_damage(5.0), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn anvil_damage_is_two_per_block_fallen_capped_at_forty() {
        assert_eq!(impact_damage(0.5, 2.0, 40), Some((0.0, 0)));
        assert_eq!(impact_damage(1.0, 2.0, 40), Some((0.0, 0)));
        assert_eq!(impact_damage(1.2, 2.0, 40), Some((2.0, 1)));
        assert_eq!(impact_damage(6.0, 2.0, 40), Some((10.0, 5)));
        assert_eq!(impact_damage(100.0, 2.0, 40), Some((40.0, 99)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn anvils_degrade_step_by_step_keeping_their_facing_then_break() {
        let facing = |s: BlockState| s.get("facing");
        let start = Block::Anvil.default_state().with("facing", "east").unwrap();
        let mut anvil = new_falling(1, 1, BlockPos::new(0, 70, 0), start);
        anvil.degrade(4.0, 2, 0.14);
        assert_eq!(anvil.state.block(), Block::ChippedAnvil);
        assert_eq!(facing(anvil.state), Some("east"));
        anvil.degrade(4.0, 2, 0.15);
        assert_eq!(
            anvil.state.block(),
            Block::ChippedAnvil,
            "roll above the chance"
        );
        anvil.degrade(0.0, 2, 0.0);
        assert_eq!(
            anvil.state.block(),
            Block::ChippedAnvil,
            "no damage, no degrade"
        );
        anvil.degrade(4.0, 2, 0.0);
        assert_eq!(anvil.state.block(), Block::DamagedAnvil);
        assert!(!anvil.cancel_drop);
        anvil.degrade(4.0, 2, 0.0);
        assert!(anvil.cancel_drop, "a damaged anvil breaks apart");
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_broken_anvil_vanishes_on_landing() {
        let mut anvil = new_falling(1, 1, BlockPos::new(0, 64, 0), Block::Anvil.default_state());
        anvil.base.on_ground = true;
        anvil.cancel_drop = true;
        assert!(matches!(
            anvil.try_land(&flat_floor(64)),
            StepOutcome::Broken(pos) if pos == BlockPos::new(0, 64, 0)
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_impact_sees_the_whole_fall_distance() {
        let mut anvil = new_falling(1, 1, BlockPos::new(0, 70, 0), Block::Anvil.default_state());
        let level = flat_floor(64);
        let mut fallen = None;
        for _ in 0..200 {
            let outcome = anvil.step_tick(&level, |_, d, _| fallen = Some(d));
            if !matches!(outcome, StepOutcome::Falling) {
                break;
            }
        }
        assert!((fallen.expect("it hit the ground") - 6.0).abs() < 1e-9);
    }

    #[tuff_core::watchdog]
    #[test]
    fn anvil_hurt_settings_round_trip_and_default_like_the_game() {
        let mut nbt = Compound::new().with("BlockState", "minecraft:anvil");
        let base = EntityBase::new(EntityType::FallingBlock, 1, 1, [0.0; 3], [0.0; 3]);
        let loaded = load(base.clone(), &mut nbt).unwrap();
        let mut out = Compound::new();
        loaded.save(&mut out);
        assert_eq!(out.get("HurtEntities"), Some(&Tag::Byte(1)));
        assert_eq!(out.get("FallHurtAmount"), Some(&Tag::Float(0.0)));
        assert_eq!(out.get("FallHurtMax"), Some(&Tag::Int(40)));
        let fresh = new_falling(1, 1, BlockPos::new(0, 0, 0), Block::Anvil.default_state());
        let mut out = Compound::new();
        fresh.save(&mut out);
        out.insert("BlockState", "minecraft:anvil");
        let reloaded = load(base, &mut out).unwrap();
        let mut again = Compound::new();
        reloaded.save(&mut again);
        assert_eq!(again.get("FallHurtAmount"), Some(&Tag::Float(2.0)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_saved_falling_block_loads_holding_its_block_and_a_kill_takes_it() {
        let block = new_falling(1, 1, BlockPos::new(0, 70, 0), Block::Gravel.default_state());
        let mut out = Compound::new();
        block.save(&mut out);
        let base = EntityBase::new(EntityType::FallingBlock, 2, 2, [0.0; 3], [0.0; 3]);
        let mut loaded = load(base, &mut out).unwrap();
        let held = loaded
            .take_held()
            .expect("the loaded block holds its block");
        assert_eq!(held.state(), Block::Gravel.default_state());
        assert!(loaded.take_held().is_none());
        held.burn(BlockBurn::Killed);
    }
}
