use super::*;
use tuff_vanilla_data::registry::EntityType;

/// Region entities and what is tracked about them.
#[derive(Default)]
pub(super) struct EntityState {
    /// Every entity this region owns but players, by entity id.
    pub(super) entities: EntityMap,
    /// Gaps blocks grew into (with their new top) while the ticking entity
    /// was out of `entities`, so it can be lifted too.
    pub(super) lifts: Option<Vec<([f64; 6], f64)>>,
    /// Player rider to vehicle, built once per entity tick and dropped on any
    /// mount or entity insert that could add a rider.
    pub(super) rider_vehicles: Option<HashMap<PlayerId, i32>>,
    /// When each player last collected an orb, for the pickup delay.
    pub(super) orb_pickups: HashMap<PlayerId, u64>,
    /// The players this region's entities see, as of a tick: those
    /// present, and those the player index lists near this region.
    pub(super) seen: Option<(u64, Arc<[entities::Seen]>)>,
    /// The crack stage last shown to others for each player mining here.
    pub(super) crack_stages: HashMap<PlayerId, (BlockPos, i32)>,
    /// Entities made of separately hit parts (the dragon), by id.
    pub(super) part_owners: HashSet<i32>,
    /// Entities that asked to get on (`Some(vehicle)`) or off (`None`)
    /// something during their tick, handled once they are back in the map.
    /// Mergeable items by block column, built on the first merge lookup of a tick.
    pub(super) item_grid: Option<HashMap<(i32, i32), Vec<i32>>>,
    pub(super) mount_requests: Vec<(i32, Option<i32>)>,
    /// Mount links found broken last tick (`true` = a rider's vehicle link),
    /// pruned only when still broken, as a vehicle and its riders cross a
    /// border as separate messages that may land in different ticks.
    pub(super) broken_links: HashSet<(i32, bool)>,
    /// Stacks taken by the entity ticking now, handed over once it is back.
    pub(super) taken_stacks: Vec<(i32, tuff_item::item::ItemStack)>,
    /// Blocks taken up for, or handed back to, the entity ticking now.
    pub(super) taken_blocks: Vec<(i32, crate::world::block_ledger::HeldBlock)>,
    /// Boss bars currently shown, by the entity id publishing one: who has
    /// been sent it and the progress they were last sent (see
    /// `entities::update_boss_bar`).
    pub(super) boss_bars: HashMap<i32, boss_bar::Shown>,
    pub(super) exploring: Vec<exploring::Exploring>,
    pub(super) presence: HashMap<PlayerId, Presence>,
    /// Outstanding shoulder offers, by the entity offered.
    pub(super) shoulder_offers: HashMap<i32, crate::world::entity::Reservation>,
    /// UUIDs of entities destroyed since the last flush, whose scores and team go.
    pub(super) destroyed: Vec<u128>,
    /// Features' per-region state.
    pub(super) feature_state: crate::world::region_api::RegionState,
}

impl EntityState {
    /// The entity whose part clients know by `id`, and which part.
    pub(super) fn part_owner(&self, id: i32) -> Option<(i32, usize)> {
        self.part_owners.iter().find_map(|owner| {
            let part = id.checked_sub(owner + 1)?;
            let count = self.entities.get(owner)?.entity.parts().len() as i32;
            (0..count)
                .contains(&part)
                .then_some((*owner, part as usize))
        })
    }

    /// Everyone riding `vehicle`, directly or through other mounts here.
    pub(super) fn indirect_passengers(&self, vehicle: i32) -> Vec<i32> {
        let mut found = Vec::new();
        let mut pending = vec![vehicle];
        while let Some(id) = pending.pop() {
            let Some(tracked) = self.entities.get(&id) else {
                continue;
            };
            for rider in crate::world::entity::all_passengers(tracked.entity.base()) {
                if !found.contains(&rider) {
                    found.push(rider);
                    pending.push(rider);
                }
            }
        }
        found
    }

    /// Where each passenger of `vehicle` present here sits now.
    pub(super) fn seats_of(&self, vehicle: &TrackedEntity) -> Vec<(i32, [f64; 3])> {
        let base = vehicle.entity.base();
        base.entity_riders
            .iter()
            .filter_map(|rider| {
                let passenger = self.entities.get(rider)?;
                let (index, count) = base.seat_index(*rider);
                Some((
                    *rider,
                    passengers::seat_position(&*vehicle.entity, &*passenger.entity, index, count),
                ))
            })
            .collect()
    }

