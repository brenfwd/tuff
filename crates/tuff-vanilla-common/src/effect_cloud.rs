//! Area effect clouds: what lingering potions leave and the dragon
//! breathes. Clouds hurt or affect living things standing in them every
//! so often, and the dragon's breath can be bottled.

use std::collections::HashMap;

use rand::Rng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::{EntityType, ParticleType};

use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::effects;
use tuff_core::world::entity::{
    Culprit, EntityBase, EntityHit, Notice, RegionEntity, Tick, TickContext, take, take_number,
};
use tuff_item::item::PotionContents;
use tuff_nbt::{Compound, Tag};

const DATA_RADIUS: u8 = 8;
const DATA_WAITING: u8 = 9;
const DATA_PARTICLE: u8 = 10;
const APPLY_INTERVAL: i32 = 5;
const MIN_RADIUS: f32 = 0.5;
const CLOUD_HEIGHT: f64 = 0.5;
const DEFAULT_WAIT: i32 = 20;
const DEFAULT_REAPPLICATION_DELAY: i32 = 20;
const INSTANT_DAMAGE: &str = "minecraft:instant_damage";

/// A lingering cloud: harming (the dragon's breath) or carrying a potion
/// whose effects it applies to everything inside.
#[derive(Debug)]
pub struct AreaEffectCloud {
    pub base: EntityBase,
    age: i32,
    radius: f32,
    radius_per_tick: f32,
    radius_on_use: f32,
    duration: i32,
    duration_on_use: i32,
    wait_time: i32,
    reapplication_delay: i32,
    potion_duration_scale: f32,
    /// The amplifier of its instant damage, if it has that effect.
    harm: Option<u8>,
    /// The dragon breath particle's power, or the saved particle as is.
    power: Option<f32>,
    raw_particle: Option<Tag>,
    raw_potion: Option<Tag>,
    /// The potion it applies, when this server understands it.
    potion: Option<PotionContents>,
    owner: Option<Culprit>,
    owner_uuid: Option<u128>,
    waiting: bool,
    victims: HashMap<i32, i32>,
    changed: Vec<u8>,
}

impl AreaEffectCloud {
    fn new(id: i32, position: [f64; 3], owner: Option<Culprit>, rng: &mut impl Rng) -> Self {
        let base = EntityBase::new(
            EntityType::AreaEffectCloud,
            id,
            rng.r#gen(),
            position,
            [0.0; 3],
        );
        Self {
            base,
            age: 0,
            radius: 3.0,
            radius_per_tick: 0.0,
            radius_on_use: 0.0,
            duration: 600,
            duration_on_use: 0,
            wait_time: DEFAULT_WAIT,
            reapplication_delay: DEFAULT_REAPPLICATION_DELAY,
            potion_duration_scale: 0.25,
            harm: Some(0),
            power: Some(1.0),
            raw_particle: None,
            raw_potion: None,
            potion: None,
            owner_uuid: owner.as_ref().map(|o| o.uuid),
            owner,
            waiting: true,
            victims: HashMap::new(),
            changed: Vec::new(),
        }
    }

    /// The cloud a breath fireball leaves: it grows from 3 to 7 blocks
    /// over 30 seconds and harms at level II.
    pub fn lingering(
        id: i32,
        position: [f64; 3],
        owner: Option<Culprit>,
        rng: &mut impl Rng,
    ) -> Self {
        let mut cloud = Self::new(id, position, owner, rng);
        cloud.radius = 3.0;
        cloud.duration = 600;
        cloud.radius_per_tick = (7.0 - cloud.radius) / cloud.duration as f32;
        cloud.harm = Some(1);
        cloud
    }

    /// The flame a perched dragon breathes: 5 blocks wide for 10 seconds.
    pub fn flame(id: i32, position: [f64; 3], owner: Culprit, rng: &mut impl Rng) -> Self {
        let mut cloud = Self::new(id, position, Some(owner), rng);
        cloud.radius = 5.0;
        cloud.duration = 200;
        cloud.harm = Some(0);
        cloud
    }

    /// The cloud a lingering potion leaves: 3 blocks wide, shrinking to
    /// nothing over 30 seconds and by half a block for each one it affects.
    pub fn from_potion(
        id: i32,
        position: [f64; 3],
        owner: Option<Culprit>,
        potion: PotionContents,
        duration_scale: f32,
        rng: &mut impl Rng,
    ) -> Self {
        let mut cloud = Self::new(id, position, owner, rng);
        cloud.radius = 3.0;
        cloud.radius_on_use = -0.5;
        cloud.duration = 600;
        cloud.wait_time = 10;
        cloud.radius_per_tick = -cloud.radius / cloud.duration as f32;
        cloud.potion_duration_scale = duration_scale;
        cloud.harm = harm_amplifier(&Tag::Compound(potion.to_nbt()));
        cloud.power = None;
        cloud.potion = Some(potion);
        cloud
    }

