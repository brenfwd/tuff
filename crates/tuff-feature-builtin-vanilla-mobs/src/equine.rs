//! The horse family: Horse, Donkey, Mule, Skeleton Horse and Zombie Horse,
//! sharing [`Equine`] with Llama (`llama.rs`): mount-and-wait taming,
//! feeding, randomly rolled attributes (and their breeding average), the
//! saddle/armor/chest inventory screen, rearing, grazing and the charged
//! ridden jump.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};
use tuff_vanilla_data::{Block, Item, tag};

use super::goal::{Flags, Goal, GoalSelector};
use super::goals::{
    Breed, FollowParent, LookAtPlayer, Panic, RandomLookAround, RandomStroll, Tempt,
};
use super::{Mob, MobCore, MobSounds, Species, animal, data, event};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::block_entity::{decode_items, encode_items};
use tuff_core::world::entity::{
    Effect, EntityInteraction, InteractOutcome, RegionEntity, RiderInput, TickContext, take_bool,
    take_number,
};
use tuff_core::world::menu::{MenuKind, MountArmor};
use tuff_core::world::messages::HandEffect;
use tuff_item::item::{ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

/// The single flags byte every abstract horse sends at `data::SPECIES`.
pub const FLAGS: u8 = data::SPECIES;
const TAME_BIT: i8 = 0x02;
const BRED_BIT: i8 = 0x08;
const EATING_BIT: i8 = 0x10;
const STANDING_BIT: i8 = 0x20;
const MOUTH_BIT: i8 = 0x40;

pub const MAX_TEMPER: i32 = 100;

pub const MIN_MAX_HEALTH: f64 = 15.0;
pub const MAX_MAX_HEALTH: f64 = 30.0;
pub const MIN_SPEED: f64 = 0.1125;
pub const MAX_SPEED: f64 = 0.3375;
pub const MIN_JUMP: f64 = 0.4;
const LEAP: f32 = 0.4;
pub const MAX_JUMP: f64 = 1.0;

pub const CHEST_COLUMNS: u8 = 5;
const STAND_TICKS: i32 = 20;
const STAND_INTERVAL: i32 = 400;
const GRAZE_TICKS: i32 = 50;
const MOUTH_TICKS: i32 = 30;

pub fn roll_health(rng: &mut StdRng) -> f64 {
    MIN_MAX_HEALTH + rng.gen_range(0..8) as f64 + rng.gen_range(0..9) as f64
}

pub fn roll_speed(rng: &mut StdRng) -> f64 {
    (0.449_999_988_079_071
        + rng.r#gen::<f64>() * 0.3
        + rng.r#gen::<f64>() * 0.3
        + rng.r#gen::<f64>() * 0.3)
        * 0.25
}

pub fn roll_jump(rng: &mut StdRng) -> f64 {
    0.400_000_005_960_464_5
        + rng.r#gen::<f64>() * 0.2
        + rng.r#gen::<f64>() * 0.2
        + rng.r#gen::<f64>() * 0.2
}

fn roll_zombie_speed(rng: &mut StdRng) -> f64 {
    (9.0 + rng.r#gen::<f64>() + rng.r#gen::<f64>() + rng.r#gen::<f64>()) / 42.159_999_847_412_11
}

fn roll_zombie_jump(rng: &mut StdRng) -> f64 {
    0.5 + (rng.r#gen::<f64>() + rng.r#gen::<f64>() + rng.r#gen::<f64>()) / 15.0
}

/// The offspring attribute average: clamps both parents into range, then
/// averages with a random skew (mean 0) scaled by their spread plus a
/// fixed margin, reflecting back in if it overshoots.
pub fn offspring_attribute(a: f64, b: f64, min: f64, max: f64, rng: &mut StdRng) -> f64 {
    let a = a.clamp(min, max);
    let b = b.clamp(min, max);
    let margin = 0.15 * (max - min);
    let range = (a - b).abs() + margin * 2.0;
    let average = (a + b) / 2.0;
    let quality = (rng.r#gen::<f64>() + rng.r#gen::<f64>() + rng.r#gen::<f64>()) / 3.0 - 0.5;
    let value = average + range * quality;
    if value > max {
        max - (value - max)
    } else if value < min {
        min + (min - value)
    } else {
        value
    }
}

/// Sets a baby's health, jump strength and speed from its parents' bases.
pub fn inherit_attributes(baby: &mut MobCore, a: &MobCore, b: &MobCore, rng: &mut StdRng) {
    for (attribute, min, max) in [
        (Attribute::MaxHealth, MIN_MAX_HEALTH, MAX_MAX_HEALTH),
        (Attribute::JumpStrength, MIN_JUMP, MAX_JUMP),
        (Attribute::MovementSpeed, MIN_SPEED, MAX_SPEED),
    ] {
        let value = offspring_attribute(
            a.attribute_base(attribute),
            b.attribute_base(attribute),
            min,
            max,
            rng,
        );
        baby.set_attribute_base(attribute, value);
    }
    baby.health = baby.max_health();
}

/// Scales a driving rider's raw input the way every abstract horse does:
/// backing up is a quarter speed, strafing is halved.
pub fn drive_input(input: RiderInput) -> (f32, f32) {
    let mut forward = input.forward;
    if forward <= 0.0 {
        forward *= 0.25;
    }
    (forward, input.strafe * 0.5)
}

pub fn is_tame(core: &MobCore) -> bool {
    core.tame.as_ref().is_some_and(|t| t.is_tamed())
}

pub fn saddled(core: &MobCore) -> bool {
    core.equipment.get(EquipmentSlot::Saddle).is_some()
}

/// A mount's ridden top speed: its movement speed, and a jump's forward
/// push at full charge.
pub fn ridden_speed(core: &MobCore) -> tuff_core::world::entity::RiddenSpeed {
    tuff_core::world::entity::RiddenSpeed {
        speed: core.attribute(Attribute::MovementSpeed),
        leap: f64::from(LEAP),
        launch: core.attribute(Attribute::JumpStrength)
            + core
                .effects
                .amplifier(tuff_vanilla_data::registry::MobEffect::JumpBoost)
                .map_or(0.0, |a| 0.1 * (f64::from(a) + 1.0)),
        levitation: core
            .effects
            .amplifier(tuff_vanilla_data::registry::MobEffect::Levitation)
            .map_or(0, |a| a.saturating_add(1)),
        slow_falling: core
            .effects
            .amplifier(tuff_vanilla_data::registry::MobEffect::SlowFalling)
            .is_some(),
    }
}

fn ridden(core: &MobCore) -> bool {
    !core.base.riders.is_empty() || !core.base.entity_riders.is_empty()
}

/// A click with a saddle on a mount that needs no taming (camel): it goes
/// on if the slot is empty and the mount is grown, with the mount's own
/// `sound` if it has one, else the saddle's.
pub fn saddle_up(
    core: &mut MobCore,
    stack: &ItemStack,
    sound: Option<SoundEvent>,
    cx: &mut TickContext,
) -> InteractOutcome {
    if core.is_baby() || saddled(core) {
        return None;
    }
    let eq = stack.equippable()?;
    if !eq.equip_on_interact || !matches!(eq.slot, EquipmentSlot::Saddle) {
        return None;
    }
    core.equipment
        .set(EquipmentSlot::Saddle, Some(ItemStack::mint_like(stack, 1, MintReason::Transform)));
    core.sound(sound.unwrap_or(eq.equip_sound), 1.0, 1.0, cx);
    Some(HandEffect::Consume(1))
}

/// Sneak-click equipping on a tamed adult (a nautilus): a saddle, and
/// with `allow_body` body armor, into an empty slot.
pub fn try_equip(
    core: &mut MobCore,
    stack: &ItemStack,
    allow_body: bool,
    cx: &mut TickContext,
) -> InteractOutcome {
    if !is_tame(core) || core.is_baby() {
        return None;
    }
    let eq = stack.equippable()?;
    if !eq.equip_on_interact
        || !(eq.slot == EquipmentSlot::Saddle || (allow_body && eq.slot == EquipmentSlot::Body))
        || core.equipment.get(eq.slot).is_some()
    {
        return None;
    }
    core.equipment
        .set(eq.slot, Some(ItemStack::mint_like(stack, 1, MintReason::Transform)));
    core.sound(eq.equip_sound, 1.0, 1.0, cx);
    Some(HandEffect::Consume(1))
}

/// How a food item helps a horse: health, seconds of growing up, temper,
/// and whether it makes a tamed adult fall in love.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Feed {
    pub heal: f32,
    pub age_up: i32,
    pub temper: i32,
    pub love: bool,
}

pub const fn feed(heal: f32, age_up: i32, temper: i32, love: bool) -> Option<Feed> {
    Some(Feed {
        heal,
        age_up,
        temper,
        love,
    })
}

pub fn horse_feed(item: Item) -> Option<Feed> {
    match item {
        Item::Wheat => feed(2.0, 20, 3, false),
        Item::Sugar => feed(1.0, 30, 3, false),
        Item::HayBlock => feed(20.0, 180, 0, false),
        Item::Apple | Item::Carrot => feed(3.0, 60, 3, false),
        Item::RedMushroom => feed(3.0, 0, 3, false),
        Item::GoldenCarrot => feed(4.0, 60, 5, true),
        Item::GoldenApple | Item::EnchantedGoldenApple => feed(10.0, 240, 10, true),
        _ => None,
    }
}

/// The per-species knobs of the shared horse behaviour.
#[derive(Debug, Clone, Copy)]
pub struct Profile {
    pub max_temper: i32,
    pub rears: bool,
    pub grazes: bool,
    pub saddles: bool,
    pub armor: MountArmor,
    pub chested: bool,
    /// Whether a click with food feeds it (skeleton horses never eat).
    pub fed_by_hand: bool,
    pub food: fn(Item) -> bool,
    pub feed: fn(Item) -> Option<Feed>,
    /// Whether eating opens its mouth and sends the eat game event.
    pub eats_visibly: bool,
    pub angry: Option<SoundEvent>,
    pub eat: Option<SoundEvent>,
    pub chest_sound: SoundEvent,
    /// Only tamed ones take a rider or a click (skeleton horses).
    pub tame_to_touch: bool,
}

impl Profile {
    pub const fn horse_like(food: fn(Item) -> bool) -> Self {
        Self {
            max_temper: MAX_TEMPER,
            rears: true,
            grazes: true,
            saddles: true,
            armor: MountArmor::None,
            chested: false,
            fed_by_hand: true,
            food,
            feed: horse_feed,
            eats_visibly: true,
            angry: None,
            eat: None,
            chest_sound: SoundEvent::EntityDonkeyChest,
            tame_to_touch: false,
        }
    }
}

/// A mount's chest, behind the saddle and body slots its screen shows,
/// which are the mob's own equipment (never a second copy of it).
#[derive(tuff_item::item::DupClone, Debug, Default)]
pub struct MountInventory {
    chest: Vec<Option<ItemStack>>,
    worn: [Option<Item>; 2],
    screen_touched: bool,
}

const WORN: [EquipmentSlot; 2] = [EquipmentSlot::Saddle, EquipmentSlot::Body];

impl MountInventory {
    pub fn chest(&self) -> &[Option<ItemStack>] {
        &self.chest
    }

    /// The chest to change from the screen, which also may change the
    /// worn slots: the next [`Self::sync`] reacts to what was put on.
    pub fn chest_from_screen(&mut self) -> &mut [Option<ItemStack>] {
        self.screen_touched = true;
        &mut self.chest
    }

    pub fn columns(&self) -> u8 {
        (self.chest.len() / 3) as u8
    }

    /// Resizes the chest to `columns`, keeping what still fits.
    pub fn resize(&mut self, columns: u8) {
        self.chest.resize_with(columns as usize * 3, || None);
    }

    /// Empties the chest, returning what it held.
    pub fn take_chest(&mut self) -> Vec<ItemStack> {
        self.chest.iter_mut().filter_map(Option::take).collect()
    }

    pub fn load_chest(&mut self, nbt: &mut Compound) {
        let Some(Tag::List(entries)) = nbt.remove("Items") else {
            return;
        };
        let size = self.chest.len();
        for (slot, stack) in decode_items(&entries, size).into_iter().enumerate() {
            self.chest[slot] = stack;
        }
    }

    pub fn save_chest(&self, out: &mut Compound) {
        out.insert("Items", encode_items(self.chest()));
    }

    /// Reacts to what the screen put on since the last sync: it is
    /// guaranteed to drop and plays its equip sound.
    pub fn sync(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let touched = std::mem::take(&mut self.screen_touched);
        for (index, slot) in WORN.into_iter().enumerate() {
            let worn = core.equipment.get(slot).map(|stack| stack.item);
            if touched && worn.is_some() && worn != self.worn[index] {
                core.equipment
                    .set_drop_chance(slot, super::equipment::PRESERVED_DROP_CHANCE);
                core.persistence_required = true;
                if let Some(sound) = core.equipment.get(slot).and_then(|s| equip_sound(slot, s)) {
                    core.sound(sound, 1.0, 1.0, cx);
                }
            }
            self.worn[index] = worn;
        }
    }

    /// Takes the equipment as it is, without reacting to it.
    pub fn adopt(&mut self, core: &MobCore) {
        self.screen_touched = false;
        for (index, slot) in WORN.into_iter().enumerate() {
            self.worn[index] = core.equipment.get(slot).map(|stack| stack.item);
        }
    }
}

fn equip_sound(slot: EquipmentSlot, stack: &ItemStack) -> Option<SoundEvent> {
    if slot == EquipmentSlot::Saddle {
        return Some(SoundEvent::EntityHorseSaddle);
    }
    stack.equippable().map(|eq| eq.equip_sound)
}

/// The state every abstract horse shares.
#[derive(Debug, Clone, Default)]
pub struct Equine {
    pub temper: i32,
    pub bred: bool,
    pub has_chest: bool,
    pub eating: bool,
    standing: bool,
    mouth_open: bool,
    eating_counter: i32,
    mouth_counter: i32,
    stand_counter: i32,
    stand_ticks: i32,
    jump_pending: f32,
    pub inventory: MountInventory,
}

impl Equine {
    pub fn bred() -> Self {
        Self {
            bred: true,
            ..Self::default()
        }
    }

    pub fn flags(&self, core: &MobCore) -> i8 {
        let bit = |on: bool, bit: i8| if on { bit } else { 0 };
        bit(is_tame(core), TAME_BIT)
            | bit(self.bred, BRED_BIT)
            | bit(self.eating, EATING_BIT)
            | bit(self.standing, STANDING_BIT)
            | bit(self.mouth_open, MOUTH_BIT)
    }

    pub fn is_standing(&self) -> bool {
        self.standing
    }

    pub fn load(&mut self, core: &mut MobCore, nbt: &mut Compound, profile: &Profile) {
        self.temper = take_number(nbt, "Temper").map_or(0, |v| v as i32);
        self.bred = take_bool(nbt, "Bred").unwrap_or(false);
        self.eating = take_bool(nbt, "EatingHaystack").unwrap_or(false);
        if take_bool(nbt, "Tame").unwrap_or(false)
            && let Some(tame) = &mut core.tame
        {
            tame.ownerless = tame.owner.is_none();
        }
        if profile.chested {
            self.has_chest = take_bool(nbt, "ChestedHorse").unwrap_or(false);
        }
        self.inventory.adopt(core);
    }

    /// Sizes the chest to `columns` and loads its contents.
    pub fn load_chest(&mut self, columns: u8, nbt: &mut Compound) {
        self.inventory.resize(columns);
        if self.has_chest {
            self.inventory.load_chest(nbt);
        }
    }

    pub fn save(&self, core: &MobCore, out: &mut Compound, profile: &Profile) {
        out.insert("EatingHaystack", self.eating);
        out.insert("Bred", self.bred);
        out.insert("Temper", self.temper);
        out.insert("Tame", is_tame(core));
        if profile.chested {
            out.insert("ChestedHorse", self.has_chest);
            if self.has_chest {
                self.inventory.save_chest(out);
            }
        }
    }

    pub fn menu(&self, profile: &Profile) -> MenuKind {
        MenuKind::Mount {
            columns: self.inventory.columns(),
            saddle: profile.saddles,
            armor: profile.armor,
        }
    }

    /// Whether `player` opens the inventory: a tamed adult, not ridden by
    /// someone else, on a sneak-click or the riding inventory key.
    pub fn opens_for(core: &MobCore, player: i32, sneaking: bool) -> bool {
        sneaking
            && is_tame(core)
            && !core.is_baby()
            && (!ridden(core) || core.base.riders.iter().any(|rider| rider.id == player))
    }

    fn set_flag_state(&mut self, core: &mut MobCore, apply: impl FnOnce(&mut Self)) {
        let before = self.flags(core);
        apply(self);
        if self.flags(core) != before {
            core.changed(FLAGS);
        }
    }

    pub fn stand_if_possible(&mut self, core: &mut MobCore, profile: &Profile) {
        if profile.rears {
            self.set_flag_state(core, |state| {
                state.eating = false;
                state.standing = true;
                state.stand_counter = STAND_TICKS;
            });
        }
    }

    /// Rears up and complains (a click it refuses, a failed taming).
    pub fn make_mad(&mut self, core: &mut MobCore, profile: &Profile, cx: &mut TickContext) {
        if self.standing {
            return;
        }
        self.stand_if_possible(core, profile);
        if let Some(sound) = profile.angry {
            let pitch = core.voice_pitch(cx.rng);
            core.sound(sound, 0.8, pitch, cx);
        }
    }

    fn eat(&mut self, core: &mut MobCore, profile: &Profile, cx: &mut TickContext) {
        if profile.eats_visibly {
            self.set_flag_state(core, |state| {
                state.mouth_open = true;
                state.mouth_counter = 1;
            });
        }
        if let Some(sound) = profile.eat {
            let pitch = 1.0 + (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2;
            core.sound(sound, 1.0, pitch, cx);
        }
        if profile.eats_visibly {
            cx.effects.push(Effect::GameEvent {
                event: tuff_core::world::game_event::GameEvent::Eat,
                position: core.base.position,
                source: Some(core.base.id),
            });
        }
    }

    /// A player feeds it `item`: true when it ate (the item is used up).
    pub fn feed(
        &mut self,
        core: &mut MobCore,
        profile: &Profile,
        item: Item,
        player: u128,
        cx: &mut TickContext,
    ) -> bool {
        let Some(feed) = (profile.feed)(item) else {
            return false;
        };
        let tamed = is_tame(core);
        let mut used = false;
        let age = core.age.as_ref().map_or(0, |a| a.age);
        if feed.love && tamed && age == 0 && !core.love.as_ref().is_some_and(|l| l.is_in_love()) {
            used = true;
            core.set_in_love(Some(player), cx);
        }
        if core.health < core.max_health() && feed.heal > 0.0 {
            core.heal(feed.heal);
            used = true;
        }
        if core.is_baby()
            && feed.age_up > 0
            && core.age.as_ref().is_some_and(animal::Age::can_age_up)
        {
            core.age_up(feed.age_up, false);
            used = true;
        }
        if feed.temper > 0 && (used || !tamed) && self.temper < profile.max_temper {
            self.temper = (self.temper + feed.temper).clamp(0, profile.max_temper);
            used = true;
        }
        if used {
            self.eat(core, profile, cx);
        }
        used
    }

    fn can_equip_saddle(&self, core: &MobCore, profile: &Profile, stack: &ItemStack) -> bool {
        profile.saddles
            && is_tame(core)
            && !core.is_baby()
            && !saddled(core)
            && stack
                .item
                .equippable()
                .is_some_and(|eq| eq.equip_on_interact && eq.slot == EquipmentSlot::Saddle)
    }

    fn can_equip_armor(&self, core: &MobCore, profile: &Profile, stack: &ItemStack) -> bool {
        profile.armor.fits(stack) && core.equipment.get(EquipmentSlot::Body).is_none()
    }

    fn can_equip_chest(&self, core: &MobCore, profile: &Profile, stack: &ItemStack) -> bool {
        profile.chested && !self.has_chest && stack.item == Item::Chest && is_tame(core)
    }

    fn equip(&mut self, core: &mut MobCore, slot: EquipmentSlot, stack: &ItemStack) {
        core.equipment.set(slot, Some(ItemStack::mint_like(stack, 1, MintReason::Transform)));
        core.equipment
            .set_drop_chance(slot, super::equipment::PRESERVED_DROP_CHANCE);
        core.persistence_required = true;
        self.inventory.adopt(core);
    }

    fn equip_chest(&mut self, core: &mut MobCore, columns: u8) {
        self.has_chest = true;
        self.inventory.resize(columns);
        core.changed(FLAGS + 1);
    }

    /// Whether a plain click holding `stack` does something other than
    /// getting on.
    pub fn refuses_mount(
        &self,
        core: &MobCore,
        profile: &Profile,
        stack: Option<&ItemStack>,
    ) -> bool {
        if profile.tame_to_touch && !is_tame(core) {
            return true;
        }
        let Some(stack) = stack else {
            return false;
        };
        if core.is_baby() || (profile.fed_by_hand && (profile.food)(stack.item)) || !is_tame(core) {
            return true;
        }
        self.can_equip_chest(core, profile, stack)
            || self.can_equip_saddle(core, profile, stack)
            || self.can_equip_armor(core, profile, stack)
            || (stack.item == Item::NameTag && stack.components.custom_name.is_some())
    }

    /// A click that did not mount it. `None` leaves it to the shared
    /// animal handling (age locking, name tags). `columns` is the chest a
    /// fitted chest gives.
    pub fn interact(
        &mut self,
        core: &mut MobCore,
        profile: &Profile,
        columns: u8,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        self.inventory.sync(core, cx);
        let tamed = is_tame(core);
        if profile.tame_to_touch && !tamed {
            return None;
        }
        let stack = interaction.stack.as_ref()?;
        let ridden = ridden(core);
        let baby = core.is_baby();
        let dandelion = baby && stack.item == Item::GoldenDandelion;
        let opens = tamed && !baby && interaction.sneaking;
        if !profile.tame_to_touch && !ridden && !dandelion && !opens {
            if profile.fed_by_hand && (profile.food)(stack.item) {
                return self
                    .feed(core, profile, stack.item, interaction.player_uuid, cx)
                    .then_some(HandEffect::Consume(1));
            }
            if !tamed {
                if matches!(stack.item, Item::NameTag | Item::Lead) {
                    return None;
                }
                self.make_mad(core, profile, cx);
                return Some(HandEffect::None);
            }
            if self.can_equip_chest(core, profile, stack) {
                self.equip_chest(core, columns);
                let pitch = 1.0 + (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2;
                core.sound(profile.chest_sound, 1.0, pitch, cx);
                return Some(HandEffect::Consume(1));
            }
        }
        if ridden || baby || opens {
            return None;
        }
        if self.can_equip_saddle(core, profile, stack) {
            self.equip(core, EquipmentSlot::Saddle, stack);
            core.sound(SoundEvent::EntityHorseSaddle, 1.0, 1.0, cx);
            return Some(HandEffect::Consume(1));
        }
        if self.can_equip_armor(core, profile, stack) {
            self.equip(core, EquipmentSlot::Body, stack);
            if let Some(eq) = stack.equippable() {
                core.sound(eq.equip_sound, 1.0, 1.0, cx);
            }
            return Some(HandEffect::Consume(1));
        }
        None
    }

    /// A dispenser fires `stack` at it: a chest onto a tamed chested one.
    pub fn dispensed_on(
        &mut self,
        core: &mut MobCore,
        profile: &Profile,
        columns: u8,
        stack: &ItemStack,
    ) -> Option<tuff_core::world::region_api::DispenseOutcome> {
        if !self.can_equip_chest(core, profile, stack) {
            return None;
        }
        self.equip_chest(core, columns);
        Some(tuff_core::world::region_api::DispenseOutcome::Dispensed)
    }

    /// Every tick: counters for the mouth and rearing, and the screen.
    pub fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.inventory.sync(core, cx);
        if self.mouth_counter > 0 {
            self.mouth_counter += 1;
            if self.mouth_counter > MOUTH_TICKS {
                self.set_flag_state(core, |state| {
                    state.mouth_counter = 0;
                    state.mouth_open = false;
                });
            }
        }
        if self.stand_counter > 0 {
            self.stand_counter -= 1;
            if self.stand_counter <= 0 {
                self.set_flag_state(core, |state| state.standing = false);
            }
        }
    }

    /// The random rearing goal: the counter starts a stand interval below
    /// zero and, past zero, rises one chance in a thousand per tick.
    fn random_stand(
        &mut self,
        core: &mut MobCore,
        profile: &Profile,
        ambient: Option<SoundEvent>,
        cx: &mut TickContext,
    ) {
        if !profile.rears {
            return;
        }
        self.stand_ticks += 1;
        let next_stand = self.stand_ticks - STAND_INTERVAL;
        if next_stand <= 0 || cx.rng.gen_range(0..1000) >= next_stand {
            return;
        }
        self.stand_ticks = 0;
        let immobile = (self.eating || self.standing) && core.base.riders.is_empty();
        if immobile || cx.rng.gen_range(0..10) != 0 {
            return;
        }
        self.stand_if_possible(core, profile);
        if let Some(sound) = ambient {
            let pitch = core.voice_pitch(cx.rng);
            core.sound(sound, 1.0, pitch, cx);
        }
    }

    /// The end of its AI step: healing, grazing and taming its rider.
    pub fn ai_step(
        &mut self,
        core: &mut MobCore,
        profile: &Profile,
        ambient: Option<SoundEvent>,
        cx: &mut TickContext,
    ) {
        self.random_stand(core, profile, ambient, cx);
        if cx.rng.gen_range(0..900) == 0 && core.death_time == 0 {
            core.heal(1.0);
        }
        if profile.grazes {
            if !self.eating && !ridden(core) && cx.rng.gen_range(0..300) == 0 {
                let at = BlockPos::containing(core.base.position);
                let below = BlockPos::new(at.x, at.y - 1, at.z);
                if (cx.block)(below).is_some_and(|state| state.block() == Block::GrassBlock) {
                    self.set_flag_state(core, |state| state.eating = true);
                }
            }
            if self.eating {
                self.eating_counter += 1;
                if self.eating_counter > GRAZE_TICKS {
                    self.eating_counter = 0;
                    self.set_flag_state(core, |state| state.eating = false);
                }
            }
        }
        self.tame_rider(core, profile, cx);
    }

    /// While an untamed horse carries a player, roughly once every 50
    /// ticks it either tames (chance `temper / max_temper`) or bucks
    /// everyone off and raises its temper by 5.
    pub fn tame_rider(&mut self, core: &mut MobCore, profile: &Profile, cx: &mut TickContext) {
        if is_tame(core) || core.base.riders.is_empty() || !core.base.entity_riders.is_empty() {
            return;
        }
        if cx.rng.gen_range(0..50) != 0 {
            return;
        }
        let rider = core.base.riders[0].clone();
        let max = profile.max_temper;
        if max > 0 && cx.rng.gen_range(0..max) < self.temper {
            if let Some(tame) = &mut core.tame {
                tame.owner = Some(rider.uuid);
            }
            core.changed(FLAGS);
            core.entity_event(event::TAME_SUCCEEDED, cx);
            core.report_tamed(rider.uuid, cx);
            return;
        }
        self.temper = (self.temper + 5).clamp(0, max);
        let position = [
            core.base.position[0],
            core.bounds()[4] + 0.1,
            core.base.position[2],
        ];
        for rider in std::mem::take(&mut core.base.riders) {
            cx.effects.push(Effect::Dismount { rider, position });
        }
        self.make_mad(core, profile, cx);
        core.entity_event(event::TAME_FAILED, cx);
    }

    /// A driving rider's input this tick: a released jump charge rears it
    /// and jumps once it is on the ground.
    pub fn ride(&mut self, core: &mut MobCore, profile: &Profile, input: &RiderInput) {
        if let Some(charge) = input.jump_charge
            && saddled(core)
        {
            self.jump_pending = jump_scale(charge);
            self.stand_if_possible(core, profile);
        }
        if let Some(latched) = &mut core.rider_input {
            latched.jump_charge = None;
        }
        if !core.base.on_ground {
            return;
        }
        let amount = std::mem::take(&mut self.jump_pending);
        if amount <= 0.0 {
            return;
        }
        let power = core.attribute(Attribute::JumpStrength) * f64::from(amount);
        core.base.velocity[1] = power;
        if input.forward > 0.0 {
            let yaw = input.yaw.to_radians();
            core.base.velocity[0] += f64::from(-LEAP * yaw.sin() * amount);
            core.base.velocity[2] += f64::from(LEAP * yaw.cos() * amount);
        }
    }

    /// What it carries drops when it dies: the chest's contents and the
    /// chest itself.
    pub fn died(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        for stack in self.inventory.take_chest() {
            core.drop_item(stack, 0.0, cx);
        }
        if std::mem::take(&mut self.has_chest) {
            core.drop_item(ItemStack::mint(Item::Chest, 1, MintReason::Spawn), 0.0, cx);
            core.changed(FLAGS + 1);
        }
    }

    pub fn hurt(&mut self, core: &mut MobCore, profile: &Profile, cx: &mut TickContext) {
        if cx.rng.gen_range(0..3) == 0 {
            self.stand_if_possible(core, profile);
        }
    }
}

/// How far a released jump of `charge` (1 to 100) carries: full at 90.
pub fn jump_scale(charge: u8) -> f32 {
    if charge >= 90 {
        1.0
    } else {
        0.4 + 0.4 * f32::from(charge) / 90.0
    }
}

/// Runs around while an untamed horse carries a player.
#[derive(Debug, Default)]
pub struct RunAroundLikeCrazy {
    speed: f64,
    target: [f64; 3],
}

impl RunAroundLikeCrazy {
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            target: [0.0; 3],
        }
    }
}

impl Goal for RunAroundLikeCrazy {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if is_tame(mob) || mob.base.riders.is_empty() || !mob.base.entity_riders.is_empty() {
            return false;
        }
        match super::random_pos::default_pos(mob, 5, 4, cx) {
            Some(target) => {
                self.target = target;
                true
            }
            None => false,
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !is_tame(mob) && !mob.navigation.is_done() && !mob.base.riders.is_empty()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.move_to_position(self.target, self.speed, cx);
    }
}

/// The goals of every abstract horse; `tempt` adds floating, panicking and
/// following food (all but skeleton horses).
pub fn add_common_goals(
    goals: &mut GoalSelector,
    core: &mut MobCore,
    tempt: Option<fn(Item) -> bool>,
) {
    if let Some(tempt) = tempt {
        goals.add(0, super::goals::Float::new(core));
        goals.add(1, Panic::new(1.2));
        goals.add(3, Tempt::new(1.25, tempt));
    }
    goals.add(1, RunAroundLikeCrazy::new(1.2));
    goals.add(2, Breed::new(1.0).with_partners(mates));
    goals.add(4, FollowParent::new(1.0));
    goals.add(6, RandomStroll::water_avoiding(0.7));
    goals.add(7, LookAtPlayer::new(6.0));
    goals.add(8, RandomLookAround::default());
}

/// Which kinds a horse-family mob courts: horses and donkeys each other,
/// llamas and trader llamas each other, everything else only its own kind.
pub fn mates(own: EntityType, other: EntityType) -> bool {
    let horses = |kind| matches!(kind, EntityType::Horse | EntityType::Donkey);
    let llamas = |kind| matches!(kind, EntityType::Llama | EntityType::TraderLlama);
    own == other || (horses(own) && horses(other)) || (llamas(own) && llamas(other))
}

/// Whether a horse may become a parent now: a tamed, unridden adult at
/// full health and in love.
pub fn can_parent(core: &MobCore) -> bool {
    !ridden(core)
        && core.base.vehicle.is_none()
        && is_tame(core)
        && !core.is_baby()
        && core.health >= core.max_health()
        && animal::ready_to_breed(core)
}

/// Two horse-family parents of species `A` and `B` have a `C` of `kind`.
pub fn breed_foal<A: Species, B: Species, C: Species>(
    mob: &mut Mob<A>,
    partner: &mut Mob<B>,
    kind: EntityType,
    species: impl FnOnce(&A, &B, &mut StdRng) -> C,
    cx: &mut TickContext,
) -> Option<Box<dyn RegionEntity>> {
    if !can_parent(&mob.core) || !can_parent(&partner.core) {
        return None;
    }
    let species = species(&mob.species, &partner.species, cx.rng);
    let mut child = animal::breed_child(mob, partner, kind, species, cx);
    inherit_attributes(&mut child.core, &mob.core, &partner.core, cx.rng);
    Some(Box::new(child))
}

fn horse_tempt(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::HORSE_TEMPT_ITEMS, item)
}

fn horse_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::HORSE_FOOD, item)
}

