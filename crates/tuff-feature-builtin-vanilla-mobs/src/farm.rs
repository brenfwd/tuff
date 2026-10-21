//! The farm animals: cows (milking), pigs, sheep (shearing, dyeing, grazing
//! wool back) and chickens (eggs, slow falling), with their variants.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_core::world::{framed, packets};
use tuff_item::item::SuspiciousEffect;
use tuff_vanilla_data::entity::{Climate, PathType, sheep_colors};
use tuff_vanilla_data::item::DyeColor;
use tuff_vanilla_data::registry::MobEffect;
use tuff_vanilla_data::registry::ParticleType;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::tag::Key;
use tuff_vanilla_data::variant;
use tuff_vanilla_data::{Item, tag};

use super::goal::GoalSelector;
use super::goals::{
    Breed, EatBlock, Float, FollowParent, LookAtPlayer, Panic, RandomLookAround, RandomStroll,
    Tempt,
};
use super::goat_ram::{RamGoal, horn_instruments};
use super::voiced_variant::Variants;
use super::{MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::crafting::{CraftingInput, RecipeSet};
use tuff_core::world::drops::EntityFacts;
use tuff_core::world::entity::{
    Effect, EntityInteraction, InteractOutcome, RiderInput, TickContext, take, take_bool,
    take_number,
};
use tuff_core::world::messages::HandEffect;
use tuff_item::item::{BurnReason, ItemStack, MintReason};
use tuff_nbt::{Compound, Tag};

const COW_VOLUME: f32 = 0.4;
const EGG_TIME_MIN: i32 = 6000;
const SHEEP_REGROW_SECONDS: i32 = 60;

fn in_tag(tag: Key<Item>, stack: &ItemStack) -> bool {
    tuff_datapack::tags::is(tag, stack.item)
}

fn animal_experience(rng: &mut StdRng) -> i32 {
    1 + rng.gen_range(0..3)
}

fn farm_sounds(set: &variant::SoundSet, volume: f32) -> MobSounds {
    MobSounds {
        ambient: Some(set.ambient),
        hurt: Some(set.hurt),
        death: Some(set.death),
        step: set.step,
        volume,
    }
}

fn common_goals(
    goals: &mut GoalSelector,
    core: &mut MobCore,
    panic: f64,
    tempt: (f64, fn(Item) -> bool),
    follow: f64,
) {
    goals.add(0, Float::new(core));
    goals.add(1, Panic::new(panic));
    goals.add(2, Breed::new(1.0));
    goals.add(3, Tempt::new(tempt.0, tempt.1));
    goals.add(4, FollowParent::new(follow));
}

#[derive(Debug)]
pub struct Cow {
    variants: Variants,
}

fn cow_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::COW_FOOD, item)
}