    /// The cloud an exploding creeper leaves for the effects it carries:
    /// 2.5 blocks wide, shrinking to nothing over 15 seconds.
    pub fn from_effects(
        id: i32,
        position: [f64; 3],
        effects: Vec<tuff_item::item::PotionEffect>,
        rng: &mut impl Rng,
    ) -> Self {
        let contents = PotionContents {
            potion: None,
            custom_color: None,
            custom_effects: effects,
            custom_name: None,
        };
        let mut cloud = Self::from_potion(id, position, None, contents, 0.25, rng);
        cloud.radius = 2.5;
        cloud.duration = 300;
        cloud.radius_per_tick = -cloud.radius / cloud.duration as f32;
        cloud
    }

    pub fn radius(&self) -> f32 {
        self.radius
    }

    /// Whether the dragon breathed it: its owner, or (once reloaded, when
    /// only the UUID is known) the fight's dragon.
    pub fn from_dragon(&self, dragon: Option<u128>) -> bool {
        self.owner_uuid.is_some()
            && self.owner_uuid == dragon
            && self
                .owner
                .as_ref()
                .is_none_or(|o| o.kind == EntityType::EnderDragon)
    }

    /// A glass bottle scoops some of it up.
    pub fn bottle(&mut self) {
        self.set_radius(self.radius - 0.5);
    }

    fn set_radius(&mut self, radius: f32) {
        let radius = radius.clamp(0.0, 32.0);
        if radius != self.radius {
            self.radius = radius;
            self.changed.push(DATA_RADIUS);
        }
    }

    fn value(&self, index: u8) -> Option<MetadataValue> {
        Some(match index {
            DATA_RADIUS => MetadataValue::Float(self.radius),
            DATA_WAITING => MetadataValue::Boolean(self.waiting),
            DATA_PARTICLE => match (&self.potion, self.power, &self.raw_particle) {
                (Some(potion), None, None) => MetadataValue::Particle {
                    id: ParticleType::EntityEffect.id() as i32,
                    options: (effects::potion_color(potion) as u32 | 0xFF00_0000)
                        .to_be_bytes()
                        .to_vec(),
                },
                _ => MetadataValue::Particle {
                    id: ParticleType::DragonBreath.id() as i32,
                    options: self.power.unwrap_or(1.0).to_be_bytes().to_vec(),
                },
            },
            _ => return None,
        })
    }

    fn harm(&self, amount: f32, cx: &TickContext) -> EntityHit {
        let mut hit = EntityHit::new(amount, &damage_type::INDIRECT_MAGIC);
        hit.attacker = self.owner.clone().or_else(|| {
            let uuid = self.owner_uuid?;
            let entity = cx.entities.iter().find(|e| e.uuid == uuid)?;
            let base = EntityBase::new(entity.kind, entity.id, uuid, entity.position, [0.0; 3]);
            Some(Culprit::entity(&base))
        });
        hit
    }
}

impl AreaEffectCloud {
    fn expired_at(&self, age: i32) -> bool {
        self.duration != -1 && age - self.wait_time >= self.duration
    }
}

impl RegionEntity for AreaEffectCloud {
    fn ignores_pistons(&self) -> bool {
        true
    }

    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn bounds(&self) -> [f64; 6] {
        let [x, y, z] = self.base.position;
        let half = f64::from(self.radius);
        [x - half, y, z - half, x + half, y + CLOUD_HEIGHT, z + half]
    }

    fn save(&self, out: &mut Compound) {
        out.insert("Age", self.age);
        out.insert("Duration", self.duration);
        out.insert("WaitTime", self.wait_time);
        out.insert("ReapplicationDelay", self.reapplication_delay);
        out.insert("DurationOnUse", self.duration_on_use);
        out.insert("RadiusOnUse", self.radius_on_use);
        out.insert("RadiusPerTick", self.radius_per_tick);
        out.insert("Radius", self.radius);
        out.insert("potion_duration_scale", self.potion_duration_scale);
        match (&self.raw_particle, self.power) {
            (Some(raw), _) => {
                out.insert("custom_particle", raw.clone());
            }
            (None, Some(power)) => {
                out.insert(
                    "custom_particle",
                    Compound::new()
                        .with("type", "minecraft:dragon_breath")
                        .with("power", power),
                );
            }
            (None, None) => {}
        }
        match (&self.raw_potion, self.harm) {
            _ if self.potion.is_some() && self.raw_potion.is_none() => {
                out.insert(
                    "potion_contents",
                    self.potion.as_ref().expect("checked").to_nbt(),
                );
            }
            (Some(raw), _) => {
                out.insert("potion_contents", raw.clone());
            }
            (None, Some(amplifier)) => {
                let effect = Compound::new()
                    .with("id", INSTANT_DAMAGE)
                    .with("amplifier", amplifier as i8)
                    .with("duration", 1);
                out.insert(
                    "potion_contents",
                    Compound::new().with("custom_effects", Tag::List(vec![Tag::Compound(effect)])),
                );
            }
            (None, None) => {}
        }
        if let Some(uuid) = self.owner_uuid {
            out.insert("Owner", Tag::uuid(uuid));
        }
    }

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        [DATA_RADIUS, DATA_WAITING, DATA_PARTICLE]
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

