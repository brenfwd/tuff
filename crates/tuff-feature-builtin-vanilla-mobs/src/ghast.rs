//! Ghasts: they float about the nether, face where they drift or whom they
//! hunt, and shoot exploding fireballs at players within four blocks of
//! their height. A fireball a player sends back kills them outright.
//! Happy ghasts are a peaceful, ageable relative that never shoots and is
//! tempted (and bred) by snowballs; a grown one wearing a harness carries
//! four riders and flies where its driving player looks.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::entity::Heightmap;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::control::{MoveStyle, Operation};
use super::goal::{Flags, Goal, GoalSelector};
use super::monster;
use super::physics::block_pos;
use super::target::NearestAttackablePlayer;
use super::{Damage, MobCore, MobSounds, OwnAirRules, Species, data};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{
    Culprit, Effect, EntityInteraction, InteractOutcome, NearbyPlayer, TickContext, take_number,
};
use tuff_core::world::messages::HandEffect;
use tuff_core::world::projectile::{self, Launch, Owner, Shot};
use tuff_core::world::{framed, packets};
use tuff_nbt::Compound;

const CHARGING: u8 = data::BABY;
const WARNING_EVENT: i32 = 1015;
const SHOOT_EVENT: i32 = 1016;
const SOUND_VOLUME: f32 = 5.0;
const REFLECTED_DAMAGE: f32 = 1000.0;
const SIGHT: f64 = 64.0;
const WANDER: f32 = 16.0;

fn within_height(mob: &MobCore, player: &NearbyPlayer) -> bool {
    (player.position[1] - mob.base.position[1]).abs() <= 4.0
}

/// The unit vector it looks along, from its body's yaw and pitch.
pub(super) fn view_vector(mob: &MobCore) -> [f64; 3] {
    let (yaw, pitch) = (mob.base.yaw.to_radians(), mob.base.pitch.to_radians());
    [
        f64::from(-yaw.sin() * pitch.cos()),
        f64::from(-pitch.sin()),
        f64::from(yaw.cos() * pitch.cos()),
    ]
}

/// Picks a point up to 16 blocks away on each axis whenever it has none
/// or is there or too far from it; over open ground below its height it
/// flies down rather than up.
#[derive(Debug, Default)]
pub struct RandomFloatAround;

