//! What every monster shares: hostile sounds, experience, preferring the
//! dark when wandering, idling longer in the light, and undead burning in
//! daylight.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::SoundEvent;

use super::{MobCore, MobSounds};
use tuff_core::world::entity::TickContext;
use tuff_world_model::BlockPos;

/// Experience a player gets for killing a monster, before equipment.
pub const MONSTER_XP: i32 = 5;
const SUN_BURN_SECONDS: f32 = 8.0;

/// A level event (door sounds, conversion cues) for everyone near `pos`.
pub fn level_event(pos: BlockPos, event: i32, data: i32, cx: &mut TickContext) {
    let packet = tuff_core::world::framed(&tuff_core::protocol::play::LevelEvent {
        event,
        position: pos,
        data,
        global: false,
    });
    cx.effects
        .push(tuff_core::world::entity::Effect::ChunkPackets(
            tuff_core::world::packets([packet]),
        ));
}

impl MobCore {
    /// Sets up the monster parts: hostile sounds and experience.
    pub fn make_monster(&mut self) {
        self.hostile = true;
        self.xp_reward = MONSTER_XP;
    }

    /// Random armor for a fresh spawn: rarely, and more often in harder
    /// places, a tier of armor put on from the head down, each further
    /// piece less likely (less so on hard).
    pub fn populate_spawn_armor(&mut self, difficulty: LocalDifficulty, rng: &mut StdRng) {
        if rng.r#gen::<f32>() >= 0.15 * difficulty.special_multiplier() {
            return;
        }
        let mut tier = rng.gen_range(0..3);
        for _ in 0..3 {
            if rng.r#gen::<f32>() < 0.1087 {
                tier += 1;
            }
        }
        let hard = difficulty.base == tuff_core::world::player::Difficulty::Hard;
        let partial = if hard { 0.1 } else { 0.25 };
        for (index, (slot, tiers)) in tuff_vanilla_data::entity::SPAWN_ARMOR.iter().enumerate() {
            if index > 0 && rng.r#gen::<f32>() < partial {
                return;
            }
            if self.equipment.get(*slot).is_none()
                && let Some(item) = tiers.get(tier)
            {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "item a mob spawns, wears or throws (populate_spawn_armor)"
                )]
                self.equipment.set(
                    *slot,
                    Some(tuff_item::item::ItemStack::mint(
                        *item,
                        1,
                        tuff_item::item::MintReason::Spawn,
                    )),
                );
            }
        }
    }

    /// Chances to enchant a fresh spawn's weapon and armor, scaled by the
    /// local difficulty: the weapon a quarter as often, armor half.
    pub fn populate_spawn_enchantments(&mut self, difficulty: LocalDifficulty, rng: &mut StdRng) {
        const SLOTS: [(EquipmentSlot, f32); 5] = [
            (EquipmentSlot::Mainhand, 0.25),
            (EquipmentSlot::Feet, 0.5),
            (EquipmentSlot::Legs, 0.5),
            (EquipmentSlot::Chest, 0.5),
            (EquipmentSlot::Head, 0.5),
        ];
        let special = difficulty.special_multiplier();
        for (slot, chance) in SLOTS {
            if self.equipment.get(slot).is_none() || rng.r#gen::<f32>() >= chance * special {
                continue;
            }
            let Some(stack) = self.equipment.take(slot) else {
                continue;
            };
            let stack = tuff_item::item::enchanting::enchant_from_provider(
                stack,
                "minecraft:mob_spawn_equipment",
                special,
                rng,
            );
            self.equipment.set(slot, Some(stack));
        }
    }

    /// The light level at `pos` mapped to the game's 0..1 brightness,
    /// lifted by the dimension's ambient light.
    pub fn brightness_at(&self, pos: BlockPos, cx: &TickContext) -> f32 {
        let (sky, block) = (cx.light)(pos).unwrap_or((0, 0));
        let raw = sky.saturating_sub(cx.sky_darken).max(block);
        let v = f32::from(raw) / 15.0;
        let curved = v / (4.0 - 3.0 * v);
        cx.ambient_light + (1.0 - cx.ambient_light) * curved
    }

    /// The brightness at its eyes, which drives burning and idling.
    pub fn brightness(&self, cx: &TickContext) -> f32 {
        let [x, _, z] = self.base.position;
        let eye = self.eye_position();
        self.brightness_at(super::physics::block_pos([x, eye[1], z]), cx)
    }

    /// Whether nothing but air stands above `pos` up to the build limit.
    pub fn can_see_sky(&self, pos: BlockPos, cx: &TickContext) -> bool {
        ((pos.y + 1)..=self.max_y).all(|y| {
            (cx.block)(BlockPos::new(pos.x, y, pos.z)).is_none_or(tuff_world_model::chunk::is_air)
        })
    }

    /// The monster part of an AI step: a bright place makes it idle
    /// longer (towards despawning), and undead catch fire in daylight.
    pub fn monster_ai_step(&mut self, cx: &mut TickContext) {
        if self.raider.is_some() || self.brightness(cx) > 0.5 {
            self.no_action_time += 2;
        }
    }

    /// Undead catch fire in the sun unless something on their head blocks
    /// it (and wears instead).
    pub fn burn_in_daylight(&mut self, cx: &mut TickContext) {
        if !self.is_alive() || !self.sun_burn_tick(cx) {
            return;
        }
        if let Some(helmet) = self.equipment.get(EquipmentSlot::Head) {
            if helmet.is_damageable() {
                let wear = cx.rng.gen_range(0..2);
                let damage = helmet.damage() + wear;
                if helmet.max_damage().is_some_and(|max| damage >= max) {
                    if let Some(helmet) = self.equipment.take(EquipmentSlot::Head) {
                        helmet.burn(tuff_item::item::BurnReason::Broken);
                    }
                } else if let Some(helmet) = self.equipment.get_mut(EquipmentSlot::Head) {
                    helmet.set_damage(damage);
                }
            }
            return;
        }
        let ticks = (SUN_BURN_SECONDS * 20.0) as i16;
        if self.base.fire < ticks {
            self.base.fire = ticks;
        }
    }

    fn sun_burn_tick(&self, cx: &mut TickContext) -> bool {
        if !cx.monsters_burn {
            return false;
        }
        let brightness = self.brightness(cx);
        let [x, _, z] = self.base.position;
        let eye = super::physics::block_pos([x, self.eye_position()[1], z]);
        let wet = self.body.in_water || cx.raining && self.can_see_sky(eye, cx);
        brightness > 0.5
            && cx.rng.r#gen::<f32>() * 30.0 < (brightness - 0.4) * 2.0
            && !wet
            && self.can_see_sky(eye, cx)
    }

    /// A monster's wander preference: darker is better.
    pub fn monster_walk_target_value(&self, pos: BlockPos, cx: &TickContext) -> f32 {
        -(self.brightness_at(pos, cx) - 0.5)
    }
}

