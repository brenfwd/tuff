//! Block entities: containers (chests, hoppers, furnaces, brewing stands,
//! campfires), their menus, locks, loot tables and viewer bookkeeping.
//! Called from region.rs's dispatch and from the block edit pipeline.

use std::collections::HashMap;
use tuff_item::item::DupUnchecked;
use tuff_item::item::ItemSnapshot;
use tuff_item::item::MintReason;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tuff_vanilla_data::block::{Facing, Type as ChestType};
use tuff_vanilla_data::registry::{BlockEntityType, DataComponentType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};

use super::{
    HOPPER_SUCK_MAX_Y, HOPPER_SUCK_MIN_Y, RegionActor, SHULKER_LID_DEPTH, SHULKER_LID_EPSILON,
    menu_stat,
};
use crate::protocol::play;
use crate::world::block_entity::{self, BlockEntity, ContainerKind, furnace};
use crate::world::experience;
use crate::world::items::{self, ItemEntity};
use crate::world::menu::MenuKind;
use crate::world::messages::{
    AdvancementEvent, ContainerChange, MenuOpen, MenuSource, OpenedHalf, Place, PlayerEvent,
    PlayerHandle, PlayerId, RegionHandle, RegionInternal,
};
use crate::world::placement;
use crate::world::region_api::BlockCause;
use crate::world::{framed, packets};
use tuff_item::item::{Components, ItemStack};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

impl RegionActor {
    pub(super) fn block_entity(&self, pos: BlockPos) -> Option<&BlockEntity> {
        self.columns
            .get(&ChunkPos::of_block(pos))?
            .meta
            .block_entities
            .get(&pos)
    }

    /// Mutable access marks the chunk for saving.
    pub(super) fn block_entity_mut(&mut self, pos: BlockPos) -> Option<&mut BlockEntity> {
        let column = self.columns.get_mut(&ChunkPos::of_block(pos))?;
        let entity = column.meta.block_entities.get_mut(&pos)?;
        column.blocks_dirty = true;
        self.sim.redstone.container_changes.insert(pos);
        Some(entity)
    }

    /// A block entity this server keeps as saved NBT.
    pub(super) fn raw_block_entity(&self, pos: BlockPos) -> Option<&Compound> {
        self.columns
            .get(&ChunkPos::of_block(pos))?
            .meta
            .raw_block_entities
            .get(&pos)
    }

    /// Mutable access marks the chunk for saving and drops its cached
    /// packet, which lists the block entity's client data.
    pub(super) fn raw_block_entity_mut(&mut self, pos: BlockPos) -> Option<&mut Compound> {
        let column = self.columns.get_mut(&ChunkPos::of_block(pos))?;
        let nbt = column.meta.raw_block_entities.get_mut(&pos)?;
        column.blocks_dirty = true;
        column.version += 1;
        column.packet = None;
        self.sim.redstone.container_changes.insert(pos);
        Some(nbt)
    }

    /// The type of the block entity at `pos`, modeled or not.
    pub(super) fn block_entity_kind(&self, pos: BlockPos) -> Option<BlockEntityType> {
        match self.block_entity(pos) {
            Some(entity) => Some(entity.kind.entity_type()),
            None => self.raw_block_entity(pos).and_then(block_entity::raw::kind),
        }
    }

    /// The block entity at `pos` as it is saved.
    pub(super) fn saved_block_entity(&self, pos: BlockPos) -> Option<Compound> {
        match self.block_entity(pos) {
            Some(entity) => Some(entity.encode(pos)),
            None => self.raw_block_entity(pos).cloned(),
        }
    }

    /// Puts a block entity at `pos`, modeled (`Ok`) or as saved NBT,
    /// replacing whatever data was there.
    pub(super) fn store_block_entity(
        &mut self,
        pos: BlockPos,
        entity: Result<BlockEntity, Compound>,
    ) {
        let Some(column) = self.columns.get_mut(&ChunkPos::of_block(pos)) else {
            return;
        };
        column.blocks_dirty = true;
        column.version += 1;
        column.packet = None;
        self.sim.redstone.container_changes.insert(pos);
        match entity {
            Ok(entity) => {
                column.meta.raw_block_entities.remove(&pos);
                let ticks = entity.kind.ticks();
                column.meta.block_entities.insert(pos, entity);
                if ticks {
                    self.menus.ticking.insert(pos);
                }
            }
            Err(nbt) => {
                column.meta.raw_block_entities.insert(pos, nbt);
                if column.meta.block_entities.remove(&pos).is_some() {
                    self.menus.ticking.remove(&pos);
                    self.menus.hopper_ticks.remove(&pos);
                }
            }
        }
    }

    /// Gives the block at `pos` a new block entity of `kind`, which takes
    /// the components of the item that placed it.
    pub(super) fn create_block_entity(
        &mut self,
        pos: BlockPos,
        kind: BlockEntityType,
        components: &Components,
    ) {
        self.store_block_entity(pos, block_entity::create(kind, pos, components));
        if kind == BlockEntityType::CommandBlock
            && self
                .block(pos)
                .is_some_and(|s| s.block() == Block::ChainCommandBlock)
            && let Some(nbt) = self.raw_block_entity_mut(pos)
        {
            nbt.insert("auto", true);
        }
    }

    /// Gives a just placed block entity the data of the item that placed
    /// it: first its `block_entity_data` (when the types match, and for
    /// operator-only types only from creative operators), then its other
    /// components.
    pub(super) fn apply_placed_item(&mut self, pos: BlockPos, place: &Place) {
        self.apply_placed_components(pos, &place.components, place.game_master);
    }