impl Goal for RandomFloatAround {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        if !mob.movement.has_wanted() {
            return true;
        }
        let distance = mob.distance_sq(mob.movement.wanted);
        !(1.0..=3600.0).contains(&distance)
    }

    fn can_continue(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        false
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let [x, y, z] = mob.base.position;
        for _ in 0..64 {
            let mut offset = || f64::from((cx.rng.r#gen::<f32>() * 2.0 - 1.0) * WANDER);
            let mut target = [x + offset(), y + offset(), z + offset()];
            let pos = block_pos(target);
            if !mob.within_home(pos) {
                continue;
            }
            let top = super::spawning::heightmap_top(
                Heightmap::MotionBlocking,
                pos.x,
                pos.z,
                cx.max_y,
                cx.min_y,
                cx.block,
            );
            if top < pos.y && top > cx.min_y {
                target[1] = y - (y - target[1]).abs();
            }
            mob.movement.set_wanted(target, 1.0);
            return;
        }
    }
}

/// Faces its drift, or its target while within 64 blocks.
#[derive(Debug, Default)]
pub struct FaceMovement;

impl Goal for FaceMovement {
    fn flags(&self) -> Flags {
        Flags::LOOK
    }

    fn can_use(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        true
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        match mob.target_view(cx) {
            None => {
                let [vx, _, vz] = mob.base.velocity;
                mob.base.yaw = -(vx.atan2(vz) as f32).to_degrees();
            }
            Some(target) if mob.distance_sq(target.position) < SIGHT * SIGHT => {
                let dx = target.position[0] - mob.base.position[0];
                let dz = target.position[2] - mob.base.position[2];
                mob.base.yaw = -(dx.atan2(dz) as f32).to_degrees();
            }
            Some(_) => return,
        }
        mob.body_yaw = mob.base.yaw;
    }
}

/// Charges for a second while it sees its target, warns halfway, then
/// shoots and waits two seconds.
#[derive(Debug, Default)]
pub struct ShootFireball {
    charge: i32,
}

impl Goal for ShootFireball {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.target.is_some()
    }

    fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.charge = 0;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.signal = 0;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        let level_event = |event: i32, mob: &MobCore, cx: &mut TickContext| {
            if !mob.base.silent {
                let packet = framed(&play::LevelEvent {
                    event,
                    position: block_pos(mob.base.position),
                    data: 0,
                    global: false,
                });
                cx.effects.push(Effect::ChunkPackets(packets([packet])));
            }
        };
        if mob.distance_sq(target.position) < SIGHT * SIGHT && mob.has_line_of_sight(&target, cx) {
            self.charge += 1;
            if self.charge == 10 {
                level_event(WARNING_EVENT, mob, cx);
            }
            if self.charge == 20 {
                let view = view_vector(mob);
                let [x, y, z] = mob.base.position;
                let middle = y + f64::from(mob.size.height) * 0.5;
                let target_middle =
                    target.position[1] + (target.bounds[4] - target.bounds[1]) * 0.5;
                let direction = [
                    target.position[0] - (x + view[0] * 4.0),
                    target_middle - (0.5 + middle),
                    target.position[2] - (z + view[2] * 4.0),
                ];
                level_event(SHOOT_EVENT, mob, cx);
                let owner = Owner::of(Culprit::entity(&mob.base), None);
                let start = [x + view[0] * 4.0, middle + 0.5, z + view[2] * 4.0];
                let id = cx.shared.next_entity_id();
                let shot = Shot::LargeFireball {
                    direction,
                    explosion_power: mob.explosion_power,
                };
                projectile::launch(
                    Launch {
                        id,
                        position: start,
                        owner,
                        shot,
                    },
                    cx,
                );
                self.charge = -40;
            }
        } else if self.charge > 0 {
            self.charge -= 1;
        }
        mob.signal = i32::from(self.charge > 10);
    }
}

#[derive(Debug)]
pub struct Ghast {
    charging: bool,
}

impl Species for Ghast {
    fn create(_rng: &mut StdRng) -> Self {
        Self { charging: false }
    }

    fn configure(core: &mut MobCore) {
        core.hostile = true;
        core.xp_reward = monster::MONSTER_XP;
        core.flying = true;
        core.explosion_power = 1;
        core.move_style = MoveStyle::Float { duration: 0 };
        let mut goals = GoalSelector::default();
        goals.add(
            1,
            NearestAttackablePlayer::with_interval(10, true, Some(within_height)),
        );
        goals.add(5, RandomFloatAround);
        goals.add(7, FaceMovement);
        goals.add(7, ShootFireball::default());
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        core.explosion_power = take_number(nbt, "ExplosionPower").map_or(1, |v| v as i8 as i32);
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        out.insert("ExplosionPower", core.explosion_power as i8);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(CHARGING, MetadataValue::Boolean(self.charging))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            step: None,
            volume: SOUND_VOLUME,
            ..monster::sounds(
                SoundEvent::EntityGhastAmbient,
                SoundEvent::EntityGhastHurt,
                SoundEvent::EntityGhastDeath,
                None,
            )
        }
    }

    fn damage_override(&self, _core: &MobCore, damage: &Damage) -> Option<f32> {
        let reflected = damage.projectile == Some(EntityType::Fireball) && damage.player.is_some();
        reflected.then_some(REFLECTED_DAMAGE)
    }

    fn ai_step(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        let charging = core.signal > 0;
        if charging != self.charging {
            self.charging = charging;
            core.changed(CHARGING);
        }
        if core.movement.operation == Operation::Wait {
            core.body.zza = 0.0;
        }
    }
}

/// A happy ghast: peaceful, floats and drifts the same way a ghast does,
/// never shoots, and breeds/is tempted by snowballs. A grown one wearing a
/// harness carries up to four riders; the first, a player, flies it where
/// they look. It hovers still for a while after a rider gets off or while
/// a player stands on top of it.
#[derive(Debug, Default)]
pub struct HappyGhast {
    still_timeout: i32,
    riders: usize,
    /// Ticks it still shows it holds a lead tied at four corners.
    leash_holder_time: i32,
}

