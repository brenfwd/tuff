use super::*;

/// How long an untouched skeleton trap horse lasts before it vanishes.
pub(super) const TRAP_MAX_LIFE: i32 = 18000;

/// A skeleton horse; a trap one (`MobCore::signal` 1, so its goal sees
/// it) springs four skeleton horsemen when a player comes near.
#[derive(Debug, Default)]
pub struct SkeletonHorse {
    pub(super) equine: Equine,
    pub(super) trap_time: i32,
}

impl SkeletonHorse {
    pub(super) const PROFILE: Profile = SKELETON_HORSE;

    pub(super) fn own_metadata(&self) -> Vec<(u8, MetadataValue)> {
        Vec::new()
    }

    pub(super) fn chest_columns(&self) -> u8 {
        0
    }

    pub(super) fn refuses_rider(&self, _core: &MobCore) -> bool {
        false
    }
}

impl Species for SkeletonHorse {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        configure_equine(core);
        core.age.as_mut().expect("equines are animals").never_ages = true;
        let mut goals = GoalSelector::default();
        goals.add(1, super::super::skeleton_trap::SkeletonTrap);
        add_common_goals(&mut goals, core, None);
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.equine.load(core, nbt, &SKELETON_HORSE);
        let trap = take_bool(nbt, "SkeletonTrap").unwrap_or(false);
        self.trap_time = take_number(nbt, "SkeletonTrapTime").map_or(0, |v| v as i32);
        core.signal = i32::from(trap);
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        self.equine.save(core, out, &SKELETON_HORSE);
        out.insert("SkeletonTrap", core.signal != 0);
        out.insert("SkeletonTrapTime", self.trap_time);
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(if core.body.eyes_in_water {
                SoundEvent::EntitySkeletonHorseAmbientWater
            } else {
                SoundEvent::EntitySkeletonHorseAmbient
            }),
            hurt: Some(SoundEvent::EntitySkeletonHorseHurt),
            death: Some(SoundEvent::EntitySkeletonHorseDeath),
            step: None,
            volume: 0.8,
        }
    }

    equine_species!();

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.equine.tick(core, cx);
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.equine
            .ai_step(core, &SKELETON_HORSE, self.sounds(core).ambient, cx);
        if core.signal != 0 && !core.persistence_required {
            self.trap_time += 1;
            if self.trap_time > TRAP_MAX_LIFE {
                core.discarded = true;
            }
        }
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.set_attribute_base(Attribute::JumpStrength, roll_jump(cx.rng));
    }
}

#[derive(Debug, Default)]
pub struct ZombieHorse {
    pub(super) equine: Equine,
}

impl ZombieHorse {
    pub(super) const PROFILE: Profile = ZOMBIE_HORSE;

    pub(super) fn own_metadata(&self) -> Vec<(u8, MetadataValue)> {
        Vec::new()
    }

    pub(super) fn chest_columns(&self) -> u8 {
        0
    }

    pub(super) fn refuses_rider(&self, _core: &MobCore) -> bool {
        false
    }
}

impl Species for ZombieHorse {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        configure_equine(core);
        core.age.as_mut().expect("equines are animals").never_ages = true;
        let mut goals = GoalSelector::default();
        goals.add(0, super::super::goals::Float::new(core));
        goals.add(1, RunAroundLikeCrazy::new(1.2));
        goals.add(2, Breed::new(1.0));
        goals.add(3, Tempt::new(1.25, zombie_horse_food));
        goals.add(4, FollowParent::new(1.0));
        goals.add(6, RandomStroll::water_avoiding(0.7));
        goals.add(7, LookAtPlayer::new(6.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.equine.load(core, nbt, &ZOMBIE_HORSE);
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        self.equine.save(core, out, &ZOMBIE_HORSE);
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntityZombieHorseAmbient),
            hurt: Some(SoundEvent::EntityZombieHorseHurt),
            death: Some(SoundEvent::EntityZombieHorseDeath),
            step: None,
            volume: 0.8,
        }
    }

    equine_species!();

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.equine.tick(core, cx);
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.equine
            .ai_step(core, &ZOMBIE_HORSE, self.sounds(core).ambient, cx);
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.set_attribute_base(Attribute::JumpStrength, roll_zombie_jump(cx.rng));
        core.set_attribute_base(Attribute::MovementSpeed, roll_zombie_speed(cx.rng));
    }
}
