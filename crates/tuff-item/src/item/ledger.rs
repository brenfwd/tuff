//! The doors through which item stacks come into and leave existence.
//! A stack is never copied: it moves between owners, and its count only
//! changes through [`ItemStack::split`], [`ItemStack::merge`] and the
//! helpers built on them, or through an audited mint or burn.

use tuff_nbt::Compound;
use tuff_vanilla_data::Item;

use super::{Components, ItemStack, ItemView};

/// Why items come into existence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MintReason {
    /// Loot tables: containers, block drops, mob drops, fishing, archaeology.
    Loot,
    /// A block broken or changed into its drops outside loot tables.
    BlockDrop,
    /// Equipment and inventory a mob spawns with.
    Spawn,
    /// A mob or other entity dropping or producing items outside loot tables.
    EntityDrop,
    /// A crafting, smithing, stonecutting, loom, grindstone or anvil result.
    Crafting,
    /// Furnace and brewing output.
    Processing,
    /// A villager or wandering trader's offer.
    Trade,
    /// What is left after using an item (bottles, buckets, bowls).
    Remainder,
    /// An item turning into another (filling a bucket, enchanting, dyeing).
    Transform,
    /// The creative inventory or pick block.
    Creative,
    /// `/give`, `/item` and other commands.
    Command,
    /// A plugin with an item grant.
    Plugin,
    /// Decoded from a client packet; trusted only where the client may
    /// create items.
    Network,
    /// A template that is never stored as a real item: icons, displays,
    /// recipe definitions, predicates.
    Template,
    /// Tests and fixtures.
    Test,
}

/// Why items leave existence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BurnReason {
    /// Eaten, drunk, thrown, placed, fired or otherwise used up.
    Consumed,
    /// An input of a crafting, smithing, processing or repair operation.
    CraftingInput,
    /// Given to a villager or trader as payment.
    TradePayment,
    /// A tool or armor piece broken by wear.
    Broken,
    /// Destroyed in the world: lava, fire, cactus, explosions, the void.
    Destroyed,
    /// An item entity despawned.
    Despawned,
    /// Still inside an entity removed for good (killed by a command,
    /// discarded, its chunk deleted).
    Discarded,
    /// Turned into another item (see [`MintReason::Transform`]).
    Transform,
    /// Deleted in the creative inventory.
    Creative,
    /// `/clear` and other commands.
    Command,
    /// A plugin with an item grant.
    Plugin,
    /// A loot function lowering the count of a stack it generated.
    Loot,
    /// A template or other stack that never was a real item.
    Template,
    /// Tests and fixtures.
    Test,
}

/// Whether a stack counts as real items. Pictures (templates, snapshots,
/// test fixtures) come and go freely; counted stacks must be stored, moved
/// or burned, never dropped.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum Tally {
    #[default]
    Counted,
    Picture,
}

impl PartialEq for Tally {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Tally {
    fn of(reason: MintReason) -> Self {
        match reason {
            MintReason::Template | MintReason::Test | MintReason::Network => Self::Picture,
            _ => Self::Counted,
        }
    }
}

/// Process-wide item accounting: what came in through the doors, what left
/// through them, and what was lost without a door. Counted stacks only.
pub mod conservation {
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use super::LedgerTotals;

    static CHECKS: AtomicBool = AtomicBool::new(true);
    static MINTED: AtomicU64 = AtomicU64::new(0);
    static BURNED: AtomicU64 = AtomicU64::new(0);
    static LOADED: AtomicU64 = AtomicU64::new(0);
    static LOST: AtomicU64 = AtomicU64::new(0);
    static VIOLATIONS: AtomicU64 = AtomicU64::new(0);

    thread_local! {
        static RELEASING: Cell<u32> = const { Cell::new(0) };
    }

    /// Turns the release-build checks on or off (`tuff.items.conservationChecks`).
    /// Debug and test builds always check.
    pub fn set_checks(on: bool) {
        CHECKS.store(on, Ordering::Relaxed);
    }

