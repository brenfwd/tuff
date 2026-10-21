//! Status effects on a living entity, ticked the way the game ticks them:
//! each effect counts down once per tick, applies its periodic action at its
//! own interval, and a stronger or longer copy takes over (keeping the
//! weaker one hidden underneath to resume when the stronger one ends).

use std::collections::BTreeMap;

use tuff_vanilla_data::item::EffectInstance;
use tuff_vanilla_data::potion::AMBIENT_ALPHA;
use tuff_vanilla_data::registry::{MobEffect, ParticleType};

use tuff_item::item::{EffectDetails, PotionContents};
use tuff_nbt::{Compound, Tag};

/// Vanilla sends a refresh every 600 ticks so clients stay in sync.
const RESYNC_INTERVAL: i64 = 600;
pub const INFINITE: i64 = -1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveEffect {
    pub effect: MobEffect,
    pub amplifier: u8,
    /// Remaining ticks; [`INFINITE`] never runs out.
    pub duration: i64,
    pub ambient: bool,
    pub show_particles: bool,
    pub show_icon: bool,
    hidden: Option<Box<ActiveEffect>>,
}

impl ActiveEffect {
    pub fn new(instance: &EffectInstance) -> Self {
        Self {
            effect: instance.effect,
            amplifier: instance.amplifier.min(u8::MAX as u64) as u8,
            duration: instance.duration.max(INFINITE),
            ambient: instance.ambient,
            show_particles: instance.show_particles,
            show_icon: instance.show_icon,
            hidden: None,
        }
    }

    pub fn timed(effect: MobEffect, duration: i64, amplifier: u8) -> Self {
        Self {
            effect,
            amplifier,
            duration,
            ambient: false,
            show_particles: true,
            show_icon: true,
            hidden: None,
        }
    }

    /// The saved form (`active_effects` entries), hidden copies nested
    /// under `hidden_effect`.
    pub fn to_nbt(&self) -> Compound {
        let mut out = Compound::new().with("id", self.effect.name());
        self.write_details(&mut out);
        out
    }

    fn write_details(&self, out: &mut Compound) {
        out.insert("amplifier", self.amplifier as i8);
        out.insert("duration", self.duration.clamp(-1, i32::MAX as i64) as i32);
        out.insert("ambient", self.ambient);
        out.insert("show_particles", self.show_particles);
        out.insert("show_icon", self.show_icon);
        if let Some(hidden) = &self.hidden {
            let mut details = Compound::new();
            hidden.write_details(&mut details);
            out.insert("hidden_effect", details);
        }
    }

    pub fn from_nbt(nbt: &Compound) -> Option<Self> {
        let effect = MobEffect::from_name(nbt.get_str("id")?)?;
        Some(Self::read_details(effect, nbt))
    }

    fn read_details(effect: MobEffect, nbt: &Compound) -> Self {
        let show_particles = nbt.get_bool("show_particles").unwrap_or(true);
        Self {
            effect,
            amplifier: nbt.get_byte("amplifier").unwrap_or(0) as u8,
            duration: i64::from(nbt.get_int("duration").unwrap_or(0).max(INFINITE as i32)),
            ambient: nbt.get_bool("ambient").unwrap_or(false),
            show_particles,
            show_icon: nbt.get_bool("show_icon").unwrap_or(show_particles),
            hidden: nbt
                .get_compound("hidden_effect")
                .map(|hidden| Box::new(Self::read_details(effect, hidden))),
        }
    }

    pub fn is_infinite(&self) -> bool {
        self.duration == INFINITE
    }

    fn is_shorter_than(&self, other: &ActiveEffect) -> bool {
        !self.is_infinite() && (other.is_infinite() || self.duration < other.duration)
    }

    /// Merges a new instance of the same effect in, returning whether the
    /// visible instance changed.
    fn update(&mut self, other: ActiveEffect) -> bool {
        let mut changed = false;
        if other.amplifier > self.amplifier {
            if other.is_shorter_than(self) {
                let previous_hidden = self.hidden.take();
                let mut copy = self.clone();
                copy.hidden = previous_hidden;
                self.hidden = Some(Box::new(copy));
            }
            self.amplifier = other.amplifier;
            self.duration = other.duration;
            changed = true;
        } else if self.is_shorter_than(&other) {
            if other.amplifier == self.amplifier {
                self.duration = other.duration;
                changed = true;
            } else {
                match &mut self.hidden {
                    None => self.hidden = Some(Box::new(other.clone())),
                    Some(hidden) => {
                        hidden.update(other.clone());
                    }
                }
            }
        }
        if (!other.ambient && self.ambient) || changed {
            self.ambient = other.ambient;
            changed = true;
        }
        if other.show_particles != self.show_particles {
            self.show_particles = other.show_particles;
            changed = true;
        }
        if other.show_icon != self.show_icon {
            self.show_icon = other.show_icon;
            changed = true;
        }
        changed
    }

