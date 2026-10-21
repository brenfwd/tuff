//! The slots of an entity's container as its menu numbers them. Most
//! entities keep them in one slice; a mount puts the saddle and body slots
//! it wears in front of its chest, without keeping a second copy of them.

use std::ops::{Index, IndexMut};

use tuff_item::item::ItemStack;

type Slot = Option<ItemStack>;

#[derive(Clone, Copy)]
pub struct ContainerSlots<'a> {
    head: [Option<&'a Slot>; 2],
    head_len: usize,
    rest: &'a [Slot],
}

impl<'a> ContainerSlots<'a> {
    pub fn new(rest: &'a [Slot]) -> Self {
        Self {
            head: [None, None],
            head_len: 0,
            rest,
        }
    }

    pub fn with_head(head: [&'a Slot; 2], rest: &'a [Slot]) -> Self {
        let [a, b] = head;
        Self {
            head: [Some(a), Some(b)],
            head_len: 2,
            rest,
        }
    }

    pub fn len(&self) -> usize {
        self.head_len + self.rest.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, slot: usize) -> Option<&'a Slot> {
        if slot < self.head_len {
            self.head[slot]
        } else {
            self.rest.get(slot - self.head_len)
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &'a Slot> + 'a {
        self.head.into_iter().flatten().chain(self.rest.iter())
    }

    /// The slots as one slice, when they are one.
    pub fn as_slice(&self) -> Option<&'a [Slot]> {
        (self.head_len == 0).then_some(self.rest)
    }
}

impl<'a> From<&'a [Slot]> for ContainerSlots<'a> {
    fn from(rest: &'a [Slot]) -> Self {
        Self::new(rest)
    }
}

impl Index<usize> for ContainerSlots<'_> {
    type Output = Slot;

    fn index(&self, slot: usize) -> &Slot {
        self.get(slot).expect("container slot out of range")
    }
}

pub struct ContainerSlotsMut<'a> {
    head: [Option<&'a mut Slot>; 2],
    head_len: usize,
    rest: &'a mut [Slot],
}

impl<'a> ContainerSlotsMut<'a> {
    pub fn new(rest: &'a mut [Slot]) -> Self {
        Self {
            head: [None, None],
            head_len: 0,
            rest,
        }
    }

    pub fn with_head(head: [&'a mut Slot; 2], rest: &'a mut [Slot]) -> Self {
        let [a, b] = head;
        Self {
            head: [Some(a), Some(b)],
            head_len: 2,
            rest,
        }
    }

    pub fn len(&self) -> usize {
        self.head_len + self.rest.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, slot: usize) -> Option<&Slot> {
        if slot < self.head_len {
            self.head[slot].as_deref()
        } else {
            self.rest.get(slot - self.head_len)
        }
    }

    pub fn get_mut(&mut self, slot: usize) -> Option<&mut Slot> {
        if slot < self.head_len {
            self.head[slot].as_deref_mut()
        } else {
            self.rest.get_mut(slot - self.head_len)
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Slot> {
        self.head
            .iter()
            .filter_map(Option::as_deref)
            .chain(self.rest.iter())
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Slot> {
        self.head
            .iter_mut()
            .filter_map(Option::as_deref_mut)
            .chain(self.rest.iter_mut())
    }

    /// The slots as one slice, when they are one.
    pub fn as_mut_slice(&mut self) -> Option<&mut [Slot]> {
        (self.head_len == 0).then_some(&mut *self.rest)
    }
}

impl<'a> From<&'a mut [Slot]> for ContainerSlotsMut<'a> {
    fn from(rest: &'a mut [Slot]) -> Self {
        Self::new(rest)
    }
}

impl Index<usize> for ContainerSlotsMut<'_> {
    type Output = Slot;

    fn index(&self, slot: usize) -> &Slot {
        self.get(slot).expect("container slot out of range")
    }
}

impl IndexMut<usize> for ContainerSlotsMut<'_> {
    fn index_mut(&mut self, slot: usize) -> &mut Slot {
        self.get_mut(slot).expect("container slot out of range")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::MintReason;
    use tuff_vanilla_data::Item;

    #[tuff_core::watchdog]
    #[test]
    fn a_head_comes_before_the_rest() {
        let mut saddle = Some(ItemStack::mint(Item::Saddle, 1, MintReason::Spawn));
        let mut body = None;
        let mut chest = vec![
            None,
            Some(ItemStack::mint(Item::Stone, 3, MintReason::Spawn)),
        ];
        let mut slots = ContainerSlotsMut::with_head([&mut saddle, &mut body], &mut chest);
        assert_eq!(slots.len(), 4);
        assert_eq!(slots[0].as_ref().map(|s| s.item), Some(Item::Saddle));
        assert_eq!(slots[3].as_ref().map(|s| s.count()), Some(3));
        slots[1] = slots[3].take();
        assert!(slots.as_mut_slice().is_none());
        assert_eq!(slots.iter().flatten().count(), 2);
        assert_eq!(body.map(|s| s.count()), Some(3));
        let view = ContainerSlots::with_head([&saddle, &None], &chest);
        assert_eq!(view.iter().flatten().count(), 1);
        assert!(view.get(4).is_none());
    }
}
