//! Armadillos: wander and breed on spider eyes, and roll into a ball when
//! something threatens them or hurts them. Rolled up they take reduced
//! damage, ignore breeding and goals, and unroll once the danger passes. Adults
//! shed a scute every five to ten minutes.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Item, tag};

use super::goal::GoalSelector;
use super::goals::{
    Breed, Float, FollowParent, LookAtPlayer, Panic, RandomLookAround, RandomStroll, Tempt,
};
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::drops::EntityFacts;
use tuff_core::world::entity::{Effect, EntityInteraction, InteractOutcome, TickContext};
use tuff_core::world::game_event::GameEvent;
use tuff_core::world::messages::HandEffect;
use tuff_core::world::region_api::DispenseOutcome;
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::Compound;

use super::Damage;

const BRUSH_WEAR: u32 = 16;
const SCARE_INTERVAL: u64 = 5;
const DANGER_TICKS: i32 = 80;
const DANGER_THRESHOLD: i32 = 75;
const SCARE_RANGE_XZ: f64 = 7.0;
const SCARE_RANGE_Y: f64 = 2.0;
const PEEK_EVENT: i8 = 64;
const SCARED_DURATION: i32 = 50;
const UNROLLING_DURATION: i32 = 30;
const ROLLING_DURATION: i64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    #[default]
    Idle,
    Rolling,
    Scared,
    Unrolling,
}

impl State {
    fn id(self) -> i32 {
        self as i32
    }

    fn name(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Rolling => "rolling",
            State::Scared => "scared",
            State::Unrolling => "unrolling",
        }
    }

    fn from_name(name: &str) -> Self {
        [State::Rolling, State::Scared, State::Unrolling]
            .into_iter()
            .find(|s| s.name() == name)
            .unwrap_or(State::Idle)
    }

    fn threatened(self) -> bool {
        self != State::Idle
    }
}

fn armadillo_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::ARMADILLO_FOOD, item)
}

#[derive(Debug, Default)]
pub struct Armadillo {
    state: State,
    in_state_ticks: i64,
    danger: i32,
    ball_active: bool,
    next_peek: i32,
    danger_was_around: bool,
    scute_time: i32,
}

const SCUTE_TIME_MIN: i32 = 6000;

impl Armadillo {
    fn next_scute_time(rng: &mut StdRng) -> i32 {
        rng.gen_range(0..SCUTE_TIME_MIN) + SCUTE_TIME_MIN
    }