fn zombie_horse_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::ZOMBIE_HORSE_FOOD, item)
}

const HORSE: Profile = Profile {
    armor: MountArmor::Horse,
    angry: Some(SoundEvent::EntityHorseAngry),
    eat: Some(SoundEvent::EntityHorseEat),
    ..Profile::horse_like(horse_food)
};

const DONKEY: Profile = Profile {
    chested: true,
    angry: Some(SoundEvent::EntityDonkeyAngry),
    eat: Some(SoundEvent::EntityDonkeyEat),
    chest_sound: SoundEvent::EntityDonkeyChest,
    ..Profile::horse_like(horse_food)
};

const MULE: Profile = Profile {
    chested: true,
    angry: Some(SoundEvent::EntityMuleAngry),
    eat: Some(SoundEvent::EntityMuleEat),
    chest_sound: SoundEvent::EntityMuleChest,
    ..Profile::horse_like(horse_food)
};

const SKELETON_HORSE: Profile = Profile {
    fed_by_hand: false,
    tame_to_touch: true,
    ..Profile::horse_like(horse_food)
};

const ZOMBIE_HORSE: Profile = Profile {
    armor: MountArmor::Horse,
    angry: Some(SoundEvent::EntityZombieHorseAngry),
    eat: Some(SoundEvent::EntityZombieHorseEat),
    ..Profile::horse_like(zombie_horse_food)
};