    /// Whether operations are checked and rejected on a violation.
    pub fn checks() -> bool {
        cfg!(debug_assertions) || CHECKS.load(Ordering::Relaxed)
    }

    pub fn totals() -> LedgerTotals {
        LedgerTotals {
            minted: MINTED.load(Ordering::Relaxed),
            burned: BURNED.load(Ordering::Relaxed),
            loaded: LOADED.load(Ordering::Relaxed),
            lost: LOST.load(Ordering::Relaxed),
            violations: VIOLATIONS.load(Ordering::Relaxed),
        }
    }

    /// Runs `f`, in which dropped stacks are leaving memory after being
    /// stored (unloads, shutdown) or nested in a stack that left: not lost.
    pub fn releasing<R>(f: impl FnOnce() -> R) -> R {
        struct Guard;
        impl Drop for Guard {
            fn drop(&mut self) {
                RELEASING.with(|depth| depth.set(depth.get() - 1));
            }
        }
        RELEASING.with(|depth| depth.set(depth.get() + 1));
        let _guard = Guard;
        f()
    }

    /// Marks the current thread as a test's own while the guard lives:
    /// stacks a test drops there are fixtures. Actors on other threads stay
    /// checked.
    pub fn fixture_thread() -> FixtureThread {
        RELEASING.with(|depth| depth.set(depth.get() + 1));
        FixtureThread(std::marker::PhantomData)
    }

    pub struct FixtureThread(std::marker::PhantomData<*const ()>);

    impl Drop for FixtureThread {
        fn drop(&mut self) {
            RELEASING.with(|depth| depth.set(depth.get() - 1));
        }
    }

    /// Drops `value` as stored state leaving memory.
    pub fn release<T>(value: T) {
        releasing(|| drop(value));
    }

    /// An actor's future whose state leaves memory with it: the actor saved
    /// what it owned, or the world is stopping and storage keeps the last
    /// saved items.
    pub fn actor<F: std::future::Future>(future: F) -> Actor<F> {
        Actor(Some(Box::pin(future)))
    }

    pub struct Actor<F>(Option<std::pin::Pin<Box<F>>>);

    impl<F: std::future::Future> std::future::Future for Actor<F> {
        type Output = F::Output;

        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<F::Output> {
            self.0
                .as_mut()
                .expect("an actor future polled after it was dropped")
                .as_mut()
                .poll(cx)
        }
    }

    impl<F> Drop for Actor<F> {
        fn drop(&mut self) {
            release(self.0.take());
        }
    }

    pub(super) fn is_releasing() -> bool {
        RELEASING.with(|depth| depth.get() > 0)
    }

    pub(super) fn minted(n: u8) {
        MINTED.fetch_add(u64::from(n), Ordering::Relaxed);
    }

    pub(super) fn burned(n: u8) {
        BURNED.fetch_add(u64::from(n), Ordering::Relaxed);
    }

    pub(super) fn loaded(n: u8) {
        LOADED.fetch_add(u64::from(n), Ordering::Relaxed);
    }

    pub(super) fn lost(stack: &super::ItemStack) {
        LOST.fetch_add(u64::from(stack.count), Ordering::Relaxed);
        VIOLATIONS.fetch_add(1, Ordering::Relaxed);
        if cfg!(debug_assertions) && !std::thread::panicking() {
            panic!(
                "item stack dropped without being stored or burned: {stack:?} at {}",
                dropped_at()
            );
        }
        log::error!("item conservation: {stack:?} dropped without being stored or burned");
    }

    fn dropped_at() -> String {
        let trace = std::backtrace::Backtrace::force_capture().to_string();
        trace
            .lines()
            .filter_map(|line| line.trim().strip_prefix("at "))
            .filter(|at| !at.starts_with("/rustc/") && !at.contains("/item/ledger.rs"))
            .filter(|at| !at.contains("/cargo/registry/") && !at.contains("/library/"))
            .take(4)
            .collect::<Vec<_>>()
            .join(" <- ")
    }

