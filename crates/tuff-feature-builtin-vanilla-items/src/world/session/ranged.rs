//! Crossbows in a player's hands: loading over the charge time (with its
//! sounds), holding the loaded arrows or rockets in the crossbow, and
//! firing them all at once (Multishot fans three out, Piercing passes
//! through).

use rand::Rng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::{SoundEvent, StatType};

use tuff_core::world::inventory::Inventory;
use tuff_core::world::player::UsingItem;
use tuff_core::world::projectile::{ArrowShot, Flight, Pickup, Shot};
use tuff_core::world::session::*;
use tuff_core::world::stats::Stat;
use tuff_item::item::ItemStack;
use tuff_vanilla_common::combat;
use tuff_vanilla_common::ranged::{self, INTANGIBLE, intangible};

use super::item_use::ItemUseExt;

const ARROW_POWER: f32 = 3.15;
const TRIDENT_POWER: f32 = 2.5;
/// Ticks a trident must be held back before it is thrown.
const THROW_THRESHOLD_TICKS: u32 = 10;
const FIREWORK_POWER: f32 = 1.6;
const UNCERTAINTY: f32 = 1.0;
const START_SOUND_PERCENT: f32 = 0.2;
const MID_SOUND_PERCENT: f32 = 0.5;

fn is_arrow(stack: &ItemStack) -> bool {
    tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::ARROWS, stack.item)
}

fn is_intangible(stack: &ItemStack) -> bool {
    stack.components.other.get(INTANGIBLE).is_some()
}

#[allow(async_fn_in_trait)]
pub trait RangedExt {
    /// What a crossbow would load: arrows or rockets held in either hand
    /// (off hand first), else the first arrows in the inventory; creative
    /// players always have an arrow. The window slot is `None` for that
    /// free arrow.
    fn crossbow_ammo(&self) -> Option<(Option<usize>, ItemStack)>;
    /// Right-clicking with a crossbow: a loaded one fires, an empty one
    /// starts loading if there is ammo.
    async fn use_crossbow(&mut self, off_hand: bool);
    /// One tick of loading: the start and middle sounds, then the load.
    async fn tick_crossbow(&mut self, using: UsingItem);
    /// Fires everything the crossbow in hand holds.
    async fn shoot_crossbow(&mut self, off_hand: bool);
    /// Right-clicking with a trident readies a throw (or, with Riptide,
    /// a launch, only in water or rain).
    async fn use_trident(&mut self, off_hand: bool);
    /// Letting go of a trident held back long enough: it flies, or a
    /// Riptide one launches its holder.
    async fn release_trident(&mut self, using: UsingItem);
    /// In water, or out in the rain.
    fn in_water_or_rain(&self) -> bool;
}

impl RangedExt for PlayerSession {
    fn crossbow_ammo(&self) -> Option<(Option<usize>, ItemStack)> {
        let held = |s: &ItemStack| is_arrow(s) || s.item == Item::FireworkRocket;
        for off_hand in [true, false] {
            if let Some(stack) = self.inventory.hand(off_hand).filter(|s| held(s)) {
                let slot = if off_hand {
                    Inventory::OFFHAND_SLOT
                } else {
                    Inventory::hotbar_slot(self.inventory.selected())
                };
                return Some((Some(slot), stack.clone()));
            }
        }
        if let Some(slot) = self.inventory.find_matching(is_arrow) {
            let stack = self.inventory.slots()[slot].clone()?;
            return Some((Some(slot), stack));
        }
        self.mode
            .abilities
            .instabuild
            .then(|| (None, ItemStack::new(Item::Arrow, 1)))
    }

    async fn use_crossbow(&mut self, off_hand: bool) {
        let Some(crossbow) = self.inventory.hand(off_hand) else {
            return;
        };
        if crossbow
            .components
            .charged_projectiles
            .as_ref()
            .is_some_and(|loaded| !loaded.is_empty())
        {
            self.shoot_crossbow(off_hand).await;
            return;
        }
        if self.actions.using.is_none() && self.crossbow_ammo().is_some() {
            self.actions.using = Some(UsingItem::new(off_hand, Item::Crossbow, UsingItem::HELD));
            self.publish(false).await;
        }
    }

