//! A mob's walking, jumping, falling and swimming each tick, as the game
//! moves living entities: input turned into velocity, collision with
//! stepping, gravity and drag, block friction and speed factors, and fall
//! damage on landing.

use rand::Rng;
use tuff_core::world::scoreboard::Member;
use tuff_vanilla_data::registry::SoundEvent;

use super::MobCore;
use super::physics::{self, CLIMBING_SPEED, Movement, block_pos, modified_friction};
use tuff_core::protocol::play;
use tuff_core::world::entity::{Effect, TickContext};
use tuff_core::world::fall;
use tuff_core::world::player::on_climbable;
use tuff_core::world::{framed, freezing, packets};
use tuff_vanilla_data::Block;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_world_model::BlockPos;

const WATER_SLOW_DOWN: f32 = 0.8;

/// Boats and minecarts, which living things push and are pushed by.
fn pushable_vehicle(kind: tuff_vanilla_data::registry::EntityType) -> bool {
    vehicle_kinds().1.contains(&kind)
}

fn is_boat(kind: tuff_vanilla_data::registry::EntityType) -> bool {
    vehicle_kinds().0.contains(&kind)
}

type KindSet = std::collections::HashSet<tuff_vanilla_data::registry::EntityType>;

fn vehicle_kinds() -> &'static (KindSet, KindSet) {
    static KINDS: std::sync::OnceLock<(KindSet, KindSet)> = std::sync::OnceLock::new();
    KINDS.get_or_init(|| {
        let boats: KindSet = tuff_vanilla_data::Item::ALL
            .iter()
            .filter_map(|item| item.boat())
            .collect();
        let mut pushable = boats.clone();
        pushable.extend(
            tuff_vanilla_data::registry::EntityType::ALL
                .iter()
                .copied()
                .filter(|kind| kind.name().contains("minecart")),
        );
        (boats, pushable)
    })
}
/// The pushers a crammed mob counted: entities by id, and players.
#[derive(Debug)]
pub struct Crush {
    pub counted: Vec<i32>,
    pub players: i32,
}

struct Pusher {
    /// `None` for a player.
    id: Option<i32>,
    position: [f64; 3],
    riding: bool,
    /// It is pushable, so this mob's own turn lists, counts and pushes it.
    listed: bool,
    /// Its own turn pushes this mob back.
    pushes_back: bool,
}

/// The shove `other` gives an entity at `own` in one push, as an
/// `[x, z]` velocity change to subtract; `None` when they stand on the
/// same spot.
pub fn push_away(own: [f64; 3], other: [f64; 3]) -> Option<[f64; 2]> {
    let mut dx = other[0] - own[0];
    let mut dz = other[2] - own[2];
    let mut distance = dx.abs().max(dz.abs());
    if distance < 0.009_999_999_776_482_582 {
        return None;
    }
    distance = distance.sqrt();
    dx /= distance;
    dz /= distance;
    let power = (1.0 / distance).min(1.0) * 0.050_000_000_745_058_06;
    Some([dx * power, dz * power])
}

/// Whether a living thing of this kind pushes what it overlaps in its own
/// turn (bats and armor stands do not push mobs).
fn pushes_in_own_turn(kind: tuff_vanilla_data::registry::EntityType) -> bool {
    use tuff_vanilla_data::registry::EntityType as E;
    !matches!(kind, E::Bat | E::ArmorStand | E::Vex)
}

fn is_horse(kind: tuff_vanilla_data::registry::EntityType) -> bool {
    use tuff_vanilla_data::registry::EntityType as E;
    matches!(
        kind,
        E::Horse
            | E::Donkey
            | E::Mule
            | E::Llama
            | E::TraderLlama
            | E::SkeletonHorse
            | E::ZombieHorse
            | E::Camel
            | E::CamelHusk
    )
}

const SWIMMER_DRAG: f64 = 0.9;
const SWIMMER_SINK: f64 = 0.005;

/// How a mob moves itself while in water.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum WaterTravel {
    /// As anything living: water drag, slow sinking, climbing out at
    /// edges.
    #[default]
    Standard,
    /// A swimmer's own stroke: its input at `stroke` speed (its current
    /// speed when `None`), then a light drag, sinking slowly as `sinks`
    /// says.
    Swimmer { stroke: Option<f32>, sinks: Sinking },
    /// Keeps whatever velocity it has, in water or out (squids).
    Drift,
    /// Its own stroke at `stroke` speed while it swims under water (see
    /// `MobCore::swimming`), otherwise as anything living (drowned).
    WhileSwimming { stroke: f32 },
    /// A turtle's paddle: `0.1` stroke and light drag, sinking unless it has
    /// a target or is near home on its way there.
    Turtle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sinking {
    Never,
    WithoutTarget,
    /// Without a target and while its move control is not driving it.
    WhenIdle,
}