    fn tick_down(&mut self) {
        if !self.is_infinite() {
            self.duration -= 1;
        }
        if let Some(hidden) = &mut self.hidden {
            hidden.tick_down();
        }
    }

    /// Swaps in the hidden effect once this one runs out; true if it did.
    fn downgrade(&mut self) -> bool {
        if self.duration == 0
            && let Some(hidden) = self.hidden.take()
        {
            let hidden = *hidden;
            self.amplifier = hidden.amplifier;
            self.duration = hidden.duration;
            self.ambient = hidden.ambient;
            self.show_particles = hidden.show_particles;
            self.show_icon = hidden.show_icon;
            self.hidden = hidden.hidden;
            return true;
        }
        false
    }

    fn has_remaining(&self) -> bool {
        self.is_infinite() || self.duration > 0
    }

    /// Flags as the Update Mob Effect packet carries them.
    pub fn flags(&self) -> u8 {
        (self.ambient as u8) | (self.show_particles as u8) << 1 | (self.show_icon as u8) << 2
    }
}

/// A periodic action an effect asks its entity to perform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EffectAction {
    Heal(f32),
    /// Poison: magic damage that never kills.
    Poison(f32),
    MagicDamage(f32),
    WitherDamage(f32),
    Exhaust(f32),
    /// Saturation: `eat(amplifier + 1, 1.0)`.
    Feed {
        nutrition: i32,
        saturation: f32,
    },
}

/// What changed during a tick or update, for the entity to broadcast.
#[derive(Debug, Clone, PartialEq)]
pub enum EffectChange {
    Added(ActiveEffect),
    Updated(ActiveEffect),
    Removed(MobEffect),
}

/// Instant effects act once when applied instead of ticking.
fn instantaneous(effect: MobEffect) -> bool {
    effect.is_instantaneous()
}

fn interval_action(effect: MobEffect, tick: i64, amplifier: u8) -> Option<EffectAction> {
    let every = |interval: i64| {
        let interval = interval >> amplifier;
        interval <= 0 || tick % interval == 0
    };
    match effect {
        MobEffect::Regeneration if every(50) => Some(EffectAction::Heal(1.0)),
        MobEffect::Poison if every(25) => Some(EffectAction::Poison(1.0)),
        MobEffect::Wither if every(40) => Some(EffectAction::WitherDamage(1.0)),
        MobEffect::Hunger => Some(EffectAction::Exhaust(0.005 * (amplifier as f32 + 1.0))),
        _ => None,
    }
}

/// The instant action of applying an effect, scaled like a splash potion
/// would (`scale` is 1 for eating and drinking).
pub fn instant_action(effect: MobEffect, amplifier: u8, scale: f64) -> Option<EffectAction> {
    match effect {
        MobEffect::InstantHealth => Some(EffectAction::Heal(
            (scale * f64::from(4 << amplifier) + 0.5) as i32 as f32,
        )),
        MobEffect::InstantDamage => Some(EffectAction::MagicDamage(
            (scale * f64::from(6 << amplifier) + 0.5) as i32 as f32,
        )),
        MobEffect::Saturation => Some(EffectAction::Feed {
            nutrition: amplifier as i32 + 1,
            saturation: (amplifier as f32 + 1.0) * 1.0 * 2.0,
        }),
        _ => None,
    }
}

impl ActiveEffect {
    fn from_details(effect: MobEffect, details: &EffectDetails) -> Self {
        Self {
            effect,
            amplifier: details.amplifier,
            duration: i64::from(details.duration).max(INFINITE),
            ambient: details.ambient,
            show_particles: details.show_particles,
            show_icon: details.show_icon,
            hidden: details
                .hidden_effect
                .as_ref()
                .map(|hidden| Box::new(Self::from_details(effect, hidden))),
        }
    }

