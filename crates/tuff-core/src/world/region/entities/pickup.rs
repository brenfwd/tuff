use super::*;
use crate::world::conservation::Transfer;

impl RegionActor {
    /// Drops `amount` experience at `position` as orbs, merging into orbs
    /// already there the way the game does.
    pub(in crate::world::region) fn spawn_experience(&mut self, position: [f64; 3], amount: i32) {
        for value in experience::split(amount) {
            let group = self.rng.gen_range(0..experience::MERGE_GROUPS);
            let nearby = [
                position[0] - 0.5,
                position[1] - 0.5,
                position[2] - 0.5,
                position[0] + 0.5,
                position[1] + 0.5,
                position[2] + 0.5,
            ];
            let existing = self.ents.entities.values_mut().find_map(|tracked| {
                let bounds = tracked.entity.bounds();
                let orb = tracked.entity.orb_mut()?;
                (orb.merges_with(group, value) && experience::intersects(bounds, nearby))
                    .then_some(orb)
            });
            if let Some(orb) = existing {
                orb.absorb(1, 0);
                continue;
            }
            let nbt = experience::spawn_nbt(position, value, &mut self.rng);
            let id = self.shared.next_entity_id();
            match self.shared.entity_types.load(&nbt, id) {
                Some(orb) => self.receive_entity(orb, 0),
                None => {
                    log::warn!(
                        "experience dropped: no feature implements minecraft:experience_orb"
                    );
                    return;
                }
            }
        }
    }

    /// Every twentieth tick an orb absorbs nearby orbs of its value and id
    /// group.
    pub(super) fn merge_orbs(&mut self, tracked: &mut TrackedEntity) {
        let Some(orb) = tracked.entity.orb() else {
            return;
        };
        if !orb.merge_due() {
            return;
        }
        let mut reach = tracked.entity.bounds();
        for axis in 0..3 {
            reach[axis] -= experience::MERGE_DISTANCE;
            reach[axis + 3] += experience::MERGE_DISTANCE;
        }
        let (id, value) = (tracked.entity.base().id, orb.value());
        let merged: Vec<i32> = self
            .ents
            .entities
            .values()
            .filter(|other| {
                other.entity.orb().is_some_and(|other_orb| {
                    other_orb.merges_with(id, value)
                        && experience::intersects(other.entity.bounds(), reach)
                })
            })
            .map(|other| other.entity.base().id)
            .collect();
        for other in merged {
            if let Some(gone) = self.remove_entity(other, &[])
                && let Some(gone) = gone.entity.orb()
                && let Some(orb) = tracked.entity.orb_mut()
            {
                orb.absorb(gone.count(), gone.age());
            }
        }
    }

    /// Gives an orb to a player touching it, at most one orb per player
    /// every two ticks.
    pub(super) fn collect_orb(&mut self, id: i32) {
        let tick = self.sched.tick;
        let Some(orb) = self
            .ents
            .entities
            .get(&id)
            .and_then(|tracked| Some((tracked.entity.orb()?, tracked.entity.base().chunk())))
        else {
            return;
        };
        let (orb, chunk) = orb;
        let value = orb.value();
        let collector = self
            .ents
            .presence
            .values()
            .find(|p| {
                p.is_physical()
                    && orb.touches(p.position)
                    && self
                        .ents
                        .orb_pickups
                        .get(&p.handle.id)
                        .is_none_or(|last| tick - last >= experience::PICKUP_DELAY)
            })
            .map(|p| (p.handle.id, p.handle.clone()));
        let Some((player, handle)) = collector else {
            return;
        };
        self.ents.orb_pickups.insert(player, tick);
        let take = packets([framed(&play::TakeItemEntity {
            collected: id,
            collector: player,
            count: 1,
        })]);
        self.fanout_chunk(chunk, None, |_| PlayerEvent::EntityPackets {
            entity: id,
            packets: take.clone(),
        });
        handle.send(PlayerEvent::Experience { amount: value });
        let left = self
            .ents
            .entities
            .get_mut(&id)
            .and_then(|tracked| tracked.entity.orb_mut())
            .is_none_or(|orb| orb.take_one());
        if !left {
            self.remove_entity(id, &[]);
        }
    }

    pub(super) fn entity_mut<T: RegionEntity>(&mut self, id: i32) -> Option<&mut T> {
        self.ents.entities.get_mut(&id)?.entity.downcast_mut::<T>()
    }