const LIQUID_JUMP: f64 = 0.04;
const NO_JUMP_DELAY: i32 = 10;
const SUPPORT_EPSILON: f32 = 1.0e-5;

impl MobCore {
    fn movement_attributes(&self) -> Movement {
        Movement::read(|a| self.attribute(a))
    }

    /// The block under the feet that sets friction and the speed and jump
    /// factors: the supporting block, `offset` below the feet.
    pub(super) fn on_pos(&self, offset: f32, cx: &TickContext) -> BlockPos {
        tuff_core::world::block_contact::on_pos(
            self.base.position,
            self.body.supporting,
            offset,
            cx.block,
        )
    }

    fn block_movement(
        &self,
        pos: BlockPos,
        cx: &TickContext,
    ) -> tuff_vanilla_data::entity::BlockMovement {
        (cx.block)(pos)
            .unwrap_or(tuff_vanilla_data::Block::Air.default_state())
            .block()
            .movement()
    }

    fn speed_factor(&self, cx: &TickContext) -> f32 {
        let here = block_pos(self.base.position);
        let state = (cx.block)(here);
        let factor = self.block_movement(here, cx).speed_factor;
        let water = state.is_some_and(|s| {
            matches!(
                s.block(),
                tuff_vanilla_data::Block::Water | tuff_vanilla_data::Block::BubbleColumn
            )
        });
        let base = if water || factor != 1.0 {
            factor
        } else {
            self.block_movement(self.on_pos(0.500_001, cx), cx)
                .speed_factor
        };
        let efficiency = self.movement_attributes().movement_efficiency;
        base + (1.0 - base) * efficiency
    }

    pub(crate) fn jump_factor(&self, cx: &TickContext) -> f32 {
        let here = self
            .block_movement(block_pos(self.base.position), cx)
            .jump_factor;
        if here == 1.0 {
            self.block_movement(self.on_pos(0.500_001, cx), cx)
                .jump_factor
        } else {
            here
        }
    }

