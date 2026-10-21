//! Custom items at runtime: stack builders, identity lookups, the behaviour
//! a definition dispatches to, and feature `custom_data` kept off the wire.
//!
//! A custom item is a stack of its base item, so everything that reads
//! components (stacking, durability, tools, the wire) works unchanged.
//! Behaviour dispatch goes custom first: the definition's
//! [`CustomItemBehavior`] answers a click, and unless the definition
//! inherits its base's behaviour, the click then acts as if the hand held
//! nothing the base could react to.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tuff_vanilla_data::registry::DataComponentType;
use tuff_vanilla_data::{BlockState, Item};

use super::{CUSTOM_DATA, CustomContent, CustomItemDef};
use crate::protocol::play::{self, GameMode};
use crate::world::entity::{EntityInteraction, InteractOutcome};
use crate::world::messages::{ActionOutcome, Place};
use crate::world::region_api::RegionWorld;
use crate::world::session::PlayerSession;
use tuff_item::item::ComponentsSnapshot;
use tuff_item::item::display::{ModelData, full_identifier};
use tuff_item::item::identity::{IDENTITY_KEY, ITEM_KEY, PRIVATE_KEY, client_custom_data};
use tuff_item::item::{Components, ItemSnapshot, ItemStack};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};

/// Set in a private item's identity compound; the wire keeps only the id.
const STASH_LIMIT: usize = 64;

/// Whether a behaviour took a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handled {
    /// Nothing happened; the base item may still act if it inherits.
    Pass,
    /// The click is answered; nothing else runs.
    Consumed,
}

pub type UseFuture<'a> = Pin<Box<dyn Future<Output = Handled> + Send + 'a>>;

/// What using a custom item does. Every method defaults to passing.
pub trait CustomItemBehavior: Send + Sync {
    /// Use Item with the item in `off_hand`: a right click in the air, or
    /// on a block that did nothing. Runs on the player's session.
    fn use_item<'a>(&'a self, _session: &'a mut PlayerSession, _off_hand: bool) -> UseFuture<'a> {
        Box::pin(async { Handled::Pass })
    }

    /// The item used on a block whose own reaction did nothing. Runs on the
    /// block's region; `Some` answers the click.
    fn use_on_block(
        &self,
        _world: &mut dyn RegionWorld,
        _place: &Place,
        _state: BlockState,
    ) -> Option<ActionOutcome> {
        None
    }

    /// The item used on an entity that ignored the click. Runs on the
    /// entity's region; `interaction` holds the real stack.
    fn use_on_entity(
        &self,
        _world: &mut dyn RegionWorld,
        _target: i32,
        _interaction: &EntityInteraction,
    ) -> InteractOutcome {
        None
    }
}

/// A definition's behaviour; several definitions may share one.
#[derive(Clone)]
pub struct ItemBehavior(pub Arc<dyn CustomItemBehavior>);

impl std::fmt::Debug for ItemBehavior {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ItemBehavior")
    }
}

fn identity(components: &Components) -> Option<&Compound> {
    components
        .other
        .get(CUSTOM_DATA)?
        .as_compound()?
        .get(IDENTITY_KEY)?
        .as_compound()
}

/// The default components of `base` whose use the client predicts on its
/// own (eating, blocking, spear charges, tilling, use cooldowns). A custom
/// item that does not inherit its base's behaviour removes them, so the
/// client never starts a use the server refuses.
pub fn predicted_use_components(base: Item) -> impl Iterator<Item = DataComponentType> {
    [
        (DataComponentType::Consumable, base.consumable().is_some()),
        (DataComponentType::Food, base.food().is_some()),
        (
            DataComponentType::UseCooldown,
            base.use_cooldown().is_some(),
        ),
        (
            DataComponentType::BlocksAttacks,
            base.blocks_attacks().is_some(),
        ),
        (DataComponentType::KineticWeapon, base.kinetic_weapon()),
        (
            DataComponentType::BlockTransformer,
            base.block_transformer().is_some(),
        ),
    ]
    .into_iter()
    .filter_map(|(kind, present)| present.then_some(kind))
}