/// The sounds of a monster with its own ambient, hurt, death and step
/// sounds.
pub fn sounds(
    ambient: SoundEvent,
    hurt: SoundEvent,
    death: SoundEvent,
    step: Option<SoundEvent>,
) -> MobSounds {
    MobSounds {
        ambient: Some(ambient),
        hurt: Some(hurt),
        death: Some(death),
        step,
        volume: 1.0,
    }
}

/// The game's local difficulty: the world difficulty scaled up with world
/// age, the time players have spent near the chunk and the moon's
/// brightness.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalDifficulty {
    pub base: tuff_core::world::player::Difficulty,
    pub effective: f32,
}

impl LocalDifficulty {
    pub fn at(cx: &TickContext, position: [f64; 3]) -> Self {
        let base = *cx.shared.difficulty.borrow();
        let local = (cx.inhabited_time)(tuff_world_model::pos::ChunkPos::of_position(position));
        let moon = tuff_core::world::moon_brightness(cx.shared, cx.dimension);
        Self::new(base, cx.tick, local, moon)
    }

    pub fn in_region(
        world: &dyn tuff_core::world::region_api::RegionWorld,
        position: [f64; 3],
    ) -> Self {
        let shared = world.shared();
        let base = *shared.difficulty.borrow();
        let local = world.inhabited_time(tuff_world_model::pos::ChunkPos::of_position(position));
        let moon = tuff_core::world::moon_brightness(shared, world.dimension_id());
        Self::new(base, world.tick(), local, moon)
    }

    pub fn new(
        base: tuff_core::world::player::Difficulty,
        total_time: u64,
        local_time: u64,
        moon_brightness: f32,
    ) -> Self {
        use tuff_core::world::player::Difficulty;
        let effective = if base == Difficulty::Peaceful {
            0.0
        } else {
            let hard = base == Difficulty::Hard;
            let global = ((total_time as f32 - 72000.0) / 1_440_000.0).clamp(0.0, 1.0) * 0.25;
            let scale = 0.75 + global;
            let mut local =
                (local_time as f32 / 3_600_000.0).clamp(0.0, 1.0) * if hard { 1.0 } else { 0.75 };
            local += (moon_brightness * 0.25).clamp(0.0, global);
            if base == Difficulty::Easy {
                local *= 0.5;
            }
            f32::from(base.id()) * (scale + local)
        };
        Self { base, effective }
    }

