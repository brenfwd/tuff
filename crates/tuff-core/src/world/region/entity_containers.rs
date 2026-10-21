//! Menus over region entities' containers (chest boats): the entity owns
//! the slots, the region keeps who views them and applies transactions,
//! as it does for block containers.

use rand::SeedableRng;
use rand::rngs::StdRng;
use tuff_item::item::DupUnchecked;

use super::*;
use tuff_item::item::{ItemSnapshot, ItemStack};
use tuff_item::text::TextComponent;
use tuff_vanilla_data::block::Facing;

use super::{HOPPER_SUCK_MAX_Y, HOPPER_SUCK_MIN_Y};
use crate::world::block_entity::{self, ContainerKind};
use crate::world::experience;
use crate::world::messages::{EntitySlotChange, MenuOpen, MenuSource};
use crate::world::region_api::SlotEdit;

impl RegionActor {
    /// Opens the container of entity `target` for `player`, rolling a
    /// pending loot table first. Spectators may look but never roll loot.
    pub(super) fn open_entity_container(&mut self, player: &PlayerHandle, target: i32) {
        let spectator = self
            .ents
            .presence
            .get(&player.id)
            .is_some_and(|presence| presence.spectator);
        let Some(tracked) = self.ents.entities.get(&target) else {
            return;
        };
        if tracked.entity.container().is_none() {
            return;
        }
        if spectator && tracked.entity.loot_pending() {
            super::containers::spectator_cant_open(player);
            return;
        }
        let loot = self.shared.block_loot();
        let tracked = self.ents.entities.get_mut(&target).expect("checked above");
        if let Some((table, seed)) = tracked.entity.take_loot_table() {
            if !spectator {
                player.send(super::containers::loot_generated(table.clone()));
            }
            let slots = tracked.entity.container().map_or(0, <[_]>::len);
            let [x, y, z] = tracked.entity.base().position;
            let origin = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
            let items = if seed == 0 {
                loot.container_loot(&table, slots, &mut self.rng)
            } else {
                loot.container_loot(&table, slots, &mut StdRng::seed_from_u64(seed as u64))
            };
            if let Some(items) = items {
                self.explore_loot(
                    super::exploring::ExploreTarget::Entity(target),
                    origin,
                    &items,
                );
                if let Some(container) = self
                    .ents
                    .entities
                    .get_mut(&target)
                    .and_then(|tracked| tracked.entity.container_mut())
                {
                    let mut items = items.into_iter();
                    for (slot, stack) in container.iter_mut().zip(&mut items) {
                        if let Some(old) = std::mem::replace(slot, stack) {
                            old.burn(tuff_item::item::BurnReason::Loot);
                        }
                    }
                    for extra in items.flatten() {
                        extra.burn(tuff_item::item::BurnReason::Loot);
                    }
                }
            }
        }
        let tracked = self.ents.entities.get(&target).expect("checked above");
        let base = tracked.entity.base();
        let title = base
            .custom_name
            .as_ref()
            .and_then(TextComponent::from_nbt)
            .unwrap_or_else(|| {
                let (namespace, path) = base.kind.name().split_once(':').unwrap_or(("", ""));
                TextComponent::translate(format!("entity.{namespace}.{path}"), [])
            });
        let contents = tracked
            .entity
            .container()
            .unwrap_or(&[])
            .iter()
            .map(DupUnchecked::dup_unchecked)
            .collect::<Vec<_>>();
        let kind = tracked.entity.container_menu();
        if !spectator {
            let viewers = self.menus.entity_viewers.entry(target).or_default();
            if !viewers.iter().any(|v| v.id == player.id) {
                viewers.push(player.clone());
            }
        }
        if !spectator {
            self.disturb_guarded_container(player.id);
            self.entity_container_event(
                player.id,
                target,
                crate::world::game_event::GameEvent::ContainerOpen,
            );
        }
        player.send(PlayerEvent::MenuOpen(MenuOpen {
            dimension: self.dimension,
            region: self.handle.clone(),
            kind,
            title,
            source: MenuSource::Entity {
                entity: target,
                contents,
            },
            stat: None,
        }));
    }

    /// Posts a container open or close game event at entity `target`.
    pub(super) fn entity_container_event(
        &mut self,
        player: crate::world::messages::PlayerId,
        target: i32,
        event: crate::world::game_event::GameEvent,
    ) {
        use crate::world::game_event::{Context, Source};
        use crate::world::region_api::RegionWorld;
        let Some(tracked) = self.ents.entities.get(&target) else {
            return;
        };
        let position = tracked.entity.base().position;
        RegionWorld::game_event(self, event, position, Context::from(Source::player(player)));
    }

