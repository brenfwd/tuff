//! Read-only access to item stacks. Code that only looks at items (packets,
//! client display, the panel, plugins, the anticheat, saving) takes an
//! [`ItemView`] of either kind of stack; data handed to other actors holds
//! an [`ItemSnapshot`], a shared picture. Neither yields real items except
//! through the mint doors.

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use super::{Components, Counted, ItemPicture, ItemStack, Mode, Picture};

/// A borrowed, read-only look at a stack.
pub struct ItemView<'a, M: Mode = Counted>(&'a ItemStack<M>);

impl<M: Mode> Clone for ItemView<'_, M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M: Mode> Copy for ItemView<'_, M> {}

impl<'a, M: Mode> ItemView<'a, M> {
    pub fn get(self) -> &'a ItemStack<M> {
        self.0
    }

    /// Several optional slots as views.
    pub fn slots(
        slots: &'a [Option<ItemStack<M>>],
    ) -> impl Iterator<Item = Option<ItemView<'a, M>>> + 'a {
        slots.iter().map(|slot| slot.as_ref().map(ItemView::from))
    }
}

impl<'a, M: Mode> From<&'a ItemStack<M>> for ItemView<'a, M> {
    fn from(stack: &'a ItemStack<M>) -> Self {
        Self(stack)
    }
}

impl<'a, 'b: 'a, M: Mode> From<&'a ItemView<'b, M>> for ItemView<'a, M> {
    fn from(view: &'a ItemView<'b, M>) -> Self {
        *view
    }
}

impl<'a> From<&'a ItemSnapshot> for ItemView<'a, Picture> {
    fn from(snapshot: &'a ItemSnapshot) -> Self {
        Self(&snapshot.0)
    }
}

impl<M: Mode> Deref for ItemView<'_, M> {
    type Target = ItemStack<M>;

    fn deref(&self) -> &ItemStack<M> {
        self.0
    }
}

impl<M: Mode> fmt::Debug for ItemView<'_, M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<M: Mode, N: Mode> PartialEq<ItemView<'_, N>> for ItemView<'_, M> {
    fn eq(&self, other: &ItemView<'_, N>) -> bool {
        *self.0 == *other.0
    }
}

impl<M: Mode, N: Mode> PartialEq<ItemStack<N>> for ItemView<'_, M> {
    fn eq(&self, other: &ItemStack<N>) -> bool {
        *self.0 == *other
    }
}

pub trait AsItemView<M: Mode = Counted> {
    fn view(&self) -> Option<ItemView<'_, M>>;
}

impl<M: Mode> AsItemView<M> for Option<ItemStack<M>> {
    fn view(&self) -> Option<ItemView<'_, M>> {
        self.as_ref().map(ItemView::from)
    }
}

/// An owned, shareable picture of a stack at one moment, for data that
/// outlives the borrow or crosses to another actor. Cloning it shares the
/// picture.
#[derive(Clone)]
pub struct ItemSnapshot(Arc<ItemPicture>);

impl ItemSnapshot {
    pub fn of<'a, M: Mode>(view: impl Into<ItemView<'a, M>>) -> Self {
        Self(Arc::new(ItemPicture::of(view)))
    }

    pub fn view(&self) -> ItemView<'_, Picture> {
        ItemView(&self.0)
    }

    pub fn any(&self) -> super::AnyItem<'_> {
        super::AnyItem::Picture(&self.0)
    }

    /// Changes the pictured components (a template being finished).
    pub fn edit_components(&mut self, edit: impl FnOnce(&mut Components<Picture>)) {
        edit(&mut Arc::make_mut(&mut self.0).components);
    }

    /// A picture of `count` of `item` without components.
    pub fn new(item: tuff_vanilla_data::Item, count: u8) -> Self {
        ItemPicture::new(item, count).into()
    }

    /// A picture of `count` of `item` with `components`.
    pub fn describe(
        item: tuff_vanilla_data::Item,
        count: u8,
        components: impl Into<Components<Picture>>,
    ) -> Self {
        Self(Arc::new(ItemPicture::describe(item, count, components)))
    }

    /// A picture of `count` of `item` with a copy of `components`.
    pub fn describe_with<M: Mode>(
        item: tuff_vanilla_data::Item,
        count: u8,
        components: &Components<M>,
    ) -> Self {
        Self::describe(item, count, components.picture())
    }

    /// The same picture showing `count` items.
    pub fn with_count(self, count: u8) -> Self {
        Self(Arc::new(self.into_picture().with_count(count)))
    }

    /// The picture itself, copied when it is shared.
    pub fn into_picture(self) -> ItemPicture {
        Arc::unwrap_or_clone(self.0)
    }
}

