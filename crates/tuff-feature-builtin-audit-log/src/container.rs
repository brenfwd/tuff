//! Container rows: slot changes netted into per-item additions and
//! removals, and undoing them on a container's current contents.

use tuff_core::world::region_api::{BlockCause, ContainerHolder, SlotEdit};
use tuff_item::item::BurnReason;
use tuff_item::item::{ItemSnapshot, ItemStack, MintReason};
use tuff_nbt::Compound;

use crate::record::{Kind, Record, Who};

/// How many holders a region nets at once before it writes them out early.
pub const MAX_PENDING: usize = 1024;

/// One item's net change in a container: `stack` is one of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Net {
    pub stack: ItemSnapshot,
    pub delta: i64,
    /// The last slot it went into or came out of.
    pub slot: usize,
}

/// A container's changes by one cause, netted until written.
#[derive(Clone, Debug)]
pub struct Pending {
    pub holder: ContainerHolder,
    pub cause: BlockCause,
    pub who: Who,
    pub items: Vec<Net>,
}

impl Pending {
    pub fn new(holder: ContainerHolder, cause: BlockCause, who: Who) -> Pending {
        Pending {
            holder,
            cause,
            who,
            items: Vec::new(),
        }
    }

    fn add(&mut self, stack: &ItemStack, delta: i64, slot: usize) {
        let one = ItemSnapshot::of(stack).with_count(1);
        match self.items.iter_mut().find(|net| net.stack == one) {
            Some(net) => {
                net.delta += delta;
                net.slot = slot;
            }
            None => self.items.push(Net {
                stack: one,
                delta,
                slot,
            }),
        }
    }

    /// Nets `slots` into what this container gained and lost.
    pub fn apply(&mut self, slots: &[SlotEdit]) {
        for edit in slots {
            if let Some(old) = &edit.old {
                self.add(old, -i64::from(old.count()), edit.slot);
            }
            if let Some(new) = &edit.new {
                self.add(new, i64::from(new.count()), edit.slot);
            }
        }
    }

    /// The rows these changes make, in `world` at `time`.
    pub fn records(
        self,
        time: i64,
        world: tuff_core::world::dimension::DimensionId,
    ) -> impl Iterator<Item = Record> {
        let Pending {
            holder, who, items, ..
        } = self;
        let pos = holder.pos();
        items
            .into_iter()
            .filter(|net| net.delta != 0)
            .map(move |net| {
                let kind = if net.delta > 0 {
                    Kind::ContainerAdd
                } else {
                    Kind::ContainerRemove
                };
                let mut record = Record::new(time, who.clone(), kind, world, [pos.x, pos.y, pos.z]);
                record.material = Some(net.stack.item().name().to_owned());
                record.amount = net.delta.unsigned_abs().min(i32::MAX as u64) as i32;
                record.text = Some(encode_text(&holder, net.slot));
                record.data = Some(net.stack.to_nbt());
                record
            })
    }
}

/// Where a row's items were: the slot, and for an entity its type and uuid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub slot: Option<usize>,
    pub entity: Option<(String, u128)>,
}

/// `slot=3`, or `slot=3 entity=minecraft:chest_boat:<uuid hex>`.
pub fn encode_text(holder: &ContainerHolder, slot: usize) -> String {
    match holder {
        ContainerHolder::Block(_) => format!("slot={slot}"),
        ContainerHolder::Entity { uuid, kind, .. } => {
            format!(
                "slot={slot} entity={}:{uuid:032x}",
                tuff_vanilla_data::Registry::name(*kind)
            )
        }
    }
}

pub fn parse_text(text: &str) -> Place {
    let mut place = Place {
        slot: None,
        entity: None,
    };
    for word in text.split_whitespace() {
        if let Some(slot) = word.strip_prefix("slot=") {
            place.slot = slot.parse().ok();
        } else if let Some(entity) = word.strip_prefix("entity=")
            && let Some((kind, uuid)) = entity.rsplit_once(':')
            && let Ok(uuid) = u128::from_str_radix(uuid, 16)
        {
            place.entity = Some((kind.to_owned(), uuid));
        }
    }
    place
}

/// One container row to undo: put `amount` of `stack` back in, or take it
/// out where it still is.
#[derive(Clone, Debug)]
pub struct Undo {
    pub id: i64,
    pub entity: Option<u128>,
    pub stack: ItemSnapshot,
    /// The stack's components are unknown: match by item alone.
    pub any_components: bool,
    pub amount: i64,
    pub add: bool,
    pub slot: Option<usize>,
    /// A strict rollback: the player to take the items from first.
    pub taker: Option<u128>,
}

fn same(stack: &ItemStack, undo: &Undo) -> bool {
    if undo.any_components {
        stack.item() == undo.stack.item()
    } else {
        stack.stacks_with(&undo.stack)
    }
}

