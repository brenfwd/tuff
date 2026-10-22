//! Axolotls: amphibious hunters that swim and walk, attack the water mobs
//! they hate or hunt, play dead when badly hurt in water, breed on buckets
//! of tropical fish and dry out on land.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{EntityType, MobEffect, SoundEvent};
use tuff_vanilla_data::{Item, tag};

use super::control::{LookStyle, MoveStyle, SmoothSwim};
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{Breed, FindWater, FollowParent, LookAtPlayer, SwimStroll, Tempt};
use super::movement::{Sinking, WaterTravel};
use super::navigation::NavKind;
use super::spawning::{GroupData, SpawnGroup};
use super::target::{TargetView, view};
use super::{Damage, MobCore, MobSounds, OwnAirRules, Species, data};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::entity::{
    Effect, EntityInteraction, InteractOutcome, Notice, TickContext, take_bool, take_number,
};
use tuff_core::world::messages::{HandEffect, PlayerEvent};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::Compound;
use tuff_vanilla_data::entity::PathType;

const SWIM_SPEED: f64 = 0.5;
const LAND_SPEED: f64 = 0.15;
const CHASE_SPEED: f64 = 0.6;
const LOVE_SPEED: f64 = 0.2;
const MAX_AIR: i16 = 6000;
const REHYDRATE_AIR: i16 = 1800;
const PLAY_DEAD_TICKS: i32 = 200;
const HUNTING_COOLDOWN: i32 = 2400;
const ATTACK_COOLDOWN: i32 = 20;
const TARGET_RANGE_SQ: f64 = 64.0;
const RARE_VARIANT_CHANCE: u32 = 1200;

const VARIANT: u8 = data::SPECIES;
const PLAYING_DEAD: u8 = data::SPECIES + 1;
const FROM_BUCKET: u8 = data::SPECIES + 2;

/// Axolotls survive much longer out of water (5 minutes) and rain keeps
/// them wet too; drying out deals its own damage type.
fn air_rules() -> OwnAirRules {
    OwnAirRules {
        max: MAX_AIR,
        in_rain: true,
        damage_kind: &damage_type::DRY_OUT,
        damage: 2.0,
    }
}

/// An axolotl's five colors, in the game's id order; `Blue` is the rare
/// one only breeding can give.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum Color {
    #[default]
    Lucy,
    Wild,
    Gold,
    Cyan,
    Blue,
}

impl Color {
    const ALL: [Self; 5] = [Self::Lucy, Self::Wild, Self::Gold, Self::Cyan, Self::Blue];
    const COMMON: [Self; 4] = [Self::Lucy, Self::Wild, Self::Gold, Self::Cyan];

    fn id(self) -> i32 {
        self as i32
    }

    fn from_id(id: i32) -> Self {
        Self::ALL.get(id as usize).copied().unwrap_or_default()
    }

    fn common(rng: &mut StdRng) -> Self {
        Self::COMMON[rng.gen_range(0..Self::COMMON.len())]
    }
}

fn hunted(kind: EntityType, hunting: bool) -> bool {
    tuff_core::world::entity_in_tag(kind, "axolotl_always_hostiles")
        || hunting && tuff_core::world::entity_in_tag(kind, "axolotl_hunt_targets")
}

fn in_love(mob: &MobCore) -> bool {
    mob.love.as_ref().is_some_and(|l| l.is_in_love())
}

/// Picks the nearest hostile water mob within eight blocks (or prey, when
/// not resting from a hunt: `signal` counts that rest down). Never while
/// breeding.
#[derive(Debug, Default)]
struct FindPrey {
    found: Option<i32>,
}

impl FindPrey {
    fn valid(mob: &MobCore, target: &TargetView, cx: &TickContext) -> bool {
        mob.can_attack(target, cx) && mob.has_line_of_sight(target, cx)
    }
}

impl Goal for FindPrey {
    fn flags(&self) -> Flags {
        Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if in_love(mob) {
            return false;
        }
        let hunting = mob.signal <= 0;
        self.found = cx
            .entities
            .iter()
            .filter(|e| hunted(e.kind, hunting))
            .filter_map(|e| view(cx, e.id))
            .filter(|t| {
                t.in_water
                    && mob.distance_sq(t.position) <= TARGET_RANGE_SQ
                    && Self::valid(mob, t, cx)
            })
            .map(|t| (mob.distance_sq(t.position), t.id))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id);
        self.found.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        !in_love(mob) && mob.target_view(cx).is_some_and(|t| mob.can_attack(&t, cx))
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = self.found;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = None;
    }
}

