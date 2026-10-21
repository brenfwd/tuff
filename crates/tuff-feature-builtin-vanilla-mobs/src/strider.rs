//! Striders: they walk on lava, shiver and slow down off it, and breed on
//! warped fungus. A saddled one is steered with a warped fungus on a stick.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::item::ModifierOperation;
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};
use tuff_vanilla_data::{Item, tag};

use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{
    Breed, FollowParent, LookAtPlayer, Panic, RandomLookAround, RandomStroll, Tempt,
};
use super::physics::block_pos;
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{EntityInteraction, InteractOutcome, RiderInput, TickContext};
use tuff_item::item::{ItemStack, MintReason};
use tuff_world_model::BlockPos;

const BOOST_TIME: u8 = data::SPECIES;
const SUFFOCATING: u8 = data::SPECIES + 1;
const SUFFOCATING_MODIFIER: &str = "minecraft:suffocating";
const SUFFOCATING_SPEED_FACTOR: f64 = -0.34;

fn strider_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::STRIDER_FOOD, item)
}

fn strider_tempt(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::STRIDER_TEMPT_ITEMS, item)
}

fn on_warm_block(mob: &MobCore, cx: &TickContext) -> bool {
    if mob.in_lava() {
        return true;
    }
    let pos = block_pos(mob.base.position);
    let inside = (cx.block)(pos)
        .is_some_and(|s| tuff_datapack::tags::is(tag::block::key::STRIDER_WARM_BLOCKS, s.block()));
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    let on = (cx.block)(below)
        .is_some_and(|s| tuff_datapack::tags::is(tag::block::key::STRIDER_WARM_BLOCKS, s.block()));
    inside || on
}

fn strider_walk_target_value(_mob: &MobCore, pos: BlockPos, cx: &TickContext) -> f32 {
    if (cx.block)(pos).is_some_and(|s| {
        !s.fluid_state().is_empty()
            && tuff_datapack::tags::is(tag::fluid::key::LAVA, s.fluid_state().fluid())
    }) {
        10.0
    } else {
        0.0
    }
}

/// Walks to the nearest lava it can see within eight blocks, while it is
/// not already standing on any.
#[derive(Debug, Default)]
struct GoToLava {
    target: Option<BlockPos>,
    retry: i32,
}

impl GoToLava {
    fn nearest(mob: &MobCore, cx: &TickContext) -> Option<BlockPos> {
        let here = block_pos(mob.base.position);
        let mut found: Option<(i32, BlockPos)> = None;
        for dx in -8i32..=8 {
            for dz in -8i32..=8 {
                for dy in -2i32..=2 {
                    let pos = BlockPos::new(here.x + dx, here.y + dy, here.z + dz);
                    let is_lava = (cx.block)(pos).is_some_and(|s| {
                        !s.fluid_state().is_empty()
                            && tuff_datapack::tags::is(
                                tag::fluid::key::LAVA,
                                s.fluid_state().fluid(),
                            )
                    });
                    if !is_lava {
                        continue;
                    }
                    let distance = dx * dx + dy * dy + dz * dz;
                    if found.is_none_or(|(d, _)| distance < d) {
                        found = Some((distance, pos));
                    }
                }
            }
        }
        found.map(|(_, pos)| pos)
    }
}

impl Goal for GoToLava {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.in_lava() {
            return false;
        }
        self.target = Self::nearest(mob, cx);
        self.target.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        !mob.in_lava()
            && self.target.is_some_and(|pos| {
                (cx.block)(pos).is_some_and(|s| {
                    !s.fluid_state().is_empty()
                        && tuff_datapack::tags::is(tag::fluid::key::LAVA, s.fluid_state().fluid())
                })
            })
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.retry = 0;
        self.go(mob, cx);
    }

    fn every_tick(&self) -> bool {
        true
    }

    /// Retries the path every twenty ticks while it has none, in case the
    /// first search failed (unloaded chunks, a budget shortage).
    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if mob.navigation.is_done() {
            self.retry -= 1;
            if self.retry <= 0 {
                self.retry = 20;
                self.go(mob, cx);
            }
        }
    }
}

impl GoToLava {
    fn go(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(pos) = self.target {
            let target = [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5];
            mob.move_to_position(target, 1.0, cx);
        }
    }
}

#[derive(Debug, Default)]
pub struct Strider {
    suffocating: bool,
    steering: super::steering::ItemSteering,
}