    /// Offers every item whose pickup delay is over to a player close
    /// enough, holding it for that player until they answer.
    pub(in crate::world::region) fn offer_items(&mut self) {
        let _span = tuff_timings::span!("region.offer_items");
        if self.quarantined() {
            return;
        }
        self.offer_ground_pickups();
        for (id, tracked) in &mut self.ents.entities {
            let Some(item) = tracked.entity.downcast_mut::<ItemEntity>() else {
                continue;
            };
            if item.pickup_delay > 0 || reservation_live(item.reserved, self.sched.tick) {
                continue;
            }
            let owner = item.owner();
            let in_range: Vec<_> = self
                .ents
                .presence
                .values()
                .filter(|p| {
                    p.is_physical()
                        && item.in_pickup_range(p.position)
                        && owner.is_none_or(|uuid| uuid == p.uuid)
                })
                .collect();
            if let Some(collector) = pick_collector(in_range.into_iter(), &mut item.refused_by) {
                let offer = crate::world::entity::next_offer_id();
                item.reserved = Some(Reservation {
                    player: collector.handle.id,
                    at: self.sched.tick,
                    offer,
                });
                collector.handle.send(PlayerEvent::OfferItem {
                    item: *id,
                    offer,
                    stack: tuff_item::item::ItemSnapshot::of(&item.stack),
                    region: self.handle.clone(),
                });
            }
        }
    }

    /// Offers entities lying on the ground (stuck arrows) to players
    /// touching them; a creative player takes one only creative players
    /// may take.
    pub(super) fn offer_ground_pickups(&mut self) {
        let mut taken = Vec::new();
        for (id, tracked) in &mut self.ents.entities {
            let Some(pickup) = tracked.entity.ground_pickup() else {
                continue;
            };
            if reservation_live(*pickup.reserved, self.sched.tick) {
                continue;
            }
            let bounds = pickup.bounds;
            let only = pickup.only;
            let touching = |p: &&crate::world::region::Presence| {
                if only.is_some_and(|uuid| uuid != p.uuid) {
                    return false;
                }
                let [x, y, z] = p.position;
                let reach = [
                    x - 1.3,
                    y - 0.5,
                    z - 1.3,
                    x + 1.3,
                    y + p.looks.height + 0.5,
                    z + 1.3,
                ];
                p.is_physical() && crate::world::contact::boxes_intersect(reach, bounds)
            };
            if pickup.creative_only {
                if let Some(p) = self
                    .ents
                    .presence
                    .values()
                    .find(|p| touching(p) && p.looks.creative)
                {
                    taken.push((*id, p.handle.id));
                }
                continue;
            }
            let Some(stack) = pickup
                .stack
                .as_deref()
                .map(tuff_item::item::ItemSnapshot::of)
            else {
                continue;
            };
            let in_range = self.ents.presence.values().filter(touching);
            if let Some(collector) = pick_collector(in_range, pickup.refused_by) {
                let offer = crate::world::entity::next_offer_id();
                *pickup.reserved = Some(Reservation {
                    player: collector.handle.id,
                    at: self.sched.tick,
                    offer,
                });
                collector.handle.send(PlayerEvent::OfferItem {
                    item: *id,
                    offer,
                    stack,
                    region: self.handle.clone(),
                });
            }
        }
        for (id, collector) in taken {
            self.take_entity(id, collector, 1);
            self.remove_entity(id, &[]);
        }
    }

    /// A mob picked up `count` of the dropped item `id`.
    pub(super) fn push_target(&mut self, target: i32, push: [f64; 3], nudge: bool) {
        if let Some(handle) = self.seen_player(target) {
            handle.send(if nudge {
                PlayerEvent::Nudged(push)
            } else {
                PlayerEvent::Pushed(push)
            });
        } else if let Some(tracked) = self.ents.entities.get_mut(&target) {
            let base = tracked.entity.base_mut();
            for axis in 0..3 {
                base.velocity[axis] += push[axis];
            }
            tracked.sync.impulse();
        }
    }

