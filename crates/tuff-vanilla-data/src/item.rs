use crate::provider::{Context, FloatProvider, IntProvider};
use crate::registry::{Attribute, Block, Item, MobEffect, SoundEvent};

include!(concat!(env!("OUT_DIR"), "/items.rs"));
include!(concat!(env!("OUT_DIR"), "/components.rs"));
include!(concat!(env!("OUT_DIR"), "/item_properties.rs"));

/// What a bucket item holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    /// A fluid bucket: the fluid's block, or `None` for the empty bucket.
    /// `mob` buckets (fish, axolotls) also carry an entity of that type.
    Fluid {
        fluid: Option<Block>,
        mob: Option<crate::registry::EntityType>,
    },
    /// A bucket of a block that is placed like a block item (powder snow).
    Solid {
        block: Block,
        place_sound: SoundEvent,
    },
}

/// An item's default `minecraft:food` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Food {
    pub nutrition: u32,
    /// The saturation points restored (not the pre-1.21.2 modifier).
    pub saturation: f32,
    pub can_always_eat: bool,
}

/// An item's default `minecraft:consumable` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Consumable {
    pub consume_seconds: f32,
    pub animation: UseAnimation,
    pub sound: SoundEvent,
    pub has_consume_particles: bool,
    pub on_consume_effects: &'static [ConsumeEffect],
}

impl Consumable {
    pub fn consume_ticks(&self) -> u32 {
        (self.consume_seconds * 20.0) as u32
    }
}

