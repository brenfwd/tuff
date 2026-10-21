//! Silverfish and endermites: small biters.
//!
//! Silverfish hide in stone (turning it into its infested kind), come out
//! of infested blocks that break, and wake the silverfish hidden around
//! them when hurt. Endermites live two minutes unless kept; one in twenty
//! ender pearls leaves one behind.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};

use super::attack::MeleeAttack;
use super::goal::{Flags, Goal, GoalSelector, reduced_delay};
use super::goals::{ClimbOnTopOfPowderSnow, Float, LookAtPlayer, RandomLookAround, RandomStroll};
use super::monster;
use super::target::{HurtByTarget, NearestAttackablePlayer};
use super::{Damage, MobCore, MobSounds, Species};
use tuff_core::world::entity::{Effect, TickContext, take_number};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

const SILVERFISH_ALERTS: &[EntityType] = &[EntityType::Silverfish];
const ENDERMITE_ALERTS: &[EntityType] = &[EntityType::Endermite];
/// The entity event that shows a mob's spawn (or vanish) poof.
pub const SPAWN_ANIMATION: i8 =
    tuff_core::protocol::play::entity_event::SILVERFISH_MERGE_WITH_STONE;
const ENDERMITE_LIFE: i32 = 2400;
const ENDERMITE_XP: i32 = 3;
const WAKE_RANGE: i32 = 10;
const WAKE_RANGE_Y: i32 = 5;

/// `state` as the infested block that imitates it, properties kept.
pub fn infested_state(state: BlockState) -> Option<BlockState> {
    let infested = state.block().infested_by_host()?;
    Some(transfer(state, infested))
}

/// An infested block as the block it imitates, properties kept.
pub fn host_state(state: BlockState) -> Option<BlockState> {
    let host = state.block().infested_host()?;
    Some(transfer(state, host))
}

fn transfer(state: BlockState, to: Block) -> BlockState {
    state
        .properties()
        .fold(to.default_state(), |s, (name, value)| {
            s.with(name, value).unwrap_or(s)
        })
}

/// Offsets 0, 1, -1, 2, -2, ... up to `range` each way, as the game walks
/// out from the middle.
fn outward(range: i32) -> impl Iterator<Item = i32> {
    (0..=2 * range).map(|i| if i % 2 == 0 { -(i / 2) } else { i / 2 + 1 })
}

/// When hurt, after a second it breaks open the infested blocks around it
/// (nearest first), stopping at each with even odds.
#[derive(Debug, Default)]
struct WakeUpFriends;

impl Goal for WakeUpFriends {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.signal > 0
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.signal -= 1;
        if mob.signal > 0 {
            return;
        }
        let base = super::physics::block_pos(mob.base.position);
        let grief = cx.shared.game_rules.borrow().bool("mob_griefing");
        for dy in outward(WAKE_RANGE_Y) {
            for dx in outward(WAKE_RANGE) {
                for dz in outward(WAKE_RANGE) {
                    let pos = BlockPos::new(base.x + dx, base.y + dy, base.z + dz);
                    let Some(state) = (cx.block)(pos) else {
                        continue;
                    };
                    let Some(host) = host_state(state) else {
                        continue;
                    };
                    if grief {
                        cx.effects.push(Effect::DestroyBlock { pos, drop: true });
                    } else {
                        cx.effects.push(Effect::SetBlock { pos, state: host });
                    }
                    if cx.rng.r#gen::<bool>() {
                        return;
                    }
                }
            }
        }
    }
}

/// Wanders, and now and then (when mobs may grief) slips into a stone
/// block beside it, turning it infested.
#[derive(Debug)]
struct MergeWithStone {
    stroll: RandomStroll,
    merge: Option<BlockPos>,
}

