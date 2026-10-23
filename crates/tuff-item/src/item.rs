//! Item stacks as the server stores them: an item, a count and the data
//! components that differ from the item's defaults
//! (https://minecraft.wiki/w/Data_component_format). Components the server
//! simulates are typed; everything else is kept as the NBT it was loaded
//! with, so saved stacks round-trip without loss.

pub use behavior::{StackConsumable, StackConsumeEffect};
pub mod bundle;
pub mod decor;
pub mod display;
pub mod enchanting;
pub mod equippable;
pub mod gear;
pub mod hash;
mod history;
pub mod identity;
mod ledger;
mod picture;
mod view;
pub use history::History;
pub use ledger::{BurnReason, LedgerTotals, MintReason, SlotsCow, conservation};
pub use picture::{AnyItem, Counted, ItemPicture, Mode, Picture};
pub use view::{AsItemView, ComponentsSnapshot, ItemSnapshot, ItemView, snapshot_slots};

use std::fmt;
use std::ops::{Deref, DerefMut};

use rand::Rng;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::item::{DyeColor, EquipmentSlotGroup, ModifierOperation, Weapon};
use tuff_vanilla_data::registry::{
    Attribute, BlockEntityType, DataComponentType, EntityType, MobEffect, Potion,
};

use crate::item::bundle::BundleContents;
use crate::item::decor::{
    ArmorTrim, BannerLayer, Filterable, FireworkExplosion, Fireworks, WrittenBook,
};
use crate::item::gear::{AttributeModifier, Tool, ToolRef};
use crate::item::hash::Canon;
use crate::text::TextComponent;
use tuff_nbt::{Compound, Tag};

const MAX_COUNT: i32 = 99;
/// How long an arm swing lasts when the item does not say.
pub const DEFAULT_SWING_TICKS: i32 = 6;

/// The arm swing an attack plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwingKind {
    None = 0,
    Whack = 1,
    Stab = 2,
}
const MAX_ENCHANTMENT_LEVEL: i64 = 255;
pub const UNBREAKING: &str = "minecraft:unbreaking";
pub const MENDING: &str = "minecraft:mending";
const DAMAGE: &str = "minecraft:damage";
const MAX_DAMAGE: &str = "minecraft:max_damage";
const MAX_STACK_SIZE: &str = "minecraft:max_stack_size";
const ATTRIBUTE_MODIFIERS: &str = "minecraft:attribute_modifiers";
const WEAPON: &str = "minecraft:weapon";
const TOOL: &str = "minecraft:tool";
const REMOVED_PREFIX: char = '!';
pub const MAX_CONTAINER_SLOTS: usize = 256;
pub const MAX_CHARGED_PROJECTILES: usize = 1024;

/// Component types whose wire form is their NBT.
const NBT_ON_WIRE: [DataComponentType; 3] = [
    DataComponentType::CustomData,
    DataComponentType::ItemName,
    DataComponentType::BucketEntityData,
];
/// Component types holding one item stack template (`{id, count,
/// components}`), sent as item, count and component patch.
const TEMPLATE_ON_WIRE: [DataComponentType; 1] = [DataComponentType::SulfurCubeContent];
/// Component types stored as a compound with an `id` field, sent as that
/// type's registry id followed by the compound without `id`.
const TYPED_NBT_ON_WIRE: [DataComponentType; 2] = [
    DataComponentType::EntityData,
    DataComponentType::BlockEntityData,
];

/// Items: an item, a count and the components that differ from the item's
/// defaults. `ItemStack` (the default, [`Counted`]) is real items;
/// [`ItemPicture`] only shows them (see [`Mode`]).
#[must_use = "items must be stored, moved or burned"]
pub struct ItemStack<M: Mode = Counted> {
    item: Item,
    count: u8,
    /// Public: editing components never changes how many items exist.
    /// Stacks nested in a counted stack's components (containers, bundles,
    /// charged projectiles) are counted stacks of their own, moved in and
    /// out, so an edit that drops one is reported as lost.
    pub components: Components<M>,
    extra: M::Extra,
}

impl<M: Mode> fmt::Debug for ItemStack<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("ItemStack");
        out.field("item", &self.item)
            .field("count", &self.count)
            .field("components", &self.components);
        if size_of::<M::Extra>() != 0 {
            out.field("id", &self.extra);
        }
        out.finish()
    }
}

/// Same item, count and components, whatever the kind of either stack.
impl<M: Mode, N: Mode> PartialEq<ItemStack<N>> for ItemStack<M> {
    fn eq(&self, other: &ItemStack<N>) -> bool {
        self.item == other.item && self.count == other.count && self.components == other.components
    }
}

/// Whether two optional slots hold the same, whatever kind of stack each
/// holds.
pub fn same_slot<M: Mode, N: Mode>(a: Option<&ItemStack<M>>, b: Option<&ItemStack<N>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        (a, b) => a.is_none() && b.is_none(),
    }
}

impl<M: Mode> Drop for ItemStack<M> {
    fn drop(&mut self) {
        M::on_drop(self);
    }
}

/// A stack's component patch, stored sparsely: a stack without components
/// carries no allocation. Reads go through `Deref` to an empty patch;
/// writes through `DerefMut` allocate it. An empty patch equals none.
pub struct Components<M: Mode = Counted>(Option<Box<ComponentPatch<M>>>);

impl<M: Mode> Default for Components<M> {
    fn default() -> Self {
        Self(None)
    }
}

/// A use cooldown of a stack: its length and the group it shares, the
/// item's own id when `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct StackCooldown {
    pub ticks: u32,
    pub group: Option<String>,
}

