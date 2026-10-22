//! Enchantment requirements and entity effects, run from their data: the
//! requirements go through the full loot-condition evaluator, and each
//! effect tree becomes a list of [`Action`]s. Actions on the affected
//! entity are applied by whoever owns it (a player session or a region
//! entity); [`WorldAction`]s run on the region through [`apply_world`].

use std::cell::{RefCell, RefMut};
use std::collections::BTreeSet;

use rand::{Rng, RngCore};
use serde_json::Value;
use tuff_core::world::predicate::{
    self, Context, DamageFacts, EntityFacts, LocationFacts, PredicateWorld,
};
use tuff_core::world::region_api::RegionWorld;
use tuff_item::enchantment::{
    self, AttributeEffect, Conditional, EnchantmentEffects, EntityEffect, HolderSet, LevelValue,
    PositionSource, Requirement, SoundRef, VelocitySource,
};
use tuff_item::item::ItemStack;
use tuff_vanilla_data::damage_type::{self, DamageType};
use tuff_vanilla_data::item::{EquipmentSlot, ModifierOperation};
use tuff_vanilla_data::registry::{Attribute, EntityType, MobEffect, ParticleType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::BlockPos;

use tuff_core::world::attributes::applies_in;

/// What a requirement can read beyond the entity it is about: a random
/// source, the other entities of a damage context, the damage, whether the
/// effect was active, and optionally the world around.
pub struct EffectWorld<'a> {
    rng: RefCell<&'a mut dyn RngCore>,
    pub attacker: Option<&'a EntityFacts>,
    pub direct_attacker: Option<&'a EntityFacts>,
    pub damage: Option<&'a DamageFacts>,
    pub active: Option<bool>,
    pub world: Option<&'a dyn PredicateWorld>,
}

impl<'a> EffectWorld<'a> {
    pub fn new(rng: &'a mut dyn RngCore) -> Self {
        Self {
            rng: RefCell::new(rng),
            attacker: None,
            direct_attacker: None,
            damage: None,
            active: None,
            world: None,
        }
    }

    pub fn with_world(mut self, world: &'a dyn PredicateWorld) -> Self {
        self.world = Some(world);
        self
    }

    pub fn rng(&self) -> RefMut<'_, &'a mut dyn RngCore> {
        self.rng.borrow_mut()
    }
}

impl PredicateWorld for EffectWorld<'_> {
    fn location(&self, position: [f64; 3]) -> LocationFacts {
        self.world.map_or_else(
            || LocationFacts {
                position,
                ..LocationFacts::default()
            },
            |world| world.location(position),
        )
    }
    fn raining(&self) -> bool {
        self.world.is_some_and(PredicateWorld::raining)
    }
    fn thundering(&self) -> bool {
        self.world.is_some_and(PredicateWorld::thundering)
    }
    fn clock(&self, id: &str) -> Option<i64> {
        self.world?.clock(id)
    }
    fn score(&self, holder: &str, objective: &str) -> Option<i32> {
        self.world?.score(holder, objective)
    }
    fn team(&self, holder: &str) -> Option<&str> {
        self.world?.team(holder)
    }
    fn random(&self) -> f32 {
        self.rng.borrow_mut().r#gen()
    }
    fn random_int(&self, min: i64, max: i64) -> i64 {
        if max <= min {
            min
        } else {
            self.rng.borrow_mut().gen_range(min..=max)
        }
    }
    fn attribute(&self, key: &str, position: [f64; 3]) -> Option<Value> {
        self.world?.attribute(key, position)
    }
    fn selected(&self, selector: &str) -> Option<&EntityFacts> {
        match selector {
            "attacker" => self.attacker,
            "attacking_player" => self.attacker.filter(|a| a.kind == Some(EntityType::Player)),
            "direct_attacker" => self.direct_attacker,
            _ => None,
        }
    }
    fn damage(&self) -> Option<&DamageFacts> {
        self.damage
    }
    fn enchantment_active(&self) -> Option<bool> {
        self.active
    }
}

/// Whether `requirement` holds for an effect of an enchantment at `level`
/// on `tool`, about `this` at `origin`.
pub fn holds(
    requirement: Option<&Requirement>,
    this: Option<&EntityFacts>,
    origin: [f64; 3],
    tool: Option<&ItemStack>,
    level: u32,
    world: &EffectWorld<'_>,
) -> bool {
    requirement.is_none_or(|requirement| {
        let context = Context {
            this,
            target: None,
            origin,
            location: None,
            tool,
            enchantment_level: Some(level as i32),
        };
        predicate::condition_in(&requirement.condition, &context, Some(world))
    })
}

/// Something an effect does to the entity it affects.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Ignite(f32),
    Damage {
        kind: &'static DamageType,
        amount: f32,
    },
    /// Wears the enchanted item.
    ItemDamage(u32),
    MobEffect {
        effect: MobEffect,
        amplifier: u8,
        ticks: i32,
    },
    /// A push of `magnitude` along `direction` in the entity's look frame,
    /// scaled per axis by `scale`.
    Impulse {
        direction: [f64; 3],
        scale: [f64; 3],
        magnitude: f64,
    },
    Exhaustion(f32),
    World(WorldAction),
}

