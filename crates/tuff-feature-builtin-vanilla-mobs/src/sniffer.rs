//! Sniffers: wander and breed on torchflower seeds, and now and then
//! scent, sniff out a diggable block, walk to it, dig up a seed and rise
//! happy. A held cooldown spaces the digging out.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};
use tuff_vanilla_data::{Item, tag};

use super::Mob;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{
    Breed, Float, FollowParent, LookAtPlayer, Panic, RandomLookAround, RandomStroll, Tempt,
};
use super::physics::block_pos;
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::entity::{Effect, RegionEntity, TickContext};
use tuff_item::item::{ItemStack, MintReason};
use tuff_world_model::BlockPos;

const DROP_SEED_AT_TICK: u8 = data::SPECIES + 1;
const STATE: u8 = data::SPECIES;
const IDLING: i32 = 0;
const FEELING_HAPPY: i32 = 1;
const SCENTING: i32 = 2;
const SNIFFING: i32 = 3;
const SEARCHING: i32 = 4;
const DIGGING: i32 = 5;
const RISING: i32 = 6;

const SNIFF_COOLDOWN_TICKS: i32 = 9600;
const DROP_SEED_OFFSET: i32 = 120;
const RISING_TICKS: i32 = 40;
const SEARCH_TIMEOUT: i32 = 600;
const EXPLORED_LIMIT: usize = 21;
const DIGGING_ENTITY_EVENT: i8 = 63;
const SNIFFER_DIGGING_TABLE: &str = "minecraft:gameplay/sniffer_digging";

fn sniffer_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::SNIFFER_FOOD, item)
}

/// Stands still, looking nowhere in particular, while a sniffing
/// behavior runs; panic, breeding and tempting outrank it.
#[derive(Debug)]
struct Hold;

impl Goal for Hold {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.busy
    }
}

#[derive(Debug, Default)]
pub struct Sniffer {
    state: i32,
    timer: i32,
    elapsed: i32,
    sniff_cooldown: i32,
    next_idle: i32,
    target: Option<BlockPos>,
    digging: bool,
    happy: bool,
    drop_at: i32,
    explored: Vec<(DimensionId, BlockPos)>,
}

fn down(pos: BlockPos) -> BlockPos {
    BlockPos::new(pos.x, pos.y - 1, pos.z)
}

fn in_love(core: &MobCore) -> bool {
    core.love.as_ref().is_some_and(|l| l.is_in_love())
}

fn tempted(core: &MobCore, cx: &TickContext) -> bool {
    let range = core.attribute(Attribute::TemptRange);
    cx.players.iter().any(|p| {
        tuff_core::world::distance_sq(core.base.position, p.position) <= range * range
            && p.held.iter().flatten().any(|item| sniffer_food(*item))
    })
}

impl Sniffer {
    fn can_sniff(core: &MobCore, cx: &TickContext) -> bool {
        !(tempted(core, cx)
            || core.panicking
            || core.body.in_water
            || in_love(core)
            || !core.base.on_ground
            || core.base.vehicle.is_some()
            || core.leash.is_some())
    }

    fn head_block(core: &MobCore) -> BlockPos {
        let yaw = f64::from(core.base.yaw).to_radians();
        let [x, y, z] = core.base.position;
        block_pos([x - yaw.sin() * 2.25, y + 0.2, z + yaw.cos() * 2.25])
    }

    fn can_dig_at(&self, core: &mut MobCore, pos: BlockPos, cx: &mut TickContext) -> bool {
        (cx.block)(pos).is_some_and(|s| {
            tuff_datapack::tags::is(tag::block::key::SNIFFER_DIGGABLE_BLOCK, s.block())
        }) && !self.explored.contains(&(cx.dimension, pos))
            && core
                .create_path(pos, 1, false, cx)
                .is_some_and(|path| path.reached)
    }

    fn can_dig(&self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        if core.panicking
            || tempted(core, cx)
            || core.is_baby()
            || core.body.in_water
            || !core.base.on_ground
            || core.base.vehicle.is_some()
        {
            return false;
        }
        let below = down(Self::head_block(core));
        self.can_dig_at(core, below, cx)
    }