/// The custom item id a component patch carries, if any.
pub fn components_item_id(components: &Components) -> Option<&str> {
    identity(components)?.get_str(ITEM_KEY)
}

impl CustomItemDef {
    /// Runs the base item's own behaviour after this item's behaviour
    /// passes (and lets the base serve as a vanilla recipe ingredient).
    pub fn inheriting(mut self) -> Self {
        self.inherit_behavior = true;
        self
    }

    /// Keeps `custom_data` other than the identity off the wire. Creative
    /// clients send stacks back without it; the session restores it.
    pub fn private(mut self) -> Self {
        self.private_data = true;
        self
    }

    pub fn with_behavior(mut self, behavior: Arc<dyn CustomItemBehavior>) -> Self {
        self.behavior = Some(ItemBehavior(behavior));
        self
    }

    fn with_other(mut self, kind: DataComponentType, value: impl Into<Tag>) -> Self {
        let value = value.into();
        self.components.edit(|components| {
            components.other.insert(kind.name(), value);
        });
        self
    }

    /// The `item_name` shown when the stack has no custom name.
    pub fn with_name(self, name: TextComponent) -> Self {
        self.with_other(DataComponentType::ItemName, name.to_nbt())
    }

    pub fn with_lore(self, lines: impl IntoIterator<Item = TextComponent>) -> Self {
        let lines = lines.into_iter().map(|line| line.to_nbt()).collect();
        self.with_other(DataComponentType::Lore, Tag::List(lines))
    }

    pub fn with_max_stack_size(self, size: u8) -> Self {
        self.with_other(DataComponentType::MaxStackSize, size as i32)
    }

    pub fn with_glint(self, glint: bool) -> Self {
        self.with_other(DataComponentType::EnchantmentGlintOverride, glint)
    }

    pub fn with_tooltip_style(self, style: &str) -> Self {
        self.with_other(DataComponentType::TooltipStyle, style)
    }

    /// The default `custom_model_data` of its stacks.
    pub fn with_model_data(self, data: &ModelData) -> Self {
        self.with_other(DataComponentType::CustomModelData, data.to_nbt())
    }

    /// Feature data every new stack starts with, beside the identity.
    pub fn with_data(mut self, data: Compound) -> Self {
        self.components.edit(|components| {
            let mut merged = match components.other.remove(CUSTOM_DATA) {
                Some(Tag::Compound(existing)) => existing,
                _ => Compound::new(),
            };
            for (key, value) in data.iter() {
                merged.insert(key, value.clone());
            }
            components.other.insert(CUSTOM_DATA, merged);
        });
        self
    }

    /// A stack showing `data` instead of the default model data.
    pub fn variant(&self, count: u8, data: &ModelData) -> ItemStack {
        let mut stack = self.stack(count);
        let key = DataComponentType::CustomModelData.name();
        if data.is_empty() {
            stack.components.remove_other(key);
        } else {
            stack.components.other.insert(key, data.to_nbt());
        }
        stack
    }

    /// Why the definition cannot build a valid stack, if it cannot.
    pub(super) fn problem(&self) -> Option<String> {
        let stack = self.stack(1);
        let size = stack
            .components
            .other
            .get(DataComponentType::MaxStackSize.name());
        if size.is_some_and(|size| size.as_int().is_none_or(|s| !(1..=99).contains(&s))) {
            return Some("max_stack_size must be 1 to 99".to_owned());
        }
        if stack.max_stack_size() > 1 && stack.max_damage().is_some() {
            return Some("a damageable item cannot stack".to_owned());
        }
        if let Some(style) = stack
            .components
            .other
            .get(DataComponentType::TooltipStyle.name())
            && style.as_str().and_then(full_identifier).is_none()
        {
            return Some("tooltip_style is not an identifier".to_owned());
        }
        if let Some(data) = stack
            .components
            .other
            .get(DataComponentType::CustomModelData.name())
            && ModelData::from_nbt(data).is_none()
        {
            return Some("custom_model_data is malformed".to_owned());
        }
        None
    }
}

