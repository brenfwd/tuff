//! Zombies: melee attackers that call nearby zombies for help, burn in
//! daylight, and sometimes spawn as fast babies or holding a tool.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::{EquipmentSlot, ModifierOperation};
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};

use super::Mob;
use super::attack::MeleeAttack;
use super::door::BreakDoor;
use super::goal::GoalSelector;
use super::goals::{LookAtPlayer, RandomLookAround, RandomStroll};
use super::monster::{self, LocalDifficulty};
use super::target::{HurtByTarget, NearestAttackableEntity, NearestAttackablePlayer};
use super::{Damage, MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{
    Effect, EntityBase, EntityHit, TickContext, take_bool, take_number,
};
use tuff_core::world::player::Difficulty;
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

const BABY_SPEED_ID: &str = "minecraft:baby";
const BABY_SPEED_BONUS: f64 = 0.5;
pub const BABY_CHANCE: f32 = 0.05;
const NAUTILUS_SHELL_CHANCE: f32 = 0.03;
/// Zombie data after the baby flag: the (unused) special type and whether
/// it is turning into a drowned.
const SPECIAL_TYPE: u8 = data::SPECIES - 1;
const DROWNED_CONVERSION: u8 = data::SPECIES;

/// Zombies that answer a zombie's call for help.
pub const ZOMBIE_ALERTS: &[EntityType] = &[
    EntityType::Zombie,
    EntityType::Husk,
    EntityType::Drowned,
    EntityType::ZombieVillager,
];

/// The villagers zombies hunt.
pub const VILLAGER_PREY: &[EntityType] = &[EntityType::Villager, EntityType::WanderingTrader];

/// Zombies go after baby turtles, but only out of the water.
pub(crate) fn baby_on_land(turtle: &tuff_core::world::entity::EntitySummary) -> bool {
    turtle
        .living
        .as_ref()
        .is_some_and(|living| living.age < 0 && !living.in_water)
}

/// Zombies whose kills turn villagers into zombie villagers.
pub const INFECTING: &[EntityType] = &[
    EntityType::Zombie,
    EntityType::Husk,
    EntityType::Drowned,
    EntityType::ZombieVillager,
    EntityType::ZombifiedPiglin,
];

const DROWN_AFFLICTION_TICKS: i32 = 600;
const DROWN_CONVERSION_TICKS: i32 = 300;
const ZOMBIE_TO_DROWNED: i32 = 1040;
const HUSK_TO_ZOMBIE: i32 = 1041;
const RANDOM_SPAWN_BONUS_ID: &str = "minecraft:random_spawn_bonus";
const ZOMBIE_SPAWN_BONUS_ID: &str = "minecraft:zombie_random_spawn_bonus";
const LEADER_BONUS_ID: &str = "minecraft:leader_zombie_bonus";
const CALLER_CHARGE_ID: &str = "minecraft:reinforcement_caller_charge";
const CALLEE_CHARGE_ID: &str = "minecraft:reinforcement_callee_charge";
const REINFORCEMENT_CHARGE: f64 = -0.05;
const REINFORCEMENT_TRIES: i32 = 50;

#[derive(Debug, Default)]
pub struct Zombie {
    can_break_doors: bool,
    in_water_time: i32,
    conversion: Option<i32>,
}

impl MobCore {
    /// Random head gear on Halloween: a pumpkin or jack o'lantern that
    /// never drops.
    pub fn wear_halloween_head(&mut self, rng: &mut StdRng, halloween: bool) {
        if halloween
            && self.equipment.get(EquipmentSlot::Head).is_none()
            && rng.r#gen::<f32>() < 0.25
        {
            let item = if rng.r#gen::<f32>() < 0.1 {
                Item::JackOLantern
            } else {
                Item::CarvedPumpkin
            };
            #[expect(
                clippy::disallowed_methods,
                reason = "item a mob spawns, wears or throws (wear_halloween_head)"
            )]
            self.equipment.set(
                EquipmentSlot::Head,
                Some(ItemStack::mint(item, 1, MintReason::Spawn)),
            );
            self.equipment.set_drop_chance(EquipmentSlot::Head, 0.0);
        }
    }
}

/// A freshly spawned baby sometimes rides a chicken: one free within a few
/// blocks, or else, as rarely again, a new one.
pub fn ride_chicken(core: &MobCore, cx: &mut TickContext) {
    if f64::from(cx.rng.r#gen::<f32>()) < CHICKEN_JOCKEY_CHANCE {
        let reach = super::physics::inflate(core.bounds(), [5.0, 3.0, 5.0]);
        let free = cx.entities.iter().find(|e| {
            e.kind == EntityType::Chicken
                && e.living.is_some_and(|l| l.alive)
                && !e.carries
                && e.vehicle.is_none()
                && e.id != core.base.id
                && super::physics::intersects(e.bounds, reach)
        });
        if let Some(chicken) = free {
            cx.effects.push(Effect::Notify {
                target: chicken.id,
                notice: tuff_core::world::entity::Notice::Jockey,
            });
            cx.effects.push(Effect::Mount {
                passenger: core.base.id,
                vehicle: chicken.id,
            });
        }
    } else if f64::from(cx.rng.r#gen::<f32>()) < CHICKEN_JOCKEY_CHANCE {
        let mut base = EntityBase::new(
            EntityType::Chicken,
            cx.shared.next_entity_id(),
            cx.rng.r#gen(),
            core.base.position,
            [0.0; 3],
        );
        base.yaw = core.base.yaw;
        let mut chicken = Mob::<super::farm::Chicken>::new(base, cx.rng);
        tuff_core::world::entity::RegionEntity::finalize_spawn(&mut chicken, cx, 0);
        chicken.species.jockey = true;
        let vehicle = chicken.core.base.id;
        cx.effects.push(Effect::Spawn(Box::new(chicken)));
        cx.effects.push(Effect::Mount {
            passenger: core.base.id,
            vehicle,
        });
    }
}