/// Something an effect does to the world at the entity.
#[derive(Debug, Clone, PartialEq)]
pub enum WorldAction {
    Sound {
        sound: SoundEvent,
        volume: f32,
        pitch: f32,
    },
    Particles {
        particle: ParticleType,
        options: Vec<u8>,
        position: [f64; 3],
        velocity: [f64; 3],
        speed: f32,
    },
    ReplaceBlock {
        offset: [i32; 3],
        predicate: Option<Value>,
        state: Value,
        game_event: Option<String>,
    },
    ReplaceDisk {
        radius: i32,
        height: i32,
        offset: [i32; 3],
        predicate: Option<Value>,
        state: Value,
        game_event: Option<String>,
    },
    SetBlockProperties {
        offset: [i32; 3],
        properties: Value,
        game_event: Option<String>,
    },
    Summon {
        entity: String,
    },
    Function(String),
    Explode {
        radius: f32,
        knockback: Option<f32>,
        offset: [f64; 3],
        fire: bool,
        damage_type: Option<String>,
        block_interaction: String,
        attribute_to_user: bool,
    },
}

/// The entity an effect runs on, as far as planning needs it.
#[derive(Debug, Clone, Copy)]
pub struct Body {
    pub kind: EntityType,
    pub position: [f64; 3],
    /// Its movement over the last tick.
    pub movement: [f64; 3],
}

/// The actions of `effect` at `level` on `body`.
pub fn actions(effect: &EntityEffect, level: u32, body: &Body, rng: &mut impl Rng) -> Vec<Action> {
    let mut out = Vec::new();
    plan(effect, level, body, rng, &mut out);
    out
}

fn plan(effect: &EntityEffect, level: u32, body: &Body, rng: &mut impl Rng, out: &mut Vec<Action>) {
    let at = |value: &LevelValue| value.at(level);
    match effect {
        EntityEffect::AllOf(effects) => {
            for effect in effects {
                plan(effect, level, body, rng, out);
            }
        }
        EntityEffect::ApplyMobEffect {
            to_apply,
            min_duration,
            max_duration,
            min_amplifier,
            max_amplifier,
        } => {
            if let Some(effect) = pick(to_apply, rng).and_then(|id| MobEffect::from_name(&id)) {
                let ticks = (between(rng, at(min_duration), at(max_duration)) * 20.0).round();
                let amplifier = between(rng, at(min_amplifier), at(max_amplifier)).round();
                out.push(Action::MobEffect {
                    effect,
                    amplifier: amplifier.clamp(0.0, 255.0) as u8,
                    ticks: ticks as i32,
                });
            }
        }
        EntityEffect::Attribute(_) => {}
        EntityEffect::ChangeItemDamage(amount) => {
            out.push(Action::ItemDamage(at(amount).max(0.0) as u32))
        }
        EntityEffect::DamageEntity {
            damage_type,
            min_damage,
            max_damage,
        } => {
            if let Some(kind) = damage_type_named(damage_type) {
                out.push(Action::Damage {
                    kind,
                    amount: between(rng, at(min_damage), at(max_damage)),
                });
            }
        }
        EntityEffect::Explode {
            attribute_to_user,
            damage_type,
            knockback_multiplier,
            offset,
            radius,
            create_fire,
            block_interaction,
            ..
        } => out.push(Action::World(WorldAction::Explode {
            radius: at(radius),
            knockback: knockback_multiplier.as_ref().map(at),
            offset: *offset,
            fire: *create_fire,
            damage_type: damage_type.clone(),
            block_interaction: block_interaction.clone(),
            attribute_to_user: *attribute_to_user,
        })),
        EntityEffect::Ignite(seconds) => out.push(Action::Ignite(at(seconds))),
        EntityEffect::ApplyImpulse {
            direction,
            coordinate_scale,
            magnitude,
        } => out.push(Action::Impulse {
            direction: *direction,
            scale: *coordinate_scale,
            magnitude: f64::from(at(magnitude)),
        }),
        EntityEffect::ApplyExhaustion(amount) => out.push(Action::Exhaustion(at(amount))),
        EntityEffect::PlaySound {
            sounds,
            volume,
            pitch,
        } => {
            let index = (level as usize)
                .saturating_sub(1)
                .min(sounds.len().saturating_sub(1));
            if let Some(sound) = sounds.get(index).and_then(sound_event) {
                out.push(Action::World(WorldAction::Sound {
                    sound,
                    volume: volume.sample(rng),
                    pitch: pitch.sample(rng),
                }));
            }
        }
        EntityEffect::ReplaceBlock {
            offset,
            predicate,
            block_state,
            trigger_game_event,
        } => out.push(Action::World(WorldAction::ReplaceBlock {
            offset: *offset,
            predicate: predicate.clone(),
            state: block_state.clone(),
            game_event: trigger_game_event.clone(),
        })),
        EntityEffect::ReplaceDisk {
            radius,
            height,
            offset,
            predicate,
            block_state,
            trigger_game_event,
        } => out.push(Action::World(WorldAction::ReplaceDisk {
            radius: at(radius) as i32,
            height: at(height) as i32,
            offset: *offset,
            predicate: predicate.clone(),
            state: block_state.clone(),
            game_event: trigger_game_event.clone(),
        })),
        EntityEffect::RunFunction(function) => {
            out.push(Action::World(WorldAction::Function(function.clone())))
        }
        EntityEffect::SetBlockProperties {
            properties,
            offset,
            trigger_game_event,
        } => out.push(Action::World(WorldAction::SetBlockProperties {
            offset: *offset,
            properties: properties.clone(),
            game_event: trigger_game_event.clone(),
        })),
        EntityEffect::SpawnParticles {
            particle,
            horizontal_position,
            vertical_position,
            horizontal_velocity,
            vertical_velocity,
            speed,
        } => {
            if let Some((particle, options)) = particle_options(particle) {
                let size = body.kind.size();
                let (width, height) = (size.width, size.height);
                let [x, y, z] = body.position;
                let position = [
                    coordinate(horizontal_position, x, x, width, rng),
                    coordinate(
                        vertical_position,
                        y,
                        y + f64::from(height / 2.0),
                        height,
                        rng,
                    ),
                    coordinate(horizontal_position, z, z, width, rng),
                ];
                let velocity = [
                    velocity_of(horizontal_velocity, body.movement[0], rng),
                    velocity_of(vertical_velocity, body.movement[1], rng),
                    velocity_of(horizontal_velocity, body.movement[2], rng),
                ];
                out.push(Action::World(WorldAction::Particles {
                    particle,
                    options,
                    position,
                    velocity,
                    speed: speed.sample(rng),
                }));
            }
        }
        EntityEffect::SummonEntity { entity, .. } => {
            if let Some(entity) = pick(entity, rng) {
                out.push(Action::World(WorldAction::Summon { entity }));
            }
        }
    }
}

