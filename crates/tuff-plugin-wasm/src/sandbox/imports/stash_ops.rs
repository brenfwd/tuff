use super::super::*;
use crate::stash::MAX_STASH_STACKS;
use crate::wit::bindings::types::ItemStack as StackPicture;
use tuff_core::world::session::plugin_inventory::TakenSink;
use tuff_item::item::{BurnReason, ItemStack, MintReason};

impl State {
    pub(in crate::sandbox) fn host_stash_new(&mut self) -> value::Result<Result<u64, WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(self.stashes.create().ok_or(WorldError::OverBudget))
    }

    pub(in crate::sandbox) fn host_stash_mint(
        &mut self,
        stash: u64,
        item: &str,
        count: u32,
        custom_data: Option<String>,
        custom_name: Option<String>,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            if !self.stashes.contains(stash) || !(1..=abi::MAX_GIVE_COUNT).contains(&count) {
                return Err(WorldError::InvalidState);
            }
            let template = state::item_template(item, custom_data, custom_name)?;
            let limit = u32::from(template.max_stack_size().max(1));
            if !self.stashes.has_room(stash, count.div_ceil(limit) as usize) {
                return Err(WorldError::OverBudget);
            }
            let mut left = count;
            let mut made = Vec::new();
            while left > 0 {
                let part = left.min(limit);
                made.push(ItemStack::mint_like(
                    &template,
                    part as u8,
                    MintReason::Plugin,
                ));
                left -= part;
            }
            self.stashes.fill(stash, made);
            Ok(())
        })())
    }

    pub(in crate::sandbox) fn host_stash_contents(
        &mut self,
        stash: u64,
    ) -> value::Result<Option<Vec<StackPicture>>> {
        Ok(self.stashes.get(stash).map(|held| {
            held.iter()
                .map(|stack| StackPicture {
                    item: stack.item().name().to_owned(),
                    count: stack.count(),
                })
                .collect()
        }))
    }

    pub(in crate::sandbox) fn host_stash_split(
        &mut self,
        stash: u64,
        item: Option<String>,
        count: u32,
    ) -> value::Result<Result<u64, WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            let item = match item {
                None => None,
                Some(name) => Some(
                    <tuff_vanilla_data::Item as tuff_vanilla_data::registry::Registry>::from_name(
                        &name,
                    )
                    .ok_or(WorldError::InvalidState)?,
                ),
            };
            if !self.stashes.contains(stash) {
                return Err(WorldError::InvalidState);
            }
            let split = self.stashes.create().ok_or(WorldError::OverBudget)?;
            let taken = self
                .stashes
                .split(stash, item, count)
                .ok_or(WorldError::InvalidState)?;
            self.stashes.fill(split, taken);
            Ok(split)
        })())
    }

    pub(in crate::sandbox) fn host_stash_merge(
        &mut self,
        target: u64,
        source: u64,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            if target == source || !self.stashes.contains(target) {
                return Err(WorldError::InvalidState);
            }
            let incoming = self
                .stashes
                .get(source)
                .ok_or(WorldError::InvalidState)?
                .len();
            if !self
                .stashes
                .has_room(target, incoming.min(MAX_STASH_STACKS))
            {
                return Err(WorldError::OverBudget);
            }
            self.stashes
                .merge(target, source)
                .ok_or(WorldError::InvalidState)
        })())
    }

    pub(in crate::sandbox) fn host_stash_burn(
        &mut self,
        stash: u64,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok(match self.stashes.remove(stash) {
            Some(held) => {
                for stack in held {
                    stack.burn(BurnReason::Plugin);
                }
                Ok(())
            }
            None => Err(WorldError::InvalidState),
        })
    }

    pub(in crate::sandbox) fn host_give_stash(
        &mut self,
        player: Uuid,
        stash: u64,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            if !self.stashes.contains(stash) {
                return Err(WorldError::InvalidState);
            }
            if self.effects.inventory_ops.len() >= abi::MAX_INVENTORY_OPS {
                return Err(WorldError::OverBudget);
            }
            let held = self.stashes.remove(stash).unwrap_or_default();
            self.queue_inventory(uuid_of(player), InventoryOp::GiveStacks(held))
        })())
    }

    pub(in crate::sandbox) fn host_take_into_stash(
        &mut self,
        player: Uuid,
        item: &str,
        count: u32,
        all_or_nothing: bool,
        stash: u64,
    ) -> value::Result<Result<(), WorldError>> {
        if !self.group_granted(Kind::PlayerWrite) {
            return Ok(Err(WorldError::NotGranted));
        }
        Ok((|| {
            if !(1..=abi::MAX_GIVE_COUNT).contains(&count)
                || !self.stashes.contains(stash)
                || self.stashes.quarantined()
            {
                return Err(WorldError::InvalidState);
            }
            let item =
                <tuff_vanilla_data::Item as tuff_vanilla_data::registry::Registry>::from_name(item)
                    .filter(|item| *item != tuff_vanilla_data::Item::Air)
                    .ok_or(WorldError::InvalidState)?;
            self.queue_inventory(
                uuid_of(player),
                InventoryOp::TakeInto {
                    item,
                    count,
                    all_or_nothing,
                    sink: TakenSink::new(stash),
                },
            )
        })())
    }
}