impl CustomContent {
    /// A new stack of the custom item `id`.
    pub fn stack(&self, id: &str, count: u8) -> Option<ItemStack> {
        Some(self.item(id)?.stack(count))
    }

    pub fn item_of_components(&self, components: &Components) -> Option<&Arc<CustomItemDef>> {
        self.items.get(components_item_id(components)?)
    }

    /// Every custom item, in no particular order.
    pub fn items(&self) -> impl Iterator<Item = &Arc<CustomItemDef>> {
        self.items.values()
    }

    /// Whether `stack` is a custom item whose base item's own behaviour
    /// (and use as a vanilla recipe ingredient) is switched off.
    pub fn suppresses_base(&self, stack: &ItemStack) -> bool {
        !self.items.is_empty() && self.item_of(stack).is_some_and(|def| !def.inherit_behavior)
    }
}

/// Before a block click runs: the custom item the click holds, with the
/// base item hidden from the click unless the definition inherits it.
pub fn prepare_place(content: &CustomContent, place: &mut Place) -> Option<Arc<CustomItemDef>> {
    if content.items.is_empty() {
        return None;
    }
    let def = content.item_of_components(&place.components)?.clone();
    if !def.inherit_behavior {
        place.item = None;
        place.held = None;
    }
    Some(def)
}

/// What an entity sees of a click: the stack, unless it is a custom item
/// that does not inherit its base's behaviour.
pub fn seen_by_entity(
    content: &CustomContent,
    interaction: &EntityInteraction,
) -> (Option<Arc<CustomItemDef>>, Option<EntityInteraction>) {
    if content.items.is_empty() {
        return (None, None);
    }
    let Some(def) = interaction
        .stack
        .as_ref()
        .and_then(|stack| content.item_of(stack))
        .cloned()
    else {
        return (None, None);
    };
    let hidden = (!def.inherit_behavior).then(|| EntityInteraction {
        stack: None,
        ..interaction.clone()
    });
    (Some(def), hidden)
}

/// Recently replaced creative stacks with private data, newest last.
#[derive(Default)]
struct PrivateStash(Vec<ItemSnapshot>);

fn client_view(stack: &ItemStack) -> ComponentsSnapshot {
    let mut components = ComponentsSnapshot::of(&stack.components);
    if let Some(view) = stack
        .components
        .other
        .get(CUSTOM_DATA)
        .and_then(client_custom_data)
    {
        components.edit(|components| {
            components.other.insert(CUSTOM_DATA, view);
        });
    }
    components
}

fn is_private(stack: &ItemStack) -> bool {
    identity(&stack.components)
        .and_then(|identity| identity.get_bool(PRIVATE_KEY))
        .unwrap_or(false)
}

impl PlayerSession {
    /// Use Item with a custom item in hand: its behaviour runs, then the
    /// base item's unless suppressed. True when the click is answered.
    pub async fn custom_use_item(&mut self, off_hand: bool, sequence: i32) -> bool {
        let content = self.shared.custom.clone();
        if content.items.is_empty() || self.is_dead() || self.mode.game_mode == GameMode::Spectator
        {
            return false;
        }
        let Some(def) = self
            .inventory
            .hand(off_hand)
            .and_then(|stack| content.item_of(stack))
            .cloned()
        else {
            return false;
        };
        let handled = match &def.behavior {
            Some(behavior) => behavior.0.use_item(self, off_hand).await,
            None => Handled::Pass,
        };
        if handled == Handled::Pass && def.inherit_behavior {
            return false;
        }
        self.send(&play::BlockChangedAck(sequence));
        self.send_inventory();
        true
    }

