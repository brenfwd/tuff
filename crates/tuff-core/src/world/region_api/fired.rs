use tuff_item::item::ItemStack;

/// The item a dispenser fires: a picture of its slot to read (through
/// `Deref`), and one item moved out of the slot that a hook keeps with
/// [`Self::take`] (an equipped piece, a launched arrow). Whatever no hook
/// took goes back to the slot.
pub struct Fired {
    shown: tuff_item::item::ItemSnapshot,
    one: std::cell::RefCell<Option<ItemStack>>,
    offered: bool,
}

impl Fired {
    pub fn new(shown: tuff_item::item::ItemSnapshot, one: Option<ItemStack>) -> Self {
        Self {
            shown,
            offered: one.is_some(),
            one: std::cell::RefCell::new(one),
        }
    }

    /// Whether a hook may keep a dispensed `stack`, so the dispenser
    /// moves one out for it: projectiles, wearables and what a sulfur cube
    /// swallows. Items used in place (shears, buckets) stay in the slot.
    pub fn keepable(stack: &ItemStack) -> bool {
        stack.item().dispense() != Some(tuff_vanilla_data::redstone::Dispense::Special)
            || stack
                .equippable()
                .is_some_and(|equippable| equippable.dispensable)
            || tuff_datapack::tags::is(
                tuff_vanilla_data::tag::item::key::SULFUR_CUBE_SWALLOWABLE,
                stack.item(),
            )
    }

    /// Moves the fired item out; `None` if a hook already took it or the
    /// dispenser could not hand it over.
    pub fn take(&self) -> Option<ItemStack> {
        self.one.borrow_mut().take()
    }

    /// Puts back an item [`Self::take`] gave out that was not kept after all.
    pub fn give_back(&self, stack: ItemStack) {
        ItemStack::put_back(&mut self.one.borrow_mut(), stack);
    }

    /// Whether a hook kept the item.
    pub fn taken(&self) -> bool {
        self.offered && self.one.borrow().is_none()
    }

    /// The picture and the item no hook kept.
    pub fn into_parts(self) -> (tuff_item::item::ItemSnapshot, Option<ItemStack>) {
        (self.shown, self.one.into_inner())
    }
}

impl std::ops::Deref for Fired {
    type Target = ItemStack;

    fn deref(&self) -> &ItemStack {
        &self.shown
    }
}
