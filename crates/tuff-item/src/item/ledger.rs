//! The doors through which item stacks come into and leave existence.
//! A counted stack is never copied: it moves between owners, and its count
//! only changes through [`ItemStack::split`], [`ItemStack::merge`] and the
//! helpers built on them, or through an audited mint or burn. Stacks nested
//! in a counted stack are counted too, once per stack: burning a stack
//! burns its contents, a part split off gets its own minted copy of them,
//! and a stack merged away or emptied takes its copy with it.

use std::fmt;
use std::sync::LazyLock;

use tuff_nbt::Compound;
use tuff_vanilla_data::Item;

use super::{
    ComponentPatch, Components, Counted, History, ItemPicture, ItemStack, ItemView, Mode, Picture,
};

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
    /// A stand-in for items that are not real yet or are owned elsewhere,
    /// where counted operations are needed: a result slot's preview (its
    /// items are made when taken), a session's mirror of slots another
    /// actor owns, a click worked out on copies before real stacks move, a
    /// probe run through recipes. Counted and drop-checked like any stack,
    /// so one cannot leak unnoticed; burned with [`BurnReason::Preview`].
    Preview,
    /// Tests and fixtures: real items like any other.
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
    /// Turned into a picture (see [`ItemStack::into_picture`]).
    Picture,
    /// A stand-in replaced or let go (see [`MintReason::Preview`]).
    Preview,
    /// Tests and fixtures.
    Test,
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

    pub fn is_releasing() -> bool {
        RELEASING.with(|depth| depth.get() > 0)
    }

    pub(super) fn minted(n: u64) {
        MINTED.fetch_add(n, Ordering::Relaxed);
    }

    pub(super) fn burned(n: u64) {
        BURNED.fetch_add(n, Ordering::Relaxed);
    }

    pub(super) fn loaded(n: u64) {
        LOADED.fetch_add(n, Ordering::Relaxed);
    }

    pub(super) fn lost(stack: &super::ItemStack) {
        LOST.fetch_add(
            u64::from(stack.count) + stack.nested_count(),
            Ordering::Relaxed,
        );
        VIOLATIONS.fetch_add(1, Ordering::Relaxed);
        if cfg!(debug_assertions) && !std::thread::panicking() {
            panic!(
                "item stack dropped without being stored or burned: {stack:?} at {}\n{}",
                dropped_at(),
                stack.history().report()
            );
        }
        log::error!(
            "item conservation: {stack:?} dropped without being stored or burned\n{}",
            stack.history().report()
        );
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

static EMPTY_COUNTED_PATCH: LazyLock<ComponentPatch<Counted>> = LazyLock::new(Default::default);

impl Mode for Counted {
    type Extra = History;

    #[track_caller]
    fn begin(item: Item, count: u8, origin: fmt::Arguments<'_>) -> History {
        History::begin(item, count, origin)
    }

    #[track_caller]
    fn record(history: &mut History, what: fmt::Arguments<'_>) {
        history.record(what);
    }

    /// A stack with items left is lost unless it is leaving memory after a
    /// save; an emptied husk takes its copy of any contents with it.
    fn on_drop(stack: &mut ItemStack) {
        if stack.is_empty() {
            conservation::burned(stack.nested_count());
        } else if !conservation::is_releasing() {
            conservation::lost(stack);
        }
        if stack.components.is_allocated() {
            conservation::release(std::mem::take(&mut stack.components));
        }
    }

    fn empty_patch() -> &'static ComponentPatch<Self> {
        &EMPTY_COUNTED_PATCH
    }

    fn any(stack: &ItemStack) -> super::AnyItem<'_> {
        super::AnyItem::Counted(stack)
    }
}

impl<M: Mode> ItemStack<M> {
    /// The item this stack holds. On a counted stack it only changes
    /// through a mint, a burn or [`ItemStack::transform`].
    #[inline]
    pub fn item(&self) -> Item {
        self.item
    }

    pub fn count(&self) -> u8 {
        self.count
    }

    /// Whether no items are left (an emptied husk or an empty stack).
    pub fn is_empty(&self) -> bool {
        self.count == 0 || self.item == Item::Air
    }