    pub(super) fn mob_takes_item(&mut self, collector: i32, id: i32, count: u8) {
        let Some(item) = self.entity_mut::<ItemEntity>(id) else {
            return;
        };
        if item.reserved.is_some() || item.pickup_delay > 0 {
            return;
        }
        let taken = count.min(item.stack.count());
        let stack = item.stack.take(taken);
        let emptied = item.stack.is_empty();
        let thrower = item.thrower();
        let at = item.base.position;
        if let Some(base) = self.ents.entities.get(&collector).map(|t| t.entity.base()) {
            let taker = crate::world::region_api::ItemTaker::Entity {
                kind: base.kind,
                uuid: base.uuid,
            };
            self.report_pickup(taker, &stack, at, thrower);
        }
        if let Some(picker) = self.ents.entities.get(&collector).map(|t| t.entity.facts()) {
            self.thrown_item_picked_up(thrower, &stack, picker, None);
        }
        match self.ents.entities.get_mut(&collector) {
            Some(tracked) => tracked.entity.took_item(stack),
            None => self.ents.taken_stacks.push((collector, stack)),
        }
        self.take_entity(id, collector, i32::from(taken));
        if emptied {
            self.remove_entity(id, &[]);
        } else {
            self.refresh_item_stack(id);
        }
    }

    /// `picker` (the player `picked_by`, or a mob) picked up `stack`, which
    /// `thrower` threw: the thrower's `thrown_item_picked_up_by_entity`
    /// when it is a player, and a picking player's
    /// `thrown_item_picked_up_by_player`, while the thrower is loaded.
    pub(super) fn thrown_item_picked_up(
        &mut self,
        thrower: Option<u128>,
        stack: &ItemStack,
        picker: crate::world::predicate::EntityFacts,
        picked_by: Option<&PlayerHandle>,
    ) {
        let Some(thrower) = thrower else {
            return;
        };
        let dimension = self.dimension().key.clone();
        let player = self
            .seen_players()
            .iter()
            .find(|p| p.uuid == thrower)
            .map(|p| (p.handle.clone(), p.position));
        let facts = match &player {
            Some((_, position)) => Some(crate::world::predicate::EntityFacts {
                kind: Some(tuff_vanilla_data::registry::EntityType::Player),
                position: *position,
                dimension,
                ..Default::default()
            }),
            None => self
                .ents
                .entities
                .values()
                .find(|t| t.entity.base().uuid == thrower)
                .map(|t| t.entity.facts()),
        };
        let Some(facts) = facts else {
            return;
        };
        if let Some((handle, _)) = player {
            handle.send(PlayerEvent::Advancement(Box::new(
                AdvancementEvent::ThrownItemPickedUpByEntity {
                    item: tuff_item::item::ItemSnapshot::of(stack),
                    entity: picker,
                },
            )));
        }
        if let Some(picked_by) = picked_by {
            picked_by.send(PlayerEvent::Advancement(Box::new(
                AdvancementEvent::ThrownItemPickedUpByPlayer {
                    item: tuff_item::item::ItemSnapshot::of(stack),
                    thrower: facts,
                },
            )));
        }
    }

    /// Shows `collector` picking up `count` of the entity `id`.
    pub(super) fn take_entity(&mut self, id: i32, collector: i32, count: i32) {
        let Some(chunk) = self.ents.entities.get(&id).map(|t| t.chunk()) else {
            return;
        };
        let take = packets([framed(&play::TakeItemEntity {
            collected: id,
            collector,
            count,
        })]);
        self.fanout_chunk(chunk, None, |_| PlayerEvent::EntityPackets {
            entity: id,
            packets: take.clone(),
        });
    }

    pub(super) fn observing_items(
        &self,
    ) -> Vec<std::sync::Arc<dyn crate::world::region_api::RegionHooks>> {
        self.shared
            .region_hooks
            .iter()
            .filter(|hook| hook.observes_items_and_deaths())
            .cloned()
            .collect()
    }

    pub(super) fn report_pickup(
        &mut self,
        taker: crate::world::region_api::ItemTaker,
        stack: &ItemStack,
        position: [f64; 3],
        thrower: Option<u128>,
    ) {
        let hooks = self.observing_items();
        if hooks.is_empty() {
            return;
        }
        let pickup = crate::world::region_api::ItemPickup {
            taker,
            stack: tuff_item::item::ItemSnapshot::of(stack),
            position,
            thrower,
        };
        for hook in hooks {
            hook.item_picked_up(self, &pickup);
        }
    }

    pub(super) fn report_death(
        &mut self,
        id: i32,
        player: Option<u128>,
        attacker: Option<EntityType>,
        damage: &'static str,
    ) {
        let hooks = self.observing_items();
        if hooks.is_empty() {
            return;
        }
        let Some(tracked) = self.ents.entities.get(&id) else {
            return;
        };
        let base = tracked.entity.base();
        let death = crate::world::region_api::EntityDeath {
            id,
            uuid: base.uuid,
            kind: base.kind,
            position: base.position,
            nbt: crate::world::entity::save(&*tracked.entity),
            player,
            attacker,
            damage,
        };
        for hook in hooks {
            hook.entity_died(self, &death);
        }
    }