    /// [`Self::apply_placed_item`] for an item placed by anything.
    pub(super) fn apply_placed_components(
        &mut self,
        pos: BlockPos,
        components: &Components,
        game_master: bool,
    ) {
        let Some(mut nbt) = self.saved_block_entity(pos) else {
            return;
        };
        let Some(kind) = block_entity::raw::kind(&nbt) else {
            return;
        };
        if let Some(data) = components
            .other
            .get_compound(DataComponentType::BlockEntityData.name())
            && data.get_str(block_entity::raw::ID) == Some(kind.name())
            && (!kind.only_op_can_set_data() || game_master)
        {
            block_entity::raw::apply_block_entity_data(&mut nbt, data);
        }
        block_entity::raw::apply_components(kind, &mut nbt, components);
        let entity = match BlockEntity::decode(&nbt) {
            Some((_, entity)) => Ok(entity),
            None => Err(nbt),
        };
        self.store_block_entity(pos, entity);
        if kind.sends_updates() && !self.block_entity_updates.contains(&pos) {
            self.block_entity_updates.push(pos);
        }
    }

    /// The block entity a container block at `pos` should have, created
    /// empty if the chunk came without one.
    pub(super) fn ensure_block_entity(&mut self, pos: BlockPos, kind: ContainerKind) -> bool {
        if self
            .block_entity(pos)
            .is_some_and(|entity| entity.kind == kind)
        {
            return true;
        }
        if !self.owns(pos) {
            return false;
        }
        self.create_block_entity(pos, kind.entity_type(), &Default::default());
        true
    }

    /// Takes the block entity at `pos` out of the world, closing menus over
    /// it and, with `spill`, scattering its contents. Returns it as saved.
    pub(super) fn remove_block_entity(
        &mut self,
        pos: BlockPos,
        spill: Option<BlockState>,
    ) -> Option<Compound> {
        let column = self.columns.get_mut(&ChunkPos::of_block(pos))?;
        if let Some(nbt) = column.meta.raw_block_entities.remove(&pos) {
            column.blocks_dirty = true;
            self.block_entity_updates.retain(|at| *at != pos);
            self.menus.sign_editors.remove(&pos);
            self.close_viewers(pos);
            if let Some(old) = spill
                && let Some(kind) = block_entity::raw::kind(&nbt)
            {
                let stacks = block_entity::raw::contents(kind, &nbt);
                let mut drops = block_entity::scatter(pos, stacks, &mut self.rng);
                drops.extend(block_entity::raw::held_drop(
                    kind,
                    &nbt,
                    old,
                    pos,
                    &mut self.rng,
                ));
                for (stack, position, velocity) in drops {
                    self.spawn_item_at(stack, position, velocity, items::BLOCK_DROP_PICKUP_DELAY);
                }
                if kind == BlockEntityType::Jukebox {
                    crate::world::region_api::RegionWorld::game_event(
                        self,
                        crate::world::game_event::GameEvent::JukeboxStopPlay,
                        pos.center(),
                        crate::world::game_event::Context::block(old),
                    );
                    self.level_event(play::level_event::SOUND_STOP_JUKEBOX_SONG, pos, 0, false);
                }
            }
            return Some(nbt);
        }
        let mut entity = column.meta.block_entities.remove(&pos)?;
        column.blocks_dirty = true;
        self.menus.ticking.remove(&pos);
        self.menus.hopper_ticks.remove(&pos);
        self.close_viewers(pos);
        if let Some(furnace) = entity.furnace() {
            let used = furnace.recipes_used.clone();
            self.pop_furnace_experience(&used, pos.center());
        }
        if spill.is_some()
            && let Some(key) = self.menus.pending_share(pos)
        {
            self.menus
                .orphans
                .entry(key)
                .or_default()
                .push((pos, entity.items.iter_mut().map(Option::take).collect()));
        } else if spill.is_some() {
            let stacks = block_entity::spilled(&mut entity);
            let drops = block_entity::scatter(pos, stacks, &mut self.rng);
            for (stack, position, velocity) in drops {
                self.spawn_item_at(stack, position, velocity, items::BLOCK_DROP_PICKUP_DELAY);
            }
        }
        Some(entity.encode(pos))
    }

    pub(super) fn spawn_item_at(
        &mut self,
        stack: ItemStack,
        position: [f64; 3],
        velocity: [f64; 3],
        pickup_delay: u32,
    ) {
        let id = self.shared.next_entity_id();
        let uuid = self.rng.r#gen();
        self.spawn_entity(ItemEntity::new(
            id,
            uuid,
            stack,
            position,
            velocity,
            pickup_delay,
        ));
    }

    pub(super) fn spawn_extended_item_at(
        &mut self,
        stack: ItemStack,
        position: [f64; 3],
        velocity: [f64; 3],
        pickup_delay: u32,
    ) {
        let id = self.shared.next_entity_id();
        let uuid = self.rng.r#gen();
        let mut item = ItemEntity::new(id, uuid, stack, position, velocity, pickup_delay);
        item.set_extended_lifetime();
        self.spawn_entity(item);
    }

    /// The block entities behind the container at `pos`, in menu slot
    /// order: both halves of a double chest (the right-hand one first), or
    /// just the one. Halves in another region cannot be joined and count as
    /// single chests.
    pub(super) fn container_parts(&self, pos: BlockPos, state: BlockState) -> Vec<BlockPos> {
        let Some(kind) = ContainerKind::of_block(state.block()) else {
            return Vec::new();
        };
        if !kind.pairs() {
            return vec![pos];
        }
        let partner = self
            .chest_partner(pos, state, kind)
            .filter(|partner| self.owns(*partner));
        match (partner, state.r#type()) {
            (Some(partner), Some(ChestType::Left)) => vec![partner, pos],
            (Some(partner), _) => vec![pos, partner],
            (None, _) => vec![pos],
        }
    }

    /// The other half of the double chest `state` at `pos`, if its block
    /// (owned here or seen in the halo) pairs back with it.
    fn chest_partner(
        &self,
        pos: BlockPos,
        state: BlockState,
        kind: ContainerKind,
    ) -> Option<BlockPos> {
        placement::chest_partner(state, pos).filter(|partner| {
            self.block(*partner).is_some_and(|other| {
                ContainerKind::of_block(other.block()) == Some(kind)
                    && other.facing() == state.facing()
                    && other.r#type().is_some_and(|t| t != ChestType::Single)
                    && placement::chest_partner(other, *partner) == Some(pos)
            })
        })
    }