/// A status effect as an item or potion applies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectInstance {
    pub effect: MobEffect,
    /// Ticks; `-1` is infinite.
    pub duration: i64,
    pub amplifier: u64,
    pub ambient: bool,
    pub show_particles: bool,
    pub show_icon: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConsumeEffect {
    ApplyEffects {
        effects: &'static [EffectInstance],
        probability: f32,
    },
    RemoveEffects(&'static [MobEffect]),
    ClearAllEffects,
    TeleportRandomly {
        diameter: f32,
        directional_particles: bool,
    },
    PlaySound(SoundEvent),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UseCooldown {
    pub seconds: f32,
    /// The cooldown group; the item's own id when `None`.
    pub group: Option<&'static str>,
}

impl UseCooldown {
    pub fn ticks(&self) -> u32 {
        (self.seconds * 20.0) as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Equippable {
    pub slot: EquipmentSlot,
    pub equip_sound: SoundEvent,
    pub asset_id: Option<&'static str>,
    /// Whether right-clicking swaps it with the worn item.
    pub swappable: bool,
    pub damage_on_hurt: bool,
    pub equip_on_interact: bool,
    /// Whether a dispenser puts it on an entity in front.
    pub dispensable: bool,
    /// Only some entity types may wear it (horse armor, saddles).
    pub restricted: bool,
    /// Whether shears take it off a mob wearing it, with this sound.
    pub can_be_sheared: bool,
    pub shearing_sound: SoundEvent,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttributeModifier {
    pub attribute: Attribute,
    pub id: &'static str,
    pub amount: f64,
    pub operation: ModifierOperation,
    pub slot: EquipmentSlotGroup,
}

impl EquipmentSlotGroup {
    pub fn contains(self, slot: EquipmentSlot) -> bool {
        match self {
            Self::Any => true,
            Self::Mainhand => slot == EquipmentSlot::Mainhand,
            Self::Offhand => slot == EquipmentSlot::Offhand,
            Self::Hand => matches!(slot, EquipmentSlot::Mainhand | EquipmentSlot::Offhand),
            Self::Feet => slot == EquipmentSlot::Feet,
            Self::Legs => slot == EquipmentSlot::Legs,
            Self::Chest => slot == EquipmentSlot::Chest,
            Self::Head => slot == EquipmentSlot::Head,
            Self::Armor => matches!(
                slot,
                EquipmentSlot::Feet
                    | EquipmentSlot::Legs
                    | EquipmentSlot::Chest
                    | EquipmentSlot::Head
            ),
            Self::Body => slot == EquipmentSlot::Body,
            Self::Saddle => slot == EquipmentSlot::Saddle,
        }
    }
}

/// How much durability a shield loses per blocked hit: `base + factor *
/// damage` once `damage >= threshold`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemDamageFunction {
    pub threshold: f32,
    pub base: f32,
    pub factor: f32,
}

impl ItemDamageFunction {
    pub fn apply(&self, damage: f32) -> i32 {
        if damage < self.threshold {
            0
        } else {
            (self.base + self.factor * damage).floor() as i32
        }
    }
}

/// How much of a hit a raised shield stops: `base + factor * damage`,
/// within `horizontal_blocking_angle` degrees of where the holder looks,
/// for the listed damage types (all when `None`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DamageReduction {
    pub horizontal_blocking_angle: f32,
    pub types: Option<&'static [&'static str]>,
    pub base: f32,
    pub factor: f32,
}

impl DamageReduction {
    /// The damage this reduction stops of a `damage` hit of `kind` coming
    /// from `angle` radians off the holder's view.
    pub fn resolve(&self, kind: &str, damage: f32, angle: f64) -> f32 {
        if angle > (std::f32::consts::PI / 180.0 * self.horizontal_blocking_angle) as f64 {
            return 0.0;
        }
        if self.types.is_some_and(|types| !types.contains(&kind)) {
            return 0.0;
        }
        (self.base + self.factor * damage).clamp(0.0, damage)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlocksAttacks {
    pub block_delay_seconds: f32,
    pub disable_cooldown_scale: f32,
    pub damage_reductions: &'static [DamageReduction],
    pub item_damage: ItemDamageFunction,
    /// A damage type tag whose members ignore the block.
    pub bypassed_by: Option<&'static str>,
    pub block_sound: Option<SoundEvent>,
    pub disabled_sound: Option<SoundEvent>,
}

impl BlocksAttacks {
    pub fn block_delay_ticks(&self) -> u32 {
        (self.block_delay_seconds * 20.0).round() as u32
    }

    /// Ticks the shield is disabled for by a weapon that disables blocking
    /// for `seconds`, or 0.
    pub fn disable_ticks(&self, seconds: f32) -> u32 {
        let seconds = seconds * self.disable_cooldown_scale;
        if seconds > 0.0 {
            (seconds * 20.0).round() as u32
        } else {
            0
        }
    }

    /// The damage the shield stops of a `damage` hit of `kind` coming from
    /// `angle` radians off the holder's view.
    pub fn blocked_damage(&self, kind: &str, damage: f32, angle: f64) -> f32 {
        self.damage_reductions
            .iter()
            .map(|reduction| reduction.resolve(kind, damage, angle))
            .sum::<f32>()
            .clamp(0.0, damage)
    }
}

/// An item's default `minecraft:cooking_fuel` component: how long it burns
/// and how much it speeds cooking, both depending on the block burning it.
#[derive(Debug)]
pub struct CookingFuel {
    pub burn_time: &'static IntProvider,
    pub speed_multiplier: &'static FloatProvider,
}

impl CookingFuel {
    /// Burn duration in ticks inside `block`.
    pub fn burn_time(&self, block: Option<crate::BlockState>) -> i32 {
        let mut random = |_: u32| 0;
        self.burn_time.get(&mut Context {
            block,
            random: &mut random,
        })
    }

    pub fn speed_multiplier(&self, block: Option<crate::BlockState>) -> f32 {
        let mut random = |_: u32| 0;
        self.speed_multiplier.get(&mut Context {
            block,
            random: &mut random,
        })
    }
}

/// An item's default `minecraft:brewing_fuel` component: how many brews
/// one item powers and how fast they go.
#[derive(Debug)]
pub struct BrewingFuel {
    pub uses: &'static IntProvider,
    pub speed_multiplier: &'static FloatProvider,
}

impl BrewingFuel {
    pub fn uses(&self, block: Option<crate::BlockState>) -> i32 {
        let mut random = |_: u32| 0;
        self.uses.get(&mut Context {
            block,
            random: &mut random,
        })
    }

    pub fn speed_multiplier(&self, block: Option<crate::BlockState>) -> f32 {
        let mut random = |_: u32| 0;
        self.speed_multiplier.get(&mut Context {
            block,
            random: &mut random,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weapon {
    pub item_damage_per_attack: u64,
    pub disable_blocking_for_seconds: f32,
}

/// An item's `minecraft:piercing_weapon` component (a spear's direct stab):
/// present only to say the item ignores the normal attack (a spear only
/// deals damage through the Stab player action), plus its sounds and
/// whether it knocks back and dismounts what it hits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PiercingWeapon {
    pub deals_knockback: bool,
    pub dismounts: bool,
    pub sound: Option<SoundEvent>,
    pub hit_sound: Option<SoundEvent>,
}

/// One speed/duration gate of a `minecraft:kinetic_weapon` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KineticCondition {
    pub max_duration_ticks: u64,
    pub min_speed: f32,
    pub min_relative_speed: f32,
}

impl KineticCondition {
    pub fn test(&self, ticks_used: u64, speed: f64, relative_speed: f64, factor: f64) -> bool {
        ticks_used <= self.max_duration_ticks
            && speed >= f64::from(self.min_speed) * factor
            && relative_speed >= f64::from(self.min_relative_speed) * factor
    }
}

/// An item's `minecraft:use_effects` component.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UseEffects {
    pub can_sprint: bool,
    pub interact_vibrations: bool,
    pub speed_multiplier: f32,
}

/// An item's `minecraft:kinetic_weapon` component: what a held spear does
/// to what it touches while the user moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KineticWeapon {
    pub contact_cooldown_ticks: u64,
    pub delay_ticks: u64,
    pub dismount_conditions: Option<KineticCondition>,
    pub knockback_conditions: Option<KineticCondition>,
    pub damage_conditions: Option<KineticCondition>,
    pub forward_movement: f32,
    pub damage_multiplier: f32,
    pub sound: Option<SoundEvent>,
    pub hit_sound: Option<SoundEvent>,
}

/// An item's `minecraft:attack_range` component: how far a stab or normal
/// attack reaches, and how loosely (`hitbox_margin`) and mob targets scale
/// (`mob_factor`, for a non-player attacker).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttackRange {
    pub min_reach: f32,
    pub max_reach: f32,
    pub min_creative_reach: f32,
    pub max_creative_reach: f32,
    pub hitbox_margin: f32,
    pub mob_factor: f32,
}

impl AttackRange {
    /// The `[min, max]` reach a player attacking with this item gets,
    /// before `hitbox_margin`.
    pub fn reach(&self, creative: bool) -> (f32, f32) {
        if creative {
            (self.min_creative_reach, self.max_creative_reach)
        } else {
            (self.min_reach, self.max_reach)
        }
    }
}

/// An item's `minecraft:mob_visibility`: how much less far the listed mobs
/// see whoever wears it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MobVisibility {
    pub targeting: &'static [crate::registry::EntityType],
    pub visibility: f32,
}

fn table<T>(table: &'static [T], index: &'static [u16], item: Item) -> Option<&'static T> {
    match index[item as usize] {
        u16::MAX => None,
        i => Some(&table[i as usize]),
    }
}

/// An item's default `minecraft:tool` component.
#[derive(Debug, PartialEq)]
pub struct Tool {
    pub rules: &'static [ToolRule],
    pub default_mining_speed: f32,
    pub damage_per_block: u32,
    pub can_destroy_blocks_in_creative: bool,
}

#[derive(Debug, PartialEq)]
pub struct ToolRule {
    /// Sorted by block ID.
    pub blocks: &'static [Block],
    /// The tag `blocks` was resolved from, when it names one.
    pub tag: Option<crate::tag::Key<Block>>,
    pub speed: Option<f32>,
    pub correct_for_drops: Option<bool>,
}

impl Tool {
    /// The speed of the first rule matching `block` that sets one.
    pub fn mining_speed(&self, block: Block) -> f32 {
        self.rules
            .iter()
            .filter(|rule| rule.matches(block))
            .find_map(|rule| rule.speed)
            .unwrap_or(self.default_mining_speed)
    }

    /// The verdict of the first rule matching `block` that has one; false if
    /// no rule does.
    pub fn is_correct_for_drops(&self, block: Block) -> bool {
        self.rules
            .iter()
            .filter(|rule| rule.matches(block))
            .find_map(|rule| rule.correct_for_drops)
            .unwrap_or(false)
    }
}

impl ToolRule {
    pub fn matches(&self, block: Block) -> bool {
        self.blocks.binary_search(&block).is_ok()
    }
}

impl Item {
    pub const fn max_stack_size(self) -> u8 {
        MAX_STACK_SIZE[self as usize]
    }

    /// Durability, or `None` if the item cannot be damaged.
    pub const fn max_damage(self) -> Option<u32> {
        match MAX_DAMAGE[self as usize] {
            0 => None,
            damage => Some(damage),
        }
    }

    pub fn tool(self) -> Option<&'static Tool> {
        TOOLS.get(ITEM_TOOL[self as usize] as usize)
    }

    /// False for weapons (swords, tridents, maces) whose `tool` component
    /// forbids breaking blocks in creative mode.
    pub fn can_destroy_blocks_in_creative(self) -> bool {
        self.tool()
            .is_none_or(|tool| tool.can_destroy_blocks_in_creative)
    }

    pub fn is_correct_tool_for_drops(self, block: Block) -> bool {
        self.tool()
            .is_some_and(|tool| tool.is_correct_for_drops(block))
    }

    pub fn food(self) -> Option<&'static Food> {
        table(&FOOD_TABLE, &ITEM_FOOD, self)
    }

    /// The entity type a spawn egg's default `entity_data` spawns.
    pub fn spawns_entity(self) -> Option<crate::registry::EntityType> {
        table(&ENTITY_DATA_TABLE, &ITEM_ENTITY_DATA, self).copied()
    }

    pub fn consumable(self) -> Option<&'static Consumable> {
        table(&CONSUMABLE_TABLE, &ITEM_CONSUMABLE, self)
    }

    /// What using up one of this item leaves behind (a bowl, a bottle, ...).
    pub const fn use_remainder(self) -> Option<Item> {
        ITEM_USE_REMAINDER[self as usize]
    }

    pub fn use_cooldown(self) -> Option<&'static UseCooldown> {
        table(&USE_COOLDOWN_TABLE, &ITEM_USE_COOLDOWN, self)
    }

    pub fn equippable(self) -> Option<&'static Equippable> {
        table(&EQUIPPABLE_TABLE, &ITEM_EQUIPPABLE, self)
    }

    pub fn attribute_modifiers(self) -> &'static [AttributeModifier] {
        table(&ATTRIBUTE_MODIFIERS_TABLE, &ITEM_ATTRIBUTE_MODIFIERS, self)
            .copied()
            .unwrap_or(&[])
    }

    pub fn blocks_attacks(self) -> Option<&'static BlocksAttacks> {
        table(&BLOCKS_ATTACKS_TABLE, &ITEM_BLOCKS_ATTACKS, self)
    }

    pub fn mob_visibility(self) -> Option<(&'static [crate::registry::EntityType], f32)> {
        table(&MOB_VISIBILITY_TABLE, &ITEM_MOB_VISIBILITY, self)
            .map(|v| (v.targeting, v.visibility))
    }

    pub fn weapon(self) -> Option<&'static Weapon> {
        table(&WEAPON_TABLE, &ITEM_WEAPON, self)
    }

    /// The item's default `minecraft:piercing_weapon` component: present
    /// only for spears, whose direct attack deals no damage (they only hit
    /// through the Stab player action).
    pub fn piercing_weapon(self) -> Option<&'static PiercingWeapon> {
        table(&PIERCING_WEAPON_TABLE, &ITEM_PIERCING_WEAPON, self)
    }

    /// The item's default `minecraft:kinetic_weapon` component.
    pub fn kinetic_weapon_data(self) -> Option<&'static KineticWeapon> {
        table(&KINETIC_WEAPON_DATA_TABLE, &ITEM_KINETIC_WEAPON_DATA, self)
    }

    /// The item's default `minecraft:use_effects` component.
    pub fn use_effects(self) -> UseEffects {
        let (can_sprint, interact_vibrations, speed_multiplier) = ITEM_USE_EFFECTS[self as usize];
        UseEffects {
            can_sprint,
            interact_vibrations,
            speed_multiplier,
        }
    }

    /// The item's default `minecraft:attack_range` component.
    pub fn attack_range(self) -> Option<&'static AttackRange> {
        table(&ATTACK_RANGE_TABLE, &ITEM_ATTACK_RANGE, self)
    }

    /// The fraction of a full attack swing (0.0-1.0, as [`attack_strength`]
    /// in combat rules computes) a spear's stab needs before it does
    /// anything; 0.0 when the item has no `minecraft:minimum_attack_charge`.
    pub const fn minimum_attack_charge(self) -> f32 {
        ITEM_MINIMUM_ATTACK_CHARGE[self as usize]
    }

    /// The item's default `minecraft:damage_type`: the damage type its
    /// stab attack deals, if it names one (a spear's `minecraft:spear`).
    pub fn damage_type(self) -> Option<&'static crate::damage_type::DamageType> {
        let name = ITEM_DAMAGE_TYPE[self as usize]?;
        crate::damage_type::ALL
            .iter()
            .find(|kind| kind.name == name)
    }

    pub fn cooking_fuel(self) -> Option<&'static CookingFuel> {
        table(&COOKING_FUEL_TABLE, &ITEM_COOKING_FUEL, self)
    }

    /// The item's default `minecraft:brewing_fuel` component.
    pub fn brewing_fuel(self) -> Option<&'static BrewingFuel> {
        table(&BREWING_FUEL_TABLE, &ITEM_BREWING_FUEL, self)
    }

    /// The default `minecraft:potion_duration_scale`: how much of each
    /// potion effect's duration the item applies.
    pub const fn potion_duration_scale(self) -> f32 {
        ITEM_POTION_DURATION_SCALE[self as usize]
    }

    /// The composter layer chance provider of the item's default
    /// `minecraft:compostable` component.
    pub const fn compostable(self) -> Option<&'static IntProvider> {
        ITEM_COMPOSTABLE[self as usize]
    }

    /// Items that repair this one in an anvil, sorted by ID.
    pub fn repairable_with(self) -> &'static [Item] {
        table(&REPAIRABLE_TABLE, &ITEM_REPAIRABLE, self)
            .copied()
            .unwrap_or(&[])
    }

    /// Enchantability, or `None` if the item cannot be enchanted.
    pub const fn enchantable(self) -> Option<u8> {
        match ITEM_ENCHANTABLE[self as usize] {
            0 => None,
            value => Some(value),
        }
    }

    /// A damage type tag this item (as a dropped entity) resists.
    pub const fn damage_resistant(self) -> Option<&'static str> {
        ITEM_DAMAGE_RESISTANT[self as usize]
    }

    /// Whether the item's default components carry `minecraft:glider`,
    /// letting it be worn to glide (the elytra).
    pub const fn glider(self) -> bool {
        ITEM_GLIDER[self as usize]
    }

    /// Whether the item's default components carry
    /// `minecraft:kinetic_weapon` (spears, charged by holding use).
    pub const fn kinetic_weapon(self) -> bool {
        ITEM_KINETIC_WEAPON[self as usize]
    }

    /// The comparator output of the song its default
    /// `minecraft:jukebox_playable` names, 0 for items that play nothing.
    pub const fn jukebox_output(self) -> u8 {
        ITEM_JUKEBOX_OUTPUT[self as usize]
    }

    /// The `jukebox_song` registry id of the song its default
    /// `minecraft:jukebox_playable` names.
    pub const fn jukebox_song_id(self) -> Option<u32> {
        match ITEM_JUKEBOX_SONG[self as usize] {
            0 => None,
            id => Some(id as u32 - 1),
        }
    }

    /// The color this item applies as a dye (its default `minecraft:dye`).
    /// The food points a villager gets from eating one, if it eats it.
    pub const fn villager_food(self) -> Option<u8> {
        match ITEM_VILLAGER_FOOD[self as usize] {
            0 => None,
            nutrition => Some(nutrition),
        }
    }

    pub const fn dye(self) -> Option<DyeColor> {
        ITEM_DYE[self as usize]
    }

    /// The `minecraft:trim_material` entry this item provides in a smithing
    /// table.
    pub const fn trim_material(self) -> Option<&'static str> {
        ITEM_TRIM_MATERIAL[self as usize]
    }

    /// What one of this item leaves in a crafting grid when used up (a
    /// bucket, a bottle), with the count left.
    pub const fn crafting_remainder(self) -> Option<(Item, u8)> {
        ITEM_CRAFTING_REMAINDER[self as usize]
    }

    /// The base color of a banner item.
    pub const fn banner_color(self) -> Option<DyeColor> {
        ITEM_BANNER_COLOR[self as usize]
    }

    pub const fn bucket(self) -> Option<Bucket> {
        ITEM_BUCKET[self as usize]
    }

    /// The boat, chest boat or raft this item places.
    pub const fn boat(self) -> Option<crate::registry::EntityType> {
        ITEM_BOAT[self as usize]
    }

    /// The item a boat entity of `kind` drops: the one that places it.
    pub fn of_boat(kind: crate::registry::EntityType) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|item| item.boat() == Some(kind))
    }

    /// The bucket item that holds a mob of `kind` (fish, axolotls).
    pub fn of_bucket(kind: crate::registry::EntityType) -> Option<Self> {
        Self::ALL.iter().copied().find(
            |item| matches!(item.bucket(), Some(Bucket::Fluid { mob: Some(m), .. }) if m == kind),
        )
    }

    /// The `minecraft:block_transformer` this item uses on blocks.
    pub fn block_transformer(self) -> Option<&'static crate::block_transformer::BlockTransformer> {
        crate::block_transformer::of_item(self)
    }
}

