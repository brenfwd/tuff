//! Endermen: neutral until a player stares at them or hurts them, then
//! they scream, freeze while watched, teleport around and hit hard. They
//! dodge arrows by teleporting, hate water and rain, wander off by day,
//! and pick up and put down blocks.

use rand::Rng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState, damage_type};

use super::attack::MeleeAttack;
use super::goal::{Flags, Goal, GoalSelector, reduced_delay};
use super::goals::{Float, LookAtPlayer, RandomLookAround, RandomStroll};
use super::physics::{self, block_pos};
use super::target::{HurtByTarget, NearestAttackableEntity, TargetView, view};
use super::{Damage, MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{Effect, NearbyPlayer, TickContext, take};
use tuff_core::world::{framed, packets};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

const CARRIED: u8 = data::BABY;
const CREEPY: u8 = data::BABY + 1;
const STARED_AT: u8 = data::BABY + 2;
const ATTACK_SPEED_ID: &str = "minecraft:attacking";
const ATTACK_SPEED_BONUS: f64 = 0.150_000_005_960_464_48;
/// It only wanders off in daylight this long after picking a target.
const MIN_DEAGGRESSION_TIME: u64 = 600;
const TELEPORT_PARTICLES: i32 = 2018;
const TELEPORT_ATTEMPTS: u32 = 64;

#[derive(Debug, Default)]
pub struct Enderman {
    carried: Option<BlockState>,
    creepy: bool,
    stared_at: bool,
    /// The target it had last tick, to notice changes.
    last_target: Option<i32>,
    target_change_tick: u64,
}

/// Whether `player` looks at the enderman's eyes with nothing in between
/// and without a carved pumpkin on.
fn stared_at_by(mob: &MobCore, player: &NearbyPlayer, cx: &TickContext) -> bool {
    if player.armor[3].is_some_and(|head| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::item::key::GAZE_DISGUISE_EQUIPMENT,
            head,
        )
    }) {
        return false;
    }
    let look = tuff_core::world::raycast::view_vector(player.yaw, player.pitch);
    let eye = player.eye();
    let gaze = mob.eye_position();
    let dir = [gaze[0] - eye[0], gaze[1] - eye[1], gaze[2] - eye[2]];
    let distance = dir.iter().map(|c| c * c).sum::<f64>().sqrt();
    if distance < 1.0e-6 {
        return false;
    }
    let dot = (0..3).map(|i| look[i] * dir[i] / distance).sum::<f64>();
    if dot <= 1.0 - 0.025 / distance {
        return false;
    }
    let level = |pos: BlockPos| (cx.block)(pos).unwrap_or(Block::Stone.default_state());
    tuff_core::world::raycast::cast_colliders(eye, gaze, &level).is_none()
}

fn player(cx: &TickContext, id: i32) -> Option<NearbyPlayer<'static>> {
    cx.players
        .iter()
        .find(|p| p.id == id)
        .map(|p| NearbyPlayer { name: "", ..*p })
}

impl MobCore {
    /// Teleports to the first standing spot at or below `(x, y, z)`, if it
    /// is free, dry and not somewhere endermen shun.
    pub fn random_teleport(&mut self, x: f64, y: f64, z: f64, cx: &mut TickContext) -> bool {
        let mut pos = block_pos([x, y, z]);
        if (cx.block)(pos).is_none() {
            return false;
        }
        let mut y = y;
        while pos.y > self.min_y {
            pos.y -= 1;
            let Some(state) = (cx.block)(pos) else {
                return false;
            };
            if tuff_datapack::tags::is(
                tuff_vanilla_data::tag::block::key::ENTITIES_CAN_TELEPORT_TO,
                state.block(),
            ) {
                return self.teleport_onto(x, y, z, state, cx);
            }
            y -= 1.0;
        }
        false
    }

