//! Read-only access to item stacks. Code that only looks at items (packets,
//! client display, the panel, plugins, the anticheat, saving) takes an
//! [`ItemView`]; data handed to other actors holds an [`ItemSnapshot`].
//! Neither can be turned back into an owned stack.

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use super::ItemStack;

/// A borrowed, read-only look at a stack.
#[derive(Clone, Copy, PartialEq)]
pub struct ItemView<'a>(&'a ItemStack);

impl<'a> ItemView<'a> {
    pub fn get(self) -> &'a ItemStack {
        self.0
    }

    /// Several optional slots as views.
    pub fn slots(
        slots: &'a [Option<ItemStack>],
    ) -> impl Iterator<Item = Option<ItemView<'a>>> + 'a {
        slots.iter().map(|slot| slot.as_ref().map(ItemView::from))
    }
}

impl<'a> From<&'a ItemStack> for ItemView<'a> {
    fn from(stack: &'a ItemStack) -> Self {
        Self(stack)
    }
}

impl<'a, 'b: 'a> From<&'a ItemView<'b>> for ItemView<'a> {
    fn from(view: &'a ItemView<'b>) -> Self {
        *view
    }
}

impl<'a> From<&'a ItemSnapshot> for ItemView<'a> {
    fn from(snapshot: &'a ItemSnapshot) -> Self {
        Self(&snapshot.0)
    }
}

impl Deref for ItemView<'_> {
    type Target = ItemStack;

    fn deref(&self) -> &ItemStack {
        self.0
    }
}

impl fmt::Debug for ItemView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq<ItemStack> for ItemView<'_> {
    fn eq(&self, other: &ItemStack) -> bool {
        self.0 == other
    }
}

/// An owned, shareable, read-only picture of a stack at one moment, for
/// data that outlives the borrow or crosses to another actor. Cloning it
/// shares the picture; it never yields a stack.
#[derive(Clone, PartialEq)]
pub struct ItemSnapshot(Arc<ItemStack>);

impl ItemSnapshot {
    pub fn of<'a>(view: impl Into<ItemView<'a>>) -> Self {
        Self(Arc::new(view.into().0.snapshot_copy()))
    }

    pub fn view(&self) -> ItemView<'_> {
        ItemView(&self.0)
    }

    /// Changes the pictured components (a template being finished).
    pub fn edit_components(&mut self, edit: impl FnOnce(&mut super::Components)) {
        if Arc::get_mut(&mut self.0).is_none() {
            self.0 = Arc::new(self.0.snapshot_copy());
        }
        if let Some(stack) = Arc::get_mut(&mut self.0) {
            edit(&mut stack.components);
        }
    }

    /// A picture of `count` of `item` with `components`, never a real item.
    pub fn describe(
        item: tuff_vanilla_data::Item,
        count: u8,
        components: super::Components,
    ) -> Self {
        Self(Arc::new(ItemStack {
            item,
            count,
            components,
            tally: super::ledger::Tally::Picture,
        }))
    }

    /// A picture of `count` of `item` with a copy of `components`.
    pub fn describe_with(
        item: tuff_vanilla_data::Item,
        count: u8,
        components: &super::Components,
    ) -> Self {
        Self::describe(item, count, components.picture_copy())
    }

    /// The same picture showing `count` items.
    pub fn with_count(self, count: u8) -> Self {
        let mut stack = Arc::try_unwrap(self.0).unwrap_or_else(|shared| shared.snapshot_copy());
        stack.count = count;
        Self(Arc::new(stack))
    }
}

impl Deref for ItemSnapshot {
    type Target = ItemStack;

    fn deref(&self) -> &ItemStack {
        &self.0
    }
}

impl fmt::Debug for ItemSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq<ItemStack> for ItemSnapshot {
    fn eq(&self, other: &ItemStack) -> bool {
        *self.0 == *other
    }
}

/// A shareable, read-only copy of a component patch for templates (a
/// command's item argument, a custom item, a placed block's contents).
/// Stamping it onto real items goes through [`ItemStack::mint_with`].
#[derive(Clone, Default, PartialEq)]
pub struct ComponentsSnapshot(Arc<super::Components>);

impl ComponentsSnapshot {
    pub fn of(components: &super::Components) -> Self {
        Self(Arc::new(components.picture_copy()))
    }

    /// Takes components that only describe items (parsed from a command,
    /// built for a template).
    pub fn describing(components: super::Components) -> Self {
        Self(Arc::new(components))
    }

    /// A private copy of the pictured components, still only a picture.
    pub fn edit(&mut self, edit: impl FnOnce(&mut super::Components)) {
        if Arc::get_mut(&mut self.0).is_none() {
            self.0 = Arc::new(self.0.picture_copy());
        }
        if let Some(components) = Arc::get_mut(&mut self.0) {
            edit(components);
        }
    }
}

impl Deref for ComponentsSnapshot {
    type Target = super::Components;

    fn deref(&self) -> &super::Components {
        &self.0
    }
}

/// Edits the picture, copying it first when it is shared; it stays a
/// picture.
impl std::ops::DerefMut for ComponentsSnapshot {
    fn deref_mut(&mut self) -> &mut super::Components {
        if Arc::get_mut(&mut self.0).is_none() {
            self.0 = Arc::new(self.0.picture_copy());
        }
        Arc::get_mut(&mut self.0).expect("unshared after the copy")
    }
}

impl From<super::Components> for ComponentsSnapshot {
    fn from(components: super::Components) -> Self {
        Self::describing(components)
    }
}

impl fmt::Debug for ComponentsSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Pictures of every slot.
pub fn snapshot_slots(slots: &[Option<ItemStack>]) -> Vec<Option<ItemSnapshot>> {
    slots
        .iter()
        .map(|slot| slot.as_ref().map(ItemSnapshot::of))
        .collect()
}

impl ItemStack {
    pub fn view(&self) -> ItemView<'_> {
        ItemView(self)
    }

    /// Turns a stack that only describes an item (a recipe result, a
    /// trade offer, a decoded template) into a picture of it.
    pub fn into_template(self) -> ItemSnapshot {
        let shown = ItemSnapshot::of(&self);
        self.burn(super::BurnReason::Template);
        shown
    }

    fn snapshot_copy(&self) -> ItemStack {
        self.picture_copy()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::MintReason;
    use tuff_vanilla_data::Item;

    #[tuff_core::watchdog]
    #[test]
    fn a_snapshot_keeps_the_stack_as_it_was() {
        let mut stack = ItemStack::mint(Item::Stone, 5, MintReason::Test);
        let snapshot = ItemSnapshot::of(&stack);
        stack.count = 1;
        assert_eq!(snapshot.count, 5);
        assert_eq!(snapshot.view().item, Item::Stone);
        assert_eq!(
            snapshot.clone(),
            ItemStack::mint(Item::Stone, 5, MintReason::Test)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_view_reads_the_stack() {
        let stack = ItemStack::mint(Item::Dirt, 3, MintReason::Test);
        let view = stack.view();
        assert_eq!(view.count, 3);
        assert_eq!(view, stack);
        assert_eq!(
            ItemView::slots(&[None, Some(stack.test_copy())])
                .flatten()
                .count(),
            1
        );
    }
}
