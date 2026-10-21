//! Camels: wander, breed on cacti, take a saddle and two riders, and dash
//! when the driver releases a charged jump. A camel sits down and stands up
//! now and then; while sitting or changing pose it refuses to move and
//! cannot dash, and it stands at once when hurt, in water, tempted by food
//! or when its driver pushes forward.
//!
//! Camel husks are the undead kind: monsters that never breed or grow up,
//! despawn unless a player has touched them, are tempted by rabbit feet,
//! and are steered by the husk that spawns riding one.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::entity::Pose;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Item, tag};

use super::goal::GoalSelector;
use super::goals::{
    Breed, Float, FollowParent, LookAtPlayer, Panic, RandomLookAround, RandomStroll, Tempt,
};
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::{MetadataValue, metadata};
use tuff_core::world::entity::{EntityInteraction, InteractOutcome, RiderInput, TickContext};
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;
use tuff_vanilla_data::registry::Attribute;

const IS_DASHING: u8 = data::SPECIES + 1;
const LAST_POSE_CHANGE_TICK: u8 = data::SPECIES + 2;

fn camel_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::CAMEL_FOOD, item)
}

fn camel_husk_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::CAMEL_HUSK_FOOD, item)
}

struct Sounds {
    ambient: SoundEvent,
    hurt: SoundEvent,
    death: SoundEvent,
    step: SoundEvent,
    eat: SoundEvent,
    sit: SoundEvent,
    stand: SoundEvent,
    dash: SoundEvent,
    dash_ready: SoundEvent,
    saddle: SoundEvent,
}

const CAMEL_SOUNDS: Sounds = Sounds {
    ambient: SoundEvent::EntityCamelAmbient,
    hurt: SoundEvent::EntityCamelHurt,
    death: SoundEvent::EntityCamelDeath,
    step: SoundEvent::EntityCamelStep,
    eat: SoundEvent::EntityCamelEat,
    sit: SoundEvent::EntityCamelSit,
    stand: SoundEvent::EntityCamelStand,
    dash: SoundEvent::EntityCamelDash,
    dash_ready: SoundEvent::EntityCamelDashReady,
    saddle: SoundEvent::EntityCamelSaddle,
};

const CAMEL_HUSK_SOUNDS: Sounds = Sounds {
    ambient: SoundEvent::EntityCamelHuskAmbient,
    hurt: SoundEvent::EntityCamelHuskHurt,
    death: SoundEvent::EntityCamelHuskDeath,
    step: SoundEvent::EntityCamelHuskStep,
    eat: SoundEvent::EntityCamelHuskEat,
    sit: SoundEvent::EntityCamelHuskSit,
    stand: SoundEvent::EntityCamelHuskStand,
    dash: SoundEvent::EntityCamelHuskDash,
    dash_ready: SoundEvent::EntityCamelHuskDashReady,
    saddle: SoundEvent::EntityCamelHuskSaddle,
};

const SIT_TICKS: u64 = 40;
const STAND_TICKS: u64 = 52;
const MIN_POSE_TICKS: u64 = 400;
const PICK_REST: std::ops::Range<u64> = 30..60;
const PICK_MOVE: u64 = 20;
const TEMPT_RANGE_SQ: f64 = 100.0;
const DASH_COOLDOWN: i32 = 55;
const DASH_HORIZONTAL: f64 = 22.2222;
const DASH_VERTICAL: f64 = 1.4285;
const RUNNING_BONUS: f64 = 0.1;

/// A camel, or with `HUSK` a camel husk.
#[derive(Debug, Default)]
pub struct Camel<const HUSK: bool = false> {
    dash_cooldown: i32,
    dashing: bool,
    sitting: bool,
    /// The tick of the last pose change; `None` once fully stood up.
    changed_at: Option<u64>,
    now: u64,
    next_pick: u64,
}

/// The undead camel.
pub type CamelHusk = Camel<true>;

