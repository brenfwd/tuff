//! Guardians and elder guardians: they swim, keep their distance and fire a
//! channeled magic beam at whatever they target, raise body spikes that
//! sting back anyone who melees them while they hold still, and (elder
//! guardians only) periodically curse nearby players with Mining Fatigue.

use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{EntityType, MobEffect, SoundEvent};

use rand::Rng;
use tuff_vanilla_data::entity::PathType;
use tuff_world_model::BlockPos;

use super::control::MoveStyle;
use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{LookAtPlayer, RandomLookAround, RandomStroll};
use super::monster::LocalDifficulty;
use super::movement::{Sinking, WaterTravel};
use super::navigation::NavKind;
use super::target::NearestAttackableEntity;
use super::{MobCore, MobSounds, Species, data};
use tuff_core::world::entity::{Culprit, Effect, EntityHit, TickContext};
use tuff_core::world::messages::PlayerEvent;
use tuff_core::world::player::Difficulty;

/// How far an elder guardian strays from where it first ticked.
const ELDER_HOME_RADIUS: i32 = 16;
const SWIM_SPEED: f64 = 1.0;
const STROLL_INTERVAL: i32 = 80;
const STROKE: f32 = 0.1;

/// Entity data after the shared fields: whether it is actively swimming
/// (spikes retract while true) and the entity id its beam is locked onto
/// (0 = none). Guardians are not ageable, so the baby/age-locked slots the
/// shared core would otherwise claim are free for these instead.
const MOVING: u8 = data::BABY;
const ATTACK_TARGET: u8 = data::AGE_LOCKED;

/// Broadcast the tick the beam locks onto its target, so viewers start
/// drawing it.
const ATTACK_BEAM_START: i8 = 21;

/// Ticks the beam goal spends locking on before it can fire (matches the
/// point `ATTACK_TARGET` starts being set and viewers see the beam begin).
const ATTACK_LOCK_ON: i32 = 10;
/// Total ticks of charge-up (from the goal starting) before a normal
/// guardian's beam fires.
const ATTACK_DURATION: i32 = 80;
/// Elder guardians charge faster.
const ELDER_ATTACK_DURATION: i32 = 60;

const THORNS_DAMAGE: f32 = 2.0;
/// Guardians neither pick nor keep beaming targets this close.
const CLOSE_RANGE_SQ: f64 = 9.0;
/// What guardians hunt besides players.
const PREY: &[EntityType] = &[
    EntityType::Squid,
    EntityType::GlowSquid,
    EntityType::Axolotl,
];

fn far_enough(mob: &MobCore, target: &super::target::TargetView, _cx: &TickContext) -> bool {
    mob.distance_sq(target.position) > CLOSE_RANGE_SQ
}

const ELDER_EFFECT_INTERVAL: u64 = 1200;
const ELDER_EFFECT_RADIUS_SQ: f64 = 50.0 * 50.0;
const ELDER_EFFECT_DURATION: i64 = 6000;
const ELDER_EFFECT_AMPLIFIER: u8 = 2;

fn attack_duration(core: &MobCore) -> i32 {
    if core.kind() == EntityType::ElderGuardian {
        ELDER_ATTACK_DURATION
    } else {
        ATTACK_DURATION
    }
}

/// Charges a beam at its target for [`attack_duration`] ticks (the beam
/// becomes visible to viewers after [`ATTACK_LOCK_ON`] of those), then
/// deals a fixed magic hit plus a normal melee-equivalent hit and lets go
/// of the target, same as the game's own attack: losing sight cancels the
/// charge immediately.
#[derive(Debug, Default)]
struct BeamAttack {
    attack_time: i32,
}

impl Goal for BeamAttack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target_view(cx).is_some_and(|t| t.alive)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(target) = mob.target_view(cx).filter(|t| t.alive) else {
            return false;
        };
        mob.kind() == EntityType::ElderGuardian || mob.distance_sq(target.position) > CLOSE_RANGE_SQ
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.attack_time = -ATTACK_LOCK_ON;
        mob.navigation.stop();
        if let Some(target) = mob.target_view(cx) {
            mob.look.set_look_at(target.eye, 90.0, 90.0);
        }
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        if mob.signal != 0 {
            mob.signal = 0;
            mob.changed(ATTACK_TARGET);
        }
        mob.target = None;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        mob.navigation.stop();
        mob.look.set_look_at(target.eye, 90.0, 90.0);
        if !mob.has_line_of_sight(&target, cx) {
            mob.target = None;
            return;
        }
        self.attack_time += 1;
        if self.attack_time == 0 {
            mob.signal = target.id;
            mob.changed(ATTACK_TARGET);
            mob.entity_event(ATTACK_BEAM_START, cx);
        } else if self.attack_time >= attack_duration(mob) {
            let hard = LocalDifficulty::at(cx, mob.base.position).base == Difficulty::Hard;
            let mut magic = if hard { 3.0 } else { 1.0 };
            if mob.kind() == EntityType::ElderGuardian {
                magic += 2.0;
            }
            let mut hit = EntityHit::new(magic, &damage_type::INDIRECT_MAGIC);
            hit.attacker = Some(Culprit::entity(&mob.base));
            hit.position = Some(mob.base.position);
            cx.effects.push(Effect::Hurt {
                target: target.id,
                hit: Box::new(hit),
            });
            mob.hurt_target(&target, cx);
            mob.target = None;
        }
    }
}

