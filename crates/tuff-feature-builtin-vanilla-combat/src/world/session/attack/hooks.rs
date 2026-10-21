use super::*;

/// Melee and the damage rules as session hooks.
pub struct CombatHooks;

/// What a spinning player keeps of its speed, reversed, after a hit.
const SPIN_BOUNCE: f64 = -0.2;

/// A Riptide spin ran into `target`: it gets hit, and the spin ends with
/// the player bouncing back.
async fn spin_into(session: &mut PlayerSession, target: i32) {
    if session.motion.spin_attack_ticks == 0 || target == session.id {
        return;
    }
    session.motion.spin_attack_ticks = 0;
    session.attack(target).await;
    let velocity = session.sync.known_movement.map(|c| c * SPIN_BOUNCE);
    session.motion.velocity_sent.push(session.tick, velocity);
    let packet = packets([framed(&play::SetEntityMotion {
        entity_id: session.id,
        velocity,
    })]);
    session.broadcast_entity(packet, true).await;
    session.publish(false).await;
}

impl PlayerDamage for CombatHooks {
    fn hurt<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        source: DamageSource,
        amount: f32,
    ) -> Pin<Box<dyn Future<Output = HurtResult> + Send + 'a>> {
        Box::pin(session.resolve_hurt(source, amount))
    }

    fn entity_hit_landed<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        hit: &'a EntityHit,
    ) -> HookFuture<'a> {
        Box::pin(entity_post_attack(session, hit))
    }
}

/// The enchantments on the player's armor that act once another entity's
/// hit lands (Thorns hurting the attacker), and on a mob's weapon in a
/// melee hit (Fire Aspect setting the player alight).
async fn entity_post_attack(session: &mut PlayerSession, hit: &EntityHit) {
    let Some(attacker) = hit.attacker.clone() else {
        return;
    };
    let context = combat::DamageContext {
        kind: hit.kind,
        victim: EntityType::Player,
        attacker: Some(attacker.kind),
        direct_attacker: hit.projectile.or(Some(attacker.kind)),
        is_direct: hit.projectile.is_none(),
        direct_attacker_fall_distance: None,
        direct_attacker_flying: None,
    };
    let equipment = session.equipment();
    let worn: Vec<combat::Equipped> = equipment
        .iter()
        .map(|(slot, stack)| (*slot, &**stack))
        .collect();
    let own = combat::post_attack(&worn, combat::Side::Victim, &context, &mut session.rng);
    let weapon = match (&hit.weapon, hit.projectile) {
        (Some(weapon), None) => combat::post_attack(
            &[(EquipmentSlot::Mainhand, weapon)],
            combat::Side::Attacker,
            &context,
            &mut session.rng,
        ),
        _ => Vec::new(),
    };
    let mut returned = Vec::new();
    for (effect, mine) in own
        .into_iter()
        .map(|effect| (effect, true))
        .chain(weapon.into_iter().map(|effect| (effect, false)))
    {
        let on_me = effect.affected == combat::Side::Victim;
        match effect.effect {
            combat::PostAttack::Damage { kind, amount } if mine && !on_me => {
                let mut back = EntityHit::new(amount, kind);
                back.attacker = Some(Culprit::player(
                    session.id,
                    session.profile.id,
                    session.profile.name.clone(),
                ));
                back.position = Some(session.pose.position);
                returned.push(back);
            }
            combat::PostAttack::ItemDamage(amount) if mine => {
                session.wear_slot(effect.slot, amount).await;
            }
            combat::PostAttack::Ignite(seconds) if on_me => session.ignite(seconds),
            combat::PostAttack::ApplyMobEffect {
                effect: mob_effect,
                amplifier,
                ticks,
            } if on_me => {
                session
                    .add_effect(ActiveEffect::timed(mob_effect, i64::from(ticks), amplifier))
                    .await;
            }
            _ => {}
        }
    }
    if returned.is_empty() {
        return;
    }
    let at = BlockPos::containing(hit.position.unwrap_or(session.pose.position));
    let Some(region) = session.region_of(at) else {
        return;
    };
    let target = attacker.id;
    region
        .request(RegionRequest::WhenLoaded {
            min: at,
            max: at,
            run: tuff_core::world::region_api::RegionTask::new(move |world| {
                for back in returned {
                    world.hit_entity(target, back);
                }
            }),
        })
        .await;
}

impl SessionHooks for CombatHooks {
    fn tick<'a>(&'a self, session: &'a mut PlayerSession, phase: SessionTick) -> HookFuture<'a> {
        Box::pin(async move {
            if phase == SessionTick::Use {
                crate::world::session::enchant::tick(session).await;
                session.kinetic_tick().await;
            }
        })
    }

    fn event<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        event: &'a mut Option<PlayerEvent>,
    ) -> HookFuture<'a> {
        Box::pin(async move {
            match event.take() {
                Some(PlayerEvent::Attacked(attack)) => session.attacked(attack).await,
                Some(PlayerEvent::Knocked { direction, power }) => {
                    session.knocked(direction, power).await
                }
                Some(PlayerEvent::SpinAttackTouched { target }) => {
                    spin_into(session, target).await;
                }
                Some(PlayerEvent::AttackLanded {
                    weapon,
                    target,
                    target_position,
                    knockback,
                    sweep,
                    health_lost,
                    target_kind,
                    dealt,
                }) => {
                    if target_kind == tuff_vanilla_data::registry::EntityType::Player {
                        session.hurt_entity(
                            weapon.as_ref(),
                            target_kind,
                            target_position,
                            dealt,
                            health_lost,
                        );
                    }
                    session
                        .attack_landed(
                            weapon,
                            target,
                            target_position,
                            knockback,
                            sweep,
                            health_lost,
                        )
                        .await
                }
                other => *event = other,
            }
        })
    }

    fn packet<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a mut Option<Serverbound>,
    ) -> HookFuture<'a> {
        Box::pin(async move {
            match packet.take() {
                Some(Serverbound::Attack(target)) => session.attack(target).await,
                Some(Serverbound::Interact {
                    entity,
                    off_hand,
                    sneaking,
                    location,
                }) => session.interact(entity, off_hand, sneaking, location).await,
                Some(Serverbound::PlayerAction {
                    status: PlayerActionStatus::Stab,
                    sequence,
                    ..
                }) => {
                    session.stab().await;
                    session.send(&play::BlockChangedAck(sequence));
                }
                other => *packet = other,
            }
        })
    }
}
