//! Foxes: a biome-picked variant, breeding on berries, picking up loot,
//! hunting chickens, rabbits and schooling fish, eating what they hold in
//! their mouth, avoiding wolves and polar bears, and biting back whoever
//! hurts them. They seek shelter from the sun and sleep there by day.
//! Stalking, trusted players, perching and village walks are not modeled;
//! both trusted-player references are always sent empty.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_item::item::BurnReason;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Item, tag};

use super::attack::{LeapAtTarget, MeleeAttack};
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{
    AvoidPlayer, Breed, FollowParent, LookAtPlayer, Panic, RandomLookAround, RandomStroll,
};
use super::target::{HurtByTarget, NearestAttackableEntity};
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{TickContext, take, take_bool};
use tuff_item::item::ItemStack;
use tuff_nbt::Compound;
use tuff_vanilla_data::entity::MobCategory;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_world_model::BlockPos;

const FLAGS: u8 = data::SPECIES + 1;
const FIRST_TRUSTED: u8 = data::SPECIES + 2;
const SECOND_TRUSTED: u8 = data::SPECIES + 3;

const FLAG_SLEEPING: i8 = 32;
const EAT_TICKS: i32 = 600;
const LAND_PREY: &[EntityType] = &[EntityType::Chicken, EntityType::Rabbit];
const FISH_PREY: &[EntityType] = &[
    EntityType::Cod,
    EntityType::Salmon,
    EntityType::TropicalFish,
];
const FEARED: &[EntityType] = &[EntityType::Wolf];
const FEARED_BEARS: &[EntityType] = &[EntityType::PolarBear];
const SLEEP_WAIT: i32 = 70;
const SHELTER_INTERVAL: i32 = 100;
const ALERT_RANGE: [f64; 3] = [12.0, 6.0, 12.0];

fn bright_outside(cx: &TickContext) -> bool {
    cx.sky_darken < 4 && cx.shared.dimensions.get(cx.dimension).has_sky_light()
}

fn thundering(cx: &TickContext) -> bool {
    cx.shared.weather.borrow().thundering
}

fn has_shelter(mob: &MobCore, cx: &TickContext) -> bool {
    let [x, _, z] = mob.base.position;
    let pos = super::physics::block_pos([x, mob.bounds()[4], z]);
    !mob.can_see_sky(pos, cx) && mob.walk_target_value(pos, cx) >= 0.0
}

fn alertable(mob: &MobCore, cx: &TickContext) -> bool {
    let area = super::physics::inflate(mob.bounds(), ALERT_RANGE);
    let hostile = cx.entities.iter().any(|e| {
        e.id != mob.base.id
            && e.living.is_some_and(|l| l.alive)
            && super::physics::intersects(area, e.bounds)
            && (e.kind.category() == MobCategory::Monster
                || matches!(e.kind, EntityType::Chicken | EntityType::Rabbit))
    });
    hostile
        || cx.players.iter().any(|p| {
            !p.creative
                && !p.invulnerable
                && !p.sneaking
                && super::physics::intersects(
                    area,
                    [
                        p.position[0] - 0.3,
                        p.position[1],
                        p.position[2] - 0.3,
                        p.position[0] + 0.3,
                        p.position[1] + p.height,
                        p.position[2] + 0.3,
                    ],
                )
        })
}

fn wake(core: &mut MobCore) {
    if core.sleeping {
        core.set_sleeping(false);
        core.changed(FLAGS);
    }
}

#[derive(Debug)]
struct SleepGoal {
    countdown: i32,
}

impl SleepGoal {
    fn new() -> Self {
        Self { countdown: -1 }
    }

    fn can_sleep(&mut self, mob: &MobCore, cx: &mut TickContext) -> bool {
        if self.countdown < 0 {
            self.countdown = cx.rng.gen_range(0..SLEEP_WAIT);
        }
        if self.countdown > 0 {
            self.countdown -= 1;
            return false;
        }
        bright_outside(cx)
            && has_shelter(mob, cx)
            && !alertable(mob, cx)
            && !mob.body.in_powder_snow
    }
}