    /// How many items the stacks nested in this one hold, at every depth.
    pub fn nested_count(&self) -> u64 {
        self.components.nested_count()
    }

    /// Tests and fixtures only: new items like this stack, minted for
    /// [`MintReason::Test`]. Only built for this crate's tests and with the
    /// `test-support` feature, which only dev-dependencies enable.
    #[cfg(any(test, feature = "test-support"))]
    #[track_caller]
    pub fn test_copy(&self) -> ItemStack {
        ItemStack::mint_like(self, self.count, MintReason::Test)
    }
}

impl<M: Mode> ComponentPatch<M> {
    /// How many items the stacks nested in this patch hold, at every depth.
    pub fn nested_count(&self) -> u64 {
        self.nested()
            .filter(|stack| !stack.is_empty())
            .map(|stack| u64::from(stack.count) + stack.nested_count())
            .sum()
    }
}

impl Components<Picture> {
    /// Counted components stamped from this picture: nested stacks are
    /// minted for `reason`.
    #[track_caller]
    pub fn mint(&self, reason: MintReason) -> Components {
        self.copy_nested(Some(|stack: &ItemPicture| {
            ItemStack::mint_like(stack, stack.count, reason)
        }))
    }
}

impl ItemStack {
    /// No item: air with a count of zero.
    #[track_caller]
    pub fn empty() -> Self {
        Self::assemble(Item::Air, 0, Components::default(), format_args!("empty"))
    }

    /// The door for new items.
    #[track_caller]
    pub fn mint(item: Item, count: u8, reason: MintReason) -> Self {
        if item != Item::Air {
            conservation::minted(u64::from(count));
        }
        Self::assemble(
            item,
            count,
            Components::default(),
            format_args!("mint {reason:?}"),
        )
    }

