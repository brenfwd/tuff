//! Using held items: eating, drinking, bows and thrown items, potions,
//! fireworks and equipment swaps, and soul speed.

use tuff_core::world::stats::Stat;
use tuff_vanilla_data::registry::StatType;

use rand::Rng;
use tuff_item::item::{StackConsumable, StackConsumeEffect};
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::{EquipmentSlot, UseAnimation};
use tuff_vanilla_data::registry::{DataComponentType, EntityType, MobEffect, SoundEvent};

const OMINOUS_BOTTLE_OMEN_TICKS: i64 = 120_000;
/// The speed a player throws a wind charge at.
const WIND_CHARGE_THROW_POWER: f32 = 1.5;

use tuff_core::protocol::play::Serverbound;
use tuff_core::protocol::play::{self, GameMode, PlayerActionStatus};
use tuff_core::world::effects::{self, ActiveEffect};
use tuff_core::world::entity::Culprit;
use tuff_core::world::inventory::Inventory;
use tuff_core::world::messages::PlayerEvent;
use tuff_core::world::messages::{Packets, RegionRequest};
use tuff_core::world::player::UsingItem;
use tuff_core::world::projectile::{ArrowShot, Flight, Launch, Owner, Pickup, Shot};
use tuff_core::world::session::hooks::{HookFuture, SessionHooks, SessionTick};
use tuff_core::world::session::*;
use tuff_core::world::{framed, packets};
use tuff_item::item::ItemStack;
use tuff_vanilla_common::combat;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const BUNDLE_USE_TICKS: u32 = 200;
const BUNDLE_FIRST_THROW_TICKS: u32 = 10;
const BUNDLE_THROW_INTERVAL: u32 = 2;

#[allow(async_fn_in_trait)]
pub trait ItemUseExt {
    /// A firework rocket used while gliding: consumed, and its boost
    /// applied to the tracked glide velocity for its flight duration.
    async fn boost_with_firework(&mut self, off_hand: bool);
    /// A knowledge book: consumed, and every recipe it lists awarded when
    /// all of them exist; an empty or invalid list teaches nothing.
    fn read_knowledge_book(&mut self, off_hand: bool);

    /// Use Item (right click in the air, or on a block that did nothing):
    /// starts eating, drinking or blocking, or swaps armor into its slot.
    async fn use_item(&mut self, off_hand: bool);
    /// Right-clicking equippable gear: it swaps with what is worn, unless
    /// the worn piece is identical or bound by a curse (outside creative).
    /// From a stack of several, one is worn and the old piece goes to the
    /// inventory.
    async fn swap_equipment(&mut self, off_hand: bool, slot: usize);
    /// The arrows a bow would shoot: arrows held in either hand (off hand
    /// first), else the first in the inventory; creative players always
    /// have one. The window slot is `None` for a creative player's free
    /// arrow.
    fn bow_ammo(&self) -> Option<(Option<usize>, ItemStack)>;
    /// The velocity of something this player shoots or throws along its
    /// view, carrying its own movement along.
    fn shot_velocity(&mut self, power: f32) -> [f64; 3];
    /// A shot from the player's rotation with its pitch raised by
    /// `pitch_offset` degrees, carrying the player's own motion.
    fn shot_velocity_offset(&mut self, power: f32, pitch_offset: f32) -> [f64; 3];
    /// Spawns the projectile `shot` at `position`, launched by this
    /// player: its entity id, unless no feature makes its type.
    async fn launch(&mut self, position: [f64; 3], shot: Shot) -> Option<i32>;
    /// Letting go of a drawn bow: the longer it was drawn, the faster the
    /// arrow (a full draw is critical). Uses an arrow unless creative or
    /// Infinity makes it free, and wears the bow.
    async fn release_bow(&mut self, using: UsingItem);
    /// Asks the region around the player's eyes where an eye of ender
    /// would fly; the answer comes back as [`PlayerEvent::EyeTarget`].
    async fn locate_eye(&mut self, off_hand: bool);
    /// Sounding a goat horn: its sound heard far around, a game event, a
    /// cooldown for the instrument's length.
    async fn toot_horn(&mut self, off_hand: bool);
    /// Throws the eye of ender in hand toward `target`, if one is still
    /// there.
    async fn throw_eye(&mut self, off_hand: bool, target: BlockPos);
    /// Casting a fishing rod, or reeling its bobber in.
    async fn use_fishing_rod(&mut self, off_hand: bool);
    async fn bobber_reeled(&mut self, damage: u32, fish: u32);

    /// Throwing a snowball, egg or ender pearl.
    async fn throw_projectile(&mut self, off_hand: bool, item: Item, kind: EntityType);
    /// Throwing a wind charge from the player's eyes.
    async fn throw_wind_charge(&mut self, off_hand: bool);
    /// The eating or drinking sound of `item`, with vanilla's volume and
    /// pitch jitter. `finishing` uses the completion sound's parameters.
    fn consume_sound(&mut self, consumable: &StackConsumable) -> Packets;
    async fn tick_using(&mut self);
    /// Throws one stack out of the bundle in the using hand: on the first
    /// tick, then every other tick once ten have passed.
    async fn drop_bundle_content(&mut self, using: UsingItem);
    /// Finishes eating or drinking: sounds, food, effects, and the item's
    /// remainder, as `Consumable.onConsume` does.
    async fn consume(&mut self, off_hand: bool, item: Item);
    async fn apply_consume_effect(&mut self, effect: &StackConsumeEffect);
    /// A splash potion or cloud: its instant effects at their strength,
    /// then the lasting ones.
    async fn take_potion(&mut self, hit: tuff_core::world::effects::PotionHit);
}