impl Species for Strider {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Water, -1.0);
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Lava, 0.0);
        core.set_path_malus(tuff_vanilla_data::entity::PathType::FireInNeighbor, 0.0);
        core.set_path_malus(tuff_vanilla_data::entity::PathType::Fire, 0.0);
        core.walk_value = Some(strider_walk_target_value);
        core.navigation.can_float = true;
        core.navigation.floats_on = super::path::floats_on_lava;
        core.navigation.stands_on_fluid = super::path::stands_on_lava;
        let mut goals = GoalSelector::default();
        goals.add(1, Panic::new(1.65));
        goals.add(2, Breed::new(1.0));
        goals.add(3, Tempt::new(1.4, strider_tempt));
        goals.add(4, GoToLava::default());
        goals.add(5, FollowParent::new(1.0));
        goals.add(7, RandomStroll::with_interval(1.0, 60));
        goals.add(8, LookAtPlayer::new(8.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    /// An adult strider sometimes spawns carrying a zombified piglin that
    /// holds a warped fungus on a stick (and wears a saddle that always
    /// drops), or a baby strider.
    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let monsters = super::monster::LocalDifficulty::at(cx, core.base.position).base
            != tuff_core::world::player::Difficulty::Peaceful;
        if core.is_baby() {
            return;
        }
        if cx.rng.gen_range(0..30) == 0 && monsters {
            core.base.passengers.push(
                tuff_nbt::Compound::new()
                    .with("id", EntityType::ZombifiedPiglin.name())
                    .with(
                        "equipment",
                        tuff_nbt::Compound::new().with(
                            "mainhand",
                            ItemStack::mint(Item::WarpedFungusOnAStick, 1, MintReason::Spawn)
                                .to_nbt(),
                        ),
                    )
                    .with(tuff_core::world::entity::FINALIZE_SPAWN, true),
            );
            core.equipment.set(
                tuff_vanilla_data::item::EquipmentSlot::Saddle,
                Some(ItemStack::mint(Item::Saddle, 1, MintReason::Spawn)),
            );
            core.equipment.set_drop_chance(
                tuff_vanilla_data::item::EquipmentSlot::Saddle,
                super::equipment::PRESERVED_DROP_CHANCE,
            );
        } else if cx.rng.gen_range(0..10) == 0 {
            core.base.passengers.push(
                tuff_nbt::Compound::new()
                    .with("id", EntityType::Strider.name())
                    .with("Age", -24000)
                    .with(tuff_core::world::entity::FINALIZE_SPAWN, true),
            );
        }
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut tuff_nbt::Compound) {
        if tuff_core::world::entity::take_bool(nbt, "Saddle").unwrap_or(false)
            && !super::equine::saddled(core)
        {
            core.equipment.set(
                tuff_vanilla_data::item::EquipmentSlot::Saddle,
                Some(ItemStack::mint(Item::Saddle, 1, MintReason::Spawn)),
            );
        }
    }

    fn seats(&self, core: &MobCore) -> usize {
        usize::from(super::equine::saddled(core))
    }

    fn accepts_rider(&self, core: &MobCore) -> bool {
        !core.is_baby()
    }

    fn drivable(&self, core: &MobCore) -> bool {
        super::equine::saddled(core)
    }

    fn controlled_by(&self, _core: &MobCore, input: &RiderInput) -> bool {
        input.holds(Item::WarpedFungusOnAStick)
    }

    fn drive_input(&self, _core: &MobCore, _input: RiderInput) -> (f32, f32) {
        (1.0, 0.0)
    }

    fn hops_when_ridden(&self) -> bool {
        false
    }

    fn ride_speed(&mut self, core: &mut MobCore, input: &RiderInput, _cx: &mut TickContext) -> f32 {
        core.base.pitch = input.pitch * 0.5;
        self.steering.tick();
        let base = if self.suffocating { 0.35 } else { 0.55 };
        base * self.steering.factor()
    }

    fn ridden_speed(&self, core: &MobCore) -> Option<tuff_core::world::entity::RiddenSpeed> {
        Some(tuff_core::world::entity::RiddenSpeed {
            speed: core.attribute(Attribute::MovementSpeed)
                * 0.55
                * f64::from(super::steering::MAX_FACTOR),
            leap: 0.0,
            ..Default::default()
        })
    }

    fn mount_refused(&self, _core: &MobCore, stack: Option<&ItemStack>) -> bool {
        stack.is_some_and(|s| strider_food(s.item))
    }

    fn boost(&mut self, core: &mut MobCore, roll: u32) -> bool {
        let boosted = self.steering.boost(roll);
        if boosted {
            core.changed(BOOST_TIME);
        }
        boosted
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (BOOST_TIME, MetadataValue::VarInt(self.steering.total())),
            (SUFFOCATING, MetadataValue::Boolean(self.suffocating)),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntityStriderAmbient),
            hurt: Some(SoundEvent::EntityStriderHurt),
            death: Some(SoundEvent::EntityStriderDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        Some(SoundEvent::EntityStriderEat)
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        strider_food(stack.item)
    }

    /// Undamaged by lava, hurt one point a tick by water or rain (it is
    /// sensitive to water), and never on fire.
    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let [x, _, z] = core.base.position;
        let eye = block_pos([x, core.eye_position()[1], z]);
        let rained_on = cx.raining && core.can_see_sky(eye, cx);
        if core.is_alive() && (core.body.in_water || rained_on) {
            core.hurt_later(1.0, &tuff_vanilla_data::damage_type::DROWN);
        }
        if core.in_lava() && !core.base.on_ground {
            let [vx, vy, vz] = core.base.velocity;
            core.base.velocity = [vx * 0.5, vy * 0.5 + 0.05, vz * 0.5];
        }
        if !core.no_ai {
            let suffocating = !on_warm_block(core, cx);
            if suffocating != self.suffocating {
                self.suffocating = suffocating;
                core.changed(SUFFOCATING);
                core.set_modifier(
                    Attribute::MovementSpeed,
                    SUFFOCATING_MODIFIER,
                    suffocating.then_some((
                        SUFFOCATING_SPEED_FACTOR,
                        ModifierOperation::AddMultipliedBase,
                    )),
                );
            }
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_ref()?;
        super::equine::saddle_up(core, stack, Some(SoundEvent::EntityStriderSaddle), cx)
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Strider, super::load::<Strider>);
    registry.register_spawn_rule(EntityType::Strider, spawn_rule);
}