#[derive(Debug, Default)]
pub struct Guardian {
    moving: bool,
}

impl Species for Guardian {
    fn create(_rng: &mut rand::rngs::StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.xp_reward = 10;
        core.set_path_malus(PathType::Water, 0.0);
        core.navigation.kind = NavKind::WaterBound { breaching: false };
        core.move_style = MoveStyle::Guardian;
        core.water_travel = WaterTravel::Swimmer {
            stroke: Some(STROKE),
            sinks: Sinking::WhenIdle,
        };
        core.walk_value = Some(walk_target_value);
        if core.kind() == EntityType::ElderGuardian {
            // Elder guardians never despawn, unconditionally, the way the
            // game sets it up when one is constructed (fresh or loaded).
            core.persistence_required = true;
        }
        let mut goals = GoalSelector::default();
        goals.add(
            1,
            NearestAttackableEntity::new(PREY, true)
                .with_players()
                .filtered(far_enough),
        );
        goals.add(4, BeamAttack::default());
        goals.add(5, super::goals::MoveTowardsHome::new(1.0).looking());
        goals.add(
            7,
            RandomStroll::with_interval(SWIM_SPEED, STROLL_INTERVAL).looking(),
        );
        goals.add(8, LookAtPlayer::new(8.0));
        goals.add(9, RandomLookAround::default());
        core.goals = goals;
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if core.moving != self.moving {
            self.moving = core.moving;
            core.changed(MOVING);
        }
        if !core.body.in_water && core.base.on_ground {
            let dx = (cx.rng.r#gen::<f32>() * 2.0 - 1.0) * 0.4;
            let dz = (cx.rng.r#gen::<f32>() * 2.0 - 1.0) * 0.4;
            let velocity = &mut core.base.velocity;
            velocity[0] += f64::from(dx);
            velocity[1] += 0.5;
            velocity[2] += f64::from(dz);
            core.base.yaw = cx.rng.r#gen::<f32>() * 360.0;
            core.base.on_ground = false;
            core.impulse = true;
        }
        if core.signal != 0 {
            core.base.yaw = core.head_yaw;
        }
        if core.kind() == EntityType::ElderGuardian && !core.no_ai && core.home.is_none() {
            core.home = Some(super::Home {
                pos: super::physics::block_pos(core.base.position),
                radius: ELDER_HOME_RADIUS,
            });
        }
        if core.kind() == EntityType::ElderGuardian
            && !core.no_ai
            && (core.tick_count.wrapping_add(core.base.id as u64))
                .is_multiple_of(ELDER_EFFECT_INTERVAL)
        {
            let culprit = Some(Culprit::entity(&core.base));
            let silent = core.base.silent;
            let targets: Vec<i32> = {
                let teams = cx.shared.scoreboard.teams();
                let own = super::target::team_member(cx, core.base.uuid, None);
                cx.players
                    .iter()
                    .filter(|player| {
                        player.effects & 1u64 << MobEffect::MiningFatigue.id() == 0
                            && !player.creative
                            && !player.invulnerable
                            && !teams.allied(
                                own,
                                tuff_core::world::scoreboard::Member::Player(player.name),
                            )
                            && tuff_core::world::distance_sq(core.base.position, player.position)
                                <= ELDER_EFFECT_RADIUS_SQ
                    })
                    .map(|player| player.id)
                    .collect()
            };
            for player in targets {
                cx.effects.push(Effect::TellPlayer {
                    player,
                    event: Box::new(PlayerEvent::Packets(tuff_core::world::packets([
                        tuff_core::world::framed(&tuff_core::protocol::play::GameEvent {
                            event: tuff_core::protocol::play::game_event::GUARDIAN_ELDER_EFFECT,
                            value: if silent { 0.0 } else { 1.0 },
                        }),
                    ]))),
                });
                tuff_core::world::effects::apply_potion(
                    player,
                    true,
                    EntityType::Player,
                    &[tuff_core::world::effects::ActiveEffect::timed(
                        MobEffect::MiningFatigue,
                        ELDER_EFFECT_DURATION,
                        ELDER_EFFECT_AMPLIFIER,
                    )],
                    &[],
                    1.0,
                    false,
                    culprit.clone(),
                    cx,
                );
            }
        }
    }

    /// A guardian that just took a hit while its spikes are up (not
    /// swimming) stings back whoever hit it directly, the way its body
    /// spikes do; thorns and hits it is already immune to don't trigger
    /// this, and only a direct attacker (never a projectile) can be hit.
    fn after_hurt(&mut self, core: &mut MobCore, damage: &super::Damage, cx: &mut TickContext) {
        if self.moving || damage.projectile.is_some() {
            return;
        }
        if damage.kind.name == damage_type::THORNS.name || damage.is("avoids_guardian_thorns") {
            return;
        }
        let Some((attacker, _)) = damage.attacker else {
            return;
        };
        let mut hit = EntityHit::new(THORNS_DAMAGE, &damage_type::THORNS);
        hit.attacker = Some(Culprit::entity(&core.base));
        cx.effects.push(Effect::Hurt {
            target: attacker,
            hit: Box::new(hit),
        });
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, tuff_core::protocol::play::MetadataValue)> {
        use tuff_core::protocol::play::MetadataValue;
        vec![
            (MOVING, MetadataValue::Boolean(self.moving)),
            (ATTACK_TARGET, MetadataValue::VarInt(core.signal)),
        ]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let elder = core.kind() == EntityType::ElderGuardian;
        let in_water = core.body.in_water;
        MobSounds {
            ambient: Some(match (elder, in_water) {
                (false, true) => SoundEvent::EntityGuardianAmbient,
                (false, false) => SoundEvent::EntityGuardianAmbientLand,
                (true, true) => SoundEvent::EntityElderGuardianAmbient,
                (true, false) => SoundEvent::EntityElderGuardianAmbientLand,
            }),
            hurt: Some(match (elder, in_water) {
                (false, true) => SoundEvent::EntityGuardianHurt,
                (false, false) => SoundEvent::EntityGuardianHurtLand,
                (true, true) => SoundEvent::EntityElderGuardianHurt,
                (true, false) => SoundEvent::EntityElderGuardianHurtLand,
            }),
            death: Some(match (elder, in_water) {
                (false, true) => SoundEvent::EntityGuardianDeath,
                (false, false) => SoundEvent::EntityGuardianDeathLand,
                (true, true) => SoundEvent::EntityElderGuardianDeath,
                (true, false) => SoundEvent::EntityElderGuardianDeathLand,
            }),
            step: None,
            volume: 1.0,
        }
    }

    fn ambient_sound_interval(&self) -> i32 {
        160
    }
}

/// Guardians prefer water, brighter the better, and otherwise the dark
/// like any monster.
fn walk_target_value(core: &MobCore, pos: BlockPos, cx: &TickContext) -> f32 {
    let water = (cx.block)(pos).is_some_and(|s| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::fluid::key::WATER,
            s.fluid_state().fluid(),
        )
    });
    if water {
        10.0 + core.brightness_at(pos, cx) - 0.5
    } else {
        core.monster_walk_target_value(pos, cx)
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Guardian, super::load::<Guardian>);
    registry.register(EntityType::ElderGuardian, super::load::<Guardian>);
    registry.register_spawn_rule(EntityType::Guardian, super::spawning::guardian_rule);
    registry.register_spawn_rule(EntityType::ElderGuardian, super::spawning::guardian_rule);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn guardian_data_indices_match_the_dumped_layouts() {
        let boolean = tuff_core::protocol::play::MetadataValue::Boolean(false).type_id() as u8;
        let varint = tuff_core::protocol::play::MetadataValue::VarInt(0).type_id() as u8;
        for kind in [EntityType::Guardian, EntityType::ElderGuardian] {
            let layout = kind.data_layout();
            assert_eq!(layout.len(), ATTACK_TARGET as usize + 1);
            assert_eq!(layout[MOVING as usize], boolean);
            assert_eq!(layout[ATTACK_TARGET as usize], varint);
        }
    }