    /// A player answered offer `offer` of the entity `id` with room for
    /// `taken`. Only the outstanding offer is honoured, and what is taken
    /// is granted to the player only here, so a stale answer changes
    /// nothing on either side.
    pub(in crate::world::region) fn item_taken(
        &mut self,
        id: i32,
        offer: u64,
        player: &PlayerHandle,
        taken: u8,
    ) {
        let answers = |reserved: &Option<Reservation>| {
            reserved.is_some_and(|r| r.offer == offer && r.player == player.id)
        };
        if let Some(pickup) = self
            .ents
            .entities
            .get_mut(&id)
            .and_then(|tracked| tracked.entity.ground_pickup())
        {
            if !answers(pickup.reserved) {
                return;
            }
            *pickup.reserved = None;
            if taken == 0 {
                if !pickup.refused_by.contains(&player.id) {
                    pickup.refused_by.push(player.id);
                }
                return;
            }
            if pickup.stack.is_none() {
                return;
            }
            let at = self
                .ents
                .entities
                .get(&id)
                .map(|t| t.entity.base().position);
            let pos = BlockPos::containing(at.unwrap_or_default());
            if !self.may_transfer_with(Transfer::Pickup, pos, player, false) {
                return;
            }
            let Some(held) = self
                .ents
                .entities
                .get_mut(&id)
                .and_then(|tracked| tracked.entity.ground_pickup())
                .and_then(|pickup| pickup.stack)
            else {
                return;
            };
            let stack = std::mem::replace(held, ItemStack::empty());
            if let Err(refused) = player.try_send(PlayerEvent::ItemGranted {
                stack,
                picked_up: true,
            }) && let PlayerEvent::ItemGranted { stack, .. } = *refused
            {
                if let Some(held) = self
                    .ents
                    .entities
                    .get_mut(&id)
                    .and_then(|tracked| tracked.entity.ground_pickup())
                    .and_then(|pickup| pickup.stack)
                {
                    *held = stack;
                }
                return;
            }
            self.take_entity(id, player.id, 1);
            self.remove_entity(id, &[]);
            return;
        }
        let Some(item) = self.entity_mut::<ItemEntity>(id) else {
            return;
        };
        if !answers(&item.reserved) {
            return;
        }
        item.reserved = None;
        if taken == 0 {
            if !item.refused_by.contains(&player.id) {
                item.refused_by.push(player.id);
            }
            return;
        }
        let pos = BlockPos::containing(item.base.position);
        if !self.may_transfer_with(Transfer::Pickup, pos, player, false) {
            return;
        }
        let Some(item) = self.entity_mut::<ItemEntity>(id) else {
            return;
        };
        let before = u64::from(item.stack.count());
        let taken = taken.min(item.stack.count());
        let granted = item.stack.take(taken);
        let after = u64::from(item.stack.count());
        let shown = tuff_item::item::ItemSnapshot::of(&granted);
        let thrower = item.thrower();
        let at = item.base.position;
        let chunk = item.base.chunk();
        let sent = u64::from(granted.count());
        if !self.transfer_balanced(Transfer::Pickup, pos, before, 0, after, sent) {
            if let Some(item) = self.entity_mut::<ItemEntity>(id)
                && let Err(stack) = item.stack.join(granted)
            {
                self.spawn_item_at(stack, at, [0.0; 3], 0);
            }
            return;
        }
        if let Err(refused) = player.try_send(PlayerEvent::ItemGranted {
            stack: granted,
            picked_up: true,
        }) {
            if let PlayerEvent::ItemGranted { stack, .. } = *refused
                && let Some(item) = self.entity_mut::<ItemEntity>(id)
                && let Err(stack) = item.stack.join(stack)
            {
                debug_assert!(false, "a refused grant no longer fits: {stack:?}");
                self.spawn_item_at(stack, at, [0.0; 3], 0);
            }
            return;
        }
        let emptied = self
            .entity_mut::<ItemEntity>(id)
            .is_none_or(|item| item.stack.is_empty());
        self.report_pickup(
            crate::world::region_api::ItemTaker::Player(player.id),
            &shown,
            at,
            thrower,
        );
        let take = packets([framed(&play::TakeItemEntity {
            collected: id,
            collector: player.id,
            count: taken as i32,
        })]);
        self.fanout_chunk(chunk, None, |_| PlayerEvent::EntityPackets {
            entity: id,
            packets: take.clone(),
        });
        if emptied {
            self.remove_entity(id, &[]);
        } else {
            self.refresh_item_stack(id);
        }
        let picker = self.ents.presence.get(&player.id).map(|p| p.position);
        if let Some(position) = picker {
            let picker = crate::world::predicate::EntityFacts {
                kind: Some(tuff_vanilla_data::registry::EntityType::Player),
                position,
                dimension: self.dimension().key.clone(),
                ..Default::default()
            };
            self.thrown_item_picked_up(thrower, &shown, picker, Some(player));
        }
    }