/// Chases its target, quicker in water, and bites it once a second when in
/// reach.
#[derive(Debug, Default)]
struct Hunt {
    cooldown: i32,
}

impl Goal for Hunt {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx).is_some_and(|t| t.alive)
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        mob.look.set_look_at(target.eye, 45.0, 90.0);
        self.cooldown = (self.cooldown - 1).max(0);
        if mob.within_melee_range(&target) {
            mob.navigation.stop();
            if self.cooldown == 0 && mob.has_line_of_sight(&target, cx) {
                mob.swing(cx);
                mob.hurt_target(&target, cx);
                mob.sound(SoundEvent::EntityAxolotlAttack, 1.0, 1.0, cx);
                self.cooldown = ATTACK_COOLDOWN;
            }
            return;
        }
        let speed = if mob.body.in_water {
            CHASE_SPEED
        } else {
            LAND_SPEED
        };
        mob.move_to_entity(target.position, speed, cx);
    }
}

/// Lies still while playing dead (shown by `seen_as_enemy` being off),
/// healing if it started in water.
#[derive(Debug, Default)]
struct PlayDead;

impl Goal for PlayDead {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK | Flags::JUMP | Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.seen_as_enemy
    }

    fn interruptable(&self) -> bool {
        false
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
        mob.target = None;
        if mob.body.in_water
            && let Some(change) =
                mob.effects
                    .add(ActiveEffect::timed(MobEffect::Regeneration, 200, 0))
        {
            mob.pending_effects.push(change);
        }
    }

    fn tick(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }
}

#[derive(Debug, Default)]
pub struct Axolotl {
    color: Color,
    from_bucket: bool,
    play_dead: i32,
    fighting: bool,
}

impl Axolotl {
    fn set_playing_dead(&mut self, core: &mut MobCore, ticks: i32) {
        let was = self.play_dead > 0;
        self.play_dead = ticks.max(0);
        if was != (self.play_dead > 0) {
            core.seen_as_enemy = self.play_dead <= 0;
            core.changed(PLAYING_DEAD);
        }
    }

    /// Breeding with (or growing up on) a tropical fish bucket, which
    /// leaves an empty water bucket.
    fn feed(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let age = core.age.as_ref().map_or(0, |a| a.age);
        if age == 0 && core.love.as_ref().is_some_and(|l| l.ticks <= 0) {
            core.set_in_love(Some(interaction.player_uuid), cx);
        } else if core
            .age
            .as_ref()
            .is_some_and(super::animal::Age::can_age_up)
        {
            core.age_up(super::animal::speed_up_seconds(-age), true);
        } else {
            return None;
        }
        Some(HandEffect::ExchangeStack(Box::new(ItemStack::mint(
            Item::WaterBucket,
            1,
            MintReason::Spawn,
        ))))
    }
}

impl Species for Axolotl {
    fn brain_state(&self, core: &MobCore) -> crate::brain_debug::BrainState {
        let mut state = crate::brain_debug::BrainState::default();
        if self.play_dead > 0 {
            state.activity = Some("play_dead");
            state
                .memories
                .push(format!("play_dead_ticks: {}", self.play_dead));
        } else if core.target.is_some() {
            state.activity = Some("fight");
        }
        state
    }

    fn pushed_by_fluid(&self) -> bool {
        false
    }