const CHICKEN_JOCKEY_CHANCE: f64 = 0.05;

/// A burning, empty-handed zombie sometimes sets its victim alight.
fn ignite_hook(
    mob: &MobCore,
    _target: &super::target::TargetView,
    hit: &mut EntityHit,
    cx: &mut TickContext,
) {
    let difficulty = LocalDifficulty::at(cx, mob.base.position).effective;
    if mob.equipment.main_hand().is_none()
        && mob.base.on_fire()
        && cx.rng.r#gen::<f32>() < difficulty * 0.3
    {
        hit.ignite = (2 * difficulty as i32) as f32;
    }
}

/// A husk's empty-handed hit also starves: hunger for seven seconds per
/// level of local difficulty.
fn husk_hook(
    mob: &MobCore,
    target: &super::target::TargetView,
    hit: &mut EntityHit,
    cx: &mut TickContext,
) {
    ignite_hook(mob, target, hit, cx);
    if mob.equipment.main_hand().is_none() {
        let difficulty = LocalDifficulty::at(cx, mob.base.position).effective as i64;
        hit.effects
            .push(tuff_core::world::effects::ActiveEffect::timed(
                tuff_vanilla_data::registry::MobEffect::Hunger,
                HUSK_HUNGER_TICKS * difficulty,
                0,
            ));
    }
}

const HUSK_HUNGER_TICKS: i64 = 140;

impl MobCore {
    /// A zombie's baby state: smaller, and half again as fast.
    pub fn set_zombie_baby(&mut self, baby: bool) {
        self.set_baby(baby);
        let bonus = baby.then_some((BABY_SPEED_BONUS, ModifierOperation::AddMultipliedBase));
        self.set_modifier(Attribute::MovementSpeed, BABY_SPEED_ID, bonus);
    }
}

impl Zombie {
    pub fn set_can_break_doors(&mut self, core: &mut MobCore, can: bool) {
        if can && !self.can_break_doors {
            core.goals.add(1, BreakDoor::new());
            core.navigation.can_open_doors = true;
        }
        self.can_break_doors = can;
    }

    fn converts_in_water(core: &MobCore) -> bool {
        matches!(core.kind(), EntityType::Zombie | EntityType::Husk)
    }