    /// The other half of the double chest at `pos` when another region
    /// owns it.
    fn far_half(&self, pos: BlockPos, state: BlockState) -> Option<BlockPos> {
        let kind = ContainerKind::of_block(state.block()).filter(|kind| kind.pairs())?;
        self.chest_partner(pos, state, kind)
            .filter(|partner| !self.owns(*partner))
    }

    /// Whether the block above `pos` is solid enough to keep a chest lid
    /// shut.
    pub(super) fn lid_blocked(&self, pos: BlockPos) -> bool {
        self.block(placement::step(pos, Facing::Up))
            .is_some_and(BlockState::is_redstone_conductor)
            || self.cat_sitting_on(pos)
    }

    fn cat_sitting_on(&self, pos: BlockPos) -> bool {
        let (x, y, z) = (f64::from(pos.x), f64::from(pos.y) + 1.0, f64::from(pos.z));
        let area = [x, y, z, x + 1.0, y + 1.0, z + 1.0];
        self.ents.entities.values().any(|tracked| {
            let b = tracked.entity.bounds();
            tracked.entity.sits_on_chests()
                && b[0] < area[3]
                && b[3] > area[0]
                && b[1] < area[4]
                && b[4] > area[1]
                && b[2] < area[5]
                && b[5] > area[2]
        })
    }

    /// A shulker box opening its lid at `pos` shoves any shulker standing
    /// where the lid swings, which teleports it.
    fn push_shulkers_off_lid(&mut self, pos: BlockPos, facing: Facing) {
        let (dx, dy, dz) = facing.step();
        let mut area = [
            f64::from(pos.x),
            f64::from(pos.y),
            f64::from(pos.z),
            f64::from(pos.x) + 1.0,
            f64::from(pos.y) + 1.0,
            f64::from(pos.z) + 1.0,
        ];
        for (axis, step) in [dx, dy, dz].into_iter().enumerate() {
            match step {
                1 => area[axis + 3] += 0.5,
                -1 => area[axis] -= 0.5,
                _ => {}
            }
        }
        for tracked in self.ents.entities.values_mut() {
            if tracked.entity.base().kind == tuff_vanilla_data::registry::EntityType::Shulker
                && crate::world::physics::intersects(tracked.entity.bounds(), area)
            {
                tracked.entity.shulker_box_pushed();
            }
        }
    }

    /// Whether the block a shulker box at `pos` opens into leaves room for
    /// its lid.
    pub(super) fn shulker_lid_blocked(&self, pos: BlockPos, facing: Facing) -> bool {
        let neighbour = placement::step(pos, facing);
        let Some(state) = self.block(neighbour) else {
            return true;
        };
        let (min, max) = {
            let lo = SHULKER_LID_EPSILON;
            let hi = SHULKER_LID_DEPTH - SHULKER_LID_EPSILON;
            let full = (lo, 1.0 - lo);
            match facing {
                Facing::Up => ([full.0, lo, full.0], [full.1, hi, full.1]),
                Facing::Down => ([full.0, 1.0 - hi, full.0], [full.1, 1.0 - lo, full.1]),
                Facing::East => ([lo, full.0, full.0], [hi, full.1, full.1]),
                Facing::West => ([1.0 - hi, full.0, full.0], [1.0 - lo, full.1, full.1]),
                Facing::South => ([full.0, full.0, lo], [full.1, full.1, hi]),
                Facing::North => ([full.0, full.0, 1.0 - hi], [full.1, full.1, 1.0 - lo]),
            }
        };
        state.collision_boxes().iter().any(|b| {
            (0..3).all(|axis| (b[axis] as f64) < max[axis] && (b[axis + 3] as f64) > min[axis])
        })
    }

    /// Opens the container block `state` at the clicked position for the
    /// clicking player, if nothing keeps it shut.
    pub(super) fn open_container(&mut self, place: &Place, state: BlockState) {
        let pos = place.target;
        let player = &place.player;
        let spectator = self
            .ents
            .presence
            .get(&player.id)
            .is_some_and(|presence| presence.spectator);
        if state.block() == Block::EnderChest {
            if self.lid_blocked(pos) {
                return;
            }
            if !spectator {
                self.add_viewer(pos, player.clone());
            }
            player.send(PlayerEvent::MenuOpen(MenuOpen {
                dimension: self.dimension,
                region: self.handle.clone(),
                kind: MenuKind::Generic { rows: 3 },
                title: TextComponent::translate("container.enderchest", []),
                source: MenuSource::EnderChest { pos },
                stat: menu_stat(state.block()),
            }));
            return;
        }
        let Some((kind, menu)) =
            ContainerKind::of_block(state.block()).and_then(|kind| Some((kind, kind.menu()?)))
        else {
            return;
        };
        let parts = self.container_parts(pos, state);
        let shut = match kind {
            ContainerKind::Chest | ContainerKind::TrappedChest => {
                parts.iter().any(|part| self.lid_blocked(*part))
            }
            ContainerKind::ShulkerBox => {
                self.menus.viewers.get(&pos).is_none_or(Vec::is_empty)
                    && state
                        .facing()
                        .is_some_and(|facing| self.shulker_lid_blocked(pos, facing))
            }
            _ => false,
        };
        if shut
            || !parts
                .iter()
                .all(|part| self.ensure_block_entity(*part, kind))
        {
            return;
        }
        let title = {
            let mut title = None;
            for part in &parts {
                let entity = self.block_entity(*part).expect("ensured above");
                if title.is_none() {
                    title = entity.custom_name.clone();
                }
            }
            title.unwrap_or_else(|| {
                let key = if parts.len() > 1 {
                    "container.chestDouble"
                } else {
                    kind.title()
                };
                TextComponent::translate(key, [])
            })
        };
        let held = place.held.map(|item| {
            ItemStack::mint(item, 1, MintReason::CrossActor)
                .with_components(place.components.clone())
        });
        if parts
            .iter()
            .any(|part| self.container_locked(*part, held.as_ref(), spectator))
        {
            if !spectator {
                self.notify_locked(&parts, place, title);
            }
            return;
        }
        if spectator && parts.iter().any(|part| self.loot_pending(*part)) {
            spectator_cant_open(player);
            return;
        }
        if parts.len() == 1
            && let Some(far) = self.far_half(pos, state)
        {
            if let Some(neighbor) = self.border.neighbors.get(&ChunkPos::of_block(far).region()) {
                neighbor.internal(RegionInternal::OpenHalf {
                    place: place.clone(),
                    pos: far,
                    from: pos,
                    spectator,
                    reply: self.handle.clone(),
                });
            }
            return;
        }
        for part in &parts {
            self.unpack_container_loot(*part, &place.player);
        }
        let recipes = self.shared.recipes();
        let mut contents = Vec::new();
        let mut sized = Vec::with_capacity(parts.len());
        let mut data = Vec::new();
        for part in &parts {
            let entity = self.block_entity(*part).expect("ensured above");
            contents.extend(entity.items.iter().map(DupUnchecked::dup_unchecked));
            sized.push((*part, entity.items.len()));
            data.extend(entity.data());
            if kind == ContainerKind::Crafter {
                contents.push(recipes.grid_result(&entity.items, 3, 3));
            }
        }
        if !spectator {
            for part in &parts {
                self.add_viewer(*part, player.clone());
            }
        }
        let kind = if parts.len() > 1 {
            MenuKind::Generic {
                rows: (contents.len() / 9) as u8,
            }
        } else {
            menu
        };
        player.send(PlayerEvent::MenuOpen(MenuOpen {
            dimension: self.dimension,
            region: self.handle.clone(),
            kind,
            title,
            source: MenuSource::Blocks {
                parts: sized,
                contents,
                data,
                remote: None,
            },
            stat: menu_stat(state.block()),
        }));
    }