impl Goal for MergeWithStone {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if mob.target.is_some() || !mob.navigation.is_done() {
            return false;
        }
        self.merge = None;
        let grief = cx.shared.game_rules.borrow().bool("mob_griefing");
        if grief && cx.rng.gen_range(0..reduced_delay(10)) == 0 {
            let [x, y, z] = mob.base.position;
            use tuff_vanilla_data::block::Facing;
            const DIRECTIONS: [Facing; 6] = [
                Facing::Down,
                Facing::Up,
                Facing::North,
                Facing::South,
                Facing::West,
                Facing::East,
            ];
            let direction = DIRECTIONS[cx.rng.gen_range(0..6)];
            let origin = super::physics::block_pos([x, y + 0.5, z]);
            let pos = tuff_vanilla_common::placement::step(origin, direction);
            if (cx.block)(pos).and_then(infested_state).is_some() {
                self.merge = Some(pos);
                return true;
            }
        }
        self.stroll.can_use(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.merge.is_none() && self.stroll.can_continue(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(pos) = self.merge else {
            self.stroll.start(mob, cx);
            return;
        };
        if let Some(infested) = (cx.block)(pos).and_then(infested_state) {
            cx.effects.push(Effect::SetBlock {
                pos,
                state: infested,
            });
            mob.entity_event(SPAWN_ANIMATION, cx);
            mob.discarded = true;
        }
    }

    fn stop(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.stroll.stop(mob, cx);
    }
}

fn silverfish_walk_value(mob: &MobCore, pos: BlockPos, cx: &TickContext) -> f32 {
    let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
    if (cx.block)(below).is_some_and(|s| s.block().infested_by_host().is_some()) {
        10.0
    } else {
        mob.monster_walk_target_value(pos, cx)
    }
}

#[derive(Debug, Default)]
pub struct Silverfish;

impl Species for Silverfish {
    fn create(_rng: &mut StdRng) -> Self {
        Self
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.walk_value = Some(silverfish_walk_value);
        let mut goals = GoalSelector::default();
        goals.add(1, HurtByTarget::new(&[]).alerting(SILVERFISH_ALERTS));
        goals.add(2, NearestAttackablePlayer::new(true));
        goals.add(1, Float::new(core));
        goals.add(1, ClimbOnTopOfPowderSnow);
        goals.add(3, WakeUpFriends);
        goals.add(4, MeleeAttack::new(1.0, false));
        goals.add(
            5,
            MergeWithStone {
                stroll: RandomStroll::with_interval(1.0, 10),
                merge: None,
            },
        );
        core.goals = goals;
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        monster::sounds(
            SoundEvent::EntitySilverfishAmbient,
            SoundEvent::EntitySilverfishHurt,
            SoundEvent::EntitySilverfishDeath,
            Some(SoundEvent::EntitySilverfishStep),
        )
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &Damage, _cx: &mut TickContext) {
        let triggers = damage.attacker.is_some() || damage.is("always_triggers_silverfish");
        if triggers && core.signal == 0 {
            core.signal = reduced_delay(20);
        }
    }

    fn tick(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        core.body_yaw = core.base.yaw;
    }
}

#[derive(Debug, Default)]
pub struct Endermite {
    life: i32,
}

impl Species for Endermite {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.xp_reward = ENDERMITE_XP;
        let mut goals = GoalSelector::default();
        goals.add(1, HurtByTarget::new(&[]).alerting(ENDERMITE_ALERTS));
        goals.add(2, NearestAttackablePlayer::new(true));
        goals.add(1, Float::new(core));
        goals.add(1, ClimbOnTopOfPowderSnow);
        goals.add(2, MeleeAttack::new(1.0, false));
        goals.add(3, RandomStroll::water_avoiding(1.0));
        goals.add(7, LookAtPlayer::new(8.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.life = take_number(nbt, "Lifetime").map_or(0, |v| v as i32);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("Lifetime", self.life);
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        monster::sounds(
            SoundEvent::EntityEndermiteAmbient,
            SoundEvent::EntityEndermiteHurt,
            SoundEvent::EntityEndermiteDeath,
            Some(SoundEvent::EntityEndermiteStep),
        )
    }

    fn tick(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        core.body_yaw = core.base.yaw;
    }

    fn ai_step(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        if !core.persistence_required {
            self.life += 1;
        }
        if self.life >= ENDERMITE_LIFE {
            core.discarded = true;
        }
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Silverfish, super::load::<Silverfish>);
    registry.register(EntityType::Endermite, super::load::<Endermite>);
    registry.register_spawn_rule(
        EntityType::Silverfish,
        super::spawning::no_player_close_rule,
    );
    registry.register_spawn_rule(EntityType::Endermite, super::spawning::no_player_close_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn infested_blocks_imitate_their_hosts() {
        assert_eq!(
            Block::InfestedStone.infested_host(),
            Some(Block::Stone),
            "infested stone hides in stone"
        );
        let pillar = Block::Deepslate
            .default_state()
            .with("axis", "x")
            .expect("deepslate has an axis");
        let infested = infested_state(pillar).expect("deepslate can be infested");
        assert_eq!(infested.block(), Block::InfestedDeepslate);
        assert_eq!(infested.properties().collect::<Vec<_>>(), [("axis", "x")]);
        assert_eq!(host_state(infested), Some(pillar));
        assert_eq!(infested_state(Block::Dirt.default_state()), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn friends_are_searched_from_the_middle_out() {
        assert_eq!(outward(2).collect::<Vec<_>>(), [0, 1, -1, 2, -2]);
    }
}