    /// Passes the viewers of `entity` on to `neighbor`, which was just
    /// handed the entity.
    pub(super) fn hand_off_viewers(&mut self, entity: i32, neighbor: &RegionHandle) {
        if let Some(viewers) = self.menus.entity_viewers.remove(&entity) {
            neighbor.internal(RegionInternal::EntityViewers { entity, viewers });
        }
    }

    pub(super) fn receive_entity_viewers(&mut self, entity: i32, viewers: Vec<PlayerHandle>) {
        for viewer in viewers {
            viewer.send(PlayerEvent::EntityContainerMoved {
                dimension: self.dimension,
                entity,
                region: self.handle.clone(),
            });
            let list = self.menus.entity_viewers.entry(entity).or_default();
            if !list.iter().any(|v| v.id == viewer.id) {
                list.push(viewer);
            }
        }
    }

    /// Closes menus over entities no longer in this region.
    pub(super) fn sweep_entity_viewers(&mut self) {
        let _span = tuff_timings::span!("region.entity_viewers");
        let gone: Vec<i32> = self
            .menus
            .entity_viewers
            .keys()
            .filter(|id| {
                !self
                    .chunk_io
                    .loading
                    .values()
                    .any(|l| l.entities.iter().any(|(e, _)| e.base().id == **id))
                    && self
                        .ents
                        .entities
                        .get(id)
                        .is_none_or(|tracked| tracked.entity.container().is_none())
            })
            .copied()
            .collect();
        for entity in gone {
            for viewer in self
                .menus
                .entity_viewers
                .remove(&entity)
                .unwrap_or_default()
            {
                viewer.send(PlayerEvent::EntityContainerClosed {
                    dimension: self.dimension,
                    entity,
                });
            }
        }
    }

    pub(super) fn entity_transaction(
        &mut self,
        player: PlayerHandle,
        window_id: i32,
        entity: i32,
        changes: Vec<EntitySlotChange>,
    ) {
        let holder = self.entity_holder(entity);
        let viewing = self
            .menus
            .entity_viewers
            .get(&entity)
            .is_some_and(|viewers| viewers.iter().any(|v| v.id == player.id));
        let container = self
            .ents
            .entities
            .get_mut(&entity)
            .and_then(|tracked| tracked.entity.container_mut());
        let Some(container) = container.filter(|_| viewing) else {
            player.send(PlayerEvent::EntityTransaction {
                window_id,
                accepted: false,
                contents: Vec::new(),
            });
            return;
        };
        let valid = changes
            .iter()
            .all(|change| container.get(change.slot) == Some(&change.expected));
        if !valid {
            player.send(PlayerEvent::EntityTransaction {
                window_id,
                accepted: false,
                contents: container
                    .iter()
                    .map(DupUnchecked::dup_unchecked)
                    .collect::<Vec<_>>(),
            });
            return;
        }
        let mut slots = Vec::with_capacity(changes.len());
        let mut edits = Vec::new();
        for change in changes {
            if holder.is_some() {
                edits.push(SlotEdit::of(
                    change.slot,
                    container[change.slot].as_ref(),
                    change.new.as_ref(),
                ));
            }
            container[change.slot] = change.new;
            slots.push(change.slot);
        }
        if let Some(holder) = holder {
            self.report_container_edit(holder, edits);
        }
        player.send(PlayerEvent::EntityTransaction {
            window_id,
            accepted: true,
            contents: Vec::new(),
        });
        self.entity_slots_changed(entity, &slots, Some(player.id));
    }

    /// Marks the entity for saving and shows its changed slots to everyone
    /// viewing its container but `except`.
    pub(super) fn entity_slots_changed(
        &mut self,
        entity: i32,
        changed: &[usize],
        except: Option<PlayerId>,
    ) {
        if let Some(chunk) = self.ents.entities.get(&entity).map(|t| t.chunk())
            && let Some(column) = self.columns.get_mut(&chunk)
        {
            column.entities_dirty = true;
        }
        let dimension = self.dimension;
        if !self.menus.entity_viewers.contains_key(&entity) {
            return;
        }
        let Some(items) = self
            .ents
            .entities
            .get(&entity)
            .and_then(|tracked| tracked.entity.container())
        else {
            return;
        };
        let slots: Vec<(usize, Option<ItemSnapshot>)> = changed
            .iter()
            .filter_map(|&slot| Some((slot, items.get(slot)?.as_ref().map(ItemSnapshot::of))))
            .collect();
        if let Some(viewers) = self.menus.entity_viewers.get_mut(&entity) {
            viewers.retain(|viewer| {
                Some(viewer.id) == except
                    || viewer.send(PlayerEvent::EntitySlots {
                        dimension,
                        entity,
                        slots: slots.clone(),
                    })
            });
        }
    }