    /// Opens this region's half of a double chest clicked in the region
    /// owning the other half at `from`, answering that region.
    pub(super) fn open_half(
        &mut self,
        place: Place,
        pos: BlockPos,
        from: BlockPos,
        spectator: bool,
        reply: RegionHandle,
    ) {
        let Some(state) = self.block(pos).filter(|_| self.owns(pos)) else {
            return;
        };
        let Some(kind) = ContainerKind::of_block(state.block()) else {
            return;
        };
        if !kind.pairs()
            || state.r#type().is_none_or(|t| t == ChestType::Single)
            || placement::chest_partner(state, pos) != Some(from)
            || self.lid_blocked(pos)
            || !self.ensure_block_entity(pos, kind)
        {
            return;
        }
        let held = place.held.map(|item| {
            ItemStack::mint(item, 1, MintReason::CrossActor)
                .with_components(place.components.clone())
        });
        let custom_name = self
            .block_entity(pos)
            .and_then(|entity| entity.custom_name.clone());
        if self.container_locked(pos, held.as_ref(), spectator) {
            if !spectator {
                let title = custom_name
                    .unwrap_or_else(|| TextComponent::translate("container.chestDouble", []));
                let mut here = place.clone();
                here.target = pos;
                self.notify_locked(&[pos], &here, title);
            }
            return;
        }
        if spectator && self.loot_pending(pos) {
            return;
        }
        self.unpack_container_loot(pos, &place.player);
        if !spectator {
            self.add_viewer(pos, place.player.clone());
        }
        let contents = self
            .block_entity(pos)
            .map(|entity| entity.items.dup_unchecked())
            .unwrap_or_default();
        reply.internal(RegionInternal::HalfOpened {
            place,
            spectator,
            half: Some(OpenedHalf {
                pos,
                region: self.handle.clone(),
                contents,
                custom_name,
            }),
        });
    }