    /// This effect with its duration scaled, as lingering potions and
    /// tipped arrows apply it: at least one tick, infinite stays infinite.
    pub fn scaled(mut self, scale: f32) -> Self {
        if !self.is_infinite() && self.duration != 0 {
            self.duration = ((self.duration as f32 * scale).floor() as i64).max(1);
        }
        self
    }

    /// The particle an entity with this effect shows: a particle type id
    /// and its encoded options.
    pub fn particle(&self) -> (i32, Vec<u8>) {
        let info = self.effect.info();
        match info.particle {
            Some(particle) => (particle.id() as i32, Vec::new()),
            None => {
                let alpha = if self.ambient { AMBIENT_ALPHA } else { u8::MAX };
                let argb = (u32::from(alpha) << 24) | (info.color as u32 & 0xFF_FFFF);
                (
                    ParticleType::EntityEffect.id() as i32,
                    argb.to_be_bytes().to_vec(),
                )
            }
        }
    }
}

/// Every effect of a potion, the potion's own first, then the custom ones,
/// with durations scaled by `scale`.
pub fn potion_effects(contents: &PotionContents, scale: f32) -> Vec<ActiveEffect> {
    let base = contents
        .potion
        .map_or(&[][..], |potion| potion.effects())
        .iter()
        .map(ActiveEffect::new);
    let custom = contents
        .custom_effects
        .iter()
        .map(|custom| ActiveEffect::from_details(custom.effect, &custom.details));
    base.chain(custom)
        .map(|effect| effect.scaled(scale))
        .collect()
}

/// The color potion particles and clouds take when the potion sets none.
pub const BASE_POTION_COLOR: i32 = -13083194;

/// A potion's color: its custom color, else its visible effects' colors
/// mixed by strength.
pub fn potion_color(contents: &PotionContents) -> i32 {
    if let Some(color) = contents.custom_color {
        return color;
    }
    let (mut red, mut green, mut blue, mut weight) = (0, 0, 0, 0);
    for effect in potion_effects(contents, 1.0) {
        if !effect.show_particles {
            continue;
        }
        let color = effect.effect.info().color;
        let strength = i32::from(effect.amplifier) + 1;
        red += strength * ((color >> 16) & 0xFF);
        green += strength * ((color >> 8) & 0xFF);
        blue += strength * (color & 0xFF);
        weight += strength;
    }
    if weight == 0 {
        return BASE_POTION_COLOR;
    }
    (0xFF << 24) | ((red / weight) << 16) | ((green / weight) << 8) | (blue / weight)
}

/// Whether any of a potion's effects acts instantly.
pub fn has_instant_effects(contents: &PotionContents) -> bool {
    potion_effects(contents, 1.0)
        .iter()
        .any(|effect| effect.effect.is_instantaneous())
}