impl Block {
    /// What honeycomb turns this block into.
    pub fn waxed(self) -> Option<Block> {
        WAXABLES
            .binary_search_by_key(&(self as usize), |(from, _)| *from as usize)
            .ok()
            .map(|index| WAXABLES[index].1)
    }

    /// The unwaxed form of a waxed block.
    pub fn unwaxed(self) -> Option<Block> {
        WAXABLES
            .iter()
            .find(|(_, to)| *to == self)
            .map(|(from, _)| *from)
    }
}

/// The comparator output of the `jukebox_song` registry entry `name`.
/// How far a bow drawn for `ticks` is pulled back, 0 to 1.
pub fn bow_power(ticks: u32) -> f32 {
    let seconds = ticks as f32 / 20.0;
    ((seconds * seconds + seconds * 2.0) / 3.0).min(1.0)
}

pub fn jukebox_song_output(name: &str) -> Option<u8> {
    JUKEBOX_SONG_OUTPUT
        .binary_search_by(|(song, _, _)| (*song).cmp(name))
        .ok()
        .map(|index| JUKEBOX_SONG_OUTPUT[index].1)
}

/// The name of the `jukebox_song` registry entry numbered `id`.
pub fn jukebox_song_name(id: u32) -> Option<&'static str> {
    JUKEBOX_SONG_OUTPUT.get(id as usize).map(|song| song.0)
}

