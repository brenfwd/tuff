use super::*;

pub(crate) const BRUTE_IMMUNE_TO_ZOMBIFICATION: u8 = data::MOB_FLAGS + 1;
pub(crate) const BRUTE_HOME_RADIUS: i32 = 100;
pub(crate) const BRUTE_ALLIES: &[EntityType] = &[EntityType::Piglin, EntityType::PiglinBrute];
pub(crate) const BRUTE_ANGER_TICKS: u64 = 600;
pub(crate) const BRUTE_IDLE_SPEED: f64 = 0.6;
pub(crate) const BRUTE_HOME_CLOSE: f64 = 2.0;
pub(crate) const BRUTE_HOME_STROLL: f64 = 5.0;

/// Piglin brutes: always hostile to players (gold does not deter them),
/// never barter, only pick up golden axes, keep to the spot they spawned
/// at, and zombify in the overworld like piglins unless immune.
#[derive(Debug, Default)]
pub struct PiglinBrute {
    pub(super) immune_to_zombification: bool,
    pub(super) time_in_overworld: i32,
}

impl PiglinBrute {
    fn take_axe(&self, core: &mut MobCore, cx: &mut TickContext) {
        if !core.can_pick_up_loot
            || core.equipment.get(EquipmentSlot::Mainhand).is_some()
            || !cx.shared.game_rules.borrow().bool("mob_griefing")
        {
            return;
        }
        let reach = crate::physics::inflate(core.bounds(), [1.0, 0.0, 1.0]);
        let Some(id) = cx.entities.iter().find_map(|e| {
            let (item, count, takeable) = e.item?;
            (takeable
                && count > 0
                && item == Item::GoldenAxe
                && crate::physics::intersects(e.bounds, reach))
            .then_some(e.id)
        }) else {
            return;
        };
        cx.effects.push(Effect::TakeItem { item: id, count: 1 });
    }
}

impl Species for PiglinBrute {
    fn create(_rng: &mut rand::rngs::StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.xp_reward = 20;
        let mut goals = GoalSelector::default();
        goals.add(
            1,
            HurtByTarget::new(BRUTE_ALLIES)
                .alerting(BRUTE_ALLIES)
                .expiring(BRUTE_ANGER_TICKS),
        );
        goals.add(
            2,
            NearestAttackablePlayer::new(true).only_nearest(crate::target::nearest_visible_player),
        );
        goals.add(
            2,
            NearestAttackableEntity::new(NEMESES, true)
                .only_nearest(crate::target::nearest_visible_player),
        );
        goals.add(3, MeleeAttack::new(1.0, false));
        goals.add(7, RandomStroll::water_avoiding(BRUTE_IDLE_SPEED));
        goals.add(
            7,
            crate::goals::WalkToHome::new(
                BRUTE_IDLE_SPEED,
                BRUTE_HOME_CLOSE,
                f64::from(BRUTE_HOME_RADIUS),
            ),
        );
        goals.add(
            7,
            crate::goals::StrollAroundHome::new(BRUTE_IDLE_SPEED, BRUTE_HOME_STROLL),
        );
        goals.add(8, LookAtPlayer::new(8.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut tuff_nbt::Compound) {
        use tuff_core::world::entity::{take_bool, take_number};
        self.immune_to_zombification = take_bool(nbt, "IsImmuneToZombification").unwrap_or(false);
        self.time_in_overworld = take_number(nbt, "TimeInOverworld").map_or(0, |v| v as i32);
    }

    fn save(&self, _core: &MobCore, out: &mut tuff_nbt::Compound) {
        out.insert("IsImmuneToZombification", self.immune_to_zombification);
        out.insert("TimeInOverworld", self.time_in_overworld);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(
            BRUTE_IMMUNE_TO_ZOMBIFICATION,
            MetadataValue::Boolean(self.immune_to_zombification),
        )]
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        tick_zombification(
            core,
            self.immune_to_zombification,
            &mut self.time_in_overworld,
            cx,
        );
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.take_axe(core, cx);
    }

    fn took_item(&mut self, core: &mut MobCore, stack: ItemStack) {
        if stack.item() == Item::GoldenAxe && core.equipment.get(EquipmentSlot::Mainhand).is_none()
        {
            core.equipment.set(EquipmentSlot::Mainhand, Some(stack));
            core.equipment.set_drop_chance(
                EquipmentSlot::Mainhand,
                crate::equipment::PRESERVED_DROP_CHANCE,
            );
            core.persistence_required = true;
        }
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        monster::sounds(
            SoundEvent::EntityPiglinBruteAmbient,
            SoundEvent::EntityPiglinBruteHurt,
            SoundEvent::EntityPiglinBruteDeath,
            Some(SoundEvent::EntityPiglinBruteStep),
        )
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        core.home = Some(crate::Home {
            pos: crate::physics::block_pos(core.base.position),
            radius: BRUTE_HOME_RADIUS,
        });
        #[expect(clippy::disallowed_methods, reason = "mob equipment")]
        core.equipment.set(
            EquipmentSlot::Mainhand,
            Some(ItemStack::mint(Item::GoldenAxe, 1, MintReason::Spawn)),
        );
    }
}