fn coordinate(
    source: &PositionSource,
    position: f64,
    center: f64,
    span: f32,
    rng: &mut impl Rng,
) -> f64 {
    let base = match source.kind.as_str() {
        "in_bounding_box" => center + (rng.r#gen::<f64>() - 0.5) * f64::from(span * source.scale),
        _ => position,
    };
    base + f64::from(source.offset)
}

fn velocity_of(source: &VelocitySource, movement: f64, rng: &mut impl Rng) -> f64 {
    movement * f64::from(source.movement_scale) + f64::from(source.base.sample(rng))
}

fn particle_options(particle: &Value) -> Option<(ParticleType, Vec<u8>)> {
    let (name, data) = match particle {
        Value::String(name) => (name.as_str(), tuff_nbt::Compound::new()),
        Value::Object(map) => {
            let name = map.get("type")?.as_str()?;
            let mut rest = map.clone();
            rest.remove("type");
            let data = match tuff_item::registries::codecs::json_to_nbt(&Value::Object(rest)) {
                Some(tuff_nbt::Tag::Compound(data)) => data,
                _ => return None,
            };
            (name, data)
        }
        _ => return None,
    };
    let kind = ParticleType::from_name(name)?;
    let options = tuff_core::command::particle::encode_options(kind, &data).ok()?;
    Some((kind, options))
}

fn between(rng: &mut impl Rng, min: f32, max: f32) -> f32 {
    if min >= max {
        min
    } else {
        rng.r#gen::<f32>() * (max - min) + min
    }
}

fn pick(set: &HolderSet, rng: &mut impl Rng) -> Option<String> {
    let ids: Vec<String> = match set {
        HolderSet::Ids(ids) => ids.clone(),
        HolderSet::Tag(tag) => tuff_datapack::tags::with(|tags| {
            ["minecraft:mob_effect", "minecraft:entity_type"]
                .into_iter()
                .find_map(|registry| tags.get(registry, tag).map(<[String]>::to_vec))
                .unwrap_or_default()
        }),
    };
    match ids.len() {
        0 => None,
        n => Some(ids[rng.gen_range(0..n)].clone()),
    }
}

fn damage_type_named(name: &str) -> Option<&'static DamageType> {
    let name = qualify(name);
    damage_type::ALL.iter().find(|kind| kind.name == name)
}

fn sound_event(sound: &SoundRef) -> Option<SoundEvent> {
    SoundEvent::from_name(&sound.id)
}

/// One enchantment effect that fired: the enchanted item's slot and what
/// it does.
#[derive(Debug, Clone, PartialEq)]
pub struct Fired {
    pub slot: EquipmentSlot,
    pub actions: Vec<Action>,
}