    /// New items with the item and components of `template`, a picture or
    /// another stack; its nested stacks are minted with them.
    #[track_caller]
    pub fn mint_like<'a, M: Mode>(
        template: impl Into<ItemView<'a, M>>,
        count: u8,
        reason: MintReason,
    ) -> Self {
        let template = template.into().get();
        Self::mint_with(template.item, count, &template.components, reason)
    }

    /// New items of `item` carrying a copy of `components`, nested stacks
    /// minted with them.
    #[track_caller]
    pub fn mint_with<M: Mode>(
        item: Item,
        count: u8,
        components: &Components<M>,
        reason: MintReason,
    ) -> Self {
        if item != Item::Air && count > 0 {
            conservation::minted(u64::from(count));
        }
        let components = components.copy_nested(Some(|stack: &ItemStack<M>| {
            ItemStack::mint_like(stack, stack.count, reason)
        }));
        Self::assemble(item, count, components, format_args!("mint {reason:?}"))
    }

    /// New items read from their `{id, count, components}` form.
    /// `None` for unknown items and air.
    #[track_caller]
    pub fn mint_from_nbt(nbt: &Compound, reason: MintReason) -> Option<Self> {
        let (item, count, components) =
            Self::decode_parts(nbt, &mut |nbt| Self::mint_from_nbt(nbt, reason))?;
        conservation::minted(u64::from(count));
        Some(Self::assemble(
            item,
            count,
            components,
            format_args!("mint {reason:?}"),
        ))
    }

    /// Stacks loaded from storage, nested ones included: they existed
    /// before the save. Only loaders call this.
    #[track_caller]
    pub fn from_storage(nbt: &Compound) -> Option<Self> {
        let (item, count, components) = Self::decode_parts(nbt, &mut Self::from_storage)?;
        conservation::loaded(u64::from(count));
        Some(Self::assemble(
            item,
            count,
            components,
            format_args!("load from storage"),
        ))
    }

    /// The door for stacks going to storage, the counterpart of
    /// [`from_storage`](Self::from_storage).
    pub fn to_storage(&self) -> Compound {
        self.to_nbt()
    }

    /// The door from real items to a picture of them: they are burned.
    #[track_caller]
    pub fn into_picture(self) -> ItemPicture {
        let picture = ItemPicture::of(&self);
        self.burn(BurnReason::Picture);
        picture
    }

    /// The door for items leaving existence, with everything nested in
    /// them.
    #[track_caller]
    pub fn burn(mut self, reason: BurnReason) {
        let own = if self.is_empty() {
            0
        } else {
            u64::from(self.count)
        };
        conservation::burned(own + self.nested_count());
        self.note(format_args!("burn {reason:?}"));
        self.count = 0;
        if self.components.is_allocated() {
            conservation::release(std::mem::take(&mut self.components));
        }
    }

    /// Replaces the components with those `nbt` (their stored form)
    /// describes: stacks nested in the old ones are burned for `burn`,
    /// the ones `nbt` describes minted for `mint`.
    #[track_caller]
    pub fn set_components_nbt(&mut self, nbt: &Compound, mint: MintReason, burn: BurnReason) {
        let new = Components::decode(nbt, &mut |nbt| Self::mint_from_nbt(nbt, mint));
        let old = std::mem::replace(&mut self.components, new);
        conservation::burned(old.nested_count());
        conservation::release(old);
        self.note(format_args!("set components from nbt ({mint:?}, {burn:?})"));
    }

    /// Turns this stack into `count` of `item` keeping its components:
    /// burns it and mints the result.
    #[track_caller]
    pub fn into_item(mut self, item: Item, count: u8, mint: MintReason, burn: BurnReason) -> Self {
        let components = std::mem::take(&mut self.components);
        let old = self.history().id();
        self.burn(burn);
        let mut stack = Self::mint(item, count, mint).with_components(components);
        stack.note(format_args!("from {old}"));
        stack
    }

    /// Turns this stack into `item` in place, keeping its count and
    /// components: the ledger records a burn of the old items and a mint of
    /// the new ones.
    #[track_caller]
    pub fn transform(&mut self, item: Item, mint: MintReason, burn: BurnReason) {
        if !self.is_empty() {
            conservation::burned(u64::from(self.count));
        }
        if item != Item::Air && self.count > 0 {
            conservation::minted(u64::from(self.count));
        }
        self.note(format_args!(
            "transform {} -> {} ({mint:?}, {burn:?})",
            self.item.name(),
            item.name()
        ));
        self.item = item;
    }

    /// The components a part split off this stack carries: a copy, with
    /// its own minted copy of any nested stacks.
    #[track_caller]
    fn split_components(&self) -> Components {
        self.components.copy_nested(Some(ItemStack::split_copy))
    }

    /// A minted copy of a stack nested in one being split: every item of
    /// the stack carries the same contents.
    #[track_caller]
    fn split_copy(&self) -> Self {
        if !self.is_empty() {
            conservation::minted(u64::from(self.count));
        }
        Self::assemble(
            self.item,
            self.count,
            self.split_components(),
            format_args!("contents copied for a split of {}", self.history().id()),
        )
    }

    /// Splits off `n` items (at least one, at most all): the taken part and
    /// what stays, `None` when everything was taken.
    #[track_caller]
    pub fn split(self, n: u8) -> (Self, Option<Self>) {
        let n = n.clamp(1, self.count.max(1));
        if n >= self.count {
            return (self, None);
        }
        let mut rest = self;
        let taken = Self::assemble(
            rest.item,
            n,
            rest.split_components(),
            format_args!("split from {}", rest.history().id()),
        );
        rest.count -= n;
        rest.note(format_args!("split {n} off to {}", taken.history().id()));
        (taken, Some(rest))
    }

    /// One stack holding both, when they stack together and the total fits
    /// a count; otherwise both back unchanged.
    #[track_caller]
    pub fn merge(a: Self, mut b: Self) -> Result<Self, (Self, Self)> {
        match a.count.checked_add(b.count) {
            Some(total) if a.stacks_with(&b) => {
                let mut merged = a;
                merged.count = total;
                merged.note(format_args!("merge {} of {} in", b.count, b.history().id()));
                b.count = 0;
                Ok(merged)
            }
            _ => Err((a, b)),
        }
    }

    /// Takes up to `n` items off this stack. Taking all moves the stack out
    /// and leaves an empty husk (count zero, no nested stacks) for the
    /// caller to clear.
    #[track_caller]
    pub fn take(&mut self, n: u8) -> Self {
        let n = n.min(self.count);
        if n == self.count {
            let husk = Self::assemble(
                self.item,
                0,
                self.components.without_nested(),
                format_args!("husk of {}", self.history().id()),
            );
            let mut taken = std::mem::replace(self, husk);
            taken.note(format_args!("taken whole, leaving {}", self.history().id()));
            return taken;
        }
        self.count -= n;
        let taken = Self::assemble(
            self.item,
            n,
            self.split_components(),
            format_args!("take from {}", self.history().id()),
        );
        self.note(format_args!("gave {n} to {}", taken.history().id()));
        taken
    }

    /// Adds all of `other` when it stacks with this one and the total fits
    /// a count; otherwise hands it back.
    #[track_caller]
    pub fn join(&mut self, mut other: Self) -> Result<(), Self> {
        match self.count.checked_add(other.count) {
            Some(total) if self.stacks_with(&other) || other.count == 0 => {
                self.count = total;
                self.note(format_args!(
                    "join {} of {} in",
                    other.count,
                    other.history().id()
                ));
                other.count = 0;
                Ok(())
            }
            _ => Err(other),
        }
    }

    /// Moves up to `n` items of `from` onto this stack when they stack
    /// together, possibly leaving `from` an empty husk; returns how many
    /// moved.
    #[track_caller]
    pub fn transfer(&mut self, from: &mut Self, n: u8) -> u8 {
        if !self.stacks_with(from) {
            return 0;
        }
        let n = n.min(from.count).min(u8::MAX - self.count);
        from.count -= n;
        self.count += n;
        from.note(format_args!("transfer {n} to {}", self.history().id()));
        self.note(format_args!("transfer {n} from {}", from.history().id()));
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
    #[track_caller]
    pub fn absorb(&mut self, other: Self, limit: u8) -> Option<Self> {
        if !self.stacks_with(&other) {
            return Some(other);
        }
        let room = limit.saturating_sub(self.count);
        if room == 0 {
            return Some(other);
        }
        let (mut moved, rest) = other.split(room);
        self.count += moved.count;
        self.note(format_args!(
            "absorb {} of {}",
            moved.count,
            moved.history().id()
        ));
        moved.count = 0;
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
            (Some(mut back), Some(mut rest)) => {
                back.count += rest.count;
                rest.count = 0;
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
    #[track_caller]
    pub fn grow(&mut self, n: u8, reason: MintReason) {
        let before = self.count;
        self.count = self.count.saturating_add(n);
        if self.item != Item::Air {
            conservation::minted(u64::from(self.count - before));
        }
        self.note(format_args!("grow {} ({reason:?})", self.count - before));
    }

    /// Destroys up to `n` items of this stack, possibly leaving an empty
    /// husk.
    #[track_caller]
    pub fn shrink(&mut self, n: u8, reason: BurnReason) {
        if n > 0 {
            self.note(format_args!("shrink {} ({reason:?})", n.min(self.count)));
            self.take(n).burn(reason);
        }
    }

    /// Destroys up to `n` items of the stack in `slot`, emptying it when
    /// none are left; returns how many were destroyed.
    #[track_caller]
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
    #[track_caller]
    pub fn take_from(slot: &mut Option<Self>, n: u8) -> Option<Self> {
        if n == 0 {
            return None;
        }
        let (taken, rest) = slot.take()?.split(n);
        *slot = rest;
        Some(taken)
    }

    /// Puts `stack` back into `slot`, which it was taken from.
    #[track_caller]
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
    #[track_caller]
    pub fn keep(mut self, count: u8, reason: BurnReason) -> Self {
        let excess = self.count.saturating_sub(count);
        self.shrink(excess, reason);
        self
    }

    /// Changes the count to `count` by minting or burning the difference.
    #[track_caller]
    pub fn resize(&mut self, count: u8, mint: MintReason, burn: BurnReason) {
        match count.cmp(&self.count) {
            std::cmp::Ordering::Greater => self.grow(count - self.count, mint),
            std::cmp::Ordering::Less => {
                let n = self.count - count;
                self.count = count;
                if self.item != Item::Air {
                    conservation::burned(u64::from(n));
                }
                self.note(format_args!("resize down by {n} ({burn:?})"));
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

    fn shulker_holding(count: u8) -> ItemPicture {
        let mut shulker = ItemPicture::new(Item::ShulkerBox, 1);
        shulker.components.container = Some(vec![None, Some(ItemPicture::new(Item::Dirt, count))]);
        shulker
    }

    #[tuff_core::watchdog]
    #[test]
    fn counted_stacks_save_as_their_nbt() {
        let stack = ItemStack::mint(Item::Stone, 3, MintReason::Command);
        assert_eq!(stack.to_storage(), stack.to_nbt());
        stack.burn(BurnReason::Destroyed);
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
        a.burn(BurnReason::Test);
        b.burn(BurnReason::Test);
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
        let taken = ItemStack::take_from(&mut slot, 2).unwrap();
        assert_eq!(taken.count(), 2);
        assert_eq!(ItemStack::burn_from(&mut slot, 10, BurnReason::Test), 4);
        assert!(slot.is_none());
        let mut holder = stack(250);
        let refused = holder.join(stack(10)).unwrap_err();
        assert_eq!(holder.count(), 250);
        for stack in [target, taken, holder, refused] {
            stack.burn(BurnReason::Test);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn taking_a_whole_stack_moves_it_and_its_contents() {
        let mut shulker = ItemStack::mint_like(&shulker_holding(5), 1, MintReason::Test);
        let id = shulker.history().id();
        let taken = shulker.take(1);
        assert_eq!(taken.history().id(), id);
        assert_eq!(taken.nested_count(), 5);
        assert!(shulker.is_empty());
        assert_eq!(shulker.nested_count(), 0);
        taken.burn(BurnReason::Test);
    }

    /// Off the test's own thread, so the drop is checked as an actor's.
    #[tuff_core::watchdog]
    #[test]
    fn a_nested_stack_is_real_items() {
        let mut shulker = ItemStack::mint_like(&shulker_holding(5), 1, MintReason::Command);
        let slots = shulker.components.container.take().unwrap();
        shulker.burn(BurnReason::Destroyed);
        let before = conservation::totals();
        let _ = std::thread::spawn(move || drop(slots)).join();
        assert!(conservation::totals().since(before).lost >= 5);
    }

    /// The counters are process-wide and other tests run alongside, so
    /// each check is a lower bound.
    #[tuff_core::watchdog]
    #[test]
    fn doors_count_nested_stacks_once_per_stack() {
        let before = conservation::totals();
        let shulkers = ItemStack::mint_like(&shulker_holding(5), 2, MintReason::Test);
        let minted = conservation::totals().since(before).minted;
        assert!(minted >= 2 + 5, "{minted}");

        let before = conservation::totals();
        let (one, other) = shulkers.split(1);
        let other = other.unwrap();
        assert_eq!((one.nested_count(), other.nested_count()), (5, 5));
        assert!(conservation::totals().since(before).minted >= 5);

        let before = conservation::totals();
        let both = ItemStack::merge(one, other).unwrap();
        assert!(conservation::totals().since(before).burned >= 5);

        let before = conservation::totals();
        let picture = both.into_picture();
        assert!(conservation::totals().since(before).burned >= 2 + 5);
        assert_eq!(picture.nested_count(), 5);

        let before = conservation::totals();
        let loaded = ItemStack::from_storage(&picture.to_nbt()).unwrap();
        assert!(conservation::totals().since(before).loaded >= 2 + 5);
        assert_eq!(loaded, picture);
        loaded.burn(BurnReason::Test);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_picture_compares_equal_to_the_stack_it_shows() {
        let stack = ItemStack::mint_like(&shulker_holding(3), 1, MintReason::Test);
        let picture = ItemPicture::of(&stack);
        assert_eq!(picture, stack);
        assert!(stack.stacks_with(&picture));
        assert_ne!(picture.clone().with_count(2), stack);
        stack.burn(BurnReason::Test);
    }
}
