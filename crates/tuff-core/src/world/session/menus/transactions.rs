use super::*;

impl PlayerSession {
    /// Sends the block container slots changed since `container_before` to
    /// the region as one transaction, remembering the inventory as of
    /// `inventory_before` to restore on rejection. Without block changes
    /// the `drops` are thrown right away.
    pub async fn submit_container_changes(
        &mut self,
        container_before: Vec<Option<ItemStack>>,
        inventory_before: Inventory,
        drops: Vec<ItemStack>,
        took_result: bool,
    ) {
        if let Some(MenuBacking::Entity(entity)) = self.menus.menu.as_ref().map(|m| &m.backing) {
            let entity = *entity;
            self.submit_entity_changes(entity, container_before, inventory_before, drops)
                .await;
            return;
        }
        let changes = self.menus.container_changes(&container_before);
        if changes.is_empty() && !took_result {
            for stack in drops {
                self.throw_item(stack).await;
            }
            return;
        }
        let inventory: Vec<(usize, Option<ItemStack>)> = (0..inventory_before.slots().len())
            .filter(|slot| inventory_before.slots()[*slot] != self.inventory.slots()[*slot])
            .map(|slot| (slot, inventory_before.slots()[slot].clone()))
            .collect();
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        menu.pending = Some(Pending {
            inventory,
            carried: inventory_before.carried().cloned(),
            drops,
            merchant: None,
        });
        let window_id = menu.window_id;
        let (far, near): (Vec<ContainerChange>, Vec<ContainerChange>) = match &menu.remote {
            Some((pos, _)) => changes.into_iter().partition(|c| c.pos == *pos),
            None => (Vec::new(), changes),
        };
        let region = menu.region.clone();
        let Some((_, remote)) = menu.remote.clone().filter(|_| !far.is_empty()) else {
            region
                .request(RegionRequest::ContainerTransaction {
                    player: self.conn.handle.clone(),
                    window_id,
                    changes: near,
                    took_result,
                })
                .await;
            return;
        };
        if near.is_empty() && !took_result {
            remote
                .request(RegionRequest::ContainerTransaction {
                    player: self.conn.handle.clone(),
                    window_id,
                    changes: far,
                    took_result,
                })
                .await;
            return;
        }
        self.menus.txn_counter += 1;
        let txn = self.menus.txn_counter;
        menu.split = Some(SplitTransaction {
            txn,
            votes: 0,
            prepared: true,
            contents: Vec::new(),
        });
        for (region, changes) in [(region, near), (remote, far)] {
            region
                .request(RegionRequest::PrepareTransaction {
                    player: self.conn.handle.clone(),
                    window_id,
                    txn,
                    changes,
                })
                .await;
        }
    }

