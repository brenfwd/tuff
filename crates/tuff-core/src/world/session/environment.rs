//! Environmental damage and status: fire, lava, block contact hazards
//! (magma, campfires, berry bushes), the world border and breathing.

use rand::Rng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::entity::MAX_AIR_SUPPLY;
use tuff_vanilla_data::registry::{Attribute, MobEffect, SoundEvent};

use super::*;
use crate::world::effects::ActiveEffect;

const TURTLE_HELMET_TICKS: i64 = 200;
const TURTLE_HELMET_REFRESH: i64 = 10;
const POWDER_SNOW_DRIFT: f32 = 1.0 / 12.0;
use crate::protocol::play;
use crate::world::{block_contact, framed, freezing, packets};
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::EntityType;

impl PlayerSession {
    /// Sets this player on fire for `seconds`, less with Fire Protection,
    /// unless it already burns longer.
    pub fn ignite(&mut self, seconds: f32) {
        if self.is_dead() {
            return;
        }
        let ticks = (seconds * 20.0).floor() as i32;
        let ticks = (ticks as f64 * self.attribute(Attribute::BurningTime)).ceil() as i32;
        if self.body.fire_ticks < ticks {
            self.body.fire_ticks = ticks;
        }
    }

    /// Burning: one point of damage a second until the fire runs out.
    pub async fn tick_fire(&mut self) {
        if self.body.fire_ticks <= 0 {
            return;
        }
        if self.body.fall.in_water() {
            self.extinguish().await;
            return;
        }
        if self.body.fire_ticks % 20 == 0 && !self.surroundings.in_lava {
            self.damage(1.0, &damage_type::ON_FIRE, None).await;
        }
        self.body.fire_ticks -= 1;
    }

    /// Lava sets the player alight for 15 seconds and hurts it every tick
    /// it can be hurt, with the burn sound when it is; it also slows a
    /// fall.
    pub async fn tick_lava(&mut self) {
        if !self.surroundings.in_lava {
            return;
        }
        self.body.fall.distance *= 0.5;
        self.ignite(LAVA_BURN_SECONDS);
        if self.damage(LAVA_DAMAGE, &damage_type::LAVA, None).await {
            let pitch = 2.0 + self.rng.r#gen::<f32>() * 0.4;
            self.player_sound(SoundEvent::EntityGenericBurn, 0.4, pitch)
                .await;
        }
    }

    /// Magma burns the feet of a player on it who does not sneak, lit
    /// campfires burn a player inside them, grown berry bushes prick a
    /// player moving through them, cactus pricks a player touching it, and
    /// a suffocating block filling its eyes chokes it unless it is
    /// sleeping.
    pub async fn tick_block_contact(&mut self) {
        let contact = self.surroundings.contact;
        if contact.hot_floor && self.pose.on_ground && !self.pose.sneaking {
            self.damage(
                block_contact::HOT_FLOOR_DAMAGE,
                &damage_type::HOT_FLOOR,
                None,
            )
            .await;
        }
        if let Some(amount) = contact.campfire {
            self.damage(amount, &damage_type::CAMPFIRE, None).await;
        }
        if contact.pricks(self.sync.known_movement) {
            self.damage(
                block_contact::BERRY_BUSH_DAMAGE,
                &damage_type::SWEET_BERRY_BUSH,
                None,
            )
            .await;
        }
        if contact.cactus {
            self.damage(block_contact::CACTUS_DAMAGE, &damage_type::CACTUS, None)
                .await;
        }
        if contact.wither_rose
            && self.shared.difficulty() != crate::world::player::Difficulty::Peaceful
            && matches!(
                self.mode.game_mode,
                GameMode::Survival | GameMode::Adventure
            )
        {
            self.add_effect(ActiveEffect::timed(
                MobEffect::Wither,
                block_contact::WITHER_ROSE_TICKS,
                0,
            ))
            .await;
        }
        if contact.suffocating && self.sleep.sleeping.is_none() {
            self.damage(
                block_contact::SUFFOCATION_DAMAGE,
                &damage_type::IN_WALL,
                None,
            )
            .await;
        } else if let Some(amount) = self.border_damage() {
            self.damage(amount, &damage_type::OUTSIDE_BORDER, None)
                .await;
        }
    }

    /// What the world border does to a player whose box pokes out of it:
    /// nothing within the safe zone, then damage per block beyond it.
    pub fn border_damage(&self) -> Option<f32> {
        let border = self.dimension().border();
        let [x, _, z] = self.pose.position;
        let half = PLAYER_HALF_WIDTH;
        if border.contains_box([x - half, z - half], [x + half, z + half]) {
            return None;
        }
        let beyond = border.distance(x, z) + border.safe_zone;
        if beyond >= 0.0 || border.damage_per_block <= 0.0 {
            return None;
        }
        Some(((-beyond * border.damage_per_block).floor() as i32).max(1) as f32)
    }