impl Goal for SleepGoal {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK | Flags::JUMP
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.body.xxa != 0.0 || mob.body.yya != 0.0 || mob.body.zza != 0.0 {
            return false;
        }
        self.can_sleep(mob, cx) || mob.sleeping
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.can_sleep(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.body.jumping = false;
        mob.set_sleeping(true);
        mob.changed(FLAGS);
        mob.navigation.stop();
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.countdown = -1;
        wake(mob);
    }
}

#[derive(Debug)]
struct SeekShelter {
    speed: f64,
    interval: i32,
    wanted: [f64; 3],
}

impl SeekShelter {
    fn new(speed: f64) -> Self {
        Self {
            speed,
            interval: super::goal::reduced_delay(SHELTER_INTERVAL),
            wanted: [0.0; 3],
        }
    }

    fn pick(&mut self, mob: &MobCore, cx: &mut TickContext) -> bool {
        let here = super::physics::block_pos(mob.base.position);
        for _ in 0..10 {
            let pos = BlockPos::new(
                here.x + cx.rng.gen_range(0..20) - 10,
                here.y + cx.rng.gen_range(0..6) - 3,
                here.z + cx.rng.gen_range(0..20) - 10,
            );
            if !mob.can_see_sky(pos, cx) && mob.walk_target_value(pos, cx) < 0.0 {
                self.wanted = [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5];
                return true;
            }
        }
        false
    }
}

impl Goal for SeekShelter {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.sleeping || mob.target.is_some() {
            return false;
        }
        let here = super::physics::block_pos(mob.base.position);
        if thundering(cx) && mob.can_see_sky(here, cx) {
            return self.pick(mob, cx);
        }
        if self.interval > 0 {
            self.interval -= 1;
            return false;
        }
        self.interval = SHELTER_INTERVAL;
        bright_outside(cx)
            && mob.can_see_sky(here, cx)
            && !super::raider::is_village(cx.pois, here)
            && self.pick(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        wake(mob);
        mob.move_to_position(self.wanted, self.speed, cx);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FoxVariant {
    Red,
    Snow,
}

impl FoxVariant {
    fn id(self) -> i32 {
        match self {
            Self::Red => 0,
            Self::Snow => 1,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Snow => "snow",
        }
    }

    fn by_biome(biome: &str) -> Self {
        if tuff_datapack::tags::has(tag::worldgen::biome::key::SPAWNS_SNOW_FOXES, biome) {
            Self::Snow
        } else {
            Self::Red
        }
    }
}

fn fox_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::FOX_FOOD, item)
}

#[derive(Debug)]
pub struct Fox {
    variant: FoxVariant,
    ticks_since_eaten: i32,
}

impl Fox {
    /// Eats the food held in its mouth after 600 idle ticks on the ground.
    fn eat_from_mouth(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.ticks_since_eaten += 1;
        let slot = EquipmentSlot::Mainhand;
        let Some(held) = core.equipment.get(slot) else {
            return;
        };
        let consumable = held.item().food().is_some() && held.consumable().is_some();
        if !(consumable && core.target.is_none() && core.base.on_ground && !core.sleeping) {
            return;
        }
        if self.ticks_since_eaten > EAT_TICKS {
            if let Some(mut rest) = core.equipment.take(slot) {
                rest.shrink(1, BurnReason::Consumed);
                if rest.count() > 0 {
                    let none = core.equipment.replace(slot, Some(rest));
                    debug_assert!(none.is_none());
                }
            }
            self.ticks_since_eaten = 0;
        } else if self.ticks_since_eaten > EAT_TICKS - 40 && cx.rng.gen_range(0.0..1.0f32) < 0.1 {
            core.sound(SoundEvent::EntityFoxEat, 1.0, 1.0, cx);
        }
    }
}