    /// Counts one region's vote on a two-region transaction; once both
    /// voted, commits it everywhere if both prepared, else aborts it.
    pub async fn transaction_prepared(
        &mut self,
        window_id: i32,
        txn: u64,
        prepared: bool,
        contents: Vec<(BlockPos, Vec<Option<ItemStack>>)>,
    ) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        let Some(split) = menu
            .split
            .as_mut()
            .filter(|split| menu.window_id == window_id && split.txn == txn)
        else {
            return;
        };
        split.votes += 1;
        split.prepared &= prepared;
        split.contents.extend(contents);
        if split.votes < 2 {
            return;
        }
        let split = menu.split.take().expect("checked above");
        let regions = [
            Some(menu.region.clone()),
            menu.remote.as_ref().map(|(_, region)| region.clone()),
        ];
        for region in regions.into_iter().flatten() {
            region
                .request(RegionRequest::ResolveTransaction {
                    player: self.id,
                    txn,
                    commit: split.prepared,
                })
                .await;
        }
        let contents = if split.prepared {
            Vec::new()
        } else {
            split.contents
        };
        self.transaction_answered(window_id, split.prepared, contents)
            .await;
    }

    /// Tells the region a menu is no longer viewed.
    pub(super) async fn release_menu(
        &self,
        region: &RegionHandle,
        remote: Option<&(BlockPos, RegionHandle)>,
        mut positions: Vec<BlockPos>,
        entity: Option<i32>,
    ) {
        if let Some((pos, far)) = remote {
            positions.retain(|p| p != pos);
            far.request(RegionRequest::CloseContainer {
                player: self.id,
                positions: vec![*pos],
            })
            .await;
        }
        let request = match entity {
            Some(entity) => RegionRequest::CloseEntityContainer {
                player: self.id,
                entity,
            },
            None => RegionRequest::CloseContainer {
                player: self.id,
                positions,
            },
        };
        region.request(request).await;
    }

    /// Sends the entity container slots changed since `container_before`
    /// as one transaction, like [`Self::submit_container_changes`].
    pub(super) async fn submit_entity_changes(
        &mut self,
        entity: i32,
        container_before: Vec<Option<ItemStack>>,
        inventory_before: Inventory,
        drops: Vec<ItemStack>,
    ) {
        let Some(menu) = self.menus.menu.as_ref() else {
            return;
        };
        let changes: Vec<EntitySlotChange> = container_before
            .iter()
            .zip(&menu.container)
            .enumerate()
            .filter(|(_, (before, after))| before != after)
            .map(|(slot, (before, after))| EntitySlotChange {
                slot,
                expected: before.clone(),
                new: after.clone(),
            })
            .collect();
        if changes.is_empty() {
            for stack in drops {
                self.throw_item(stack).await;
            }
            return;
        }
        let inventory: Vec<(usize, Option<ItemStack>)> = (0..inventory_before.slots().len())
            .filter(|slot| inventory_before.slots()[*slot] != self.inventory.slots()[*slot])
            .map(|slot| (slot, inventory_before.slots()[slot].clone()))
            .collect();
        let menu = self.menus.menu.as_mut().expect("checked above");
        menu.pending = Some(Pending {
            inventory,
            carried: inventory_before.carried().cloned(),
            drops,
            merchant: None,
        });
        let region = menu.region.clone();
        let window_id = menu.window_id;
        region
            .request(RegionRequest::EntityTransaction {
                player: self.conn.handle.clone(),
                window_id,
                entity,
                changes,
            })
            .await;
    }

    pub async fn entity_transaction_answered(
        &mut self,
        window_id: i32,
        accepted: bool,
        contents: Vec<Option<ItemStack>>,
    ) {
        if let Some(menu) = self.menus.menu.as_mut()
            && menu.window_id == window_id
            && menu.pending.is_some()
            && !accepted
            && contents.len() == menu.container.len()
        {
            menu.container = contents;
        }
        self.transaction_answered(window_id, accepted, Vec::new())
            .await;
    }

    pub fn entity_slots_changed(&mut self, entity: i32, slots: Vec<(usize, Option<ItemStack>)>) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        if menu.backing != MenuBacking::Entity(entity) {
            return;
        }
        let window_id = menu.window_id;
        let mut updates = Vec::new();
        for (slot, item) in slots {
            if slot < menu.container.len() {
                menu.container[slot] = item.clone();
                updates.push((menu.next_state_id(), menu.kind.wire_slot(slot) as i16, item));
            }
        }
        for (state_id, slot, item) in updates {
            self.send(&play::ContainerSetSlot {
                window_id,
                state_id,
                slot,
                item: item.as_ref().map(ItemStack::view),
            });
        }
    }

    pub async fn transaction_answered(
        &mut self,
        window_id: i32,
        accepted: bool,
        contents: Vec<(BlockPos, Vec<Option<ItemStack>>)>,
    ) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        if menu.window_id != window_id {
            return;
        }
        let Some(pending) = menu.pending.take() else {
            return;
        };
        if accepted {
            for stack in pending.drops {
                self.throw_item(stack).await;
            }
        } else {
            for (slot, value) in pending.inventory {
                self.inventory.set_slot(slot, value);
            }
            self.inventory.set_carried(pending.carried);
            let menu = self.menus.menu.as_mut().unwrap();
            if let Some((container, offers)) = pending.merchant {
                menu.container = container;
                if let Some(Station::Merchant { view, .. }) = &mut menu.station {
                    view.offers = offers;
                }
                self.send_merchant_offers();
            }
            let menu = self.menus.menu.as_mut().unwrap();
            for (pos, items) in contents {
                if let Some(offset) = menu.backing.offset(pos) {
                    for (slot, item) in items.into_iter().enumerate() {
                        if let Some(entry) = menu.container.get_mut(offset + slot) {
                            *entry = item;
                        }
                    }
                }
            }
            menu.queued.clear();
            menu.drag = None;
            self.send_inventory();
            self.send_menu_content();
        }
        if self
            .menus
            .menu
            .as_ref()
            .is_some_and(|m| m.closing.is_some())
        {
            self.finish_menu_close().await;
            return;
        }
        while let Some(click) = self.menus.menu.as_mut().and_then(|m| m.queued.pop_front()) {
            self.apply_menu_click(click).await;
            if self.menus.menu.as_ref().is_none_or(|m| m.pending.is_some()) {
                break;
            }
        }
    }

    pub fn container_slots_changed(
        &mut self,
        pos: BlockPos,
        slots: Vec<(usize, Option<ItemStack>)>,
    ) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        let Some(offset) = menu.backing.offset(pos) else {
            return;
        };
        let window_id = menu.window_id;
        let mut updates = Vec::new();
        for (slot, item) in slots {
            let index = offset + slot;
            if index < menu.container.len() {
                menu.container[index] = item.clone();
                updates.push((
                    menu.next_state_id(),
                    menu.kind.wire_slot(index) as i16,
                    item,
                ));
            }
        }
        for (state_id, slot, item) in updates {
            self.send(&play::ContainerSetSlot {
                window_id,
                state_id,
                slot,
                item: item.as_ref().map(ItemStack::view),
            });
        }
    }

    pub fn container_data_changed(&mut self, pos: BlockPos, data: Vec<(usize, i32)>) {
        let Some(menu) = &self.menus.menu else {
            return;
        };
        if menu.backing.offset(pos) != Some(0) {
            return;
        }
        for (property, value) in data {
            self.send_menu_data(property as i16, value as i16);
        }
    }
}

impl super::MenuState {
    /// The block container slots the open menu changed since `before`.
    pub fn container_changes(&self, before: &[Option<ItemStack>]) -> Vec<ContainerChange> {
        let Some(menu) = &self.menu else {
            return Vec::new();
        };
        let MenuBacking::Blocks(parts) = &menu.backing else {
            return Vec::new();
        };
        let mut changes = Vec::new();
        let mut offset = 0;
        for (pos, size) in parts {
            for slot in 0..*size {
                let index = offset + slot;
                if before[index] != menu.container[index] {
                    changes.push(ContainerChange {
                        pos: *pos,
                        slot,
                        expected: before[index].clone(),
                        new: menu.container[index].clone(),
                    });
                }
            }
            offset += size;
        }
        changes
    }
}