    /// 0 below effective difficulty 2, 1 above 4, linear between.
    pub fn special_multiplier(&self) -> f32 {
        if self.effective < 2.0 {
            0.0
        } else if self.effective > 4.0 {
            1.0
        } else {
            (self.effective - 2.0) / 2.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::world::player::Difficulty;

    #[tuff_core::watchdog]
    #[test]
    fn some_monsters_spawn_in_armor_from_the_head_down() {
        super::super::tests::with_context(|mob, cx| {
            let hard = LocalDifficulty::new(Difficulty::Hard, 10_000_000, 0, 0.0);
            let mut armored = 0;
            for _ in 0..2000 {
                mob.equipment = super::super::equipment::Equipment::default();
                mob.populate_spawn_armor(hard, cx.rng);
                let head = mob.equipment.get(EquipmentSlot::Head).map(|s| s.item());
                let feet = mob.equipment.get(EquipmentSlot::Feet);
                if feet.is_some() {
                    assert!(head.is_some(), "armor goes on from the head down");
                }
                if head.is_some() {
                    armored += 1;
                }
            }
            assert!((50..600).contains(&armored), "{armored} of 2000");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn local_difficulty_grows_with_world_age() {
        let fresh = LocalDifficulty::new(Difficulty::Normal, 0, 0, 0.0);
        assert_eq!(fresh.effective, 1.5);
        assert_eq!(fresh.special_multiplier(), 0.0);
        let old = LocalDifficulty::new(Difficulty::Hard, 2_000_000, 3_600_000, 1.0);
        assert_eq!(old.effective, 6.75);
        assert_eq!(old.special_multiplier(), 1.0);
        let easy = LocalDifficulty::new(Difficulty::Easy, 2_000_000, 3_600_000, 1.0);
        assert_eq!(easy.effective, 1.0 * (1.0 + (0.75 + 0.25) * 0.5));
        assert_eq!(
            LocalDifficulty::new(Difficulty::Peaceful, 9_999_999, 0, 1.0).effective,
            0.0
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn undead_catch_fire_under_open_sky_unless_wearing_a_helmet() {
        super::super::tests::with_context(|mob, cx| {
            let mut burned = false;
            for _ in 0..2000 {
                mob.burn_in_daylight(cx);
                if mob.base.fire > 0 {
                    burned = true;
                    break;
                }
            }
            assert!(burned, "full daylight sets it alight within a few seconds");
            assert_eq!(mob.base.fire, 160);
            mob.base.fire = 0;
            let helmet = tuff_item::item::ItemStack::mint(
                tuff_vanilla_data::Item::LeatherHelmet,
                1,
                tuff_item::item::MintReason::Test,
            );
            mob.equipment.set(EquipmentSlot::Head, Some(helmet));
            for _ in 0..2000 {
                mob.burn_in_daylight(cx);
            }
            assert_eq!(mob.base.fire, 0, "the helmet takes it");
            let worn = mob.equipment.get(EquipmentSlot::Head).map(|h| h.damage());
            assert!(worn.is_some_and(|d| d > 0), "and wears: {worn:?}");
            cx.monsters_burn = false;
            mob.equipment.set(EquipmentSlot::Head, None);
            for _ in 0..2000 {
                mob.burn_in_daylight(cx);
            }
            assert_eq!(mob.base.fire, 0, "no burning when the sun does not burn");
        });
    }
}

/// While the sun is out and nothing covers its head, paths stop short of
/// open sky.
#[derive(Debug, Default)]
pub struct RestrictSun;

impl super::goal::Goal for RestrictSun {
    fn flags(&self) -> super::goal::Flags {
        super::goal::Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        cx.sky_darken < 4 && mob.equipment.get(EquipmentSlot::Head).is_none()
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.avoid_sun = true;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.avoid_sun = false;
    }
}

/// A burning mob without a target runs for the shade.
#[derive(Debug)]
pub struct FleeSun {
    speed: f64,
    wanted: [f64; 3],
}

impl FleeSun {
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            wanted: [0.0; 3],
        }
    }
}

impl super::goal::Goal for FleeSun {
    fn flags(&self) -> super::goal::Flags {
        super::goal::Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let here = super::physics::block_pos(mob.base.position);
        if mob.target.is_some()
            || cx.sky_darken >= 4
            || !mob.base.on_fire()
            || !mob.can_see_sky(here, cx)
            || mob.equipment.get(EquipmentSlot::Head).is_some()
        {
            return false;
        }
        for _ in 0..10 {
            let pos = BlockPos::new(
                here.x + cx.rng.gen_range(0..20) - 10,
                here.y + cx.rng.gen_range(0..6) - 3,
                here.z + cx.rng.gen_range(0..20) - 10,
            );
            if !mob.can_see_sky(pos, cx) && mob.walk_target_value(pos, cx) < 0.0 {
                self.wanted = [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5];
                return true;
            }
        }
        false
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.move_to_position(self.wanted, self.speed, cx);
    }
}