    /// Random bonuses every zombie spawn gets, and sometimes a leader's.
    pub fn handle_attributes(
        &mut self,
        core: &mut MobCore,
        special: f32,
        fresh: bool,
        rng: &mut StdRng,
    ) {
        let reinforcements = if core.kind() == EntityType::ZombifiedPiglin {
            0.0
        } else {
            rng.r#gen::<f64>() * 0.1
        };
        core.set_attribute_base(Attribute::SpawnReinforcements, reinforcements);
        core.set_permanent_modifier(
            Attribute::KnockbackResistance,
            RANDOM_SPAWN_BONUS_ID,
            Some((rng.r#gen::<f64>() * 0.05, ModifierOperation::AddValue)),
        );
        let follow = rng.r#gen::<f64>() * 1.5 * f64::from(special);
        if follow > 1.0 {
            core.set_permanent_modifier(
                Attribute::FollowRange,
                ZOMBIE_SPAWN_BONUS_ID,
                Some((follow, ModifierOperation::AddMultipliedTotal)),
            );
        }
        if rng.r#gen::<f32>() < special * 0.05 {
            core.set_permanent_modifier(
                Attribute::SpawnReinforcements,
                LEADER_BONUS_ID,
                Some((rng.r#gen::<f64>() * 0.25 + 0.5, ModifierOperation::AddValue)),
            );
            core.set_permanent_modifier(
                Attribute::MaxHealth,
                LEADER_BONUS_ID,
                Some((
                    rng.r#gen::<f64>() * 3.0 + 1.0,
                    ModifierOperation::AddMultipliedTotal,
                )),
            );
            if fresh {
                core.health = core.max_health();
            }
            self.set_can_break_doors(core, true);
        }
    }

    fn convert_in_water(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let (kind, event) = if core.kind() == EntityType::Husk {
            (EntityType::Zombie, HUSK_TO_ZOMBIE)
        } else {
            (EntityType::Drowned, ZOMBIE_TO_DROWNED)
        };
        let mut converted: Mob<Zombie> = super::convert(core, kind, true, cx);
        converted.core.can_pick_up_loot = core.can_pick_up_loot;
        if core.is_baby() {
            converted.core.set_zombie_baby(true);
        }
        let special = LocalDifficulty::at(cx, core.base.position).special_multiplier();
        let Mob { core: new, species } = &mut converted;
        species.handle_attributes(new, special, false, cx.rng);
        new.health = new.max_health();
        if !core.base.silent {
            monster::level_event(super::physics::block_pos(core.base.position), event, 0, cx);
        }
        cx.effects.push(Effect::Spawn(Box::new(converted)));
        core.discarded = true;
    }

    /// The conversion clock: submerged long enough, a zombie shakes for
    /// fifteen seconds and then becomes a drowned (a husk, a zombie).
    pub fn tick_conversion(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !Self::converts_in_water(core) || !core.is_alive() || core.no_ai || core.body.first_tick
        {
            return;
        }
        if core.body.eyes_in_water {
            if let Some(left) = &mut self.conversion {
                *left -= 1;
                if *left < 0 {
                    self.convert_in_water(core, cx);
                }
                return;
            }
            self.in_water_time += 1;
            if self.in_water_time >= DROWN_AFFLICTION_TICKS {
                self.conversion = Some(DROWN_CONVERSION_TICKS);
                core.changed(DROWNED_CONVERSION);
            }
        } else {
            self.in_water_time = -1;
            if self.conversion.take().is_some() {
                core.changed(DROWNED_CONVERSION);
            }
        }
    }

    /// On hard difficulty, a hurt zombie with a target may call another
    /// of its kind from a dark spot out of sight of players.
    pub fn reinforcement_spot(
        core: &mut MobCore,
        damage: &Damage,
        cx: &mut TickContext,
    ) -> Option<([f64; 3], i32)> {
        let target = core.target.or(damage.attacker.map(|(id, _)| id))?;
        if *cx.shared.difficulty.borrow() != Difficulty::Hard
            || cx.rng.r#gen::<f32>() >= core.attribute(Attribute::SpawnReinforcements) as f32
        {
            return None;
        }
        let [x, y, z] = core.base.position.map(|v| v.floor() as i32);
        let kind = core.kind();
        for _ in 0..REINFORCEMENT_TRIES {
            let offset = |rng: &mut StdRng| rng.gen_range(7..=40) * rng.gen_range(-1..=1);
            let pos = tuff_world_model::BlockPos::new(
                x + offset(cx.rng),
                y + offset(cx.rng),
                z + offset(cx.rng),
            );
            if !super::spawning::placement_ok(kind, pos, cx.block)
                || !reinforcement_rule_holds(kind, pos, cx)
            {
                continue;
            }
            let position = [
                f64::from(pos.x) + 0.5,
                f64::from(pos.y),
                f64::from(pos.z) + 0.5,
            ];
            let near = cx.players.iter().any(|p| {
                let d = [
                    p.position[0] - position[0],
                    p.position[1] - position[1],
                    p.position[2] - position[2],
                ];
                d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < 49.0
            });
            if near
                || !super::spawning::unobstructed(
                    super::spawning::spawn_box(kind, position),
                    cx.block,
                )
            {
                continue;
            }
            let existing = core
                .modifier_amount(Attribute::SpawnReinforcements, CALLER_CHARGE_ID)
                .unwrap_or(0.0);
            core.set_permanent_modifier(
                Attribute::SpawnReinforcements,
                CALLER_CHARGE_ID,
                Some((existing + REINFORCEMENT_CHARGE, ModifierOperation::AddValue)),
            );
            return Some((position, target));
        }
        None
    }

    /// Marks a freshly called zombie as a callee: it is less likely to
    /// call more.
    pub fn mark_callee(core: &mut MobCore) {
        core.set_permanent_modifier(
            Attribute::SpawnReinforcements,
            CALLEE_CHARGE_ID,
            Some((REINFORCEMENT_CHARGE, ModifierOperation::AddValue)),
        );
    }
}

/// Whether the type's own spawn rule lets a reinforcement stand at `pos`.
fn reinforcement_rule_holds(
    kind: EntityType,
    pos: tuff_world_model::BlockPos,
    cx: &mut TickContext,
) -> bool {
    let Some(rule) = cx.shared.entity_types.spawn_rule(kind) else {
        return true;
    };
    let dimension = cx.shared.dimensions.get(cx.dimension);
    let weather = *cx.shared.weather.borrow();
    let players = cx.players;
    let player_within = |at: [f64; 3], range: f64| {
        players
            .iter()
            .any(|p| tuff_core::world::distance_sq(p.position, at) < range * range)
    };
    let level = super::spawning::SpawnLevel {
        block: cx.block,
        light: cx.light,
        sky_darken: cx.sky_darken,
        min_y: cx.min_y,
        sea_level: dimension.sea_level(),
        dimension: dimension.kind,
        thundering: weather.thunder_level * weather.rain_level > 0.9,
        biome: cx.biome,
        seed: cx.shared.seed,
        surface_slime_chance: tuff_core::world::surface_slime_chance(cx.shared, dimension),
        difficulty: *cx.shared.difficulty.borrow(),
        player_within: &player_within,
    };
    rule(
        kind,
        &level,
        pos,
        super::spawning::SpawnReason::Reinforcement,
        cx.rng,
    )
}

impl Species for Zombie {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.attack_hook = Some(if core.kind() == EntityType::Husk {
            husk_hook
        } else {
            ignite_hook
        });
        let mut goals = GoalSelector::default();
        let drowned = core.kind() == EntityType::Drowned;
        goals.add(3, NearestAttackableEntity::new(VILLAGER_PREY, false));
        goals.add(
            3,
            NearestAttackableEntity::new(&[EntityType::IronGolem], true),
        );
        goals.add(
            5,
            NearestAttackableEntity::new(&[EntityType::Turtle], true).only(baby_on_land),
        );
        if drowned {
            goals.add(
                1,
                HurtByTarget::new(&[EntityType::Drowned]).alerting(ZOMBIE_ALERTS),
            );
            goals.add(
                2,
                NearestAttackablePlayer::new(true).filtered(super::drowned::ok_target),
            );
            goals.add(
                3,
                NearestAttackableEntity::new(&[EntityType::Axolotl], true),
            );
            goals.add(2, super::ranged::TridentAttack::new(1.0, 40, 10.0));
            goals.add(
                2,
                MeleeAttack::zombie(1.0, false).only_targets(super::drowned::ok_target),
            );
            super::drowned::configure(core, &mut goals);
        } else {
            goals.add(1, HurtByTarget::new(&[]).alerting(ZOMBIE_ALERTS));
            goals.add(2, NearestAttackablePlayer::new(true));
            goals.add(2, super::spear::SpearUse::standard());
            goals.add(3, MeleeAttack::zombie(1.0, false));
            goals.add(
                6,
                super::village_walk::MoveThroughVillage::new(1.0, true, 4),
            );
            goals.add(7, RandomStroll::water_avoiding(1.0));
        }
        goals.add(4, AttackTurtleEgg::new());
        goals.add(8, LookAtPlayer::new(8.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        if take_bool(nbt, "IsBaby").unwrap_or(false) {
            core.set_zombie_baby(true);
        }
        let can = take_bool(nbt, "CanBreakDoors").unwrap_or(false);
        self.set_can_break_doors(core, can);
        self.in_water_time = take_number(nbt, "InWaterTime").map_or(0, |v| v as i32);
        let converting = take_number(nbt, "DrownedConversionTime").map_or(-1, |v| v as i32);
        if converting != -1 {
            self.conversion = Some(converting);
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.tick_conversion(core, cx);
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) {
        let Some((position, target)) = Self::reinforcement_spot(core, damage, cx) else {
            return;
        };
        let base = EntityBase::new(
            core.kind(),
            cx.shared.next_entity_id(),
            cx.rng.r#gen(),
            position,
            [0.0; 3],
        );
        let mut child = Mob::<Zombie>::new(base, cx.rng);
        child.core.target = Some(target);
        child.species.finalize_spawn(&mut child.core, cx);
        Self::mark_callee(&mut child.core);
        cx.effects.push(Effect::Spawn(Box::new(child)));
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        out.insert("IsBaby", core.is_baby());
        out.insert("CanBreakDoors", self.can_break_doors);
        out.insert(
            "InWaterTime",
            if core.body.eyes_in_water {
                self.in_water_time
            } else {
                -1
            },
        );
        out.insert("DrownedConversionTime", self.conversion.unwrap_or(-1));
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (data::BABY, MetadataValue::Boolean(core.is_baby())),
            (SPECIAL_TYPE, MetadataValue::VarInt(0)),
            (
                DROWNED_CONVERSION,
                MetadataValue::Boolean(self.conversion.is_some()),
            ),
        ]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        if core.kind() == EntityType::Husk {
            return monster::sounds(
                SoundEvent::EntityHuskAmbient,
                SoundEvent::EntityHuskHurt,
                SoundEvent::EntityHuskDeath,
                Some(SoundEvent::EntityHuskStep),
            );
        }
        if core.kind() == EntityType::Drowned {
            return if core.body.in_water {
                monster::sounds(
                    SoundEvent::EntityDrownedAmbientWater,
                    SoundEvent::EntityDrownedHurtWater,
                    SoundEvent::EntityDrownedDeathWater,
                    Some(SoundEvent::EntityDrownedStep),
                )
            } else {
                monster::sounds(
                    SoundEvent::EntityDrownedAmbient,
                    SoundEvent::EntityDrownedHurt,
                    SoundEvent::EntityDrownedDeath,
                    Some(SoundEvent::EntityDrownedStep),
                )
            };
        }
        monster::sounds(
            SoundEvent::EntityZombieAmbient,
            SoundEvent::EntityZombieHurt,
            SoundEvent::EntityZombieDeath,
            Some(SoundEvent::EntityZombieStep),
        )
    }

    fn experience(&self, core: &MobCore, rng: &mut StdRng) -> i32 {
        let reward = if core.is_baby() {
            (f64::from(core.xp_reward) * 2.5) as i32
        } else {
            core.xp_reward
        };
        reward + core.equipment.experience_bonus(rng)
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.setup_spawn(core, cx, true);
        if core.kind() == EntityType::Husk && super::spawning::spawning_naturally(cx) {
            ride_camel_husk(core, cx);
        }
        if core.kind() == EntityType::Drowned && super::spawning::spawning_naturally(cx) {
            ride_zombie_nautilus(core, cx);
        }
    }
}

const CAMEL_HUSK_CHANCE: f32 = 0.1;
const NAUTILUS_JOCKEY_CHANCE: f32 = 0.5;

/// A drowned spawning naturally with a trident sometimes rides a fresh
/// zombie nautilus, except a baby or one in a biome that spawns drowned
/// often.
fn ride_zombie_nautilus(core: &mut MobCore, cx: &mut TickContext) {
    let holds_trident = core
        .equipment
        .get(EquipmentSlot::Mainhand)
        .is_some_and(|stack| stack.item() == Item::Trident);
    if !holds_trident || cx.rng.r#gen::<f32>() >= NAUTILUS_JOCKEY_CHANCE || core.is_baby() {
        return;
    }
    let position = core.base.position;
    let frequent = (cx.biome)(super::physics::block_pos(position)).is_some_and(|biome| {
        tuff_datapack::tags::has(
            tuff_vanilla_data::tag::worldgen::biome::key::MORE_FREQUENT_DROWNED_SPAWNS,
            biome,
        )
    });
    if frequent {
        return;
    }
    let types = &cx.shared.entity_types;
    let Some(mut nautilus) = types.create(
        EntityType::ZombieNautilus,
        cx.shared.next_entity_id(),
        position,
        core.base.yaw,
    ) else {
        return;
    };
    nautilus.finalize_spawn(cx, 0);
    let vehicle = nautilus.base().id;
    cx.effects.push(Effect::Spawn(nautilus));
    cx.effects.push(Effect::StopRiding);
    cx.effects.push(Effect::Mount {
        passenger: core.base.id,
        vehicle,
    });
}

/// A husk spawning naturally where a camel husk fits sometimes takes an
/// iron spear and rides a fresh one (leaving any chicken it got on), with
/// a parched behind it.
fn ride_camel_husk(core: &mut MobCore, cx: &mut TickContext) {
    let position = core.base.position;
    let feet = [
        position[0].floor() + 0.5,
        position[1].floor(),
        position[2].floor() + 0.5,
    ];
    let area = super::spawning::spawn_box(EntityType::CamelHusk, feet);
    if !super::physics::no_collision(area, cx.block) || cx.rng.r#gen::<f32>() >= CAMEL_HUSK_CHANCE {
        return;
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "item a mob spawns, wears or throws (ride_camel_husk)"
    )]
    core.equipment.set(
        EquipmentSlot::Mainhand,
        Some(ItemStack::mint(Item::IronSpear, 1, MintReason::Spawn)),
    );
    let types = &cx.shared.entity_types;
    let Some(mut camel) = types.create(
        EntityType::CamelHusk,
        cx.shared.next_entity_id(),
        position,
        0.0,
    ) else {
        return;
    };
    let parched = types.create(
        EntityType::Parched,
        cx.shared.next_entity_id(),
        position,
        core.base.yaw,
    );
    camel.finalize_spawn(cx, 0);
    let vehicle = camel.base().id;
    cx.effects.push(Effect::Spawn(camel));
    cx.effects.push(Effect::StopRiding);
    cx.effects.push(Effect::Mount {
        passenger: core.base.id,
        vehicle,
    });
    if let Some(mut parched) = parched {
        parched.finalize_spawn(cx, 0);
        let passenger = parched.base().id;
        cx.effects.push(Effect::Spawn(parched));
        cx.effects.push(Effect::Mount { passenger, vehicle });
    }
}

impl Zombie {
    pub(crate) fn setup_spawn(
        &mut self,
        core: &mut MobCore,
        cx: &mut TickContext,
        roll_baby: bool,
    ) {
        let difficulty = LocalDifficulty::at(cx, core.base.position);
        let special = difficulty.special_multiplier();
        core.can_pick_up_loot = cx.rng.r#gen::<f32>() < 0.55 * special;
        if roll_baby && cx.rng.r#gen::<f32>() < BABY_CHANCE {
            core.set_zombie_baby(true);
            ride_chicken(core, cx);
        }
        let can_break = cx.rng.r#gen::<f32>() < special * 0.1;
        self.set_can_break_doors(core, can_break);
        if core.kind() != EntityType::Drowned {
            core.populate_spawn_armor(difficulty, cx.rng);
        }
        if core.kind() == EntityType::Drowned {
            // Drowned replace the generic zombie tool chance with their
            // own trident/fishing rod one and a rare nautilus shell in
            // the off hand, guaranteed to drop if it rolls.
            if cx.rng.r#gen::<f32>() > 0.9 {
                let item = if cx.rng.gen_range(0..16) < 10 {
                    Item::Trident
                } else {
                    Item::FishingRod
                };
                #[expect(
                    clippy::disallowed_methods,
                    reason = "item a mob spawns, wears or throws (setup_spawn)"
                )]
                core.equipment.set(
                    EquipmentSlot::Mainhand,
                    Some(ItemStack::mint(item, 1, MintReason::Spawn)),
                );
            }
            if core.equipment.get(EquipmentSlot::Offhand).is_none()
                && cx.rng.r#gen::<f32>() < NAUTILUS_SHELL_CHANCE
            {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "item a mob spawns, wears or throws (setup_spawn)"
                )]
                core.equipment.set(
                    EquipmentSlot::Offhand,
                    Some(ItemStack::mint(Item::NautilusShell, 1, MintReason::Spawn)),
                );
                core.equipment.set_drop_chance(EquipmentSlot::Offhand, 2.0);
            }
        } else {
            let hard = difficulty.base == Difficulty::Hard;
            if cx.rng.r#gen::<f32>() < if hard { 0.05 } else { 0.01 } {
                let item = match cx.rng.gen_range(0..6) {
                    0 => Item::IronSword,
                    1 => Item::IronSpear,
                    _ => Item::IronShovel,
                };
                #[expect(
                    clippy::disallowed_methods,
                    reason = "item a mob spawns, wears or throws (setup_spawn)"
                )]
                core.equipment.set(
                    EquipmentSlot::Mainhand,
                    Some(ItemStack::mint(item, 1, MintReason::Spawn)),
                );
            }
        }
        core.populate_spawn_enchantments(difficulty, cx.rng);
        core.wear_halloween_head(cx.rng, super::calendar::halloween_now());
        self.handle_attributes(core, special, true, cx.rng);
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Zombie, super::load::<Zombie>);
    registry.register(EntityType::Husk, super::load::<Zombie>);
    registry.register(EntityType::Drowned, super::load::<Zombie>);
    registry.register_spawn_rule(EntityType::Zombie, super::spawning::monster_rule);
    registry.register_spawn_rule(EntityType::Husk, super::spawning::monster_under_sky_rule);
    registry.register_spawn_rule(EntityType::Drowned, super::spawning::drowned_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_converted_zombie_gets_spawn_attributes_but_no_baby_roll() {
        crate::tests::with_context(|core, cx| {
            let mut zombie = Zombie::default();
            for _ in 0..300 {
                zombie.setup_spawn(core, cx, false);
                assert!(!core.is_baby());
            }
            assert!(core.attribute_base(Attribute::SpawnReinforcements) > 0.0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn monster_data_indices_match_the_dumped_layouts() {
        let byte = MetadataValue::Byte(0).type_id() as u8;
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let varint = MetadataValue::VarInt(0).type_id() as u8;
        let zombie = EntityType::Zombie.data_layout();
        assert_eq!(zombie.len(), DROWNED_CONVERSION as usize + 1);
        assert_eq!(zombie[data::LIVING_FLAGS as usize], byte);
        assert_eq!(zombie[data::MOB_FLAGS as usize], byte);
        assert_eq!(zombie[data::BABY as usize..], [boolean, varint, boolean]);
        let drowned = EntityType::Drowned.data_layout();
        assert_eq!(drowned.len(), DROWNED_CONVERSION as usize + 1);
        assert_eq!(drowned[data::BABY as usize..], [boolean, varint, boolean]);
        assert_eq!(
            EntityType::Skeleton.data_layout().len(),
            data::BABY as usize + 1,
            "a skeleton adds only its freezing flag"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_trident_drowned_rides_a_zombie_nautilus_half_the_time_unless_a_baby() {
        use rand::SeedableRng;
        let rides = |trident: bool, baby: bool| {
            let mut count = 0;
            crate::tests::with_context(|core, cx| {
                let stack = trident.then(|| ItemStack::mint(Item::Trident, 1, MintReason::Test));
                core.equipment.set(EquipmentSlot::Mainhand, stack);
                core.set_zombie_baby(baby);
                for seed in 0..400 {
                    *cx.rng = StdRng::seed_from_u64(seed);
                    cx.effects.clear();
                    ride_zombie_nautilus(core, cx);
                    let spawned = cx.effects.iter().any(|e| {
                        matches!(e, Effect::Spawn(entity) if entity.base().kind == EntityType::ZombieNautilus)
                    });
                    let mounted = cx.effects.iter().any(|e| matches!(e, Effect::Mount { .. }));
                    assert_eq!(spawned, mounted);
                    count += usize::from(spawned);
                }
            });
            count
        };
        let jockeys = rides(true, false);
        assert!(
            (120..=280).contains(&jockeys),
            "about half ride, got {jockeys}"
        );
        assert_eq!(rides(false, false), 0);
        assert_eq!(rides(true, true), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn halloween_heads_are_pumpkins_that_never_drop() {
        use rand::SeedableRng;
        let mut heads = std::collections::HashSet::new();
        crate::tests::with_context(|core, _| {
            for seed in 0..200 {
                core.equipment.set(EquipmentSlot::Head, None);
                let mut rng = StdRng::seed_from_u64(seed);
                core.wear_halloween_head(&mut rng, false);
                assert!(core.equipment.get(EquipmentSlot::Head).is_none());
                core.wear_halloween_head(&mut rng, true);
                if let Some(head) = core.equipment.get(EquipmentSlot::Head) {
                    heads.insert(head.item());
                }
            }
        });
        assert!(heads.contains(&Item::CarvedPumpkin));
        assert!(heads.contains(&Item::JackOLantern));
    }

    #[tuff_core::watchdog]
    #[test]
    fn spawn_attributes_roll_reinforcements_and_leaders() {
        use rand::SeedableRng;
        let (mut leaders, mut followers) = (0, 0);
        crate::tests::with_context(|core, _| {
            for seed in 0..400 {
                let mut rng = StdRng::seed_from_u64(seed);
                let mut zombie = Zombie::default();
                zombie.handle_attributes(core, 1.0, true, &mut rng);
                let chance = core.attribute(Attribute::SpawnReinforcements);
                assert!((0.0..=0.1).contains(&core.attribute_base(Attribute::SpawnReinforcements)));
                if zombie.can_break_doors {
                    leaders += 1;
                    assert!((0.5..=0.85).contains(&chance), "{chance}");
                    assert!(
                        core.max_health() as f64 >= core.attribute_base(Attribute::MaxHealth) * 2.0
                    );
                    assert_eq!(core.health, core.max_health());
                } else {
                    followers += 1;
                    assert!(chance <= 0.1);
                }
                core.modifiers.clear();
            }
        });
        assert!(leaders > 0 && followers > leaders);
    }

    #[tuff_core::watchdog]
    #[test]
    fn spawn_gear_is_enchanted_by_local_difficulty() {
        use rand::SeedableRng;
        let hard = LocalDifficulty {
            base: Difficulty::Hard,
            effective: 6.0,
        };
        let easy = LocalDifficulty {
            base: Difficulty::Easy,
            effective: 0.5,
        };
        let count = |difficulty: LocalDifficulty| {
            let mut enchanted = 0;
            crate::tests::with_context(|core, _| {
                for seed in 0..100 {
                    let mut rng = StdRng::seed_from_u64(seed);
                    core.equipment.set(
                        EquipmentSlot::Chest,
                        Some(ItemStack::mint(
                            Item::DiamondChestplate,
                            1,
                            MintReason::Test,
                        )),
                    );
                    core.populate_spawn_enchantments(difficulty, &mut rng);
                    let chest = core.equipment.get(EquipmentSlot::Chest).unwrap();
                    enchanted += usize::from(chest.components.enchantments.is_some());
                }
            });
            enchanted
        };
        assert_eq!(count(easy), 0);
        let hard = count(hard);
        assert!(hard > 20 && hard < 80, "{hard}");
    }

    fn chicken(id: i32, x: f64, carries: bool) -> tuff_core::world::entity::EntitySummary {
        tuff_core::world::entity::EntitySummary {
            id,
            uuid: id as u128,
            kind: EntityType::Chicken,
            position: [x, 64.0, 0.5],
            velocity: [0.0; 3],
            yaw: 0.0,
            bounds: [x - 0.2, 64.0, 0.3, x + 0.2, 64.7, 0.7],
            living: Some(tuff_core::world::entity::LivingSummary {
                unpushable: false,
                alive: true,
                target: None,
                age: 0,
                in_love: false,
                panicking: false,
                eye_height: 0.6,
                in_water: false,
                seen_as_enemy: true,
                enemy: false,
                health: 4.0,
                tamed: false,
                owner: None,
                chasing: None,
            }),
            pickable: true,
            item: None,
            vehicle: None,
            carries,
            ominous_banner: false,
            blue_sheep: false,
        }
    }

    /// Mount effects of `ride_chicken` over many rolls: (onto an existing
    /// chicken, onto a new one).
    fn jockey_rolls(entities: &[tuff_core::world::entity::EntitySummary]) -> (usize, usize) {
        use rand::SeedableRng;
        let (mut existing, mut new) = (0, 0);
        crate::tests::with_entities(entities, |core, cx| {
            for seed in 0..4000 {
                *cx.rng = StdRng::seed_from_u64(seed);
                cx.effects.clear();
                ride_chicken(core, cx);
                let mounted = cx.effects.iter().find_map(|e| match e {
                    Effect::Mount { passenger, vehicle } => Some((*passenger, *vehicle)),
                    _ => None,
                });
                let Some((passenger, vehicle)) = mounted else {
                    assert!(cx.effects.is_empty());
                    continue;
                };
                assert_eq!(passenger, core.base.id);
                let spawned = cx.effects.iter().find_map(|e| match e {
                    Effect::Spawn(entity) => Some(entity),
                    _ => None,
                });
                match spawned {
                    Some(entity) => {
                        let chicken = entity
                            .downcast_ref::<Mob<super::super::farm::Chicken>>()
                            .unwrap();
                        assert!(chicken.species.jockey);
                        assert_eq!(chicken.core.base.id, vehicle);
                        new += 1;
                    }
                    None => {
                        assert!(cx.effects.iter().any(|e| matches!(
                            e,
                            Effect::Notify {
                                target,
                                notice: tuff_core::world::entity::Notice::Jockey,
                            } if *target == vehicle
                        )));
                        existing += 1;
                    }
                }
            }
        });
        (existing, new)
    }

    #[tuff_core::watchdog]
    #[test]
    fn babies_ride_a_free_chicken_nearby_or_rarely_a_new_one() {
        let (existing, new) = jockey_rolls(&[]);
        assert_eq!(existing, 0);
        assert!((120..270).contains(&new), "{new}");
        let (existing, new) = jockey_rolls(&[chicken(50, 4.0, false)]);
        assert!((120..280).contains(&existing), "{existing}");
        assert!((100..270).contains(&new), "{new}");
        let (existing, _) = jockey_rolls(&[chicken(50, 4.0, true), chicken(51, 9.0, false)]);
        assert_eq!(
            existing, 0,
            "a ridden chicken or one out of reach is not taken"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn spawn_bonuses_are_saved_and_loaded() {
        use rand::SeedableRng;
        use tuff_core::world::entity::RegionEntity;
        let leader = (0..400)
            .find_map(|seed| {
                let mut rng = StdRng::seed_from_u64(seed);
                let base = EntityBase::new(EntityType::Zombie, 1, 7, [0.5, 64.0, 0.5], [0.0; 3]);
                let mut mob = Mob::<Zombie>::new(base, &mut rng);
                let Mob { core, species } = &mut mob;
                species.handle_attributes(core, 1.0, true, &mut rng);
                species.can_break_doors.then_some(mob)
            })
            .expect("a leader rolls");
        let mut leader = leader;
        Zombie::mark_callee(&mut leader.core);
        leader.core.set_zombie_baby(true);
        let mut saved = Compound::new();
        leader.save(&mut saved);
        let base = EntityBase::new(EntityType::Zombie, 2, 7, [0.5, 64.0, 0.5], [0.0; 3]);
        let loaded = super::super::load::<Zombie>(base, &mut saved).unwrap();
        let loaded = loaded.downcast_ref::<Mob<Zombie>>().unwrap();
        for attribute in [
            Attribute::MaxHealth,
            Attribute::SpawnReinforcements,
            Attribute::KnockbackResistance,
            Attribute::FollowRange,
            Attribute::MovementSpeed,
        ] {
            assert_eq!(
                loaded.core.attribute(attribute),
                leader.core.attribute(attribute),
                "{attribute:?}"
            );
        }
        assert!(loaded.core.max_health() > 20.0);
        assert_eq!(loaded.core.health, leader.core.health);
        assert!(saved.is_empty() || !saved.contains_key("attributes"));
        let baby = loaded
            .core
            .modifiers
            .iter()
            .filter(|m| m.id == BABY_SPEED_ID)
            .count();
        assert_eq!(baby, 1, "the baby speed comes back from IsBaby, not twice");
    }

    #[tuff_core::watchdog]
    #[test]
    fn saved_modifier_lists_follow_the_game_format() {
        let mut core_nbt = Compound::new().with(
            "attributes",
            tuff_nbt::Tag::List(vec![tuff_nbt::Tag::Compound(
                Compound::new()
                    .with("id", "minecraft:max_health")
                    .with("base", 20.0f64)
                    .with(
                        "modifiers",
                        tuff_nbt::Tag::List(vec![tuff_nbt::Tag::Compound(
                            Compound::new()
                                .with("id", "example:boost")
                                .with("amount", 1.0f64)
                                .with("operation", "add_multiplied_total"),
                        )]),
                    ),
            )]),
        );
        let base = EntityBase::new(EntityType::Zombie, 2, 7, [0.5, 64.0, 0.5], [0.0; 3]);
        let mob = super::super::load::<Zombie>(base, &mut core_nbt).unwrap();
        let mob = mob.downcast_ref::<Mob<Zombie>>().unwrap();
        assert_eq!(mob.core.max_health(), 40.0);
        let mut out = Compound::new();
        tuff_core::world::entity::RegionEntity::save(mob, &mut out);
        let entry = out.get_list("attributes").unwrap()[0]
            .as_compound()
            .unwrap()
            .clone();
        assert_eq!(entry.get_str("id"), Some("minecraft:max_health"));
        let modifier = entry.get_list("modifiers").unwrap()[0]
            .as_compound()
            .unwrap()
            .clone();
        assert_eq!(modifier.get_str("id"), Some("example:boost"));
        assert_eq!(modifier.get_str("operation"), Some("add_multiplied_total"));
    }
}

const EGG_SEARCH_RANGE: i32 = 24;
const EGG_SEARCH_HEIGHT: i32 = 3;
const EGG_STOMP_TICKS: i32 = 60;
const EGG_REACH_SQ: f64 = 1.14 * 1.14;

fn egg_spot(_mob: &MobCore, pos: BlockPos, cx: &TickContext) -> bool {
    let air = |dy| {
        (cx.block)(BlockPos::new(pos.x, pos.y + dy, pos.z))
            .is_some_and(tuff_world_model::chunk::is_air)
    };
    (cx.block)(pos).is_some_and(|s| s.block() == tuff_vanilla_data::Block::TurtleEgg)
        && air(1)
        && air(2)
}

/// Seeks out a turtle egg clutch and stomps on it until it breaks.
#[derive(Debug)]
pub struct AttackTurtleEgg {
    seek: super::goals::MoveToBlock,
    stomping: i32,
}

impl AttackTurtleEgg {
    pub fn new() -> Self {
        let mut seek =
            super::goals::MoveToBlock::new(1.0, EGG_SEARCH_RANGE, EGG_SEARCH_HEIGHT, egg_spot);
        seek.accepted_sq = EGG_REACH_SQ;
        Self { seek, stomping: 0 }
    }

    fn egg_near(mob: &MobCore, cx: &TickContext) -> Option<BlockPos> {
        let here = super::physics::block_pos(mob.base.position);
        let at = |dx, dy, dz| BlockPos::new(here.x + dx, here.y + dy, here.z + dz);
        [
            at(0, 0, 0),
            at(0, -1, 0),
            at(-1, 0, 0),
            at(1, 0, 0),
            at(0, 0, -1),
            at(0, 0, 1),
            at(0, -2, 0),
        ]
        .into_iter()
        .find(|p| (cx.block)(*p).is_some_and(|s| s.block() == tuff_vanilla_data::Block::TurtleEgg))
    }

    fn sound(sound: SoundEvent, category: i32, volume: f32, pos: BlockPos, cx: &mut TickContext) {
        let pitch = 0.9 + cx.rng.r#gen::<f32>() * 0.2;
        let packet = tuff_core::world::framed(&tuff_core::protocol::play::Sound {
            sound_event: sound.id(),
            category,
            position: pos.center(),
            volume,
            pitch,
            seed: cx.rng.r#gen(),
        });
        cx.effects
            .push(Effect::ChunkPackets(tuff_core::world::packets([packet])));
    }
}

impl Default for AttackTurtleEgg {
    fn default() -> Self {
        Self::new()
    }
}

impl super::goal::Goal for AttackTurtleEgg {
    fn flags(&self) -> super::goal::Flags {
        self.seek.flags()
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        cx.shared.game_rules.borrow().bool("mob_griefing") && self.seek.can_use(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.seek.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.seek.start(mob, cx);
        self.stomping = 0;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.base.fall_distance = 1.0;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        use tuff_core::protocol::play;
        self.seek.tick(mob, cx);
        let Some(egg) = Self::egg_near(mob, cx) else {
            return;
        };
        if !self.seek.reached() {
            return;
        }
        if self.stomping > 0 {
            mob.base.velocity[1] = 0.3;
        }
        if self.stomping % 2 == 0 {
            mob.base.velocity[1] = -0.3;
            if self.stomping % 6 == 0 {
                Self::sound(
                    SoundEvent::EntityZombieDestroyEgg,
                    play::sound_category::HOSTILE,
                    0.5,
                    self.seek.pos,
                    cx,
                );
            }
        }
        if self.stomping > EGG_STOMP_TICKS {
            cx.effects.push(Effect::SetBlock {
                pos: egg,
                state: tuff_vanilla_data::Block::Air.default_state(),
            });
            let poof = tuff_core::world::framed(&play::Particles {
                particle: tuff_vanilla_data::registry::ParticleType::Poof.id(),
                position: [
                    f64::from(egg.x) + 0.5,
                    f64::from(egg.y),
                    f64::from(egg.z) + 0.5,
                ],
                spread: [0.02; 3],
                speed: 0.15,
                count: 20,
                alternative: false,
            });
            cx.effects
                .push(Effect::ChunkPackets(tuff_core::world::packets([poof])));
            Self::sound(
                SoundEvent::EntityTurtleEggBreak,
                play::sound_category::BLOCKS,
                0.7,
                egg,
                cx,
            );
        }
        self.stomping += 1;
    }
}