/// The values a stack sets on top of its item's defaults and the defaults
/// removed from it. Nested stacks are of the same kind as the stack.
#[derive(Debug, Default)]
pub struct ComponentPatch<M: Mode = Counted> {
    pub damage: Option<u32>,
    pub max_damage: Option<u32>,
    pub unbreakable: bool,
    pub custom_name: Option<Box<TextComponent>>,
    pub enchantments: Option<Enchantments>,
    pub stored_enchantments: Option<Enchantments>,
    pub potion_contents: Option<Box<PotionContents>>,
    pub repair_cost: Option<u32>,
    /// The Bad Omen level an ominous bottle gives, less one.
    pub ominous_bottle_amplifier: Option<u32>,
    pub map_id: Option<u32>,
    /// The contents of a container item (shulker boxes): the slots up to
    /// the last occupied one.
    pub container: Option<Vec<Option<ItemStack<M>>>>,
    /// What a loaded crossbow will fire.
    pub charged_projectiles: Option<Vec<ItemStack<M>>>,
    /// What a bundle holds; absent when empty.
    pub bundle_contents: Option<Box<BundleContents<M>>>,
    /// RGB of dyed leather.
    pub dyed_color: Option<i32>,
    pub base_color: Option<DyeColor>,
    pub banner_patterns: Option<Vec<BannerLayer>>,
    pub trim: Option<Box<ArmorTrim>>,
    pub fireworks: Option<Box<Fireworks>>,
    pub firework_explosion: Option<Box<FireworkExplosion>>,
    pub writable_book: Option<Vec<Filterable<String>>>,
    pub written_book: Option<Box<WrittenBook>>,
    pub attribute_modifiers: Option<Vec<AttributeModifier>>,
    pub tool: Option<Box<Tool>>,
    pub weapon: Option<Weapon>,
    /// A suspicious stew's effects when eaten (set by the recipe from the
    /// flower it was crafted with).
    pub suspicious_stew_effects: Option<Vec<SuspiciousEffect>>,
    /// Components this server does not model (or could not parse), by id,
    /// as stored.
    pub other: Compound,
    /// Ids of default components removed from the item, sorted.
    pub removed: Vec<String>,
}

fn slots_eq<M: Mode, N: Mode>(a: &[Option<ItemStack<M>>], b: &[Option<ItemStack<N>>]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|pair| match pair {
            (Some(a), Some(b)) => a == b,
            (None, None) => true,
            _ => false,
        })
}

fn option_eq<A: PartialEq<B> + ?Sized, B: ?Sized>(a: Option<&A>, b: Option<&B>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a == b,
        (None, None) => true,
        _ => false,
    }
}

impl<M: Mode, N: Mode> PartialEq<ComponentPatch<N>> for ComponentPatch<M> {
    fn eq(&self, other: &ComponentPatch<N>) -> bool {
        let Self {
            damage,
            max_damage,
            unbreakable,
            custom_name,
            enchantments,
            stored_enchantments,
            potion_contents,
            repair_cost,
            ominous_bottle_amplifier,
            map_id,
            container,
            charged_projectiles,
            bundle_contents,
            dyed_color,
            base_color,
            banner_patterns,
            trim,
            fireworks,
            firework_explosion,
            writable_book,
            written_book,
            attribute_modifiers,
            tool,
            weapon,
            suspicious_stew_effects,
            other: unmodelled,
            removed,
        } = self;
        *damage == other.damage
            && *max_damage == other.max_damage
            && *unbreakable == other.unbreakable
            && *custom_name == other.custom_name
            && *enchantments == other.enchantments
            && *stored_enchantments == other.stored_enchantments
            && *potion_contents == other.potion_contents
            && *repair_cost == other.repair_cost
            && *ominous_bottle_amplifier == other.ominous_bottle_amplifier
            && *map_id == other.map_id
            && match (container, &other.container) {
                (Some(a), Some(b)) => slots_eq(a, b),
                (a, b) => a.is_none() && b.is_none(),
            }
            && option_eq(
                charged_projectiles.as_deref(),
                other.charged_projectiles.as_deref(),
            )
            && option_eq(bundle_contents.as_deref(), other.bundle_contents.as_deref())
            && *dyed_color == other.dyed_color
            && *base_color == other.base_color
            && *banner_patterns == other.banner_patterns
            && *trim == other.trim
            && *fireworks == other.fireworks
            && *firework_explosion == other.firework_explosion
            && *writable_book == other.writable_book
            && *written_book == other.written_book
            && *attribute_modifiers == other.attribute_modifiers
            && *tool == other.tool
            && *weapon == other.weapon
            && *suspicious_stew_effects == other.suspicious_stew_effects
            && *unmodelled == other.other
            && *removed == other.removed
    }
}

impl<M: Mode> ComponentPatch<M> {
    /// This patch with every value cloned and each nested stack copied
    /// through `nested`, or left out without it.
    #[allow(clippy::clone_on_copy)] // every value is copied the same way
    fn copy_nested<N: Mode, F: FnMut(&ItemStack<M>) -> ItemStack<N>>(
        &self,
        mut nested: Option<F>,
    ) -> ComponentPatch<N> {
        let Self {
            damage,
            max_damage,
            unbreakable,
            custom_name,
            enchantments,
            stored_enchantments,
            potion_contents,
            repair_cost,
            ominous_bottle_amplifier,
            map_id,
            container,
            charged_projectiles,
            bundle_contents,
            dyed_color,
            base_color,
            banner_patterns,
            trim,
            fireworks,
            firework_explosion,
            writable_book,
            written_book,
            attribute_modifiers,
            tool,
            weapon,
            suspicious_stew_effects,
            other,
            removed,
        } = self;
        ComponentPatch {
            damage: damage.clone(),
            max_damage: max_damage.clone(),
            unbreakable: unbreakable.clone(),
            custom_name: custom_name.clone(),
            enchantments: enchantments.clone(),
            stored_enchantments: stored_enchantments.clone(),
            potion_contents: potion_contents.clone(),
            repair_cost: repair_cost.clone(),
            ominous_bottle_amplifier: ominous_bottle_amplifier.clone(),
            map_id: map_id.clone(),
            container: nested
                .as_mut()
                .zip(container.as_ref())
                .map(|(nested, slots)| {
                    slots
                        .iter()
                        .map(|slot| slot.as_ref().map(&mut *nested))
                        .collect()
                }),
            charged_projectiles: nested
                .as_mut()
                .zip(charged_projectiles.as_ref())
                .map(|(nested, stacks)| stacks.iter().map(nested).collect()),
            bundle_contents: nested
                .as_mut()
                .zip(bundle_contents.as_ref())
                .map(|(nested, contents)| Box::new(contents.copy_nested(nested))),
            dyed_color: dyed_color.clone(),
            base_color: base_color.clone(),
            banner_patterns: banner_patterns.clone(),
            trim: trim.clone(),
            fireworks: fireworks.clone(),
            firework_explosion: firework_explosion.clone(),
            writable_book: writable_book.clone(),
            written_book: written_book.clone(),
            attribute_modifiers: attribute_modifiers.clone(),
            tool: tool.clone(),
            weapon: weapon.clone(),
            suspicious_stew_effects: suspicious_stew_effects.clone(),
            other: other.clone(),
            removed: removed.clone(),
        }
    }