/// Only where nothing but air stands above the lava it spawns over.
fn spawn_rule(
    _kind: EntityType,
    level: &super::spawning::SpawnLevel,
    pos: BlockPos,
    _reason: super::spawning::SpawnReason,
    _rng: &mut StdRng,
) -> bool {
    let mut above = BlockPos::new(pos.x, pos.y + 1, pos.z);
    while (level.block)(above).is_some_and(|s| {
        !s.fluid_state().is_empty()
            && tuff_datapack::tags::is(tag::fluid::key::LAVA, s.fluid_state().fluid())
    }) {
        above.y += 1;
    }
    (level.block)(above).is_some_and(tuff_world_model::chunk::is_air)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[tuff_core::watchdog]
    #[test]
    fn adult_striders_roll_piglin_and_baby_jockeys() {
        let (mut piglins, mut babies, mut none) = (0, 0, 0);
        crate::tests::with_context(|_, cx| {
            for seed in 0..3000 {
                *cx.rng = StdRng::seed_from_u64(seed);
                let base = tuff_core::world::entity::EntityBase::new(
                    EntityType::Strider,
                    9,
                    9,
                    [0.5, 64.0, 0.5],
                    [0.0; 3],
                );
                let mut strider = crate::Mob::<Strider>::new(base, cx.rng);
                strider.species.finalize_spawn(&mut strider.core, cx);
                match strider.core.base.passengers.first() {
                    Some(rider) if rider.get_str("id") == Some("minecraft:zombified_piglin") => {
                        piglins += 1;
                        assert!(super::super::equine::saddled(&strider.core));
                        assert_eq!(
                            strider
                                .core
                                .equipment
                                .drop_chance(tuff_vanilla_data::item::EquipmentSlot::Saddle),
                            super::super::equipment::PRESERVED_DROP_CHANCE
                        );
                        assert!(rider.contains_key(tuff_core::world::entity::FINALIZE_SPAWN));
                    }
                    Some(rider) => {
                        assert_eq!(rider.get_str("id"), Some("minecraft:strider"));
                        assert_eq!(rider.get_int("Age"), Some(-24000));
                        babies += 1;
                    }
                    None => none += 1,
                }
            }
        });
        assert!((60..150).contains(&piglins), "{piglins}");
        assert!((200..380).contains(&babies), "{babies}");
        assert!(none > 2400, "{none}");
    }
}