impl From<ItemPicture> for ItemSnapshot {
    fn from(picture: ItemPicture) -> Self {
        Self(Arc::new(picture))
    }
}

impl Deref for ItemSnapshot {
    type Target = ItemPicture;

    fn deref(&self) -> &ItemPicture {
        &self.0
    }
}

impl fmt::Debug for ItemSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq for ItemSnapshot {
    fn eq(&self, other: &Self) -> bool {
        *self.0 == *other.0
    }
}

impl<M: Mode> PartialEq<ItemStack<M>> for ItemSnapshot {
    fn eq(&self, other: &ItemStack<M>) -> bool {
        *self.0 == *other
    }
}

/// A shareable picture of a component patch for templates (a command's
/// item argument, a custom item, a placed block's contents). Stamping it
/// onto real items goes through [`ItemStack::mint_with`].
#[derive(Clone, Default, PartialEq)]
pub struct ComponentsSnapshot(Arc<Components<Picture>>);

impl ComponentsSnapshot {
    pub fn of<M: Mode>(components: &Components<M>) -> Self {
        Self(Arc::new(components.picture()))
    }

    /// Takes components that only describe items (parsed from a command,
    /// built for a template).
    pub fn describing(components: Components<Picture>) -> Self {
        Self(Arc::new(components))
    }

    /// Edits the pictured components, copying them first when shared.
    pub fn edit(&mut self, edit: impl FnOnce(&mut Components<Picture>)) {
        edit(Arc::make_mut(&mut self.0));
    }
}

impl Deref for ComponentsSnapshot {
    type Target = Components<Picture>;

    fn deref(&self) -> &Components<Picture> {
        &self.0
    }
}

/// Edits the picture, copying it first when it is shared.
impl std::ops::DerefMut for ComponentsSnapshot {
    fn deref_mut(&mut self) -> &mut Components<Picture> {
        Arc::make_mut(&mut self.0)
    }
}

impl From<Components<Picture>> for ComponentsSnapshot {
    fn from(components: Components<Picture>) -> Self {
        Self::describing(components)
    }
}

impl fmt::Debug for ComponentsSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Pictures of every slot.
pub fn snapshot_slots<M: Mode>(slots: &[Option<ItemStack<M>>]) -> Vec<Option<ItemSnapshot>> {
    slots
        .iter()
        .map(|slot| slot.as_ref().map(ItemSnapshot::of))
        .collect()
}

impl<M: Mode> ItemStack<M> {
    pub fn view(&self) -> ItemView<'_, M> {
        ItemView(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::{BurnReason, MintReason};
    use tuff_vanilla_data::Item;

    #[tuff_core::watchdog]
    #[test]
    fn a_snapshot_keeps_the_stack_as_it_was() {
        let mut stack = ItemStack::mint(Item::Stone, 5, MintReason::Test);
        let snapshot = ItemSnapshot::of(&stack);
        stack.shrink(4, BurnReason::Test);
        assert_eq!(snapshot.count(), 5);
        assert_eq!(snapshot.view().item(), Item::Stone);
        assert_eq!(snapshot.clone(), ItemPicture::new(Item::Stone, 5));
        stack.burn(BurnReason::Test);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_view_reads_the_stack() {
        let stack = ItemStack::mint(Item::Dirt, 3, MintReason::Test);
        let view = stack.view();
        assert_eq!(view.count(), 3);
        assert_eq!(view, stack);
        let slots = [None, Some(stack)];
        assert_eq!(ItemView::slots(&slots).flatten().count(), 1);
        let [_, Some(stack)] = slots else {
            unreachable!()
        };
        stack.burn(BurnReason::Test);
    }
}