    /// Finishes opening a double chest whose far half `half` its region
    /// opened: views this half and hands the player the joined menu.
    pub(super) fn half_opened(&mut self, place: Place, spectator: bool, half: OpenedHalf) {
        let pos = place.target;
        let player = &place.player;
        let valid = self
            .block(pos)
            .filter(|_| self.owns(pos))
            .and_then(|state| {
                let kind = ContainerKind::of_block(state.block())?;
                (self.far_half(pos, state) == Some(half.pos) && self.ensure_block_entity(pos, kind))
                    .then_some(state)
            });
        let Some(state) = valid else {
            if !spectator {
                half.region.internal(RegionInternal::ReleaseHalf {
                    player: player.id,
                    pos: half.pos,
                });
            }
            return;
        };
        self.unpack_container_loot(pos, player);
        if !spectator {
            self.add_viewer(pos, player.clone());
        }
        let entity = self.block_entity(pos).expect("ensured above");
        let near = (
            pos,
            entity.items.dup_unchecked(),
            entity.custom_name.clone(),
        );
        let far = (half.pos, half.contents, half.custom_name);
        let ordered = if state.r#type() == Some(ChestType::Left) {
            [far, near]
        } else {
            [near, far]
        };
        let title = ordered
            .iter()
            .find_map(|(_, _, name)| name.clone())
            .unwrap_or_else(|| TextComponent::translate("container.chestDouble", []));
        let parts = ordered
            .iter()
            .map(|(pos, items, _)| (*pos, items.len()))
            .collect();
        let contents: Vec<Option<ItemStack>> = ordered
            .into_iter()
            .flat_map(|(_, items, _)| items)
            .collect();
        player.send(PlayerEvent::MenuOpen(MenuOpen {
            dimension: self.dimension,
            region: self.handle.clone(),
            kind: MenuKind::Generic {
                rows: (contents.len() / 9) as u8,
            },
            title,
            source: MenuSource::Blocks {
                parts,
                contents,
                data: Vec::new(),
                remote: Some((half.pos, half.region)),
            },
            stat: menu_stat(state.block()),
        }));
    }

    /// Whether the container at `pos` refuses `held` (its `lock` predicate
    /// does not admit it). Spectators bypass every lock.
    pub(super) fn container_locked(
        &self,
        pos: BlockPos,
        held: Option<&ItemStack>,
        spectator: bool,
    ) -> bool {
        if spectator {
            return false;
        }
        let Some(entity) = self.block_entity(pos) else {
            return false;
        };
        match entity
            .extra
            .get(block_entity::raw::LOCK)
            .and_then(Tag::as_compound)
        {
            Some(lock) => !self.shared.block_loot().lock_admits(lock, held),
            None => false,
        }
    }

    /// Whether the container at `pos` still has an unrolled loot table.
    pub(super) fn loot_pending(&self, pos: BlockPos) -> bool {
        self.block_entity(pos)
            .is_some_and(|entity| entity.extra.contains_key(block_entity::raw::LOOT_TABLE))
    }

    /// Rolls a pending `LootTable`/`LootTableSeed` into the container's
    /// items on first open, and removes both keys, as the game does.
    pub(super) fn unpack_container_loot(&mut self, pos: BlockPos, player: &PlayerHandle) {
        let Some((table, seed, slots)) = self.block_entity(pos).and_then(|entity| {
            let table = entity
                .extra
                .get_str(block_entity::raw::LOOT_TABLE)?
                .to_owned();
            let seed = entity.extra.get_long(block_entity::raw::LOOT_TABLE_SEED);
            Some((table, seed, entity.items.len()))
        }) else {
            return;
        };
        let loot = self.shared.block_loot();
        let items = match seed.filter(|seed| *seed != 0) {
            Some(seed) => {
                loot.container_loot(&table, slots, &mut StdRng::seed_from_u64(seed as u64))
            }
            None => loot.container_loot(&table, slots, &mut self.rng),
        };
        if let Some(items) = &items {
            self.explore_loot(super::exploring::ExploreTarget::Block(pos), pos, items);
        }
        let Some(entity) = self.block_entity_mut(pos) else {
            return;
        };
        entity.extra.remove(block_entity::raw::LOOT_TABLE);
        entity.extra.remove(block_entity::raw::LOOT_TABLE_SEED);
        if let Some(items) = items {
            entity.items = items;
        }
        player.send(PlayerEvent::Advancement(Box::new(
            AdvancementEvent::GeneratedLoot(table),
        )));
    }

    /// The overlay message and broadcast sound of trying to open a locked
    /// container.
    pub(super) fn notify_locked(
        &mut self,
        parts: &[BlockPos],
        place: &Place,
        title: TextComponent,
    ) {
        let position = match parts {
            [a, b] => {
                let (a, b) = (a.center(), b.center());
                [(a[0] + b[0]) / 2.0, a[1], (a[2] + b[2]) / 2.0]
            }
            _ => parts.first().copied().unwrap_or(place.target).center(),
        };
        let seed = sound_seed(position, self.sched.tick);
        let sound = packets([framed(&play::Sound {
            sound_event: SoundEvent::BlockChestLocked.id(),
            category: play::sound_category::BLOCKS,
            position,
            volume: 1.0,
            pitch: 1.0,
            seed,
        })]);
        self.send_sound_near(position, 1.0, None, sound);
        let message = TextComponent::translate("container.isLocked", [title]);
        place.player.send(PlayerEvent::Packets(packets([framed(
            &play::SetActionBarText(&message),
        )])));
    }

    pub(super) fn add_viewer(&mut self, pos: BlockPos, player: PlayerHandle) {
        let viewers = self.menus.viewers.entry(pos).or_default();
        if viewers.iter().any(|v| v.id == player.id) {
            return;
        }
        let previous = viewers.len();
        viewers.push(player);
        let mobs = self.mob_open_count(pos);
        self.open_count_changed(pos, previous + mobs, previous + mobs + 1);
    }

    pub(super) fn remove_viewer(&mut self, pos: BlockPos, player: PlayerId) {
        let Some(viewers) = self.menus.viewers.get_mut(&pos) else {
            return;
        };
        let previous = viewers.len();
        viewers.retain(|v| v.id != player);
        let current = viewers.len();
        if current == 0 {
            self.menus.viewers.remove(&pos);
        }
        if current != previous {
            let mobs = self.mob_open_count(pos);
            self.open_count_changed(pos, previous + mobs, current + mobs);
        }
    }

    /// Ends every menu over the container at `pos` (it is gone), and takes
    /// those players off the other half of a double chest.
    pub(super) fn close_viewers(&mut self, pos: BlockPos) {
        self.menus.container_locks.remove(&pos);
        self.menus.mob_openers.remove(&pos);
        let Some(viewers) = self.menus.viewers.remove(&pos) else {
            return;
        };
        let partner = self
            .block(pos)
            .and_then(|state| placement::chest_partner(state, pos));
        for viewer in viewers {
            viewer.send(PlayerEvent::ContainerClosed {
                dimension: self.dimension,
                pos,
            });
            if let Some(partner) = partner {
                self.remove_viewer(partner, viewer.id);
            }
        }
    }

    pub(super) fn mob_open_count(&self, pos: BlockPos) -> usize {
        self.menus.mob_openers.get(&pos).map_or(0, Vec::len)
    }

    /// `mob` opens or closes the container at `pos` and its other half.
    pub(super) fn mob_container_open(&mut self, mob: i32, pos: BlockPos, open: bool) {
        let Some(state) = self.block(pos) else {
            return;
        };
        let mut parts = vec![pos];
        parts.extend(placement::chest_partner(state, pos));
        parts.retain(|p| self.owns(*p));
        for part in parts {
            let players = self.menus.viewers.get(&part).map_or(0, Vec::len);
            let openers = self.menus.mob_openers.entry(part).or_default();
            let previous = players + openers.len();
            if open {
                if !openers.contains(&mob) {
                    openers.push(mob);
                }
            } else {
                openers.retain(|id| *id != mob);
            }
            let current = players + openers.len();
            if openers.is_empty() {
                self.menus.mob_openers.remove(&part);
            }
            if current != previous {
                self.open_count_changed(part, previous, current);
            }
        }
    }

    /// Closes the containers held open by mobs that are gone.
    pub(super) fn close_orphaned_mob_openers(&mut self) {
        if self.menus.mob_openers.is_empty() {
            return;
        }
        let gone: Vec<(BlockPos, i32)> = self
            .menus
            .mob_openers
            .iter()
            .flat_map(|(pos, ids)| ids.iter().map(move |id| (*pos, *id)))
            .filter(|(_, id)| !self.ents.entities.contains_key(id))
            .collect();
        for (pos, id) in gone {
            self.mob_container_open(id, pos, false);
        }
    }

    /// The lid and sound effects of the viewer count of `pos` changing.
    pub(super) fn open_count_changed(&mut self, pos: BlockPos, previous: usize, current: usize) {
        let Some(state) = self.block(pos) else {
            return;
        };
        let block = state.block();
        if previous != current && state.is_signal_source() {
            crate::world::redstone::RedstoneLevel::update_neighbors_at(self, pos, block);
            crate::world::redstone::RedstoneLevel::update_neighbors_at(
                self,
                BlockPos::new(pos.x, pos.y - 1, pos.z),
                block,
            );
        }
        let pitch = self.rng.r#gen::<f32>() * 0.1 + 0.9;
        let opened = previous == 0 && current > 0;
        let closed = previous > 0 && current == 0;
        match ContainerKind::of_block(block) {
            _ if block == Block::EnderChest => {
                self.block_event(pos, play::block_event::CONTAINER_OPEN_COUNT, current as u8);
                if opened {
                    self.play_sound(SoundEvent::BlockEnderChestOpen, pos, 0.5, pitch, None);
                } else if closed {
                    self.play_sound(SoundEvent::BlockEnderChestClose, pos, 0.5, pitch, None);
                }
            }
            Some(ContainerKind::Chest | ContainerKind::TrappedChest) => {
                self.block_event(pos, play::block_event::CONTAINER_OPEN_COUNT, current as u8);
                let sound = match (opened, closed) {
                    (true, _) => chest_sound(block, true),
                    (_, true) => chest_sound(block, false),
                    _ => return,
                };
                let kind = state.r#type();
                if kind == Some(ChestType::Left) {
                    return;
                }
                let mut position = pos.center();
                if kind == Some(ChestType::Right)
                    && let Some(direction) = placement::chest_connected_direction(state)
                {
                    let (dx, _, dz) = direction.step();
                    position[0] += dx as f64 * 0.5;
                    position[2] += dz as f64 * 0.5;
                }
                self.play_sound_at(sound, position, 0.5, pitch);
            }
            Some(ContainerKind::Barrel) => {
                let sound = match (opened, closed) {
                    (true, _) => SoundEvent::BlockBarrelOpen,
                    (_, true) => SoundEvent::BlockBarrelClose,
                    _ => return,
                };
                if let Some(open) = state.with_open(opened) {
                    self.put_block(pos, open);
                }
                let mut position = pos.center();
                if let Some(facing) = state.facing() {
                    let (dx, dy, dz) = facing.step();
                    position[0] += dx as f64 * 0.5;
                    position[1] += dy as f64 * 0.5;
                    position[2] += dz as f64 * 0.5;
                }
                self.play_sound_at(sound, position, 0.5, pitch);
            }
            Some(ContainerKind::ShulkerBox) => {
                self.block_event(pos, play::block_event::CONTAINER_OPEN_COUNT, current as u8);
                if opened {
                    if let Some(facing) = state.facing() {
                        self.push_shulkers_off_lid(pos, facing);
                    }
                    self.play_sound(SoundEvent::BlockShulkerBoxOpen, pos, 0.5, pitch, None);
                } else if closed {
                    self.play_sound(SoundEvent::BlockShulkerBoxClose, pos, 0.5, pitch, None);
                }
            }
            _ => {}
        }
    }

    pub(super) fn block_event(&self, pos: BlockPos, action: u8, parameter: u8) {
        let Some(state) = self.block(pos) else {
            return;
        };
        let packet = packets([framed(&play::BlockEvent {
            position: pos,
            action,
            parameter,
            block: state.block().id(),
        })]);
        self.send_chunk_packets(ChunkPos::of_block(pos), None, packet);
    }

    pub(super) fn play_sound_at(
        &self,
        sound: SoundEvent,
        position: [f64; 3],
        volume: f32,
        pitch: f32,
    ) {
        let seed = sound_seed(position, self.sched.tick);
        let packet = packets([framed(&play::Sound {
            sound_event: sound.id(),
            category: play::sound_category::BLOCKS,
            position,
            volume,
            pitch,
            seed,
        })]);
        self.send_sound_near(position, volume, None, packet);
    }

    /// Tells everyone viewing the container at `pos` about its changed
    /// slots, forgetting viewers who have left.
    pub(super) fn broadcast_slots(&mut self, pos: BlockPos, changed: &[usize]) {
        if changed.is_empty() || !self.menus.viewers.contains_key(&pos) {
            return;
        }
        let Some(items) = self.block_entity(pos).map(|entity| &entity.items) else {
            return;
        };
        let mut slots: Vec<(usize, Option<ItemSnapshot>)> = changed
            .iter()
            .filter_map(|&slot| Some((slot, items.get(slot)?.as_ref().map(ItemSnapshot::of))))
            .collect();
        if let Some(preview) = self.crafter_preview(pos) {
            slots.push((
                crate::world::menu::crafter::PREVIEW,
                preview.as_ref().map(ItemSnapshot::of),
            ));
            if let Some(preview) = preview {
                preview.burn(tuff_item::item::BurnReason::Template);
            }
        }
        let Some(viewers) = self.menus.viewers.get_mut(&pos) else {
            return;
        };
        let dimension = self.dimension;
        let previous = viewers.len();
        viewers.retain(|viewer| {
            viewer.send(PlayerEvent::ContainerSlots {
                dimension,
                pos,
                slots: slots.clone(),
            })
        });
        let current = viewers.len();
        if current == 0 {
            self.menus.viewers.remove(&pos);
        }
        if current != previous {
            let mobs = self.mob_open_count(pos);
            self.open_count_changed(pos, previous + mobs, current + mobs);
        }
    }

    /// Tells everyone viewing the container at `pos` about its changed
    /// data slots.
    pub(super) fn broadcast_data(&mut self, pos: BlockPos, data: Vec<(usize, i32)>) {
        if data.is_empty() {
            return;
        }
        let Some(viewers) = self.menus.viewers.get(&pos) else {
            return;
        };
        let dimension = self.dimension;
        for viewer in viewers {
            viewer.send(PlayerEvent::ContainerData {
                dimension,
                pos,
                data: data.clone(),
            });
        }
    }

    /// Sets one slot of a block container, restarting a furnace whose
    /// input became a different item.
    pub(super) fn set_container_slot(
        &mut self,
        pos: BlockPos,
        slot: usize,
        stack: Option<ItemStack>,
    ) -> Option<ItemStack> {
        let Some(entity) = self.block_entity(pos) else {
            return stack;
        };
        let kind = entity.kind;
        let same = match (&entity.items[slot], &stack) {
            (Some(old), Some(new)) => old.stacks_with(new),
            _ => false,
        };
        let displaced = self
            .write_container_slot(pos, slot, stack)
            .unwrap_or_else(|stack| stack);
        if kind.is_furnace() && slot == furnace::INPUT && !same {
            self.furnace_input_changed(pos);
        }
        displaced
    }

    /// Restarts the furnace at `pos` after a new kind of input arrived.
    pub(super) fn furnace_input_changed(&mut self, pos: BlockPos) {
        let recipes = self.shared.recipes();
        if let Some(entity) = self.block_entity_mut(pos)
            && let Some(kind) = Some(entity.kind).filter(|kind| kind.is_furnace())
        {
            let items = std::mem::take(&mut entity.items);
            let furnace = entity.furnace_mut().expect("a furnace");
            recipes.furnace_input_changed(kind, &items, furnace);
            let data = furnace.data();
            entity.items = items;
            self.broadcast_data(
                pos,
                vec![
                    (
                        furnace::DATA_COOKING_PROGRESS,
                        data[furnace::DATA_COOKING_PROGRESS],
                    ),
                    (
                        furnace::DATA_COOKING_TOTAL,
                        data[furnace::DATA_COOKING_TOTAL],
                    ),
                ],
            );
        }
    }

    /// Spawns the experience the furnace's cooked recipes earned at
    /// `position` and returns them for the recipe book.
    pub(super) fn pop_furnace_experience(
        &mut self,
        used: &std::collections::BTreeMap<String, i32>,
        position: [f64; 3],
    ) -> Vec<tuff_vanilla_data::Frozen<str>> {
        let mut awarded = Vec::new();
        for (name, count) in used {
            let Some(name) = self.shared.recipes().resolve(name) else {
                continue;
            };
            let Some(experience) = self.shared.recipes().cooking_experience(&name) else {
                continue;
            };
            awarded.push(name);
            let amount = experience::cooking_reward(*count, experience, &mut self.rng);
            self.spawn_experience(position, amount);
        }
        awarded
    }

    pub(super) fn container_transaction(
        &mut self,
        player: PlayerHandle,
        window_id: i32,
        changes: Vec<ContainerChange>,
        took_result: bool,
    ) {
        let valid = self.transaction_valid(player.id, &changes);
        if !valid {
            let contents = self.contents_of(&changes);
            player.send(PlayerEvent::Transaction {
                window_id,
                accepted: false,
                contents,
            });
            return;
        }
        player.send(PlayerEvent::Transaction {
            window_id,
            accepted: true,
            contents: Vec::new(),
        });
        let positions = self.apply_container_changes(changes);
        if !took_result {
            return;
        }
        for pos in positions {
            let Some(used) = self
                .block_entity_mut(pos)
                .and_then(|entity| entity.furnace_mut())
                .map(|furnace| std::mem::take(&mut furnace.recipes_used))
            else {
                continue;
            };
            let at = self
                .ents
                .presence
                .get(&player.id)
                .map_or_else(|| pos.center(), |p| p.position);
            let awarded = self.pop_furnace_experience(&used, at);
            if !awarded.is_empty() {
                player.send(PlayerEvent::RecipesAwarded(awarded));
            }
        }
    }

    /// Whether `player` views every container `changes` touch, none of them
    /// is locked, every slot holds what the change expects, and nothing
    /// goes into a disabled crafter slot.
    fn transaction_valid(&self, player: PlayerId, changes: &[ContainerChange]) -> bool {
        changes.iter().all(|change| {
            !self.menus.container_locks.contains_key(&change.pos)
                && !(change.new.is_some() && self.crafter_slot_disabled(change.pos, change.slot))
                && self
                    .menus
                    .viewers
                    .get(&change.pos)
                    .is_some_and(|viewers| viewers.iter().any(|v| v.id == player))
                && self
                    .block_entity(change.pos)
                    .and_then(|entity| entity.items.get(change.slot))
                    .is_some_and(|current| *current == change.expected)
        })
    }

    fn crafter_slot_disabled(&self, pos: BlockPos, slot: usize) -> bool {
        self.block_entity(pos)
            .and_then(|entity| entity.crafter())
            .is_some_and(|crafter| slot < 16 && crafter.disabled & (1 << slot) != 0)
    }

    /// The current contents of every container `changes` touch.
    fn contents_of(&self, changes: &[ContainerChange]) -> Vec<(BlockPos, Vec<Option<ItemStack>>)> {
        let mut positions: Vec<BlockPos> = changes.iter().map(|c| c.pos).collect();
        positions.sort_by_key(|p| (p.x, p.y, p.z));
        positions.dedup();
        positions
            .into_iter()
            .filter_map(|pos| Some((pos, self.block_entity(pos)?.items.dup_unchecked())))
            .collect()
    }

    /// Applies `changes` and tells the viewers, returning the positions
    /// touched in order. A change to a container that is gone drops what
    /// it would have added.
    fn apply_container_changes(&mut self, changes: Vec<ContainerChange>) -> Vec<BlockPos> {
        let mut by_pos: HashMap<BlockPos, Vec<usize>> = HashMap::new();
        let mut positions = Vec::new();
        for change in changes {
            if self.block_entity(change.pos).is_none() {
                let gained = match (&change.expected, &change.new) {
                    (Some(old), Some(new)) if old.stacks_with(new) => (new.count() > old.count())
                        .then(|| new.dup_count_unchecked(new.count() - old.count())),
                    (_, new) => new.dup_unchecked(),
                };
                if let Some(stack) = gained {
                    self.spawn_item_at(
                        stack,
                        change.pos.center(),
                        [0.0; 3],
                        items::BLOCK_DROP_PICKUP_DELAY,
                    );
                }
                continue;
            }
            self.set_container_slot(change.pos, change.slot, change.new.dup_unchecked());
            if !positions.contains(&change.pos) {
                positions.push(change.pos);
            }
            by_pos.entry(change.pos).or_default().push(change.slot);
        }
        for (pos, slots) in by_pos {
            self.broadcast_slots(pos, &slots);
        }
        positions
    }

    /// The first phase of a transaction over both halves of a double chest
    /// split between regions: validate this region's share and lock it.
    pub(super) fn prepare_transaction(
        &mut self,
        player: PlayerHandle,
        window_id: i32,
        txn: u64,
        changes: Vec<ContainerChange>,
    ) {
        let prepared = self.transaction_valid(player.id, &changes);
        let contents = self.contents_of(&changes);
        if prepared {
            for change in &changes {
                self.menus
                    .container_locks
                    .insert(change.pos, (player.id, txn));
            }
            self.menus.prepared.insert((player.id, txn), changes);
        }
        player.send(PlayerEvent::TransactionPrepared {
            window_id,
            txn,
            prepared,
            contents,
        });
    }

    fn spill_orphans(&mut self, orphans: Vec<(BlockPos, Vec<Option<ItemStack>>)>) {
        for (pos, items) in orphans {
            let drops = block_entity::scatter(pos, items.into_iter().flatten(), &mut self.rng);
            for (stack, position, velocity) in drops {
                self.spawn_item_at(stack, position, velocity, items::BLOCK_DROP_PICKUP_DELAY);
            }
        }
    }

    /// The second phase: applies or drops a prepared share and unlocks it.
    /// Containers broken in between spill their contents with the share
    /// applied (or not), so a commit neither duplicates nor loses items.
    pub(super) fn resolve_transaction(&mut self, player: PlayerId, txn: u64, commit: bool) {
        let Some(changes) = self.menus.prepared.remove(&(player, txn)) else {
            return;
        };
        self.menus
            .container_locks
            .retain(|_, lock| *lock != (player, txn));
        let mut orphans = self
            .menus
            .orphans
            .remove(&(player, txn))
            .unwrap_or_default();
        let mut live = Vec::new();
        for change in changes {
            match orphans.iter_mut().find(|(pos, _)| *pos == change.pos) {
                Some((_, items)) if commit => {
                    if let Some(slot) = items.get_mut(change.slot) {
                        *slot = change.new;
                    }
                }
                Some(_) => {}
                None => live.push(change),
            }
        }
        if commit {
            self.apply_container_changes(live);
        }
        self.spill_orphans(orphans);
    }

    /// Drops every prepared share of `player` (its menu closed).
    pub(super) fn drop_prepared(&mut self, player: PlayerId) {
        self.menus.prepared.retain(|(owner, _), _| *owner != player);
        let keys: Vec<_> = self
            .menus
            .orphans
            .keys()
            .filter(|(owner, _)| *owner == player)
            .copied()
            .collect();
        for key in keys {
            if let Some(orphans) = self.menus.orphans.remove(&key) {
                self.spill_orphans(orphans);
            }
        }
        self.menus
            .container_locks
            .retain(|_, (owner, _)| *owner != player);
    }

    /// Sends a viewer every slot of the container at `pos`.
    pub(super) fn refresh_container(&self, player: &PlayerHandle, pos: BlockPos) {
        let viewing = self
            .menus
            .viewers
            .get(&pos)
            .is_some_and(|viewers| viewers.iter().any(|v| v.id == player.id));
        let Some(entity) = self.block_entity(pos).filter(|_| viewing) else {
            return;
        };
        player.send(PlayerEvent::ContainerSlots {
            dimension: self.dimension,
            pos,
            slots: entity
                .items
                .iter()
                .map(|stack| stack.as_ref().map(ItemSnapshot::of))
                .enumerate()
                .collect(),
        });
    }
}