    fn notice(&mut self, notice: Notice) {
        if notice == Notice::Discard {
            self.duration = 0;
            self.wait_time = 0;
        }
    }

    fn tick(&mut self, cx: &mut TickContext) -> Tick {
        self.age += 1;
        let age = self.age;
        if self.expired_at(age) {
            return Tick::Discard;
        }
        let should_wait = age < self.wait_time;
        if self.waiting != should_wait {
            self.waiting = should_wait;
            self.changed.push(DATA_WAITING);
        }
        if should_wait {
            return Tick::Alive;
        }
        let mut radius = self.radius;
        if self.radius_per_tick != 0.0 {
            radius += self.radius_per_tick;
            if radius < MIN_RADIUS {
                return Tick::Discard;
            }
            self.set_radius(radius);
        }
        if age % APPLY_INTERVAL != 0 {
            return Tick::Alive;
        }
        self.victims.retain(|_, until| age < *until);
        let lasting: Vec<effects::ActiveEffect> = self
            .potion
            .as_ref()
            .map(|potion| effects::potion_effects(potion, self.potion_duration_scale))
            .unwrap_or_default();
        let mut instant: Vec<(tuff_vanilla_data::registry::MobEffect, u8)> = lasting
            .iter()
            .filter(|e| e.effect.is_instantaneous())
            .map(|e| (e.effect, e.amplifier))
            .collect();
        let lasting: Vec<effects::ActiveEffect> = lasting
            .into_iter()
            .filter(|e| !e.effect.is_instantaneous())
            .collect();
        if let Some(amplifier) = self.harm
            && !instant
                .iter()
                .any(|(e, _)| *e == tuff_vanilla_data::registry::MobEffect::InstantDamage)
        {
            instant.push((
                tuff_vanilla_data::registry::MobEffect::InstantDamage,
                amplifier,
            ));
        }
        if instant.is_empty() && lasting.is_empty() {
            self.victims.clear();
            return Tick::Alive;
        }
        let area = self.bounds();
        let inside = |b: [f64; 6]| {
            area[0] < b[3]
                && area[3] > b[0]
                && area[1] < b[4]
                && area[4] > b[1]
                && area[2] < b[5]
                && area[5] > b[2]
        };
        let [x, _, z] = self.base.position;
        let targets: Vec<(i32, bool, EntityType, [f64; 3])> = cx
            .players
            .iter()
            .filter(|p| inside(p.bounds()))
            .map(|p| (p.id, true, EntityType::Player, p.position))
            .chain(
                cx.entities
                    .iter()
                    .filter(|e| e.living.is_some_and(|l| l.alive))
                    .filter(|e| inside(e.bounds))
                    .map(|e| (e.id, false, e.kind, e.position)),
            )
            .collect();
        let culprit = self.harm(0.0, cx).attacker;
        for (id, player, kind, position) in targets {
            if self.victims.contains_key(&id) {
                continue;
            }
            let (dx, dz) = (position[0] - x, position[2] - z);
            if dx * dx + dz * dz > f64::from(radius * radius) {
                continue;
            }
            self.victims.insert(id, age + self.reapplication_delay);
            effects::apply_potion(
                id,
                player,
                kind,
                &lasting,
                &instant,
                0.5,
                false,
                culprit.clone(),
                cx,
            );
            if self.radius_on_use != 0.0 {
                radius += self.radius_on_use;
                if radius < MIN_RADIUS {
                    return Tick::Discard;
                }
                self.set_radius(radius);
            }
            if self.duration_on_use != 0 && self.duration != -1 {
                self.duration += self.duration_on_use;
                if self.duration <= 0 {
                    return Tick::Discard;
                }
            }
        }
        Tick::Alive
    }
}

fn harm_amplifier(potion: &Tag) -> Option<u8> {
    let effects = potion.as_compound()?.get("custom_effects")?.as_list()?;
    effects.iter().find_map(|effect| {
        let effect = effect.as_compound()?;
        (effect.get_str("id")? == INSTANT_DAMAGE).then(|| {
            effect
                .get("amplifier")
                .and_then(|a| match a {
                    Tag::Byte(v) => Some(*v as u8),
                    Tag::Int(v) => Some(*v as u8),
                    _ => None,
                })
                .unwrap_or(0)
        })
    })
}