/// The [`Species`] methods every horse-family type answers the same way
/// from its `equine` field, its `PROFILE` and its `columns()`.
macro_rules! equine_species {
    () => {
        fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
            let mut data = vec![(
                $crate::equine::FLAGS,
                MetadataValue::Byte(self.equine.flags(core)),
            )];
            data.extend(self.own_metadata());
            data
        }

        fn is_food(&self, stack: &ItemStack) -> bool {
            (Self::PROFILE.food)(stack.item)
        }

        fn can_fall_in_love(&self, _core: &MobCore) -> bool {
            false
        }

        fn immobile(&self, core: &MobCore) -> bool {
            (self.equine.eating || self.equine.is_standing()) && core.base.riders.is_empty()
        }

        fn seats(&self, core: &MobCore) -> usize {
            if core.is_baby() { 0 } else { 1 }
        }

        fn drivable(&self, core: &MobCore) -> bool {
            $crate::equine::is_tame(core) && $crate::equine::saddled(core)
        }

        fn drive_input(&self, _core: &MobCore, input: RiderInput) -> (f32, f32) {
            $crate::equine::drive_input(input)
        }

        fn hops_when_ridden(&self) -> bool {
            false
        }

        fn ride_speed(
            &mut self,
            core: &mut MobCore,
            input: &RiderInput,
            _cx: &mut TickContext,
        ) -> f32 {
            self.equine.ride(core, &Self::PROFILE, input);
            1.0
        }

        fn mount_refused(&self, core: &MobCore, stack: Option<&ItemStack>) -> bool {
            self.equine.refuses_mount(core, &Self::PROFILE, stack) || self.refuses_rider(core)
        }

        fn ridden_speed(&self, core: &MobCore) -> Option<tuff_core::world::entity::RiddenSpeed> {
            Some($crate::equine::ridden_speed(core))
        }

        fn inventory(&self) -> Option<(&[Option<ItemStack>], tuff_core::world::menu::MenuKind)> {
            Some((
                self.equine.inventory.chest(),
                self.equine.menu(&Self::PROFILE),
            ))
        }

        fn inventory_mut(&mut self) -> Option<&mut [Option<ItemStack>]> {
            Some(self.equine.inventory.chest_from_screen())
        }

        fn inventory_opens_for(&self, core: &MobCore, player: i32, sneaking: bool) -> bool {
            $crate::equine::Equine::opens_for(core, player, sneaking)
        }

        fn interact(
            &mut self,
            core: &mut MobCore,
            interaction: &EntityInteraction,
            cx: &mut TickContext,
        ) -> InteractOutcome {
            let columns = self.chest_columns();
            self.equine
                .interact(core, &Self::PROFILE, columns, interaction, cx)
        }

        fn dispensed_on(
            &mut self,
            core: &mut MobCore,
            stack: &ItemStack,
            _cx: &mut TickContext,
        ) -> Option<tuff_core::world::region_api::DispenseOutcome> {
            let columns = self.chest_columns();
            self.equine
                .dispensed_on(core, &Self::PROFILE, columns, stack)
        }

        fn after_hurt(
            &mut self,
            core: &mut MobCore,
            _damage: &$crate::Damage,
            cx: &mut TickContext,
        ) {
            self.equine.hurt(core, &Self::PROFILE, cx);
        }

        fn died(&mut self, core: &mut MobCore, _damage: &$crate::Damage, cx: &mut TickContext) {
            self.equine.died(core, cx);
        }
    };
}