impl<const HUSK: bool> Camel<HUSK> {
    const SOUNDS: &'static Sounds = if HUSK {
        &CAMEL_HUSK_SOUNDS
    } else {
        &CAMEL_SOUNDS
    };

    fn food(item: Item) -> bool {
        if HUSK {
            camel_husk_food(item)
        } else {
            camel_food(item)
        }
    }

    fn pose_time(&self) -> u64 {
        self.changed_at
            .map_or(u64::MAX, |at| self.now.saturating_sub(at))
    }

    fn in_transition(&self) -> bool {
        self.pose_time() < if self.sitting { SIT_TICKS } else { STAND_TICKS }
    }

    fn refuses_to_move(&self) -> bool {
        self.sitting || self.in_transition()
    }

    fn pose_changed(core: &mut MobCore) {
        core.changed(metadata::POSE);
        core.changed(LAST_POSE_CHANGE_TICK);
    }

    fn sit_down(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.sitting {
            return;
        }
        core.sound(Self::SOUNDS.sit, 1.0, 1.0, cx);
        self.sitting = true;
        self.changed_at = Some(cx.tick);
        Self::pose_changed(core);
    }

    fn stand_up(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !self.sitting {
            return;
        }
        core.sound(Self::SOUNDS.stand, 1.0, 1.0, cx);
        self.sitting = false;
        self.changed_at = Some(cx.tick);
        Self::pose_changed(core);
    }

    fn stand_up_instantly(&mut self, core: &mut MobCore) {
        if self.sitting || self.changed_at.is_some() {
            self.sitting = false;
            self.changed_at = None;
            Self::pose_changed(core);
        }
    }

    fn last_pose_tick(&self) -> i64 {
        let at = self.changed_at.unwrap_or(0) as i64;
        if self.sitting { -at } else { at }
    }

    fn tempted(core: &MobCore, cx: &TickContext) -> bool {
        cx.players.iter().any(|p| {
            tuff_core::world::distance_sq(p.position, core.base.position) <= TEMPT_RANGE_SQ
                && p.held.iter().flatten().any(|item| Self::food(*item))
        })
    }
}

impl<const HUSK: bool> Species for Camel<HUSK> {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn seats(&self, core: &MobCore) -> usize {
        if super::equine::saddled(core) { 2 } else { 0 }
    }

    fn accepts_rider(&self, core: &MobCore) -> bool {
        !core.is_baby()
    }

    fn drivable(&self, core: &MobCore) -> bool {
        super::equine::saddled(core)
    }

    fn drive_input(&self, _core: &MobCore, input: RiderInput) -> (f32, f32) {
        if self.refuses_to_move() {
            return (0.0, 0.0);
        }
        super::equine::drive_input(input)
    }

    fn immobile(&self, core: &MobCore) -> bool {
        self.refuses_to_move() && core.base.riders.is_empty()
    }

    fn hops_when_ridden(&self) -> bool {
        false
    }

    fn ridden_speed(&self, core: &MobCore) -> Option<tuff_core::world::entity::RiddenSpeed> {
        let speed = core.attribute(Attribute::MovementSpeed);
        Some(tuff_core::world::entity::RiddenSpeed {
            speed: speed + RUNNING_BONUS,
            leap: DASH_HORIZONTAL * speed,
            launch: DASH_VERTICAL * core.attribute(Attribute::JumpStrength),
            ..Default::default()
        })
    }