/// The `component` effects of the enchanted `equipment` of the entity
/// `this` whose requirements hold.
pub fn run(
    equipment: &[(EquipmentSlot, &ItemStack)],
    component: fn(&EnchantmentEffects) -> &[Conditional<EntityEffect>],
    this: &EntityFacts,
    body: &Body,
    world: &EffectWorld<'_>,
) -> Vec<Fired> {
    let registry = enchantment::current();
    let mut fired = Vec::new();
    for (slot, stack) in equipment {
        for (_, def, level) in
            enchantment::leveled(&registry, stack.components.enchantments.as_ref())
        {
            if !applies_in(def, *slot) {
                continue;
            }
            for entry in component(&def.effects) {
                if holds(
                    entry.requirements.as_ref(),
                    Some(this),
                    this.position,
                    Some(stack),
                    level,
                    world,
                ) {
                    let actions = actions(&entry.effect, level, body, &mut *world.rng());
                    fired.push(Fired {
                        slot: *slot,
                        actions,
                    });
                }
            }
        }
    }
    fired
}

/// The `component` effects of `weapon` on a projectile or the entity it
/// was shot or thrown from; the weapon is held, not worn, so slots do not
/// apply.
pub fn run_weapon(
    weapon: &ItemStack,
    component: fn(&EnchantmentEffects) -> &[Conditional<EntityEffect>],
    this: &EntityFacts,
    origin: [f64; 3],
    body: &Body,
    world: &EffectWorld<'_>,
) -> Vec<Action> {
    let registry = enchantment::current();
    let mut out = Vec::new();
    for (_, def, level) in enchantment::leveled(&registry, weapon.components.enchantments.as_ref())
    {
        for entry in component(&def.effects) {
            if holds(
                entry.requirements.as_ref(),
                Some(this),
                origin,
                Some(weapon),
                level,
                world,
            ) {
                out.extend(actions(&entry.effect, level, body, &mut *world.rng()));
            }
        }
    }
    out
}

/// A `location_changed` effect that is active: the slot it came from, the
/// enchantment and the effect's index in it.
pub type ActiveKey = (&'static str, String, usize);

/// What a block change does with the `location_changed` effects.
#[derive(Debug, Default)]
pub struct LocationChange {
    /// Effects whose requirements hold now, in order.
    pub fired: Vec<Fired>,
    /// Every attribute modifier the active effects give (id, amount,
    /// operation), to replace the ones they gave before.
    pub modifiers: Vec<(Attribute, String, f64, ModifierOperation)>,
    pub active: BTreeSet<ActiveKey>,
}

/// Runs the `location_changed` effects of `equipment` for `this`, which
/// moved to a new block (or changed equipment): an effect whose
/// requirements hold, given whether it was active before, is active now
/// and fires; one that stops holding is deactivated.
pub fn location_changed(
    equipment: &[(EquipmentSlot, &ItemStack)],
    this: &EntityFacts,
    body: &Body,
    was_active: &BTreeSet<ActiveKey>,
    world: &mut EffectWorld<'_>,
) -> LocationChange {
    let registry = enchantment::current();
    let mut change = LocationChange::default();
    for (slot, stack) in equipment {
        for (id, def, level) in
            enchantment::leveled(&registry, stack.components.enchantments.as_ref())
        {
            if !applies_in(def, *slot) {
                continue;
            }
            for (index, entry) in def.effects.location_changed.iter().enumerate() {
                let key = (slot_name(*slot), enchantment::normalize(id), index);
                world.active = Some(was_active.contains(&key));
                if !holds(
                    entry.requirements.as_ref(),
                    Some(this),
                    this.position,
                    Some(stack),
                    level,
                    world,
                ) {
                    continue;
                }
                attribute_modifiers(&entry.effect, level, *slot, &mut change.modifiers);
                let actions = actions(&entry.effect, level, body, &mut *world.rng());
                if !actions.is_empty() {
                    change.fired.push(Fired {
                        slot: *slot,
                        actions,
                    });
                }
                change.active.insert(key);
            }
        }
    }
    world.active = None;
    change
}

fn attribute_modifiers(
    effect: &EntityEffect,
    level: u32,
    slot: EquipmentSlot,
    out: &mut Vec<(Attribute, String, f64, ModifierOperation)>,
) {
    match effect {
        EntityEffect::AllOf(effects) => {
            for effect in effects {
                attribute_modifiers(effect, level, slot, out);
            }
        }
        EntityEffect::Attribute(AttributeEffect {
            id,
            attribute,
            amount,
            operation,
        }) => {
            if let Some(kind) = Attribute::from_name(attribute) {
                out.push((
                    kind,
                    format!("{id}/{}", slot_name(slot)),
                    f64::from(amount.at(level)),
                    tuff_core::world::attributes::modifier_operation(*operation),
                ));
            }
        }
        _ => {}
    }
}

fn slot_name(slot: EquipmentSlot) -> &'static str {
    match slot {
        EquipmentSlot::Mainhand => "mainhand",
        EquipmentSlot::Offhand => "offhand",
        EquipmentSlot::Feet => "feet",
        EquipmentSlot::Legs => "legs",
        EquipmentSlot::Chest => "chest",
        EquipmentSlot::Head => "head",
        EquipmentSlot::Body => "body",
        EquipmentSlot::Saddle => "saddle",
    }
}