pub(crate) use equine_species;

fn configure_equine(core: &mut MobCore) {
    core.make_animal();
    core.tame = Some(animal::Tame::default());
}

// ---------------------------------------------------------------------
// Horse
// ---------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct Horse {
    equine: Equine,
    variant: u8,
    markings: u8,
}

impl Horse {
    const PROFILE: Profile = HORSE;

    fn variant_value(&self) -> i32 {
        (self.variant as i32) | ((self.markings as i32) << 8)
    }

    fn own_metadata(&self) -> Vec<(u8, MetadataValue)> {
        vec![(FLAGS + 1, MetadataValue::VarInt(self.variant_value()))]
    }

    fn chest_columns(&self) -> u8 {
        0
    }

    fn refuses_rider(&self, _core: &MobCore) -> bool {
        false
    }
}

impl Species for Horse {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn configure(core: &mut MobCore) {
        configure_equine(core);
        core.set_attribute_base(Attribute::MaxHealth, MIN_MAX_HEALTH);
        core.set_attribute_base(Attribute::MovementSpeed, MIN_SPEED);
        core.set_attribute_base(Attribute::JumpStrength, MIN_JUMP);
        let mut goals = GoalSelector::default();
        add_common_goals(&mut goals, core, Some(horse_tempt));
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        self.equine.load(core, nbt, &HORSE);
        if let Some(variant) = take_number(nbt, "Variant") {
            let variant = variant as i32;
            self.variant = (variant & 0xFF) as u8;
            self.markings = ((variant >> 8) & 0xFF) as u8;
        }
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        self.equine.save(core, out, &HORSE);
        out.insert("Variant", self.variant_value());
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        let baby = core.is_baby();
        let pick = |adult, young| Some(if baby { young } else { adult });
        MobSounds {
            ambient: pick(
                SoundEvent::EntityHorseAmbient,
                SoundEvent::EntityBabyHorseAmbient,
            ),
            hurt: pick(SoundEvent::EntityHorseHurt, SoundEvent::EntityBabyHorseHurt),
            death: pick(
                SoundEvent::EntityHorseDeath,
                SoundEvent::EntityBabyHorseDeath,
            ),
            step: pick(SoundEvent::EntityHorseStep, SoundEvent::EntityBabyHorseStep),
            volume: 0.8,
        }
    }

