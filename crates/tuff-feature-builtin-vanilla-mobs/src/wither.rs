//! The wither: a flying boss assembled from 4 soul sand/soil blocks and 3
//! wither skulls (see [`try_assemble`]), never naturally spawned. It is a
//! plain [`Species`] like any other mob -- unlike the ender dragon (which
//! is not `living()`, has independently hit-testable body parts with their
//! own client ids, and a dedicated fight actor for its arena and phases),
//! the wither is a single hit box with no arena, so nothing here forces the
//! dragon's special-cased structure. Its three heads are ordinary per-tick
//! species logic: the center head drives movement and melee through the
//! shared goal selector like any other mob, and the two side heads
//! independently pick their own targets and shoot on their own cooldowns
//! entirely inside [`Species::tick`], writing three plain entity data ints
//! the real client already knows how to render as head aim.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};

use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{LookAtPlayer, RandomLookAround};
use super::target::{self, TargetView};
use super::{Damage, MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{
    BossBarState, Culprit, Effect, RegionEntity, TickContext, take_number,
};
use tuff_core::world::explosion::{BlockInteraction, Explosion};
use tuff_core::world::player::Difficulty;
use tuff_core::world::projectile::{self, Launch, Owner, Shot};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

/// How long the birth (spawn) invulnerability lasts, in ticks. During it
/// the wither cannot act or be hurt, takes no goals, heals steadily and
/// its boss bar fills; at the end it detonates once.
const INVULNERABLE_TICKS: i32 = 220;
const BIRTH_HEAL_INTERVAL: i32 = 10;
const BIRTH_HEAL_AMOUNT: f32 = 10.0;
const NORMAL_HEAL_INTERVAL: i32 = 20;
const NORMAL_HEAL_AMOUNT: f32 = 1.0;
const BIRTH_EXPLOSION_POWER: f32 = 7.0;

const DESTROY_BLOCKS_TICKS: i32 = 20;

/// Boss Bar packet color/overlay enum ordinals (purple, plain progress
/// bar), matching the reference's own wither boss bar.
const BOSS_BAR_COLOR_PURPLE: i32 = 5;
const BOSS_BAR_OVERLAY_PROGRESS: i32 = 0;

const FLIGHT_VERTICAL_DAMPING: f64 = 0.6;
const FLIGHT_UNPOWERED_HOVER: f64 = 5.0;
const FLIGHT_CLIMB: f64 = 0.3;
const FLIGHT_CLIMB_DRAG: f64 = 0.600_000_023_841_857_9;
const FLIGHT_ACCEL: f64 = 0.3;
const FLIGHT_HORIZONTAL_DRAG: f64 = 0.6;
const FLIGHT_APPROACH_SQ: f64 = 9.0;
const CENTER_ATTACK_INTERVAL: i32 = 40;
/// One in a thousand of the center head's targeted shots is a dangerous
/// (black) skull instead of a normal one.
const CENTER_DANGEROUS_CHANCE: f64 = 0.001;

/// Side heads pick a target within this box (half width/height/depth) and
/// this squared range, matching the reference's own combat conditions.
const SIDE_HEAD_RANGE_SQ: f64 = 900.0;
const SIDE_HEAD_IDLE_BOX: [f64; 3] = [10.0, 5.0, 10.0];
const SIDE_HEAD_BOX: [f64; 3] = [20.0, 8.0, 20.0];
const SIDE_HEAD_REACQUIRE_MIN: i32 = 10;
const SIDE_HEAD_REACQUIRE_SPREAD: i32 = 10;
const SIDE_HEAD_COOLDOWN_MIN: i32 = 40;
const SIDE_HEAD_COOLDOWN_SPREAD: i32 = 20;
/// A side head that has gone this many reacquire cycles without a target
/// fires a dangerous skull into open space, like the reference's own idle
/// firing.
const SIDE_HEAD_IDLE_LIMIT: i32 = 15;

/// The entity data index of the center head's target id. The wither is not
/// ageable, so this is the first free species index (see [`super::data`]);
/// checked against the dumped layout in
/// `wither_data_indices_match_the_dumped_layout`.
const DATA_TARGET_A: u8 = data::BABY;
const DATA_TARGET_B: u8 = DATA_TARGET_A + 1;
const DATA_TARGET_C: u8 = DATA_TARGET_A + 2;
const DATA_INVULNERABLE: u8 = DATA_TARGET_A + 3;

fn candidate(kind: EntityType) -> bool {
    !tuff_core::world::entity_in_tag(kind, "wither_friends")
}

/// The nearest attackable, non-wither-friend living entity (player or mob)
/// within `range_sq`, excluding `exclude`.
fn nearest_target(
    core: &MobCore,
    cx: &TickContext,
    range_sq: f64,
    exclude: Option<i32>,
) -> Option<TargetView> {
    let ids = cx.players.iter().map(|p| p.id).chain(
        cx.entities
            .iter()
            .filter(|e| e.living.is_some())
            .map(|e| e.id),
    );
    ids.filter(|id| Some(*id) != exclude)
        .filter_map(|id| target::view(cx, id))
        .filter(|t| candidate(t.kind) && core.can_attack(t, cx))
        .filter(|t| core.distance_sq(t.position) <= range_sq)
        .min_by(|a, b| {
            core.distance_sq(a.position)
                .total_cmp(&core.distance_sq(b.position))
        })
}

fn random_target_in_box(core: &MobCore, cx: &mut TickContext) -> Option<TargetView> {
    let center = core.base.position;
    let ids: Vec<i32> = cx
        .players
        .iter()
        .map(|p| p.id)
        .chain(
            cx.entities
                .iter()
                .filter(|e| e.living.is_some())
                .map(|e| e.id),
        )
        .collect();
    let mut found: Vec<TargetView> = ids
        .into_iter()
        .filter_map(|id| target::view(cx, id))
        .filter(|t| candidate(t.kind) && core.can_attack(t, cx))
        .filter(|t| {
            (0..3).all(|axis| (t.position[axis] - center[axis]).abs() <= SIDE_HEAD_BOX[axis])
        })
        .collect();
    if found.is_empty() {
        return None;
    }
    let pick = cx.rng.gen_range(0..found.len());
    Some(found.swap_remove(pick))
}

/// Still there, still alive, still attackable, still in range.
fn keep_target(core: &MobCore, cx: &TickContext, id: i32, range_sq: f64) -> Option<TargetView> {
    let view = target::view(cx, id)?;
    if !candidate(view.kind) || !core.can_attack(&view, cx) {
        return None;
    }
    (core.distance_sq(view.position) <= range_sq).then_some(view)
}

/// One of the wither's three heads: 0 is the center (body position, no
/// horizontal offset), 1 and 2 are the side heads, offset by `body_yaw`
/// plus 0 or 180 degrees -- matches the reference's own head placement.
fn head_position(core: &MobCore, index: u8) -> [f64; 3] {
    let [x, y, z] = core.base.position;
    if index == 0 {
        return [x, y + f64::from(core.size.height) * 0.86, z];
    }
    let angle = (core.body_yaw + 180.0 * f32::from(index - 1)).to_radians();
    [
        x + f64::from(angle.cos()) * 1.3,
        y + f64::from(core.size.height) * 0.63,
        z + f64::from(angle.sin()) * 1.3,
    ]
}

fn shoot_skull(core: &MobCore, head: u8, at: [f64; 3], dangerous: bool, cx: &mut TickContext) {
    let from = head_position(core, head);
    let direction = [at[0] - from[0], at[1] - from[1], at[2] - from[2]];
    let owner = Owner::of(Culprit::entity(&core.base), None);
    let id = cx.shared.next_entity_id();
    projectile::launch(
        Launch {
            id,
            position: from,
            owner,
            shot: Shot::WitherSkull {
                direction,
                dangerous,
            },
        },
        cx,
    );
    core.sound(SoundEvent::EntityWitherShoot, 1.0, 1.0, cx);
}

/// Whether damage of `kind` bypasses invulnerability windows entirely (out
/// of world, `/kill`).
fn bypasses_invulnerability(damage: &Damage) -> bool {
    damage.is("bypasses_invulnerability")
}

#[derive(Debug)]
pub struct Wither {
    invulnerable_ticks: i32,
    target_b: i32,
    target_c: i32,
    reacquire: [i32; 2],
    idle_cycles: [i32; 2],
    destroy_blocks_tick: i32,
    boss_progress: f32,
}

impl Wither {
    fn is_powered(&self, core: &MobCore) -> bool {
        core.health <= core.max_health() / 2.0
    }

    fn side_head_index(head: u8) -> usize {
        (head - 1) as usize
    }

    fn side_target(&self, head: u8) -> i32 {
        if Self::side_head_index(head) == 0 {
            self.target_b
        } else {
            self.target_c
        }
    }

    fn set_side_target(&mut self, core: &mut MobCore, head: u8, id: i32) {
        let index = Self::side_head_index(head);
        let (slot, data_index) = if index == 0 {
            (&mut self.target_b, DATA_TARGET_B)
        } else {
            (&mut self.target_c, DATA_TARGET_C)
        };
        if *slot != id {
            *slot = id;
            core.changed(data_index);
        }
    }

    fn set_center_target(&self, core: &mut MobCore, id: i32) {
        if core.target != Some(id).filter(|id| *id != 0) {
            core.target = (id != 0).then_some(id);
            core.changed(DATA_TARGET_A);
        }
    }

    fn tick_side_head(&mut self, core: &mut MobCore, head: u8, cx: &mut TickContext) {
        let index = Self::side_head_index(head);
        self.reacquire[index] -= 1;
        if self.reacquire[index] > 0 {
            return;
        }
        self.reacquire[index] =
            SIDE_HEAD_REACQUIRE_MIN + cx.rng.gen_range(0..SIDE_HEAD_REACQUIRE_SPREAD);
        if matches!(
            *cx.shared.difficulty.borrow(),
            Difficulty::Normal | Difficulty::Hard
        ) {
            self.idle_cycles[index] += 1;
            if self.idle_cycles[index] > SIDE_HEAD_IDLE_LIMIT {
                self.idle_cycles[index] = 0;
                let from = core.base.position;
                let radius = SIDE_HEAD_IDLE_BOX;
                let at = [
                    from[0] + (f64::from(cx.rng.r#gen::<f32>()) * 2.0 - 1.0) * radius[0],
                    from[1] + (f64::from(cx.rng.r#gen::<f32>()) * 2.0 - 1.0) * radius[1],
                    from[2] + (f64::from(cx.rng.r#gen::<f32>()) * 2.0 - 1.0) * radius[2],
                ];
                shoot_skull(core, head, at, true, cx);
            }
        }
        let current = self.side_target(head);
        if current != 0 {
            let target = keep_target(core, cx, current, SIDE_HEAD_RANGE_SQ)
                .filter(|target| core.has_line_of_sight(target, cx));
            if let Some(target) = target {
                shoot_skull(core, head, target.eye, false, cx);
                self.reacquire[index] +=
                    SIDE_HEAD_COOLDOWN_MIN + cx.rng.gen_range(0..SIDE_HEAD_COOLDOWN_SPREAD);
                self.idle_cycles[index] = 0;
            } else {
                self.set_side_target(core, head, 0);
            }
        } else if let Some(target) = random_target_in_box(core, cx) {
            self.set_side_target(core, head, target.id);
        }
    }
}

impl Species for Wither {
    fn create(_rng: &mut StdRng) -> Self {
        Self {
            invulnerable_ticks: 0,
            target_b: 0,
            target_c: 0,
            reacquire: [0; 2],
            idle_cycles: [0; 2],
            destroy_blocks_tick: 0,
            boss_progress: 1.0,
        }
    }

    fn configure(core: &mut MobCore) {
        core.hostile = true;
        core.flying = true;
        core.xp_reward = 50;
        core.move_style = super::control::MoveStyle::Float { duration: 0 };
        let mut goals = GoalSelector::default();
        goals.add(1, CenterHeadAttack::default());
        goals.add(5, WaterAvoidingFlying);
        goals.add(6, LookAtPlayer::new(8.0));
        goals.add(7, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.invulnerable_ticks = take_number(nbt, "Invul").map_or(0, |v| v as i32);
        core.no_ai = self.invulnerable_ticks > 0;
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("Invul", self.invulnerable_ticks);
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (
                DATA_TARGET_A,
                MetadataValue::VarInt(core.target.unwrap_or(0)),
            ),
            (DATA_TARGET_B, MetadataValue::VarInt(self.target_b)),
            (DATA_TARGET_C, MetadataValue::VarInt(self.target_c)),
            (
                DATA_INVULNERABLE,
                MetadataValue::VarInt(self.invulnerable_ticks),
            ),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntityWitherAmbient),
            hurt: Some(SoundEvent::EntityWitherHurt),
            death: Some(SoundEvent::EntityWitherDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn before_hurt(&mut self, core: &mut MobCore, damage: &Damage, _cx: &mut TickContext) -> bool {
        if self.invulnerable_ticks > 0 && !bypasses_invulnerability(damage) {
            return false;
        }
        if damage.is("wither_immune_to") {
            return false;
        }
        if damage
            .attacker
            .as_ref()
            .is_some_and(|(_, kind)| !candidate(*kind))
        {
            return false;
        }
        if self.is_powered(core)
            && matches!(
                damage.projectile,
                Some(
                    EntityType::Arrow
                        | EntityType::SpectralArrow
                        | EntityType::Trident
                        | EntityType::WindCharge
                        | EntityType::BreezeWindCharge
                )
            )
        {
            return false;
        }
        true
    }

    fn after_hurt(&mut self, _core: &mut MobCore, _damage: &Damage, _cx: &mut TickContext) {
        for idle in &mut self.idle_cycles {
            *idle += 3;
        }
        if self.destroy_blocks_tick <= 0 {
            self.destroy_blocks_tick = DESTROY_BLOCKS_TICKS;
        }
    }

    /// The wither's loot table is empty in the jar data (`ENTITIES_WITHER`
    /// has no pools): in the reference, the Nether Star comes from the
    /// entity's own code, not the data-driven table, and drops
    /// unconditionally (unlike the table drops the `mob_drops` game rule
    /// gates) since it is spawned before that gate is even checked.
    fn died(&mut self, core: &mut MobCore, _damage: &Damage, cx: &mut TickContext) {
        let [x, y, z] = core.base.position;
        let velocity = [
            cx.rng.r#gen::<f64>() * 0.2 - 0.1,
            0.2,
            cx.rng.r#gen::<f64>() * 0.2 - 0.1,
        ];
        #[expect(
            clippy::disallowed_methods,
            reason = "item a mob spawns, wears or throws (died)"
        )]
        cx.effects.push(Effect::DropExtendedItem {
            stack: tuff_item::item::ItemStack::mint(
                tuff_vanilla_data::Item::NetherStar,
                1,
                tuff_item::item::MintReason::Spawn,
            ),
            position: [x, y, z],
            velocity,
            pickup_delay: tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY,
        });
    }

    fn boss_bar(&self, core: &MobCore) -> Option<BossBarState> {
        Some(BossBarState {
            name: Culprit::entity(&core.base).name,
            progress: self.boss_progress,
            color: BOSS_BAR_COLOR_PURPLE,
            overlay: BOSS_BAR_OVERLAY_PROGRESS,
            flags: tuff_core::protocol::play::boss_flags::DARKEN_SCREEN,
        })
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.base.velocity[1] *= FLIGHT_VERTICAL_DAMPING;
        if let Some(target) = core.target_view(cx) {
            let powered = self.is_powered(core);
            core.base.velocity = flight_velocity(
                core.base.velocity,
                core.base.position,
                target.position,
                powered,
            );
        }
        let [vx, _, vz] = core.base.velocity;
        if vx * vx + vz * vz > 0.05 {
            core.base.yaw = (vz.atan2(vx) as f32).to_degrees() - 90.0;
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.invulnerable_ticks > 0 {
            self.invulnerable_ticks -= 1;
            core.changed(DATA_INVULNERABLE);
            core.no_ai = true;
            self.boss_progress = 1.0 - self.invulnerable_ticks as f32 / INVULNERABLE_TICKS as f32;
            if self.invulnerable_ticks <= 0 {
                let rules = cx.shared.game_rules.borrow();
                let blocks = if !rules.bool("mob_griefing") {
                    BlockInteraction::Keep
                } else if rules.bool("mob_explosion_drop_decay") {
                    BlockInteraction::DestroyWithDecay
                } else {
                    BlockInteraction::Destroy
                };
                drop(rules);
                cx.effects.push(Effect::Explode(Explosion {
                    dangerous_wither_skull: false,
                    spares_nether_portals: false,
                    center: [
                        core.base.position[0],
                        core.eye_position()[1],
                        core.base.position[2],
                    ],
                    radius: BIRTH_EXPLOSION_POWER,
                    fire: false,
                    blocks,
                    damage_type: &tuff_vanilla_data::damage_type::EXPLOSION,
                    igniter: None,
                    culprit: Some(Box::new(Culprit::entity(&core.base))),
                }));
                if !core.base.silent {
                    let position = BlockPos::new(
                        core.base.position[0].floor() as i32,
                        core.base.position[1].floor() as i32,
                        core.base.position[2].floor() as i32,
                    );
                    if cx.shared.rule_bool("global_sound_events") {
                        let _ = cx.shared.coordinator.try_send(
                            tuff_core::world::messages::CoordinatorMessage::GlobalLevelEvent {
                                dimension: cx.dimension,
                                event:
                                    tuff_core::protocol::play::level_event::SOUND_WITHER_BOSS_SPAWN,
                                pos: position,
                                data: 0,
                            },
                        );
                    } else {
                        let packet =
                            tuff_core::world::framed(&tuff_core::protocol::play::LevelEvent {
                                event:
                                    tuff_core::protocol::play::level_event::SOUND_WITHER_BOSS_SPAWN,
                                position,
                                data: 0,
                                global: false,
                            });
                        cx.effects
                            .push(Effect::ChunkPackets(tuff_core::world::packets([packet])));
                    }
                }
            } else if self.invulnerable_ticks % BIRTH_HEAL_INTERVAL == 0 {
                core.heal(BIRTH_HEAL_AMOUNT);
            }
            return;
        }
        core.no_ai = false;
        if self.destroy_blocks_tick > 0 {
            self.destroy_blocks_tick -= 1;
            if self.destroy_blocks_tick == 0 && cx.shared.rule_bool("mob_griefing") {
                destroy_nearby_blocks(core, cx);
            }
        }
        if core.tick_count.is_multiple_of(NORMAL_HEAL_INTERVAL as u64) {
            core.heal(NORMAL_HEAL_AMOUNT);
        }
        self.boss_progress = (core.health / core.max_health()).clamp(0.0, 1.0);
        if let Some(id) = core.target
            && keep_target(core, cx, id, core.follow_range().powi(2)).is_none()
        {
            self.set_center_target(core, 0);
        }
        if core.target.is_none()
            && let Some(target) = nearest_target(core, cx, core.follow_range().powi(2), None)
        {
            self.set_center_target(core, target.id);
        }
        self.tick_side_head(core, 1, cx);
        self.tick_side_head(core, 2, cx);
    }
}

/// Blocks a griefing-enabled wither clears as it moves: not air, not
/// `wither_immune`, within its own bounding box footprint plus one block
/// of height.
fn flight_velocity(
    mut velocity: [f64; 3],
    position: [f64; 3],
    target: [f64; 3],
    powered: bool,
) -> [f64; 3] {
    if position[1] < target[1] || (!powered && position[1] < target[1] + FLIGHT_UNPOWERED_HOVER) {
        let up = velocity[1].max(0.0);
        velocity[1] = up + (FLIGHT_CLIMB - up * FLIGHT_CLIMB_DRAG);
    }
    let (dx, dz) = (target[0] - position[0], target[2] - position[2]);
    let distance_sq = dx * dx + dz * dz;
    if distance_sq > FLIGHT_APPROACH_SQ {
        let length = distance_sq.sqrt();
        velocity[0] += dx / length * FLIGHT_ACCEL - velocity[0] * FLIGHT_HORIZONTAL_DRAG;
        velocity[2] += dz / length * FLIGHT_ACCEL - velocity[2] * FLIGHT_HORIZONTAL_DRAG;
    }
    velocity
}

fn destroy_nearby_blocks(core: &MobCore, cx: &mut TickContext) {
    let [x, y, z] = core.base.position;
    let half = ((f64::from(core.size.width) / 2.0) + 1.0).floor() as i32;
    let height = f64::from(core.size.height).floor() as i32;
    let (bx, by, bz) = (x.floor() as i32, y.floor() as i32, z.floor() as i32);
    let mut destroyed = false;
    for dx in -half..=half {
        for dy in 0..=height {
            for dz in -half..=half {
                let pos = BlockPos::new(bx + dx, by + dy, bz + dz);
                let Some(state) = (cx.block)(pos) else {
                    continue;
                };
                if can_destroy(state) {
                    cx.effects.push(Effect::DestroyBlock { pos, drop: true });
                    destroyed = true;
                }
            }
        }
    }
    if destroyed {
        core.sound(SoundEvent::EntityWitherBreakBlock, 1.0, 1.0, cx);
    }
}

fn can_destroy(state: BlockState) -> bool {
    !tuff_world_model::chunk::is_air(state)
        && !tuff_datapack::tags::is(
            tuff_vanilla_data::tag::block::key::WITHER_IMMUNE,
            state.block(),
        )
}

/// The ranged attack goal driving the center head: moves toward its
/// target, faces it, and shoots on a cooldown once it has line of sight --
/// the usual ranged attack behavior.
#[derive(Debug, Default)]
struct CenterHeadAttack {
    cooldown: i32,
}

impl Goal for CenterHeadAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.target.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx).is_some_and(|t| t.alive)
    }

    fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.cooldown = 0;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        mob.look.set_look_at(target.eye, 10.0, 40.0);
        self.cooldown -= 1;
        if self.cooldown > 0 || !mob.has_line_of_sight(&target, cx) {
            return;
        }
        self.cooldown = CENTER_ATTACK_INTERVAL;
        let dangerous = cx.rng.r#gen::<f64>() < CENTER_DANGEROUS_CHANCE;
        shoot_skull(mob, 0, target.eye, dangerous, cx);
    }
}

// ---------------------------------------------------------------------
// Summon detection: 3 wither skulls atop a T of 4 soul sand/soil blocks
// with air under the bar's ends, in any of the 24 orientations, checked
// whenever a wither skull is placed.
// ---------------------------------------------------------------------

pub struct Assembly {
    pub spawn: [f64; 3],
    pub yaw: f32,
    pub consumed: Vec<BlockPos>,
}

fn is_base(lookup: &impl Fn(BlockPos) -> Option<BlockState>, pos: BlockPos) -> bool {
    lookup(pos).is_some_and(|s| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::block::key::WITHER_SUMMON_BASE_BLOCKS,
            s.block(),
        )
    })
}

fn is_skull(lookup: &impl Fn(BlockPos) -> Option<BlockState>, pos: BlockPos) -> bool {
    lookup(pos).is_some_and(|s| {
        matches!(
            s.block(),
            Block::WitherSkeletonSkull | Block::WitherSkeletonWallSkull
        )
    })
}

const DIRECTIONS: [[i32; 3]; 6] = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
];

fn is_air(lookup: &impl Fn(BlockPos) -> Option<BlockState>, pos: BlockPos) -> bool {
    lookup(pos).is_some_and(tuff_world_model::chunk::is_air)
}

fn offset(origin: BlockPos, across: [i32; 3], column: i32, down: [i32; 3], row: i32) -> BlockPos {
    BlockPos::new(
        origin.x + across[0] * column + down[0] * row,
        origin.y + across[1] * column + down[1] * row,
        origin.z + across[2] * column + down[2] * row,
    )
}

/// Matches the pattern `^^^ / ### / ~#~` with its top-left corner at
/// `origin`, columns running along `across` and rows along `down`.
fn try_orientation(
    origin: BlockPos,
    across: [i32; 3],
    down: [i32; 3],
    lookup: &impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<Assembly> {
    let at = |column: i32, row: i32| offset(origin, across, column, down, row);
    let mut consumed = Vec::with_capacity(7);
    for column in 0..3 {
        if !is_skull(lookup, at(column, 0)) || !is_base(lookup, at(column, 1)) {
            return None;
        }
        consumed.push(at(column, 0));
        consumed.push(at(column, 1));
    }
    let stem = at(1, 2);
    if !is_base(lookup, stem) || !is_air(lookup, at(0, 2)) || !is_air(lookup, at(2, 2)) {
        return None;
    }
    consumed.push(stem);
    let yaw = if across[0] != 0 { 0.0 } else { 90.0 };
    Some(Assembly {
        spawn: [
            stem.x as f64 + 0.5,
            stem.y as f64 + 0.55,
            stem.z as f64 + 0.5,
        ],
        yaw,
        consumed,
    })
}

/// Whether placing a wither skull at `placed` completes the summon shape
/// in any orientation, and if so, what to do about it.
pub fn try_assemble(
    placed: BlockPos,
    lookup: impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<Assembly> {
    if !is_skull(&lookup, placed) {
        return None;
    }
    for across in DIRECTIONS {
        for down in DIRECTIONS {
            if across.iter().zip(down).any(|(a, d)| *a != 0 && d != 0) {
                continue;
            }
            for column in 0..3 {
                for row in 0..3 {
                    let origin = offset(placed, across.map(|v| -v), column, down.map(|v| -v), row);
                    if let Some(assembly) = try_orientation(origin, across, down, &lookup) {
                        return Some(assembly);
                    }
                }
            }
        }
    }
    None
}

/// Starts a freshly assembled wither's birth: invulnerable for
/// [`INVULNERABLE_TICKS`], at a third of its max health (which the birth
/// heals back up), matching the game.
pub fn make_invulnerable(entity: &mut dyn RegionEntity) {
    if let Some(mob) = entity.downcast_mut::<super::Mob<Wither>>() {
        mob.species.invulnerable_ticks = INVULNERABLE_TICKS;
        mob.species.boss_progress = 0.0;
        mob.core.health = mob.core.max_health() / 3.0;
        mob.core.no_ai = true;
        mob.core.changed(DATA_INVULNERABLE);
        mob.core.changed(data::HEALTH);
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Wither, super::load::<Wither>);
}

/// Idles to a spot ahead of where it looks, now and then.
#[derive(Debug, Default)]
struct WaterAvoidingFlying;

impl Goal for WaterAvoidingFlying {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.no_action_time < 100
            && !mob.movement.has_wanted()
            && cx
                .rng
                .gen_range(0..super::goal::reduced_delay(super::goals::RandomStroll::INTERVAL))
                == 0
    }

    fn can_continue(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        false
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let view = super::ghast::view_vector(mob);
        if let Some(target) = super::random_pos::flying_pos(mob, view, cx) {
            mob.movement.set_wanted(target, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::protocol::play::MetadataValue;

    #[tuff_core::watchdog]
    #[test]
    fn flight_climbs_below_the_target_and_closes_distance_past_three_blocks() {
        let climbing = flight_velocity([0.0; 3], [0.0, 10.0, 0.0], [0.0, 12.0, 0.0], true);
        assert!((climbing[1] - 0.3).abs() < 1.0e-9);
        let unpowered = flight_velocity([0.0; 3], [0.0, 10.0, 0.0], [0.0, 6.0, 0.0], false);
        assert!(unpowered[1] > 0.0);
        let powered = flight_velocity([0.0; 3], [0.0, 10.0, 0.0], [0.0, 6.0, 0.0], true);
        assert_eq!(powered[1], 0.0);
        let closing = flight_velocity([0.0; 3], [0.0; 3], [10.0, 0.0, 0.0], true);
        assert!((closing[0] - 0.3).abs() < 1.0e-9);
        let near = flight_velocity([0.0; 3], [0.0; 3], [2.0, 0.0, 0.0], true);
        assert_eq!(near[0], 0.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn wither_data_indices_match_the_dumped_layout() {
        let int = MetadataValue::VarInt(0).type_id() as u8;
        let layout = EntityType::Wither.data_layout();
        assert_eq!(layout.len(), DATA_INVULNERABLE as usize + 1);
        for index in [
            DATA_TARGET_A,
            DATA_TARGET_B,
            DATA_TARGET_C,
            DATA_INVULNERABLE,
        ] {
            assert_eq!(layout[index as usize], int, "index {index}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn wither_skull_data_index_matches_the_dumped_layout() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let layout = EntityType::WitherSkull.data_layout();
        assert_eq!(layout.len(), 9);
        assert_eq!(layout[8], boolean);
    }

    fn lookup_of(blocks: Vec<(BlockPos, BlockState)>) -> impl Fn(BlockPos) -> Option<BlockState> {
        move |pos| blocks.iter().find(|(p, _)| *p == pos).map(|(_, s)| *s)
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_t_of_soul_sand_and_three_skulls_assembles_along_x() {
        let base = Block::SoulSand.default_state();
        let skull = Block::WitherSkeletonSkull.default_state();
        let air = Block::Air.default_state();
        let lookup = lookup_of(vec![
            (BlockPos::new(-1, 65, 0), skull),
            (BlockPos::new(0, 65, 0), skull),
            (BlockPos::new(1, 65, 0), skull),
            (BlockPos::new(-1, 64, 0), base),
            (BlockPos::new(0, 64, 0), base),
            (BlockPos::new(1, 64, 0), base),
            (BlockPos::new(0, 63, 0), base),
            (BlockPos::new(-1, 63, 0), air),
            (BlockPos::new(1, 63, 0), air),
        ]);
        let assembly = try_assemble(BlockPos::new(0, 65, 0), lookup).expect("assembles");
        assert_eq!(assembly.yaw, 0.0);
        assert_eq!(assembly.consumed.len(), 7);
    }

    #[tuff_core::watchdog]
    #[test]
    fn assembly_also_triggers_from_either_end_skull_along_z_with_soul_soil() {
        let base = Block::SoulSoil.default_state();
        let skull = Block::WitherSkeletonSkull.default_state();
        let air = Block::Air.default_state();
        let lookup = lookup_of(vec![
            (BlockPos::new(0, 65, -1), skull),
            (BlockPos::new(0, 65, 0), skull),
            (BlockPos::new(0, 65, 1), skull),
            (BlockPos::new(0, 64, -1), base),
            (BlockPos::new(0, 64, 0), base),
            (BlockPos::new(0, 64, 1), base),
            (BlockPos::new(0, 63, 0), base),
            (BlockPos::new(0, 63, -1), air),
            (BlockPos::new(0, 63, 1), air),
        ]);
        let assembly = try_assemble(BlockPos::new(0, 65, -1), lookup).expect("assembles");
        assert_eq!(assembly.yaw, 90.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn solid_blocks_under_the_bar_ends_do_not_assemble() {
        let base = Block::SoulSand.default_state();
        let skull = Block::WitherSkeletonSkull.default_state();
        let air = Block::Air.default_state();
        let build = |corner| {
            lookup_of(vec![
                (BlockPos::new(-1, 65, 0), skull),
                (BlockPos::new(0, 65, 0), skull),
                (BlockPos::new(1, 65, 0), skull),
                (BlockPos::new(-1, 64, 0), base),
                (BlockPos::new(0, 64, 0), base),
                (BlockPos::new(1, 64, 0), base),
                (BlockPos::new(0, 63, 0), base),
                (BlockPos::new(-1, 63, 0), corner),
                (BlockPos::new(1, 63, 0), air),
            ])
        };
        assert!(try_assemble(BlockPos::new(0, 65, 0), build(air)).is_some());
        assert!(try_assemble(BlockPos::new(0, 65, 0), build(base)).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_sideways_wither_against_a_wall_assembles() {
        let base = Block::SoulSand.default_state();
        let skull = Block::WitherSkeletonSkull.default_state();
        let air = Block::Air.default_state();
        let mut blocks = Vec::new();
        for y in 0..3 {
            blocks.push((BlockPos::new(0, 64 + y, 0), skull));
            blocks.push((BlockPos::new(1, 64 + y, 0), base));
        }
        blocks[2].1 = skull;
        blocks.retain(|(p, _)| p.x != 1 || p.y != 65);
        blocks.push((BlockPos::new(1, 65, 0), base));
        blocks.push((BlockPos::new(2, 65, 0), base));
        blocks.push((BlockPos::new(2, 64, 0), air));
        blocks.push((BlockPos::new(2, 66, 0), air));
        let assembly = try_assemble(BlockPos::new(0, 65, 0), lookup_of(blocks)).expect("assembles");
        assert_eq!(assembly.yaw, 90.0);
        assert_eq!(assembly.consumed.len(), 7);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_missing_stem_block_does_not_assemble() {
        let base = Block::SoulSand.default_state();
        let skull = Block::WitherSkeletonSkull.default_state();
        let lookup = lookup_of(vec![
            (BlockPos::new(-1, 65, 0), skull),
            (BlockPos::new(0, 65, 0), skull),
            (BlockPos::new(1, 65, 0), skull),
            (BlockPos::new(-1, 64, 0), base),
            (BlockPos::new(0, 64, 0), base),
            (BlockPos::new(1, 64, 0), base),
        ]);
        assert!(try_assemble(BlockPos::new(0, 65, 0), lookup).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn two_wither_skeleton_skulls_are_not_enough() {
        let base = Block::SoulSand.default_state();
        let skull = Block::WitherSkeletonSkull.default_state();
        let lookup = lookup_of(vec![
            (BlockPos::new(-1, 65, 0), skull),
            (BlockPos::new(0, 65, 0), skull),
            (BlockPos::new(-1, 64, 0), base),
            (BlockPos::new(0, 64, 0), base),
            (BlockPos::new(1, 64, 0), base),
            (BlockPos::new(0, 63, 0), base),
        ]);
        assert!(try_assemble(BlockPos::new(0, 65, 0), lookup).is_none());
    }
}

#[cfg(test)]
mod behavior_tests {
    use super::*;
    use crate::world::poi::PoiStore;
    use rand::SeedableRng;
    use tuff_core::world::entity::{EntityBase, NearbyPlayer};

    #[tuff_core::watchdog]
    #[test]
    fn a_vulnerable_wither_targets_a_nearby_player() {
        let shared = tuff_test_support::bare_shared();
        let block = |_: BlockPos| Some(tuff_vanilla_data::Block::Air.default_state());
        let light = |_: BlockPos| Some((15, 0));
        let biome = |_: BlockPos| Some("minecraft:plains");
        let mut rng = StdRng::seed_from_u64(1);
        let mut effects = Vec::new();
        let mut budget = 10_000;
        let pois = PoiStore::default();
        let player = NearbyPlayer {
            id: 1000,
            uuid: 77,
            name: "runner",
            position: [10.0, 64.0, 0.0],
            eye_height: 1.62,
            yaw: 0.0,
            pitch: 0.0,
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
        };
        let players = [player];
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
            players_in_dimension: 1,
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
        let base = EntityBase::new(EntityType::Wither, 1, 1, [0.0, 64.0, 0.0], [0.0; 3]);
        let mut core = MobCore::new(base);
        Wither::configure(&mut core);
        let mut wither = Wither::create(&mut StdRng::seed_from_u64(2));
        assert_eq!(wither.invulnerable_ticks, 0);
        wither.tick(&mut core, &mut cx);
        assert_eq!(
            core.target,
            Some(1000),
            "follow range {:?}, distance {:?}",
            core.follow_range(),
            core.distance_sq(player.position)
        );

        let mut goal = CenterHeadAttack::default();
        assert!(goal.can_use(&mut core, &mut cx), "can_use");
        goal.start(&mut core, &mut cx);
        let mut spawned = false;
        for _ in 0..CENTER_ATTACK_INTERVAL + 1 {
            goal.tick(&mut core, &mut cx);
            if cx.effects.iter().any(|e| matches!(e, Effect::Spawn(_))) {
                spawned = true;
                break;
            }
        }
        assert!(spawned, "the center head shoots a skull once in range");
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_nether_star_lives_long_and_the_wither_is_not_allowed_in_peaceful() {
        assert!(!EntityType::Wither.mob().allowed_in_peaceful);
        crate::tests::with_context(|_, cx| {
            let base = EntityBase::new(EntityType::Wither, 1, 1, [0.0, 64.0, 0.0], [0.0; 3]);
            let mut core = MobCore::new(base);
            Wither::configure(&mut core);
            let mut wither = Wither::create(&mut StdRng::seed_from_u64(2));
            cx.effects.clear();
            wither.died(
                &mut core,
                &Damage::of(&tuff_vanilla_data::damage_type::GENERIC),
                cx,
            );
            assert!(cx.effects.iter().any(|e| matches!(
                e,
                Effect::DropExtendedItem { stack, .. } if stack.item() == tuff_vanilla_data::Item::NetherStar
            )));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn undead_attackers_and_powered_arrows_cannot_hurt_the_wither() {
        crate::tests::with_context(|_, cx| {
            let base = EntityBase::new(EntityType::Wither, 1, 1, [0.0, 64.0, 0.0], [0.0; 3]);
            let mut core = MobCore::new(base);
            Wither::configure(&mut core);
            let mut wither = Wither::create(&mut StdRng::seed_from_u64(2));
            let mut hit = Damage::of(&tuff_vanilla_data::damage_type::GENERIC);
            hit.attacker = Some((5, EntityType::Zombie));
            assert!(!wither.before_hurt(&mut core, &hit, cx));
            let mut arrow = Damage::of(&tuff_vanilla_data::damage_type::GENERIC);
            arrow.projectile = Some(EntityType::SpectralArrow);
            assert!(wither.before_hurt(&mut core, &arrow, cx));
            core.health = core.max_health() / 2.0;
            assert!(!wither.before_hurt(&mut core, &arrow, cx));
            arrow.projectile = Some(EntityType::Trident);
            assert!(!wither.before_hurt(&mut core, &arrow, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_wither_announces_its_birth_when_the_countdown_ends() {
        crate::tests::with_context(|_, cx| {
            let base = EntityBase::new(EntityType::Wither, 1, 1, [0.0, 64.0, 0.0], [0.0; 3]);
            let mut core = MobCore::new(base);
            Wither::configure(&mut core);
            let mut wither = Wither::create(&mut StdRng::seed_from_u64(2));
            wither.invulnerable_ticks = 1;
            cx.effects.clear();
            wither.tick(&mut core, cx);
            assert!(cx.effects.iter().any(|e| matches!(e, Effect::Explode(_))));
        });
    }

    fn birth_blocks(griefing: bool) -> BlockInteraction {
        let mut out = None;
        crate::tests::with_rule("mob_griefing", griefing, |_, cx| {
            let base = EntityBase::new(EntityType::Wither, 1, 1, [0.0, 64.0, 0.0], [0.0; 3]);
            let mut core = MobCore::new(base);
            Wither::configure(&mut core);
            let mut wither = Wither::create(&mut StdRng::seed_from_u64(2));
            wither.invulnerable_ticks = 1;
            cx.effects.clear();
            wither.tick(&mut core, cx);
            out = cx.effects.iter().find_map(|e| match e {
                Effect::Explode(x) => Some(x.blocks),
                _ => None,
            });
        });
        out.expect("explosion")
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_birth_blast_breaks_blocks_only_with_mob_griefing() {
        assert_eq!(birth_blocks(false), BlockInteraction::Keep);
        assert!(matches!(
            birth_blocks(true),
            BlockInteraction::Destroy | BlockInteraction::DestroyWithDecay
        ));
    }
}