impl ItemUseExt for PlayerSession {
    /// A firework rocket used while gliding: consumed, and its boost
    /// applied to the tracked glide velocity for its flight duration.
    async fn boost_with_firework(&mut self, off_hand: bool) {
        let Some(stack) = self.inventory.hand(off_hand).cloned() else {
            return;
        };
        if !self.mode.abilities.instabuild {
            self.inventory.take_from_hand(off_hand, 1);
            self.send_inventory();
        }
        let lifetime = tuff_core::world::projectile::firework_lifetime(&stack, &mut self.rng);
        self.motion.firework_boost_ticks = lifetime.max(0) as u32;
        let shot = Shot::Firework {
            item: stack,
            flight: Flight::Attached {
                player: self.id,
                lifetime,
            },
        };
        self.launch(self.pose.position, shot).await;
    }

    /// A knowledge book: consumed, and every recipe it lists awarded when
    /// all of them exist; an empty or invalid list teaches nothing.
    fn read_knowledge_book(&mut self, off_hand: bool) {
        let Some(stack) = self.inventory.hand(off_hand) else {
            return;
        };
        let names: Vec<String> = stack
            .components
            .other
            .get("minecraft:recipes")
            .and_then(|tag| match tag {
                tuff_nbt::Tag::List(list) => Some(list),
                _ => None,
            })
            .map(|list| {
                list.iter()
                    .filter_map(|tag| tag.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if !self.mode.abilities.instabuild {
            self.inventory.take_from_hand(off_hand, 1);
            self.send_inventory();
        }
        let set = self.shared.recipes();
        if names.is_empty() || names.iter().any(|name| set.get(name).is_none()) {
            return;
        }
        self.award_recipes(names.iter().map(String::as_str));
        self.progress
            .stats
            .award(Stat::item(StatType::Used, Item::KnowledgeBook), 1);
    }

    /// Use Item (right click in the air, or on a block that did nothing):
    /// starts eating, drinking or blocking, or swaps armor into its slot.
    async fn use_item(&mut self, off_hand: bool) {
        if self.is_dead() || self.mode.game_mode == GameMode::Spectator {
            return;
        }
        let Some(stack) = self.inventory.hand(off_hand) else {
            return;
        };
        let item = stack.item;
        let kept = |kind: DataComponentType| !stack.components.is_removed(kind.name());
        let consumable = stack.consumable();
        let food = item.food().filter(|_| kept(DataComponentType::Food));
        let equippable = stack.equippable();
        let blocks_attacks =
            item.blocks_attacks().is_some() && kept(DataComponentType::BlocksAttacks);
        if self.stack_on_cooldown(stack) {
            return;
        }
        if item == Item::FireworkRocket && self.motion.glide.active {
            self.boost_with_firework(off_hand).await;
            return;
        }
        if item == Item::Bow {
            if self.actions.using.is_none() && self.bow_ammo().is_some() {
                self.actions.using = Some(UsingItem::new(off_hand, item, UsingItem::HELD));
                self.publish(false).await;
            }
            return;
        }
        if item == Item::Crossbow {
            super::ranged::RangedExt::use_crossbow(self, off_hand).await;
            return;
        }
        if item == Item::Trident {
            super::ranged::RangedExt::use_trident(self, off_hand).await;
            return;
        }
        if item == Item::GoatHorn {
            self.toot_horn(off_hand).await;
            return;
        }
        if item == Item::Spyglass {
            if self.actions.using.is_none() {
                self.progress
                    .stats
                    .award(Stat::item(StatType::Used, item), 1);
                self.actions.using = Some(UsingItem::new(off_hand, item, SPYGLASS_USE_TICKS));
                self.player_sound_to_others(SoundEvent::ItemSpyglassUse, 1.0, 1.0)
                    .await;
                self.publish(false).await;
            }
            return;
        }
        if item == Item::KnowledgeBook {
            self.read_knowledge_book(off_hand);
            return;
        }
        if item == Item::EnderEye {
            self.locate_eye(off_hand).await;
            return;
        }
        if item == Item::FishingRod {
            self.use_fishing_rod(off_hand).await;
            return;
        }
        if item == Item::WindCharge {
            self.throw_wind_charge(off_hand).await;
            return;
        }
        if let Some(kind) = tuff_core::world::projectile::thrown_kind(item) {
            self.throw_projectile(off_hand, item, kind).await;
            return;
        }
        if stack.is_bundle() {
            if self.actions.using.is_none() {
                self.actions.using = Some(UsingItem::new(off_hand, item, BUNDLE_USE_TICKS));
                self.publish(false).await;
            }
            return;
        }
        if let Some(consumable) = consumable {
            let edible = food.is_none_or(|food| self.can_eat(food.can_always_eat));
            if !edible || self.actions.using.is_some() {
                return;
            }
            let ticks = consumable.consume_ticks();
            if ticks == 0 {
                self.consume(off_hand, item).await;
            } else {
                self.actions.using = Some(UsingItem::new(off_hand, item, ticks));
                self.publish(false).await;
            }
            return;
        }
        if let Some(equippable) = equippable
            && equippable.swappable
            && let Some(slot) = Inventory::armor_slot(equippable.slot)
        {
            self.swap_equipment(off_hand, slot).await;
            return;
        }
        if let Some(kinetic) = stack.kinetic_weapon() {
            if self.actions.using.is_none() {
                self.actions.using = Some(UsingItem::new(off_hand, item, UsingItem::HELD));
                if let Some(sound) = kinetic.sound {
                    self.sound_at(sound, self.pose.position).await;
                }
                self.publish(false).await;
            }
            return;
        }
        if blocks_attacks && self.actions.using.is_none() {
            self.actions.using = Some(UsingItem::new(off_hand, item, UsingItem::HELD));
            self.publish(false).await;
        }
    }

    /// Right-clicking equippable gear: it swaps with what is worn, unless
    /// the worn piece is identical or bound by a curse (outside creative).
    /// From a stack of several, one is worn and the old piece goes to the
    /// inventory.
    async fn swap_equipment(&mut self, off_hand: bool, slot: usize) {
        let Some(hand) = self.inventory.hand(off_hand).cloned() else {
            return;
        };
        let worn = self.inventory.slots()[slot].clone();
        let bound = worn.as_ref().is_some_and(|stack| {
            stack.has_enchantment_effect(|effects| effects.prevent_armor_change)
        });
        let identical = worn
            .as_ref()
            .is_some_and(|w| w.item == hand.item && w.components == hand.components);
        if (bound && !self.mode.abilities.instabuild) || identical {
            self.send_inventory();
            return;
        }
        let hand_slot = if off_hand {
            Inventory::OFFHAND_SLOT
        } else {
            Inventory::hotbar_slot(self.inventory.selected())
        };
        if hand.count <= 1 {
            let to_hand = if worn.is_none() && self.mode.abilities.instabuild {
                Some(hand.clone())
            } else {
                worn
            };
            self.inventory.set_slot(slot, Some(hand));
            self.inventory.set_slot(hand_slot, to_hand);
        } else {
            self.inventory
                .set_slot(slot, Some(hand.clone().with_count(1)));
            if !self.mode.abilities.instabuild {
                self.inventory.take_from_hand(off_hand, 1);
            }
            if let Some(worn) = worn
                && let Some(extra) = self.inventory.add(worn)
            {
                self.throw_item(extra).await;
            }
        }
        self.send_inventory();
    }

    /// The arrows a bow would shoot: arrows held in either hand (off hand
    /// first), else the first in the inventory; creative players always
    /// have one. The window slot is `None` for a creative player's free
    /// arrow.
    fn bow_ammo(&self) -> Option<(Option<usize>, ItemStack)> {
        let is_arrow = |s: &ItemStack| {
            tuff_datapack::tags::is(tuff_vanilla_data::tag::item::key::ARROWS, s.item)
        };
        for off_hand in [true, false] {
            if let Some(stack) = self.inventory.hand(off_hand).filter(|s| is_arrow(s)) {
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

    /// The velocity of something this player shoots or throws along its
    /// view, carrying its own movement along.
    fn shot_velocity(&mut self, power: f32) -> [f64; 3] {
        self.shot_velocity_offset(power, 0.0)
    }

    /// A shot from the player's rotation with its pitch raised by
    /// `pitch_offset` degrees, carrying the player's own motion.
    fn shot_velocity_offset(&mut self, power: f32, pitch_offset: f32) -> [f64; 3] {
        let mut velocity = tuff_core::world::projectile::shot_from_rotation(
            self.pose.pitch,
            self.pose.yaw,
            pitch_offset,
            power,
            1.0,
            &mut self.rng,
        );
        let movement = self.sync.known_movement;
        velocity[0] += movement[0];
        velocity[2] += movement[2];
        if !self.pose.on_ground {
            velocity[1] += movement[1];
        }
        velocity
    }

    async fn launch(&mut self, position: [f64; 3], shot: Shot) -> Option<i32> {
        let launched = match &shot {
            Shot::Arrow(arrow) => Some(arrow.item.clone()),
            Shot::Thrown { item, .. } | Shot::Firework { item, .. } => Some(item.clone()),
            _ => None,
        };
        if let Some(stack) = launched {
            self.item_action_hooks(
                tuff_core::world::session::hooks::ItemAction::Shot,
                stack.view(),
            );
        }
        let id = self.shared.next_entity_id();
        let owner = Owner::of(
            Culprit::player(self.id, self.profile.id, self.profile.name.clone()),
            Some(self.conn.handle.clone()),
        );
        let launch = Launch {
            id,
            position,
            owner,
            shot,
        };
        let entity = self.shared.entity_types.launch(launch, &mut self.rng)?;
        let region = ChunkPos::of_position(position).region();
        if let Some(handle) = self.region(region).await {
            handle.request(RegionRequest::SpawnEntity(entity)).await;
        }
        self.emit_game_event(
            tuff_core::world::game_event::GameEvent::ProjectileShoot,
            position,
        )
        .await;
        Some(id)
    }

    /// Letting go of a drawn bow: the longer it was drawn, the faster the
    /// arrow (a full draw is critical). Uses an arrow unless creative or
    /// Infinity makes it free, and wears the bow.
    async fn release_bow(&mut self, using: UsingItem) {
        let Some(bow) = self
            .inventory
            .hand(using.off_hand)
            .filter(|s| s.item == Item::Bow)
            .cloned()
        else {
            return;
        };
        let Some((slot, ammo)) = self.bow_ammo() else {
            return;
        };
        let power = tuff_vanilla_data::item::bow_power(using.total - using.remaining);
        if power < 0.1 {
            return;
        }
        self.progress
            .stats
            .award(Stat::item(StatType::Used, Item::Bow), 1);
        let uses = if self.mode.abilities.instabuild {
            0
        } else {
            combat::ammo_use(&bow, ammo.item, 1.0).floor().max(0.0) as u8
        };
        if uses > ammo.count {
            return;
        }
        if uses > 0
            && let Some(slot) = slot
        {
            let left = ammo.count - uses;
            self.inventory
                .set_slot(slot, (left > 0).then(|| ammo.clone().with_count(left)));
        }
        let velocity = self.shot_velocity(power * 3.0);
        let eye = self.eye();
        let position = [eye[0], eye[1] - 0.100_000_001_490_116_12, eye[2]];
        let mut arrow = ArrowShot::new(ammo.clone().with_count(1), velocity);
        arrow.weapon = Some(bow.clone());
        arrow.crit = power == 1.0;
        if uses == 0 {
            arrow.pickup = Some(Pickup::CreativeOnly);
        }
        let look = (self.pose.yaw, self.pose.pitch);
        let world = tuff_vanilla_common::enchant_effects::arrow_spawned(
            &mut arrow,
            &bow,
            position,
            look,
            &mut self.rng,
        );
        tuff_vanilla_common::enchant_effects::session_world(self, position, world).await;
        self.launch(position, Shot::Arrow(arrow)).await;
        self.hurt_hand(using.off_hand, 1).await;
        let pitch = 1.0 / (self.rng.r#gen::<f32>() * 0.4 + 1.2) + power * 0.5;
        self.player_sound(SoundEvent::EntityArrowShoot, 1.0, pitch)
            .await;
        self.send_inventory();
    }

    /// Asks the region around the player's eyes where an eye of ender
    /// would fly; the answer comes back as [`PlayerEvent::EyeTarget`].
    async fn toot_horn(&mut self, off_hand: bool) {
        let Some(stack) = self.inventory.hand(off_hand) else {
            return;
        };
        let name = stack
            .components
            .other
            .get_str("minecraft:instrument")
            .unwrap_or("minecraft:ponder_goat_horn");
        let Some(sound) = goat_horn_sound(name) else {
            return;
        };
        let ticks = (GOAT_HORN_SECONDS * 20.0) as u32;
        self.actions.using = Some(UsingItem::new(off_hand, Item::GoatHorn, ticks));
        let seed = self.rng.r#gen();
        let packet = tuff_core::world::packets([tuff_core::world::framed(&play::Sound {
            sound_event: sound.id(),
            category: play::sound_category::RECORDS,
            position: self.pose.position,
            volume: GOAT_HORN_RANGE / 16.0,
            pitch: 1.0,
            seed,
        })]);
        self.broadcast_chunk_to_others(packet).await;
        self.emit_game_event(
            tuff_core::world::game_event::GameEvent::InstrumentPlay,
            self.pose.position,
        )
        .await;
        self.add_cooldown(Item::GoatHorn, ticks);
        self.publish(false).await;
    }

    async fn locate_eye(&mut self, off_hand: bool) {
        let eye = self.eye();
        let from = BlockPos::new(
            self.pose.position[0].floor() as i32,
            self.pose.position[1].floor() as i32,
            self.pose.position[2].floor() as i32,
        );
        let Some(region) = self.region_of(BlockPos::new(
            eye[0].floor() as i32,
            eye[1].floor() as i32,
            eye[2].floor() as i32,
        )) else {
            return;
        };
        region
            .request(crate::world::region::interact::locate_eye_request(
                self.conn.handle.clone(),
                eye,
                tuff_core::world::raycast::view_vector(self.pose.yaw, self.pose.pitch),
                BLOCK_INTERACTION_RANGE,
                from,
                off_hand,
            ))
            .await;
    }

    /// Throws the eye of ender in hand toward `target`, if one is still
    /// there.
    async fn throw_eye(&mut self, off_hand: bool, target: BlockPos) {
        if self.is_dead() || self.mode.game_mode == GameMode::Spectator {
            return;
        }
        let Some(stack) = self
            .inventory
            .hand(off_hand)
            .filter(|s| s.item == Item::EnderEye)
            .cloned()
        else {
            return;
        };
        let position = [
            self.pose.position[0],
            self.pose.position[1] + self.height() * 0.5,
            self.pose.position[2],
        ];
        self.launch(
            position,
            Shot::EyeOfEnder {
                item: stack,
                target,
            },
        )
        .await;
        let (dx, dz) = (
            self.pose.position[0] - f64::from(target.x),
            self.pose.position[2] - f64::from(target.z),
        );
        self.fire(
            tuff_core::world::advancement::trigger::Event::UsedEnderEye {
                distance_sqr: dx * dx + dz * dz,
            },
        );
        let pitch = 0.33 + self.rng.r#gen::<f32>() * (0.5 - 0.33);
        let seed = self.rng.r#gen();
        let packet = packets([framed(&play::Sound {
            sound_event: SoundEvent::EntityEnderEyeLaunch.id(),
            category: play::sound_category::NEUTRAL,
            position: self.pose.position,
            volume: 1.0,
            pitch,
            seed,
        })]);
        self.broadcast_chunk(packet).await;
        if !self.mode.abilities.instabuild {
            self.inventory.take_from_hand(off_hand, 1);
        }
        self.progress
            .stats
            .award(Stat::item(StatType::Used, Item::EnderEye), 1);
        self.send_inventory();
    }

    /// Throwing a snowball, egg or ender pearl.
    async fn throw_projectile(&mut self, off_hand: bool, item: Item, kind: EntityType) {
        let Some(stack) = self.inventory.hand(off_hand).cloned() else {
            return;
        };
        let cooldown = stack.use_cooldown();
        let (sound, category) = match kind {
            EntityType::Snowball => (
                SoundEvent::EntitySnowballThrow,
                play::sound_category::NEUTRAL,
            ),
            EntityType::Egg => (SoundEvent::EntityEggThrow, play::sound_category::NEUTRAL),
            EntityType::SplashPotion => (
                SoundEvent::EntitySplashPotionThrow,
                play::sound_category::PLAYERS,
            ),
            EntityType::LingeringPotion => (
                SoundEvent::EntityLingeringPotionThrow,
                play::sound_category::NEUTRAL,
            ),
            EntityType::ExperienceBottle => (
                SoundEvent::EntityExperienceBottleThrow,
                play::sound_category::NEUTRAL,
            ),
            _ => (
                SoundEvent::EntityEnderPearlThrow,
                play::sound_category::NEUTRAL,
            ),
        };
        let pitch = 0.4 / (self.rng.r#gen::<f32>() * 0.4 + 0.8);
        let seed = self.rng.r#gen();
        let packet = packets([framed(&play::Sound {
            sound_event: sound.id(),
            category,
            position: self.pose.position,
            volume: 0.5,
            pitch,
            seed,
        })]);
        self.broadcast_chunk(packet).await;
        let velocity = match kind {
            EntityType::SplashPotion | EntityType::LingeringPotion => {
                self.shot_velocity_offset(0.5, -20.0)
            }
            EntityType::ExperienceBottle => self.shot_velocity_offset(0.7, -20.0),
            _ => self.shot_velocity(1.5),
        };
        let eye = self.eye();
        let position = [eye[0], eye[1] - 0.100_000_001_490_116_12, eye[2]];
        let shot = Shot::Thrown {
            kind,
            item: stack,
            velocity,
        };
        self.launch(position, shot).await;
        self.progress
            .stats
            .award(Stat::item(StatType::Used, item), 1);
        if !self.mode.abilities.instabuild {
            self.inventory.take_from_hand(off_hand, 1);
        }
        if let Some(cooldown) = cooldown {
            self.add_stack_cooldown(item, cooldown);
        }
        self.send_inventory();
    }

    async fn use_fishing_rod(&mut self, off_hand: bool) {
        let Some(stack) = self.inventory.hand(off_hand).cloned() else {
            return;
        };
        let pitch = 0.4 / (self.rng.r#gen::<f32>() * 0.4 + 0.8);
        let seed = self.rng.r#gen();
        if let Some(id) = self.ext.entry::<FishingBobber>().0.take() {
            let home = ChunkPos::of_position(self.pose.position).region();
            for region in std::iter::once(home).chain(home.neighbors()) {
                if let Some(handle) = self.region(region).await {
                    handle
                        .request(RegionRequest::Notify {
                            target: id,
                            notice: tuff_core::world::entity::Notice::Reel,
                        })
                        .await;
                }
            }
            let packet = packets([framed(&play::Sound {
                sound_event: SoundEvent::EntityFishingBobberRetrieve.id(),
                category: play::sound_category::NEUTRAL,
                position: self.pose.position,
                volume: 1.0,
                pitch,
                seed,
            })]);
            self.broadcast_chunk(packet).await;
        } else {
            let packet = packets([framed(&play::Sound {
                sound_event: SoundEvent::EntityFishingBobberThrow.id(),
                category: play::sound_category::NEUTRAL,
                position: self.pose.position,
                volume: 0.5,
                pitch,
                seed,
            })]);
            self.broadcast_chunk(packet).await;
            let mut eye = self.pose.position;
            eye[1] = self.eye()[1];
            let shot = Shot::Bobber {
                rotation: (self.pose.yaw, self.pose.pitch),
                rod: stack,
                luck: self.attribute(tuff_vanilla_data::registry::Attribute::Luck) as f32,
            };
            if let Some(id) = self.launch(eye, shot).await {
                self.ext.entry::<FishingBobber>().0 = Some(id);
            }
            self.progress
                .stats
                .award(Stat::item(StatType::Used, Item::FishingRod), 1);
        }
    }

    /// A reeled-in bobber wears the rod and counts its fish.
    async fn bobber_reeled(&mut self, damage: u32, fish: u32) {
        use tuff_item::item::Hurt;
        self.ext.entry::<FishingBobber>().0 = None;
        if fish > 0 {
            self.progress.stats.award_custom(
                tuff_vanilla_data::registry::CustomStat::FishCaught,
                fish as i32,
            );
        }
        if damage == 0 || self.mode.abilities.instabuild {
            return;
        }
        let main = self
            .inventory
            .hand(false)
            .is_some_and(|s| s.item == Item::FishingRod);
        if main {
            self.wear_slot(EquipmentSlot::Mainhand, damage).await;
        } else if self
            .inventory
            .hand(true)
            .is_some_and(|s| s.item == Item::FishingRod)
        {
            match self.hurt_slot_reporting(Inventory::OFFHAND_SLOT, damage) {
                Hurt::Unchanged => {}
                _ => self.send_inventory(),
            }
        }
    }

    async fn throw_wind_charge(&mut self, off_hand: bool) {
        if self.inventory.hand(off_hand).is_none() {
            return;
        }
        let velocity = self.shot_velocity(WIND_CHARGE_THROW_POWER);
        let position = [self.pose.position[0], self.eye()[1], self.pose.position[2]];
        let shot = Shot::WindCharge {
            kind: EntityType::WindCharge,
            velocity,
        };
        self.launch(position, shot).await;
        let pitch = 0.4 / (self.rng.r#gen::<f32>() * 0.4 + 0.8);
        let seed = self.rng.r#gen();
        let packet = packets([framed(&play::Sound {
            sound_event: SoundEvent::EntityWindChargeThrow.id(),
            category: play::sound_category::NEUTRAL,
            position: self.pose.position,
            volume: 0.5,
            pitch,
            seed,
        })]);
        self.broadcast_chunk(packet).await;
        self.progress
            .stats
            .award(Stat::item(StatType::Used, Item::WindCharge), 1);
        if !self.mode.abilities.instabuild {
            self.inventory.take_from_hand(off_hand, 1);
        }
        if let Some(cooldown) = Item::WindCharge.use_cooldown() {
            self.add_cooldown(Item::WindCharge, cooldown.ticks());
        }
        self.send_inventory();
    }

    /// The eating or drinking sound of `item`, with vanilla's volume and
    /// pitch jitter. `finishing` uses the completion sound's parameters.
    fn consume_sound(&mut self, consumable: &StackConsumable) -> Packets {
        let (volume, pitch) = match consumable.animation {
            UseAnimation::Drink => (0.5, self.rng.gen_range(0.9..=1.0)),
            _ => (
                if self.rng.r#gen::<bool>() { 0.5 } else { 1.0 },
                1.0 + 0.2 * (self.rng.r#gen::<f32>() - self.rng.r#gen::<f32>()),
            ),
        };
        let seed = self.rng.r#gen();
        packets([framed(&play::Sound {
            sound_event: consumable.sound.id(),
            category: play::sound_category::PLAYERS,
            position: self.pose.position,
            volume,
            pitch,
            seed,
        })])
    }

    async fn tick_using(&mut self) {
        let Some(mut using) = self.actions.using else {
            return;
        };
        if self.is_dead() || self.inventory.hand(using.off_hand).map(|s| s.item) != Some(using.item)
        {
            self.stop_using();
            self.publish(false).await;
            return;
        }
        if using.item == Item::Brush {
            super::brush::BrushExt::tick_brush(self, using).await;
            return;
        }
        if using.item == Item::Spyglass {
            let finished = using.tick();
            self.actions.using = (!finished).then_some(using);
            if finished {
                self.player_sound_to_others(SoundEvent::ItemSpyglassStopUsing, 1.0, 1.0)
                    .await;
                self.publish(false).await;
            }
            return;
        }
        if self
            .inventory
            .hand(using.off_hand)
            .is_some_and(ItemStack::is_bundle)
        {
            self.drop_bundle_content(using).await;
            let finished = using.tick();
            self.actions.using = (!finished).then_some(using);
            if finished {
                self.publish(false).await;
            }
            return;
        }
        if using.emits_effects()
            && let Some(consumable) = self
                .inventory
                .hand(using.off_hand)
                .and_then(ItemStack::consumable)
        {
            let sound = self.consume_sound(&consumable);
            self.broadcast_chunk_to_others(sound).await;
        }
        if using.item == Item::Crossbow {
            super::ranged::RangedExt::tick_crossbow(self, using).await;
        }
        let finished = using.tick();
        self.actions.using = Some(using);
        if finished {
            self.send(&play::EntityEvent {
                entity_id: self.id,
                status: play::entity_event::USE_ITEM_COMPLETE,
            });
            self.actions.using = None;
            if self
                .inventory
                .hand(using.off_hand)
                .is_none_or(|s| s.use_effects().interact_vibrations)
            {
                self.emit_game_event(
                    tuff_core::world::game_event::GameEvent::ItemInteractFinish,
                    self.pose.position,
                )
                .await;
            }
            self.consume(using.off_hand, using.item).await;
            self.publish(false).await;
        }
    }

    async fn drop_bundle_content(&mut self, using: UsingItem) {
        let first = using.remaining == using.total;
        let repeat = using.remaining + BUNDLE_FIRST_THROW_TICKS < using.total
            && using.remaining.is_multiple_of(BUNDLE_THROW_INTERVAL);
        if !(first || repeat) {
            return;
        }
        let Some(removed) = self
            .inventory
            .hand_mut(using.off_hand)
            .and_then(|stack| stack.edit_bundle(|contents| contents.remove_one()))
        else {
            return;
        };
        let pitch = 0.8 + self.rng.r#gen::<f32>() * 0.4;
        self.player_sound_to_others(SoundEvent::ItemBundleRemoveOne, 0.8, pitch)
            .await;
        let pitch = 0.8 + self.rng.r#gen::<f32>() * 0.4;
        self.player_sound(SoundEvent::ItemBundleDropContents, 0.8, pitch)
            .await;
        self.progress
            .stats
            .award(Stat::item(StatType::Used, using.item), 1);
        self.send_inventory();
        let spread = self.rng.r#gen::<f64>() * 0.5;
        let angle = self.rng.r#gen::<f64>() * std::f64::consts::TAU;
        let eye = self.eye();
        self.spawn_item(
            removed,
            [eye[0], eye[1] - 0.3, eye[2]],
            [-angle.sin() * spread, 0.2, angle.cos() * spread],
            tuff_core::world::player::THROWN_PICKUP_DELAY,
        )
        .await;
    }

    /// Finishes eating or drinking: sounds, food, effects, and the item's
    /// remainder, as `Consumable.onConsume` does.
    async fn consume(&mut self, off_hand: bool, item: Item) {
        let Some(consumable) = self
            .inventory
            .hand(off_hand)
            .and_then(ItemStack::consumable)
        else {
            return;
        };
        let potion = self
            .inventory
            .hand(off_hand)
            .and_then(|stack| stack.components.potion_contents.clone());
        let stew_effects = self
            .inventory
            .hand(off_hand)
            .and_then(|stack| stack.components.suspicious_stew_effects.clone());
        let consumed = self.inventory.hand(off_hand).cloned();
        if let Some(stack) = &consumed {
            self.fire(tuff_core::world::advancement::trigger::Event::ConsumeItem(
                stack,
            ));
        }
        let omen = (item == Item::OminousBottle).then(|| {
            self.inventory
                .hand(off_hand)
                .and_then(|stack| stack.ominous_bottle_amplifier())
                .unwrap_or(0)
        });
        let sound = self.consume_sound(&consumable);
        self.broadcast_chunk_to_others(sound).await;
        let event = if consumable.animation == tuff_vanilla_data::item::UseAnimation::Drink {
            tuff_core::world::game_event::GameEvent::Drink
        } else {
            tuff_core::world::game_event::GameEvent::Eat
        };
        self.emit_game_event(event, self.pose.position).await;
        if let Some(amplifier) = omen {
            let mut bad_omen = ActiveEffect::timed(
                MobEffect::BadOmen,
                OMINOUS_BOTTLE_OMEN_TICKS,
                amplifier.min(4) as u8,
            );
            bad_omen.show_particles = false;
            self.add_effect(bad_omen).await;
        }
        if let Some(food) = item.food() {
            let seed = self.rng.r#gen();
            let pitch = 1.0 + 0.4 * (self.rng.r#gen::<f32>() - self.rng.r#gen::<f32>());
            let burp_pitch = self.rng.gen_range(0.9..=1.0);
            let burp_seed = self.rng.r#gen();
            let sounds = packets([
                framed(&play::Sound {
                    sound_event: consumable.sound.id(),
                    category: play::sound_category::NEUTRAL,
                    position: self.pose.position,
                    volume: 1.0,
                    pitch,
                    seed,
                }),
                framed(&play::Sound {
                    sound_event: SoundEvent::EntityPlayerBurp.id(),
                    category: play::sound_category::PLAYERS,
                    position: self.pose.position,
                    volume: 0.5,
                    pitch: burp_pitch,
                    seed: burp_seed,
                }),
            ]);
            self.body.food.eat(food.nutrition as i32, food.saturation);
            self.broadcast_chunk(sounds).await;
        }
        if let Some(contents) = potion {
            for effect in effects::potion_effects(&contents, item.potion_duration_scale()) {
                self.add_effect(effect).await;
            }
        }
        if let Some(effects) = stew_effects {
            for entry in effects {
                self.add_effect(ActiveEffect::timed(entry.effect, entry.duration as i64, 0))
                    .await;
            }
        }
        self.progress
            .stats
            .award(Stat::item(StatType::Used, item), 1);
        for effect in &consumable.on_consume_effects {
            self.apply_consume_effect(effect).await;
        }
        if !self.mode.abilities.instabuild {
            let before = self.inventory.hand(off_hand).map_or(0, |s| s.count);
            self.inventory.take_from_hand(off_hand, 1);
            if before > 0
                && let Some(remainder) = consumed.as_ref().and_then(ItemStack::use_remainder)
            {
                if self.inventory.hand(off_hand).is_none() {
                    let slot = if off_hand {
                        Inventory::OFFHAND_SLOT
                    } else {
                        Inventory::hotbar_slot(self.inventory.selected())
                    };
                    self.inventory.set_slot(slot, Some(remainder));
                } else if let Some(extra) = self.inventory.add(remainder) {
                    self.throw_item(extra).await;
                }
            }
        }
        if let Some(cooldown) = consumed.as_ref().and_then(ItemStack::use_cooldown) {
            self.add_stack_cooldown(item, cooldown);
        }
        self.send_inventory();
    }

    async fn apply_consume_effect(&mut self, effect: &StackConsumeEffect) {
        match effect {
            StackConsumeEffect::ApplyEffects {
                effects,
                probability,
            } => {
                if self.rng.r#gen::<f32>() < *probability {
                    for instance in effects {
                        self.add_effect(ActiveEffect::new(instance)).await;
                    }
                }
            }
            StackConsumeEffect::RemoveEffects(kinds) => {
                for kind in kinds {
                    let change = self.body.effects.remove(*kind);
                    self.apply_effect_change(change).await;
                }
            }
            StackConsumeEffect::ClearAllEffects => {
                for change in self.body.effects.clear() {
                    self.apply_effect_change(Some(change)).await;
                }
            }
            StackConsumeEffect::TeleportRandomly {
                diameter,
                directional_particles,
            } => {
                self.teleport_randomly(*diameter, *directional_particles)
                    .await;
            }
            StackConsumeEffect::PlaySound(sound) => {
                let seed = self.rng.r#gen();
                let sound = packets([framed(&play::Sound {
                    sound_event: sound.id(),
                    category: play::sound_category::PLAYERS,
                    position: self.pose.position,
                    volume: 1.0,
                    pitch: 1.0,
                    seed,
                })]);
                self.broadcast_chunk(sound).await;
            }
        }
    }

    /// A splash potion or cloud: its instant effects at their strength,
    /// then the lasting ones.
    async fn take_potion(&mut self, hit: tuff_core::world::effects::PotionHit) {
        if self.is_dead() {
            return;
        }
        if hit.extinguish {
            self.extinguish().await;
        }
        for (effect, amplifier, scale) in hit.instant {
            if let Some(action) = effects::instant_action(effect, amplifier, scale) {
                self.apply_effect_action(action).await;
            }
        }
        for effect in hit.effects {
            self.add_effect(effect).await;
        }
    }
}

/// Held item use as session hooks.
pub struct ItemUseHooks;

impl SessionHooks for ItemUseHooks {
    fn tick<'a>(&'a self, session: &'a mut PlayerSession, phase: SessionTick) -> HookFuture<'a> {
        Box::pin(async move {
            if phase == SessionTick::Use {
                session.tick_using().await;
            }
        })
    }

    fn use_item<'a>(&'a self, session: &'a mut PlayerSession, off_hand: bool) -> HookFuture<'a> {
        Box::pin(session.use_item(off_hand))
    }

    fn event<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        event: &'a mut Option<PlayerEvent>,
    ) -> HookFuture<'a> {
        Box::pin(async move {
            match event.take() {
                Some(PlayerEvent::Potion(hit)) => session.take_potion(*hit).await,
                Some(PlayerEvent::Reeled { damage, fish }) => {
                    session.bobber_reeled(damage, fish).await;
                }
                Some(PlayerEvent::BobberGone(id)) => {
                    if session.ext.entry::<FishingBobber>().0 == Some(id) {
                        session.ext.entry::<FishingBobber>().0 = None;
                    }
                }
                other => *event = other,
            }
        })
    }

    fn packet<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        packet: &'a mut Option<Serverbound>,
    ) -> HookFuture<'a> {
        Box::pin(async move {
            match packet.take() {
                Some(Serverbound::PlayerAction {
                    status: PlayerActionStatus::ReleaseUseItem,
                    sequence,
                    ..
                }) => {
                    let using = session.actions.using;
                    session.stop_using();
                    match using {
                        Some(using) if using.item == Item::Bow => {
                            session.release_bow(using).await;
                        }
                        Some(using) if using.item == Item::Trident => {
                            super::ranged::RangedExt::release_trident(session, using).await;
                        }
                        Some(using) if using.item == Item::Spyglass => {
                            session
                                .player_sound_to_others(SoundEvent::ItemSpyglassStopUsing, 1.0, 1.0)
                                .await;
                            session.publish(false).await;
                        }
                        _ => {}
                    }
                    session.send(&play::BlockChangedAck(sequence));
                }
                other => *packet = other,
            }
        })
    }
}

/// The id of this player's fishing bobber in the world, if cast.
#[derive(Default)]
pub struct FishingBobber(pub Option<i32>);

const SPYGLASS_USE_TICKS: u32 = 1200;
const GOAT_HORN_SECONDS: f32 = 7.0;
const GOAT_HORN_RANGE: f32 = 256.0;

fn goat_horn_sound(instrument: &str) -> Option<SoundEvent> {
    Some(match instrument.strip_prefix("minecraft:")? {
        "ponder_goat_horn" => SoundEvent::ItemGoatHornSound0,
        "sing_goat_horn" => SoundEvent::ItemGoatHornSound1,
        "seek_goat_horn" => SoundEvent::ItemGoatHornSound2,
        "feel_goat_horn" => SoundEvent::ItemGoatHornSound3,
        "admire_goat_horn" => SoundEvent::ItemGoatHornSound4,
        "call_goat_horn" => SoundEvent::ItemGoatHornSound5,
        "yearn_goat_horn" => SoundEvent::ItemGoatHornSound6,
        "dream_goat_horn" => SoundEvent::ItemGoatHornSound7,
        _ => return None,
    })
}