    equine_species!();

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.equine.tick(core, cx);
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.equine
            .ai_step(core, &HORSE, self.sounds(core).ambient, cx);
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        let pick_variant = rng.gen_range(0..9);
        let variant = if pick_variant < 4 {
            self.variant
        } else if pick_variant < 8 {
            partner.variant
        } else {
            rng.gen_range(0..7)
        };
        let pick_markings = rng.gen_range(0..5);
        let markings = if pick_markings < 2 {
            self.markings
        } else if pick_markings < 4 {
            partner.markings
        } else {
            rng.gen_range(0..5)
        };
        Self {
            equine: Equine::bred(),
            variant,
            markings,
        }
    }

    fn breed_with(
        mob: &mut Mob<Self>,
        partner: &mut dyn RegionEntity,
        cx: &mut TickContext,
    ) -> Option<Box<dyn RegionEntity>> {
        if let Some(donkey) = partner.downcast_mut::<Mob<Donkey>>() {
            return breed_foal(mob, donkey, EntityType::Mule, |_, _, _| Mule::bred(), cx);
        }
        let partner = partner.downcast_mut::<Mob<Self>>()?;
        breed_foal(
            mob,
            partner,
            EntityType::Horse,
            |a, b, rng| a.offspring(b, rng),
            cx,
        )
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.set_attribute_base(Attribute::MaxHealth, roll_health(cx.rng));
        core.set_attribute_base(Attribute::MovementSpeed, roll_speed(cx.rng));
        core.set_attribute_base(Attribute::JumpStrength, roll_jump(cx.rng));
        core.health = core.max_health();
        self.variant = cx.rng.gen_range(0..7);
        self.markings = cx.rng.gen_range(0..5);
    }
}

