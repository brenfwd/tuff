use super::*;

impl RegionEntity for EnderDragon {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, out: &mut Compound) {
        out.insert("Health", self.health);
        if self.absorption > 0.0 {
            out.insert("AbsorptionAmount", self.absorption);
        }
        out.insert("DragonPhase", self.phase.id());
        out.insert("DragonDeathTime", self.death_time);
        out.insert("sitting_damage_received", self.sitting_damage);
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        [DATA_HEALTH, DATA_PHASE]
            .into_iter()
            .filter_map(|i| Some((i, self.value(i)?)))
            .collect()
    }

    fn take_changed_data(&mut self) -> Vec<(u8, MetadataValue)> {
        let mut changed = std::mem::take(&mut self.changed);
        changed.sort_unstable();
        changed.dedup();
        changed
            .into_iter()
            .filter_map(|i| Some((i, self.value(i)?)))
            .collect()
    }

    fn resend_data(&mut self, index: u8) {
        self.changed.push(index);
    }

    fn rotates(&self) -> bool {
        true
    }

    fn uses_portals(&self) -> bool {
        false
    }

    fn take_impulse(&mut self) -> bool {
        std::mem::take(&mut self.impulse)
    }

    fn take_id_span(&mut self) -> i32 {
        if self.ids_reserved {
            0
        } else {
            self.ids_reserved = true;
            PART_COUNT as i32
        }
    }

    fn parts(&self) -> Vec<[f64; 6]> {
        (0..PART_COUNT)
            .map(|part| part_box(self.parts[part], part))
            .collect()
    }

    fn part_positions(&self) -> Vec<[f64; 3]> {
        self.parts.to_vec()
    }

    fn hurt(&mut self, amount: f32, kind: &'static DamageType) -> bool {
        self.pending.push(PendingHurt {
            part: BODY,
            amount,
            kind,
        });
        false
    }

    fn hurt_part(&mut self, part: usize, amount: f32, kind: &'static DamageType) -> bool {
        self.pending.push(PendingHurt { part, amount, kind });
        false
    }

    fn attacked(&mut self, attack: &EntityAttack, cx: &mut TickContext) -> Option<AttackOutcome> {
        let part = attack.part.filter(|p| *p < PART_COUNT).unwrap_or(BODY);
        let eye = [
            attack.attacker_position[0],
            attack.attacker_position[1] + tuff_core::world::EYE_HEIGHT,
            attack.attacker_position[2],
        ];
        let area = part_box(self.parts[part], part);
        let closest = [0, 1, 2].map(|axis| eye[axis].clamp(area[axis], area[axis + 3]));
        if distance_sq(eye, closest) > REACH * REACH {
            return None;
        }
        let lost = self.hurt_part(
            part,
            attack.damage,
            &damage_type::PLAYER_ATTACK,
            Some(attack.attacker),
            Some(attack.attacker_position),
            cx,
        )?;
        Some(AttackOutcome {
            hurt: lost > 0.0,
            health_lost: lost,
        })
    }

    fn hit(&mut self, hit: &EntityHit, cx: &mut TickContext) -> Option<AttackOutcome> {
        let arrow = matches!(
            hit.projectile,
            Some(
                EntityType::Arrow
                    | EntityType::SpectralArrow
                    | EntityType::Trident
                    | EntityType::WindCharge
            )
        );
        let perched = matches!(
            self.phase,
            PhaseKind::SittingFlaming | PhaseKind::SittingScanning | PhaseKind::SittingAttacking
        );
        if arrow && perched {
            return Some(AttackOutcome {
                hurt: false,
                health_lost: 0.0,
            });
        }
        let attacker = hit
            .attacker
            .as_ref()
            .filter(|a| a.is_player())
            .map(|a| a.id);
        let part = hit.part.filter(|p| *p < PART_COUNT).unwrap_or(BODY);
        let lost = self.hurt_part(part, hit.amount, hit.kind, attacker, hit.position, cx)?;
        Some(AttackOutcome {
            hurt: lost > 0.0,
            health_lost: lost,
        })
    }

    fn kill(&mut self, cx: &mut TickContext) -> bool {
        if let Some(fight) = cx.shared.fight(cx.dimension) {
            fight.send(FightMessage::DragonKilled {
                uuid: self.base.uuid,
            });
        }
        true
    }

    fn is_persistent(&self) -> bool {
        true
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        self.ticks += 1;
        for hurt in std::mem::take(&mut self.pending) {
            self.hurt_part(hurt.part, hurt.amount, hurt.kind, None, None, cx);
        }
        if self.hurt_time > 0 {
            self.hurt_time -= 1;
        }
        if self.base.invulnerable_time > 0 {
            self.base.invulnerable_time -= 1;
        }
        if self.is_dead() {
            return self.tick_death(cx);
        }
        let roll = cx.rng.gen_range(0..1000);
        let time = self.ambient_sound_time;
        self.ambient_sound_time += 1;
        if roll < time {
            self.ambient_sound_time = -AMBIENT_INTERVAL;
            self.sound(SoundEvent::EntityEnderDragonAmbient, cx);
        }
        self.ai_step(cx);
        self.report(cx);
        Tick::Alive
    }
}