/// The absorption hearts an active absorption effect grants.
pub fn max_absorption(amplifier: u8) -> f32 {
    4.0 * (1.0 + amplifier as f32)
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ActiveEffects {
    effects: BTreeMap<MobEffect, ActiveEffect>,
}

impl ActiveEffects {
    pub fn get(&self, effect: MobEffect) -> Option<&ActiveEffect> {
        self.effects.get(&effect)
    }

    pub fn has(&self, effect: MobEffect) -> bool {
        self.effects.contains_key(&effect)
    }

    pub fn amplifier(&self, effect: MobEffect) -> Option<u8> {
        self.effects.get(&effect).map(|e| e.amplifier)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ActiveEffect> {
        self.effects.values()
    }

    pub fn to_nbt(&self) -> Tag {
        Tag::from(self.iter().map(ActiveEffect::to_nbt).collect::<Vec<_>>())
    }

    /// Reads an `active_effects` list, skipping unknown or finished
    /// effects.
    pub fn from_nbt(list: &[Tag]) -> Self {
        let effects = list
            .iter()
            .filter_map(Tag::as_compound)
            .filter_map(ActiveEffect::from_nbt)
            .filter(|effect| !instantaneous(effect.effect) && effect.has_remaining())
            .map(|effect| (effect.effect, effect))
            .collect();
        Self { effects }
    }

    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }

    /// The particles of the visible effects and whether all of those are
    /// ambient, as living entity data carries them.
    pub fn particles(&self) -> (Vec<(i32, Vec<u8>)>, bool) {
        let visible = self.effects.values().filter(|effect| effect.show_particles);
        let particles = visible.clone().map(ActiveEffect::particle).collect();
        let ambient = !self.effects.is_empty() && visible.clone().all(|effect| effect.ambient);
        (particles, ambient)
    }

    /// Adds or merges an effect. Instant effects are not stored; the caller
    /// applies [`instant_action`] for them. Returns the change to announce.
    pub fn add(&mut self, effect: ActiveEffect) -> Option<EffectChange> {
        if instantaneous(effect.effect) || !effect.has_remaining() {
            return None;
        }
        match self.effects.get_mut(&effect.effect) {
            None => {
                self.effects.insert(effect.effect, effect.clone());
                Some(EffectChange::Added(effect))
            }
            Some(existing) => existing
                .update(effect)
                .then(|| EffectChange::Updated(existing.clone())),
        }
    }

    pub fn remove(&mut self, effect: MobEffect) -> Option<EffectChange> {
        self.effects
            .remove(&effect)
            .map(|_| EffectChange::Removed(effect))
    }

    pub fn clear(&mut self) -> Vec<EffectChange> {
        let removed: Vec<EffectChange> = self
            .effects
            .keys()
            .map(|effect| EffectChange::Removed(*effect))
            .collect();
        self.effects.clear();
        removed
    }

    /// One game tick. `entity_tick` is the entity's age, which infinite
    /// effects use as their interval clock.
    pub fn tick(
        &mut self,
        entity_tick: i64,
        absorption: f32,
    ) -> (Vec<EffectAction>, Vec<EffectChange>) {
        let mut actions = Vec::new();
        let mut changes = Vec::new();
        let mut expired = Vec::new();
        for (kind, effect) in &mut self.effects {
            let tick = if effect.is_infinite() {
                entity_tick
            } else {
                effect.duration
            };
            if *kind == MobEffect::Absorption && absorption <= 0.0 {
                expired.push(*kind);
                continue;
            }
            if let Some(action) = interval_action(*kind, tick, effect.amplifier) {
                actions.push(action);
            }
            effect.tick_down();
            if effect.downgrade() {
                changes.push(EffectChange::Updated(effect.clone()));
            }
            if !effect.has_remaining() {
                expired.push(*kind);
            } else if effect.duration % RESYNC_INTERVAL == 0 {
                changes.push(EffectChange::Updated(effect.clone()));
            }
        }
        for kind in expired {
            self.effects.remove(&kind);
            changes.push(EffectChange::Removed(kind));
        }
        (actions, changes)
    }
}

/// What a potion does to one player: effects to add, instant effects with
/// their strength, and whether it puts the player's fire out.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PotionHit {
    pub effects: Vec<ActiveEffect>,
    /// Instant effects other than harming (which arrives as a hurt), with
    /// the amplifier and scale they act with.
    pub instant: Vec<(MobEffect, u8, f64)>,
    pub extinguish: bool,
}

impl PotionHit {
    pub fn is_empty(&self) -> bool {
        self.effects.is_empty() && self.instant.is_empty() && !self.extinguish
    }
}

/// A harming or healing effect's damage on one target, if it hurts it.
fn instant_harm(effect: MobEffect, amplifier: u8, scale: f64, inverted: bool) -> Option<f32> {
    let harm = match effect {
        MobEffect::InstantDamage => !inverted,
        MobEffect::InstantHealth => inverted,
        _ => return None,
    };
    harm.then(|| (scale * f64::from(6 << amplifier) + 0.5) as i32 as f32)
}