    /// This patch with each nested stack moved through `nested`.
    fn map_nested<N: Mode>(
        self,
        mut nested: impl FnMut(ItemStack<M>) -> ItemStack<N>,
    ) -> ComponentPatch<N> {
        let Self {
            damage,
            max_damage,
            unbreakable,
            custom_name,
            enchantments,
            stored_enchantments,
            potion_contents,
            repair_cost,
            ominous_bottle_amplifier,
            map_id,
            container,
            charged_projectiles,
            bundle_contents,
            dyed_color,
            base_color,
            banner_patterns,
            trim,
            fireworks,
            firework_explosion,
            writable_book,
            written_book,
            attribute_modifiers,
            tool,
            weapon,
            suspicious_stew_effects,
            other,
            removed,
        } = self;
        ComponentPatch {
            damage,
            max_damage,
            unbreakable,
            custom_name,
            enchantments,
            stored_enchantments,
            potion_contents,
            repair_cost,
            ominous_bottle_amplifier,
            map_id,
            container: container.map(|slots| {
                slots
                    .into_iter()
                    .map(|slot| slot.map(&mut nested))
                    .collect()
            }),
            charged_projectiles: charged_projectiles
                .map(|stacks| stacks.into_iter().map(&mut nested).collect()),
            bundle_contents: bundle_contents
                .map(|contents| Box::new(contents.map_nested(&mut nested))),
            dyed_color,
            base_color,
            banner_patterns,
            trim,
            fireworks,
            firework_explosion,
            writable_book,
            written_book,
            attribute_modifiers,
            tool,
            weapon,
            suspicious_stew_effects,
            other,
            removed,
        }
    }

    /// A picture of this patch, nested stacks included.
    pub fn picture(&self) -> ComponentPatch<Picture> {
        self.copy_nested(Some(|stack: &ItemStack<M>| ItemPicture::of(stack)))
    }

    /// The stacks nested directly in this patch.
    pub fn nested(&self) -> impl Iterator<Item = &ItemStack<M>> {
        let container = self.container.iter().flatten().flatten();
        let charged = self.charged_projectiles.iter().flatten();
        let bundle = self
            .bundle_contents
            .iter()
            .flat_map(|contents| contents.items());
        container.chain(charged).chain(bundle)
    }

    pub fn is_empty(&self) -> bool {
        *self == *M::empty_patch()
    }
}

impl<M: Mode> Components<M> {
    /// Drops the allocation when the patch is empty.
    pub fn compact(&mut self) {
        if self.0.as_deref().is_some_and(ComponentPatch::is_empty) {
            self.0 = None;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.as_deref().is_none_or(ComponentPatch::is_empty)
    }

    pub fn into_patch(self) -> ComponentPatch<M> {
        self.0.map(|patch| *patch).unwrap_or_default()
    }

    /// Removes an unmodelled component without allocating a patch when
    /// there is none.
    pub fn remove_other(&mut self, id: &str) -> Option<Tag> {
        let removed = self.0.as_deref_mut()?.other.remove(id);
        self.compact();
        removed
    }

    /// Whether the patch currently holds an allocation.
    pub fn is_allocated(&self) -> bool {
        self.0.is_some()
    }

    /// A picture of these components, nested stacks included.
    pub fn picture(&self) -> Components<Picture> {
        self.copy_nested(Some(|stack: &ItemStack<M>| ItemPicture::of(stack)))
    }

    fn copy_nested<N: Mode, F: FnMut(&ItemStack<M>) -> ItemStack<N>>(
        &self,
        nested: Option<F>,
    ) -> Components<N> {
        match self.0.as_deref() {
            Some(patch) if !patch.is_empty() => {
                Components(Some(Box::new(patch.copy_nested(nested))))
            }
            _ => Components(None),
        }
    }

    /// A copy without the nested stacks.
    pub(crate) fn without_nested(&self) -> Self {
        let mut copy = self.copy_nested(None::<fn(&ItemStack<M>) -> ItemStack<M>>);
        copy.compact();
        copy
    }

    fn map_nested<N: Mode>(
        self,
        nested: impl FnMut(ItemStack<M>) -> ItemStack<N>,
    ) -> Components<N> {
        Components(self.0.map(|patch| Box::new(patch.map_nested(nested))))
    }
}

impl Components {
    /// No components, on a counted stack. [`Default`] gives the same for
    /// either kind where the kind is known.
    pub fn new() -> Self {
        Self(None)
    }
}

impl ComponentPatch {
    /// An empty patch for a counted stack.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<M: Mode> Deref for Components<M> {
    type Target = ComponentPatch<M>;

    fn deref(&self) -> &ComponentPatch<M> {
        self.0.as_deref().unwrap_or(M::empty_patch())
    }
}

impl<M: Mode> DerefMut for Components<M> {
    fn deref_mut(&mut self) -> &mut ComponentPatch<M> {
        self.0.get_or_insert_with(Box::default)
    }
}

impl<M: Mode> From<ComponentPatch<M>> for Components<M> {
    fn from(patch: ComponentPatch<M>) -> Self {
        let mut components = Self(Some(Box::new(patch)));
        components.compact();
        components
    }
}

impl<M: Mode, N: Mode> PartialEq<Components<N>> for Components<M> {
    fn eq(&self, other: &Components<N>) -> bool {
        match (self.0.as_deref(), other.0.as_deref()) {
            (Some(a), Some(b)) => a == b,
            (Some(patch), None) => patch.is_empty(),
            (None, Some(patch)) => patch.is_empty(),
            (None, None) => true,
        }
    }
}

impl<M: Mode> fmt::Debug for Components<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

/// One attribute modifier of a stack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModifierRef<'a> {
    pub attribute: Attribute,
    pub id: &'a str,
    pub amount: f64,
    pub operation: ModifierOperation,
    pub slot: EquipmentSlotGroup,
}

