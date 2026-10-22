//! Vexes: they drift through blocks, wander near where they were bound,
//! and charge straight at whatever they or their owner (an evoker's
//! summon) target, hitting it once and backing off to charge again.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::control::MoveStyle;
use super::goal::{Flags, Goal, GoalSelector, reduced_delay};
use super::goals::{Float, LookAtPlayer};
use super::illager;
use super::physics;
use super::target::{self, HurtByTarget, NearestAttackablePlayer};
use super::{MobCore, MobSounds, Species};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{TickContext, take, take_number};
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

const IS_ATTACKING: u8 = super::data::BABY;
const STARVE_DAMAGE: f32 = 1.0;
const STARVE_INTERVAL: i32 = 20;

/// Wanders to an empty position within a 15x11x15 box around where it was
/// bound (its current position, absent that).
#[derive(Debug, Default)]
struct RandomMove;

impl Goal for RandomMove {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        !mob.movement.has_wanted() && cx.rng.gen_range(0..reduced_delay(7)) == 0
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let origin = mob
            .bound_origin
            .unwrap_or_else(|| physics::block_pos(mob.base.position));
        for _ in 0..3 {
            let pos = BlockPos::new(
                origin.x + cx.rng.gen_range(0..15) - 7,
                origin.y + cx.rng.gen_range(0..11) - 5,
                origin.z + cx.rng.gen_range(0..15) - 7,
            );
            if (cx.block)(pos).is_some_and(tuff_world_model::chunk::is_air) {
                let target = [
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y) + 0.5,
                    f64::from(pos.z) + 0.5,
                ];
                mob.movement.set_wanted(target, 0.25);
                if mob.target.is_none() {
                    mob.look.set_look_at(target, 180.0, 20.0);
                }
                return;
            }
        }
    }
}

/// Charges at a target from up to three blocks and hits it once on
/// contact.
#[derive(Debug, Default)]
struct ChargeAttack;

impl Goal for ChargeAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(target) = mob.target_view(cx).filter(|t| t.alive) else {
            return false;
        };
        !mob.movement.has_wanted()
            && cx.rng.gen_range(0..reduced_delay(7)) == 0
            && mob.distance_sq(target.position) > 4.0
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.movement.has_wanted() && mob.signal != 0 && mob.target_view(cx).is_some_and(|t| t.alive)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(target) = mob.target_view(cx) {
            mob.movement.set_wanted(target.eye, 1.0);
        }
        mob.signal = 1;
        mob.sound(SoundEvent::EntityVexCharge, 1.0, 1.0, cx);
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
        if physics::intersects(mob.bounds(), target.bounds) {
            mob.hurt_target(&target, cx);
            mob.signal = 0;
            return;
        }
        if mob.distance_sq(target.position) < 9.0 {
            mob.movement.set_wanted(target.eye, 1.0);
        }
    }
}

/// Targets whatever its owner (the evoker that summoned it) targets.
#[derive(Debug, Default)]
struct CopyOwnerTarget;

impl CopyOwnerTarget {
    fn owner_target(mob: &MobCore, cx: &TickContext) -> Option<i32> {
        let uuid = mob.owner?;
        cx.entities
            .iter()
            .find(|e| e.uuid == uuid)
            .and_then(|e| e.living)
            .and_then(|l| l.target)
    }
}

