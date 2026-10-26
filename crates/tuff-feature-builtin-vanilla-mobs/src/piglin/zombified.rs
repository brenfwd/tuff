use super::*;

#[derive(Debug, Default)]
pub struct ZombifiedPiglin {
    pub(super) zombie: super::super::zombie::Zombie,
    pub(super) had_target: bool,
    pub(super) first_anger_sound_in: i32,
    pub(super) next_alert: i32,
}

pub(super) const ALERT_RANGE_Y: f64 = 10.0;
pub(super) const ATTACKING_MODIFIER: &str = "attacking";

impl ZombifiedPiglin {
    pub(super) fn track_target(&mut self, core: &MobCore, cx: &mut TickContext) {
        let has_target = core.target.is_some();
        if has_target && !self.had_target {
            self.first_anger_sound_in = cx.rng.gen_range(0..=20);
            self.next_alert = cx.rng.gen_range(80..=120);
        }
        self.had_target = has_target;
    }

    pub(super) fn alert_others(&self, core: &MobCore, target: i32, cx: &mut TickContext) {
        let range = core.follow_range();
        let [x, y, z] = core.base.position;
        let area = [
            x - range,
            y - ALERT_RANGE_Y,
            z - range,
            x + 1.0 + range,
            y + 1.0 + ALERT_RANGE_Y,
            z + 1.0 + range,
        ];
        for other in cx.entities {
            let helps = other.id != core.base.id
                && other.kind == EntityType::ZombifiedPiglin
                && other.living.is_some_and(|l| l.alive && l.target.is_none())
                && super::super::physics::intersects(other.bounds, area);
            if helps {
                cx.effects.push(Effect::Notify {
                    target: other.id,
                    notice: tuff_core::world::entity::Notice::Alert { attacker: target },
                });
            }
        }
    }
}

/// Zombified piglins only go for players they are angry with.
pub(super) fn grudge(mob: &MobCore, player: &NearbyPlayer) -> bool {
    mob.angry_at_player(player.uuid)
}

/// A zombified piglin's default gear: a golden sword or, rarely, spear.
pub(crate) fn arm_zombified(core: &mut MobCore, cx: &mut TickContext) {
    if core.equipment.get(EquipmentSlot::Mainhand).is_some() {
        return;
    }
    let weapon = if cx.rng.gen_range(0..20) == 0 {
        Item::GoldenSpear
    } else {
        Item::GoldenSword
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "item a mob spawns, wears or throws (arm_zombified)"
    )]
    core.equipment.set(
        EquipmentSlot::Mainhand,
        Some(ItemStack::mint(weapon, 1, MintReason::Spawn)),
    );
}

impl Species for ZombifiedPiglin {
    fn create(_rng: &mut rand::rngs::StdRng) -> Self {
        Self::default()
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut tuff_nbt::Compound) {
        self.zombie.load(core, nbt);
    }

    fn save(&self, core: &MobCore, out: &mut tuff_nbt::Compound) {
        self.zombie.save(core, out);
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        let mut goals = GoalSelector::default();
        goals.add(
            1,
            HurtByTarget::new(&[]).alerting(&[EntityType::ZombifiedPiglin]),
        );
        goals.add(
            2,
            NearestAttackablePlayer::with_interval(10, true, Some(grudge)),
        );
        goals.add(3, super::super::target::ResetUniversalAnger::new(true));
        goals.add(1, super::super::spear::SpearUse::standard());
        goals.add(2, MeleeAttack::zombie(1.0, false));
        goals.add(4, super::super::zombie::AttackTurtleEgg::new());
        goals.add(7, RandomStroll::water_avoiding(1.0));
        goals.add(8, LookAtPlayer::new(8.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (data::BABY, MetadataValue::Boolean(core.is_baby())),
            (data::BABY + 1, MetadataValue::VarInt(0)),
            (data::BABY + 2, MetadataValue::Boolean(false)),
        ]
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let angry = core.anger.is_some();
        monster::sounds(
            if angry {
                SoundEvent::EntityZombifiedPiglinAngry
            } else {
                SoundEvent::EntityZombifiedPiglinAmbient
            },
            SoundEvent::EntityZombifiedPiglinHurt,
            SoundEvent::EntityZombifiedPiglinDeath,
            None,
        )
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.track_target(core, cx);
        if core.anger.is_some() {
            if !core.is_baby() {
                core.set_modifier(
                    tuff_vanilla_data::registry::Attribute::MovementSpeed,
                    ATTACKING_MODIFIER,
                    Some((0.05, tuff_vanilla_data::item::ModifierOperation::AddValue)),
                );
            }
            if self.first_anger_sound_in > 0 {
                self.first_anger_sound_in -= 1;
                if self.first_anger_sound_in == 0 {
                    core.sound(SoundEvent::EntityZombifiedPiglinAngry, 2.0, 1.8, cx);
                }
            }
        } else {
            core.set_modifier(
                tuff_vanilla_data::registry::Attribute::MovementSpeed,
                ATTACKING_MODIFIER,
                None,
            );
        }
        core.update_anger(true, cx);
        if let Some(target) = core.target_view(cx) {
            if self.next_alert > 0 {
                self.next_alert -= 1;
            } else {
                if core.has_line_of_sight(&target, cx) {
                    self.alert_others(core, target.id, cx);
                }
                self.next_alert = cx.rng.gen_range(80..=120);
            }
        }
    }

    /// A zombie's spawn (babies, chicken jockeys, door breakers, leaders)
    /// with a golden sword or, rarely, spear in place of a zombie's gear.
    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let difficulty = monster::LocalDifficulty::at(cx, core.base.position);
        let special = difficulty.special_multiplier();
        core.can_pick_up_loot = cx.rng.r#gen::<f32>() < 0.55 * special;
        if cx.rng.r#gen::<f32>() < super::super::zombie::BABY_CHANCE {
            core.set_zombie_baby(true);
            super::super::zombie::ride_chicken(core, cx);
        }
        let can_break = cx.rng.r#gen::<f32>() < special * 0.1;
        self.zombie.set_can_break_doors(core, can_break);
        arm_zombified(core, cx);
        core.populate_spawn_enchantments(difficulty, cx.rng);
        core.wear_halloween_head(cx.rng, super::super::calendar::halloween_now());
        self.zombie.handle_attributes(core, special, true, cx.rng);
    }
}