/// Undoes `undo` on `items`, returning how many items moved: an addition
/// tops up matching stacks (the row's slot first) and then fills empty
/// slots; a removal takes from the row's slot first and then from the
/// last matching slot back.
pub fn apply(items: &mut [Option<ItemStack>], undo: &Undo) -> i64 {
    let hint = undo.slot.filter(|slot| *slot < items.len());
    let order: Vec<usize> = hint
        .into_iter()
        .chain((0..items.len()).filter(|slot| Some(*slot) != hint))
        .collect();
    let mut left = undo.amount;
    if undo.add {
        let max = i64::from(undo.stack.max_stack_size());
        for empty_pass in [false, true] {
            for &slot in &order {
                if left == 0 {
                    break;
                }
                let room = match &items[slot] {
                    Some(stack) if !empty_pass && same(stack, undo) => {
                        max - i64::from(stack.count())
                    }
                    None if empty_pass => max,
                    _ => continue,
                };
                let moved = room.min(left);
                if moved <= 0 {
                    continue;
                }
                left -= moved;
                match &mut items[slot] {
                    Some(stack) => stack.grow(moved as u8, MintReason::Command),
                    empty => {
                        *empty = Some(ItemStack::mint_like(
                            &undo.stack,
                            moved as u8,
                            MintReason::Command,
                        ))
                    }
                }
            }
        }
    } else {
        let reversed: Vec<usize> = hint
            .into_iter()
            .chain((0..items.len()).rev().filter(|slot| Some(*slot) != hint))
            .collect();
        for slot in reversed {
            if left == 0 {
                break;
            }
            let Some(stack) = items[slot].as_mut().filter(|stack| same(stack, undo)) else {
                continue;
            };
            let moved = i64::from(stack.count()).min(left);
            left -= moved;
            stack.shrink(moved as u8, BurnReason::Command);
            if stack.count() == 0 {
                items[slot] = None;
            }
        }
    }
    undo.amount - left
}

/// The stack a row's item data names.
pub fn stack_of(data: Option<&Compound>) -> Option<ItemSnapshot> {
    let stack = ItemStack::mint_from_nbt(data?, MintReason::Template)?;
    let one = ItemSnapshot::of(&stack).with_count(1);
    stack.burn(BurnReason::Template);
    Some(one)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Item;
    use tuff_world_model::BlockPos;

    fn stack(item: Item, count: u8) -> Option<ItemStack> {
        Some(ItemStack::mint(item, count, MintReason::Test))
    }

    fn undo(item: Item, amount: i64, add: bool, slot: Option<usize>) -> Undo {
        Undo {
            id: 1,
            entity: None,
            stack: ItemSnapshot::describe(item, 1, Default::default()),
            any_components: false,
            amount,
            add,
            slot,
            taker: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn slot_edits_net_per_item() {
        let mut pending = Pending::new(
            ContainerHolder::Block(BlockPos::new(1, 2, 3)),
            BlockCause::World,
            Who::cause("#hopper"),
        );
        pending.apply(&[
            SlotEdit {
                slot: 0,
                old: stack(Item::Diamond, 10)
                    .as_ref()
                    .map(tuff_item::item::ItemSnapshot::of),
                new: None,
            },
            SlotEdit {
                slot: 4,
                old: None,
                new: stack(Item::Diamond, 10)
                    .as_ref()
                    .map(tuff_item::item::ItemSnapshot::of),
            },
            SlotEdit {
                slot: 1,
                old: stack(Item::Stone, 3)
                    .as_ref()
                    .map(tuff_item::item::ItemSnapshot::of),
                new: stack(Item::Stone, 1)
                    .as_ref()
                    .map(tuff_item::item::ItemSnapshot::of),
            },
            SlotEdit {
                slot: 2,
                old: None,
                new: stack(Item::Dirt, 5)
                    .as_ref()
                    .map(tuff_item::item::ItemSnapshot::of),
            },
        ]);
        let records: Vec<Record> = pending
            .records(7, tuff_core::world::dimension::DimensionId::default())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].kind, Kind::ContainerRemove);
        assert_eq!(records[0].material.as_deref(), Some("minecraft:stone"));
        assert_eq!(records[0].amount, 2);
        assert_eq!(records[0].text.as_deref(), Some("slot=1"));
        assert_eq!(records[0].pos, [1, 2, 3]);
        assert_eq!(records[1].kind, Kind::ContainerAdd);
        assert_eq!(records[1].amount, 5);
        assert_eq!(
            stack_of(records[1].data.as_ref()),
            Some(tuff_item::item::ItemSnapshot::describe(
                Item::Dirt,
                1,
                Default::default()
            ))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn entity_places_round_trip() {
        let holder = ContainerHolder::Entity {
            uuid: 0xabc,
            kind: tuff_vanilla_data::registry::EntityType::ChestMinecart,
            pos: BlockPos::new(0, 0, 0),
        };
        let place = parse_text(&encode_text(&holder, 12));
        assert_eq!(place.slot, Some(12));
        assert_eq!(
            place.entity,
            Some(("minecraft:chest_minecart".to_owned(), 0xabc))
        );
        assert_eq!(
            parse_text("slot=x"),
            Place {
                slot: None,
                entity: None
            }
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn undoing_adds_back_and_takes_out_what_is_still_there() {
        let mut items = vec![stack(Item::Coal, 60), None, stack(Item::Stone, 1), None];
        assert_eq!(apply(&mut items, &undo(Item::Coal, 10, true, Some(3))), 10);
        assert_eq!(items[0], stack(Item::Coal, 64));
        assert_eq!(items[3], stack(Item::Coal, 6));
        assert_eq!(apply(&mut items, &undo(Item::Stone, 5, false, None)), 1);
        assert_eq!(items[2], None);
        assert_eq!(apply(&mut items, &undo(Item::Coal, 8, false, Some(3))), 8);
        assert_eq!(items[3], None);
        assert_eq!(items[0], stack(Item::Coal, 62));
        let mut full = vec![stack(Item::Dirt, 64)];
        assert_eq!(apply(&mut full, &undo(Item::Coal, 3, true, None)), 0);
    }
}