    fn player(
        id: i32,
        effects: u64,
        creative: bool,
    ) -> tuff_core::world::entity::NearbyPlayer<'static> {
        tuff_core::world::entity::NearbyPlayer {
            id,
            uuid: id as u128,
            name: "p",
            position: [3.0, 64.0, 0.0],
            eye_height: 1.62,
            yaw: 0.0,
            pitch: 0.0,
            held: [None; 2],
            creative,
            height: 1.8,
            sneaking: false,
            invisible: false,
            invulnerable: creative,
            armor: [None; 4],
            effects,
            movement: [0.0; 3],
            vehicle: None,
            sleeping: None,
            last_hurt_by: None,
            last_hurt_mob: None,
            perchable: false,
            sprinting: false,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_elder_curse_skips_players_who_hold_it_and_non_survival_players() {
        let block = |_: BlockPos| Some(tuff_vanilla_data::Block::Water.default_state());
        let held = 1u64 << MobEffect::MiningFatigue.id();
        let players = [
            player(1, 0, false),
            player(2, held, false),
            player(3, 0, true),
        ];
        crate::tests::with_players(&block, &[], &players, |core, cx| {
            core.base.kind = EntityType::ElderGuardian;
            core.tick_count = 1200 - core.base.id as u64;
            let mut species = Guardian::default();
            species.ai_step(core, cx);
            let told: Vec<i32> = cx
                .effects
                .iter()
                .filter_map(|e| match e {
                    Effect::TellPlayer { player, .. } => Some(*player),
                    _ => None,
                })
                .collect();
            assert_eq!(told, vec![1]);
        });
    }
}