    /// One entity with a container whose box meets the unit box centred
    /// on `center`, picked at random as hoppers do.
    pub(super) fn container_entity_near(&mut self, center: [f64; 3]) -> Option<i32> {
        let [x, y, z] = center;
        let area = [x - 0.5, y - 0.5, z - 0.5, x + 0.5, y + 0.5, z + 0.5];
        let mut found: Vec<i32> = self
            .ents
            .entities
            .iter()
            .filter(|(_, tracked)| {
                tracked.entity.container().is_some()
                    && !matches!(
                        tracked.entity.container_menu(),
                        crate::world::menu::MenuKind::Mount { .. }
                    )
                    && experience::intersects(tracked.entity.base().bounds(), area)
            })
            .map(|(id, _)| *id)
            .collect();
        if found.is_empty() {
            return None;
        }
        found.sort_unstable();
        Some(found[self.rng.gen_range(0..found.len())])
    }

    /// [`Self::container_entity_near`] the centre of the block at `pos`.
    pub(super) fn container_entity_at(&mut self, pos: BlockPos) -> Option<i32> {
        self.container_entity_near([pos.x as f64 + 0.5, pos.y as f64 + 0.5, pos.z as f64 + 0.5])
    }

    /// Puts `stack` into the container of `entity` as a hopper would;
    /// returns what is left.
    pub(super) fn insert_into_entity(
        &mut self,
        entity: i32,
        stack: ItemStack,
    ) -> Option<ItemStack> {
        let recipes = self.shared.recipes();
        let holder = self.entity_holder(entity);
        let Some(items) = self
            .ents
            .entities
            .get_mut(&entity)
            .and_then(|tracked| tracked.entity.container_mut())
        else {
            return Some(stack);
        };
        let kind = entity_container_kind(items.len());
        let before = holder.map(|_| {
            items
                .iter()
                .map(|stack| stack.as_ref().map(ItemSnapshot::of))
                .collect::<Vec<_>>()
        });
        let mut changed = Vec::new();
        let rest = block_entity::insert(&recipes, kind, items, None, stack, &mut changed, 0);
        if let (Some(holder), Some(before)) = (holder, before) {
            let edits = changed
                .iter()
                .map(|&slot| SlotEdit {
                    slot,
                    old: before[slot].clone(),
                    new: items[slot].as_ref().map(ItemSnapshot::of),
                })
                .collect();
            self.report_container_edit(holder, edits);
        }
        if !changed.is_empty() {
            self.entity_slots_changed(entity, &changed, None);
        }
        rest
    }

    /// Moves one item out of the first non-empty slot of `entity`'s
    /// container that `accept` takes; a refused item goes back.
    pub(super) fn extract_from_entity(
        &mut self,
        entity: i32,
        mut accept: impl FnMut(&mut Self, ItemStack) -> Result<(), ItemStack>,
    ) -> bool {
        let size = self
            .ents
            .entities
            .get(&entity)
            .and_then(|tracked| tracked.entity.container())
            .map_or(0, <[_]>::len);
        let at = self
            .ents
            .entities
            .get(&entity)
            .map_or([0.0; 3], |tracked| tracked.entity.base().position);
        for slot in 0..size {
            let holder = self.entity_holder(entity);
            let Some(held) = self
                .ents
                .entities
                .get_mut(&entity)
                .and_then(|tracked| tracked.entity.container_mut()?.get_mut(slot))
            else {
                continue;
            };
            let old = holder.and_then(|_| held.as_ref().map(ItemSnapshot::of));
            let Some(one) = ItemStack::take_from(held, 1) else {
                continue;
            };
            match accept(self, one) {
                Ok(()) => {}
                Err(one) => {
                    if let Some(held) = self
                        .ents
                        .entities
                        .get_mut(&entity)
                        .and_then(|tracked| tracked.entity.container_mut()?.get_mut(slot))
                    {
                        ItemStack::put_back(held, one);
                    } else {
                        debug_assert!(false, "a container entity vanished mid-move");
                        self.spawn_item_at(one, at, [0.0; 3], 0);
                    }
                    continue;
                }
            }
            if let Some(holder) = holder {
                let new = self
                    .ents
                    .entities
                    .get(&entity)
                    .and_then(|tracked| tracked.entity.container()?.get(slot)?.as_ref())
                    .map(ItemSnapshot::of);
                self.report_container_edit(holder, vec![SlotEdit { slot, old, new }]);
            }
            self.entity_slots_changed(entity, &[slot], None);
            return true;
        }
        false
    }