    /// Fire blocks hurt the player and, once its immunity after the last
    /// fire has run out, set it alight for eight seconds.
    pub async fn tick_in_fire(&mut self) {
        let Some(damage) = self.surroundings.in_fire else {
            return;
        };
        if self.is_dead() {
            return;
        }
        if self.body.fire_ticks < 0 {
            self.body.fire_ticks += 1;
        } else {
            self.body.fire_ticks += self.rng.gen_range(1..3);
        }
        if self.body.fire_ticks >= 0 {
            self.ignite(IN_FIRE_BURN_SECONDS);
        }
        self.damage(damage, &damage_type::IN_FIRE, None).await;
    }

    /// After this tick's lava and fire contact: rain puts the fire out
    /// (with the sound if it was burning before them), and a player that
    /// is not burning and was not set alight again needs
    /// [`FIRE_IMMUNE_TICKS`] in fire blocks before they light it.
    pub async fn settle_fire(&mut self, before: i32) {
        if self.surroundings.contact.in_rain {
            self.body.fire_ticks = self.body.fire_ticks.min(0);
        }
        if before > 0 && self.body.fire_ticks <= 0 {
            self.extinguish_sound().await;
        }
        if self.body.fire_ticks <= 0 && self.body.fire_ticks <= before {
            self.body.fire_ticks = -FIRE_IMMUNE_TICKS;
        }
    }

    /// Whether this player can freeze: not a spectator and wearing no
    /// leather.
    pub fn can_freeze(&self) -> bool {
        let armor = [
            EquipmentSlot::Head,
            EquipmentSlot::Chest,
            EquipmentSlot::Legs,
            EquipmentSlot::Feet,
        ]
        .map(|slot| self.inventory.equipped(slot).map(|stack| stack.item));
        freezing::can_freeze(
            EntityType::Player,
            self.mode.game_mode == GameMode::Spectator,
            armor.iter().map(Option::as_ref),
        )
    }

    /// Powder snow freezes a player standing in it; fully frozen, it takes
    /// freeze damage every two seconds unless the `freeze_damage` rule is
    /// off. Fire thaws it at once.
    pub async fn tick_freezing(&mut self) {
        let can_freeze = self.can_freeze();
        let inside = self.surroundings.contact.freezing;
        if inside.thaws || self.surroundings.in_fire.is_some() {
            self.body.ticks_frozen = 0;
        }
        self.body.ticks_frozen =
            freezing::step(self.body.ticks_frozen, inside.powder_snow, can_freeze);
        if freezing::hurts(self.tick, self.body.ticks_frozen, can_freeze)
            && self.shared.rule_bool("freeze_damage")
        {
            self.damage(freezing::FREEZE_DAMAGE, &damage_type::FREEZE, None)
                .await;
        }
        if let Some(pos) = inside.powder_snow_at {
            self.inside_powder_snow(pos).await;
        }
        self.inside_cauldron().await;
    }

    /// A burning player in a water or powder snow cauldron puts the fire
    /// out and drains a level; in lava it thaws, ignites and takes lava
    /// damage.
    async fn inside_cauldron(&mut self) {
        let cauldron = self.surroundings.contact.cauldron;
        if let Some(pos) = cauldron.extinguishes
            && self.body.fire_ticks > 0
        {
            let chunk = self.chunk();
            if let Some(region) = self.region(chunk.region()).await {
                region.request(RegionRequest::LowerCauldron { pos }).await;
            }
            self.body.fire_ticks = 0;
        }
        if cauldron.lava {
            self.body.ticks_frozen = 0;
            self.ignite(LAVA_BURN_SECONDS);
            if self.damage(LAVA_DAMAGE, &damage_type::LAVA, None).await {
                let pitch = 2.0 + self.rng.r#gen::<f32>() * 0.4;
                self.player_sound(SoundEvent::EntityGenericBurn, 0.4, pitch)
                    .await;
            }
        }
    }