const IS_LEASH_HOLDER: u8 = data::SPECIES;
const STAYS_STILL: u8 = data::SPECIES + 1;
const SEATS: usize = 4;
const MAX_STILL_TIMEOUT: i32 = 10;
const STILL_TIMEOUT_GRACE: u64 = 60;
const FLYING_SPEED: f32 = 0.05;
const RIDDEN_INPUT_SCALE: f32 = 3.9;
const TURN_RATE: f32 = 0.08;
const CLOUD_BOTTOM: f64 = 192.33;
const CLOUD_THICKNESS: f64 = 4.0;

fn in_clouds(core: &MobCore, cx: &TickContext) -> bool {
    let y = core.base.position[1];
    cx.shared.dimension(cx.dimension).key == tuff_core::world::dimension::OVERWORLD
        && y + f64::from(core.size.height) >= CLOUD_BOTTOM
        && y <= CLOUD_BOTTOM + CLOUD_THICKNESS
}

fn precipitating(core: &MobCore, cx: &TickContext) -> bool {
    let pos = block_pos(core.base.position);
    cx.raining
        && core.can_see_sky(pos, cx)
        && (cx.biome)(pos)
            .and_then(tuff_vanilla_data::biome::Biome::get)
            .is_some_and(|biome| biome.has_precipitation)
}

const FAST_HEALING_TICKS: u64 = 20;
/// Ticks it keeps showing it holds a four-cornered lead after the last pull.
const LEASH_HOLDER_TICKS: i32 = 5;
const SLOW_HEALING_TICKS: u64 = 600;

fn happy_ghast_food(item: tuff_vanilla_data::Item) -> bool {
    tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::HAPPY_GHAST_FOOD, item)
}

fn happy_ghast_tempts(core: &MobCore, item: tuff_vanilla_data::Item) -> bool {
    if core.is_baby() || harnessed(core) {
        happy_ghast_food(item)
    } else {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::item::key::HAPPY_GHAST_TEMPT_ITEMS,
            item,
        )
    }
}

/// Runs the wrapped goal only while the mob is a baby.
#[derive(Debug)]
struct BabyOnly<G>(G);