/// Enchantment ids and levels, sorted by id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Enchantments(Vec<(String, i32)>);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PotionContents {
    pub potion: Option<Potion>,
    pub custom_color: Option<i32>,
    pub custom_effects: Vec<PotionEffect>,
    pub custom_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PotionEffect {
    pub effect: MobEffect,
    pub details: EffectDetails,
}

/// One effect a suspicious stew applies when eaten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuspiciousEffect {
    pub effect: MobEffect,
    pub duration: i32,
}

impl SuspiciousEffect {
    /// Vanilla's default duration when a stew's entry omits it.
    const DEFAULT_DURATION: i32 = 160;

    fn from_nbt(tag: &Tag) -> Option<Self> {
        let map = tag.as_compound()?;
        let effect = MobEffect::from_name(map.get_str("id")?)?;
        let duration = match map.get("duration") {
            Some(duration) => as_i64(duration)? as i32,
            None => Self::DEFAULT_DURATION,
        };
        Some(Self { effect, duration })
    }

    fn to_nbt(self) -> Compound {
        let mut nbt = Compound::new().with("id", self.effect.name());
        if self.duration != Self::DEFAULT_DURATION {
            nbt.insert("duration", self.duration);
        }
        nbt
    }

    fn canon(self) -> Canon {
        let mut entries = vec![("id".to_owned(), Canon::str(self.effect.name()))];
        if self.duration != Self::DEFAULT_DURATION {
            entries.push(("duration".to_owned(), Canon::Int(self.duration)));
        }
        Canon::Map(entries)
    }
}

pub fn suspicious_stew_effects_from_nbt(value: &Tag) -> Option<Vec<SuspiciousEffect>> {
    value
        .as_list()?
        .iter()
        .map(SuspiciousEffect::from_nbt)
        .collect()
}

pub fn suspicious_stew_effects_to_nbt(effects: &[SuspiciousEffect]) -> Tag {
    Tag::List(effects.iter().map(|e| Tag::Compound(e.to_nbt())).collect())
}

/// One consume effect of a `death_protection` component.
#[derive(Debug, Clone, PartialEq)]
pub enum DeathEffect {
    ClearAllEffects,
    ApplyEffects {
        effects: Vec<(MobEffect, i64, u8)>,
        probability: f32,
    },
}

impl DeathEffect {
    fn apply(effect: MobEffect, duration: i64, amplifier: u8) -> Self {
        Self::ApplyEffects {
            effects: vec![(effect, duration, amplifier)],
            probability: 1.0,
        }
    }