// ---------------------------------------------------------------------
// Donkey / Mule (chested horses; only max health is randomized)
// ---------------------------------------------------------------------

macro_rules! chested_horse {
    ($name:ident, $profile:expr, $ambient:ident, $hurt:ident, $death:ident) => {
        #[derive(Debug, Default)]
        pub struct $name {
            equine: Equine,
        }

        impl $name {
            const PROFILE: Profile = $profile;

            fn bred() -> Self {
                Self {
                    equine: Equine::bred(),
                }
            }

            fn own_metadata(&self) -> Vec<(u8, MetadataValue)> {
                vec![(FLAGS + 1, MetadataValue::Boolean(self.equine.has_chest))]
            }

            fn chest_columns(&self) -> u8 {
                CHEST_COLUMNS
            }

            fn refuses_rider(&self, _core: &MobCore) -> bool {
                false
            }
        }

        impl Species for $name {
            fn create(_rng: &mut StdRng) -> Self {
                Self::default()
            }

            fn configure(core: &mut MobCore) {
                configure_equine(core);
                core.set_attribute_base(Attribute::MaxHealth, MIN_MAX_HEALTH);
                core.set_attribute_base(Attribute::MovementSpeed, 0.175);
                core.set_attribute_base(Attribute::JumpStrength, 0.5);
                let mut goals = GoalSelector::default();
                add_common_goals(&mut goals, core, Some(horse_tempt));
                core.goals = goals;
            }

            fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
                self.equine.load(core, nbt, &$profile);
                let columns = if self.equine.has_chest {
                    CHEST_COLUMNS
                } else {
                    0
                };
                self.equine.load_chest(columns, nbt);
            }

            fn save(&self, core: &MobCore, out: &mut Compound) {
                self.equine.save(core, out, &$profile);
            }

            fn sounds(&self, _core: &MobCore) -> MobSounds {
                MobSounds {
                    ambient: Some(SoundEvent::$ambient),
                    hurt: Some(SoundEvent::$hurt),
                    death: Some(SoundEvent::$death),
                    step: Some(SoundEvent::EntityHorseStep),
                    volume: 0.8,
                }
            }

            equine_species!();

            fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
                self.equine.tick(core, cx);
            }

            fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
                self.equine
                    .ai_step(core, &$profile, self.sounds(core).ambient, cx);
            }

            fn offspring(&self, _partner: &Self, _rng: &mut StdRng) -> Self {
                Self::bred()
            }

            fn breed_with(
                mob: &mut Mob<Self>,
                partner: &mut dyn RegionEntity,
                cx: &mut TickContext,
            ) -> Option<Box<dyn RegionEntity>> {
                Self::cross(mob, partner, cx)
            }

            fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
                core.set_attribute_base(Attribute::MaxHealth, roll_health(cx.rng));
                core.health = core.max_health();
            }
        }
    };
}