impl Species for Fox {
    fn create(_rng: &mut StdRng) -> Self {
        Self {
            variant: FoxVariant::Red,
            ticks_since_eaten: 0,
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.can_pick_up_loot = true;
        let mut goals = GoalSelector::default();
        goals.add(0, super::goals::ClimbOnTopOfPowderSnow);
        goals.add(0, HurtByTarget::new(&[]));
        goals.add(4, NearestAttackableEntity::new(LAND_PREY, false));
        goals.add(6, NearestAttackableEntity::new(FISH_PREY, false));
        goals.add(1, MeleeAttack::new(1.2, true));
        goals.add(3, AvoidPlayer::entities(FEARED, 8.0, 1.6, 1.4));
        goals.add(3, AvoidPlayer::entities(FEARED_BEARS, 8.0, 1.6, 1.4));
        goals.add(10, LeapAtTarget::new(0.4));
        goals.add(2, Panic::new(2.2));
        goals.add(3, Breed::new(1.0));
        goals.add(6, SeekShelter::new(1.25));
        goals.add(7, SleepGoal::new());
        goals.add(8, FollowParent::new(1.25));
        goals.add(5, RandomStroll::water_avoiding(1.0));
        goals.add(6, LookAtPlayer::new(10.0));
        goals.add(7, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        if let Some(sleeping) = take_bool(nbt, "Sleeping") {
            core.sleeping = sleeping;
        }
        if let Some(variant) = take(nbt, "Type", |t| {
            Some(if t.as_str()? == "snow" {
                FoxVariant::Snow
            } else {
                FoxVariant::Red
            })
        }) {
            self.variant = variant;
        }
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        out.insert("Sleeping", core.sleeping);
        out.insert("Type", self.variant.name());
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.eat_from_mouth(core, cx);
        if core.sleeping && (core.body.in_water || core.target.is_some() || thundering(cx)) {
            wake(core);
        }
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (data::SPECIES, MetadataValue::VarInt(self.variant.id())),
            (
                FLAGS,
                MetadataValue::Byte(if core.sleeping { FLAG_SLEEPING } else { 0 }),
            ),
            (FIRST_TRUSTED, MetadataValue::OptionalUuid(None)),
            (SECOND_TRUSTED, MetadataValue::OptionalUuid(None)),
        ]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(if core.sleeping {
                SoundEvent::EntityFoxSleep
            } else {
                SoundEvent::EntityFoxAmbient
            }),
            hurt: Some(SoundEvent::EntityFoxHurt),
            death: Some(SoundEvent::EntityFoxDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        Some(SoundEvent::EntityFoxEat)
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        fox_food(stack.item())
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        Self {
            variant: if rng.r#gen() {
                self.variant
            } else {
                partner.variant
            },
            ticks_since_eaten: 0,
        }
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let pos = super::physics::block_pos(core.base.position);
        let biome = (cx.biome)(pos).unwrap_or("minecraft:plains");
        self.variant = FoxVariant::by_biome(biome);
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

/// Registers the fox's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Fox, super::load::<Fox>);
    registry.register_spawn_rule(EntityType::Fox, super::spawning::animal_rule);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goals::Tempt;
    use crate::tests::with_world;
    use tuff_core::protocol::play::metadata::POSE;
    use tuff_item::item::MintReason;
    use tuff_vanilla_data::Block;

    fn roofed(pos: BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        Some(if pos.y < 64 {
            Block::GrassBlock.default_state()
        } else if pos.y == 66 {
            Block::Stone.default_state()
        } else {
            Block::Air.default_state()
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn foxes_follow_parents_and_cannot_be_tempted() {
        with_world(&roofed, &[], |core, _cx| {
            Fox::configure(core);
            assert!(core.goals.find::<FollowParent>().is_some());
            assert!(core.goals.find::<Tempt>().is_none());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_sheltered_fox_sleeps_by_day_and_wakes_in_water() {
        with_world(&roofed, &[], |core, cx| {
            Fox::configure(core);
            let mut fox = Fox::create(cx.rng);
            let mut goal = SleepGoal::new();
            let mut slept = false;
            for _ in 0..SLEEP_WAIT + 2 {
                if goal.can_use(core, cx) {
                    slept = true;
                    break;
                }
            }
            assert!(slept, "the sleep countdown runs out under a roof by day");
            goal.start(core, cx);
            assert!(core.sleeping);
            assert!(core.changed.contains(&POSE));
            assert!(core.changed.contains(&FLAGS));
            assert!(
                fox.metadata(core)
                    .contains(&(FLAGS, MetadataValue::Byte(FLAG_SLEEPING)))
            );
            assert_eq!(
                core.core_metadata(POSE),
                Some(MetadataValue::Pose(i32::from(
                    tuff_vanilla_data::entity::Pose::Sleeping.id()
                )))
            );
            fox.tick(core, cx);
            assert!(core.sleeping, "dry and undisturbed it keeps sleeping");
            core.body.in_water = true;
            fox.tick(core, cx);
            assert!(!core.sleeping);
            assert!(
                fox.metadata(core)
                    .contains(&(FLAGS, MetadataValue::Byte(0)))
            );
        });
    }

    fn open(pos: BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        Some(if pos.y < 64 {
            Block::GrassBlock.default_state()
        } else {
            Block::Air.default_state()
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_fox_in_the_open_does_not_sleep() {
        with_world(&open, &[], |core, cx| {
            Fox::configure(core);
            let mut goal = SleepGoal::new();
            for _ in 0..SLEEP_WAIT * 3 {
                assert!(!goal.can_use(core, cx));
            }
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_sleeping_fox_round_trips_through_nbt() {
        with_world(&roofed, &[], |core, cx| {
            let mut fox = Fox::create(cx.rng);
            core.sleeping = true;
            let mut out = Compound::new();
            fox.save(core, &mut out);
            let mut other = MobCore::new(tuff_core::world::entity::EntityBase::new(
                EntityType::Fox,
                2,
                2,
                [0.5, 64.0, 0.5],
                [0.0; 3],
            ));
            fox.load(&mut other, &mut out);
            assert!(other.sleeping);
        });
    }

    fn partly_roofed(pos: BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        Some(if pos.y < 64 {
            Block::GrassBlock.default_state()
        } else if pos.y == 66 && pos.x >= 3 {
            Block::Stone.default_state()
        } else {
            Block::Air.default_state()
        })
    }

    fn seeks_shelter_within(pois: &crate::world::poi::PoiStore) -> bool {
        let mut seeks = false;
        let light = |pos: BlockPos| Some(if pos.x >= 3 { (0, 0) } else { (15, 0) });
        crate::tests::with_pois(pois, &partly_roofed, &light, |core, cx| {
            Fox::configure(core);
            let mut goal = SeekShelter::new(1.25);
            for _ in 0..SHELTER_INTERVAL * 4 {
                if goal.can_use(core, cx) {
                    seeks = true;
                    break;
                }
            }
        });
        seeks
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_fox_flees_the_sun_except_inside_a_village() {
        use crate::world::poi::PoiStore;
        use tuff_vanilla_data::registry::PointOfInterestType;
        use tuff_world_model::poi::Record;
        use tuff_world_model::pos::ChunkPos;
        assert!(seeks_shelter_within(&PoiStore::default()));
        let mut village = PoiStore::default();
        let bed = BlockPos::new(2, 64, 2);
        village.install(
            ChunkPos::new(0, 0),
            vec![Record::new(bed, PointOfInterestType::Home)],
        );
        village.hold(1, vec![bed]);
        assert!(!seeks_shelter_within(&village));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_fox_eats_the_food_in_its_mouth_after_600_ticks() {
        with_world(&open, &[], |core, cx| {
            Fox::configure(core);
            let mut fox = Fox::create(cx.rng);
            core.base.on_ground = true;
            core.equipment.set(
                EquipmentSlot::Mainhand,
                Some(ItemStack::mint(Item::SweetBerries, 2, MintReason::Test)),
            );
            for _ in 0..EAT_TICKS {
                fox.tick(core, cx);
            }
            assert_eq!(
                core.equipment
                    .get(EquipmentSlot::Mainhand)
                    .map(|s| s.count()),
                Some(2)
            );
            fox.tick(core, cx);
            assert_eq!(
                core.equipment
                    .get(EquipmentSlot::Mainhand)
                    .map(|s| s.count()),
                Some(1)
            );
            core.equipment.set(
                EquipmentSlot::Mainhand,
                Some(ItemStack::mint(Item::Feather, 1, MintReason::Test)),
            );
            for _ in 0..EAT_TICKS + 5 {
                fox.tick(core, cx);
            }
            assert_eq!(
                core.equipment
                    .get(EquipmentSlot::Mainhand)
                    .map(|s| s.count()),
                Some(1)
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_fox_hunts_chickens_and_dodges_wolves() {
        with_world(&open, &[], |core, _| {
            Fox::configure(core);
            let goals = format!("{:?}", core.goals);
            assert!(goals.contains("Chicken") && goals.contains("Wolf"));
        });
    }
}
