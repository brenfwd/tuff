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

impl ItemStack {
    pub fn view(&self) -> ItemView<'_> {
        ItemView(self)
    }

    fn snapshot_copy(&self) -> ItemStack {
        self.dup_unchecked()
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
            ItemView::slots(&[None, Some(stack.dup_unchecked())])
                .flatten()
                .count(),
            1
        );
    }
}