    /// Records a failed check of an item operation. The caller rejects the
    /// operation and keeps both sides' state.
    pub fn violation(operation: &str, detail: std::fmt::Arguments<'_>) {
        VIOLATIONS.fetch_add(1, Ordering::Relaxed);
        log::error!("item conservation: {operation} rejected: {detail}");
    }
}

/// A reading of the [`conservation`] counters, in items.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LedgerTotals {
    pub minted: u64,
    pub burned: u64,
    pub loaded: u64,
    /// Items in counted stacks dropped without a store or burn.
    pub lost: u64,
    /// Lost stacks plus rejected operations.
    pub violations: u64,
}

impl LedgerTotals {
    /// The change from `earlier` to this reading.
    pub fn since(self, earlier: Self) -> Self {
        Self {
            minted: self.minted - earlier.minted,
            burned: self.burned - earlier.burned,
            loaded: self.loaded - earlier.loaded,
            lost: self.lost - earlier.lost,
            violations: self.violations - earlier.violations,
        }
    }
}

impl Drop for ItemStack {
    fn drop(&mut self) {
        let counted = matches!(self.tally, Tally::Counted);
        if counted && !self.is_empty() && !conservation::is_releasing() {
            conservation::lost(self);
        }
        if self.components.0.is_some() {
            let nested = std::mem::take(&mut self.components);
            conservation::release(nested);
        }
    }
}

impl ItemStack {
    /// No item: air with a count of zero.
    pub fn empty() -> Self {
        Self {
            item: Item::Air,
            count: 0,
            components: Components::default(),
            tally: Tally::Picture,
        }
    }

    /// The door for new items.
    pub fn mint(item: Item, count: u8, reason: MintReason) -> Self {
        let tally = Tally::of(reason);
        if matches!(tally, Tally::Counted) && item != Item::Air {
            conservation::minted(count);
        }
        Self {
            item,
            count,
            components: Components::default(),
            tally,
        }
    }