    fn shed_scute(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.scute_time -= 1;
        if self.scute_time > 0 || core.is_baby() || !cx.shared.game_rules.borrow().bool("mob_drops")
        {
            return;
        }
        let facts = EntityFacts {
            kind: Some(core.base.kind),
            tables: Some(cx.shared.loot_tables()),
            ..Default::default()
        };
        let drops = cx.shared.block_loot().entity_drops(
            "minecraft:gameplay/armadillo_shed",
            &facts,
            cx.rng,
        );
        if !drops.is_empty() {
            for stack in drops {
                core.drop_item(stack, 0.0, cx);
            }
            let pitch = (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2 + 1.0;
            core.sound(SoundEvent::EntityArmadilloScuteDrop, 1.0, pitch, cx);
            cx.effects.push(Effect::GameEvent {
                event: GameEvent::EntityPlace,
                position: core.base.position,
                source: Some(core.base.id),
            });
        }
        self.scute_time = Self::next_scute_time(cx.rng);
    }

    fn scared(&self) -> bool {
        self.state != State::Idle
    }

    fn switch(&mut self, state: State, core: &mut MobCore) {
        if self.state != state {
            self.state = state;
            self.in_state_ticks = 0;
            core.changed(data::SPECIES);
        }
    }

    fn can_stay_rolled_up(core: &MobCore) -> bool {
        !core.is_panicking()
            && !core.in_liquid()
            && core.leash.is_none()
            && core.base.vehicle.is_none()
            && core.base.riders.is_empty()
            && core.base.entity_riders.is_empty()
    }

    fn roll_up(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.scared() {
            return;
        }
        core.navigation.stop();
        if let Some(love) = &mut core.love {
            love.reset();
        }
        self.game_event(core, cx);
        core.sound(SoundEvent::EntityArmadilloRoll, 1.0, 1.0, cx);
        self.switch(State::Rolling, core);
    }

    fn roll_out(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !self.scared() {
            return;
        }
        self.game_event(core, cx);
        core.sound(SoundEvent::EntityArmadilloUnrollFinish, 1.0, 1.0, cx);
        self.switch(State::Idle, core);
    }

    fn game_event(&self, core: &MobCore, cx: &mut TickContext) {
        cx.effects.push(Effect::GameEvent {
            event: GameEvent::EntityAction,
            position: core.base.position,
            source: Some(core.base.id),
        });
    }

    fn scares(&self, core: &MobCore, cx: &TickContext) -> bool {
        let mut area = core.bounds();
        for axis in 0..3 {
            let reach = if axis == 1 {
                SCARE_RANGE_Y
            } else {
                SCARE_RANGE_XZ
            };
            area[axis] -= reach;
            area[axis + 3] += reach;
        }
        let near = |bounds: [f64; 6]| tuff_core::world::physics::intersects(area, bounds);
        let hurt_by = core.last_hurt_by_mob.map(|(id, _)| id);
        let entity = cx.entities.iter().any(|e| {
            e.living.is_some()
                && e.id != core.base.id
                && near(e.bounds)
                && (hurt_by == Some(e.id)
                    || tuff_datapack::tags::is(tag::entity_type::key::UNDEAD, e.kind))
        });
        entity
            || cx.players.iter().any(|p| {
                near(p.bounds()) && (hurt_by == Some(p.id) || p.sprinting || p.vehicle.is_some())
            })
    }

    fn pick_peek(cx: &mut TickContext) -> i32 {
        SCARED_DURATION + cx.rng.gen_range(100..=240)
    }

    fn ball_up(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.next_peek > 0 {
            self.next_peek -= 1;
        }
        if self.state == State::Rolling && self.in_state_ticks > ROLLING_DURATION {
            self.switch(State::Scared, core);
            if core.base.on_ground {
                core.sound(SoundEvent::EntityArmadilloLand, 1.0, 1.0, cx);
            }
            return;
        }
        let around = self.danger > DANGER_THRESHOLD;
        if around != self.danger_was_around {
            self.next_peek = Self::pick_peek(cx);
        }
        self.danger_was_around = around;
        match self.state {
            State::Scared => {
                if self.next_peek == 0 && core.base.on_ground && around {
                    core.entity_event(PEEK_EVENT, cx);
                    self.next_peek = Self::pick_peek(cx);
                }
                if self.danger < UNROLLING_DURATION {
                    core.sound(SoundEvent::EntityArmadilloUnrollStart, 1.0, 1.0, cx);
                    self.switch(State::Unrolling, core);
                }
            }
            State::Unrolling if self.danger > UNROLLING_DURATION => {
                self.switch(State::Scared, core);
            }
            _ => {}
        }
    }
}

impl Species for Armadillo {
    fn create(rng: &mut StdRng) -> Self {
        Self {
            scute_time: Self::next_scute_time(rng),
            ..Self::default()
        }
    }

    fn tick(&mut self, _core: &mut MobCore, _cx: &mut TickContext) {
        if self.danger > 0 {
            self.danger -= 1;
        }
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.shed_scute(core, cx);
        if (cx.tick + core.base.id as u64).is_multiple_of(SCARE_INTERVAL) {
            if !Self::can_stay_rolled_up(core) {
                self.danger = 0;
            } else if self.scares(core, cx) {
                self.danger = DANGER_TICKS;
            }
        }
        if self.danger <= 0 {
            self.ball_active = false;
            self.roll_out(core, cx);
        } else if !core.is_panicking() {
            if !self.ball_active {
                if core.base.on_ground && !core.in_liquid() {
                    self.ball_active = true;
                    self.roll_up(core, cx);
                }
            } else if self.state.threatened() {
                self.ball_up(core, cx);
            } else {
                self.ball_active = false;
            }
        } else if self.ball_active {
            self.ball_active = false;
            if !Self::can_stay_rolled_up(core) {
                self.roll_out(core, cx);
            }
        }
        self.in_state_ticks += 1;
    }

    fn immobile(&self, _core: &MobCore) -> bool {
        self.scared()
    }

    fn can_fall_in_love(&self, _core: &MobCore) -> bool {
        !self.scared()
    }