    fn dig_position(&self, core: &mut MobCore, cx: &mut TickContext) -> Option<BlockPos> {
        (0..5).find_map(|i| {
            let pos = super::random_pos::land_pos(core, 10 + 2 * i, 3, cx)?;
            let below = down(block_pos(pos));
            self.can_dig_at(core, below, cx).then_some(below)
        })
    }

    fn transition(&mut self, state: i32, core: &mut MobCore, cx: &mut TickContext) {
        self.state = state;
        self.elapsed = 0;
        core.changed(STATE);
        match state {
            FEELING_HAPPY => core.sound(SoundEvent::EntitySnifferHappy, 1.0, 1.0, cx),
            SCENTING => {
                let pitch = if core.is_baby() { 1.3 } else { 1.0 };
                core.sound(SoundEvent::EntitySnifferScenting, 1.0, pitch, cx);
            }
            SNIFFING => core.sound(SoundEvent::EntitySnifferSniffing, 1.0, 1.0, cx),
            DIGGING => {
                self.drop_at = DROP_SEED_OFFSET;
                core.changed(DROP_SEED_AT_TICK);
                core.entity_event(DIGGING_ENTITY_EVENT, cx);
            }
            RISING => core.sound(SoundEvent::EntitySnifferDiggingStop, 1.0, 1.0, cx),
            _ => {}
        }
    }

    fn reset(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.digging = false;
        self.target = None;
        if self.state != IDLING {
            self.transition(IDLING, core, cx);
        }
    }

    fn drop_seed(&self, core: &mut MobCore, cx: &mut TickContext) {
        let head = Self::head_block(core);
        for stack in cx.shared.block_loot().roll(SNIFFER_DIGGING_TABLE, cx.rng) {
            cx.effects.push(Effect::DropItem {
                stack,
                position: [f64::from(head.x), f64::from(head.y), f64::from(head.z)],
                velocity: [0.0; 3],
                pickup_delay: 10,
            });
        }
        core.sound(SoundEvent::EntitySnifferDropSeed, 1.0, 1.0, cx);
    }

    fn idle(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.happy {
            self.timer = cx.rng.gen_range(40..100);
            self.transition(FEELING_HAPPY, core, cx);
            return;
        }
        if self.next_idle > 0 {
            self.next_idle -= 1;
            return;
        }
        self.next_idle = cx.rng.gen_range(5..20);
        match cx.rng.gen_range(0..8) {
            0 if !core.panicking && !in_love(core) && !tempted(core, cx) => {
                self.timer = cx.rng.gen_range(40..80);
                self.transition(SCENTING, core, cx);
            }
            1 if !core.is_baby() && self.sniff_cooldown == 0 && Self::can_sniff(core, cx) => {
                self.timer = cx.rng.gen_range(40..80);
                self.transition(SNIFFING, core, cx);
            }
            _ => {}
        }
    }

    fn finish_sniffing(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let Some(pos) = self.dig_position(core, cx) else {
            self.transition(IDLING, core, cx);
            return;
        };
        let path = core.create_path(pos, 1, false, cx);
        if path.is_none() {
            self.transition(IDLING, core, cx);
            return;
        }
        self.target = Some(pos);
        core.move_along(path, 1.25, cx);
        self.transition(SEARCHING, core, cx);
    }

    fn finish_searching(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.navigation.stop();
        self.target = None;
        if self.can_dig(core, cx) && Self::can_sniff(core, cx) {
            self.timer = cx.rng.gen_range(160..180);
            self.digging = true;
            self.transition(DIGGING, core, cx);
        } else {
            self.transition(IDLING, core, cx);
        }
    }

