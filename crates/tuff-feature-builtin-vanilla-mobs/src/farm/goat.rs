use super::*;

pub(super) fn goat_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::GOAT_FOOD, item)
}

/// A goat's horns and whether it screams; it rams through `RamGoal` and
/// leaps through `LongJump`.
#[derive(Debug)]
pub struct Goat {
    pub(super) initial_ram_cooldown: Option<i32>,
    pub(super) screaming: bool,
    pub(super) left_horn: bool,
    pub(super) right_horn: bool,
}

impl Goat {
    pub(super) fn drop_horn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if core.is_baby() || !(self.left_horn || self.right_horn) {
            return;
        }
        let left = if !self.left_horn {
            false
        } else if !self.right_horn {
            true
        } else {
            cx.rng.r#gen()
        };
        if left {
            self.left_horn = false;
        } else {
            self.right_horn = false;
        }
        let instruments = horn_instruments(self.screaming);
        let name = instruments[cx.rng.gen_range(0..instruments.len())];
        let mut stack = ItemStack::mint(Item::GoatHorn, 1, MintReason::Spawn);
        stack.components.other.insert("minecraft:instrument", name);
        let [x, y, z] = core.base.position;
        let velocity = [
            cx.rng.gen_range(-0.2..0.2),
            cx.rng.gen_range(0.3..0.7),
            cx.rng.gen_range(-0.2..0.2),
        ];
        cx.effects.push(Effect::DropItem {
            stack,
            position: [x, y, z],
            velocity,
            pickup_delay: 10,
        });
        core.sound_as(
            SoundEvent::EntityGoatHornBreak,
            play::sound_category::NEUTRAL,
            1.0,
            1.0,
            cx,
        );
    }
}

impl Species for Goat {
    fn create(rng: &mut StdRng) -> Self {
        Self {
            initial_ram_cooldown: Some(rng.gen_range(600..=6000)),
            screaming: false,
            left_horn: true,
            right_horn: true,
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        let mut goals = GoalSelector::default();
        common_goals(&mut goals, core, 2.0, (1.25, goat_food), 1.25);
        goals.add(4, RamGoal::new(0));
        goals.add(
            4,
            super::super::long_jump::LongJump::new(super::super::long_jump::LongJumpSpec {
                cooldown: (600, 1200),
                height: 5,
                width: 5,
                jump_sound: SoundEvent::EntityGoatLongJump,
                screaming_jump_sound: SoundEvent::EntityGoatScreamingLongJump,
                landing_sound: SoundEvent::EntityGoatStep,
                dimension_scale: 0.7,
                preference: None,
                landing: super::super::long_jump::default_landing,
                blocked: |_| false,
            }),
        );
        goals.add(5, RandomStroll::water_avoiding(1.0));
        goals.add(6, LookAtPlayer::new(6.0));
        goals.add(7, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.screaming = take_bool(nbt, "IsScreamingGoat").unwrap_or(false);
        self.left_horn = take_bool(nbt, "HasLeftHorn").unwrap_or(true);
        self.right_horn = take_bool(nbt, "HasRightHorn").unwrap_or(true);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("IsScreamingGoat", self.screaming);
        out.insert("HasLeftHorn", self.left_horn);
        out.insert("HasRightHorn", self.right_horn);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (data::SPECIES, MetadataValue::Boolean(self.screaming)),
            (data::SPECIES + 1, MetadataValue::Boolean(self.left_horn)),
            (data::SPECIES + 2, MetadataValue::Boolean(self.right_horn)),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        if self.screaming {
            MobSounds {
                ambient: Some(SoundEvent::EntityGoatScreamingAmbient),
                hurt: Some(SoundEvent::EntityGoatScreamingHurt),
                death: Some(SoundEvent::EntityGoatScreamingDeath),
                step: Some(SoundEvent::EntityGoatStep),
                volume: 1.0,
            }
        } else {
            MobSounds {
                ambient: Some(SoundEvent::EntityGoatAmbient),
                hurt: Some(SoundEvent::EntityGoatHurt),
                death: Some(SoundEvent::EntityGoatDeath),
                step: Some(SoundEvent::EntityGoatStep),
                volume: 1.0,
            }
        }
    }

    fn ambient_sound_interval(&self) -> i32 {
        170
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        goat_food(stack.item())
    }

    fn eating_sound(&self, _core: &MobCore) -> Option<SoundEvent> {
        Some(if self.screaming {
            SoundEvent::EntityGoatScreamingEat
        } else {
            SoundEvent::EntityGoatEat
        })
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if stack.item() != Item::Bucket || core.is_baby() {
            return None;
        }
        core.sound_as(
            if self.screaming {
                SoundEvent::EntityGoatScreamingMilk
            } else {
                SoundEvent::EntityGoatMilk
            },
            play::sound_category::PLAYERS,
            1.0,
            1.0,
            cx,
        );
        Some(HandEffect::Exchange(Item::MilkBucket))
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if let Some(jump) = core.goals.find_mut::<super::super::long_jump::LongJump>() {
            jump.screaming = self.screaming;
        }
        let first = self.initial_ram_cooldown.take();
        let Some(ram) = core.goals.find_mut::<RamGoal>() else {
            return;
        };
        if let Some(cooldown) = first {
            ram.set_cooldown(cooldown);
        }
        ram.sync(self.screaming);
        if ram.take_horn_break() {
            self.drop_horn(core, cx);
        }
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        let selected = if rng.r#gen() { self } else { partner };
        Self {
            initial_ram_cooldown: Some(rng.gen_range(600..=6000)),
            screaming: selected.screaming || rng.gen_range(0.0..1.0) < 0.02,
            left_horn: true,
            right_horn: true,
        }
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.screaming = cx.rng.r#gen::<f64>() < 0.02;
        if !core.is_baby() && cx.rng.r#gen::<f32>() < 0.1 {
            if cx.rng.r#gen() {
                self.left_horn = false;
            } else {
                self.right_horn = false;
            }
        }
    }

    fn loot_facts(&self, facts: &mut EntityFacts) {
        facts
            .components
            .push(("minecraft:goat/has_left_horn", self.left_horn.to_string()));
        facts
            .components
            .push(("minecraft:goat/has_right_horn", self.right_horn.to_string()));
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        animal_experience(rng)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}
