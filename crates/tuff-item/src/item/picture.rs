//! The two kinds of item stack. A counted stack ([`ItemStack`], the
//! default) is real items: linear, entering and leaving existence only
//! through the ledger's doors, never dropped. A picture ([`ItemPicture`])
//! only shows items: templates, displays, recipe results, decoded packets
//! and snapshots. Both share the read API; the only conversions are the
//! doors `ItemStack::mint_like` (picture to real, nested contents minted
//! with it), `ItemStack::into_picture` (real to picture, burned) and
//! [`ItemPicture::of`] (a look, no accounting).

use std::fmt;
use std::sync::LazyLock;

use tuff_nbt::Compound;
use tuff_vanilla_data::Item;

use super::{ComponentPatch, Components, ItemStack, MAX_COUNT};

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Counted {}
    impl Sealed for super::Picture {}
}

/// Which kind of stack: [`Counted`] or [`Picture`].
pub trait Mode:
    sealed::Sealed + Default + Copy + fmt::Debug + PartialEq + Send + Sync + 'static
{
    /// What a stack carries besides its items: counted stacks keep a
    /// [`History`](super::History).
    type Extra: fmt::Debug + Send + Sync;

    #[doc(hidden)]
    #[track_caller]
    fn begin(item: Item, count: u8, origin: fmt::Arguments<'_>) -> Self::Extra;

    #[doc(hidden)]
    #[track_caller]
    fn record(extra: &mut Self::Extra, what: fmt::Arguments<'_>);

    #[doc(hidden)]
    fn on_drop(stack: &mut ItemStack<Self>);

    #[doc(hidden)]
    fn empty_patch() -> &'static ComponentPatch<Self>;

    #[doc(hidden)]
    fn any(stack: &ItemStack<Self>) -> AnyItem<'_>;
}

/// A stack of either kind behind one type, for function pointers and
/// trait objects that cannot be generic over [`Mode`].
#[derive(Clone, Copy, Debug)]
pub enum AnyItem<'a> {
    Counted(&'a ItemStack),
    Picture(&'a ItemPicture),
}

/// Evaluates `$body` with `$stack` bound to the stack behind an
/// [`AnyItem`], whichever kind it is.
#[macro_export]
macro_rules! with_item {
    ($any:expr, |$stack:ident| $body:expr) => {
        match $any {
            $crate::item::AnyItem::Counted($stack) => $body,
            $crate::item::AnyItem::Picture($stack) => $body,
        }
    };
}

impl<'a, M: Mode> From<&'a ItemStack<M>> for AnyItem<'a> {
    fn from(stack: &'a ItemStack<M>) -> Self {
        stack.any()
    }
}

impl<'a, M: Mode> From<super::ItemView<'a, M>> for AnyItem<'a> {
    fn from(view: super::ItemView<'a, M>) -> Self {
        view.get().any()
    }
}

impl<'a> From<&'a super::ItemSnapshot> for AnyItem<'a> {
    fn from(snapshot: &'a super::ItemSnapshot) -> Self {
        AnyItem::Picture(snapshot)
    }
}

impl<'a> From<&AnyItem<'a>> for AnyItem<'a> {
    fn from(item: &AnyItem<'a>) -> Self {
        *item
    }
}

impl AnyItem<'_> {
    pub fn item(self) -> Item {
        with_item!(self, |stack| stack.item())
    }

    pub fn count(self) -> u8 {
        with_item!(self, |stack| stack.count())
    }
}

impl<M: Mode> ItemStack<M> {
    pub fn any(&self) -> AnyItem<'_> {
        M::any(self)
    }
}

/// Real items, accounted for by the ledger.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counted;

/// A picture of items that are not real.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Picture;

pub type ItemPicture = ItemStack<Picture>;

static EMPTY_PICTURE_PATCH: LazyLock<ComponentPatch<Picture>> = LazyLock::new(Default::default);

impl Mode for Picture {
    type Extra = ();

    fn begin(_: Item, _: u8, _: fmt::Arguments<'_>) {}

    fn record(_: &mut (), _: fmt::Arguments<'_>) {}

    fn on_drop(_: &mut ItemPicture) {}

    fn empty_patch() -> &'static ComponentPatch<Self> {
        &EMPTY_PICTURE_PATCH
    }

    fn any(stack: &ItemPicture) -> AnyItem<'_> {
        AnyItem::Picture(stack)
    }
}

impl Clone for ItemPicture {
    fn clone(&self) -> Self {
        Self::describe(self.item, self.count, self.components.clone())
    }
}

impl Clone for Components<Picture> {
    fn clone(&self) -> Self {
        self.picture()
    }
}

impl Clone for ComponentPatch<Picture> {
    fn clone(&self) -> Self {
        self.picture()
    }
}

impl ItemPicture {
    /// A picture of `count` of `item` without components.
    pub fn new(item: Item, count: u8) -> Self {
        Self::describe(item, count, Components::default())
    }

    /// A picture of `count` of `item` with `components`.
    pub fn describe(item: Item, count: u8, components: impl Into<Components<Picture>>) -> Self {
        let mut components = components.into();
        components.compact();
        Self::assemble(item, count, components, format_args!("describe"))
    }

    /// A picture of `stack` as it is now, nested stacks included. No
    /// accounting: the stack stays where it is.
    pub fn of<'a, M: Mode>(stack: impl Into<super::ItemView<'a, M>>) -> Self {
        let stack = stack.into().get();
        Self::describe(stack.item, stack.count, stack.components.picture())
    }

    /// A stack in its `{id, count, components}` form, as a picture.
    /// `None` for unknown items and air.
    pub fn from_nbt(nbt: &Compound) -> Option<Self> {
        let item = Item::from_name(nbt.get_str("id")?)?;
        if item == Item::Air {
            return None;
        }
        let count = nbt.get_int("count").unwrap_or(1).clamp(1, MAX_COUNT);
        let components = nbt
            .get_compound("components")
            .map(Components::from_nbt)
            .unwrap_or_default();
        Some(Self::describe(item, count as u8, components))
    }

    /// The same picture showing `count` items.
    pub fn with_count(mut self, count: u8) -> Self {
        self.count = count;
        self
    }

    pub fn set_count(&mut self, count: u8) {
        self.count = count;
    }

    /// The same picture showing `item`.
    pub fn with_item(mut self, item: Item) -> Self {
        self.item = item;
        self
    }
}