/// The velocity change an impulse gives an entity looking `yaw`/`pitch`.
pub fn impulse(
    direction: [f64; 3],
    scale: [f64; 3],
    magnitude: f64,
    yaw: f32,
    pitch: f32,
) -> [f64; 3] {
    let (yaw, pitch) = (f64::from(yaw).to_radians(), f64::from(pitch).to_radians());
    let forward = [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ];
    let up = [
        -yaw.sin() * -pitch.sin(),
        pitch.cos(),
        yaw.cos() * -pitch.sin(),
    ];
    let left = [yaw.cos(), 0.0, yaw.sin()];
    let rotated: [f64; 3] = [0, 1, 2]
        .map(|i| left[i] * direction[0] + up[i] * direction[1] + forward[i] * direction[2]);
    [0, 1, 2].map(|i| rotated[i] * scale[i] * magnitude)
}

/// Who a world action runs for: the entity it happens at.
#[derive(Debug, Clone, Copy)]
pub struct Executor {
    pub entity: i32,
    pub uuid: Option<u128>,
    pub kind: EntityType,
    pub position: [f64; 3],
}

/// Runs `actions` on the region at `executor`.
pub fn apply_world(world: &mut dyn RegionWorld, executor: Executor, actions: &[WorldAction]) {
    let origin = executor.position;
    let origin_block = BlockPos::new(
        origin[0].floor() as i32,
        origin[1].floor() as i32,
        origin[2].floor() as i32,
    );
    let shifted = |offset: [i32; 3]| {
        BlockPos::new(
            origin_block.x + offset[0],
            origin_block.y + offset[1],
            origin_block.z + offset[2],
        )
    };
    let event = |world: &mut dyn RegionWorld, name: &Option<String>, pos: BlockPos| {
        if let Some(event) = name
            .as_deref()
            .and_then(tuff_core::world::game_event::GameEvent::from_name)
        {
            world.game_event(
                event,
                pos.center(),
                tuff_core::world::game_event::Context::from(
                    tuff_core::world::game_event::Source::entity(executor.entity),
                ),
            );
        }
    };
    let category = if executor.kind == EntityType::Player {
        tuff_core::protocol::play::sound_category::PLAYERS
    } else {
        tuff_core::protocol::play::sound_category::NEUTRAL
    };
    for action in actions {
        match action {
            WorldAction::Sound {
                sound,
                volume,
                pitch,
            } => {
                let seed = world.rng().r#gen();
                let packet = tuff_core::world::framed(&tuff_core::protocol::play::Sound {
                    sound_event: sound.id(),
                    category,
                    position: origin,
                    volume: *volume,
                    pitch: *pitch,
                    seed,
                });
                let range = f64::from(volume.max(1.0) * 16.0);
                world.send_near(origin, range, None, tuff_core::world::packets([packet]));
            }
            WorldAction::Particles {
                particle,
                options,
                position,
                velocity,
                speed,
            } => {
                let packet =
                    tuff_core::world::framed(&tuff_core::protocol::play::ParticlesWithOptions {
                        particle: particle.id(),
                        options,
                        force: false,
                        position: *position,
                        spread: velocity.map(|v| v as f32),
                        speed: *speed,
                        count: 0,
                    });
                world.send_near(*position, 32.0, None, tuff_core::world::packets([packet]));
            }
            WorldAction::ReplaceBlock {
                offset,
                predicate,
                state,
                game_event,
            } => {
                let pos = shifted(*offset);
                let passes = predicate
                    .as_ref()
                    .is_none_or(|p| block_predicate(p, pos, &|at| world.block(at)));
                if passes
                    && let Some(state) = provided_state(state, &mut *world.rng())
                    && world.block(pos) != Some(state)
                {
                    world.put_block(pos, state);
                    event(world, game_event, pos);
                }
            }
            WorldAction::ReplaceDisk {
                radius,
                height,
                offset,
                predicate,
                state,
                game_event,
            } => {
                let center = shifted(*offset);
                let top = (height - 1).min(0);
                let radius_sq = f64::from(radius * radius);
                for y in center.y..=center.y + top {
                    for x in center.x - radius..=center.x + radius {
                        for z in center.z - radius..=center.z + radius {
                            let pos = BlockPos::new(x, y, z);
                            let dx = f64::from(x) + 0.5 - origin[0];
                            let dy = 0.0;
                            let dz = f64::from(z) + 0.5 - origin[2];
                            if dx * dx + dy * dy + dz * dz >= radius_sq {
                                continue;
                            }
                            if !predicate
                                .as_ref()
                                .is_none_or(|p| block_predicate(p, pos, &|at| world.block(at)))
                            {
                                continue;
                            }
                            let Some(state) = provided_state(state, &mut *world.rng()) else {
                                continue;
                            };
                            if world.block(pos) == Some(state) {
                                continue;
                            }
                            world.put_block(pos, state);
                            event(world, game_event, pos);
                        }
                    }
                }
            }
            WorldAction::SetBlockProperties {
                offset,
                properties,
                game_event,
            } => {
                let pos = shifted(*offset);
                let Some(current) = world.block(pos) else {
                    continue;
                };
                let mut state = current;
                for (key, value) in properties.as_object().into_iter().flatten() {
                    let value = match value {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    if let Some(next) = state.with(key, &value) {
                        state = next;
                    }
                }
                if state != current {
                    world.put_block(pos, state);
                    event(world, game_event, pos);
                }
            }
            WorldAction::Summon { entity } => {
                let created = EntityType::from_name(&qualify(entity)).and_then(|kind| {
                    let id = world.shared().next_entity_id();
                    world.shared().entity_types.create(kind, id, origin, 0.0)
                });
                match created {
                    Some(created) => world.add_entity(created),
                    None => {
                        let mut nbt = tuff_nbt::Compound::new();
                        nbt.insert("id", tuff_nbt::Tag::String(qualify(entity)));
                        world.summon(nbt, origin, true);
                    }
                }
            }
            WorldAction::Function(function) => {
                let target = executor
                    .uuid
                    .map_or_else(|| "@s".to_owned(), tuff_core::resource_pack::format_uuid);
                let command = format!(
                    "execute as {target} positioned {} {} {} run function {}",
                    origin[0],
                    origin[1],
                    origin[2],
                    qualify(function)
                );
                let request = tuff_core::world::commands::block::BlockCommand {
                    dimension: world.dimension_id(),
                    position: origin,
                    yaw: 0.0,
                    name: tuff_item::text::TextComponent::new("Enchantment"),
                    command,
                    track_output: false,
                };
                let shared = world.shared().clone();
                tokio::spawn(tuff_core::world::commands::block::run(shared, request));
            }
            WorldAction::Explode {
                radius,
                knockback,
                offset,
                fire,
                damage_type,
                block_interaction,
                attribute_to_user,
            } => {
                use tuff_core::world::explosion::{BlockInteraction, Explosion, WindBurst};
                let center = [0, 1, 2].map(|i| origin[i] + offset[i]);
                let kind = damage_type
                    .as_deref()
                    .and_then(damage_type_named)
                    .unwrap_or(&damage_type::EXPLOSION);
                let blocks = match (block_interaction.as_str(), knockback) {
                    ("trigger", Some(knockback)) => BlockInteraction::Trigger(WindBurst {
                        knockback: *knockback,
                        sound: SoundEvent::EntityWindChargeWindBurst,
                        source: executor.kind,
                        spares_fall: true,
                    }),
                    ("none" | "trigger", _) => BlockInteraction::Keep,
                    ("block" | "tnt", _) => BlockInteraction::Destroy,
                    _ => BlockInteraction::DestroyWithDecay,
                };
                let culprit = attribute_to_user.then(|| {
                    Box::new(tuff_core::world::entity::Culprit {
                        id: executor.entity,
                        uuid: executor.uuid.unwrap_or(0),
                        kind: executor.kind,
                        name: tuff_item::text::TextComponent::new(String::new()),
                        charged: None,
                        owner_name: None,
                        held_name: None,
                    })
                });
                world.explode(Explosion {
                    center,
                    radius: *radius,
                    fire: *fire,
                    blocks,
                    damage_type: kind,
                    igniter: None,
                    culprit,
                    dangerous_wither_skull: false,
                    spares_nether_portals: false,
                });
            }
        }
    }
}

fn qualify(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

/// The state a block state provider gives: an inline state, a simple or
/// weighted provider, or a pack `worldgen/block_state_provider` entry.
pub fn provided_state(provider: &Value, rng: &mut dyn RngCore) -> Option<BlockState> {
    match provider {
        Value::String(id) => {
            let registries = tuff_item::registries::current();
            let entry = registries
                .get("minecraft:worldgen/block_state_provider")?
                .get(&qualify(id))?
                .json
                .clone()?;
            provided_state(&entry, rng)
        }
        Value::Object(map) => match map.get("type").and_then(Value::as_str) {
            None => state_of(provider),
            Some(kind) => match kind.strip_prefix("minecraft:").unwrap_or(kind) {
                "simple_state_provider" | "simple" | "rotated_block_provider" => {
                    state_of(&provider["state"])
                }
                "weighted_state_provider" => {
                    let entries = provider["entries"].as_array()?;
                    let total: i64 = entries.iter().filter_map(|e| e["weight"].as_i64()).sum();
                    if total <= 0 {
                        return None;
                    }
                    let mut roll = rng.gen_range(0..total);
                    for entry in entries {
                        let weight = entry["weight"].as_i64().unwrap_or(0);
                        if roll < weight {
                            return state_of(&entry["data"]);
                        }
                        roll -= weight;
                    }
                    None
                }
                _ => None,
            },
        },
        _ => None,
    }
}

fn state_of(value: &Value) -> Option<BlockState> {
    let name = value
        .get("id")
        .or_else(|| value.get("Name"))
        .and_then(Value::as_str)
        .or_else(|| value.as_str())?;
    let mut state = Block::from_name(&qualify(name))?.default_state();
    if let Some(properties) = value
        .get("properties")
        .or_else(|| value.get("Properties"))
        .and_then(Value::as_object)
    {
        for (key, value) in properties {
            state = state.with(key, value.as_str()?)?;
        }
    }
    Some(state)
}

/// A block predicate (`matching_blocks`, `matching_block_tag`,
/// `matching_fluids`, `solid`, `replaceable`, `unobstructed`,
/// `inside_world_bounds`, `true` and the logical ones) at `pos`.
pub fn block_predicate(
    predicate: &Value,
    pos: BlockPos,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let offset = predicate["offset"].as_array().map_or([0; 3], |o| {
        [0, 1, 2].map(|i| o.get(i).and_then(Value::as_i64).unwrap_or(0) as i32)
    });
    let at = BlockPos::new(pos.x + offset[0], pos.y + offset[1], pos.z + offset[2]);
    let list = |key: &str| predicate[key].as_array().map_or(&[][..], Vec::as_slice);
    let kind = predicate["type"].as_str().unwrap_or_default();
    match kind.strip_prefix("minecraft:").unwrap_or(kind) {
        "true" => true,
        "not" => !block_predicate(&predicate["predicate"], pos, block),
        "all_of" => list("predicates")
            .iter()
            .all(|p| block_predicate(p, pos, block)),
        "any_of" => list("predicates")
            .iter()
            .any(|p| block_predicate(p, pos, block)),
        "inside_world_bounds" | "unobstructed" => block(at).is_some(),
        "matching_blocks" => block(at).is_some_and(|state| {
            predicate::in_set(&predicate["blocks"], state.block().name(), |tag| {
                tuff_datapack::tags::with(|tags| {
                    tags.contains("minecraft:block", tag, state.block().name())
                })
            })
        }),
        "matching_block_tag" => block(at).is_some_and(|state| {
            let tag = predicate["tag"].as_str().unwrap_or_default();
            let tag = tag.strip_prefix('#').unwrap_or(tag);
            tuff_datapack::tags::with(|tags| {
                tags.contains("minecraft:block", tag, state.block().name())
            })
        }),
        "matching_fluids" => block(at).is_some_and(|state| {
            let fluid = match state.block() {
                Block::Water => "minecraft:water",
                Block::Lava => "minecraft:lava",
                _ if state.get("waterlogged") == Some("true") => "minecraft:water",
                _ => "minecraft:empty",
            };
            predicate::in_set(&predicate["fluids"], fluid, |tag| {
                tuff_datapack::tags::with(|tags| tags.contains("minecraft:fluid", tag, fluid))
            })
        }),
        "solid" => block(at).is_some_and(|state| state.is_solid()),
        "replaceable" => block(at).is_some_and(|state| {
            tuff_datapack::tags::with(|tags| {
                tags.contains(
                    "minecraft:block",
                    "minecraft:replaceable",
                    state.block().name(),
                )
            })
        }),
        _ => false,
    }
}

/// The world around a region entity as requirements see it.
pub struct TickWorld<'a, 'b> {
    pub cx: &'a tuff_core::world::entity::TickContext<'b>,
}

impl PredicateWorld for TickWorld<'_, '_> {
    fn location(&self, position: [f64; 3]) -> LocationFacts {
        let pos = BlockPos::new(
            position[0].floor() as i32,
            position[1].floor() as i32,
            position[2].floor() as i32,
        );
        LocationFacts {
            position,
            dimension: self.cx.shared.dimension(self.cx.dimension).key.clone(),
            biome: (self.cx.biome)(pos).map(str::to_owned),
            block: (self.cx.block)(pos),
            can_see_sky: Some(tuff_core::world::projectile::sees_sky(pos, self.cx)),
            ..LocationFacts::default()
        }
    }
    fn raining(&self) -> bool {
        self.cx.raining
    }
    fn thundering(&self) -> bool {
        tuff_core::world::projectile::thundering(self.cx)
    }
    fn clock(&self, _id: &str) -> Option<i64> {
        None
    }
    fn score(&self, _holder: &str, _objective: &str) -> Option<i32> {
        None
    }
    fn team(&self, _holder: &str) -> Option<&str> {
        None
    }
    fn random(&self) -> f32 {
        0.0
    }
    fn random_int(&self, min: i64, _max: i64) -> i64 {
        min
    }
}

/// The weapon's `hit_block` effects for a projectile of `kind` that struck
/// a block at `position` (inside the block).
pub fn projectile_hit_block(
    weapon: &ItemStack,
    kind: EntityType,
    position: [f64; 3],
    movement: [f64; 3],
    cx: &mut tuff_core::world::entity::TickContext<'_>,
) -> Vec<Action> {
    let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(cx.rng.r#gen());
    let this = facts_of(kind, position);
    let body = Body {
        kind,
        position,
        movement,
    };
    let world = TickWorld { cx };
    let effects = EffectWorld::new(&mut rng).with_world(&world);
    run_weapon(weapon, |e| &e.hit_block, &this, position, &body, &effects)
}

/// What a weapon's `projectile_spawned` effects do to a new projectile.
#[derive(Debug, Default, PartialEq)]
pub struct Spawned {
    /// Seconds to set it on fire for, the longest of any.
    pub fire: Option<f32>,
    /// Added to its velocity.
    pub push: [f64; 3],
    pub world: Vec<WorldAction>,
}

/// The weapon's `projectile_spawned` effects on a projectile of `kind`
/// leaving `position` at `velocity`, facing `yaw`/`pitch`.
pub fn projectile_spawned(
    weapon: &ItemStack,
    kind: EntityType,
    position: [f64; 3],
    velocity: [f64; 3],
    look: (f32, f32),
    rng: &mut dyn RngCore,
) -> Spawned {
    let this = facts_of(kind, position);
    let body = Body {
        kind,
        position,
        movement: velocity,
    };
    let effects = EffectWorld::new(rng);
    let mut spawned = Spawned::default();
    for action in run_weapon(
        weapon,
        |e| &e.projectile_spawned,
        &this,
        position,
        &body,
        &effects,
    ) {
        match action {
            Action::Ignite(seconds) => {
                spawned.fire = Some(spawned.fire.map_or(seconds, |f| f.max(seconds)));
            }
            Action::Impulse {
                direction,
                scale,
                magnitude,
            } => {
                let push = impulse(direction, scale, magnitude, look.0, look.1);
                spawned.push = [0, 1, 2].map(|i| spawned.push[i] + push[i]);
            }
            Action::World(world) => spawned.world.push(world),
            _ => {}
        }
    }
    spawned
}

/// Applies the weapon's `projectile_spawned` effects to an arrow or trident
/// shot from `position` facing `look`; returns its world actions.
pub fn arrow_spawned(
    shot: &mut tuff_core::world::projectile::ArrowShot,
    weapon: &ItemStack,
    position: [f64; 3],
    look: (f32, f32),
    rng: &mut dyn RngCore,
) -> Vec<WorldAction> {
    let kind = match shot.item.item() {
        tuff_vanilla_data::Item::SpectralArrow => EntityType::SpectralArrow,
        tuff_vanilla_data::Item::Trident => EntityType::Trident,
        _ => EntityType::Arrow,
    };
    let spawned = projectile_spawned(weapon, kind, position, shot.velocity, look, rng);
    if let Some(seconds) = spawned.fire {
        shot.fire_ticks = shot.fire_ticks.max((seconds * 20.0) as i16);
    }
    shot.velocity = [0, 1, 2].map(|i| shot.velocity[i] + spawned.push[i]);
    spawned.world
}

/// Runs `actions` at `position` for a player, on its region there.
pub async fn session_world(
    session: &mut tuff_core::world::session::PlayerSession,
    position: [f64; 3],
    actions: Vec<WorldAction>,
) {
    if actions.is_empty() {
        return;
    }
    let at = BlockPos::new(
        position[0].floor() as i32,
        position[1].floor() as i32,
        position[2].floor() as i32,
    );
    let Some(region) = session
        .region(tuff_world_model::pos::ChunkPos::of_block(at).region())
        .await
    else {
        return;
    };
    let executor = Executor {
        entity: session.id,
        uuid: Some(session.profile.id),
        kind: EntityType::Player,
        position,
    };
    region
        .request(tuff_core::world::messages::RegionRequest::Run(
            tuff_core::world::region_api::RegionTask::new(move |world| {
                apply_world(world, executor, &actions);
            }),
        ))
        .await;
}

/// Runs `actions` on the region of a ticking entity, after its tick.
pub fn push_world(
    cx: &mut tuff_core::world::entity::TickContext<'_>,
    executor: Executor,
    actions: Vec<WorldAction>,
) {
    if actions.is_empty() {
        return;
    }
    cx.effects.push(tuff_core::world::entity::Effect::Run(
        tuff_core::world::region_api::RegionTask::new(move |world| {
            apply_world(world, executor, &actions);
        }),
    ));
}

/// Damage facts for a damage context's kind and entities.
pub fn damage_facts(
    kind: &DamageType,
    source: Option<EntityFacts>,
    direct: Option<EntityFacts>,
) -> Option<DamageFacts> {
    Some(DamageFacts {
        kind: damage_type_named(kind.name)?,
        source,
        direct,
    })
}

/// Facts for an entity known only by its type.
pub fn facts_of(kind: EntityType, position: [f64; 3]) -> EntityFacts {
    EntityFacts {
        kind: Some(kind),
        position,
        ..EntityFacts::default()
    }
}

#[cfg(test)]
mod tests;