    /// Powder snow puts out a burning player, melting away under it, and
    /// puffs snowflakes as the player wades through.
    async fn inside_powder_snow(&mut self, pos: BlockPos) {
        let before = std::mem::replace(&mut self.body.wade_position, self.pose.position);
        let moving = self.pose.position[0] != before[0] || self.pose.position[2] != before[2];
        if moving && self.rng.r#gen::<bool>() {
            let [x, _, z] = self.pose.position;
            let mut drift = || (self.rng.r#gen::<f32>() * 2.0 - 1.0) * POWDER_SNOW_DRIFT;
            let (dx, dz) = (drift(), drift());
            let flake = framed(&play::Particles {
                particle: tuff_vanilla_data::registry::ParticleType::Snowflake.id(),
                position: [x, f64::from(pos.y + 1), z],
                spread: [dx, 0.05, dz],
                speed: 1.0,
                count: 0,
                alternative: true,
            });
            self.broadcast_entity(packets([flake]), true).await;
        }
        if self.body.fire_ticks > 0 {
            let chunk = self.chunk();
            if let Some(region) = self.region(chunk.region()).await {
                region.request(RegionRequest::MeltPowderSnow { pos }).await;
            }
            self.body.fire_ticks = 0;
            self.extinguish_sound().await;
        }
    }

    /// Starts swimming when sprinting with the eyes under water and the
    /// feet in a water block, and keeps it up while sprinting in water.
    pub fn update_swimming(&mut self) {
        let in_water = self.body.fall.in_water();
        let can_start = self.surroundings.submersion.eyes && self.surroundings.submersion.feet;
        self.pose.swimming = !self.mode.abilities.flying
            && self.ride.riding.is_none()
            && self.pose.sprinting
            && in_water
            && (self.pose.swimming || can_start);
    }

    /// Under water the air runs out (Respiration may skip a tick) and the
    /// player then drowns every 20 ticks; anywhere else it refills.
    pub async fn tick_air(&mut self) {
        self.turtle_helmet().await;
        if !self.surroundings.submersion.holding_breath() {
            if self.body.air < MAX_AIR_SUPPLY {
                self.body.air = (self.body.air + AIR_REFILL).min(MAX_AIR_SUPPLY);
            }
            return;
        }
        let breathing = [
            MobEffect::WaterBreathing,
            MobEffect::ConduitPower,
            MobEffect::BreathOfTheNautilus,
        ]
        .into_iter()
        .any(|effect| self.body.effects.has(effect));
        if breathing || self.mode.abilities.invulnerable {
            let refills = !self.body.effects.has(MobEffect::BreathOfTheNautilus)
                || self.body.effects.has(MobEffect::WaterBreathing)
                || self.body.effects.has(MobEffect::ConduitPower);
            if refills && self.body.air < MAX_AIR_SUPPLY {
                self.body.air = (self.body.air + AIR_REFILL).min(MAX_AIR_SUPPLY);
            }
            return;
        }
        let bonus = self.attribute(Attribute::OxygenBonus);
        let keeps = bonus > 0.0 && self.rng.r#gen::<f64>() >= 1.0 / (bonus + 1.0);
        if !keeps {
            self.body.air -= 1;
        }
        if self.body.air <= DROWNING_AIR {
            self.body.air = 0;
            let event = packets([framed(&play::EntityEvent {
                entity_id: self.id,
                status: play::entity_event::DROWN_PARTICLES,
            })]);
            self.broadcast_entity(event, true).await;
            if self.shared.rule_bool("drowning_damage") {
                self.damage(DROWNING_DAMAGE, &damage_type::DROWN, None)
                    .await;
            }
        }
    }

    /// A turtle helmet keeps ten seconds of Water Breathing topped up while
    /// the eyes are out of the water.
    async fn turtle_helmet(&mut self) {
        let worn = self
            .inventory
            .equipped(EquipmentSlot::Head)
            .is_some_and(|stack| stack.item == tuff_vanilla_data::Item::TurtleHelmet);
        if !worn || self.surroundings.submersion.eyes {
            return;
        }
        let expiring = self
            .body
            .effects
            .get(MobEffect::WaterBreathing)
            .is_none_or(|active| active.duration <= TURTLE_HELMET_REFRESH);
        if expiring {
            let mut effect = ActiveEffect::new(&tuff_vanilla_data::item::EffectInstance {
                effect: MobEffect::WaterBreathing,
                duration: TURTLE_HELMET_TICKS,
                amplifier: 0,
                ambient: false,
                show_particles: false,
                show_icon: true,
            });
            effect.show_particles = false;
            self.add_effect(effect).await;
        }
    }

    /// Water puts the fire out.
    pub async fn extinguish(&mut self) {
        if self.body.fire_ticks <= 0 {
            return;
        }
        self.body.fire_ticks = 0;
        self.extinguish_sound().await;
    }

    async fn extinguish_sound(&mut self) {
        let pitch = 1.6 + (self.rng.r#gen::<f32>() - self.rng.r#gen::<f32>()) * 0.4;
        self.player_sound(SoundEvent::EntityGenericExtinguishFire, 0.7, pitch)
            .await;
    }
}