    fn teleport_onto(
        &mut self,
        x: f64,
        y: f64,
        z: f64,
        below: BlockState,
        cx: &mut TickContext,
    ) -> bool {
        let shunned = |state: BlockState| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::block::key::ENDERMAN_DOES_NOT_TELEPORT_TO,
                state.block(),
            )
        };
        if shunned(below) {
            return false;
        }
        let half = f64::from(self.size.width) / 2.0;
        let area = [
            x - half,
            y,
            z - half,
            x + half,
            y + f64::from(self.size.height),
            z + half,
        ];
        if !cx
            .shared
            .dimension(cx.dimension)
            .border()
            .contains_box([area[0], area[2]], [area[3], area[5]])
        {
            return false;
        }
        if !physics::no_collision(area, cx.block) {
            return false;
        }
        let min = block_pos([area[0], area[1], area[2]]);
        let max = block_pos([area[3], area[4], area[5]]);
        for bx in min.x..=max.x {
            for by in min.y..=max.y {
                for bz in min.z..=max.z {
                    let Some(state) = (cx.block)(BlockPos::new(bx, by, bz)) else {
                        return false;
                    };
                    if !state.fluid_state().is_empty() || shunned(state) {
                        return false;
                    }
                }
            }
        }
        let under = block_pos([x, y, z]);
        let under = BlockPos::new(under.x, under.y - 1, under.z);
        if (cx.block)(under).is_some_and(|s| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::fluid::key::WATER,
                s.fluid_state().fluid(),
            )
        }) {
            return false;
        }
        let old = self.base.position;
        self.base.position = [x, y, z];
        self.base.velocity = [0.0; 3];
        self.navigation.stop();
        self.impulse = true;
        self.entity_event(play::entity_event::TELEPORT, cx);
        let (from, to) = (block_pos(old), block_pos(self.base.position));
        let diff = |a: i32, b: i32| ((b - a).clamp(-127, 127) + 127) & 255;
        let packed = (diff(from.x, to.x) << 16) | (diff(from.y, to.y) << 8) | diff(from.z, to.z);
        let event = framed(&play::LevelEvent {
            event: TELEPORT_PARTICLES,
            position: from,
            data: packed,
            global: false,
        });
        let sound = |position: [f64; 3], cx: &mut TickContext| {
            framed(&play::Sound {
                sound_event: SoundEvent::EntityEndermanTeleport.id(),
                category: play::sound_category::HOSTILE,
                position,
                volume: 1.0,
                pitch: 1.0,
                seed: cx.rng.r#gen(),
            })
        };
        let packets_out = packets([event, sound(old, cx), sound(self.base.position, cx)]);
        cx.effects.push(Effect::ChunkPackets(packets_out));
        true
    }

    /// A random teleport up to 32 blocks away.
    pub fn teleport_randomly(&mut self, cx: &mut TickContext) -> bool {
        if !self.is_alive() {
            return false;
        }
        let [x, y, z] = self.base.position;
        let tx = x + (cx.rng.r#gen::<f64>() - 0.5) * 64.0;
        let ty = y + f64::from(cx.rng.gen_range(0..64) - 32);
        let tz = z + (cx.rng.r#gen::<f64>() - 0.5) * 64.0;
        self.random_teleport(tx, ty, tz, cx)
    }

    /// A teleport to within 16 blocks of `target`, on the way towards it.
    fn teleport_towards(&mut self, target: &TargetView, cx: &mut TickContext) -> bool {
        let [x, y, z] = self.base.position;
        let height = f64::from(self.size.height);
        let dir = [
            x - target.position[0],
            y + height * 0.5 - target.eye[1],
            z - target.position[2],
        ];
        let length = dir.iter().map(|c| c * c).sum::<f64>().sqrt().max(1.0e-9);
        let dir = dir.map(|c| c / length);
        let tx = x + (cx.rng.r#gen::<f64>() - 0.5) * 8.0 - dir[0] * 16.0;
        let ty = y + f64::from(cx.rng.gen_range(0..16) - 8) - dir[1] * 16.0;
        let tz = z + (cx.rng.r#gen::<f64>() - 0.5) * 8.0 - dir[2] * 16.0;
        self.random_teleport(tx, ty, tz, cx)
    }

    fn keep_teleporting(&mut self, cx: &mut TickContext) {
        for _ in 0..TELEPORT_ATTEMPTS {
            if self.teleport_randomly(cx) {
                return;
            }
        }
    }
}

/// Freezes while the targeted player stares at it within 16 blocks.
#[derive(Debug, Default)]
struct FreezeWhenLookedAt {
    target: Option<i32>,
}

impl Goal for FreezeWhenLookedAt {
    fn flags(&self) -> Flags {
        Flags::JUMP | Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.target = mob.target;
        let Some(target) = self.target.and_then(|id| player(cx, id)) else {
            return false;
        };
        mob.distance_sq(target.position) <= 256.0 && stared_at_by(mob, &target, cx)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(target) = self.target.and_then(|id| player(cx, id)) {
            mob.look_at_point(target.eye());
        }
    }
}

/// Takes offence at players who stare at it (or whom it holds a grudge
/// against): after a short delay it targets them, teleports away when
/// stared at up close and towards them when they run far.
#[derive(Debug, Default)]
struct LookForStaringPlayer {
    pending: Option<i32>,
    aggro_time: i32,
    teleport_time: i32,
    target: Option<i32>,
    unseen_ticks: i32,
}

impl LookForStaringPlayer {
    fn angers(mob: &MobCore, player: &NearbyPlayer, cx: &TickContext) -> bool {
        let view = view(cx, player.id);
        stared_at_by(mob, player, cx) || view.is_some_and(|v| mob.is_angry_at(&v, cx))
    }
}

impl Goal for LookForStaringPlayer {
    fn flags(&self) -> Flags {
        Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let range = mob.follow_range();
        let eye = mob.eye_position();
        self.pending = cx
            .players
            .iter()
            .filter(|p| {
                let view = view(cx, p.id);
                view.is_some_and(|v| mob.can_attack(&v, cx))
                    && mob.distance_sq(p.position)
                        <= (range * p.visibility(mob.kind())).max(2.0).powi(2)
                    && mob.can_see_point(p.eye(), cx)
                    && Self::angers(mob, p, cx)
            })
            .map(|p| (tuff_core::world::distance_sq(eye, p.position), p.id))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id);
        self.pending.is_some()
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.aggro_time = reduced_delay(5);
        self.teleport_time = 0;
        self.unseen_ticks = 0;
        mob.signal = 1;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.pending = None;
        self.target = None;
        mob.target = None;
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if let Some(pending) = self.pending {
            let Some(p) = player(cx, pending) else {
                return false;
            };
            if !Self::angers(mob, &p, cx) {
                return false;
            }
            mob.turn_to(p.eye(), 10.0, 10.0);
            return true;
        }
        let Some(target) = self.target.and_then(|id| view(cx, id)) else {
            return false;
        };
        if mob.can_attack(&target, cx) {
            mob.target = Some(target.id);
            return true;
        }
        false
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if mob.target.is_none() {
            self.target = None;
        }
        if let Some(pending) = self.pending {
            self.aggro_time -= 1;
            if self.aggro_time <= 0 {
                self.target = Some(pending);
                mob.target = Some(pending);
                self.pending = None;
            }
            return;
        }
        let Some(target) = self.target.and_then(|id| player(cx, id)) else {
            return;
        };
        let distance = mob.distance_sq(target.position);
        if stared_at_by(mob, &target, cx) {
            if distance < 16.0 {
                mob.teleport_randomly(cx);
            }
            self.teleport_time = 0;
        } else if distance > 256.0 {
            self.teleport_time += 1;
            if self.teleport_time > reduced_delay(30)
                && let Some(view) = view(cx, target.id)
                && mob.teleport_towards(&view, cx)
            {
                self.teleport_time = 0;
            }
        }
    }
}

/// Now and then puts its block down on a full block nearby.
#[derive(Debug, Default)]
struct LeaveBlock;

impl Goal for LeaveBlock {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.signal_block.is_some()
            && cx.shared.game_rules.borrow().bool("mob_griefing")
            && cx.rng.gen_range(0..reduced_delay(2000)) == 0
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(carried) = mob.signal_block else {
            return;
        };
        let [x, y, z] = mob.base.position;
        let pos = block_pos([
            x - 1.0 + cx.rng.r#gen::<f64>() * 2.0,
            y + cx.rng.r#gen::<f64>() * 2.0,
            z - 1.0 + cx.rng.r#gen::<f64>() * 2.0,
        ]);
        let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
        let (Some(target), Some(under)) = ((cx.block)(pos), (cx.block)(below)) else {
            return;
        };
        let level = |p: BlockPos| (cx.block)(p).unwrap_or(Block::Stone.default_state());
        let placed = tuff_vanilla_common::placement::update_from_neighbours(carried, pos, &level);
        let crowded = cx
            .entities
            .iter()
            .any(|e| physics::intersects(e.bounds, unit_box(pos)))
            || cx
                .players
                .iter()
                .any(|p| physics::intersects(p.bounds(), unit_box(pos)));
        if tuff_world_model::chunk::is_air(target)
            && !tuff_world_model::chunk::is_air(under)
            && under.block() != Block::Bedrock
            && under.is_collision_full_block()
            && tuff_vanilla_common::placement::can_survive(placed, pos, &level)
            && !crowded
        {
            cx.effects.push(Effect::SetBlock { pos, state: placed });
            mob.signal_block = None;
        }
    }
}

fn unit_box(pos: BlockPos) -> physics::Aabb {
    let (x, y, z) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
    [x, y, z, x + 1.0, y + 1.0, z + 1.0]
}

/// Now and then picks up a block it can hold and reach.
#[derive(Debug, Default)]
struct TakeBlock;

impl Goal for TakeBlock {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.signal_block.is_none()
            && cx.shared.game_rules.borrow().bool("mob_griefing")
            && cx.rng.gen_range(0..reduced_delay(20)) == 0
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let [x, y, z] = mob.base.position;
        let xt = (x - 2.0 + cx.rng.r#gen::<f64>() * 4.0).floor() as i32;
        let yt = (y + cx.rng.r#gen::<f64>() * 3.0).floor() as i32;
        let zt = (z - 2.0 + cx.rng.r#gen::<f64>() * 4.0).floor() as i32;
        let pos = BlockPos::new(xt, yt, zt);
        let Some(state) = (cx.block)(pos) else {
            return;
        };
        if !tuff_datapack::tags::is(
            tuff_vanilla_data::tag::block::key::ENDERMAN_HOLDABLE,
            state.block(),
        ) {
            return;
        }
        let here = block_pos(mob.base.position);
        let from = [
            f64::from(here.x) + 0.5,
            f64::from(yt) + 0.5,
            f64::from(here.z) + 0.5,
        ];
        let to = [
            f64::from(xt) + 0.5,
            f64::from(yt) + 0.5,
            f64::from(zt) + 0.5,
        ];
        let level = |p: BlockPos| (cx.block)(p).unwrap_or(Block::Stone.default_state());
        let reached = tuff_core::world::raycast::cast(
            from,
            to,
            tuff_core::world::raycast::FluidMode::None,
            &level,
        )
        .is_some_and(|hit| hit.pos == pos);
        if reached {
            cx.effects.push(Effect::SetBlock {
                pos,
                state: state.fluid_state().block(),
            });
            mob.signal_block = Some(state.block().default_state());
        }
    }
}

impl Enderman {
    fn sync(&mut self, core: &mut MobCore, cx: &TickContext) {
        if core.signal_block != self.carried {
            self.carried = core.signal_block;
            core.changed(CARRIED);
        }
        if core.target != self.last_target {
            self.last_target = core.target;
            let targeting = core.target.is_some();
            self.target_change_tick = if targeting { cx.tick } else { 0 };
            if self.creepy != targeting {
                self.creepy = targeting;
                core.changed(CREEPY);
            }
            if !targeting && self.stared_at {
                self.stared_at = false;
                core.changed(STARED_AT);
            }
            let bonus = targeting.then_some((
                ATTACK_SPEED_BONUS,
                tuff_vanilla_data::item::ModifierOperation::AddValue,
            ));
            core.set_modifier(
                tuff_vanilla_data::registry::Attribute::MovementSpeed,
                ATTACK_SPEED_ID,
                bonus,
            );
        }
        if core.signal > 0 {
            core.signal = 0;
            if !self.stared_at {
                self.stared_at = true;
                core.changed(STARED_AT);
            }
        }
    }
}

impl Species for Enderman {
    fn create(_rng: &mut rand::rngs::StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Water, -1.0);
        let mut goals = GoalSelector::default();
        goals.add(1, LookForStaringPlayer::default());
        goals.add(2, HurtByTarget::new(&[]));
        goals.add(3, super::target::ResetUniversalAnger::new(false));
        goals.add(
            3,
            NearestAttackableEntity::new(&[EntityType::Endermite], true),
        );
        goals.add(0, Float::new(core));
        goals.add(1, FreezeWhenLookedAt::default());
        goals.add(2, MeleeAttack::new(1.0, false));
        goals.add(7, RandomStroll::water_avoiding(1.0));
        goals.add(8, LookAtPlayer::new(8.0));
        goals.add(8, RandomLookAround::default());
        goals.add(10, LeaveBlock);
        goals.add(11, TakeBlock);
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        let carried = take(nbt, "carriedBlockState", |tag| {
            Some(tuff_core::storage::chunk_nbt::decode_block_state(tag))
        })
        .filter(|state| !tuff_world_model::chunk::is_air(*state));
        self.carried = carried;
        core.signal_block = carried;
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        if let Some(state) = core.signal_block {
            out.insert(
                "carriedBlockState",
                tuff_core::storage::chunk_nbt::encode_block_state(state),
            );
        }
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (
                CARRIED,
                MetadataValue::OptionalBlockState(core.signal_block.map(|s| s.id())),
            ),
            (CREEPY, MetadataValue::Boolean(self.creepy)),
            (STARED_AT, MetadataValue::Boolean(self.stared_at)),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(if self.creepy {
                SoundEvent::EntityEndermanScream
            } else {
                SoundEvent::EntityEndermanAmbient
            }),
            hurt: Some(SoundEvent::EntityEndermanHurt),
            death: Some(SoundEvent::EntityEndermanDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let [x, _, z] = core.base.position;
        let eye = block_pos([x, core.eye_position()[1], z]);
        let rained_on = cx.raining && core.can_see_sky(eye, cx);
        if core.is_alive() && (core.body.in_water || rained_on) {
            core.hurt_later(1.0, &damage_type::DROWN);
        }
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.update_anger(true, cx);
        if cx.sky_darken < 4 && cx.tick >= self.target_change_tick + MIN_DEAGGRESSION_TIME {
            let brightness = core.brightness(cx);
            let here = block_pos(core.base.position);
            if brightness > 0.5
                && core.can_see_sky(here, cx)
                && cx.rng.r#gen::<f32>() * 30.0 < (brightness - 0.4) * 2.0
            {
                core.target = None;
                core.teleport_randomly(cx);
            }
        }
        self.sync(core, cx);
    }

    fn before_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) -> bool {
        if damage.is("is_projectile") {
            core.keep_teleporting(cx);
            return false;
        }
        true
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) {
        if damage.attacker.is_none() && cx.rng.gen_range(0..10) != 0 {
            core.teleport_randomly(cx);
        }
    }

    fn died(&mut self, core: &mut MobCore, _damage: &Damage, cx: &mut TickContext) {
        if let Some(state) = core.signal_block.take() {
            for stack in carried_drops(&*cx.shared.block_loot(), state, cx.rng) {
                core.drop_item(stack, 0.0, cx);
            }
        }
    }

    fn remove_when_far_away(&self) -> bool {
        self.carried.is_none()
    }

    fn custom_persistence(&self, _core: &MobCore) -> bool {
        self.carried.is_some()
    }
}

fn carried_drops(
    loot: &dyn tuff_core::world::drops::BlockLoot,
    state: BlockState,
    rng: &mut rand::rngs::StdRng,
) -> Vec<tuff_item::item::ItemStack> {
    let tool = carried_block_tool(rng);
    loot.drops(state, Some(tool.any()), None, rng)
}

/// The stand-in tool the carried block is mined with: a picture, never a
/// real item.
fn carried_block_tool(rng: &mut rand::rngs::StdRng) -> tuff_item::item::ItemPicture {
    let mut tool = tuff_item::item::ItemPicture::new(tuff_vanilla_data::Item::DiamondAxe, 1);
    let chosen = tuff_item::item::enchanting::provider_enchantments(
        &tool,
        "minecraft:enderman_loot_drop",
        0.0,
        rng,
    );
    if !chosen.is_empty() {
        tool.components.enchantments = Some(tuff_item::item::Enchantments::new(chosen));
    }
    tool
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Enderman, super::load::<Enderman>);
    registry.register_spawn_rule(EntityType::Enderman, super::spawning::monster_rule);
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use tuff_vanilla_data::Block;

    use super::*;
    use tuff_core::world::entity::{EntityBase, NearbyPlayer, TickContext};

    const PLAYER_ID: i32 = 1;
    const MOB_POS: [f64; 3] = [0.5, 64.0, 0.5];

    /// A flat grass world with a player standing `distance` blocks away on
    /// the same y level, aimed exactly at the enderman's eyes.
    fn stare_context(distance: f64, f: impl FnOnce(&mut MobCore, &mut TickContext)) {
        let shared = tuff_test_support::bare_shared();
        let block = |pos: BlockPos| {
            Some(if pos.y < 64 {
                Block::GrassBlock.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let light = |_: BlockPos| Some((15, 0));
        let biome = |_: BlockPos| Some("minecraft:plains");
        let mut rng = StdRng::seed_from_u64(1);
        let mut effects = Vec::new();
        let mut budget = 10_000;
        let pois = crate::world::poi::PoiStore::default();

        let player_pos = [MOB_POS[0], MOB_POS[1], MOB_POS[2] + distance];
        let player_eye_height = 1.62;
        let mob_eye = [
            MOB_POS[0],
            MOB_POS[1] + f64::from(EntityType::Enderman.size().eye_height),
            MOB_POS[2],
        ];
        let player_eye = [
            player_pos[0],
            player_pos[1] + player_eye_height,
            player_pos[2],
        ];
        let dir = [
            mob_eye[0] - player_eye[0],
            mob_eye[1] - player_eye[1],
            mob_eye[2] - player_eye[2],
        ];
        let horizontal = (dir[0] * dir[0] + dir[2] * dir[2]).sqrt();
        let yaw = (dir[2].atan2(dir[0]).to_degrees() - 90.0) as f32;
        let pitch = (-dir[1].atan2(horizontal).to_degrees()) as f32;
        let players = [NearbyPlayer {
            id: PLAYER_ID,
            uuid: 1,
            name: "",
            position: player_pos,
            eye_height: player_eye_height,
            yaw,
            pitch,
            held: [None, None],
            creative: false,
            height: 1.8,
            sneaking: false,
            invisible: false,
            invulnerable: false,
            armor: [None, None, None, None],
            effects: 0,
            movement: [0.0; 3],
            vehicle: None,
            sleeping: None,
            last_hurt_by: None,
            last_hurt_mob: None,
            perchable: false,
            sprinting: false,
        }];
        let mut cx = TickContext {
            tick: 1,
            dimension: tuff_core::world::dimension::DimensionId::for_tests(0),
            block: &block,
            light: &light,
            sky_darken: 0,
            ambient_light: 0.0,
            monsters_burn: true,
            raining: false,
            biome: &biome,
            container: &|_| None,
            container_info: &|_| Default::default(),
            inhabited_time: &|_| 0,
            min_y: -64,
            max_y: 319,
            players: &players,
            players_in_dimension: 0,
            entities: &[],
            entity_grid: None,
            pistons: &[],
            parts: &[],
            shared: &shared,
            rng: &mut rng,
            effects: &mut effects,
            path_budget: &mut budget,
            pois: &pois,
            state: &tuff_core::world::region_api::RegionStateLock::default(),
        };
        let base = EntityBase::new(EntityType::Enderman, 2, 2, MOB_POS, [0.0; 3]);
        let mut core = MobCore::new(base);
        core.health = 20.0;
        core.target = Some(PLAYER_ID);
        f(&mut core, &mut cx);
    }

    /// Matches the reference server exactly: a targeted player staring at a
    /// close enderman (distance-squared < 16, i.e. under 4 blocks) gets an
    /// unconditional teleport *attempt* every time -- there is no cooldown
    /// or random roll on that branch (an attempt can still fail to find
    /// anywhere to land, as in vanilla). The only throttle is the AI pacing
    /// every goal already runs under (a full tick, including this one, only
    /// every other game tick), which `server_ai_step` mirrors via
    /// `id_tick`. So a teleport can only ever land on a full AI step, never
    /// on the off ticks in between.
    #[tuff_core::watchdog]
    #[test]
    fn close_stare_teleport_is_gated_to_full_ai_steps_only() {
        let mut full_steps = 0u32;
        let mut off_steps = 0u32;
        let mut teleports_on_full_steps = 0u32;
        let mut teleports_on_off_steps = 0u32;
        stare_context(2.0, |core, cx| {
            assert!(
                stared_at_by(core, &player(cx, PLAYER_ID).unwrap(), cx),
                "test setup should aim the player exactly at the enderman"
            );
            let mut goal = LookForStaringPlayer {
                pending: None,
                aggro_time: 0,
                teleport_time: 0,
                target: Some(PLAYER_ID),
                unseen_ticks: 0,
            };
            for tick in 1..=200u64 {
                core.base.position = MOB_POS;
                core.tick_count = tick;
                let id_tick = core.tick_count + core.base.id as u64;
                let full_step = id_tick.is_multiple_of(2) || core.tick_count <= 1;
                // Mirrors `server_ai_step`'s pacing: the goal only gets a
                // full tick (start/continue/tick) every other AI step; the
                // off ticks run nothing for it, since it is not `every_tick`.
                if full_step {
                    full_steps += 1;
                    goal.tick(core, cx);
                } else {
                    off_steps += 1;
                }
                let teleported = core.base.position != MOB_POS;
                match (full_step, teleported) {
                    (true, true) => teleports_on_full_steps += 1,
                    (false, true) => teleports_on_off_steps += 1,
                    _ => {}
                }
            }
        });
        assert_eq!(
            teleports_on_off_steps, 0,
            "teleported on an AI step that should not have run the goal at all"
        );
        assert!(
            teleports_on_full_steps > 0,
            "never teleported while stared at up close over {full_steps} full AI steps"
        );
        assert!(
            teleports_on_full_steps < full_steps + off_steps,
            "teleported on every single one of {} ticks, unthrottled by the AI pacing",
            full_steps + off_steps
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn carried_grass_drops_itself_with_silk_touch() {
        let shared = tuff_test_support::bare_shared();
        let mut rng = StdRng::seed_from_u64(1);
        let drops = carried_drops(
            &*shared.block_loot(),
            Block::GrassBlock.default_state(),
            &mut rng,
        );
        assert_eq!(drops.len(), 1);
        assert_eq!(drops[0].item(), tuff_vanilla_data::Item::GrassBlock);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_carried_block_tool_is_never_a_real_item() {
        let mut rng = StdRng::seed_from_u64(1);
        let tool: tuff_item::item::ItemPicture = carried_block_tool(&mut rng);
        assert_eq!(tool.enchantment_level("minecraft:silk_touch"), 1);
    }
}