    fn ride_speed(&mut self, core: &mut MobCore, input: &RiderInput, cx: &mut TickContext) -> f32 {
        let speed = core.attribute(Attribute::MovementSpeed);
        if input.forward > 0.0 && self.sitting && !self.in_transition() {
            self.stand_up(core, cx);
        }
        if let Some(charge) = input.jump_charge
            && self.dash_cooldown == 0
            && core.base.on_ground
            && !self.refuses_to_move()
        {
            let amount = if charge >= 90 {
                1.0
            } else {
                0.4 + 0.4 * f64::from(charge) / 90.0
            };
            let yaw = f64::from(input.yaw).to_radians();
            let push = DASH_HORIZONTAL * amount * speed;
            core.base.velocity[0] -= yaw.sin() * push;
            core.base.velocity[2] += yaw.cos() * push;
            core.base.velocity[1] +=
                DASH_VERTICAL * amount * core.attribute(Attribute::JumpStrength);
            self.dash_cooldown = DASH_COOLDOWN;
            self.dashing = true;
            core.changed(IS_DASHING);
            core.sound(Self::SOUNDS.dash, 1.0, 1.0, cx);
        }
        let bonus = if input.sprinting && self.dash_cooldown == 0 {
            RUNNING_BONUS
        } else {
            0.0
        };
        ((speed + bonus) / speed.max(1.0e-9)) as f32
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.now = cx.tick;
        if self.sitting && core.in_liquid() {
            self.stand_up_instantly(core);
        }
        if self.sitting && !self.in_transition() && Self::tempted(core, cx) {
            self.stand_up(core, cx);
        }
        let idle = !core.in_liquid()
            && core.base.on_ground
            && core.leash.is_none()
            && core.base.riders.is_empty()
            && core.base.entity_riders.is_empty()
            && !core.movement.has_wanted()
            && self.pose_time() >= MIN_POSE_TICKS;
        if idle && core.is_alive() && !core.no_ai && cx.tick >= self.next_pick {
            let options = if self.sitting { 2 } else { 4 };
            match cx.rng.gen_range(0..options) {
                0 => {
                    if self.sitting {
                        self.stand_up(core, cx);
                    } else {
                        self.sit_down(core, cx);
                    }
                    self.next_pick = cx.tick + PICK_MOVE;
                }
                1 => self.next_pick = cx.tick + cx.rng.gen_range(PICK_REST),
                _ => self.next_pick = cx.tick + PICK_MOVE,
            }
        }
        if self.dashing
            && self.dash_cooldown < 50
            && (core.base.on_ground || core.in_liquid() || !core.base.riders.is_empty())
        {
            self.dashing = false;
            core.changed(IS_DASHING);
        }
        if self.dash_cooldown > 0 {
            self.dash_cooldown -= 1;
            if self.dash_cooldown == 0 {
                core.sound(Self::SOUNDS.dash_ready, 1.0, 1.0, cx);
            }
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        if HUSK {
            core.persistence_required = true;
        }
        let stack = interaction.stack.as_deref()?;
        super::equine::saddle_up(core, stack, Some(Self::SOUNDS.saddle), cx)
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        let mut goals = GoalSelector::default();
        goals.add(0, Float::new(core));
        goals.add(1, Panic::new(4.0));
        goals.add(3, Tempt::new(2.5, Self::food));
        if HUSK {
            core.age = None;
            core.love = None;
        } else {
            goals.add(2, Breed::new(1.0).brain());
            goals.add(4, FollowParent::new(2.5));
        }
        goals.add(5, RandomStroll::water_avoiding(2.0));
        goals.add(6, LookAtPlayer::new(6.0));
        goals.add(7, RandomLookAround::default());
        core.goals = goals;
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let pose = if self.sitting {
            Pose::Sitting
        } else {
            Pose::Standing
        };
        vec![
            (data::SPECIES, MetadataValue::Byte(0)),
            (IS_DASHING, MetadataValue::Boolean(self.dashing)),
            (
                LAST_POSE_CHANGE_TICK,
                MetadataValue::VarLong(self.last_pose_tick()),
            ),
            (metadata::POSE, MetadataValue::Pose(pose as i32)),
        ]
    }

    fn after_hurt(&mut self, core: &mut MobCore, _damage: &super::Damage, _cx: &mut TickContext) {
        self.stand_up_instantly(core);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("LastPoseTick", self.last_pose_tick());
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        let tick =
            tuff_core::world::entity::take_number(nbt, "LastPoseTick").map_or(0, |n| n as i64);
        self.sitting = tick < 0;
        self.changed_at = (tick != 0).then_some(tick.unsigned_abs());
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(Self::SOUNDS.ambient),
            hurt: Some(Self::SOUNDS.hurt),
            death: Some(Self::SOUNDS.death),
            step: Some(Self::SOUNDS.step),
            volume: 1.0,
        }
    }

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        Some(Self::SOUNDS.eat)
    }

    fn can_fall_in_love(&self, _core: &MobCore) -> bool {
        !HUSK
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        Self::food(stack.item)
    }

    fn offspring(&self, _partner: &Self, _rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    fn remove_when_far_away(&self) -> bool {
        HUSK
    }
}