    /// Hopper minecarts: each enabled one pulls one item a tick from the
    /// container above it, else from dropped items around it.
    pub(super) fn tick_entity_hoppers(&mut self) {
        let _span = tuff_timings::span!("region.entity_hoppers");
        let mut hoppers: Vec<i32> = self
            .ents
            .entities
            .iter()
            .filter(|(_, tracked)| tracked.entity.collects_items())
            .map(|(id, _)| *id)
            .collect();
        if hoppers.is_empty() {
            return;
        }
        hoppers.sort_unstable();
        for id in hoppers {
            let Some(cause) = self
                .ents
                .entities
                .get(&id)
                .map(|tracked| BlockCause::entity(tracked.entity.as_ref()))
            else {
                continue;
            };
            self.with_cause(cause, |region| region.entity_hopper_pull(id));
        }
    }

    /// Moves one item from the block container `(kind, parts)` into the
    /// container of `entity`.
    fn pull_from_block_container(
        &mut self,
        entity: i32,
        (kind, parts): (ContainerKind, Vec<BlockPos>),
    ) -> bool {
        let face = Some(Facing::Down);
        for part in parts {
            if !self.owns(part) {
                continue;
            }
            for slot in kind.slots_for_face(face) {
                let extractable = self
                    .block_entity(part)
                    .and_then(|block| block.items[slot].as_ref())
                    .is_some_and(|stack| kind.can_extract(slot, stack, face));
                if !extractable {
                    continue;
                }
                let Some(one) = self.take_one_out(part, slot) else {
                    continue;
                };
                let shown = self.observes_containers().then(|| ItemSnapshot::of(&one));
                if let Some(back) = self.insert_into_entity(entity, one) {
                    self.return_one(part, slot, back);
                    continue;
                }
                self.took_one(part, slot, shown.as_deref());
                return true;
            }
        }
        false
    }

    fn entity_hopper_pull(&mut self, id: i32) -> bool {
        let Some(tracked) = self.ents.entities.get(&id) else {
            return false;
        };
        let [x, y, z] = tracked.entity.base().position;
        let bounds = tracked.entity.base().bounds();
        let full = tracked.entity.container().is_none_or(|items| {
            items.iter().all(|slot| {
                slot.as_ref()
                    .is_some_and(|s| s.count() >= s.max_stack_size())
            })
        });
        if full {
            return false;
        }
        let above = BlockPos::new(x.floor() as i32, (y + 1.5).floor() as i32, z.floor() as i32);
        let source = if let Some(source) = self.container_at(above) {
            Some(self.pull_from_block_container(id, source))
        } else if let Some(pulled) = self.cart_pull_port(id, above) {
            Some(pulled)
        } else {
            self.container_entity_near([x, y + 1.5, z])
                .filter(|other| *other != id)
                .map(|other| {
                    self.extract_from_entity(other, |region, one| {
                        region.insert_into_entity(id, one).map_or(Ok(()), Err)
                    })
                })
        };
        if source == Some(true) {
            return true;
        }
        let blocked = source.is_some()
            || self.block(above).is_some_and(|state| {
                state.is_collision_full_block()
                    && !tuff_datapack::tags::is(
                        tuff_vanilla_data::tag::block::key::DOES_NOT_BLOCK_HOPPERS,
                        state.block(),
                    )
            });
        let mut items = Vec::new();
        if !blocked {
            items.extend(self.items_in([
                x - 0.5,
                y + HOPPER_SUCK_MIN_Y,
                z - 0.5,
                x + 0.5,
                y + HOPPER_SUCK_MAX_Y,
                z + 0.5,
            ]));
        }
        items.extend(self.items_in([
            bounds[0] - CART_REACH,
            bounds[1],
            bounds[2] - CART_REACH,
            bounds[3] + CART_REACH,
            bounds[4],
            bounds[5] + CART_REACH,
        ]));
        let mut seen = HashSet::new();
        for item in items {
            if !seen.insert(item) {
                continue;
            }
            let Some(stack) = self.take_item_stack(item) else {
                continue;
            };
            let before = stack.count();
            let rest = self.insert_into_entity(id, stack);
            let left = rest.as_ref().map_or(0, ItemStack::count);
            self.return_item_stack(item, rest, before);
            if left != before {
                return true;
            }
        }
        false
    }
}

/// How far past its own box a hopper minecart reaches for dropped items.
const CART_REACH: f64 = 0.25;

/// The container kind whose insertion rules an entity container of `size`
/// slots follows.
pub(super) fn entity_container_kind(size: usize) -> ContainerKind {
    if size == ContainerKind::Hopper.size() {
        ContainerKind::Hopper
    } else {
        ContainerKind::Chest
    }
}
