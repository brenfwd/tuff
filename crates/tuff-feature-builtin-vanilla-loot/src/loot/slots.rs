use std::collections::BTreeSet;

use serde_json::Value;
use tuff_core::world::predicate::slot_range;
use tuff_datapack::slot_source::{SlotSource, SlotSourceRef};
use tuff_item::item::{ItemPicture, ItemStack, MintReason};

use super::{Context, functions, match_tool};

const MAX_DEPTH: usize = 16;

/// The stacks a `minecraft:slots` entry's source selects: container slots
/// of the broken block's block entity, minted as loot. Entity inventories
/// are not part of the loot context, so sources on entities select
/// nothing.
pub(super) fn provide(raw: &str, context: &Context) -> Vec<ItemStack> {
    let Ok(entry) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    let Some(source) = entry.get("slot_source") else {
        return Vec::new();
    };
    let reference = match source {
        Value::String(id) => SlotSourceRef::Id(normalize(id)),
        inline => {
            match tuff_datapack::slot_source::parse(inline, 0, &|name| slot_range(name).is_some()) {
                Ok(parsed) => SlotSourceRef::Inline(parsed),
                Err(_) => return Vec::new(),
            }
        }
    };
    slots(&reference, context, 0)
        .into_iter()
        .filter(|(_, picture)| picture.count() > 0)
        .map(|(_, picture)| ItemStack::mint_like(&picture, picture.count(), MintReason::Loot))
        .collect()
}

fn normalize(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

fn slots(reference: &SlotSourceRef, context: &Context, depth: usize) -> Vec<(i32, ItemPicture)> {
    if depth > MAX_DEPTH {
        return Vec::new();
    }
    match reference {
        SlotSourceRef::Inline(source) => evaluate(source, context, depth),
        SlotSourceRef::Id(id) => context
            .tables
            .slot_source(id)
            .map(|source| evaluate(&source, context, depth))
            .unwrap_or_default(),
    }
}

fn evaluate(source: &SlotSource, context: &Context, depth: usize) -> Vec<(i32, ItemPicture)> {
    match source {
        SlotSource::Empty => Vec::new(),
        SlotSource::Group(terms) => terms
            .iter()
            .flat_map(|term| slots(term, context, depth + 1))
            .collect(),
        SlotSource::Filtered {
            source,
            item_filter,
        } => slots(source, context, depth + 1)
            .into_iter()
            .filter(|(_, stack)| match_tool(item_filter, Some(stack)))
            .collect(),
        SlotSource::Limit { source, limit } => slots(source, context, depth + 1)
            .into_iter()
            .take(*limit as usize)
            .collect(),
        SlotSource::Range { source, slots } => {
            if !matches!(source.as_str(), "block_entity" | "container") {
                return Vec::new();
            }
            let Some(wanted) = slot_range(slots) else {
                return Vec::new();
            };
            let wanted: BTreeSet<i32> = wanted.into_iter().collect();
            let Some(items) = context
                .block_entity
                .and_then(|nbt| nbt.get("Items"))
                .and_then(tuff_nbt::Tag::as_list)
            else {
                return Vec::new();
            };
            let mut found: Vec<(i32, ItemPicture)> = items
                .iter()
                .filter_map(|tag| {
                    let entry = tag.as_compound()?;
                    let slot = i32::from(entry.get("Slot")?.as_byte()?);
                    if !wanted.contains(&slot) {
                        return None;
                    }
                    Some((slot, ItemPicture::from_nbt(entry)?))
                })
                .collect();
            found.sort_by_key(|(slot, _)| *slot);
            found
        }
        SlotSource::Contents { source, component } => slots(source, context, depth + 1)
            .into_iter()
            .flat_map(|(_, mut picture)| functions::take_contents(&mut picture, component))
            .enumerate()
            .map(|(slot, stack)| (slot as i32, stack))
            .collect(),
    }
}