/// Registers the camel's and camel husk's loaders.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Camel, super::load::<Camel>);
    registry.register_spawn_rule(EntityType::Camel, super::spawning::animal_rule);
    registry.register(EntityType::CamelHusk, super::load::<CamelHusk>);
    registry.register_spawn_rule(
        EntityType::CamelHusk,
        super::spawning::monster_under_sky_rule,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::MintReason;

    #[tuff_core::watchdog]
    #[test]
    fn a_sitting_camel_refuses_to_move_until_it_has_stood_up() {
        crate::tests::with_context(|core, cx| {
            let mut camel = Camel::<false> {
                now: cx.tick,
                ..Camel::default()
            };
            assert!(!camel.immobile(core));
            camel.sit_down(core, cx);
            assert!(camel.immobile(core));
            assert_eq!(camel.drive_input(core, RiderInput::default()), (0.0, 0.0));
            camel.now = cx.tick + SIT_TICKS;
            assert!(camel.sitting && !camel.in_transition());
            camel.stand_up(core, cx);
            assert!(camel.immobile(core), "still standing up");
            camel.now = cx.tick + STAND_TICKS;
            assert!(!camel.immobile(core));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_idle_camel_sits_down_and_stands_up_again_on_its_own() {
        crate::tests::with_context(|core, cx| {
            let mut camel = Camel::<false>::default();
            core.base.on_ground = true;
            let (mut sat, mut stood) = (false, false);
            for _ in 0..20_000 {
                cx.tick += 1;
                camel.tick(core, cx);
                if camel.sitting {
                    sat = true;
                } else if sat {
                    stood = true;
                    break;
                }
            }
            assert!(sat && stood);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_hurt_camel_stands_up_at_once_and_the_pose_survives_saving() {
        crate::tests::with_context(|core, cx| {
            let mut camel = Camel::<false>::default();
            camel.sit_down(core, cx);
            let mut saved = Compound::new();
            camel.save(core, &mut saved);
            let mut loaded = Camel::<false>::default();
            loaded.load(core, &mut saved);
            assert!(loaded.sitting);
            assert_eq!(loaded.changed_at, camel.changed_at);
            camel.stand_up_instantly(core);
            assert!(!camel.sitting && !camel.refuses_to_move());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn camel_husks_eat_rabbit_feet_never_breed_and_despawn_until_touched() {
        crate::tests::with_context(|core, cx| {
            CamelHusk::configure(core);
            let mut husk = CamelHusk::default();
            assert!(core.age.is_none() && core.love.is_none());
            assert!(!husk.can_fall_in_love(core) && husk.remove_when_far_away());
            assert!(husk.is_food(&ItemStack::mint(Item::RabbitFoot, 1, MintReason::Test)));
            assert!(!husk.is_food(&ItemStack::mint(Item::Cactus, 1, MintReason::Test)));
            assert_eq!(
                husk.sounds(core).ambient,
                Some(SoundEvent::EntityCamelHuskAmbient)
            );
            let click = EntityInteraction {
                player: 1,
                player_uuid: 1,
                position: [0.0; 3],
                location: [0.0; 3],
                stack: None,
                off_hand: false,
                creative: false,
                sneaking: false,
                riding: false,
                hero: None,
            };
            assert!(!core.persistence_required);
            husk.interact(core, &click, cx);
            assert!(core.persistence_required);

            let camel = Camel::<false>::default();
            assert!(camel.can_fall_in_love(core) && !camel.remove_when_far_away());
            assert!(camel.is_food(&ItemStack::mint(Item::Cactus, 1, MintReason::Test)));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn ridden_pigs_and_striders_mirror_half_the_riders_pitch() {
        use crate::Species;
        crate::tests::with_context(|core, cx| {
            let input = RiderInput {
                pitch: 40.0,
                ..RiderInput::default()
            };
            let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(1);
            crate::farm::Pig::create(&mut rng).ride_speed(core, &input, cx);
            assert_eq!(core.base.pitch, 20.0);
            core.base.pitch = 0.0;
            crate::strider::Strider::create(&mut rng).ride_speed(core, &input, cx);
            assert_eq!(core.base.pitch, 20.0);
        });
    }
}