chested_horse!(
    Donkey,
    DONKEY,
    EntityDonkeyAmbient,
    EntityDonkeyHurt,
    EntityDonkeyDeath
);
chested_horse!(
    Mule,
    MULE,
    EntityMuleAmbient,
    EntityMuleHurt,
    EntityMuleDeath
);

impl Donkey {
    fn cross(
        mob: &mut Mob<Self>,
        partner: &mut dyn RegionEntity,
        cx: &mut TickContext,
    ) -> Option<Box<dyn RegionEntity>> {
        if let Some(horse) = partner.downcast_mut::<Mob<Horse>>() {
            return breed_foal(mob, horse, EntityType::Mule, |_, _, _| Mule::bred(), cx);
        }
        let partner = partner.downcast_mut::<Mob<Self>>()?;
        breed_foal(mob, partner, EntityType::Donkey, |_, _, _| Self::bred(), cx)
    }
}

impl Mule {
    /// Mules are sterile.
    fn cross(
        _mob: &mut Mob<Self>,
        _partner: &mut dyn RegionEntity,
        _cx: &mut TickContext,
    ) -> Option<Box<dyn RegionEntity>> {
        None
    }
}

// ---------------------------------------------------------------------
// Skeleton Horse / Zombie Horse
// ---------------------------------------------------------------------

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Horse, super::load::<Horse>);
    registry.register(EntityType::Donkey, super::load::<Donkey>);
    registry.register(EntityType::Mule, super::load::<Mule>);
    registry.register(EntityType::SkeletonHorse, super::load::<SkeletonHorse>);
    registry.register(EntityType::ZombieHorse, super::load::<ZombieHorse>);
}

mod undead;
pub use undead::*;

#[cfg(test)]
#[path = "equine_tests.rs"]
mod tests;
