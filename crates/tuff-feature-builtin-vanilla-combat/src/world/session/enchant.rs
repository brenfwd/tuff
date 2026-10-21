//! A player's enchantment entity effects, run from their data: `tick` every
//! tick, `location_changed` when the player changes block or equipment,
//! and `post_piercing_attack` after a stab. Effects on the player apply
//! here; effects on the world run on the home region.

use std::collections::BTreeSet;

use tuff_core::protocol::play;
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::predicate::{EntityFacts, LocationFacts, PredicateWorld};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::{framed, packets};
use tuff_item::enchantment::{Conditional, EnchantmentEffects, EntityEffect};
use tuff_item::item::{Enchantments, ItemStack};
use tuff_vanilla_common::enchant_effects::{self as fx, Action, ActiveKey, Body, EffectWorld};
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{EntityType, Item};
use tuff_world_model::BlockPos;

const SLOTS: [EquipmentSlot; 6] = [
    EquipmentSlot::Mainhand,
    EquipmentSlot::Offhand,
    EquipmentSlot::Feet,
    EquipmentSlot::Legs,
    EquipmentSlot::Chest,
    EquipmentSlot::Head,
];

/// What the `location_changed` effects last saw.
#[derive(Default)]
pub struct LocationEffects {
    block: Option<BlockPos>,
    active: BTreeSet<ActiveKey>,
    equipment: Vec<(EquipmentSlot, Item, Option<Enchantments>)>,
}

/// The world as a player's enchantment requirements see it: the blocks the
/// client was sent and the weather.
struct SessionWorld<'a> {
    session: &'a PlayerSession,
}