    /// New items with the item and components of `template`.
    pub fn mint_like<'a>(template: impl Into<ItemView<'a>>, count: u8, reason: MintReason) -> Self {
        let template = template.into().get();
        Self::mint(template.item, count, reason).with_components(template.components.picture_copy())
    }

    /// New items of `item` carrying a copy of `components` (nested stacks
    /// included, minted with them).
    pub fn mint_with(item: Item, count: u8, components: &Components, reason: MintReason) -> Self {
        Self::mint(item, count, reason).with_components(components.picture_copy())
    }

    /// Tests and fixtures only: new items like this stack, minted for
    /// [`MintReason::Test`]. Only built for this crate's tests and with the
    /// `test-support` feature, which only dev-dependencies enable.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_copy(&self) -> Self {
        Self::mint_like(self, self.count, MintReason::Test)
    }

    /// New items read from their `{id, count, components}` form.
    /// `None` for unknown items and air.
    pub fn mint_from_nbt(nbt: &Compound, reason: MintReason) -> Option<Self> {
        let mut stack = Self::decode_nbt(nbt)?;
        stack.tally = Tally::of(reason);
        if matches!(stack.tally, Tally::Counted) {
            conservation::minted(stack.count);
        }
        Some(stack)
    }

    /// Stacks loaded from storage: they existed before the save.
    /// Only loaders call this.
    pub fn from_storage(nbt: &Compound) -> Option<Self> {
        let mut stack = Self::decode_nbt(nbt)?;
        stack.tally = Tally::Counted;
        conservation::loaded(stack.count);
        Some(stack)
    }

    /// A look at items kept in stored NBT, which stays their owner: a
    /// picture, never real items.
    pub fn picture_from_nbt(nbt: &Compound) -> Option<Self> {
        Self::decode_nbt(nbt)
    }

    /// The door for items leaving existence.
    pub fn burn(mut self, _reason: BurnReason) {
        if matches!(self.tally, Tally::Counted) && !self.is_empty() {
            conservation::burned(self.count);
        }
        self.count = 0;
    }

    /// Turns this stack into `count` of `item` keeping its components:
    /// burns it and mints the result.
    pub fn into_item(mut self, item: Item, count: u8, mint: MintReason, burn: BurnReason) -> Self {
        let components = std::mem::take(&mut self.components);
        self.burn(burn);
        Self::mint(item, count, mint).with_components(components)
    }

    /// The item this stack holds. It only changes through a mint, a burn
    /// or [`Self::transform`].
    #[inline]
    pub fn item(&self) -> Item {
        self.item
    }

    /// Turns this stack into `item` in place, keeping its count and
    /// components: the ledger records a burn of the old items and a mint of
    /// the new ones. A picture stays a picture.
    pub fn transform(&mut self, item: Item, mint: MintReason, _burn: BurnReason) {
        if matches!(self.tally, Tally::Counted) {
            if !self.is_empty() {
                conservation::burned(self.count);
            }
            self.tally = Tally::of(mint);
            if matches!(self.tally, Tally::Counted) && item != Item::Air && self.count > 0 {
                conservation::minted(self.count);
            }
        }
        self.item = item;
    }

    /// Whether this stack is a picture (template, snapshot or test
    /// fixture) rather than counted items.
    pub fn is_picture(&self) -> bool {
        matches!(self.tally, Tally::Picture)
    }

    /// A copy for a picture or a template. Only the snapshot and mint
    /// doors call it.
    pub(crate) fn picture_copy(&self) -> Self {
        Self {
            item: self.item,
            count: self.count,
            components: self.components.picture_copy(),
            tally: Tally::Picture,
        }
    }

    pub fn count(&self) -> u8 {
        self.count
    }

    /// Splits off `n` items (at least one, at most all): the taken part and
    /// what stays, `None` when everything was taken.
    pub fn split(self, n: u8) -> (Self, Option<Self>) {
        let n = n.clamp(1, self.count.max(1));
        if n >= self.count {
            return (self, None);
        }
        let mut rest = self;
        let taken = Self {
            item: rest.item,
            count: n,
            components: rest.components.picture_copy(),
            tally: rest.tally,
        };
        rest.count -= n;
        (taken, Some(rest))
    }

    /// One stack holding both, when they stack together and the total fits
    /// a count; otherwise both back unchanged.
    pub fn merge(a: Self, b: Self) -> Result<Self, (Self, Self)> {
        match a.count.checked_add(b.count) {
            Some(total) if a.stacks_with(&b) => {
                let mut merged = a;
                merged.count = total;
                merged.absorb_tally(b);
                Ok(merged)
            }
            _ => Err((a, b)),
        }
    }

    /// Ends `other` after its items were added to this stack. A picture
    /// joined into a counted stack becomes counted items: minted.
    fn absorb_tally(&mut self, mut other: Self) {
        match (self.tally, other.tally) {
            (Tally::Counted, Tally::Picture) => conservation::minted(other.count),
            (Tally::Picture, Tally::Counted) => conservation::burned(other.count),
            _ => {}
        }
        other.count = 0;
    }

    /// Takes up to `n` items off this stack. Taking all leaves an empty
    /// husk (count zero) for the caller to clear.
    pub fn take(&mut self, n: u8) -> Self {
        let n = n.min(self.count);
        self.count -= n;
        Self {
            item: self.item,
            count: n,
            components: self.components.picture_copy(),
            tally: self.tally,
        }
    }

    /// Whether no items are left (an emptied husk or [`empty`](Self::empty)).
    pub fn is_empty(&self) -> bool {
        self.count == 0 || self.item == Item::Air
    }

    /// Adds all of `other` when it stacks with this one and the total fits
    /// a count; otherwise hands it back.
    pub fn join(&mut self, other: Self) -> Result<(), Self> {
        match self.count.checked_add(other.count) {
            Some(total) if self.stacks_with(&other) || other.count == 0 => {
                self.count = total;
                self.absorb_tally(other);
                Ok(())
            }
            _ => Err(other),
        }
    }

    /// Moves up to `n` items of `from` onto this stack when they stack
    /// together, possibly leaving `from` an empty husk; returns how many
    /// moved.
    pub fn transfer(&mut self, from: &mut Self, n: u8) -> u8 {
        if !self.stacks_with(from) {
            return 0;
        }
        let n = n.min(from.count).min(u8::MAX - self.count);
        from.count -= n;
        self.count += n;
        match (self.tally, from.tally) {
            (Tally::Counted, Tally::Picture) => conservation::minted(n),
            (Tally::Picture, Tally::Counted) => conservation::burned(n),
            _ => {}
        }
        n
    }

    /// [`join`](Self::join), pushing `other` onto `spill` when it cannot.
    pub fn join_or_drop(&mut self, other: Self, spill: &mut Vec<Self>) {
        if let Err(other) = self.join(other) {
            spill.push(other);
        }
    }

    /// Moves as many items of `other` into this stack as keep it within
    /// `limit`; returns what is left of `other`. Stacks that do not stack
    /// together are returned whole.
    pub fn absorb(&mut self, other: Self, limit: u8) -> Option<Self> {
        if !self.stacks_with(&other) {
            return Some(other);
        }
        let room = limit.saturating_sub(self.count);
        if room == 0 {
            return Some(other);
        }
        let (moved, rest) = other.split(room);
        self.count += moved.count;
        self.absorb_tally(moved);
        rest
    }

    /// Moves up to `n` items from `from` into this stack, within `limit`;
    /// returns how many moved.
    pub fn absorb_from(&mut self, from: &mut Option<Self>, n: u8, limit: u8) -> u8 {
        let Some(source) = from.take() else {
            return 0;
        };
        let before = source.count;
        let (part, rest) = source.split(n.max(1));
        let part_count = part.count;
        let back = self.absorb(part, limit);
        let returned = back.as_ref().map_or(0, |stack| stack.count);
        *from = match (back, rest) {
            (Some(mut back), Some(rest)) => {
                back.count += rest.count;
                back.absorb_tally(rest);
                Some(back)
            }
            (back, rest) => back.or(rest),
        };
        let after = from.as_ref().map_or(0, |stack| stack.count);
        if before != after + part_count - returned {
            conservation::violation(
                "absorb",
                format_args!("{before} items became {after} + {}", part_count - returned),
            );
            debug_assert!(false, "absorb_from lost or made items");
        }
        part_count - returned
    }

    /// Adds `n` new items to this stack.
    pub fn grow(&mut self, n: u8, _reason: MintReason) {
        let before = self.count;
        self.count = self.count.saturating_add(n);
        if matches!(self.tally, Tally::Counted) && self.item != Item::Air {
            conservation::minted(self.count - before);
        }
    }

    /// Destroys up to `n` items of this stack, possibly leaving an empty
    /// husk.
    pub fn shrink(&mut self, n: u8, reason: BurnReason) {
        if n > 0 {
            self.take(n).burn(reason);
        }
    }

    /// Destroys up to `n` items of the stack in `slot`, emptying it when
    /// none are left; returns how many were destroyed.
    pub fn burn_from(slot: &mut Option<Self>, n: u8, reason: BurnReason) -> u8 {
        match Self::take_from(slot, n) {
            Some(part) => {
                let count = part.count;
                part.burn(reason);
                count
            }
            None => 0,
        }
    }

    /// Takes up to `n` items out of `slot`, emptying it when none are
    /// left.
    pub fn take_from(slot: &mut Option<Self>, n: u8) -> Option<Self> {
        if n == 0 {
            return None;
        }
        let (taken, rest) = slot.take()?.split(n);
        *slot = rest;
        Some(taken)
    }

    /// Puts `stack` back into `slot`, which it was taken from.
    pub fn put_back(slot: &mut Option<Self>, stack: Self) {
        match slot {
            Some(held) => {
                if let Err(stack) = held.join(stack) {
                    conservation::violation(
                        "put back",
                        format_args!("{stack:?} does not fit back into {held:?}"),
                    );
                    debug_assert!(false, "put back a stack that does not fit: {stack:?}");
                    stack.burn(BurnReason::Destroyed);
                }
            }
            None => *slot = Some(stack),
        }
    }

    /// Keeps `count` items (at most all) and burns the rest.
    pub fn keep(mut self, count: u8, reason: BurnReason) -> Self {
        let excess = self.count.saturating_sub(count);
        self.shrink(excess, reason);
        self
    }

    /// Changes the count to `count` by minting or burning the difference.
    pub fn resize(&mut self, count: u8, mint: MintReason, burn: BurnReason) {
        match count.cmp(&self.count) {
            std::cmp::Ordering::Greater => self.grow(count - self.count, mint),
            std::cmp::Ordering::Less => {
                let n = self.count - count;
                self.count = count;
                Self {
                    item: self.item,
                    count: n,
                    components: Components::default(),
                    tally: self.tally,
                }
                .burn(burn);
            }
            std::cmp::Ordering::Equal => {}
        }
    }
}