impl<G: Goal> Goal for BabyOnly<G> {
    fn flags(&self) -> Flags {
        self.0.flags()
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.is_baby() && self.0.can_use(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.is_baby() && self.0.can_continue(mob, cx)
    }

    fn interruptable(&self) -> bool {
        self.0.interruptable()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.0.start(mob, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.0.stop(mob, cx);
    }

    fn every_tick(&self) -> bool {
        self.0.every_tick()
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.0.tick(mob, cx);
    }
}

fn is_harness(stack: &tuff_item::item::ItemStack) -> bool {
    stack.item.name().ends_with("_harness")
}

fn harnessed(core: &MobCore) -> bool {
    core.equipment
        .get(tuff_vanilla_data::item::EquipmentSlot::Body)
        .is_some()
}

fn wrap_degrees(angle: f32) -> f32 {
    let wrapped = angle % 360.0;
    if wrapped >= 180.0 {
        wrapped - 360.0
    } else if wrapped < -180.0 {
        wrapped + 360.0
    } else {
        wrapped
    }
}

/// The input a driving player gives it: sideways as they strafe, along
/// their look as they press forward (half as fast backward), and up while
/// they hold jump.
pub fn ridden_input(input: &tuff_core::world::entity::RiderInput) -> [f32; 3] {
    let strafe = -input.strafe;
    let (mut up, mut forward) = (0.0, 0.0);
    if input.forward != 0.0 {
        let pitch = input.pitch.to_radians();
        let (mut along, mut climb) = (pitch.cos(), -pitch.sin());
        if input.forward < 0.0 {
            along *= -0.5;
            climb *= -0.5;
        }
        up = climb;
        forward = along;
    }
    if input.jumping {
        up += 0.5;
    }
    let scale = RIDDEN_INPUT_SCALE * FLYING_SPEED;
    [strafe * scale, up * scale, forward * scale]
}

impl HappyGhast {
    /// Keeps a home around where it floats when nothing leads or rides it:
    /// 64 blocks for a grown one without a harness, else 32.
    fn keep_home(&self, core: &mut MobCore) {
        if core.leash.is_some()
            || !core.base.riders.is_empty()
            || !core.base.entity_riders.is_empty()
        {
            return;
        }
        let radius = if !core.is_baby() && !harnessed(core) {
            64
        } else {
            32
        };
        let here = block_pos(core.base.position);
        if let Some(home) = core.home {
            let d = [
                home.pos.x - here.x,
                home.pos.y - here.y,
                home.pos.z - here.z,
            ]
            .map(f64::from);
            let reach = f64::from(radius + 16);
            if d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < reach * reach && home.radius == radius {
                return;
            }
        }
        core.home = Some(super::Home { pos: here, radius });
    }

    fn on_still_timeout(&self) -> bool {
        self.still_timeout > 0
    }

    fn set_still_timeout(&mut self, core: &mut MobCore, ticks: i32) {
        if (self.still_timeout > 0) != (ticks > 0) {
            core.changed(STAYS_STILL);
        }
        self.still_timeout = ticks;
    }

    /// A player (not riding it) stands on its back.
    fn player_above(&self, core: &MobCore, cx: &TickContext) -> bool {
        let b = core.bounds();
        let height = b[4] - b[1];
        let area = [
            b[0] - 1.0,
            b[4] - 1.0e-5,
            b[2] - 1.0,
            b[3] + 1.0,
            b[4] + height / 2.0,
            b[5] + 1.0,
        ];
        cx.players.iter().any(|p| {
            !core.base.riders.iter().any(|r| r.id == p.id)
                && (0..3).all(|i| p.position[i] >= area[i] && p.position[i] < area[i + 3])
        })
    }
}

impl Species for HappyGhast {
    fn track_shape(&self, core: &MobCore) -> Option<tuff_core::world::messages::Shape> {
        core.is_baby().then_some(tuff_core::world::messages::Shape {
            non_solid: true,
            offset: None,
        })
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn follows_lead(&self) -> bool {
        false
    }

    fn lead_pulled(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        core.movement.operation = Operation::Wait;
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.flying = true;
        core.flying_speed = Some(FLYING_SPEED * 5.0 / 3.0);
        core.move_style = MoveStyle::Float { duration: 0 };
        let mut goals = GoalSelector::default();
        goals.add(0, BabyOnly(super::goals::Float));
        goals.add(1, BabyOnly(super::goals::Panic::new(2.0)));
        goals.add(
            3,
            super::goals::Tempt::for_mob(1.0, happy_ghast_tempts)
                .baby_speed(1.25)
                .without_pathfinding(7.0),
        );
        goals.add(4, super::goals::Breed::new(1.0));
        goals.add(5, RandomFloatAround);
        goals.add(6, super::goals::FollowParent::new(1.1));
        goals.add(7, FaceMovement);
        goals.add(8, super::goals::RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.still_timeout = take_number(nbt, "still_timeout").map_or(0, |v| v as i32);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("still_timeout", self.still_timeout);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (
                IS_LEASH_HOLDER,
                MetadataValue::Boolean(self.leash_holder_time > 0),
            ),
            (STAYS_STILL, MetadataValue::Boolean(self.on_still_timeout())),
        ]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let baby = core.is_baby();
        MobSounds {
            step: None,
            volume: if baby { 1.0 } else { 4.0 },
            ..if baby {
                monster::sounds(
                    SoundEvent::EntityGhastlingAmbient,
                    SoundEvent::EntityGhastlingHurt,
                    SoundEvent::EntityGhastlingDeath,
                    None,
                )
            } else {
                monster::sounds(
                    SoundEvent::EntityHappyGhastAmbient,
                    SoundEvent::EntityHappyGhastHurt,
                    SoundEvent::EntityHappyGhastDeath,
                    None,
                )
            }
        }
    }

    fn is_food(&self, stack: &tuff_item::item::ItemStack) -> bool {
        happy_ghast_food(stack.item)
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    /// The game only ever breathes underwater unconditionally as a baby;
    /// an adult falls back to the standard rule. That baby distinction
    /// needs per-tick state `air_rules` does not have access to, so this
    /// approximates it as never drowning at any age.
    fn air_rules(&self) -> Option<OwnAirRules> {
        Some(OwnAirRules::NEVER_LOSES_AIR)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let riders = core.base.riders.len() + core.base.entity_riders.len();
        if riders > self.riders {
            if self.riders == 0 {
                core.sound(SoundEvent::EntityHappyGhastHarnessGogglesDown, 1.0, 1.0, cx);
            }
            if !self.player_above(core, cx) {
                self.set_still_timeout(core, 0);
            } else if self.still_timeout > MAX_STILL_TIMEOUT {
                self.set_still_timeout(core, MAX_STILL_TIMEOUT);
            }
        } else if riders < self.riders {
            self.set_still_timeout(core, MAX_STILL_TIMEOUT);
            if riders == 0 {
                core.sound(SoundEvent::EntityHappyGhastHarnessGogglesUp, 1.0, 1.0, cx);
            }
        }
        self.riders = riders;
        if self.still_timeout > 0 {
            let next = if core.tick_count > STILL_TIMEOUT_GRACE {
                self.still_timeout - 1
            } else {
                self.still_timeout
            };
            self.set_still_timeout(core, next);
        }
        if self.player_above(core, cx) {
            self.set_still_timeout(core, MAX_STILL_TIMEOUT);
        }
        let held = cx
            .state
            .get::<super::leash::Leashes>()
            .holds_quad(core.base.id, cx.tick);
        let was = self.leash_holder_time > 0;
        if held {
            self.leash_holder_time = LEASH_HOLDER_TICKS;
        } else if self.leash_holder_time > 0 {
            self.leash_holder_time -= 1;
        }
        if was != (self.leash_holder_time > 0) {
            core.changed(IS_LEASH_HOLDER);
        }
        self.keep_home(core);
        if core.is_alive() && core.death_time == 0 && core.health < core.max_health() {
            let fast = in_clouds(core, cx) || precipitating(core, cx);
            let every = if fast {
                FAST_HEALING_TICKS
            } else {
                SLOW_HEALING_TICKS
            };
            if core.tick_count.is_multiple_of(every) {
                core.heal(1.0);
            }
        }
    }

    fn immobile(&self, core: &MobCore) -> bool {
        self.on_still_timeout() && core.base.riders.is_empty()
    }

    fn ambient_sound_interval(&self) -> i32 {
        super::DEFAULT_AMBIENT_INTERVAL * if self.riders > 0 { 6 } else { 1 }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        if core.is_baby() {
            return None;
        }
        let stack = interaction.stack.as_deref()?;
        if is_harness(stack) && !harnessed(core) {
            core.equipment.set(
                tuff_vanilla_data::item::EquipmentSlot::Body,
                Some(tuff_item::item::ItemStack::mint_like(
                    stack,
                    1,
                    tuff_item::item::MintReason::Transform,
                )),
            );
            core.sound(SoundEvent::EntityHappyGhastEquip, 1.0, 1.0, cx);
            return Some(HandEffect::Consume(1));
        }
        None
    }

    fn seats(&self, core: &MobCore) -> usize {
        if harnessed(core) && !core.is_baby() {
            SEATS
        } else {
            0
        }
    }

    fn accepts_rider(&self, core: &MobCore) -> bool {
        !core.is_baby() && core.is_alive()
    }

    fn mount_refused(&self, core: &MobCore, stack: Option<&tuff_item::item::ItemStack>) -> bool {
        stack.is_some_and(|stack| {
            happy_ghast_food(stack.item)
                || stack.item == tuff_vanilla_data::Item::Shears
                || stack.item == tuff_vanilla_data::Item::Lead
                || (is_harness(stack) && !harnessed(core))
        })
    }

    fn drivable(&self, core: &MobCore) -> bool {
        harnessed(core) && !core.is_baby() && !self.on_still_timeout()
    }

    fn flying_vehicle(&self, core: &MobCore) -> bool {
        !core.is_baby()
    }

    fn hops_when_ridden(&self) -> bool {
        false
    }

    fn ridden_speed(&self, _core: &MobCore) -> Option<tuff_core::world::entity::RiddenSpeed> {
        let acceleration = f64::from(FLYING_SPEED * 5.0 / 3.0 * RIDDEN_INPUT_SCALE * FLYING_SPEED);
        Some(tuff_core::world::entity::RiddenSpeed {
            speed: acceleration * 2.0 / 0.09,
            leap: 0.0,
            ..Default::default()
        })
    }

    fn ride(
        &mut self,
        core: &mut MobCore,
        input: tuff_core::world::entity::RiderInput,
        _cx: &mut TickContext,
    ) -> bool {
        let yaw = core.base.yaw + wrap_degrees(input.yaw - core.base.yaw) * TURN_RATE;
        core.base.yaw = yaw;
        core.base.pitch = input.pitch * 0.5;
        core.head_yaw = yaw;
        core.body_yaw = yaw;
        let [x, y, z] = ridden_input(&input);
        core.body.xxa = x;
        core.body.yya = y;
        core.body.zza = z;
        true
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Ghast, super::load::<Ghast>);
    registry.register_spawn_rule(EntityType::Ghast, spawn_rule);
    registry.register(EntityType::HappyGhast, super::load::<HappyGhast>);
}

/// One in twenty tries, on something to stand on.
fn spawn_rule(
    kind: EntityType,
    level: &super::spawning::SpawnLevel,
    pos: tuff_world_model::BlockPos,
    reason: super::spawning::SpawnReason,
    rng: &mut StdRng,
) -> bool {
    rng.gen_range(0..20) == 0 && super::spawning::mob_rule(kind, level, pos, reason, rng)
}

#[cfg(test)]
mod tests {
    #[tuff_core::watchdog]
    #[test]
    fn a_projectile_hitting_a_fireball_sends_it_on_as_its_own() {
        use tuff_core::world::entity::{EntityHit, RegionEntity};
        use tuff_core::world::projectile::Owner;
        use tuff_feature_builtin_vanilla_combat::projectile::fireball::LargeFireball;
        crate::tests::with_context(|_, cx| {
            let mut fireball =
                LargeFireball::new(9, [10.0, 70.0, 10.0], [0.0, 0.0, -1.0], Owner::default(), 1);
            let mut arrow = EntityHit::new(6.0, &tuff_vanilla_data::damage_type::ARROW);
            let shooter = tuff_core::world::entity::Culprit::player(4, 0xabc, "Shooter");
            arrow.attacker = Some(shooter);
            arrow.projectile = Some(EntityType::Arrow);
            arrow.position = Some([8.0, 70.0, 10.0]);
            let outcome = fireball.hit(&arrow, cx).expect("a projectile deflects it");
            assert!(!outcome.hurt);
            assert_eq!(fireball.base().velocity, [1.0, 0.0, 0.0]);
            assert!(fireball.take_impulse());
            let mut melee = arrow.clone();
            melee.projectile = None;
            assert!(fireball.hit(&melee, cx).is_none());
        });
    }

    use super::*;

    fn happy_ghast() -> crate::Mob<HappyGhast> {
        let base = tuff_core::world::entity::EntityBase::new(
            EntityType::HappyGhast,
            1,
            1,
            [0.5, 70.0, 0.5],
            [0.0; 3],
        );
        let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
        crate::Mob::<HappyGhast>::new(base, &mut rng)
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_happy_ghast_is_tempted_by_harnesses_until_it_wears_one() {
        crate::tests::with_context(|core, _| {
            core.base.kind = EntityType::HappyGhast;
            assert!(happy_ghast_tempts(
                core,
                tuff_vanilla_data::Item::WhiteHarness
            ));
            assert!(happy_ghast_tempts(core, tuff_vanilla_data::Item::Snowball));
            core.baby = true;
            assert!(!happy_ghast_tempts(
                core,
                tuff_vanilla_data::Item::WhiteHarness
            ));
            assert!(happy_ghast_tempts(core, tuff_vanilla_data::Item::Snowball));
        });
    }

    fn click(stack: tuff_vanilla_data::Item) -> EntityInteraction {
        EntityInteraction {
            player: 7,
            player_uuid: 7,
            position: [0.0; 3],
            location: [0.0; 3],
            stack: Some(tuff_item::item::ItemSnapshot::describe(
                stack,
                1,
                Default::default(),
            )),
            off_hand: false,
            creative: false,
            sneaking: false,
            riding: false,
            hero: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_happy_ghast_heals_fast_only_in_clouds_or_local_precipitation() {
        crate::tests::with_context(|core, cx| {
            core.base.position = [0.5, 70.0, 0.5];
            assert!(!in_clouds(core, cx));
            assert!(!precipitating(core, cx));
            cx.raining = true;
            assert!(precipitating(core, cx));
            core.base.position[1] = 195.0;
            assert!(in_clouds(core, cx));
            core.base.position[1] = 200.0;
            assert!(!in_clouds(core, cx));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_harness_gives_a_grown_happy_ghast_four_seats() {
        use tuff_core::world::entity::RegionEntity;
        crate::tests::with_context(|_, cx| {
            let mut ghast = happy_ghast();
            assert_eq!(ghast.seats(), 0);
            let outcome = ghast.interact(&click(tuff_vanilla_data::Item::BlueHarness), cx);
            assert_eq!(outcome, Some(HandEffect::Consume(1)));
            assert_eq!(ghast.seats(), 4);
            assert!(ghast.rideable_by_input() && ghast.flying_vehicle());
            assert!(
                ghast.mount_refused_by(Some(&tuff_item::item::ItemStack::mint(
                    tuff_vanilla_data::Item::Snowball,
                    1,
                    tuff_item::item::MintReason::Test
                )))
            );
            assert_eq!(
                ghast.interact(&click(tuff_vanilla_data::Item::Shears), cx),
                Some(HandEffect::Damage(1))
            );
            assert_eq!(ghast.seats(), 0, "sheared off");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn its_rider_flies_it_along_their_look() {
        use tuff_core::world::entity::{RegionEntity, Rider, RiderInput};
        let input = RiderInput {
            forward: 1.0,
            pitch: -30.0,
            ..RiderInput::default()
        };
        let [x, up, forward] = ridden_input(&input);
        assert_eq!(x, 0.0);
        assert!(up > 0.0 && forward > up, "climbs along the look");
        let back = ridden_input(&RiderInput {
            forward: -1.0,
            ..RiderInput::default()
        });
        assert!(
            (back[2] + forward / 0.866 * 0.5).abs() < 1.0e-2,
            "half speed backward"
        );
        let jump = ridden_input(&RiderInput {
            jumping: true,
            ..RiderInput::default()
        });
        assert!(jump[1] > 0.0);

        crate::tests::with_context(|_, cx| {
            let mut ghast = happy_ghast();
            ghast.interact(&click(tuff_vanilla_data::Item::WhiteHarness), cx);
            let (handle, _events) = tuff_core::world::messages::PlayerHandle::new(7);
            ghast.core.base.riders.push(Rider {
                id: 7,
                uuid: 7,
                handle,
            });
            ghast.steer_with_input(input);
            for _ in 0..40 {
                ghast.tick(cx);
            }
            let before = happy_ghast().core.base.position;
            assert_eq!(
                ghast.core.base.position, before,
                "its driver's client flies it, never the region"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn happy_ghast_data_indices_match_the_dumped_layout() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let layout = EntityType::HappyGhast.data_layout();
        assert_eq!(
            layout.len(),
            data::AGE_LOCKED as usize + 3,
            "happy ghast is ageable and adds two booleans of its own"
        );
        assert_eq!(
            layout[data::BABY as usize..],
            [boolean, boolean, boolean, boolean]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_free_happy_ghast_retries_its_home_once_it_drifts_out_of_reach() {
        let mut ghast = happy_ghast();
        let here = block_pos(ghast.core.base.position);
        let (species, core) = (&ghast.species, &mut ghast.core);
        species.keep_home(core);
        let home = core.home.expect("set while free");
        assert_eq!(home.pos, here);
        core.base.position = [0.5, 70.0, 0.5 + 200.0];
        species.keep_home(core);
        assert_ne!(core.home.expect("moved").pos, home.pos);
        let moved = core.home.expect("moved").pos;
        core.base.position = [0.5, 70.0, 0.5 + 400.0];
        core.leash = Some(tuff_core::world::leash::Leash::pending(
            tuff_core::world::leash::Anchor::Entity(7),
        ));
        species.keep_home(core);
        assert_eq!(core.home.expect("kept").pos, moved, "a lead keeps it");
    }
}