    fn step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.elapsed += 1;
        match self.state {
            FEELING_HAPPY => {
                self.timer -= 1;
                if self.timer <= 0 {
                    self.happy = false;
                    self.transition(IDLING, core, cx);
                }
            }
            SCENTING => {
                self.timer -= 1;
                if self.timer <= 0 || core.panicking || in_love(core) || tempted(core, cx) {
                    self.transition(IDLING, core, cx);
                }
            }
            SNIFFING => {
                self.timer -= 1;
                if !Self::can_sniff(core, cx) {
                    self.transition(IDLING, core, cx);
                } else if self.timer <= 0 {
                    self.finish_sniffing(core, cx);
                }
            }
            SEARCHING => {
                if !Self::can_sniff(core, cx) {
                    self.reset(core, cx);
                } else if core.navigation.is_done() || self.elapsed >= SEARCH_TIMEOUT {
                    self.finish_searching(core, cx);
                }
            }
            DIGGING => {
                if !self.can_dig(core, cx) || in_love(core) {
                    self.reset(core, cx);
                    return;
                }
                if self.elapsed == DROP_SEED_OFFSET {
                    self.drop_seed(core, cx);
                }
                self.timer -= 1;
                if self.timer <= 0 {
                    self.sniff_cooldown = SNIFF_COOLDOWN_TICKS;
                    self.transition(RISING, core, cx);
                }
            }
            RISING => {
                if self.elapsed >= RISING_TICKS {
                    self.explored
                        .insert(0, (cx.dimension, block_pos(core.base.position)));
                    self.explored.truncate(EXPLORED_LIMIT);
                    self.digging = false;
                    self.happy = true;
                    self.transition(IDLING, core, cx);
                }
            }
            _ => self.idle(core, cx),
        }
    }
}