impl Species for Cow {
    fn create(_rng: &mut StdRng) -> Self {
        Self {
            variants: Variants::default_for(
                crate::pack_variants::cow(),
                crate::pack_variants::cow_sounds(),
            ),
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        let mut goals = GoalSelector::default();
        common_goals(&mut goals, core, 2.0, (1.25, cow_food), 1.25);
        goals.add(5, RandomStroll::water_avoiding(1.0));
        goals.add(6, LookAtPlayer::new(6.0));
        goals.add(7, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.variants.load(
            nbt,
            crate::pack_variants::cow(),
            crate::pack_variants::cow_sounds(),
        );
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        self.variants.save(
            out,
            crate::pack_variants::cow(),
            crate::pack_variants::cow_sounds(),
        );
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        self.variants.metadata(core, data::SPECIES)
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        farm_sounds(
            self.variants
                .sounds(crate::pack_variants::cow_sounds(), core.is_baby()),
            COW_VOLUME,
        )
    }

    fn ambient_sound_interval(&self) -> i32 {
        120
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        in_tag(tag::item::key::COW_FOOD, stack)
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if stack.item != Item::Bucket || core.is_baby() {
            return None;
        }
        core.sound_as(
            SoundEvent::EntityCowMilk,
            play::sound_category::PLAYERS,
            1.0,
            1.0,
            cx,
        );
        Some(HandEffect::Exchange(Item::MilkBucket))
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        Self {
            variants: self.variants.inherit(
                &partner.variants,
                Variants::default_for(
                    crate::pack_variants::cow(),
                    crate::pack_variants::cow_sounds(),
                ),
                rng,
            ),
        }
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.variants.finalize(
            core,
            crate::pack_variants::cow(),
            crate::pack_variants::cow_sounds(),
            cx,
        );
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        animal_experience(rng)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

/// Red or brown; a fixed, code-only two-value enum (protocol wire values
/// 0/1), unlike the cow/pig/chicken family's data-driven variant registries
/// -- 26.3's game code still assigns mooshroom color this way rather than
/// through a `mooshroom_variant` data pack registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MooshroomColor {
    Red,
    Brown,
}

impl MooshroomColor {
    fn id(self) -> i32 {
        match self {
            Self::Red => 0,
            Self::Brown => 1,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Brown => "brown",
        }
    }
}

#[derive(Debug)]
pub struct Mooshroom {
    color: MooshroomColor,
    stew: Option<Vec<SuspiciousEffect>>,
    struck: bool,
}

const FLOWER_EFFECT_TICKS: [(Item, MobEffect, f32); 17] = [
    (Item::Dandelion, MobEffect::Saturation, 0.35),
    (Item::GoldenDandelion, MobEffect::Saturation, 0.35),
    (Item::Torchflower, MobEffect::NightVision, 5.0),
    (Item::Poppy, MobEffect::NightVision, 5.0),
    (Item::BlueOrchid, MobEffect::Saturation, 0.35),
    (Item::Allium, MobEffect::FireResistance, 3.0),
    (Item::AzureBluet, MobEffect::Blindness, 11.0),
    (Item::RedTulip, MobEffect::Weakness, 7.0),
    (Item::OrangeTulip, MobEffect::Weakness, 7.0),
    (Item::WhiteTulip, MobEffect::Weakness, 7.0),
    (Item::PinkTulip, MobEffect::Weakness, 7.0),
    (Item::OxeyeDaisy, MobEffect::Regeneration, 7.0),
    (Item::Cornflower, MobEffect::JumpBoost, 5.0),
    (Item::WitherRose, MobEffect::Wither, 7.0),
    (Item::LilyOfTheValley, MobEffect::Poison, 11.0),
    (Item::OpenEyeblossom, MobEffect::Blindness, 11.0),
    (Item::ClosedEyeblossom, MobEffect::Nausea, 7.0),
];

fn flower_effects(item: Item) -> Option<Vec<SuspiciousEffect>> {
    FLOWER_EFFECT_TICKS
        .iter()
        .find(|(flower, ..)| *flower == item)
        .map(|&(_, effect, seconds)| {
            vec![SuspiciousEffect {
                effect,
                duration: (seconds * 20.0).floor() as i32,
            }]
        })
}

impl Species for Mooshroom {
    fn shear(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        mooshroom_shear(self, core, play::sound_category::BLOCKS, cx)
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self {
            color: MooshroomColor::Red,
            stew: None,
            struck: false,
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        let mut goals = GoalSelector::default();
        common_goals(&mut goals, core, 2.0, (1.25, cow_food), 1.25);
        goals.add(5, RandomStroll::water_avoiding(1.0));
        goals.add(6, LookAtPlayer::new(6.0));
        goals.add(7, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        if let Some(color) = take(nbt, "Type", |t| {
            Some(match t.as_str()? {
                "brown" => MooshroomColor::Brown,
                _ => MooshroomColor::Red,
            })
        }) {
            self.color = color;
        }
        self.stew = take(nbt, "stew_effects", |t| {
            tuff_item::item::suspicious_stew_effects_from_nbt(t)
        });
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("Type", self.color.name());
        if let Some(effects) = &self.stew {
            out.insert(
                "stew_effects",
                tuff_item::item::suspicious_stew_effects_to_nbt(effects),
            );
        }
    }

    fn heard(&mut self, _core: &mut MobCore, notice: tuff_core::world::entity::Notice) {
        if notice == tuff_core::world::entity::Notice::Charged {
            self.struck = true;
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !std::mem::take(&mut self.struck) {
            return;
        }
        self.color = match self.color {
            MooshroomColor::Red => MooshroomColor::Brown,
            MooshroomColor::Brown => MooshroomColor::Red,
        };
        core.changed(data::SPECIES);
        core.sound(SoundEvent::EntityMooshroomConvert, 2.0, 1.0, cx);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(data::SPECIES, MetadataValue::VarInt(self.color.id()))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        farm_sounds(&crate::pack_variants::cow_sounds()[0].adult, COW_VOLUME)
    }

    fn ambient_sound_interval(&self) -> i32 {
        120
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        in_tag(tag::item::key::COW_FOOD, stack)
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if core.is_baby() {
            return None;
        }
        if stack.item == Item::Bucket {
            core.sound_as(
                SoundEvent::EntityCowMilk,
                play::sound_category::PLAYERS,
                1.0,
                1.0,
                cx,
            );
            return Some(HandEffect::Exchange(Item::MilkBucket));
        }
        if stack.item == Item::Bowl {
            let (sound, result) = match self.stew.take() {
                Some(effects) => {
                    let mut stew = ItemStack::mint(Item::SuspiciousStew, 1, MintReason::Spawn);
                    stew.components.suspicious_stew_effects = Some(effects);
                    (
                        SoundEvent::EntityMooshroomSuspiciousMilk,
                        HandEffect::ExchangeStack(Box::new(stew)),
                    )
                }
                None => (
                    SoundEvent::EntityMooshroomMilk,
                    HandEffect::Exchange(Item::MushroomStew),
                ),
            };
            core.sound_as(sound, play::sound_category::PLAYERS, 1.0, 1.0, cx);
            return Some(result);
        }
        if stack.item == Item::Shears {
            return mooshroom_shear(self, core, play::sound_category::PLAYERS, cx)
                .then_some(HandEffect::Damage(1));
        }
        if self.color == MooshroomColor::Brown
            && let Some(effects) = flower_effects(stack.item)
        {
            if self.stew.is_some() {
                let [x, y, z] = core.base.position;
                let packet = framed(&play::Particles {
                    particle: ParticleType::Smoke.id(),
                    position: [x, y + f64::from(core.size.height) * 0.5, z],
                    spread: [0.5, 0.0, 0.5],
                    speed: 0.2,
                    count: 2,
                    alternative: true,
                });
                cx.effects.push(Effect::ChunkPackets(packets([packet])));
                return Some(HandEffect::None);
            }
            self.stew = Some(effects);
            core.sound_as(
                SoundEvent::EntityMooshroomEat,
                play::sound_category::PLAYERS,
                2.0,
                1.0,
                cx,
            );
            return Some(HandEffect::Consume(1));
        }
        None
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        let color = if self.color == partner.color && rng.gen_range(0..1024) == 0 {
            if self.color == MooshroomColor::Brown {
                MooshroomColor::Red
            } else {
                MooshroomColor::Brown
            }
        } else if rng.r#gen() {
            self.color
        } else {
            partner.color
        };
        Self {
            color,
            stew: None,
            struck: false,
        }
    }

    fn loot_facts(&self, facts: &mut EntityFacts) {
        facts
            .components
            .push(("minecraft:mooshroom/variant", self.color.name().to_owned()));
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        animal_experience(rng)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct Pig {
    variants: Variants,
    steering: super::steering::ItemSteering,
    struck: bool,
}

fn pig_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::PIG_FOOD, item)
}

fn carrot_on_a_stick(item: Item) -> bool {
    item == Item::CarrotOnAStick
}

impl Species for Pig {
    fn create(_rng: &mut StdRng) -> Self {
        Self {
            variants: Variants::default_for(
                crate::pack_variants::pig(),
                crate::pack_variants::pig_sounds(),
            ),
            steering: Default::default(),
            struck: false,
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        let mut goals = GoalSelector::default();
        goals.add(0, Float::new(core));
        goals.add(1, Panic::new(1.25));
        goals.add(3, Breed::new(1.0));
        goals.add(4, Tempt::new(1.2, carrot_on_a_stick));
        goals.add(4, Tempt::new(1.2, pig_food));
        goals.add(5, FollowParent::new(1.1));
        goals.add(6, RandomStroll::water_avoiding(1.0));
        goals.add(7, LookAtPlayer::new(6.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.variants.load(
            nbt,
            crate::pack_variants::pig(),
            crate::pack_variants::pig_sounds(),
        );
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        self.variants.save(
            out,
            crate::pack_variants::pig(),
            crate::pack_variants::pig_sounds(),
        );
    }

    fn seats(&self, core: &MobCore) -> usize {
        usize::from(super::equine::saddled(core))
    }

    fn accepts_rider(&self, core: &MobCore) -> bool {
        !core.is_baby()
    }

    fn drivable(&self, core: &MobCore) -> bool {
        super::equine::saddled(core)
    }

    fn controlled_by(&self, _core: &MobCore, input: &RiderInput) -> bool {
        input.holds(Item::CarrotOnAStick)
    }

    fn drive_input(&self, _core: &MobCore, _input: RiderInput) -> (f32, f32) {
        (1.0, 0.0)
    }

    fn hops_when_ridden(&self) -> bool {
        false
    }

    fn ride_speed(&mut self, core: &mut MobCore, input: &RiderInput, _cx: &mut TickContext) -> f32 {
        core.base.pitch = input.pitch * 0.5;
        self.steering.tick();
        0.225 * self.steering.factor()
    }

    fn ridden_speed(&self, core: &MobCore) -> Option<tuff_core::world::entity::RiddenSpeed> {
        Some(tuff_core::world::entity::RiddenSpeed {
            speed: core.attribute(tuff_vanilla_data::registry::Attribute::MovementSpeed)
                * 0.225
                * f64::from(super::steering::MAX_FACTOR),
            leap: 0.0,
            ..Default::default()
        })
    }

    fn mount_refused(&self, _core: &MobCore, stack: Option<&ItemStack>) -> bool {
        stack.is_some_and(|s| pig_food(s.item))
    }

    fn boost(&mut self, core: &mut MobCore, roll: u32) -> bool {
        let boosted = self.steering.boost(roll);
        if boosted {
            core.changed(data::SPECIES);
        }
        boosted
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &tuff_core::world::entity::EntityInteraction,
        cx: &mut TickContext,
    ) -> tuff_core::world::entity::InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        super::equine::saddle_up(
            core,
            stack,
            &interaction.hand,
            Some(SoundEvent::EntityPigSaddle),
            cx,
        )
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let mut values = vec![(data::SPECIES, MetadataValue::VarInt(self.steering.total()))];
        values.extend(self.variants.metadata(core, data::SPECIES + 1));
        values
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        farm_sounds(
            self.variants
                .sounds(crate::pack_variants::pig_sounds(), core.is_baby()),
            1.0,
        )
    }

    fn eating_sound(&self, core: &MobCore) -> Option<SoundEvent> {
        self.variants
            .sounds(crate::pack_variants::pig_sounds(), core.is_baby())
            .eat
    }

    fn ambient_sound_interval(&self) -> i32 {
        120
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        in_tag(tag::item::key::PIG_FOOD, stack)
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        Self {
            variants: self.variants.inherit(
                &partner.variants,
                Variants::default_for(
                    crate::pack_variants::pig(),
                    crate::pack_variants::pig_sounds(),
                ),
                rng,
            ),
            steering: Default::default(),
            struck: false,
        }
    }

    fn heard(&mut self, _core: &mut MobCore, notice: tuff_core::world::entity::Notice) {
        if notice == tuff_core::world::entity::Notice::Charged {
            self.struck = true;
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !std::mem::take(&mut self.struck)
            || !core.is_alive()
            || *cx.shared.difficulty.borrow() == tuff_core::world::player::Difficulty::Peaceful
        {
            return;
        }
        let mut zombified = super::convert::<super::piglin::ZombifiedPiglin>(
            core,
            EntityType::ZombifiedPiglin,
            false,
            cx,
        );
        super::piglin::arm_zombified(&mut zombified.core, cx);
        zombified.core.persistence_required = true;
        if core.is_baby() {
            zombified.core.set_zombie_baby(true);
        }
        cx.effects.push(Effect::Spawn(Box::new(zombified)));
        core.discarded = true;
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.variants.finalize(
            core,
            crate::pack_variants::pig(),
            crate::pack_variants::pig_sounds(),
            cx,
        );
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        animal_experience(rng)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct Sheep {
    pub color: DyeColor,
    pub sheared: bool,
}

fn sheep_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::SHEEP_FOOD, item)
}

/// The dye color crafting two dyes together makes, if any recipe does.
fn mixed_color(recipes: &RecipeSet, a: DyeColor, b: DyeColor) -> Option<DyeColor> {
    let dyes = |color: DyeColor| {
        Item::ALL
            .iter()
            .copied()
            .filter(move |i| i.dye() == Some(color))
    };
    for first in dyes(a) {
        for second in dyes(b) {
            let probes = [
                ItemStack::mint(first, 1, MintReason::Template),
                ItemStack::mint(second, 1, MintReason::Template),
            ];
            let input = CraftingInput {
                width: 2,
                height: 1,
                items: probes.iter().map(Some).collect(),
            };
            let color = recipes.find(&input).and_then(|(_, recipe)| {
                let result = recipe.assemble(&input)?;
                let color = result.item.dye();
                result.burn(BurnReason::Template);
                color
            });
            for probe in probes {
                probe.burn(BurnReason::Template);
            }
            if color.is_some() {
                return color;
            }
        }
    }
    None
}

impl Sheep {
    fn wool_byte(&self) -> i8 {
        (self.color.id() as i8 & 15) | if self.sheared { 16 } else { 0 }
    }
}

impl Species for Sheep {
    fn shear(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        sheep_shear(self, core, play::sound_category::BLOCKS, cx)
    }

    fn create(_rng: &mut StdRng) -> Self {
        Self {
            color: DyeColor::White,
            sheared: false,
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        let mut goals = GoalSelector::default();
        common_goals(&mut goals, core, 1.25, (1.1, sheep_food), 1.1);
        goals.add(5, EatBlock::default());
        goals.add(6, RandomStroll::water_avoiding(1.0));
        goals.add(7, LookAtPlayer::new(6.0));
        goals.add(8, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.sheared = take_bool(nbt, "Sheared").unwrap_or(false);
        if let Some(color) = take(nbt, "Color", |t| {
            let id = match *t {
                Tag::Byte(v) => v as i32,
                Tag::Int(v) => v,
                _ => return None,
            };
            DyeColor::from_id(id.clamp(0, 15) as u32)
        }) {
            self.color = color;
        }
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("Sheared", self.sheared);
        out.insert("Color", self.color.id() as i8);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(data::SPECIES, MetadataValue::Byte(self.wool_byte()))]
    }

    fn blue_wool(&self) -> bool {
        self.color == DyeColor::Blue
    }

    fn heard(&mut self, core: &mut MobCore, notice: tuff_core::world::entity::Notice) {
        if notice == tuff_core::world::entity::Notice::Wololo && core.is_alive() {
            self.color = DyeColor::Red;
            core.changed(data::SPECIES);
        }
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntitySheepAmbient),
            hurt: Some(SoundEvent::EntitySheepHurt),
            death: Some(SoundEvent::EntitySheepDeath),
            step: Some(SoundEvent::EntitySheepStep),
            volume: 1.0,
        }
    }

    fn ambient_sound_interval(&self) -> i32 {
        120
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        in_tag(tag::item::key::SHEEP_FOOD, stack)
    }

    fn ate(&mut self, core: &mut MobCore) {
        self.sheared = false;
        core.changed(data::SPECIES);
        if core
            .age
            .as_ref()
            .is_some_and(super::animal::Age::can_age_up)
        {
            core.age_up(SHEEP_REGROW_SECONDS, false);
        }
    }

    fn interact(
        &mut self,
        core: &mut MobCore,
        interaction: &EntityInteraction,
        cx: &mut TickContext,
    ) -> InteractOutcome {
        let stack = interaction.stack.as_deref()?;
        if stack.item == Item::Shears {
            return sheep_shear(self, core, play::sound_category::PLAYERS, cx)
                .then_some(HandEffect::Damage(1));
        }
        if let Some(color) = stack.item.dye()
            && !self.sheared
            && color != self.color
        {
            core.sound_as(
                SoundEvent::ItemDyeUse,
                play::sound_category::PLAYERS,
                1.0,
                1.0,
                cx,
            );
            self.color = color;
            core.changed(data::SPECIES);
            return Some(HandEffect::Consume(1));
        }
        None
    }

    fn offspring_in(&self, partner: &Self, cx: &mut TickContext) -> Self {
        let recipes = cx.shared.recipes();
        let color = mixed_color(&recipes, self.color, partner.color).unwrap_or(if cx.rng.r#gen() {
            self.color
        } else {
            partner.color
        });
        Self {
            color,
            sheared: false,
        }
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let pos = super::physics::block_pos(core.base.position);
        let biome = (cx.biome)(pos).unwrap_or("minecraft:plains");
        let climate = if tuff_datapack::tags::has(
            tag::worldgen::biome::key::SPAWNS_WARM_VARIANT_FARM_ANIMALS,
            biome,
        ) {
            Climate::Warm
        } else if tuff_datapack::tags::has(
            tag::worldgen::biome::key::SPAWNS_COLD_VARIANT_FARM_ANIMALS,
            biome,
        ) {
            Climate::Cold
        } else {
            Climate::Temperate
        };
        let name = sheep_colors(climate).pick(&mut |bound| cx.rng.gen_range(0..bound));
        if let Some(color) = DyeColor::from_name(name) {
            self.color = color;
        }
    }

    fn loot_facts(&self, facts: &mut EntityFacts) {
        facts
            .components
            .push(("minecraft:sheep/color", self.color.name().to_owned()));
        facts.sheared = Some(self.sheared);
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        animal_experience(rng)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct Chicken {
    variants: Variants,
    pub egg_time: i32,
    pub jockey: bool,
}

fn chicken_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::CHICKEN_FOOD, item)
}

impl Species for Chicken {
    fn create(rng: &mut StdRng) -> Self {
        Self {
            variants: Variants::default_for(
                crate::pack_variants::chicken(),
                crate::pack_variants::chicken_sounds(),
            ),
            egg_time: rng.gen_range(0..EGG_TIME_MIN) + EGG_TIME_MIN,
            jockey: false,
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.set_path_malus(PathType::Water, 0.0);
        let mut goals = GoalSelector::default();
        common_goals(&mut goals, core, 1.4, (1.0, chicken_food), 1.1);
        goals.add(5, RandomStroll::water_avoiding(1.0));
        goals.add(6, LookAtPlayer::new(6.0));
        goals.add(7, RandomLookAround::default());
        core.goals = goals;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut Compound) {
        self.jockey = take_bool(nbt, "IsChickenJockey").unwrap_or(false);
        if let Some(time) = take_number(nbt, "EggLayTime") {
            self.egg_time = time as i32;
        }
        self.variants.load(
            nbt,
            crate::pack_variants::chicken(),
            crate::pack_variants::chicken_sounds(),
        );
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("IsChickenJockey", self.jockey);
        out.insert("EggLayTime", self.egg_time);
        self.variants.save(
            out,
            crate::pack_variants::chicken(),
            crate::pack_variants::chicken_sounds(),
        );
    }

    fn metadata(&self, core: &MobCore) -> Vec<(u8, MetadataValue)> {
        self.variants.metadata(core, data::SPECIES)
    }

    fn sounds(&self, core: &MobCore) -> MobSounds {
        farm_sounds(
            self.variants
                .sounds(crate::pack_variants::chicken_sounds(), core.is_baby()),
            1.0,
        )
    }

    fn ambient_sound_interval(&self) -> i32 {
        120
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        in_tag(tag::item::key::CHICKEN_FOOD, stack)
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if !core.base.on_ground && core.base.velocity[1] < 0.0 {
            core.base.velocity[1] *= 0.6;
        }
        if core.is_baby() || self.jockey {
            return;
        }
        self.egg_time -= 1;
        if self.egg_time > 0 {
            return;
        }
        let mut facts = EntityFacts {
            kind: Some(core.base.kind),
            tables: Some(cx.shared.loot_tables()),
            ..Default::default()
        };
        self.loot_facts(&mut facts);
        let drops =
            cx.shared
                .block_loot()
                .entity_drops("minecraft:gameplay/chicken_lay", &facts, cx.rng);
        if !drops.is_empty() {
            for stack in drops {
                core.drop_item(stack, 0.0, cx);
            }
            let pitch = (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2 + 1.0;
            core.sound_as(
                SoundEvent::EntityChickenEgg,
                play::sound_category::NEUTRAL,
                1.0,
                pitch,
                cx,
            );
        }
        self.egg_time = cx.rng.gen_range(0..EGG_TIME_MIN) + EGG_TIME_MIN;
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        let mut child = Self::create(rng);
        child.variants = self.variants.inherit(
            &partner.variants,
            Variants::default_for(
                crate::pack_variants::chicken(),
                crate::pack_variants::chicken_sounds(),
            ),
            rng,
        );
        child
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.variants.finalize(
            core,
            crate::pack_variants::chicken(),
            crate::pack_variants::chicken_sounds(),
            cx,
        );
    }

    fn loot_facts(&self, facts: &mut EntityFacts) {
        facts.components.push((
            "minecraft:chicken/variant",
            crate::pack_variants::chicken()[self.variants.variant]
                .name
                .to_owned(),
        ));
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        if self.jockey {
            10
        } else {
            animal_experience(rng)
        }
    }

    fn remove_when_far_away(&self) -> bool {
        self.jockey
    }

    fn heard(&mut self, _core: &mut MobCore, notice: tuff_core::world::entity::Notice) {
        if notice == tuff_core::world::entity::Notice::Jockey {
            self.jockey = true;
        }
    }
}

/// A rabbit's coat, by its wire VarInt value; `Evil` (the "killer bunny")
/// is summon-only, so nothing here naturally rolls it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RabbitVariant {
    Brown,
    White,
    Black,
    WhiteSplotched,
    Gold,
    Salt,
    Evil,
}

impl RabbitVariant {
    fn id(self) -> i32 {
        match self {
            Self::Brown => 0,
            Self::White => 1,
            Self::Black => 2,
            Self::WhiteSplotched => 3,
            Self::Gold => 4,
            Self::Salt => 5,
            Self::Evil => 99,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Brown => "brown",
            Self::White => "white",
            Self::Black => "black",
            Self::WhiteSplotched => "white_splotched",
            Self::Gold => "gold",
            Self::Salt => "salt",
            Self::Evil => "evil",
        }
    }

    fn from_name(name: &str) -> Self {
        match name {
            "white" => Self::White,
            "black" => Self::Black,
            "white_splotched" => Self::WhiteSplotched,
            "gold" => Self::Gold,
            "salt" => Self::Salt,
            "evil" => Self::Evil,
            _ => Self::Brown,
        }
    }

    /// The game's biome-weighted natural spawn roll.
    fn natural(biome: &str, rng: &mut StdRng) -> Self {
        let roll = rng.gen_range(0..100);
        if tuff_datapack::tags::has(tag::worldgen::biome::key::SPAWNS_WHITE_RABBITS, biome) {
            return if roll < 80 {
                Self::White
            } else {
                Self::WhiteSplotched
            };
        }
        if tuff_datapack::tags::has(tag::worldgen::biome::key::SPAWNS_GOLD_RABBITS, biome) {
            return Self::Gold;
        }
        if roll < 50 {
            Self::Brown
        } else if roll < 90 {
            Self::Salt
        } else {
            Self::Black
        }
    }
}

fn rabbit_food(item: Item) -> bool {
    tuff_datapack::tags::is(tag::item::key::RABBIT_FOOD, item)
}

#[derive(Debug)]
pub struct Rabbit {
    variant: RabbitVariant,
    hop: super::rabbit::Hop,
    evil_applied: bool,
    reroll: bool,
}

const RABBIT_PANIC_SPEED: f64 = 2.2;
const RABBIT_AVOID_SPEED: f64 = 2.2;
const EVIL_ARMOR: f64 = 8.0;
const EVIL_ATTACK_BONUS: f64 = 5.0;
const EVIL_MELEE_SPEED: f64 = 1.4;
const KILLER_BUNNY: &str = "entity.minecraft.killer_bunny";

impl Rabbit {
    fn new(variant: RabbitVariant) -> Self {
        Self {
            variant,
            hop: super::rabbit::Hop::default(),
            evil_applied: false,
            reroll: false,
        }
    }

    fn goals(core: &mut MobCore, evil: bool) -> GoalSelector {
        use super::goals::AvoidPlayer;
        let mut goals = GoalSelector::default();
        goals.add(1, Float::new(core));
        goals.add(1, super::goals::ClimbOnTopOfPowderSnow);
        goals.add(1, Panic::new(RABBIT_PANIC_SPEED));
        goals.add(2, Breed::new(0.8));
        goals.add(3, Tempt::new(1.0, rabbit_food));
        if evil {
            goals.add(4, super::attack::MeleeAttack::new(EVIL_MELEE_SPEED, true));
            goals.add(1, super::target::HurtByTarget::new(&[]));
            goals.add(2, super::target::NearestAttackablePlayer::new(true));
            goals.add(
                2,
                super::target::NearestAttackableEntity::new(&[EntityType::Wolf], true),
            );
        } else {
            goals.add(
                4,
                AvoidPlayer::new(8.0, RABBIT_AVOID_SPEED, RABBIT_AVOID_SPEED),
            );
            goals.add(
                4,
                AvoidPlayer::entities(
                    &[EntityType::Wolf],
                    10.0,
                    RABBIT_AVOID_SPEED,
                    RABBIT_AVOID_SPEED,
                ),
            );
            goals.add(
                4,
                AvoidPlayer::monsters(4.0, RABBIT_AVOID_SPEED, RABBIT_AVOID_SPEED),
            );
        }
        goals.add(5, super::rabbit::RaidGarden::new());
        goals.add(6, RandomStroll::water_avoiding(0.6));
        goals.add(11, LookAtPlayer::new(10.0));
        goals
    }

    fn become_evil(&mut self, core: &mut MobCore) {
        self.evil_applied = true;
        core.set_attribute_base(tuff_vanilla_data::registry::Attribute::Armor, EVIL_ARMOR);
        let damage = core.attribute(tuff_vanilla_data::registry::Attribute::AttackDamage);
        core.set_attribute_base(
            tuff_vanilla_data::registry::Attribute::AttackDamage,
            damage + EVIL_ATTACK_BONUS,
        );
        core.goals = Self::goals(core, true);
        if core.base.custom_name.is_none() {
            core.base.custom_name =
                Some(Tag::from(Compound::new().with("translate", KILLER_BUNNY)));
            core.changed(tuff_core::protocol::play::metadata::CUSTOM_NAME);
        }
    }
}

impl Species for Rabbit {
    fn create(_rng: &mut StdRng) -> Self {
        Self::new(RabbitVariant::Brown)
    }

    fn configure(core: &mut MobCore) {
        core.make_animal();
        core.goals = Self::goals(core, false);
    }

    fn custom_ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if std::mem::take(&mut self.reroll) {
            let pos = super::physics::block_pos(core.base.position);
            let biome = (cx.biome)(pos).unwrap_or("minecraft:plains");
            self.variant = RabbitVariant::natural(biome, cx.rng);
        }
        let evil = self.variant == RabbitVariant::Evil;
        if evil && !self.evil_applied {
            self.become_evil(core);
        }
        self.hop.step(core, evil, cx);
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        core.more_carrot_ticks = take_number(nbt, "MoreCarrotTicks").map_or(0, |v| v as i32);
        if let Some(variant) = take(nbt, "RabbitType", |t| {
            let id = match *t {
                Tag::Byte(v) => v as i32,
                Tag::Int(v) => v,
                _ => return None,
            };
            Some(if id == 99 {
                RabbitVariant::Evil
            } else {
                RabbitVariant::from_name(match id {
                    1 => "white",
                    2 => "black",
                    3 => "white_splotched",
                    4 => "gold",
                    5 => "salt",
                    _ => "brown",
                })
            })
        }) {
            self.variant = variant;
        }
    }

    fn save(&self, core: &MobCore, out: &mut Compound) {
        out.insert("RabbitType", self.variant.id());
        out.insert("MoreCarrotTicks", core.more_carrot_ticks);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![(data::SPECIES, MetadataValue::VarInt(self.variant.id()))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntityRabbitAmbient),
            hurt: Some(SoundEvent::EntityRabbitHurt),
            death: Some(SoundEvent::EntityRabbitDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn ambient_sound_interval(&self) -> i32 {
        120
    }

    fn is_food(&self, stack: &ItemStack) -> bool {
        rabbit_food(stack.item)
    }

    fn offspring(&self, partner: &Self, rng: &mut StdRng) -> Self {
        let mut baby = Self::new(if rng.r#gen() {
            self.variant
        } else {
            partner.variant
        });
        baby.reroll = rng.gen_range(0..20) == 0;
        baby
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let pos = super::physics::block_pos(core.base.position);
        let biome = (cx.biome)(pos).unwrap_or("minecraft:plains");
        self.variant = RabbitVariant::natural(biome, cx.rng);
    }

    fn loot_facts(&self, facts: &mut EntityFacts) {
        facts
            .components
            .push(("minecraft:rabbit/variant", self.variant.name().to_owned()));
    }

    fn experience(&self, _core: &MobCore, rng: &mut StdRng) -> i32 {
        animal_experience(rng)
    }

    fn remove_when_far_away(&self) -> bool {
        false
    }
}

/// Registers the farm animals' loaders.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Cow, super::load::<Cow>);
    registry.register(EntityType::Mooshroom, super::load::<Mooshroom>);
    registry.register(EntityType::Pig, super::load::<Pig>);
    registry.register(EntityType::Sheep, super::load::<Sheep>);
    registry.register(EntityType::Chicken, super::load::<Chicken>);
    registry.register(EntityType::Rabbit, super::load::<Rabbit>);
    registry.register(EntityType::Goat, super::load::<Goat>);
    for kind in [
        EntityType::Cow,
        EntityType::Pig,
        EntityType::Sheep,
        EntityType::Chicken,
    ] {
        registry.register_spawn_rule(kind, super::spawning::animal_rule);
    }
    registry.register_spawn_rule(EntityType::Mooshroom, super::spawning::mooshroom_rule);
    registry.register_spawn_rule(EntityType::Goat, super::spawning::goat_rule);
    registry.register_spawn_rule(EntityType::Rabbit, super::spawning::rabbit_rule);
}

/// Takes a mooshroom's mushrooms, with the shearing sound in `category`.
fn mooshroom_shear(
    mooshroom: &Mooshroom,
    core: &mut MobCore,
    category: i32,
    cx: &mut TickContext,
) -> bool {
    if core.is_baby() {
        return false;
    }
    core.sound_as(SoundEvent::EntityMooshroomShear, category, 1.0, 1.0, cx);
    let mut facts = EntityFacts {
        kind: Some(core.base.kind),
        tables: Some(cx.shared.loot_tables()),
        ..Default::default()
    };
    mooshroom.loot_facts(&mut facts);
    for drop in cx
        .shared
        .block_loot()
        .entity_drops("minecraft:shearing/mooshroom", &facts, cx.rng)
    {
        let mut drop = drop;
        while drop.count() > 0 {
            core.drop_item(drop.take(1), 1.0, cx);
        }
    }
    // Turning it into a plain cow (as the game does) needs a
    // general in-place entity-type replacement path this server
    // does not have yet; shearing here only takes the mushrooms.
    true
}

/// Shears a sheep's wool off, with the shearing sound in `category`.
fn sheep_shear(sheep: &mut Sheep, core: &mut MobCore, category: i32, cx: &mut TickContext) -> bool {
    if sheep.sheared || core.is_baby() {
        return false;
    }
    core.sound_as(SoundEvent::EntitySheepShear, category, 1.0, 1.0, cx);
    let mut facts = EntityFacts {
        kind: Some(core.base.kind),
        tables: Some(cx.shared.loot_tables()),
        ..Default::default()
    };
    sheep.loot_facts(&mut facts);
    for drop in cx
        .shared
        .block_loot()
        .entity_drops("minecraft:shearing/sheep", &facts, cx.rng)
    {
        let mut drop = drop;
        while drop.count() > 0 {
            let [x, y, z] = core.base.position;
            let rng = &mut *cx.rng;
            let velocity = [
                rng.r#gen::<f64>() * 0.2 - 0.1
                    + ((rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.1) as f64,
                0.2 + (rng.r#gen::<f32>() * 0.05) as f64,
                rng.r#gen::<f64>() * 0.2 - 0.1
                    + ((rng.r#gen::<f32>() - rng.r#gen::<f32>()) * 0.1) as f64,
            ];
            let single = drop.take(1);
            cx.effects.push(Effect::DropItem {
                stack: single,
                position: [x, y + 1.0, z],
                velocity,
                pickup_delay: tuff_core::world::items::BLOCK_DROP_PICKUP_DELAY,
            });
        }
    }
    sheep.sheared = true;
    core.changed(data::SPECIES);
    true
}

mod goat;
pub use goat::*;

#[cfg(test)]
#[path = "farm_tests.rs"]
mod tests;