    /// Tells the viewers of dropped item `id` what it holds now.
    pub(in crate::world::region) fn refresh_item_stack(&mut self, id: i32) {
        let Some(tracked) = self.ents.entities.get_mut(&id) else {
            return;
        };
        let Some(item) = tracked.entity.downcast_ref::<ItemEntity>() else {
            return;
        };
        let chunk = item.base.chunk();
        let snapshot = tuff_item::item::ItemSnapshot::of(&item.stack);
        let data =
            tracked.data_packets(&[(metadata::ITEM_STACK, MetadataValue::Slot(Some(snapshot)))]);
        if let Some(column) = self.columns.get_mut(&chunk) {
            column.entities_dirty = true;
        }
        self.fanout_chunk(chunk, None, |_| PlayerEvent::EntityPackets {
            entity: id,
            packets: data.clone(),
        });
    }

    /// Takes the whole stack out of dropped item `id`, leaving an empty
    /// husk until [`return_item_stack`](Self::return_item_stack).
    pub(in crate::world::region) fn take_item_stack(&mut self, id: i32) -> Option<ItemStack> {
        let item = self.entity_mut::<ItemEntity>(id)?;
        Some(std::mem::replace(&mut item.stack, ItemStack::empty()))
    }

    /// Puts what is left of a stack taken with
    /// [`take_item_stack`](Self::take_item_stack) back into item `id`,
    /// removing the item when nothing is left and telling viewers when
    /// its count is no longer `before`.
    pub(in crate::world::region) fn return_item_stack(
        &mut self,
        id: i32,
        stack: Option<ItemStack>,
        before: u8,
    ) {
        let Some(item) = self.entity_mut::<ItemEntity>(id) else {
            if let Some(stack) = stack {
                debug_assert!(false, "returned a stack to a vanished item: {stack:?}");
                let at = self
                    .ents
                    .entities
                    .get(&id)
                    .map_or([0.0; 3], |t| t.entity.base().position);
                self.spawn_item_at(stack, at, [0.0; 3], 0);
            }
            return;
        };
        let stack = stack.filter(|stack| !stack.is_empty());
        let after = stack.as_ref().map_or(0, ItemStack::count);
        if let Some(stack) = stack {
            item.stack = stack;
        }
        if after == 0 {
            self.remove_entity(id, &[]);
        } else if after != before {
            self.refresh_item_stack(id);
        }
    }

    /// Dropped items whose box meets `area`, in id order.
    pub(in crate::world::region) fn items_in(&self, area: [f64; 6]) -> Vec<i32> {
        let mut found: Vec<i32> = self
            .ents
            .entities
            .iter()
            .filter_map(|(id, tracked)| {
                let item = tracked.entity.downcast_ref::<ItemEntity>()?;
                experience::intersects(item.bounds(), area).then_some(*id)
            })
            .collect();
        found.sort_unstable();
        found
    }
}

/// Whether an offer made at `reserved` still holds the entity back.
fn reservation_live(reserved: Option<Reservation>, tick: u64) -> bool {
    reserved.is_some_and(|r| tick - r.at < PICKUP_RESERVATION_TICKS)
}

/// The first player in range who has not refused since everyone in range
/// last did; once all have, the refusals are forgotten and the first is
/// asked again (their inventory may have room by now).
fn pick_collector<'a>(
    mut in_range: impl Iterator<Item = &'a crate::world::region::Presence>,
    refused_by: &mut Vec<PlayerId>,
) -> Option<&'a crate::world::region::Presence> {
    let first = in_range.next()?;
    let found = std::iter::once(first)
        .chain(in_range)
        .find(|p| !refused_by.contains(&p.handle.id));
    Some(found.unwrap_or_else(|| {
        refused_by.clear();
        first
    }))
}