impl Species for Sniffer {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.sniff_cooldown > 0 {
            self.sniff_cooldown -= 1;
        }
        if core.no_ai {
            return;
        }
        if self.state != IDLING && self.state != RISING && (core.panicking || in_love(core)) {
            self.reset(core, cx);
        }
        self.step(core, cx);
        core.busy = self.state != IDLING;
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        let mut goals = GoalSelector::default();
        goals.add(0, Float::new(core));
        goals.add(1, Panic::new(2.0));
        goals.add(2, Breed::new(1.0).brain());
        goals.add(3, Tempt::new(1.0, sniffer_food));
        goals.add(4, FollowParent::new(1.0));
        goals.add(5, Hold);
        goals.add(6, RandomStroll::water_avoiding(1.0));
        goals.add(7, LookAtPlayer::new(6.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (
                data::SPECIES,
                MetadataValue::Holder {
                    serializer: core.base.kind.data_serializer(data::SPECIES).unwrap_or(0),
                    id: self.state,
                },
            ),
            (
                DROP_SEED_AT_TICK,
                MetadataValue::VarInt(if self.state == DIGGING {
                    core.tick_count as i32 + self.drop_at - self.elapsed
                } else {
                    0
                }),
            ),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntitySnifferIdle),
            hurt: Some(SoundEvent::EntitySnifferHurt),
            death: Some(SoundEvent::EntitySnifferDeath),
            step: Some(SoundEvent::EntitySnifferStep),
            volume: 1.0,
        }
    }

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        Some(SoundEvent::EntitySnifferEat)
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        sniffer_food(stack.item)
    }

    fn offspring(&self, _partner: &Self, _rng: &mut StdRng) -> Self {
        Self::default()
    }

    /// Bred sniffers plop an egg item instead of a baby.
    fn breed(
        mob: &mut Mob<Self>,
        partner: &mut Mob<Self>,
        cx: &mut TickContext,
    ) -> Option<Box<dyn RegionEntity>> {
        let can_mate = |s: &Sniffer| matches!(s.state, IDLING | SCENTING | FEELING_HAPPY);
        if !can_mate(&mob.species)
            || !can_mate(&partner.species)
            || !super::animal::ready_to_breed(&mob.core)
            || !super::animal::ready_to_breed(&partner.core)
        {
            return None;
        }
        let cause = [&mob.core, &partner.core]
            .into_iter()
            .find_map(|parent| parent.love.as_ref().and_then(|love| love.cause));
        if let Some(player) = cause {
            cx.effects.push(Effect::Advancement {
                player,
                event: Box::new(tuff_core::world::messages::AdvancementEvent::BredAnimals {
                    parent: mob.core.facts(),
                    partner: partner.core.facts(),
                    child: None,
                }),
            });
        }
        for parent in [&mut mob.core, &mut partner.core] {
            parent.set_age(super::animal::PARENT_AGE_AFTER_BREEDING);
            if let Some(love) = &mut parent.love {
                love.reset();
            }
        }
        mob.core.entity_event(super::event::IN_LOVE, cx);
        if cx.shared.game_rules.borrow().bool("mob_drops") {
            cx.effects.push(Effect::Experience {
                position: mob.core.base.position,
                amount: cx.rng.gen_range(1..=7),
            });
        }
        let pitch = (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2 + 0.5;
        mob.core
            .sound(SoundEvent::BlockSnifferEggPlop, 1.0, pitch, cx);
        cx.effects.push(Effect::DropItem {
            stack: ItemStack::mint(Item::SnifferEgg, 1, MintReason::Spawn),
            position: mob.core.base.position,
            velocity: [0.0; 3],
            pickup_delay: 10,
        });
        None
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        1 + rng.gen_range(0..3)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

/// Registers the sniffer's loader.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Sniffer, super::load::<Sniffer>);
    registry.register_spawn_rule(EntityType::Sniffer, super::spawning::animal_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn standing(core: &mut MobCore) {
        core.base.on_ground = true;
        core.base.position = [0.5, 64.0, 0.5];
        core.base.yaw = 0.0;
    }

    #[tuff_core::watchdog]
    #[test]
    fn digging_drops_a_seed_then_rises_happy_and_remembers_the_spot() {
        crate::tests::with_context(|core, cx| {
            Sniffer::configure(core);
            standing(core);
            let mut sniffer = Sniffer {
                timer: 160,
                digging: true,
                ..Sniffer::default()
            };
            sniffer.transition(DIGGING, core, cx);
            let mut dropped = 0;
            for _ in 0..120 {
                sniffer.ai_step(core, cx);
            }
            dropped += cx
                .effects
                .iter()
                .filter(|e| matches!(e, Effect::DropItem { .. }))
                .count();
            assert!(dropped > 0, "a seed drops 120 ticks in");
            assert_eq!(sniffer.state, DIGGING);
            for _ in 0..40 {
                sniffer.ai_step(core, cx);
            }
            assert_eq!(sniffer.state, RISING);
            assert_eq!(sniffer.sniff_cooldown, SNIFF_COOLDOWN_TICKS);
            for _ in 0..RISING_TICKS {
                sniffer.ai_step(core, cx);
            }
            assert!(sniffer.happy);
            assert_eq!(sniffer.explored.len(), 1);
            assert_eq!(sniffer.state, IDLING);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn breeding_drops_an_egg_instead_of_a_baby_and_needs_calm_states() {
        use crate::tests::with_context;
        with_context(|core, cx| {
            let base = core.base.clone();
            let mut a = Mob::<Sniffer>::new(base.clone(), cx.rng);
            let mut b = Mob::<Sniffer>::new(base, cx.rng);
            for m in [&mut a, &mut b] {
                m.core.make_animal();
                m.core.set_in_love(None, cx);
            }
            a.species.state = DIGGING;
            assert!(Sniffer::breed(&mut a, &mut b, cx).is_none());
            assert!(
                !cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::DropItem { .. }))
            );
            a.species.state = SCENTING;
            assert!(Sniffer::breed(&mut a, &mut b, cx).is_none());
            let eggs = cx
                .effects
                .iter()
                .filter(|e| matches!(e, Effect::DropItem { stack, .. } if stack.item == Item::SnifferEgg))
                .count();
            assert_eq!(eggs, 1);
            assert!(!a.core.love.as_ref().unwrap().is_in_love());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn panic_cancels_sniffing_and_the_cooldown_blocks_a_new_one() {
        crate::tests::with_context(|core, cx| {
            Sniffer::configure(core);
            standing(core);
            let mut sniffer = Sniffer {
                timer: 60,
                ..Sniffer::default()
            };
            sniffer.transition(SNIFFING, core, cx);
            core.panicking = true;
            sniffer.ai_step(core, cx);
            assert_eq!(sniffer.state, IDLING);
            assert!(!core.busy);
            core.panicking = false;
            sniffer.sniff_cooldown = SNIFF_COOLDOWN_TICKS;
            for _ in 0..2000 {
                sniffer.ai_step(core, cx);
                assert_ne!(sniffer.state, SNIFFING);
            }
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_state_and_seed_tick_go_out_as_metadata() {
        crate::tests::with_context(|core, cx| {
            standing(core);
            let mut sniffer = Sniffer::default();
            sniffer.transition(DIGGING, core, cx);
            let values = sniffer.metadata(core);
            assert!(matches!(
                values[0].1,
                MetadataValue::Holder { id: DIGGING, .. }
            ));
            assert!(
                matches!(values[1].1, MetadataValue::VarInt(at) if at == core.tick_count as i32 + DROP_SEED_OFFSET)
            );
        });
    }
}