    /// The entity of this region that `passenger` (an entity or a player)
    /// rides.
    pub(super) fn vehicle_holding(&self, passenger: i32) -> Option<i32> {
        if let Some(tracked) = self.entities.get(&passenger) {
            return tracked
                .entity
                .base()
                .vehicle
                .as_ref()
                .map(|mount| mount.id)
                .filter(|id| self.entities.contains_key(id));
        }
        self.entities.iter().find_map(|(id, tracked)| {
            tracked
                .entity
                .base()
                .riders
                .iter()
                .any(|rider| rider.id == passenger)
                .then_some(*id)
        })
    }

    /// Whether `passenger` is somewhere below `vehicle` in its stack.
    pub(super) fn carried_by(&self, vehicle: i32, passenger: i32) -> bool {
        let mut id = vehicle;
        for _ in 0..passengers::MAX_STACK {
            if id == passenger {
                return true;
            }
            match self
                .entities
                .get(&id)
                .and_then(|t| t.entity.base().vehicle.as_ref())
            {
                Some(mount) => id = mount.id,
                None => return false,
            }
        }
        false
    }
}

/// The region's entities by id, with the ids of each kind kept sorted so
/// rare kinds (game event listeners) are found without a full scan.
#[derive(Default)]
pub(super) struct EntityMap {
    map: HashMap<i32, TrackedEntity>,
    by_kind: HashMap<EntityType, std::collections::BTreeSet<i32>>,
}

impl std::ops::Deref for EntityMap {
    type Target = HashMap<i32, TrackedEntity>;

    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

impl EntityMap {
    fn unindex(&mut self, id: i32, kind: EntityType) {
        if let Some(ids) = self.by_kind.get_mut(&kind) {
            ids.remove(&id);
            if ids.is_empty() {
                self.by_kind.remove(&kind);
            }
        }
    }

    pub(super) fn insert(&mut self, id: i32, tracked: TrackedEntity) -> Option<TrackedEntity> {
        self.by_kind
            .entry(tracked.entity.base().kind)
            .or_default()
            .insert(id);
        let old = self.map.insert(id, tracked);
        if let Some(old) = &old {
            let (old_kind, new_kind) = (old.entity.base().kind, self.map[&id].entity.base().kind);
            if old_kind != new_kind {
                self.unindex(id, old_kind);
            }
        }
        old
    }

    pub(super) fn remove(&mut self, id: &i32) -> Option<TrackedEntity> {
        let old = self.map.remove(id)?;
        self.unindex(*id, old.entity.base().kind);
        Some(old)
    }

    pub(super) fn get_or_insert(&mut self, id: i32, tracked: TrackedEntity) -> &mut TrackedEntity {
        if !self.map.contains_key(&id) {
            self.insert(id, tracked);
        }
        self.map.get_mut(&id).expect("just inserted")
    }

    pub(super) fn retain(&mut self, mut keep: impl FnMut(&i32, &mut TrackedEntity) -> bool) {
        let by_kind = &mut self.by_kind;
        self.map.retain(|id, tracked| {
            let kept = keep(id, tracked);
            if !kept && let Some(ids) = by_kind.get_mut(&tracked.entity.base().kind) {
                ids.remove(id);
            }
            kept
        });
        by_kind.retain(|_, ids| !ids.is_empty());
    }

    pub(super) fn get_mut(&mut self, id: &i32) -> Option<&mut TrackedEntity> {
        self.map.get_mut(id)
    }

    pub(super) fn values_mut(
        &mut self,
    ) -> std::collections::hash_map::ValuesMut<'_, i32, TrackedEntity> {
        self.map.values_mut()
    }

    pub(super) fn iter_mut(
        &mut self,
    ) -> std::collections::hash_map::IterMut<'_, i32, TrackedEntity> {
        self.map.iter_mut()
    }

    /// The ids of entities of `kind`, ascending.
    pub(super) fn ids_of(&self, kind: EntityType) -> impl Iterator<Item = i32> + '_ {
        self.by_kind.get(&kind).into_iter().flatten().copied()
    }
}

impl<'a> IntoIterator for &'a mut EntityMap {
    type Item = (&'a i32, &'a mut TrackedEntity);
    type IntoIter = std::collections::hash_map::IterMut<'a, i32, TrackedEntity>;

    fn into_iter(self) -> Self::IntoIter {
        self.map.iter_mut()
    }
}

impl<'a> IntoIterator for &'a EntityMap {
    type Item = (&'a i32, &'a TrackedEntity);
    type IntoIter = std::collections::hash_map::Iter<'a, i32, TrackedEntity>;

    fn into_iter(self) -> Self::IntoIter {
        self.map.iter()
    }
}