pub fn load_cloud(base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    let mut cloud = AreaEffectCloud::new(base.id, base.position, None, &mut rand::thread_rng());
    cloud.base = base;
    let int = |nbt: &mut Compound, key: &str, default: i32| {
        take_number(nbt, key).map_or(default, |v| v as i32)
    };
    let float = |nbt: &mut Compound, key: &str, default: f32| {
        take_number(nbt, key).map_or(default, |v| v as f32)
    };
    cloud.age = int(nbt, "Age", 0);
    cloud.duration = int(nbt, "Duration", 600);
    cloud.wait_time = int(nbt, "WaitTime", DEFAULT_WAIT);
    cloud.reapplication_delay = int(nbt, "ReapplicationDelay", DEFAULT_REAPPLICATION_DELAY);
    cloud.duration_on_use = int(nbt, "DurationOnUse", 0);
    cloud.radius_on_use = float(nbt, "RadiusOnUse", 0.0);
    cloud.radius_per_tick = float(nbt, "RadiusPerTick", 0.0);
    cloud.radius = float(nbt, "Radius", 3.0);
    cloud.potion_duration_scale = float(nbt, "potion_duration_scale", 1.0);
    cloud.owner_uuid = take(nbt, "Owner", Tag::as_uuid);
    let particle = nbt.remove("custom_particle");
    cloud.power = match &particle {
        Some(Tag::Compound(p)) if p.get_str("type") == Some("minecraft:dragon_breath") => Some(
            p.get("power")
                .and_then(|v| match v {
                    Tag::Float(f) => Some(*f),
                    _ => None,
                })
                .unwrap_or(1.0),
        ),
        _ => None,
    };
    if cloud.power.is_none() {
        cloud.raw_particle = particle;
    }
    let potion = nbt.remove("potion_contents");
    cloud.harm = potion.as_ref().and_then(harm_amplifier);
    cloud.potion = potion.as_ref().and_then(PotionContents::from_nbt);
    cloud.raw_potion = potion;
    cloud.waiting = cloud.age < cloud.wait_time;
    Some(Box::new(cloud))
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_cloud_is_the_dragons_only_while_that_dragon_lives() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut cloud = AreaEffectCloud::lingering(5, [1.0, 64.0, 2.0], None, &mut rng);
        cloud.owner_uuid = Some(9);
        assert!(cloud.from_dragon(Some(9)));
        assert!(!cloud.from_dragon(None));
        assert!(!cloud.from_dragon(Some(8)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_cloud_expires_once_its_incremented_age_reaches_wait_plus_duration() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut cloud = AreaEffectCloud::lingering(5, [0.0; 3], None, &mut rng);
        cloud.duration = 10;
        cloud.wait_time = 4;
        assert!(!cloud.expired_at(13));
        assert!(cloud.expired_at(14));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_lingering_cloud_grows_to_seven_blocks_and_round_trips() {
        let mut rng = StdRng::seed_from_u64(1);
        let cloud = AreaEffectCloud::lingering(5, [1.0, 64.0, 2.0], None, &mut rng);
        assert!((cloud.radius_per_tick * 600.0 - 4.0).abs() < 1e-4);
        let saved = tuff_core::world::entity::save(&cloud);
        let mut registry = tuff_core::world::entity::Registry::default();
        registry.register(EntityType::AreaEffectCloud, load_cloud);
        let loaded = registry.load(&saved, 7).expect("clouds load");
        let loaded = loaded
            .downcast_ref::<AreaEffectCloud>()
            .expect("an area effect cloud");
        assert_eq!(loaded.harm, Some(1));
        assert_eq!(loaded.radius, 3.0);
        assert_eq!(loaded.power, Some(1.0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_the_dragons_breath_is_bottled_half_a_block_at_a_time() {
        let mut rng = StdRng::seed_from_u64(1);
        let dragon = EntityBase::new(EntityType::EnderDragon, 9, 99, [0.0; 3], [0.0; 3]);
        let mut flame = AreaEffectCloud::flame(5, [0.0; 3], Culprit::entity(&dragon), &mut rng);
        assert!(flame.from_dragon(Some(99)));
        assert!(!flame.from_dragon(None));
        flame.bottle();
        assert_eq!(flame.radius(), 4.5);
        let mut reloaded = AreaEffectCloud::lingering(6, [0.0; 3], None, &mut rng);
        reloaded.owner_uuid = Some(99);
        assert!(reloaded.from_dragon(Some(99)));
        assert!(!reloaded.from_dragon(Some(100)));
        let player = Culprit::player(1, 2, "someone");
        let other = AreaEffectCloud::lingering(7, [0.0; 3], Some(player), &mut rng);
        assert!(!other.from_dragon(Some(2)));
    }
}