    /// A stack a creative client set into `slot` (`None` for a drop), with
    /// the private custom data it was never sent put back: from the stack it
    /// replaces, else from one it replaced recently, else the definition's.
    pub fn restore_private_data(
        &mut self,
        slot: Option<usize>,
        stack: Option<ItemStack>,
    ) -> Option<ItemStack> {
        let content = self.shared.custom.clone();
        if content.items.is_empty() {
            return stack;
        }
        if let Some(previous) = slot
            .and_then(|slot| self.inventory.slots().get(slot).and_then(Option::as_ref))
            .filter(|stack| is_private(stack))
            .map(ItemSnapshot::of)
        {
            let stash = &mut self.ext.entry::<PrivateStash>().0;
            stash.retain(|stashed| *stashed != previous);
            stash.push(previous);
            if stash.len() > STASH_LIMIT {
                stash.remove(0);
            }
        }
        let mut stack = stack?;
        let Some(def) = content.item_of(&stack).filter(|def| def.private_data) else {
            return Some(stack);
        };
        if is_private(&stack) {
            return Some(stack);
        }
        let stash = self.ext.get::<PrivateStash>();
        let source = stash
            .and_then(|stash| {
                stash.0.iter().rev().find(|stashed| {
                    stashed.item == stack.item && *client_view(stashed) == stack.components
                })
            })
            .cloned()
            .unwrap_or_else(|| def.template());
        if let Some(data) = source.components.other.get(CUSTOM_DATA) {
            stack.components.other.insert(CUSTOM_DATA, data.clone());
        }
        Some(stack)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom::Definitions;
    use crate::protocol::codec::HashedSlot;
    use tuff_item::item::MintReason;

    fn content(defs: impl IntoIterator<Item = CustomItemDef>) -> Arc<CustomContent> {
        let mut definitions = Definitions::default();
        for def in defs {
            definitions.register_item("ex", def).unwrap();
        }
        definitions.resolve().unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn builders_set_display_components() {
        let data = ModelData {
            strings: vec!["charged".into()],
            ..ModelData::default()
        };
        let def = CustomItemDef::new("ex:wand", Item::Stick)
            .with_model("ex:wand")
            .with_name(TextComponent::new("Wand"))
            .with_max_stack_size(1)
            .with_glint(true)
            .with_model_data(&data)
            .with_data(Compound::new().with("charge", 3));
        let stack = def.stack(1);
        let kinds: Vec<_> = stack
            .components
            .network_added()
            .iter()
            .map(|c| c.kind())
            .collect();
        for kind in [
            DataComponentType::ItemModel,
            DataComponentType::ItemName,
            DataComponentType::MaxStackSize,
            DataComponentType::EnchantmentGlintOverride,
            DataComponentType::CustomModelData,
            DataComponentType::CustomData,
        ] {
            assert!(kinds.contains(&kind), "{kind:?} missing from {kinds:?}");
        }
        assert_eq!(stack.max_stack_size(), 1);
        let custom = stack.components.other.get_compound(CUSTOM_DATA).unwrap();
        assert_eq!(custom.get_int("charge"), Some(3));
        assert_eq!(components_item_id(&stack.components), Some("ex:wand"));

        let off = def.variant(1, &ModelData::default());
        assert!(
            !off.components
                .other
                .contains_key(DataComponentType::CustomModelData.name())
        );
        assert!(!off.stacks_with(&stack));
    }

    #[tuff_core::watchdog]
    #[test]
    fn invalid_definitions_are_refused() {
        let mut defs = Definitions::default();
        let stacked_sword = CustomItemDef::new("ex:blade", Item::IronSword).with_max_stack_size(16);
        assert!(defs.register_item("ex", stacked_sword).is_err());
        let huge = CustomItemDef::new("ex:pile", Item::Paper).with_max_stack_size(100);
        assert!(defs.register_item("ex", huge).is_err());
        let bad_style = CustomItemDef::new("ex:x", Item::Paper).with_tooltip_style("Bad Style");
        assert!(defs.register_item("ex", bad_style).is_err());
        let single_sword = CustomItemDef::new("ex:blade", Item::IronSword).with_max_stack_size(1);
        assert!(defs.register_item("ex", single_sword).is_ok());
    }

    #[tuff_core::watchdog]
    #[test]
    fn private_data_stays_off_the_wire() {
        let def = CustomItemDef::new("ex:key", Item::TripwireHook)
            .private()
            .with_data(Compound::new().with("door", "vault"));
        let stack = def.stack(1);
        let custom = stack.components.other.get_compound(CUSTOM_DATA).unwrap();
        assert_eq!(custom.get_str("door"), Some("vault"));
        let added = stack.components.network_added();
        let sent = added
            .iter()
            .find(|c| c.kind() == DataComponentType::CustomData)
            .unwrap();
        let expected = Compound::new().with(IDENTITY_KEY, Compound::new().with(ITEM_KEY, "ex:key"));
        assert_eq!(
            *sent,
            tuff_item::item::NetworkComponent::ClientData(expected.clone())
        );
        let hashed = HashedSlot {
            item: stack.item.id(),
            count: 1,
            added: added.iter().map(|c| (c.kind().id(), c.hash())).collect(),
            removed: Vec::new(),
        };
        assert!(HashedSlot::matches(Some(&hashed), Some(stack.view())));

        let public = CustomItemDef::new("ex:open", Item::Paper)
            .with_data(Compound::new().with("door", "vault"))
            .stack(1);
        assert!(public.components.network_added().iter().any(|c| matches!(
            c,
            tuff_item::item::NetworkComponent::Nbt(DataComponentType::CustomData, _)
        )));
        assert_eq!(
            client_view(&stack).other.get_compound(CUSTOM_DATA),
            Some(&expected)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn non_inheriting_items_strip_client_predicted_use() {
        use DataComponentType as Kind;
        let removed = |def: CustomItemDef| def.stack(1).components.network_removed();
        let charm = CustomItemDef::new("ex:charm", Item::Apple);
        assert_eq!(removed(charm.clone()), [Kind::Consumable, Kind::Food]);
        assert!(removed(charm.clone().inheriting()).is_empty());
        assert_eq!(
            removed(CustomItemDef::new("ex:pearl", Item::ChorusFruit)),
            [Kind::Consumable, Kind::Food, Kind::UseCooldown]
        );
        assert!(
            removed(CustomItemDef::new("ex:guard", Item::Shield)).contains(&Kind::BlocksAttacks)
        );
        assert!(
            removed(CustomItemDef::new("ex:hatchet", Item::IronAxe))
                .contains(&Kind::BlockTransformer)
        );
        assert!(
            removed(CustomItemDef::new("ex:lance", Item::IronSpear)).contains(&Kind::KineticWeapon)
        );
        assert!(removed(CustomItemDef::new("ex:gem", Item::Emerald)).is_empty());

        let mut own = Components::default();
        own.other.insert(
            Kind::Consumable.name(),
            Compound::new().with("consume_seconds", 3.0f32),
        );
        assert_eq!(
            removed(CustomItemDef::new("ex:snack", Item::Apple).with_components(own)),
            [Kind::Food],
            "a consumable the feature sets is its own"
        );

        let stack = charm.stack(1);
        let hashed = |removed: Vec<u32>| HashedSlot {
            item: stack.item.id(),
            count: 1,
            added: stack
                .components
                .network_added()
                .iter()
                .map(|c| (c.kind().id(), c.hash()))
                .collect(),
            removed,
        };
        let ids = vec![Kind::Consumable.id(), Kind::Food.id()];
        assert!(HashedSlot::matches(Some(&hashed(ids)), Some(stack.view())));
        assert!(!HashedSlot::matches(
            Some(&hashed(Vec::new())),
            Some(stack.view())
        ));
        let saved = Components::from_nbt(&stack.components.to_nbt());
        assert_eq!(saved, stack.components, "the removal survives saves");
    }

    #[tuff_core::watchdog]
    #[test]
    fn suppression_follows_inheritance() {
        let content = content([
            CustomItemDef::new("ex:ruby", Item::Emerald),
            CustomItemDef::new("ex:gem", Item::Emerald).inheriting(),
        ]);
        assert!(content.suppresses_base(&content.stack("ex:ruby", 1).unwrap()));
        assert!(!content.suppresses_base(&content.stack("ex:gem", 1).unwrap()));
        assert!(!content.suppresses_base(&ItemStack::mint(Item::Emerald, 1, MintReason::Test)));
        assert!(content.stack("ex:missing", 1).is_none());
        assert_eq!(content.items().count(), 2);
    }
}