    const RANDOM_BABIES: bool = false;

    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.set_path_malus(PathType::Water, 0.0);
        core.navigation.kind = NavKind::Amphibious;
        core.move_style = MoveStyle::SmoothSwim(SmoothSwim {
            max_turn_x: 85.0,
            max_turn_y: 10.0,
            in_water_speed: 0.1,
            outside_water_speed: 0.5,
            buoyant: false,
        });
        core.look_style = LookStyle::SmoothSwim {
            max_from_center: 20.0,
        };
        core.max_head_x_rot = 1.0;
        core.max_head_y_rot = 1.0;
        core.walk_value = Some(|_, _, _| 0.0);
        core.water_travel = WaterTravel::Swimmer {
            stroke: None,
            sinks: Sinking::Never,
        };
        let mut goals = GoalSelector::default();
        goals.add(0, PlayDead);
        goals.add(1, Hunt::default());
        goals.add(1, Breed::new(LOVE_SPEED).brain());
        goals.add(
            2,
            Tempt::new(SWIM_SPEED, |item| {
                tuff_datapack::tags::is(tag::item::key::AXOLOTL_FOOD, item)
            })
            .on_land(LAND_SPEED),
        );
        goals.add(2, FollowParent::new(CHASE_SPEED).on_land(LAND_SPEED));
        goals.add(3, FindPrey::default());
        goals.add(3, FindWater::walking(6, LAND_SPEED));
        goals.add(4, SwimStroll::amphibious(SWIM_SPEED, LAND_SPEED));
        goals.add(5, LookAtPlayer::new(6.0));
        core.goals = goals;
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let group = cx.state.get::<SpawnGroup>();
        let index = group.index;
        let existing = match group.data {
            Some(GroupData::Axolotls(colors)) => Some(colors),
            _ => None,
        };
        drop(group);
        let colors = match existing {
            Some(colors) => colors,
            None => {
                let colors = [Color::common(cx.rng).id(), Color::common(cx.rng).id()];
                cx.state.get::<SpawnGroup>().data = Some(GroupData::Axolotls(colors));
                colors
            }
        };
        self.color = Color::from_id(colors[cx.rng.gen_range(0..2)]);
        if index >= 2 {
            core.set_baby(true);
        }
    }

    fn tick(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        if core.signal > 0 {
            core.signal -= 1;
        }
        if core.no_ai {
            return;
        }
        if self.play_dead > 0 {
            let left = if in_love(core) { 0 } else { self.play_dead - 1 };
            self.set_playing_dead(core, left);
        }
    }

    fn ai_step(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        let fighting = core.target.is_some();
        if self.fighting && !fighting {
            core.signal = HUNTING_COOLDOWN;
        }
        self.fighting = fighting;
    }

    fn incoming_damage(
        &mut self,
        core: &mut MobCore,
        damage: &Damage,
        amount: f32,
        cx: &mut TickContext,
    ) {
        let health = core.health;
        let roll = !core.no_ai && cx.rng.gen_range(0..3) == 0;
        if roll
            && ((cx.rng.gen_range(0..3) as f32) < amount || health / core.max_health() < 0.5)
            && amount < health
            && core.body.in_water
            && (damage.attacker.is_some() || damage.projectile.is_some())
            && self.play_dead <= 0
        {
            self.set_playing_dead(core, PLAY_DEAD_TICKS);
        }
    }

    fn heard(&mut self, core: &mut MobCore, notice: Notice) {
        if notice == Notice::Rehydrate {
            core.base.air = (core.base.air + REHYDRATE_AIR).min(MAX_AIR);
            core.changed(play::metadata::AIR_SUPPLY);
        }
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        if let Some(id) = take_number(nbt, "Variant").or_else(|| take_number(nbt, "variant")) {
            self.color = Color::from_id(id as i32);
        }
        self.from_bucket = take_bool(nbt, "FromBucket").unwrap_or(false);
        if let Some(ticks) = take_number(nbt, "HuntingCooldown") {
            core.signal = ticks as i32;
        }
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("Variant", self.color.id());
        out.insert("FromBucket", self.from_bucket);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (VARIANT, MetadataValue::VarInt(self.color.id())),
            (PLAYING_DEAD, MetadataValue::Boolean(self.play_dead > 0)),
            (FROM_BUCKET, MetadataValue::Boolean(self.from_bucket)),
        ]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let ambient = if self.play_dead > 0 {
            None
        } else if core.body.in_water {
            Some(SoundEvent::EntityAxolotlIdleWater)
        } else {
            Some(SoundEvent::EntityAxolotlIdleAir)
        };
        MobSounds {
            ambient,
            hurt: Some(SoundEvent::EntityAxolotlHurt),
            death: Some(SoundEvent::EntityAxolotlDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        tuff_datapack::tags::is(tag::item::key::AXOLOTL_FOOD, stack.item())
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if stack.item() == Item::TropicalFishBucket && self.is_food(stack) {
            return self.feed(core, interaction, cx);
        }
        let color = self.color;
        let age = core.age.as_ref().map(|a| (a.age, a.locked));
        let hunting_cooldown = core.signal;
        super::fish::bucket_interact(
            EntityType::Axolotl,
            core,
            interaction,
            SoundEvent::ItemBucketFillAxolotl,
            |nbt| {
                nbt.insert("Variant", color.id());
                if let Some((age, locked)) = age {
                    nbt.insert("Age", age);
                    nbt.insert("AgeLocked", locked);
                }
                if hunting_cooldown > 0 {
                    nbt.insert("HuntingCooldown", i64::from(hunting_cooldown));
                }
            },
            cx,
        )
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        let color = if rng.gen_range(0..RARE_VARIANT_CHANCE) == 0 {
            Color::Blue
        } else if rng.r#gen::<bool>() {
            self.color
        } else {
            partner.color
        };
        Self {
            color,
            ..Self::default()
        }
    }

    fn finalize_offspring(&mut self, core: &mut MobCore) {
        core.persistence_required = true;
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    fn remove_when_far_away(&self) -> bool {
        !self.from_bucket
    }

    fn custom_persistence(&self, _core: &MobCore) -> bool {
        self.from_bucket
    }

    fn air_rules(&self) -> Option<OwnAirRules> {
        Some(air_rules())
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Axolotl, super::load::<Axolotl>);
    registry.register_spawn_rule(EntityType::Axolotl, super::spawning::axolotl_rule);
}