/// Queues what `effects` (non-instant ones already sized) and `instant`
/// ones at `scale` do to a target: the damage as a hurt, the rest as a
/// potion hit for players.
#[allow(clippy::too_many_arguments)]
pub fn apply_potion(
    target: i32,
    player: bool,
    kind: tuff_vanilla_data::registry::EntityType,
    effects: &[ActiveEffect],
    instant: &[(MobEffect, u8)],
    scale: f64,
    extinguish: bool,
    culprit: Option<crate::world::entity::Culprit>,
    cx: &mut crate::world::entity::TickContext,
) {
    let inverted = !player
        && tuff_datapack::tags::is(
            tuff_vanilla_data::tag::entity_type::key::INVERTED_HEALING_AND_HARM,
            kind,
        );
    let mut hit = PotionHit {
        extinguish,
        ..PotionHit::default()
    };
    for &(effect, amplifier) in instant {
        if let Some(damage) = instant_harm(effect, amplifier, scale, inverted) {
            let mut hurt = crate::world::entity::EntityHit::new(
                damage,
                &tuff_vanilla_data::damage_type::INDIRECT_MAGIC,
            );
            hurt.attacker = culprit.clone();
            cx.effects.push(crate::world::entity::Effect::Hurt {
                target,
                hit: Box::new(hurt),
            });
        } else if player {
            hit.instant.push((effect, amplifier, scale));
        }
    }
    if !player && !effects.is_empty() {
        cx.effects.push(crate::world::entity::Effect::MobEffects {
            target,
            effects: effects.to_vec(),
        });
    }
    if player {
        hit.effects = effects.to_vec();
        if !hit.is_empty() {
            cx.effects.push(crate::world::entity::Effect::Potion {
                target,
                hit: Box::new(hit),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn effects_round_trip_through_saved_nbt_with_hidden_copies() {
        let mut effects = ActiveEffects::default();
        effects.add(ActiveEffect::timed(MobEffect::NightVision, INFINITE, 0));
        effects.add(ActiveEffect::timed(MobEffect::Speed, 400, 0));
        effects.add(ActiveEffect::timed(MobEffect::Speed, 100, 2));
        let Tag::List(list) = effects.to_nbt() else {
            panic!("not a list");
        };
        let speed = list
            .iter()
            .filter_map(Tag::as_compound)
            .find(|c| c.get_str("id") == Some("minecraft:speed"))
            .unwrap();
        assert_eq!(speed.get_byte("amplifier"), Some(2));
        let hidden = speed.get_compound("hidden_effect").unwrap();
        assert_eq!(hidden.get_int("duration"), Some(400));
        assert_eq!(ActiveEffects::from_nbt(&list), effects);
    }

    #[tuff_core::watchdog]
    #[test]
    fn loading_effects_skips_unknown_instant_and_expired_entries() {
        let list = vec![
            Tag::from(Compound::new().with("id", "minecraft:not_an_effect")),
            Tag::from(
                Compound::new()
                    .with("id", "minecraft:instant_health")
                    .with("duration", 1),
            ),
            Tag::from(
                Compound::new()
                    .with("id", "minecraft:speed")
                    .with("duration", 0),
            ),
            Tag::from(
                Compound::new()
                    .with("id", "night_vision")
                    .with("duration", 20),
            ),
        ];
        let effects = ActiveEffects::from_nbt(&list);
        assert_eq!(
            effects.iter().map(|e| e.effect).collect::<Vec<_>>(),
            vec![MobEffect::NightVision]
        );
        let night_vision = effects.get(MobEffect::NightVision).unwrap();
        assert!(night_vision.show_particles && night_vision.show_icon);
    }

    #[tuff_core::watchdog]
    #[test]
    fn stronger_effects_take_over_and_weaker_ones_resume() {
        let mut effects = ActiveEffects::default();
        let added = effects.add(ActiveEffect::timed(MobEffect::Regeneration, 100, 0));
        assert!(matches!(added, Some(EffectChange::Added(_))));
        let updated = effects.add(ActiveEffect::timed(MobEffect::Regeneration, 10, 1));
        assert!(matches!(updated, Some(EffectChange::Updated(e)) if e.amplifier == 1));
        assert_eq!(
            effects.add(ActiveEffect::timed(MobEffect::Regeneration, 5, 0)),
            None
        );
        let mut ticks = 0;
        loop {
            let (_, changes) = effects.tick(ticks, 0.0);
            ticks += 1;
            if changes
                .iter()
                .any(|c| matches!(c, EffectChange::Updated(e) if e.amplifier == 0))
            {
                break;
            }
            assert!(ticks < 20, "hidden effect never resumed");
        }
        let regen = effects.get(MobEffect::Regeneration).unwrap();
        assert_eq!(regen.amplifier, 0);
        assert_eq!(regen.duration, 100 - ticks);
    }

    #[tuff_core::watchdog]
    #[test]
    fn effects_expire_and_announce_removal() {
        let mut effects = ActiveEffects::default();
        effects.add(ActiveEffect::timed(MobEffect::Poison, 2, 0));
        assert!(effects.tick(0, 0.0).1.is_empty());
        let (_, changes) = effects.tick(1, 0.0);
        assert_eq!(changes, vec![EffectChange::Removed(MobEffect::Poison)]);
        assert!(!effects.has(MobEffect::Poison));
    }

    #[tuff_core::watchdog]
    #[test]
    fn periodic_actions_follow_vanilla_intervals() {
        let mut effects = ActiveEffects::default();
        effects.add(ActiveEffect::timed(MobEffect::Regeneration, 200, 0));
        effects.add(ActiveEffect::timed(MobEffect::Poison, 200, 1));
        effects.add(ActiveEffect::timed(MobEffect::Hunger, 200, 0));
        let mut heals = 0;
        let mut poisons = 0;
        let mut hunger = 0;
        for tick in 0..100 {
            for action in effects.tick(tick, 0.0).0 {
                match action {
                    EffectAction::Heal(_) => heals += 1,
                    EffectAction::Poison(_) => poisons += 1,
                    EffectAction::Exhaust(_) => hunger += 1,
                    _ => {}
                }
            }
        }
        assert_eq!(heals, 2);
        assert_eq!(poisons, 100 / 12);
        assert_eq!(hunger, 100);
    }

    #[tuff_core::watchdog]
    #[test]
    fn instant_effects_act_once_and_are_not_stored() {
        let mut effects = ActiveEffects::default();
        assert_eq!(
            effects.add(ActiveEffect::timed(MobEffect::InstantHealth, 1, 0)),
            None
        );
        assert_eq!(
            instant_action(MobEffect::InstantHealth, 1, 1.0),
            Some(EffectAction::Heal(8.0))
        );
        assert_eq!(
            instant_action(MobEffect::InstantDamage, 0, 0.5),
            Some(EffectAction::MagicDamage(3.0))
        );
        assert_eq!(
            instant_action(MobEffect::Saturation, 0, 1.0),
            Some(EffectAction::Feed {
                nutrition: 1,
                saturation: 2.0
            })
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn absorption_ends_once_the_hearts_are_gone() {
        let mut effects = ActiveEffects::default();
        effects.add(ActiveEffect::timed(MobEffect::Absorption, 2400, 0));
        assert!(effects.tick(0, 4.0).1.is_empty());
        let (_, changes) = effects.tick(1, 0.0);
        assert_eq!(changes, vec![EffectChange::Removed(MobEffect::Absorption)]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn potions_mix_their_colors_and_scale_their_durations() {
        use tuff_vanilla_data::registry::Potion;
        let fire = PotionContents {
            potion: Some(Potion::FireResistance),
            ..Default::default()
        };
        let effects = potion_effects(&fire, 0.25);
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].effect, MobEffect::FireResistance);
        assert_eq!(effects[0].duration, 3600 / 4);
        assert_eq!(potion_color(&fire), 0xFF_FF_99_00_u32 as i32);
        assert_eq!(
            potion_color(&PotionContents {
                potion: Some(Potion::Water),
                ..Default::default()
            }),
            BASE_POTION_COLOR
        );
        assert!(has_instant_effects(&PotionContents {
            potion: Some(Potion::Harming),
            ..Default::default()
        }));
        let mut active = ActiveEffects::default();
        assert_eq!(active.particles(), (Vec::new(), false));
        active.add(ActiveEffect::timed(MobEffect::Speed, 100, 0));
        let (particles, ambient) = active.particles();
        assert!(!ambient);
        assert_eq!(
            particles,
            vec![(
                ParticleType::EntityEffect.id() as i32,
                vec![0xFF, 0x33, 0xEB, 0xFF]
            )]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn effect_particle_entity_data_matches_the_living_layout() {
        use crate::protocol::play::{MetadataValue, metadata};
        use tuff_vanilla_data::registry::EntityType;
        let player = EntityType::Player;
        assert_eq!(
            player.data_serializer(metadata::EFFECT_PARTICLES),
            Some(MetadataValue::Particles(Vec::new()).type_id() as u8)
        );
        assert_eq!(
            player.data_serializer(metadata::EFFECT_AMBIENCE),
            Some(MetadataValue::Boolean(false).type_id() as u8)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn packet_flags_pack_ambient_particles_and_icon() {
        let mut effect = ActiveEffect::timed(MobEffect::Speed, 10, 0);
        assert_eq!(effect.flags(), 0b110);
        effect.ambient = true;
        effect.show_icon = false;
        assert_eq!(effect.flags(), 0b011);
    }
}