    async fn tick_crossbow(&mut self, using: UsingItem) {
        let Some(crossbow) = self.inventory.hand(using.off_hand).cloned() else {
            return;
        };
        let charge = ranged::crossbow_charge_ticks(&crossbow) as f32;
        let elapsed = (using.total - using.remaining) as f32;
        let percent = elapsed / charge;
        let before = (elapsed - 1.0) / charge;
        let sounds = ranged::charging_sounds(&crossbow);
        let crossed = |at: f32| {
            percent >= at
                && !matches!(
                    before.partial_cmp(&at),
                    Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                )
        };
        if crossed(START_SOUND_PERCENT)
            && let Some(sound) = sounds.start
        {
            self.player_sound(sound, 0.5, 1.0).await;
        }
        if crossed(MID_SOUND_PERCENT)
            && let Some(sound) = sounds.mid
        {
            self.player_sound(sound, 0.5, 1.0).await;
        }
        let loaded = crossbow
            .components
            .charged_projectiles
            .as_ref()
            .is_some_and(|loaded| !loaded.is_empty());
        if percent >= 1.0
            && !loaded
            && load(self, using.off_hand, &crossbow)
            && let Some(sound) = sounds.end
        {
            let pitch = 1.0 / (self.rng.r#gen::<f32>() * 0.5 + 1.0) + 0.2;
            self.player_sound(sound, 1.0, pitch).await;
        }
    }

    async fn shoot_crossbow(&mut self, off_hand: bool) {
        let slot = hand_slot(self, off_hand);
        let Some(mut crossbow) = self.inventory.hand(off_hand).cloned() else {
            return;
        };
        let projectiles = crossbow
            .components
            .charged_projectiles
            .take()
            .unwrap_or_default();
        if projectiles.is_empty() {
            return;
        }
        self.inventory.set_slot(slot, Some(crossbow.clone()));
        let power = if projectiles.iter().any(|p| p.item == Item::FireworkRocket) {
            FIREWORK_POWER
        } else {
            ARROW_POWER
        };
        let angles = ranged::volley_angles(projectiles.len(), ranged::projectile_spread(&crossbow));
        let eye = self.eye();
        for (index, (projectile, angle)) in projectiles.iter().zip(angles).enumerate() {
            let direction = ranged::volley_direction(self.pose.pitch, self.pose.yaw, angle);
            let velocity = tuff_core::world::projectile::shot_velocity(
                direction,
                power,
                UNCERTAINTY,
                &mut self.rng,
            );
            if projectile.item == Item::FireworkRocket {
                let position = [eye[0], eye[1] - 0.150_000_005_960_464_48, eye[2]];
                let shot = Shot::Firework {
                    item: projectile.clone(),
                    flight: Flight::Shot(velocity),
                };
                self.launch(position, shot).await;
            } else {
                let position = [eye[0], eye[1] - 0.100_000_001_490_116_12, eye[2]];
                let mut item = projectile.clone();
                let free = is_intangible(&item);
                item.components.remove_other(INTANGIBLE);
                let mut arrow = ArrowShot::new(item.with_count(1), velocity);
                arrow.crit = true;
                arrow.sound = Some(SoundEvent::ItemCrossbowHit);
                arrow.weapon = Some(crossbow.clone());
                arrow.pierce = ranged::piercing(&crossbow);
                if free {
                    arrow.pickup = Some(Pickup::CreativeOnly);
                }
                let look = (self.pose.yaw, self.pose.pitch);
                let world = tuff_vanilla_common::enchant_effects::arrow_spawned(
                    &mut arrow,
                    &crossbow,
                    position,
                    look,
                    &mut self.rng,
                );
                tuff_vanilla_common::enchant_effects::session_world(self, position, world).await;
                self.launch(position, Shot::Arrow(arrow)).await;
            }
            let pitch = if index == 0 {
                1.0
            } else {
                let high = index & 1 == 1;
                1.0 / (self.rng.r#gen::<f32>() * 0.5 + 1.8) + if high { 0.63 } else { 0.43 }
            };
            self.player_sound(SoundEvent::ItemCrossbowShoot, 1.0, pitch)
                .await;
            let wear = if projectile.item == Item::FireworkRocket {
                3
            } else {
                1
            };
            self.hurt_hand(off_hand, wear).await;
            if self
                .inventory
                .hand(off_hand)
                .is_none_or(|s| s.item != Item::Crossbow)
            {
                break;
            }
        }
        self.fire(tuff_core::world::advancement::trigger::Event::ShotCrossbow(
            &crossbow,
        ));
        self.progress
            .stats
            .award(Stat::item(StatType::Used, Item::Crossbow), 1);
        self.send_inventory();
    }

    async fn use_trident(&mut self, off_hand: bool) {
        let Some(trident) = self.inventory.hand(off_hand) else {
            return;
        };
        if next_damage_breaks(trident)
            || (ranged::spin_attack_strength(trident) > 0.0 && !self.in_water_or_rain())
            || self.actions.using.is_some()
        {
            return;
        }
        self.actions.using = Some(UsingItem::new(off_hand, Item::Trident, UsingItem::HELD));
        self.publish(false).await;
    }

    async fn release_trident(&mut self, using: UsingItem) {
        if using.total - using.remaining < THROW_THRESHOLD_TICKS {
            return;
        }
        let Some(trident) = self
            .inventory
            .hand(using.off_hand)
            .filter(|s| s.item == Item::Trident)
            .cloned()
        else {
            return;
        };
        let strength = ranged::spin_attack_strength(&trident);
        if (strength > 0.0 && (!self.in_water_or_rain() || self.ride.riding.is_some()))
            || next_damage_breaks(&trident)
        {
            return;
        }
        let sound = ranged::trident_sound(&trident);
        self.progress
            .stats
            .award(Stat::item(StatType::Used, Item::Trident), 1);
        self.hurt_hand(using.off_hand, 1).await;
        if strength > 0.0 {
            self.start_spin_attack();
            self.motion.shoves.push(
                self.tick,
                tuff_core::world::session::ShoveKind::Riptide {
                    strength,
                    yaw: self.pose.yaw,
                    pitch: self.pose.pitch,
                },
            );
            self.player_sound(sound, 1.0, 1.0).await;
            self.publish(false).await;
            return;
        }
        let Some(thrown) = self.inventory.hand(using.off_hand).cloned() else {
            return;
        };
        if !self.mode.abilities.instabuild {
            self.inventory.take_from_hand(using.off_hand, 1);
        }
        let velocity = self.shot_velocity(TRIDENT_POWER);
        let eye = self.eye();
        let position = [eye[0], eye[1] - 0.100_000_001_490_116_12, eye[2]];
        let mut trident = ArrowShot::new(thrown.with_count(1), velocity);
        if self.mode.abilities.instabuild {
            trident.pickup = Some(Pickup::CreativeOnly);
        }
        self.launch(position, Shot::Arrow(trident)).await;
        let seed = self.rng.r#gen();
        let packet = tuff_core::world::packets([tuff_core::world::framed(
            &tuff_core::protocol::play::Sound {
                sound_event: sound.id(),
                category: tuff_core::protocol::play::sound_category::PLAYERS,
                position,
                volume: 1.0,
                pitch: 1.0,
                seed,
            },
        )]);
        self.broadcast_chunk(packet).await;
        self.send_inventory();
    }

    fn in_water_or_rain(&self) -> bool {
        self.body.fall.in_water() || self.surroundings.contact.in_rain
    }
}

/// Whether one more point of wear would break the stack.
fn next_damage_breaks(stack: &ItemStack) -> bool {
    !stack.components.unbreakable
        && stack
            .max_damage()
            .is_some_and(|max| stack.damage() + 1 >= max)
}

fn hand_slot(session: &PlayerSession, off_hand: bool) -> usize {
    if off_hand {
        Inventory::OFFHAND_SLOT
    } else {
        Inventory::hotbar_slot(session.inventory.selected())
    }
}

/// Draws ammo into the crossbow in hand: one from the stack (none for a
/// creative player), and free copies for Multishot's extra bolts.
fn load(session: &mut PlayerSession, off_hand: bool, crossbow: &ItemStack) -> bool {
    let Some((slot, ammo)) = session.crossbow_ammo() else {
        return false;
    };
    let count = ranged::projectile_count(crossbow);
    let mut drawn = Vec::with_capacity(count);
    let mut left = ammo.count;
    for i in 0..count {
        let uses = if i > 0 || session.mode.abilities.instabuild {
            0
        } else {
            combat::ammo_use(crossbow, ammo.item, 1.0).floor().max(0.0) as u8
        };
        if uses > left {
            continue;
        }
        if uses == 0 {
            drawn.push(intangible(ammo.clone()));
        } else {
            left -= uses;
            drawn.push(ammo.clone().with_count(uses));
        }
    }
    if drawn.is_empty() {
        return false;
    }
    if left != ammo.count
        && let Some(slot) = slot
    {
        session
            .inventory
            .set_slot(slot, (left > 0).then(|| ammo.clone().with_count(left)));
    }
    let mut loaded = crossbow.clone();
    loaded.components.charged_projectiles = Some(drawn);
    session
        .inventory
        .set_slot(hand_slot(session, off_hand), Some(loaded));
    session.send_inventory();
    true
}