    fn scale_damage(&self, _core: &MobCore, _damage: &Damage, amount: f32) -> f32 {
        if self.scared() {
            ((amount - 1.0) / 2.0).max(0.0)
        } else {
            amount
        }
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) {
        if core.no_ai {
            return;
        }
        if damage.attacker.is_some() {
            self.danger = DANGER_TICKS;
            if Self::can_stay_rolled_up(core) {
                self.roll_up(core, cx);
            }
        } else if tuff_core::world::damage_type_in_tag(
            damage.kind,
            "minecraft:panic_environmental_causes",
        ) {
            self.roll_out(core, cx);
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let brush = interaction
            .stack
            .as_ref()
            .is_some_and(|stack| stack.item == Item::Brush);
        if brush && !core.is_baby() {
            core.drop_item(
                ItemStack::mint(Item::ArmadilloScute, 1, MintReason::Spawn),
                0.0,
                cx,
            );
            core.sound(SoundEvent::EntityArmadilloBrush, 1.0, 1.0, cx);
            cx.effects.push(Effect::GameEvent {
                event: GameEvent::EntityInteract,
                position: core.base.position,
                source: Some(core.base.id),
            });
            return Some(HandEffect::Damage(BRUSH_WEAR));
        }
        self.scared().then_some(HandEffect::None)
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        if let Some(name) = nbt.get_str("state") {
            self.state = State::from_name(name);
        }
        if let Some(time) = tuff_core::world::entity::take_number(nbt, "scute_time") {
            self.scute_time = time as i32;
        }
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("state", self.state.name());
        out.insert("scute_time", self.scute_time);
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        let mut goals = GoalSelector::default();
        goals.add(0, Float::new(core));
        goals.add(1, Panic::new(2.0));
        goals.add(2, Breed::new(1.0).brain());
        goals.add(3, Tempt::new(1.25, armadillo_food));
        goals.add(4, FollowParent::new(1.25));
        goals.add(5, RandomStroll::water_avoiding(1.0));
        goals.add(6, LookAtPlayer::new(6.0));
        goals.add(7, RandomLookAround::default());
        core.goals = goals;
    }

    fn dispensed_on(
        &mut self,
        core: &mut MobCore,
        stack: &tuff_core::world::region_api::Fired,
        cx: &mut TickContext,
    ) -> Option<DispenseOutcome> {
        if stack.item != Item::Brush || core.is_baby() {
            return None;
        }
        core.drop_item(
            ItemStack::mint(Item::ArmadilloScute, 1, MintReason::Spawn),
            0.0,
            cx,
        );
        core.sound(SoundEvent::EntityArmadilloBrush, 1.0, 1.0, cx);
        Some(DispenseOutcome::Worn(BRUSH_WEAR))
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(
            data::SPECIES,
            MetadataValue::Holder {
                serializer: core.base.kind.data_serializer(data::SPECIES).unwrap_or(0),
                id: self.state.id(),
            },
        )]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: (!self.scared()).then_some(SoundEvent::EntityArmadilloAmbient),
            hurt: Some(if self.scared() {
                SoundEvent::EntityArmadilloHurtReduced
            } else {
                SoundEvent::EntityArmadilloHurt
            }),
            death: Some(SoundEvent::EntityArmadilloDeath),
            step: Some(SoundEvent::EntityArmadilloStep),
            volume: 1.0,
        }
    }

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        Some(SoundEvent::EntityArmadilloEat)
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        armadillo_food(stack.item)
    }

    fn offspring(&self, _partner: &Self, rng: &mut StdRng) -> Self {
        Self::create(rng)
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

/// Registers the armadillo's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Armadillo, super::load::<Armadillo>);
    registry.register_spawn_rule(EntityType::Armadillo, super::spawning::animal_rule);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::world::entity::NearbyPlayer;
    use tuff_vanilla_data::Block;

    fn player(position: [f64; 3], sprinting: bool) -> NearbyPlayer<'static> {
        NearbyPlayer {
            id: 1,
            uuid: 1,
            name: "",
            position,
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
            sprinting,
        }
    }

    fn flat(pos: tuff_world_model::BlockPos) -> Option<tuff_vanilla_data::BlockState> {
        Some(if pos.y < 64 {
            Block::GrassBlock.default_state()
        } else {
            Block::Air.default_state()
        })
    }

    fn standing(core: &mut MobCore) {
        core.base.on_ground = true;
        core.base.position = [0.5, 64.0, 0.5];
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_sprinting_player_rolls_it_up_then_it_unrolls_once_they_leave() {
        let runner = [player([3.5, 64.0, 0.5], true)];
        crate::tests::with_players(&flat, &[], &runner, |core, cx| {
            Armadillo::configure(core);
            standing(core);
            let mut armadillo = Armadillo::default();
            for tick in 0..40 {
                cx.tick = tick;
                armadillo.tick(core, cx);
                armadillo.ai_step(core, cx);
            }
            assert_eq!(armadillo.state, State::Scared);
            assert!(armadillo.immobile(core));
            assert!(!armadillo.can_fall_in_love(core));
            assert_eq!(armadillo.scale_damage(core, &test_damage(), 5.0), 2.0);
            assert_eq!(armadillo.scale_damage(core, &test_damage(), 0.5), 0.0);
        });
        crate::tests::with_context(|core, cx| {
            Armadillo::configure(core);
            standing(core);
            let mut armadillo = Armadillo {
                state: State::Scared,
                danger: 20,
                ball_active: true,
                ..Armadillo::default()
            };
            for tick in 0..200 {
                cx.tick = tick;
                armadillo.tick(core, cx);
                armadillo.ai_step(core, cx);
            }
            assert_eq!(armadillo.state, State::Idle);
            assert!(!armadillo.immobile(core));
            assert_eq!(armadillo.scale_damage(core, &test_damage(), 5.0), 5.0);
        });
    }

    fn brushing(item: Item) -> EntityInteraction {
        EntityInteraction {
            player: 1,
            player_uuid: 1,
            position: [0.0; 3],
            location: [0.0; 3],
            stack: Some(tuff_item::item::ItemSnapshot::describe(
                item,
                1,
                Default::default(),
            )),
            hand: tuff_core::world::entity::HandOffer::new(
                (Some(tuff_item::item::ItemSnapshot::describe(
                    item,
                    1,
                    Default::default(),
                )))
                .as_deref()
                .map(tuff_item::item::ItemStack::test_copy),
            ),
            off_hand: false,
            creative: false,
            sneaking: false,
            riding: false,
            hero: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn brushing_an_adult_drops_a_scute_and_wears_the_brush_even_when_scared() {
        crate::tests::with_context(|core, cx| {
            Armadillo::configure(core);
            let mut armadillo = Armadillo::default();
            let before = cx.effects.len();
            let outcome = armadillo.interact(core, &brushing(Item::Brush), cx);
            assert_eq!(outcome, Some(HandEffect::Damage(BRUSH_WEAR)));
            assert!(cx.effects.len() > before);
            armadillo.state = State::Scared;
            let outcome = armadillo.interact(core, &brushing(Item::Brush), cx);
            assert_eq!(outcome, Some(HandEffect::Damage(BRUSH_WEAR)));
            let outcome = armadillo.interact(core, &brushing(Item::Stick), cx);
            assert_eq!(outcome, Some(HandEffect::None));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn brushing_a_baby_does_nothing() {
        crate::tests::with_context(|core, cx| {
            Armadillo::configure(core);
            core.set_age(-24000);
            let mut armadillo = Armadillo::default();
            let before = cx.effects.len();
            assert_eq!(armadillo.interact(core, &brushing(Item::Brush), cx), None);
            assert_eq!(cx.effects.len(), before);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_walking_player_does_not_scare_it() {
        let walker = [player([3.5, 64.0, 0.5], false)];
        crate::tests::with_players(&flat, &[], &walker, |core, cx| {
            Armadillo::configure(core);
            standing(core);
            let mut armadillo = Armadillo::default();
            for tick in 0..40 {
                cx.tick = tick;
                armadillo.tick(core, cx);
                armadillo.ai_step(core, cx);
            }
            assert_eq!(armadillo.state, State::Idle);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_mob_attacker_rolls_it_up_at_once() {
        crate::tests::with_context(|core, cx| {
            Armadillo::configure(core);
            standing(core);
            let mut armadillo = Armadillo::default();
            let mut damage = test_damage();
            damage.attacker = Some((9, EntityType::Zombie));
            armadillo.after_hurt(core, &damage, cx);
            assert_eq!(armadillo.state, State::Rolling);
            assert_eq!(armadillo.danger, DANGER_TICKS);
        });
    }

    fn test_damage() -> Damage {
        Damage::of(&tuff_vanilla_data::damage_type::GENERIC)
    }

    #[tuff_core::watchdog]
    #[test]
    fn it_sheds_a_scute_when_the_timer_runs_out_and_resets_it() {
        crate::tests::with_world(&flat, &[], |core, cx| {
            let mut armadillo = Armadillo {
                scute_time: 1,
                ..Armadillo::default()
            };
            armadillo.shed_scute(core, cx);
            assert!(cx.effects.iter().any(|e| matches!(
                e,
                Effect::DropItem { stack, .. } if stack.item == Item::ArmadilloScute
            )));
            assert!(armadillo.scute_time >= SCUTE_TIME_MIN);
        });
    }
}