const SUPPORT_RANGE: f64 = 20.0;
const SUPPORT_REGEN: i64 = 100;
const SUPPORT_REGEN_CAP: i64 = 2400;

/// `victim` died to `player`: each axolotl that was attacking it, with the
/// player close by, gives the player Regeneration and clears its Mining
/// Fatigue.
pub fn support_killer(victim: i32, player: i32, cx: &mut TickContext) {
    let Some(at) = cx
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.bounds())
    else {
        return;
    };
    let helped = cx.entities.iter().any(|e| {
        e.kind == EntityType::Axolotl
            && e.living
                .is_some_and(|l| l.alive && l.target == Some(victim))
            && super::physics::intersects(super::physics::inflate(e.bounds, [SUPPORT_RANGE; 3]), at)
    });
    if !helped {
        return;
    }
    for event in [
        PlayerEvent::StackEffect {
            effect: MobEffect::Regeneration,
            add: SUPPORT_REGEN,
            cap: SUPPORT_REGEN_CAP,
        },
        PlayerEvent::RemoveEffect(MobEffect::MiningFatigue),
    ] {
        cx.effects.push(Effect::TellPlayer {
            player,
            event: Box::new(event),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::with_context;

    #[tuff_core::watchdog]
    #[test]
    fn axolotl_data_indices_match_the_dumped_layout() {
        let layout = EntityType::Axolotl.data_layout();
        assert_eq!(layout.len(), FROM_BUCKET as usize + 1);
        assert_eq!(
            layout[VARIANT as usize],
            MetadataValue::VarInt(0).type_id() as u8
        );
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        assert_eq!(layout[PLAYING_DEAD as usize], boolean);
        assert_eq!(layout[FROM_BUCKET as usize], boolean);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_badly_hurt_axolotl_in_water_plays_dead_and_is_no_target() {
        with_context(|core, cx| {
            Axolotl::configure(core);
            core.body.in_water = true;
            let mut axolotl = Axolotl::default();
            let mut hit = Damage::of(&damage_type::MOB_ATTACK);
            hit.attacker = Some((5, EntityType::Drowned));
            for _ in 0..200 {
                axolotl.incoming_damage(core, &hit, 3.0, cx);
                if axolotl.play_dead > 0 {
                    break;
                }
            }
            assert_eq!(axolotl.play_dead, PLAY_DEAD_TICKS);
            assert!(!core.seen_as_enemy);
            assert!(!core.summary().seen_as_enemy);
            for _ in 0..PLAY_DEAD_TICKS {
                axolotl.tick(core, cx);
            }
            assert_eq!(axolotl.play_dead, 0);
            assert!(core.seen_as_enemy, "it gets up again");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn splashed_water_gives_an_axolotl_its_moisture_back() {
        with_context(|core, _cx| {
            let mut axolotl = Axolotl::default();
            core.base.air = 100;
            axolotl.heard(core, Notice::Rehydrate);
            assert_eq!(core.base.air, 100 + REHYDRATE_AIR);
            core.base.air = MAX_AIR - 10;
            axolotl.heard(core, Notice::Rehydrate);
            assert_eq!(core.base.air, MAX_AIR);
        });
    }
}