/// The slots of a container as a read-only view: one block entity's, or
/// both halves of a double chest joined in menu order.
#[derive(Debug, Clone, Copy)]
pub enum SlotsCow<'a> {
    Borrowed(&'a [Option<ItemStack>]),
    Joined(&'a [Option<ItemStack>], &'a [Option<ItemStack>]),
}

impl Default for SlotsCow<'_> {
    fn default() -> Self {
        Self::Borrowed(&[])
    }
}

impl<'a> SlotsCow<'a> {
    pub fn iter(&self) -> impl Iterator<Item = &'a Option<ItemStack>> + 'a {
        let (first, second): (&'a [Option<ItemStack>], &'a [Option<ItemStack>]) = match *self {
            Self::Borrowed(slots) => (slots, &[]),
            Self::Joined(first, second) => (first, second),
        };
        first.iter().chain(second)
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Borrowed(slots) => slots.len(),
            Self::Joined(first, second) => first.len() + second.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(count: u8) -> ItemStack {
        ItemStack::mint(Item::Stone, count, MintReason::Test)
    }

    #[tuff_core::watchdog]
    #[test]
    fn split_and_merge_keep_the_total() {
        let (taken, rest) = stack(10).split(3);
        assert_eq!(
            (taken.count(), rest.as_ref().map(ItemStack::count)),
            (3, Some(7))
        );
        let merged = ItemStack::merge(taken, rest.unwrap()).unwrap();
        assert_eq!(merged.count(), 10);
        let (all, none) = merged.split(200);
        assert_eq!((all.count(), none.is_none()), (10, true));
        let other = ItemStack::mint(Item::Dirt, 1, MintReason::Test);
        let (a, b) = ItemStack::merge(all, other).unwrap_err();
        assert_eq!((a.count(), b.count()), (10, 1));
    }

    #[tuff_core::watchdog]
    #[test]
    fn moves_between_stacks_keep_the_total() {
        let mut target = stack(60);
        let mut source = stack(10);
        assert_eq!(target.transfer(&mut source, 4), 4);
        assert_eq!((target.count(), source.count()), (64, 6));
        let rest = target.absorb(source, 64).unwrap();
        assert_eq!((target.count(), rest.count()), (64, 6));
        let mut slot = Some(rest);
        assert_eq!(
            ItemStack::take_from(&mut slot, 2).map(|s| s.count()),
            Some(2)
        );
        assert_eq!(ItemStack::burn_from(&mut slot, 10, BurnReason::Test), 4);
        assert!(slot.is_none());
        let mut holder = stack(250);
        assert!(holder.join(stack(10)).is_err());
        assert_eq!(holder.count(), 250);
    }
}