    /// Knockback away from `(dx, dz)`, which points from the mob towards
    /// what pushes it.
    pub fn knockback(
        &mut self,
        power: f64,
        mut dx: f64,
        mut dz: f64,
        rng: &mut rand::rngs::StdRng,
    ) {
        let resistance =
            self.attribute(tuff_vanilla_data::registry::Attribute::KnockbackResistance);
        let power = power * (1.0 - resistance);
        if power <= 0.0 || self.no_knockback {
            return;
        }
        self.impulse = true;
        while dx * dx + dz * dz < 1.0e-5 {
            dx = (rng.r#gen::<f64>() - rng.r#gen::<f64>()) * 0.01;
            dz = (rng.r#gen::<f64>() - rng.r#gen::<f64>()) * 0.01;
        }
        let length = (dx * dx + dz * dz).sqrt();
        let (x, z) = (dx / length * power, dz / length * power);
        let velocity = self.base.velocity;
        self.base.velocity = [
            velocity[0] / 2.0 - x,
            if self.base.on_ground {
                (velocity[1] / 2.0 + power).min(0.4)
            } else {
                velocity[1]
            },
            velocity[2] / 2.0 - z,
        ];
    }

    pub(crate) fn jump_from_ground(&mut self, cx: &TickContext) {
        let power = self.movement_attributes().jump_strength * self.jump_factor(cx)
            + self.jump_boost as f32;
        if power <= 1.0e-5 {
            return;
        }
        self.base.velocity[1] = self.base.velocity[1].max(power as f64);
    }

    /// The living entity's jump handling: swim up in liquids, jump off the
    /// ground at most every ten ticks.
    pub fn apply_jump(&mut self, cx: &TickContext) {
        if !self.body.jumping {
            self.body.no_jump_delay = 0;
            return;
        }
        let in_lava = self.in_lava();
        let height = if in_lava {
            self.body.lava_height
        } else {
            self.body.water_height
        };
        let in_water = self.body.in_water && height > 0.0;
        let threshold = self.fluid_jump_threshold();
        let on_ground = self.base.on_ground;
        let swims = in_water && (!on_ground || height > threshold);
        let wades = in_lava && (!on_ground || self.body.lava_height > threshold);
        if swims || wades {
            self.jump_in_liquid();
        } else if (on_ground || in_water && height <= threshold) && self.body.no_jump_delay == 0 {
            self.jump_from_ground(cx);
            self.body.no_jump_delay = NO_JUMP_DELAY;
        }
    }

    fn jump_in_liquid(&mut self) {
        if let Some(speed) = self.lava_jump.filter(|_| self.in_lava()) {
            self.base.velocity[1] = speed;
            return;
        }
        self.base.velocity[1] += if self.navigation.can_float {
            LIQUID_JUMP
        } else {
            0.3
        };
    }

    pub fn travel(&mut self, input: [f64; 3], cx: &mut TickContext) {
        if self.water_travel == WaterTravel::Drift {
            let velocity = self.base.velocity;
            self.move_self(velocity, cx);
        } else if self.flying {
            self.travel_flying(input, cx);
        } else if self.in_liquid() {
            self.travel_in_fluid(input, cx);
        } else {
            self.travel_in_air(input, cx);
        }
    }

    pub(crate) fn effective_gravity(&self) -> f64 {
        if self.base.no_gravity {
            0.0
        } else {
            self.movement_attributes().gravity
        }
    }

    /// Flying: no gravity, and drag that depends on the fluid it is in.
    fn travel_flying(&mut self, input: [f64; 3], cx: &mut TickContext) {
        let speed = self.flying_speed.unwrap_or(0.02);
        let drag = if self.body.in_water {
            0.800_000_011_920_929
        } else if self.in_lava() {
            0.5
        } else {
            0.910_000_026_226_043_7
        };
        self.move_relative(speed, input);
        let velocity = self.base.velocity;
        self.move_self(velocity, cx);
        self.base.velocity = self.base.velocity.map(|c| c * drag);
    }

    pub fn move_relative(&mut self, speed: f32, input: [f64; 3]) {
        let delta = physics::input_vector(input, speed, self.base.yaw);
        for axis in 0..3 {
            self.base.velocity[axis] += delta[axis];
        }
    }

    fn travel_in_air(&mut self, input: [f64; 3], cx: &mut TickContext) {
        let attributes = self.movement_attributes();
        let friction = if self.base.on_ground && !self.discard_friction {
            let below = self.on_pos(0.500_001, cx);
            modified_friction(
                self.block_movement(below, cx).friction,
                attributes.friction_modifier,
            )
        } else {
            1.0
        };
        let speed = if !self.base.on_ground {
            0.02
        } else if friction > 0.6 {
            self.body.speed * (0.216_000_02 / (friction * friction * friction))
        } else {
            self.body.speed
        };
        self.move_relative(speed, input);
        let climbing = self.climbing || on_climbable(self.base.position, cx.block);
        if climbing {
            self.base.fall_distance = 0.0;
            let velocity = &mut self.base.velocity;
            velocity[0] = velocity[0].clamp(-0.15, 0.15);
            velocity[2] = velocity[2].clamp(-0.15, 0.15);
            velocity[1] = velocity[1].max(-0.15);
        }
        let velocity = self.base.velocity;
        self.move_self(velocity, cx);
        self.slime_step(cx);
        let mut movement = self.base.velocity;
        let feet = self
            .equipment
            .get(tuff_vanilla_data::item::EquipmentSlot::Feet)
            .map(|s| s.item());
        let snow_climb = self.body.in_powder_snow
            && tuff_core::world::freezing::walks_on_powder_snow(self.base.kind, feet);
        if (self.body.horizontal_collision || self.body.jumping) && (climbing || snow_climb) {
            movement[1] = CLIMBING_SPEED;
        }
        let y = movement[1] - self.effective_gravity();
        if self.discard_friction {
            self.base.velocity = [movement[0], y, movement[2]];
            return;
        }
        let air = modified_friction(0.91, attributes.air_drag_modifier);
        let horizontal = (friction * air) as f64;
        let vertical = if self.omnidirectional_air {
            air as f64
        } else {
            modified_friction(0.98, attributes.air_drag_modifier) as f64
        };
        self.base.velocity = [
            movement[0] * horizontal,
            y * vertical,
            movement[2] * horizontal,
        ];
    }

    fn slime_step(&mut self, cx: &TickContext) {
        if !self.base.on_ground || slime_scale(self.base.velocity[1]).is_none() {
            return;
        }
        let on_slime = (cx.block)(self.on_pos(0.2, cx))
            .is_some_and(|state| state.block() == Block::SlimeBlock);
        if on_slime {
            let scale = slime_scale(self.base.velocity[1]).unwrap_or(1.0);
            self.base.velocity[0] *= scale;
            self.base.velocity[2] *= scale;
        }
    }

    fn swim(&mut self, input: [f64; 3], stroke: Option<f32>, sinks: Sinking, cx: &mut TickContext) {
        self.move_relative(stroke.unwrap_or(self.body.speed), input);
        let velocity = self.base.velocity;
        self.move_self(velocity, cx);
        self.base.velocity = self.base.velocity.map(|c| c * SWIMMER_DRAG);
        let sinking = match sinks {
            Sinking::Never => false,
            Sinking::WithoutTarget => self.target.is_none(),
            Sinking::WhenIdle => !self.moving && self.target.is_none(),
        };
        if sinking {
            self.base.velocity[1] -= SWIMMER_SINK;
        }
    }

    fn travel_in_fluid(&mut self, input: [f64; 3], cx: &mut TickContext) {
        if self.body.in_water {
            match self.water_travel {
                WaterTravel::Swimmer { stroke, sinks } => {
                    return self.swim(input, stroke, sinks, cx);
                }
                WaterTravel::WhileSwimming { stroke }
                    if self.swimming && self.body.eyes_in_water =>
                {
                    return self.swim(input, Some(stroke), Sinking::Never, cx);
                }
                WaterTravel::Turtle => {
                    let sinks = if self.target.is_none() && !(self.homeward && self.near_home(20.0))
                    {
                        Sinking::WithoutTarget
                    } else {
                        Sinking::Never
                    };
                    return self.swim(input, Some(0.1), sinks, cx);
                }
                _ => {}
            }
        }
        let falling = self.base.velocity[1] <= 0.0;
        let old_y = self.base.position[1];
        let gravity = self.effective_gravity();
        if self.body.in_water {
            let attributes = self.movement_attributes();
            let mut slow_down = WATER_SLOW_DOWN;
            let mut speed = 0.02;
            let mut efficiency = attributes.water_efficiency;
            if !self.base.on_ground {
                efficiency *= 0.5;
            }
            if efficiency > 0.0 {
                slow_down += (0.546_000_06 - slow_down) * efficiency;
                speed = 0.02 + (self.body.speed - 0.02) * efficiency;
            }
            self.move_relative(speed, input);
            let velocity = self.base.velocity;
            self.move_self(velocity, cx);
            let mut movement = self.base.velocity;
            if self.body.horizontal_collision
                && (self.climbing || on_climbable(self.base.position, cx.block))
            {
                movement[1] = CLIMBING_SPEED;
            }
            let movement = [
                movement[0] * slow_down as f64,
                movement[1] * 0.800_000_011_920_929,
                movement[2] * slow_down as f64,
            ];
            self.base.velocity = fluid_falling(gravity, falling, movement);
        } else {
            self.move_relative(0.02, input);
            let velocity = self.base.velocity;
            self.move_self(velocity, cx);
            if self.body.lava_height <= self.fluid_jump_threshold() {
                let v = self.base.velocity;
                let slowed = [v[0] * 0.5, v[1] * 0.800_000_011_920_929, v[2] * 0.5];
                self.base.velocity = fluid_falling(gravity, falling, slowed);
            } else {
                self.base.velocity = self.base.velocity.map(|c| c * 0.5);
            }
            if gravity != 0.0 {
                self.base.velocity[1] -= gravity / 4.0;
            }
        }
        let v = self.base.velocity;
        let lifted = physics::offset(
            self.bounds(),
            [v[0], v[1] + 0.6 - self.base.position[1] + old_y, v[2]],
        );
        if self.body.horizontal_collision
            && physics::no_collision(lifted, cx.block)
            && physics::fluid_heights(lifted, self.eye_position(), cx.block) == Default::default()
        {
            self.base.velocity[1] = 0.300_000_011_920_929;
        }
    }

    /// Moves by `delta` against the world, as the game's entity move does
    /// for a mob moving itself.
    pub fn move_self(&mut self, delta: [f64; 3], cx: &mut TickContext) {
        if self.phasing {
            self.body.stuck = None;
            for axis in 0..3 {
                self.base.position[axis] += delta[axis];
            }
            self.body.horizontal_collision = false;
            self.body.vertical_collision = false;
            return;
        }
        let delta = match self.body.stuck.take() {
            Some(scale) => {
                self.base.velocity = [0.0; 3];
                [0, 1, 2].map(|axis| delta[axis] * scale[axis])
            }
            None => delta,
        };
        let step = self.max_up_step();
        let bounds = self.bounds();
        let walkable = freezing::walks_on_powder_snow(
            self.kind(),
            self.equipment
                .get(EquipmentSlot::Feet)
                .map(|stack| stack.item()),
        );
        let fall_distance = self.base.fall_distance;
        let block = cx.block;
        let shape = |pos: BlockPos| match block(pos) {
            Some(state) if state.block() == Block::PowderSnow => {
                freezing::powder_snow_boxes(pos, bounds[1], fall_distance, walkable).into()
            }
            Some(state) if state.block() == Block::Scaffolding => {
                tuff_vanilla_common::scaffolding::collision_boxes(state, pos, bounds[1], false)
                    .into()
            }
            state => physics::Shape::collision(state, pos),
        };
        let moved =
            physics::collide_with(bounds, delta, step, self.base.on_ground, &shape, cx.pistons);
        let from = self.base.position;
        let length_sq = moved.iter().map(|v| v * v).sum::<f64>();
        let delta_sq = delta.iter().map(|v| v * v).sum::<f64>();
        if length_sq > 1.0e-7 || delta_sq - length_sq < 1.0e-7 {
            for axis in 0..3 {
                self.base.position[axis] += moved[axis];
            }
        }
        let x_hit = !approx_equal(delta[0], moved[0]);
        let z_hit = !approx_equal(delta[2], moved[2]);
        self.body.horizontal_collision = x_hit || z_hit;
        if delta[1].abs() > 0.0 {
            self.body.vertical_collision = delta[1] != moved[1];
            self.base.on_ground = self.body.vertical_collision && delta[1] < 0.0;
        }
        self.body.supporting = if self.base.on_ground {
            physics::supporting_block(self.bounds(), cx.block)
        } else {
            None
        };
        let effect_pos = self.on_pos(0.2, cx);
        let effect_state = (cx.block)(effect_pos);
        let traced = [0, 1, 2].map(|axis| self.base.position[axis] - from[axis]);
        if fall::move_resets_fall(from, traced, false, false, cx.block) {
            self.base.fall_distance = 0.0;
        }
        if self.base.on_ground
            && effect_state.is_some_and(|s| s.block() == Block::SlimeBlock)
            && self.base.velocity[1].abs() < 0.1
        {
            let scale = 0.4 + self.base.velocity[1].abs() * 0.2;
            self.base.velocity[0] *= scale;
            self.base.velocity[2] *= scale;
        }
        self.check_fall_damage(moved[1], effect_pos, effect_state, cx);
        if self.body.vertical_collision && delta[1].abs() > 0.0 {
            self.base.velocity[1] = self.bounce(moved[1], cx);
        }
        if x_hit {
            self.base.velocity[0] = 0.0;
        }
        if z_hit {
            self.base.velocity[2] = 0.0;
        }
        if self.base.on_ground && !self.base.silent {
            let horizontal = (moved[0] * moved[0] + moved[2] * moved[2]).sqrt();
            self.move_distance += (horizontal * 0.600_000_023_841_857_9) as f32;
            let support = self.on_pos(SUPPORT_EPSILON, cx);
            let ground = (cx.block)(support).is_some_and(|s| !tuff_world_model::chunk::is_air(s));
            if self.move_distance > self.next_step && ground {
                self.next_step = self.move_distance.floor() + 1.0;
                self.stepped = true;
            }
        }
        let factor = self.speed_factor(cx) as f64;
        self.base.velocity[0] *= factor;
        self.base.velocity[2] *= factor;
    }

    /// Whether others push it and count it toward cramming: alive, not
    /// climbing (parrots excepted) and, for a horse, carrying nobody.
    pub fn pushable_now(&self, cx: &TickContext) -> bool {
        use tuff_vanilla_data::registry::EntityType as E;
        let kind = self.kind();
        if !self.is_alive() || self.unpushable {
            return false;
        }
        let carries = !self.base.riders.is_empty() || !self.base.entity_riders.is_empty();
        if carries && is_horse(kind) {
            return false;
        }
        kind == E::Parrot || !(self.climbing || on_climbable(self.base.position, cx.block))
    }

    /// Pushes itself away from the pushable things it overlaps, counting
    /// the push each of them gives back in its own turn, unless something
    /// rides it or both ride the same vehicle. What it is crammed among
    /// when that is more than the `max_entity_cramming` rule allows.
    pub fn push_entities(&mut self, cx: &mut TickContext) -> Option<Crush> {
        let bounds = self.bounds();
        let self_pushable = self.pushable_now(cx);
        self.pushed_off = !self_pushable;
        let pushable_other = |e: &tuff_core::world::entity::EntitySummary| {
            e.living.is_some_and(|l| l.alive && !l.unpushable)
        };
        if self.retargets_on_push {
            let bumped = cx.entities_near(&bounds).iter().copied().find(|e| {
                e.id != self.base.id
                    && e.kind != tuff_vanilla_data::registry::EntityType::Creeper
                    && pushable_other(e)
                    && e.living.is_some_and(|l| l.enemy)
                    && physics::intersects(e.bounds, bounds)
            });
            if let Some(enemy) = bumped.map(|e| e.id)
                && cx.rng.gen_range(0..20) == 0
            {
                self.target = Some(enemy);
            }
        }
        let root = |mut id: i32, mut vehicle: Option<i32>| {
            for _ in 0..16 {
                let Some(next) = vehicle else { break };
                id = next;
                vehicle = cx
                    .entities
                    .iter()
                    .find(|e| e.id == next)
                    .and_then(|e| e.vehicle);
            }
            id
        };
        let own_root = root(self.base.id, self.base.vehicle.as_ref().map(|m| m.id));
        let teams = cx.shared.scoreboard.teams();
        let own = crate::target::team_member(
            cx,
            self.base.uuid,
            self.tame.as_ref().and_then(|t| t.owner),
        );
        let max = cx.shared.rule_int("max_entity_cramming");
        let (near, sample) = if self.crowded {
            let sample = crate::crowd::MAX_PUSHERS.max(max.max(0) as usize);
            let seed = self.tick_count + self.base.id as u64;
            (
                cx.entities_overlapping_sample(&bounds, seed, sample * 2),
                sample,
            )
        } else {
            (cx.entities_near(&bounds), usize::MAX)
        };
        let mut pushers: Vec<Pusher> = near
            .iter()
            .copied()
            .filter(|e| {
                e.id != self.base.id
                    && e.living.is_some_and(|l| l.alive)
                    && physics::intersects(e.bounds, bounds)
                    && root(e.id, e.vehicle) != own_root
                    && teams.pushes(
                        own,
                        crate::target::team_member(cx, e.uuid, e.living.and_then(|l| l.owner)),
                    )
            })
            .map(|e| Pusher {
                id: Some(e.id),
                position: e.position,
                riding: e.vehicle.is_some(),
                listed: pushable_other(e),
                pushes_back: pushes_in_own_turn(e.kind),
            })
            .take(sample)
            .collect();
        let co_riders: &[i32] = self
            .base
            .vehicle
            .as_ref()
            .map_or(&[], |mount| &mount.passengers);
        pushers.extend(
            cx.players
                .iter()
                .filter(|p| {
                    !self.base.riders.iter().any(|r| r.id == p.id)
                        && !co_riders.contains(&p.id)
                        && teams.pushes(own, Member::Player(p.name))
                })
                .filter(|p| {
                    let [x, y, z] = p.position;
                    physics::intersects([x - 0.3, y, z - 0.3, x + 0.3, y + 1.8, z + 0.3], bounds)
                })
                .map(|p| Pusher {
                    id: None,
                    position: p.position,
                    riding: p.vehicle.is_some(),
                    listed: !on_climbable(p.position, cx.block),
                    pushes_back: true,
                }),
        );
        let vehicles: Vec<&tuff_core::world::entity::EntitySummary> = near
            .iter()
            .copied()
            .filter(|e| {
                e.id != self.base.id
                    && e.living.is_none()
                    && pushable_vehicle(e.kind)
                    && physics::intersects(e.bounds, bounds)
                    && root(e.id, e.vehicle) != own_root
                    && teams.pushes(
                        own,
                        crate::target::team_member(cx, e.uuid, e.living.and_then(|l| l.owner)),
                    )
            })
            .collect();
        let listed = pushers.iter().filter(|p| p.listed).count() + vehicles.len();
        let counted = || {
            let mut ids: Vec<i32> = vehicles
                .iter()
                .filter(|v| v.vehicle.is_none())
                .map(|v| v.id)
                .collect();
            let mut players = 0;
            for pusher in pushers.iter().filter(|p| p.listed && !p.riding) {
                match pusher.id {
                    Some(id) => ids.push(id),
                    None => players += 1,
                }
            }
            Crush {
                counted: ids,
                players,
            }
        };
        let crammed = (max > 0
            && listed as i32 > max - 1
            && cx
                .rng
                .gen_range(0..4 / crate::crowd::push_scale(self) as u32)
                == 0)
            .then(counted)
            .filter(|crush| crush.counted.len() as i32 + crush.players > max - 1);
        if self.no_physics {
            return crammed;
        }
        let carries = !self.base.riders.is_empty() || !self.base.entity_riders.is_empty();
        let mut shoves = Vec::new();
        for vehicle in vehicles {
            let xa = self.base.position[0] - vehicle.position[0];
            let za = self.base.position[2] - vehicle.position[2];
            let (push, back) = if is_boat(vehicle.kind) {
                if bounds[1] > vehicle.bounds[1] {
                    continue;
                }
                let dd = xa.abs().max(za.abs());
                if dd < 0.009_999_999_776_482_582 {
                    continue;
                }
                let dd = dd.sqrt();
                let pow = (1.0 / dd).min(1.0) * 0.050_000_000_745_058_06;
                let push = [xa / dd * pow, za / dd * pow];
                (push, (!vehicle.carries).then_some(push))
            } else {
                let dd = xa * xa + za * za;
                if dd < 9.999_999_747_378_752e-5 {
                    continue;
                }
                let dd = dd.sqrt();
                let pow = (1.0 / dd).min(1.0) * 0.100_000_001_490_116_12 * 0.5;
                let push = [xa / dd * pow, za / dd * pow];
                ([push[0] / 4.0, push[1] / 4.0], Some(push))
            };
            if !carries && self_pushable {
                self.base.velocity[0] += push[0];
                self.base.velocity[2] += push[1];
                self.impulse = true;
            }
            if let Some(back) = back {
                shoves.push(Effect::Push {
                    target: vehicle.id,
                    push: [-back[0], 0.0, -back[1]],
                });
            }
        }
        cx.effects.extend(shoves);
        if carries || !self_pushable {
            return crammed;
        }
        if self.crowded {
            crate::crowd::keep_nearest(&mut pushers, self.base.position, |p| p.position);
        }
        let scale = crate::crowd::push_scale(self);
        for pusher in pushers {
            let own_turn = f64::from(u8::from(pusher.listed && !self.phasing));
            let their_turn = f64::from(u8::from(pusher.pushes_back));
            let Some([dx, dz]) = push_away(self.base.position, pusher.position) else {
                continue;
            };
            let turns = (own_turn + their_turn) * scale;
            if turns > 0.0 {
                self.base.velocity[0] -= turns * dx;
                self.base.velocity[2] -= turns * dz;
                self.impulse = true;
            }
        }
        crammed
    }

    /// The vertical speed left after hitting something vertically: a mob
    /// landing on a slime block or bed keeps some of it, reversed, with
    /// the part of the tick's gravity and drag it had not yet moved
    /// through given back.
    fn bounce(&mut self, moved_y: f64, cx: &mut TickContext) -> f64 {
        let current = self.base.velocity[1];
        if current >= 0.0 {
            return 0.0;
        }
        let gravity = self.effective_gravity();
        let on = (cx.block)(self.on_pos(0.500_001, cx));
        let restitution = fall::bounce_restitution(
            on,
            -current,
            gravity,
            false,
            self.attribute(tuff_vanilla_data::registry::Attribute::Bounciness),
        );
        if restitution <= 0.0 {
            return 0.0;
        }
        self.position_sync = true;
        if on.is_some_and(|s| s.block() == Block::ShelfMushroom) && !self.base.silent {
            let pos = self.on_pos(0.500_001, cx);
            let packet = framed(&play::Sound {
                sound_event: SoundEvent::BlockShelfMushroomBounce.id(),
                category: play::sound_category::BLOCKS,
                position: [
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y) + 0.5,
                    f64::from(pos.z) + 0.5,
                ],
                volume: 1.0,
                pitch: 1.0,
                seed: cx.rng.r#gen(),
            });
            cx.effects.push(Effect::ChunkPackets(packets([packet])));
        }
        let portion = moved_y / current;
        let still_air = if self.omnidirectional_air { 0.91 } else { 0.98 };
        let air_drag =
            modified_friction(still_air, self.movement_attributes().air_drag_modifier) as f64;
        let drag = 1.0 + portion * (air_drag - 1.0);
        (portion * gravity - current) * drag * restitution
    }

    /// The driving client moved this mob: only the landing it reported
    /// is settled here, never any travel of its own.
    pub fn land_from_client_move(&mut self, cx: &mut TickContext) {
        let pos = self.on_pos(0.2, cx);
        let state = (cx.block)(pos);
        self.check_fall_damage(0.0, pos, state, cx);
    }

    fn check_fall_damage(
        &mut self,
        moved_y: f64,
        landed_pos: tuff_world_model::BlockPos,
        landed_on: Option<tuff_vanilla_data::BlockState>,
        cx: &mut TickContext,
    ) {
        if self.flying {
            self.base.fall_distance = 0.0;
            return;
        }
        if !self.body.in_water && moved_y < 0.0 {
            self.base.fall_distance -= moved_y as f32 as f64;
        }
        if !self.base.on_ground {
            return;
        }
        if self.base.fall_distance > 0.0 {
            let immune = tuff_core::world::entity_in_tag(self.base.kind, "fall_damage_immune");
            let solid = landed_on.is_some_and(|s| !tuff_world_model::chunk::is_air(s));
            if solid {
                self.landed_fall = Some(self.base.fall_distance);
                cx.effects
                    .push(Effect::Landed(tuff_core::world::region_api::Landing {
                        entity: self.base.id,
                        position: self.base.position,
                        pos: landed_pos,
                        fall_distance: self.base.fall_distance,
                        kind: self.base.kind,
                        size: [f64::from(self.size.width), f64::from(self.size.height)],
                        living: true,
                        gamemaster: false,
                    }));
            }
            let impact = solid
                .then(|| fall::impact(landed_on, self.base.fall_distance, false))
                .flatten();
            if let Some(impact) = impact.filter(|_| !immune) {
                let damage = impact.damage(
                    self.attribute(tuff_vanilla_data::registry::Attribute::SafeFallDistance),
                    self.attribute(tuff_vanilla_data::registry::Attribute::FallDamageMultiplier),
                );
                if damage > 0 {
                    let sound = if damage > 4 {
                        SoundEvent::EntityGenericBigFall
                    } else {
                        SoundEvent::EntityGenericSmallFall
                    };
                    if !self.base.silent {
                        let honey = landed_on.is_some_and(|s| s.block() == Block::HoneyBlock);
                        let (sound, volume, pitch) = if honey {
                            (SoundEvent::BlockHoneyBlockFall, 0.5, 0.75)
                        } else {
                            (sound, 1.0, 1.0)
                        };
                        let packet = framed(&play::Sound {
                            sound_event: sound.id(),
                            category: play::sound_category::NEUTRAL,
                            position: self.base.position,
                            volume,
                            pitch,
                            seed: cx.rng.r#gen(),
                        });
                        cx.effects.push(Effect::ChunkPackets(packets([packet])));
                    }
                    self.fall_damage = Some((damage as f32, impact.kind));
                }
            }
        }
        self.base.fall_distance = 0.0;
    }
}

fn approx_equal(a: f64, b: f64) -> bool {
    (b - a).abs() < 1.0e-5
}

fn fluid_falling(gravity: f64, falling: bool, movement: [f64; 3]) -> [f64; 3] {
    if gravity == 0.0 {
        return movement;
    }
    let y = if falling
        && (movement[1] - 0.005).abs() >= 0.003
        && (movement[1] - gravity / 16.0).abs() < 0.003
    {
        -0.003
    } else {
        movement[1] - gravity / 16.0
    };
    [movement[0], y, movement[2]]
}

fn slime_scale(vertical: f64) -> Option<f64> {
    let lift = vertical.abs();
    (lift < 0.1).then_some(0.4 + lift * 0.2)
}

#[cfg(test)]
mod slime_tests {
    use super::slime_scale;

    #[tuff_core::watchdog]
    #[test]
    fn slime_slows_only_slow_vertical_motion() {
        assert_eq!(slime_scale(0.0), Some(0.4));
        assert!((slime_scale(-0.05).unwrap() - 0.41).abs() < 1e-12);
        assert_eq!(slime_scale(0.1), None);
        assert_eq!(slime_scale(-0.5), None);
    }
}

#[cfg(test)]
mod push_tests {
    use super::push_away;

    #[tuff_core::watchdog]
    #[test]
    fn one_push_matches_vanilla_strength_and_direction() {
        let [dx, dz] = push_away([0.0, 0.0, 0.0], [0.5, 0.0, 0.0]).unwrap();
        assert!((dx - 0.035_355_339_586_442_36).abs() < 1e-12, "{dx}");
        assert_eq!(dz, 0.0);
        let [dx, dz] = push_away([0.0, 0.0, 0.0], [0.04, 0.0, -0.02]).unwrap();
        assert!(
            (dx - 0.01).abs() < 1e-9 && (dz + 0.005).abs() < 1e-9,
            "{dx} {dz}"
        );
        assert!(push_away([0.0; 3], [0.005, 9.0, -0.005]).is_none());
    }
}