impl PredicateWorld for SessionWorld<'_> {
    fn location(&self, position: [f64; 3]) -> LocationFacts {
        let pos = BlockPos::new(
            position[0].floor() as i32,
            position[1].floor() as i32,
            position[2].floor() as i32,
        );
        LocationFacts {
            position,
            dimension: self.session.dimension().key.clone(),
            block: self.session.client_world().block_at(pos),
            ..LocationFacts::default()
        }
    }
    fn raining(&self) -> bool {
        self.session.shared.weather.borrow().raining
    }
    fn thundering(&self) -> bool {
        self.session.shared.weather.borrow().thundering
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

fn body(session: &PlayerSession) -> Body {
    Body {
        kind: EntityType::Player,
        position: session.pose.position,
        movement: session.sync.known_movement,
    }
}

fn enchanted(session: &PlayerSession) -> bool {
    SLOTS.iter().any(|slot| {
        session
            .inventory
            .equipped(*slot)
            .is_some_and(|stack| stack.components.enchantments.is_some())
    })
}

/// Fires `component` for the player's equipment.
fn fire(
    session: &mut PlayerSession,
    component: fn(&EnchantmentEffects) -> &[Conditional<EntityEffect>],
    equipment: &[(EquipmentSlot, tuff_item::item::ItemSnapshot)],
    facts: &EntityFacts,
) -> Vec<fx::Fired> {
    let worn: Vec<(EquipmentSlot, &ItemStack)> = equipment
        .iter()
        .map(|(slot, stack)| (*slot, &**stack))
        .collect();
    let body = body(session);
    let mut rng = std::mem::replace(&mut session.rng, rand::SeedableRng::seed_from_u64(0));
    let fired = {
        let world = SessionWorld { session };
        let effects = EffectWorld::new(&mut rng).with_world(&world);
        fx::run(&worn, component, facts, &body, &effects)
    };
    session.rng = rng;
    fired
}

/// One tick of the player's enchantment effects.
pub async fn tick(session: &mut PlayerSession) {
    let tracked = session.ext.entry::<LocationEffects>();
    let nothing_active = tracked.active.is_empty() && tracked.equipment.is_empty();
    if !enchanted(session) && nothing_active {
        return;
    }
    if session.is_dead() {
        return;
    }
    let equipment = session.equipment();
    let facts = session.player_facts();
    for fired in fire(session, |e| &e.tick, &equipment, &facts) {
        apply(session, fired.slot, fired.actions).await;
    }
    location(session, &equipment, &facts).await;
}

async fn location(
    session: &mut PlayerSession,
    equipment: &[(EquipmentSlot, tuff_item::item::ItemSnapshot)],
    facts: &EntityFacts,
) {
    let position = session.pose.position;
    let block = BlockPos::new(
        position[0].floor() as i32,
        position[1].floor() as i32,
        position[2].floor() as i32,
    );
    let signature: Vec<(EquipmentSlot, Item, Option<Enchantments>)> = equipment
        .iter()
        .map(|(slot, stack)| (*slot, stack.item, stack.components.enchantments.clone()))
        .collect();
    let tracked = session.ext.entry::<LocationEffects>();
    if tracked.block == Some(block) && tracked.equipment == signature {
        return;
    }
    tracked.block = Some(block);
    tracked.equipment = signature;
    let was_active = std::mem::take(&mut tracked.active);
    let worn: Vec<(EquipmentSlot, &ItemStack)> = equipment
        .iter()
        .map(|(slot, stack)| (*slot, &**stack))
        .collect();
    let body = body(session);
    let mut rng = std::mem::replace(&mut session.rng, rand::SeedableRng::seed_from_u64(0));
    let change = {
        let world = SessionWorld { session };
        let mut effects = EffectWorld::new(&mut rng).with_world(&world);
        fx::location_changed(&worn, facts, &body, &was_active, &mut effects)
    };
    session.rng = rng;
    session.ext.entry::<LocationEffects>().active = change.active;
    if session.surroundings.enchantment_modifiers != change.modifiers {
        session.surroundings.enchantment_modifiers = change.modifiers;
        session.sync_attributes(false).await;
    }
    for fired in change.fired {
        apply(session, fired.slot, fired.actions).await;
    }
}

/// The weapon's `post_piercing_attack` effects after a stab.
pub async fn post_piercing_attack(session: &mut PlayerSession, weapon: &ItemStack) {
    let equipment = [(EquipmentSlot::Mainhand, tuff_item::item::ItemSnapshot::of(weapon))];
    let facts = session.player_facts();
    for fired in fire(session, |e| &e.post_piercing_attack, &equipment, &facts) {
        Box::pin(apply(session, fired.slot, fired.actions)).await;
    }
}

/// Applies `actions` of an enchantment on the item in `slot` to the player.
pub async fn apply(session: &mut PlayerSession, slot: EquipmentSlot, actions: Vec<Action>) {
    let mut world = Vec::new();
    for action in actions {
        match action {
            Action::Ignite(seconds) => session.ignite(seconds),
            Action::Damage { kind, amount } => {
                session.damage(amount, kind, None).await;
            }
            Action::ItemDamage(amount) => session.wear_slot(slot, amount).await,
            Action::MobEffect {
                effect,
                amplifier,
                ticks,
            } => {
                session
                    .add_effect(ActiveEffect::timed(effect, i64::from(ticks), amplifier))
                    .await;
            }
            Action::Impulse {
                direction,
                scale,
                magnitude,
            } => {
                let push = fx::impulse(
                    direction,
                    scale,
                    magnitude,
                    session.pose.yaw,
                    session.pose.pitch,
                );
                let known = session.sync.known_movement;
                let velocity = [0, 1, 2].map(|i| known[i] + push[i]);
                session.motion.velocity_sent.push(session.tick, velocity);
                let motion = packets([framed(&play::SetEntityMotion {
                    entity_id: session.id,
                    velocity,
                })]);
                session.broadcast_entity(motion, true).await;
            }
            Action::Exhaustion(amount) => session.exhaust(amount),
            Action::World(action) => world.push(action),
        }
    }
    let position = session.pose.position;
    fx::session_world(session, position, world).await;
}