impl Goal for CopyOwnerTarget {
    fn flags(&self) -> Flags {
        Flags::TARGET
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(id) = Self::owner_target(mob, cx) else {
            return false;
        };
        target::view(cx, id).is_some_and(|t| mob.can_attack(&t, cx))
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.can_use(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.target = Self::owner_target(mob, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.target = None;
    }
}

#[derive(Debug, Default)]
pub struct Vex {
    attacking: bool,
    /// Ticks left before it takes a point of starvation damage, only set
    /// for an evoker's summon (a plain `/summon`ed vex lives normally).
    limited_life: Option<i32>,
}

impl Vex {
    /// Binds a freshly summoned vex to its owner and origin, with a
    /// limited lifespan (an evoker's summon spell).
    pub fn bind(&mut self, core: &mut MobCore, owner: u128, origin: BlockPos, life_ticks: i32) {
        core.owner = Some(owner);
        core.bound_origin = Some(origin);
        self.limited_life = Some(life_ticks);
    }
}

impl Species for Vex {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.xp_reward = 3;
        core.phasing = true;
        core.base.no_gravity = true;
        core.move_style = MoveStyle::Phase;
        let mut goals = GoalSelector::default();
        goals.add(0, Float::new(core));
        goals.add(1, HurtByTarget::new(&[]).alerting(illager::RAIDERS));
        goals.add(2, CopyOwnerTarget);
        goals.add(3, NearestAttackablePlayer::new(true));
        goals.add(4, ChargeAttack);
        goals.add(8, RandomMove);
        goals.add(9, LookAtPlayer::new(3.0));
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        core.owner = take(nbt, "owner", Tag::as_uuid);
        core.bound_origin = take(nbt, "bound_pos", |tag| match tag {
            Tag::IntArray(v) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
            _ => None,
        });
        self.limited_life = take_number(nbt, "life_ticks").map(|v| v as i32);
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        if let Some(uuid) = core.owner {
            out.insert("owner", Tag::uuid(uuid));
        }
        if let Some(pos) = core.bound_origin {
            out.insert("bound_pos", vec![pos.x, pos.y, pos.z]);
        }
        if let Some(life) = self.limited_life {
            out.insert("life_ticks", life);
        }
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(IS_ATTACKING, MetadataValue::Byte(i8::from(self.attacking)))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        super::monster::sounds(
            SoundEvent::EntityVexAmbient,
            SoundEvent::EntityVexHurt,
            SoundEvent::EntityVexDeath,
            None,
        )
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.equipment.set(
            EquipmentSlot::Mainhand,
            Some(ItemStack::mint(Item::IronSword, 1, MintReason::Spawn)),
        );
        core.equipment.set_drop_chance(EquipmentSlot::Mainhand, 0.0);
        let difficulty = super::monster::LocalDifficulty::at(cx, core.base.position);
        core.populate_spawn_enchantments(difficulty, cx.rng);
    }

    fn ai_step(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        core.base.no_gravity = true;
        let attacking = core.signal != 0;
        if attacking != self.attacking {
            self.attacking = attacking;
            core.changed(IS_ATTACKING);
        }
        if let Some(life) = &mut self.limited_life {
            *life -= 1;
            if *life <= 0 {
                *life = STARVE_INTERVAL;
                core.hurt_later(STARVE_DAMAGE, &tuff_vanilla_data::damage_type::STARVE);
            }
        }
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Vex, super::load::<Vex>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mob;
    use tuff_core::world::entity::{EntityBase, RegionEntity};

    #[tuff_core::watchdog]
    #[test]
    fn spawned_vexes_sometimes_carry_an_enchanted_sword() {
        let mut shared = tuff_test_support::bare_shared();
        shared.difficulty =
            tuff_test_support::watch::channel(tuff_core::world::player::Difficulty::Hard).1;
        let block = |_: BlockPos| Some(tuff_vanilla_data::Block::Air.default_state());
        crate::tests::with_shared(shared, &block, &[], &[], |core, cx| {
            cx.inhabited_time = &|_| 3_600_000;
            let mut vex = Vex::default();
            let mut enchanted = 0;
            for _ in 0..2000 {
                vex.finalize_spawn(core, cx);
                let sword = core.equipment.get(EquipmentSlot::Mainhand).unwrap();
                assert_eq!(sword.item(), Item::IronSword);
                enchanted += usize::from(sword.components.enchantments.is_some());
            }
            assert!(enchanted > 0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn vexes_drift_through_walls_unhurt() {
        use tuff_vanilla_data::Block;
        let block = |pos: BlockPos| {
            Some(if pos.y < 64 || (2..4).contains(&pos.x) {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        crate::tests::with_world(&block, &[], |_, cx| {
            let base = EntityBase::new(EntityType::Vex, 2, 2, [0.5, 65.0, 0.5], [0.0; 3]);
            let mut vex = Mob::<Vex>::new(base, cx.rng);
            vex.core.no_ai = true;
            vex.core.movement.set_wanted([6.5, 65.0, 0.5], 1.0);
            let mut inside_wall = false;
            for _ in 0..80 {
                vex.core.tick_move_control(cx);
                vex.tick(cx);
                inside_wall |= (2.0..4.0).contains(&vex.core.base.position[0]);
            }
            assert!(inside_wall, "passed through the wall");
            assert!(
                vex.core.base.position[0] > 5.0,
                "{:?}",
                vex.core.base.position
            );
            assert!(!vex.core.movement.has_wanted(), "it arrived and braked");
            assert_eq!(vex.core.health, vex.core.max_health(), "never suffocated");
            assert!((vex.core.base.position[1] - 65.0).abs() < 0.1, "no gravity");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn vex_data_indices_match_the_dumped_layout() {
        let byte = MetadataValue::Byte(0).type_id() as u8;
        let layout = EntityType::Vex.data_layout();
        assert_eq!(layout.len(), IS_ATTACKING as usize + 1);
        assert_eq!(layout[IS_ATTACKING as usize], byte);
    }
}