/// The lid sound of a chest block: copper chests sound by weathering.
fn chest_sound(block: Block, open: bool) -> SoundEvent {
    match block {
        Block::WeatheredCopperChest | Block::WaxedWeatheredCopperChest => {
            if open {
                SoundEvent::BlockCopperChestWeatheredOpen
            } else {
                SoundEvent::BlockCopperChestWeatheredClose
            }
        }
        Block::OxidizedCopperChest | Block::WaxedOxidizedCopperChest => {
            if open {
                SoundEvent::BlockCopperChestOxidizedOpen
            } else {
                SoundEvent::BlockCopperChestOxidizedClose
            }
        }
        _ if tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::COPPER_CHESTS, block) => {
            if open {
                SoundEvent::BlockCopperChestOpen
            } else {
                SoundEvent::BlockCopperChestClose
            }
        }
        _ if open => SoundEvent::BlockChestOpen,
        _ => SoundEvent::BlockChestClose,
    }
}

pub(super) fn loot_generated(table: String) -> PlayerEvent {
    PlayerEvent::Advancement(Box::new(
        crate::world::messages::AdvancementEvent::GeneratedContainerLoot(table),
    ))
}

pub(super) fn spectator_cant_open(player: &PlayerHandle) {
    let message = TextComponent::translate("container.spectatorCantOpen", []).color("red");
    player.send(PlayerEvent::Packets(packets([framed(
        &play::SetActionBarText(&message),
    )])));
}
mod hoppers;

/// A sound's variation seed from where and when it plays, so a replayed
/// tick sends the same packets.
fn sound_seed(position: [f64; 3], tick: u64) -> i64 {
    let mut hash = tick.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for axis in position {
        hash = (hash ^ axis.to_bits()).wrapping_mul(0xD1B5_4A32_D192_ED03);
    }
    (hash ^ (hash >> 29)) as i64
}