    fn from_nbt(tag: &Tag) -> Option<Self> {
        let map = tag.as_compound()?;
        match map.get_str("type")? {
            "minecraft:clear_all_effects" | "clear_all_effects" => Some(Self::ClearAllEffects),
            "minecraft:apply_effects" | "apply_effects" => {
                let effects = map
                    .get("effects")?
                    .as_list()?
                    .iter()
                    .filter_map(|entry| {
                        let entry = entry.as_compound()?;
                        let effect = MobEffect::from_name(entry.get_str("id")?)?;
                        let duration = entry.get("duration").and_then(as_i64).unwrap_or(0);
                        let amplifier = entry.get("amplifier").and_then(as_i64).unwrap_or(0);
                        Some((effect, duration, amplifier.clamp(0, 255) as u8))
                    })
                    .collect();
                let probability = map
                    .get("probability")
                    .and_then(Tag::as_float)
                    .unwrap_or(1.0);
                Some(Self::ApplyEffects {
                    effects,
                    probability,
                })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectDetails {
    pub amplifier: u8,
    pub duration: i32,
    pub ambient: bool,
    pub show_particles: bool,
    pub show_icon: bool,
    pub hidden_effect: Option<Box<EffectDetails>>,
}

/// What applying durability loss did to a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hurt {
    Unchanged,
    Damaged,
    /// The stack broke: one item was consumed.
    Broken,
}

/// A component as it is sent in a Slot, borrowed from a stack.
#[derive(Debug, PartialEq)]
pub enum NetworkComponent<'a, M: Mode = Counted> {
    Damage(u32),
    MaxDamage(u32),
    Unbreakable,
    CustomName(&'a TextComponent),
    Enchantments(&'a Enchantments),
    StoredEnchantments(&'a Enchantments),
    PotionContents(&'a PotionContents),
    RepairCost(u32),
    OminousBottleAmplifier(u32),
    MapId(u32),
    Container(&'a [Option<ItemStack<M>>]),
    ChargedProjectiles(&'a [ItemStack<M>]),
    BundleContents(&'a BundleContents<M>),
    DyedColor(i32),
    BaseColor(DyeColor),
    BannerPatterns(&'a [BannerLayer]),
    Trim(&'a ArmorTrim),
    Fireworks(&'a Fireworks),
    FireworkExplosion(&'a FireworkExplosion),
    WritableBook(&'a [Filterable<String>]),
    WrittenBook(&'a WrittenBook),
    Lore(&'a [Tag]),
    AttributeModifiers(&'a [AttributeModifier]),
    Tool(&'a Tool),
    Weapon(Weapon),
    SuspiciousStewEffects(&'a [SuspiciousEffect]),
    Nbt(DataComponentType, &'a Tag),
    /// `pot_decorations`: an optional item template per side, in
    /// [`POT_SIDES`] order.
    PotDecorations(Box<PotSides>),
    /// `block_state`: property names to values.
    BlockState(&'a Compound),
    /// A typed entity-data compound (including its `id`) with the registry
    /// id of that type.
    TypedNbt(DataComponentType, u32, &'a Compound),
    MaxStackSize(u32),
    /// `item_model` or `tooltip_style`, with its namespace.
    Identifier(DataComponentType, std::borrow::Cow<'a, str>),
    /// `rarity`, by index into [`display::RARITIES`].
    Rarity(u32),
    GlintOverride(bool),
    CustomModelData(display::ModelData),
    TooltipDisplay(display::TooltipDisplay),
    /// `custom_data` as clients see it, when it differs from the stored
    /// value.
    ClientData(Compound),
    LodestoneTracker(LodestoneTracker),
    /// A component holding one item stack template.
    Template(DataComponentType, Box<ItemPicture>),
}

/// The sides of a decorated pot, as `pot_decorations` and the pot's
/// `sherds` name them.
pub const POT_SIDES: [&str; 4] = ["back", "left", "right", "front"];

/// A decorated pot's decorations: an item per side in [`POT_SIDES`] order.
pub type PotSides = [Option<ItemPicture>; 4];

/// Reads `pot_decorations` (or a pot's `sherds`): one item template per
/// side, each `{id, count, components}` or a bare item id. A plain list of item
/// ids is read too, missing sides as bricks.
/// `None` when any side names an unknown item.
pub fn pot_decorations_from_nbt(tag: &Tag) -> Option<PotSides> {
    let template = |tag: &Tag| match tag {
        Tag::String(id) => Item::from_name(id)
            .filter(|item| *item != Item::Air)
            .map(|item| ItemPicture::new(item, 1)),
        Tag::Compound(nbt) => ItemPicture::from_nbt(nbt),
        _ => None,
    };
    let mut sides: PotSides = Default::default();
    match tag {
        Tag::Compound(map) => {
            for (side, name) in sides.iter_mut().zip(POT_SIDES) {
                if let Some(tag) = map.get(name) {
                    *side = Some(template(tag)?);
                }
            }
        }
        Tag::List(ids) if ids.len() <= POT_SIDES.len() => {
            for (index, side) in sides.iter_mut().enumerate() {
                *side = Some(match ids.get(index) {
                    Some(tag) => template(tag)?,
                    None => ItemPicture::new(Item::Brick, 1),
                });
            }
        }
        _ => return None,
    }
    Some(sides)
}

/// `pot_decorations` in its saved form, leaving out empty sides.
pub fn pot_decorations_to_nbt(sides: &PotSides) -> Tag {
    Tag::Compound(
        POT_SIDES
            .iter()
            .zip(sides)
            .filter_map(|(name, side)| Some((*name, side.as_ref()?.to_nbt())))
            .collect(),
    )
}

/// The registry id a typed entity-data component's `id` names, if this
/// server knows it.
pub fn typed_entity_id(kind: DataComponentType, tag: &Compound) -> Option<u32> {
    let name = tag.get("id")?.as_str()?;
    match kind {
        DataComponentType::EntityData => EntityType::from_name(name).map(EntityType::id),
        DataComponentType::BlockEntityData => {
            BlockEntityType::from_name(name).map(BlockEntityType::id)
        }
        _ => None,
    }
}

/// The `id` a typed entity-data component stores for a registry id.
pub fn typed_entity_name(kind: DataComponentType, id: u32) -> Option<&'static str> {
    match kind {
        DataComponentType::EntityData => EntityType::from_id(id).map(EntityType::name),
        DataComponentType::BlockEntityData => {
            BlockEntityType::from_id(id).map(BlockEntityType::name)
        }
        _ => None,
    }
}

impl<M: Mode> ItemStack<M> {
    #[track_caller]
    pub fn with_components(mut self, components: impl Into<Components<M>>) -> Self {
        self.components = components.into();
        self.components.compact();
        self.note(format_args!("set components"));
        self
    }

    pub fn max_stack_size(&self) -> u8 {
        self.components
            .other
            .get_int(MAX_STACK_SIZE)
            .map_or(self.item.max_stack_size(), |size| {
                size.clamp(1, MAX_COUNT) as u8
            })
    }

    /// Whether the stack carries any (non-stored) enchantment.
    pub fn is_enchanted(&self) -> bool {
        self.components
            .enchantments
            .as_ref()
            .is_some_and(|e| !e.entries().is_empty())
    }

    /// Whether `other` can merge into this stack: same item, same
    /// components.
    pub fn stacks_with<N: Mode>(&self, other: &ItemStack<N>) -> bool {
        self.item == other.item && self.components == other.components
    }

    /// Durability, or `None` if the stack cannot be damaged.
    pub fn max_damage(&self) -> Option<u32> {
        if self.components.is_removed(MAX_DAMAGE) {
            return None;
        }
        self.components.max_damage.or(self.item.max_damage())
    }

    /// Whether the stack carries a `damage` component (its own or the
    /// item's default), which repair needs.
    pub fn has_damage(&self) -> bool {
        self.components.damage.is_some()
            || (self.item.max_damage().is_some() && !self.components.is_removed(DAMAGE))
    }

    pub fn is_damageable(&self) -> bool {
        self.max_damage().is_some() && !self.components.unbreakable && self.has_damage()
    }

    /// Whether wearing this stack lets a player glide: it carries the
    /// `minecraft:glider` component and its very next durability hit would
    /// not break it.
    pub fn can_glide(&self) -> bool {
        self.item.glider()
            && !(self.is_damageable()
                && self.damage() >= self.max_damage().unwrap_or(0).saturating_sub(1))
    }

    pub fn damage(&self) -> u32 {
        self.components
            .damage
            .unwrap_or(0)
            .min(self.max_damage().unwrap_or(0))
    }

    #[track_caller]
    pub fn set_damage(&mut self, value: u32) {
        let value = value.min(self.max_damage().unwrap_or(0));
        self.components.damage = (value != 0 || self.item.max_damage().is_none()).then_some(value);
        self.note(format_args!("set damage {value}"));
    }

    /// Sets the Bad Omen amplifier an ominous bottle gives, leaving the
    /// patch empty where it matches the item's default.
    #[track_caller]
    pub fn set_ominous_bottle_amplifier(&mut self, amplifier: u32) {
        let default = (self.item == Item::OminousBottle).then_some(0);
        self.components.ominous_bottle_amplifier =
            Some(amplifier.min(4)).filter(|a| Some(*a) != default);
        self.note(format_args!("set ominous bottle amplifier {amplifier}"));
    }

    /// The Bad Omen amplifier drinking this gives, if it is ominous.
    pub fn ominous_bottle_amplifier(&self) -> Option<u32> {
        self.components
            .ominous_bottle_amplifier
            .or((self.item == Item::OminousBottle).then_some(0))
    }

    /// The stack's `weapon` component.
    /// The use cooldown in effect: the stack's own `use_cooldown`
    /// component, else the item's default unless the stack removes it.
    pub fn use_cooldown(&self) -> Option<StackCooldown> {
        const ID: &str = "minecraft:use_cooldown";
        if let Some(Tag::Compound(nbt)) = self.components.other.get(ID) {
            let seconds = nbt.get("seconds").and_then(Tag::as_float)?;
            if seconds <= 0.0 {
                return None;
            }
            return Some(StackCooldown {
                ticks: (seconds * 20.0) as u32,
                group: nbt.get_str("cooldown_group").map(str::to_owned),
            });
        }
        if self.components.is_removed(ID) {
            return None;
        }
        let cooldown = self.item.use_cooldown()?;
        Some(StackCooldown {
            ticks: cooldown.ticks(),
            group: cooldown.group.map(str::to_owned),
        })
    }

    /// The effects the stack's `death_protection` component applies when it
    /// cheats a death: its own, else the totem's default unless removed.
    pub fn death_protection(&self) -> Option<Vec<DeathEffect>> {
        const ID: &str = "minecraft:death_protection";
        if let Some(Tag::Compound(nbt)) = self.components.other.get(ID) {
            let effects = nbt
                .get("death_effects")
                .and_then(Tag::as_list)
                .map(|list| list.iter().filter_map(DeathEffect::from_nbt).collect())
                .unwrap_or_default();
            return Some(effects);
        }
        if self.components.is_removed(ID) || self.item != Item::TotemOfUndying {
            return None;
        }
        Some(vec![
            DeathEffect::ClearAllEffects,
            DeathEffect::apply(MobEffect::Regeneration, 900, 1),
            DeathEffect::apply(MobEffect::Absorption, 100, 1),
            DeathEffect::apply(MobEffect::FireResistance, 800, 0),
        ])
    }

    /// The swing other players see when this stack is used to attack: its
    /// own `attack_animation` component, else a spear's stab, else the
    /// default whack.
    pub fn attack_animation(&self) -> (SwingKind, i32) {
        const ID: &str = "minecraft:attack_animation";
        let default = (SwingKind::Whack, DEFAULT_SWING_TICKS);
        if let Some(Tag::Compound(nbt)) = self.components.other.get(ID) {
            let kind = match nbt.get_str("type") {
                Some("stab") => SwingKind::Stab,
                Some("none") => SwingKind::None,
                _ => default.0,
            };
            let duration = nbt
                .get("duration")
                .and_then(as_i64)
                .map_or(default.1, |d| d as i32);
            return (kind, duration);
        }
        if self.components.is_removed(ID) || self.item.piercing_weapon().is_none() {
            return default;
        }
        let speed = self
            .item
            .attribute_modifiers()
            .iter()
            .find(|m| m.attribute == Attribute::AttackSpeed)
            .map_or(4.0, |m| m.amount + 4.0);
        (SwingKind::Stab, (20.0 / speed as f32) as i32)
    }

    /// What using up one of this stack leaves behind: its own
    /// `use_remainder` component, else the item's default unless removed.
    pub fn use_remainder(&self) -> Option<ItemStack> {
        const ID: &str = "minecraft:use_remainder";
        if let Some(Tag::Compound(nbt)) = self.components.other.get(ID) {
            return ItemStack::mint_from_nbt(nbt, MintReason::Remainder);
        }
        if self.components.is_removed(ID) {
            return None;
        }
        self.item
            .use_remainder()
            .map(|item| ItemStack::mint(item, 1, MintReason::Remainder))
    }

    pub fn weapon(&self) -> Option<Weapon> {
        if let Some(weapon) = self.components.weapon {
            return Some(weapon);
        }
        if self.components.is_removed(WEAPON) {
            return None;
        }
        self.item.weapon().copied()
    }

    /// The stack's `tool` component.
    pub fn tool(&self) -> Option<ToolRef<'_>> {
        if let Some(tool) = &self.components.tool {
            return Some(ToolRef::Own(tool));
        }
        if self.components.is_removed(TOOL) {
            return None;
        }
        self.item.tool().map(ToolRef::Default)
    }

    pub fn is_correct_tool_for_drops(&self, block: tuff_vanilla_data::Block) -> bool {
        self.tool()
            .is_some_and(|tool| tool.is_correct_for_drops(block))
    }

    /// False for weapons (swords, tridents, maces) whose `tool` component
    /// forbids breaking blocks in creative mode.
    pub fn can_destroy_blocks_in_creative(&self) -> bool {
        self.tool()
            .is_none_or(|tool| tool.can_destroy_blocks_in_creative())
    }

    /// The attribute modifiers the stack applies when worn or held.
    pub fn attribute_modifiers(&self) -> Vec<ModifierRef<'_>> {
        if let Some(modifiers) = &self.components.attribute_modifiers {
            return modifiers
                .iter()
                .map(|m| ModifierRef {
                    attribute: m.attribute,
                    id: &m.id,
                    amount: m.amount,
                    operation: m.operation,
                    slot: m.slot,
                })
                .collect();
        }
        if self.components.is_removed(ATTRIBUTE_MODIFIERS) {
            return Vec::new();
        }
        self.item
            .attribute_modifiers()
            .iter()
            .map(|m| ModifierRef {
                attribute: m.attribute,
                id: m.id,
                amount: m.amount,
                operation: m.operation,
                slot: m.slot,
            })
            .collect()
    }

    /// The stack's `minecraft:use_effects`: the item's default with any
    /// per-stack override applied.
    pub fn use_effects(&self) -> tuff_vanilla_data::item::UseEffects {
        let mut effects = self.item.use_effects();
        if let Some(Tag::Compound(over)) = self.components.other.get("minecraft:use_effects") {
            if let Some(value) = over.get_byte("can_sprint") {
                effects.can_sprint = value != 0;
            }
            if let Some(value) = over.get_byte("interact_vibrations") {
                effects.interact_vibrations = value != 0;
            }
            if let Some(value) = over.get_float("speed_multiplier") {
                effects.speed_multiplier = value;
            }
        }
        effects
    }

    pub fn enchantment_level(&self, id: &str) -> i32 {
        self.components
            .enchantments
            .as_ref()
            .map_or(0, |enchantments| enchantments.level(id))
    }

    /// Whether any enchantment on the stack has an effect `has` accepts.
    pub fn has_enchantment_effect(
        &self,
        has: impl Fn(&crate::enchantment::EnchantmentEffects) -> bool,
    ) -> bool {
        let registry = crate::enchantment::current();
        crate::enchantment::leveled(&registry, self.components.enchantments.as_ref())
            .any(|(_, def, _)| has(&def.effects))
    }

    /// Whether this stack is eligible for Mending to spend orb experience
    /// on it: an enchantment repairs it with experience, it is damageable,
    /// and currently damaged.
    pub fn wants_mending(&self) -> bool {
        let registry = crate::enchantment::current();
        crate::enchantment::leveled(&registry, self.components.enchantments.as_ref())
            .any(|(_, def, _)| !def.effects.repair_with_xp.is_empty())
            && self.is_damageable()
            && self.damage() > 0
    }

    /// Runs a value effect component of the stack's enchantments over
    /// `value`, skipping entries whose requirements need more than the
    /// item itself.
    pub fn enchantment_value(
        &self,
        component: impl Fn(
            &crate::enchantment::EnchantmentEffects,
        )
            -> &[crate::enchantment::Conditional<crate::enchantment::ValueEffect>],
        value: f32,
    ) -> f32 {
        let registry = crate::enchantment::current();
        let mut rng = rand::thread_rng();
        let mut value = value;
        for (_, def, level) in
            crate::enchantment::leveled(&registry, self.components.enchantments.as_ref())
        {
            for entry in component(&def.effects) {
                if crate::enchantment::item_requirement(
                    entry.requirements.as_ref(),
                    self,
                    level,
                    &mut rng,
                ) {
                    value = entry.effect.process(level, &mut rng, value);
                }
            }
        }
        value
    }

    /// Wear after the `item_damage` effects (Unbreaking may skip points).
    pub(crate) fn after_unbreaking(&self, amount: u32, rng: &mut impl Rng) -> u32 {
        let registry = crate::enchantment::current();
        let mut value = amount as f32;
        for (_, def, level) in
            crate::enchantment::leveled(&registry, self.components.enchantments.as_ref())
        {
            for entry in &def.effects.item_damage {
                if crate::enchantment::item_requirement(
                    entry.requirements.as_ref(),
                    self,
                    level,
                    &mut *rng,
                ) {
                    value = entry.effect.process(level, rng, value);
                }
            }
        }
        value.max(0.0) as u32
    }

    /// The vanilla `ItemStack` codec's form: `{id, count, components}`.
    pub fn to_nbt(&self) -> Compound {
        let mut nbt = Compound::new()
            .with("id", self.item.name())
            .with("count", self.count as i32);
        let components = self.components.to_nbt();
        if !components.is_empty() {
            nbt.insert("components", components);
        }
        nbt
    }

    /// The stack in its serialized shape, as the client hashes a stack
    /// nested in a component. Fields at their default are left out.
    pub fn canon(&self) -> Canon {
        let mut fields = vec![("id".to_owned(), Canon::str(self.item.name()))];
        if self.count != 1 {
            fields.push(("count".into(), Canon::Int(self.count as i32)));
        }
        let components: Vec<(String, Canon)> = self
            .components
            .network_added()
            .iter()
            .map(|component| (component.kind().name().to_owned(), component.canon()))
            .chain(
                self.components
                    .removed
                    .iter()
                    .map(|id| (format!("{REMOVED_PREFIX}{id}"), Canon::Map(Vec::new()))),
            )
            .collect();
        if !components.is_empty() {
            fields.push(("components".into(), Canon::Map(components)));
        }
        Canon::Map(fields)
    }
}

impl ItemStack {
    /// Applies `amount` points of durability loss the way the game does for
    /// a survival player: Unbreaking may skip points, and reaching the
    /// maximum consumes one item. Creative and unbreakable stacks are the
    /// caller's job to skip.
    #[track_caller]
    pub fn hurt(&mut self, amount: u32, rng: &mut impl Rng) -> Hurt {
        if amount == 0 || !self.is_damageable() {
            return Hurt::Unchanged;
        }
        let amount = self.after_unbreaking(amount, rng);
        if amount == 0 {
            return Hurt::Unchanged;
        }
        let max = self.max_damage().unwrap_or(0);
        let damage = self.damage().saturating_add(amount).min(max);
        self.set_damage(damage);
        if damage >= max {
            self.shrink(1, BurnReason::Broken);
            Hurt::Broken
        } else {
            Hurt::Damaged
        }
    }

    /// Repairs durability with orb experience the way Mending does: the
    /// `repair_with_xp` effects turn experience into durability, capped by
    /// the current damage (so a nearly-full repair does not cost extra
    /// experience). Returns the experience actually spent.
    #[track_caller]
    pub fn repair_with_experience(&mut self, xp: i32) -> i32 {
        if xp <= 0 {
            return 0;
        }
        let damage = self.damage();
        if damage == 0 {
            return 0;
        }
        let registry = crate::enchantment::current();
        let mut rng = rand::thread_rng();
        let mut durability = xp as f32;
        for (_, def, level) in
            crate::enchantment::leveled(&registry, self.components.enchantments.as_ref())
        {
            for entry in &def.effects.repair_with_xp {
                if crate::enchantment::item_requirement(
                    entry.requirements.as_ref(),
                    self,
                    level,
                    &mut rng,
                ) {
                    durability = entry.effect.process(level, &mut rng, durability);
                }
            }
        }
        let durability = durability.max(0.0) as u32;
        if durability == 0 {
            return 0;
        }
        let repaired = durability.min(damage);
        self.set_damage(damage - repaired);
        (u64::from(repaired) * xp as u64 / u64::from(durability)) as i32
    }
}

/// Reads the stored `container` component: `[{slot, item}]`, sparse.
fn container_from_nbt(tag: &Tag) -> Option<Vec<Option<ItemPicture>>> {
    let entries = tag.as_list()?;
    if entries.len() > MAX_CONTAINER_SLOTS {
        return None;
    }
    let mut contents: Vec<Option<ItemPicture>> = Vec::new();
    for entry in entries {
        let entry = entry.as_compound()?;
        let slot = entry.get_int("slot")?;
        if !(0..MAX_CONTAINER_SLOTS as i32).contains(&slot) {
            return None;
        }
        let stack = ItemPicture::from_nbt(entry.get_compound("item")?)?;
        let slot = slot as usize;
        if contents.len() <= slot {
            contents.resize_with(slot + 1, || None);
        }
        contents[slot] = Some(stack);
    }
    Some(contents)
}

fn container_to_nbt<M: Mode>(contents: &[Option<ItemStack<M>>]) -> Tag {
    Tag::List(
        contents
            .iter()
            .enumerate()
            .filter_map(|(slot, stack)| {
                let stack = stack.as_ref()?;
                Some(Tag::Compound(
                    Compound::new()
                        .with("slot", slot as i32)
                        .with("item", stack.to_nbt()),
                ))
            })
            .collect(),
    )
}

/// Reads the stored `charged_projectiles` component: a list of stacks.
fn charged_from_nbt(tag: &Tag) -> Option<Vec<ItemPicture>> {
    let entries = tag.as_list()?;
    if entries.len() > MAX_CHARGED_PROJECTILES {
        return None;
    }
    entries
        .iter()
        .map(|entry| ItemPicture::from_nbt(entry.as_compound()?))
        .collect()
}

fn as_i64(tag: &Tag) -> Option<i64> {
    match *tag {
        Tag::Byte(v) => Some(v.into()),
        Tag::Short(v) => Some(v.into()),
        Tag::Int(v) => Some(v.into()),
        Tag::Long(v) => Some(v),
        Tag::Float(v) => Some(v as i64),
        Tag::Double(v) => Some(v as i64),
        _ => None,
    }
}

fn as_u32(tag: &Tag) -> Option<u32> {
    u32::try_from(as_i64(tag)?).ok()
}

fn as_bool(tag: &Tag) -> Option<bool> {
    Some(as_i64(tag)? != 0)
}

fn known_component(id: &str) -> Option<DataComponentType> {
    DataComponentType::from_name(id)
}

/// A stored display component in its wire form; `None` for another kind
/// or a value the client could not read.
fn display_component<M: Mode>(
    kind: DataComponentType,
    value: &Tag,
) -> Option<NetworkComponent<'_, M>> {
    Some(match kind {
        DataComponentType::MaxStackSize => NetworkComponent::MaxStackSize(
            value
                .as_int()
                .filter(|size| (1..=MAX_COUNT).contains(size))? as u32,
        ),
        DataComponentType::ItemModel | DataComponentType::TooltipStyle => {
            NetworkComponent::Identifier(kind, display::full_identifier(value.as_str()?)?)
        }
        DataComponentType::Rarity => NetworkComponent::Rarity(display::rarity_id(value)?),
        DataComponentType::EnchantmentGlintOverride => {
            NetworkComponent::GlintOverride(value.as_bool()?)
        }
        DataComponentType::CustomModelData => {
            NetworkComponent::CustomModelData(display::ModelData::from_nbt(value)?)
        }
        DataComponentType::TooltipDisplay => {
            NetworkComponent::TooltipDisplay(display::TooltipDisplay::from_nbt(value)?)
        }
        DataComponentType::LodestoneTracker => {
            NetworkComponent::LodestoneTracker(LodestoneTracker::from_nbt(value)?)
        }
        _ => return None,
    })
}

mod behavior;
mod components;
mod effects;
pub mod lodestone;
pub(crate) use effects::dropped_points;
pub use lodestone::LodestoneTracker;
#[cfg(test)]
mod tests;

/// A condition over the tool `item` alone: `match_tool` on its id or tag,
/// and the logical combinators; anything else never matches.
pub fn tool_requirement(condition: &serde_json::Value, item: Item) -> bool {
    let terms = || condition["terms"].as_array().map_or(&[][..], Vec::as_slice);
    match condition["type"].as_str().unwrap_or_default() {
        "minecraft:match_tool" => {}
        "minecraft:inverted" => return !tool_requirement(&condition["term"], item),
        "minecraft:all_of" => return terms().iter().all(|term| tool_requirement(term, item)),
        "minecraft:any_of" => return terms().iter().any(|term| tool_requirement(term, item)),
        _ => return false,
    }
    let predicate = &condition["predicate"];
    let Some(fields) = predicate.as_object() else {
        return false;
    };
    fields.iter().all(|(key, value)| match key.as_str() {
        "items" => tool_item_matches(value, item),
        _ => false,
    })
}

fn tool_item_matches(value: &serde_json::Value, item: Item) -> bool {
    match value {
        serde_json::Value::String(entry) => match entry.strip_prefix('#') {
            Some(tag) => {
                tuff_datapack::tags::with(|tags| tags.contains("minecraft:item", tag, item.name()))
            }
            None => entry == item.name(),
        },
        serde_json::Value::Array(entries) => {
            entries.iter().any(|entry| tool_item_matches(entry, item))
        }
        _ => false,
    }
}