/// The `jukebox_song` registry id and length in ticks of the song `name`.
pub fn jukebox_song(name: &str) -> Option<(u32, u32)> {
    JUKEBOX_SONG_OUTPUT
        .binary_search_by(|(song, _, _)| (*song).cmp(name))
        .ok()
        .map(|index| (index as u32, JUKEBOX_SONG_OUTPUT[index].2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jukebox_songs_are_numbered_like_the_synchronized_registry() {
        let registry = crate::data_registry::DataRegistry::get("minecraft:jukebox_song").unwrap();
        for (index, name) in registry.entries.iter().enumerate() {
            let (id, ticks) = jukebox_song(name).unwrap();
            assert_eq!(id as usize, index);
            assert!(ticks > 0);
        }
        assert_eq!(jukebox_song("minecraft:13").unwrap().1, 178 * 20);
    }

    #[test]
    fn tool_rules_follow_tiers_and_order() {
        assert!(Item::DiamondPickaxe.is_correct_tool_for_drops(Block::Obsidian));
        assert!(!Item::IronPickaxe.is_correct_tool_for_drops(Block::Obsidian));
        assert!(Item::WoodenPickaxe.is_correct_tool_for_drops(Block::Stone));
        assert!(!Item::WoodenPickaxe.is_correct_tool_for_drops(Block::IronOre));
        assert!(!Item::DiamondShovel.is_correct_tool_for_drops(Block::Stone));
        assert!(Item::Shears.is_correct_tool_for_drops(Block::Cobweb));
        assert!(Item::DiamondSword.is_correct_tool_for_drops(Block::Cobweb));
        assert!(!Item::Stick.is_correct_tool_for_drops(Block::Stone));
    }

    #[test]
    fn mining_speed_uses_first_rule_with_a_speed() {
        let pickaxe = Item::DiamondPickaxe.tool().unwrap();
        assert_eq!(pickaxe.mining_speed(Block::Stone), 8.0);
        assert_eq!(pickaxe.mining_speed(Block::Dirt), 1.0);
        let shears = Item::Shears.tool().unwrap();
        assert_eq!(shears.mining_speed(Block::Cobweb), 15.0);
        assert!(Item::Mace.tool().unwrap().rules.is_empty());
        assert!(!Item::Mace.can_destroy_blocks_in_creative());
        assert!(Item::Stick.tool().is_none());
    }

    #[test]
    fn spear_components_carry_the_stab_attack_data() {
        assert!(Item::Mace.piercing_weapon().is_none());
        let spear = Item::WoodenSpear.piercing_weapon().unwrap();
        assert!(spear.deals_knockback);
        assert!(!spear.dismounts);
        assert!(spear.sound.is_some());
        assert!(spear.hit_sound.is_some());
        let range = Item::WoodenSpear.attack_range().unwrap();
        assert_eq!((range.min_reach, range.max_reach), (2.0, 4.5));
        assert_eq!(
            (range.min_creative_reach, range.max_creative_reach),
            (2.0, 6.5)
        );
        assert_eq!(range.hitbox_margin, 0.125);
        assert_eq!(range.mob_factor, 0.5);
        assert_eq!(range.reach(false), (2.0, 4.5));
        assert_eq!(range.reach(true), (2.0, 6.5));
        assert_eq!(Item::WoodenSpear.minimum_attack_charge(), 1.0);
        assert_eq!(Item::DiamondSword.minimum_attack_charge(), 0.0);
        assert!(Item::WoodenSpear.use_effects().can_sprint);
        assert!(!Item::WoodenSpear.use_effects().interact_vibrations);
        assert_eq!(Item::Bow.use_effects().speed_multiplier, 0.2);
        assert_eq!(
            Item::WoodenSpear.damage_type().unwrap().name,
            "minecraft:spear"
        );
        assert!(Item::DiamondSword.damage_type().is_none());
        assert!(Item::DiamondSword.attack_range().is_none());
    }

    #[test]
    fn blocks_require_correct_tool_independently_of_mineable_tags() {
        let requires = |block: Block| block.default_state().requires_correct_tool_for_drops();
        assert!(requires(Block::Stone));
        assert!(requires(Block::Obsidian));
        assert!(requires(Block::Cobweb));
        assert!(!requires(Block::Dirt));
        assert!(!requires(Block::Ice));
        assert!(!requires(Block::OakLog));
    }

    #[test]
    fn food_and_consumables_come_from_components() {
        let apple = Item::Apple.food().unwrap();
        assert_eq!((apple.nutrition, apple.saturation), (4, 2.4));
        assert!(!apple.can_always_eat);
        assert!(Item::GoldenApple.food().unwrap().can_always_eat);
        let consumable = Item::Apple.consumable().unwrap();
        assert_eq!(consumable.consume_ticks(), 32);
        assert_eq!(consumable.animation, UseAnimation::Eat);
        assert_eq!(consumable.sound, SoundEvent::EntityGenericEat);
        assert_eq!(Item::DriedKelp.consumable().unwrap().consume_ticks(), 16);
        assert_eq!(
            Item::Potion.consumable().unwrap().animation,
            UseAnimation::Drink
        );
        assert_eq!(Item::Stone.food(), None);
        assert_eq!(Item::MushroomStew.use_remainder(), Some(Item::Bowl));
        assert_eq!(Item::ChorusFruit.use_cooldown().unwrap().ticks(), 20);
        let golden = Item::GoldenApple.consumable().unwrap().on_consume_effects;
        assert!(matches!(
            golden[0],
            ConsumeEffect::ApplyEffects { effects, probability } if probability == 1.0
                && effects[0].effect == MobEffect::Regeneration
                && effects[0].amplifier == 1
                && effects[0].duration == 100
        ));
        assert!(matches!(
            Item::MilkBucket.consumable().unwrap().on_consume_effects[0],
            ConsumeEffect::ClearAllEffects
        ));
        assert!(matches!(
            Item::HoneyBottle.consumable().unwrap().on_consume_effects[0],
            ConsumeEffect::RemoveEffects(&[MobEffect::Poison])
        ));
    }

    #[test]
    fn equipment_and_weapon_components() {
        let chestplate = Item::IronChestplate.equippable().unwrap();
        assert_eq!(chestplate.slot, EquipmentSlot::Chest);
        assert!(chestplate.swappable && chestplate.damage_on_hurt);
        let armor = Item::IronChestplate
            .attribute_modifiers()
            .iter()
            .find(|m| m.attribute == Attribute::Armor)
            .unwrap();
        assert_eq!(armor.amount, 6.0);
        assert!(armor.slot.contains(EquipmentSlot::Chest));
        assert!(!armor.slot.contains(EquipmentSlot::Head));
        let shield = Item::Shield.blocks_attacks().unwrap();
        assert_eq!(shield.block_delay_seconds, 0.25);
        assert_eq!(shield.item_damage.apply(2.0), 0);
        assert_eq!(shield.item_damage.apply(5.0), 6);
        assert_eq!(shield.bypassed_by, Some("minecraft:bypasses_shield"));
        assert_eq!(
            Item::Shield.equippable().unwrap().slot,
            EquipmentSlot::Offhand
        );
        assert!(!Item::Shield.equippable().unwrap().swappable);
        assert_eq!(Item::StoneHoe.weapon().unwrap().item_damage_per_attack, 2);
        assert_eq!(
            Item::DiamondSword.weapon().unwrap().item_damage_per_attack,
            1
        );
        assert_eq!(Item::DiamondSword.enchantable(), Some(10));
        assert_eq!(Item::Stick.enchantable(), None);
        assert!(
            Item::DiamondSword
                .repairable_with()
                .contains(&Item::Diamond)
        );
        assert!(Item::DiamondSword.repairable_with().is_sorted());
        assert_eq!(
            Item::AncientDebris.damage_resistant(),
            Some("minecraft:is_fire")
        );
    }

    #[test]
    fn blaze_powder_fuels_twenty_brews() {
        let fuel = Item::BlazePowder.brewing_fuel().unwrap();
        let stand = Some(Block::BrewingStand.default_state());
        assert_eq!(fuel.uses(stand), 20);
        assert_eq!(fuel.speed_multiplier(stand), 1.0);
        assert!(Item::Coal.brewing_fuel().is_none());
        assert_eq!(Item::LingeringPotion.potion_duration_scale(), 0.25);
        assert_eq!(Item::Potion.potion_duration_scale(), 1.0);
    }

    #[test]
    fn cooking_fuel_follows_the_burning_block() {
        let furnace = Some(Block::Furnace.default_state());
        let blast = Some(Block::BlastFurnace.default_state());
        let smoker = Some(Block::Smoker.default_state());
        let burn = |item: Item, block| item.cooking_fuel().unwrap().burn_time(block);
        assert_eq!(burn(Item::Coal, furnace), 1600);
        assert_eq!(burn(Item::Coal, blast), 800);
        assert_eq!(burn(Item::Charcoal, smoker), 800);
        assert_eq!(burn(Item::CoalBlock, furnace), 16000);
        assert_eq!(burn(Item::CoalBlock, blast), 8000);
        assert_eq!(burn(Item::LavaBucket, furnace), 20000);
        assert_eq!(burn(Item::LavaBucket, smoker), 10000);
        assert_eq!(burn(Item::OakBoat, furnace), 1200);
        assert_eq!(burn(Item::OakBoat, blast), 600);
        assert_eq!(burn(Item::Stick, furnace), 100);
        assert_eq!(burn(Item::Bamboo, furnace), 50);
        assert_eq!(burn(Item::Bamboo, blast), 25);
        let speed = |item: Item, block| item.cooking_fuel().unwrap().speed_multiplier(block);
        assert_eq!(speed(Item::Coal, furnace), 1.0);
        assert_eq!(speed(Item::Coal, blast), 2.0);
        assert_eq!(speed(Item::LavaBucket, smoker), 2.0);
        assert!(Item::Stone.cooking_fuel().is_none());
        assert!(Item::Bucket.cooking_fuel().is_none());
        assert!(Item::Apple.compostable().is_some());
        assert!(Item::Stone.compostable().is_none());
    }

    #[test]
    fn only_the_elytra_carries_the_glider_component() {
        assert!(Item::Elytra.glider());
        assert!(!Item::Stone.glider());
        assert!(!Item::DiamondChestplate.glider());
    }

    #[test]
    fn spears_carry_the_kinetic_weapon_component() {
        assert!(Item::IronSpear.kinetic_weapon());
        let kinetic = Item::IronSpear.kinetic_weapon_data().unwrap();
        assert_eq!(kinetic.delay_ticks, 12);
        assert_eq!(kinetic.damage_conditions.unwrap().min_relative_speed, 4.6);
        assert!(Item::IronSword.kinetic_weapon_data().is_none());
        assert!(!Item::IronSword.kinetic_weapon());
    }

    #[test]
    fn rule_blocks_are_sorted() {
        for tool in &TOOLS {
            for rule in tool.rules {
                assert!(rule.blocks.is_sorted());
            }
        }
    }
}
