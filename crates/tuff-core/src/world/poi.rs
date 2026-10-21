//! Points of interest: beds, workstations and bells that villagers claim.
//! A region keeps the points of interest of the chunks it holds, each with
//! its free tickets, found from their blocks when a chunk arrives (keeping
//! the tickets saved with it) and kept current as blocks change. Entities
//! take and release tickets as their memories of places change; a ticket on
//! another region's point travels there by message. Each region also keeps
//! a read-only mirror of its neighbours' points near the shared border, so
//! searches see across it.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::PointOfInterestType;

use tuff_world_model::BlockPos;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::poi::Record;
use tuff_world_model::pos::{ChunkPos, REGION_CHUNKS, RegionPos};

/// Bits by block state id: whether the state makes a point of interest.
fn poi_bits() -> &'static [u64] {
    static BITS: OnceLock<Vec<u64>> = OnceLock::new();
    BITS.get_or_init(|| {
        let states = PointOfInterestType::states();
        let max = states.last().map_or(0, |(id, _)| *id as usize);
        let mut bits = vec![0u64; max / 64 + 1];
        for (id, _) in states {
            bits[*id as usize / 64] |= 1 << (id % 64);
        }
        bits
    })
}

fn may_be_poi(state: BlockState) -> bool {
    let id = state.id() as usize;
    poi_bits()
        .get(id / 64)
        .is_some_and(|word| word & (1 << (id % 64)) != 0)
}

/// The points of interest in a chunk's blocks.
pub fn scan(pos: ChunkPos, chunk: &Chunk) -> Vec<(BlockPos, PointOfInterestType)> {
    let mut found = Vec::new();
    let shape = chunk.shape();
    for (index, section) in chunk.sections().enumerate() {
        let base = shape.section_base(index);
        let mut push = |i: usize, state: BlockState| {
            if let Some(kind) = PointOfInterestType::of_state(state) {
                let at = BlockPos::new(
                    pos.x * 16 + (i & 15) as i32,
                    base + (i >> 8) as i32,
                    pos.z * 16 + ((i >> 4) & 15) as i32,
                );
                found.push((at, kind));
            }
        };
        if !section.palette().iter().any(|s| may_be_poi(*s)) {
            continue;
        }
        for i in 0..4096 {
            let state = section.get(i);
            if may_be_poi(state) {
                push(i, state);
            }
        }
    }
    found
}

/// A claim taken on or given back to a point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ticket {
    Take,
    Release,
}

fn apply(record: &mut Record, ticket: Ticket) -> bool {
    match ticket {
        Ticket::Take if record.free > 0 => record.free -= 1,
        Ticket::Release if record.free < record.kind.info().max_tickets => record.free += 1,
        _ => return false,
    }
    true
}

/// The records of the points `found` in a chunk's blocks, keeping the
/// tickets `saved` with the chunk where the same point is still there, and
/// whether they differ from what was saved.
pub fn merge(
    found: Vec<(BlockPos, PointOfInterestType)>,
    saved: Option<&[Record]>,
) -> (Vec<Record>, bool) {
    let saved = saved.unwrap_or_default();
    let records: Vec<Record> = found
        .into_iter()
        .map(|(pos, kind)| {
            saved
                .iter()
                .find(|r| r.pos == pos && r.kind == kind)
                .map(|r| Record {
                    free: r.free.min(kind.info().max_tickets),
                    ..*r
                })
                .unwrap_or_else(|| Record::new(pos, kind))
        })
        .collect();
    let changed = records.len() != saved.len() || records.iter().any(|r| !saved.contains(r));
    (records, changed)
}

/// [`PoiStore::sections_to_village`] when no village is near.
pub const NO_VILLAGE: i32 = 7;

/// How many chunks past a region's edge its neighbours' points are mirrored
/// into it: the widest search (`sections_to_village`) reaches this far.
pub const MIRROR_CHUNKS: i32 = NO_VILLAGE - 1;

/// Whether `chunk` lies outside `region` but within [`MIRROR_CHUNKS`] of
/// it, so `region` keeps a mirror of its points.
pub fn mirrored_in(region: RegionPos, chunk: ChunkPos) -> bool {
    if region.contains(chunk) {
        return false;
    }
    let gap = |c: i32, low: i32| (low - c).max(c - (low + REGION_CHUNKS - 1)).max(0);
    let dx = gap(chunk.x, region.x * REGION_CHUNKS);
    let dz = gap(chunk.z, region.z * REGION_CHUNKS);
    dx.max(dz) <= MIRROR_CHUNKS
}

/// Which points a search accepts by how claimed they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occupancy {
    HasSpace,
    IsOccupied,
    Any,
}

/// Pending ticket changes kept per unloaded chunk, at most.
const MAX_PENDING: usize = 256;

#[derive(Debug, Default)]
pub struct PoiStore {
    /// The region owning this store; `None` treats every chunk as its own.
    region: Option<RegionPos>,
    own: HashMap<ChunkPos, Vec<Record>>,
    /// Neighbours' records near the border, as they last sent them.
    mirror: HashMap<ChunkPos, Vec<Record>>,
    /// The places each entity holds a ticket on, as last counted.
    holders: HashMap<i32, Vec<BlockPos>>,
    /// Ticket changes for points in own chunks that are not loaded, applied
    /// when they load.
    pending: HashMap<ChunkPos, Vec<(BlockPos, Ticket)>>,
    /// Ticket changes for other regions' points, to send them.
    outbox: Vec<(BlockPos, Ticket)>,
    unsaved: HashSet<ChunkPos>,
    unmirrored: HashSet<ChunkPos>,
    /// Own chunks whose points were never saved with their tickets (new,
    /// or saved without tickets): an entity arriving with a claim on
    /// one takes its ticket, as the claim was never counted.
    uncounted: HashSet<ChunkPos>,
}

impl PoiStore {
    pub fn for_region(region: RegionPos) -> Self {
        Self {
            region: Some(region),
            ..Self::default()
        }
    }

    fn owns(&self, chunk: ChunkPos) -> bool {
        self.region.is_none_or(|region| region.contains(chunk))
    }

    /// A loaded chunk's points; tickets changed while it was away apply now.
    pub fn install(&mut self, chunk: ChunkPos, mut records: Vec<Record>) {
        if let Some(changes) = self.pending.remove(&chunk) {
            for (pos, ticket) in changes {
                if let Some(record) = records.iter_mut().find(|r| r.pos == pos) {
                    apply(record, ticket);
                }
            }
            self.unsaved.insert(chunk);
        }
        if records.is_empty() {
            self.own.remove(&chunk);
        } else {
            self.own.insert(chunk, records);
        }
        self.unmirrored.insert(chunk);
    }

    /// The chunk's points were found in its blocks with nothing saved.
    pub fn mark_uncounted(&mut self, chunk: ChunkPos) {
        self.uncounted.insert(chunk);
    }

    /// The chunk's points and their tickets were saved.
    pub fn saved(&mut self, chunk: ChunkPos) {
        self.uncounted.remove(&chunk);
    }

    pub fn unload(&mut self, chunk: ChunkPos) {
        self.own.remove(&chunk);
        self.unsaved.remove(&chunk);
        self.uncounted.remove(&chunk);
        self.unmirrored.insert(chunk);
    }

    pub fn is_empty(&self) -> bool {
        self.own.is_empty() && self.mirror.is_empty()
    }

    /// A block changed in a loaded chunk: its point of interest goes or
    /// comes with it, a new one unclaimed.
    pub fn block_changed(&mut self, pos: BlockPos, state: BlockState) {
        let chunk = ChunkPos::of_block(pos);
        let kind = PointOfInterestType::of_state(state);
        let list = self.own.entry(chunk).or_default();
        let existing = list.iter().position(|r| r.pos == pos);
        match (existing, kind) {
            (Some(index), Some(kind)) if list[index].kind == kind => return,
            (Some(index), Some(kind)) => list[index] = Record::new(pos, kind),
            (Some(index), None) => {
                list.swap_remove(index);
            }
            (None, Some(kind)) => list.push(Record::new(pos, kind)),
            (None, None) => {}
        }
        if list.is_empty() {
            self.own.remove(&chunk);
        }
        self.unsaved.insert(chunk);
        self.unmirrored.insert(chunk);
    }

    fn chunk_records(&self, chunk: ChunkPos) -> Option<&[Record]> {
        self.own
            .get(&chunk)
            .or_else(|| self.mirror.get(&chunk))
            .map(Vec::as_slice)
    }

    pub fn record(&self, pos: BlockPos) -> Option<Record> {
        self.chunk_records(ChunkPos::of_block(pos))?
            .iter()
            .find(|r| r.pos == pos)
            .copied()
    }

    pub fn kind_at(&self, pos: BlockPos) -> Option<PointOfInterestType> {
        self.record(pos).map(|r| r.kind)
    }

    pub fn claims(&self, pos: BlockPos) -> u32 {
        self.record(pos).map_or(0, |r| r.claims())
    }

    pub fn has_space(&self, pos: BlockPos) -> bool {
        self.record(pos).is_some_and(|r| r.free > 0)
    }

    /// This region's own records of a chunk, as saved.
    pub fn records(&self, chunk: ChunkPos) -> &[Record] {
        self.own.get(&chunk).map_or(&[], Vec::as_slice)
    }

    /// Every own chunk with points.
    pub fn own_chunks(&self) -> impl Iterator<Item = ChunkPos> + '_ {
        self.own.keys().copied()
    }

    fn occupancy_accepts(occupancy: Occupancy, record: &Record) -> bool {
        match occupancy {
            Occupancy::HasSpace => record.free > 0,
            Occupancy::IsOccupied => record.claims() > 0,
            Occupancy::Any => true,
        }
    }

    /// Points of the accepted kinds within `radius` blocks of `center`
    /// (block distance), closest first.
    pub fn closest_first(
        &self,
        center: BlockPos,
        radius: i32,
        occupancy: Occupancy,
        accept: impl Fn(PointOfInterestType) -> bool,
    ) -> Vec<(BlockPos, PointOfInterestType)> {
        let chunk_radius = radius.div_euclid(16) + 1;
        let middle = ChunkPos::of_block(center);
        let limit = (radius as i64) * (radius as i64);
        let mut found: Vec<(i64, BlockPos, PointOfInterestType)> = Vec::new();
        for x in middle.x - chunk_radius..=middle.x + chunk_radius {
            for z in middle.z - chunk_radius..=middle.z + chunk_radius {
                let Some(list) = self.chunk_records(ChunkPos::new(x, z)) else {
                    continue;
                };
                for record in list {
                    let pos = record.pos;
                    if (pos.x - center.x).abs() > radius || (pos.z - center.z).abs() > radius {
                        continue;
                    }
                    let d = |a: i32, b: i32| ((a - b) as i64).pow(2);
                    let distance = d(pos.x, center.x) + d(pos.y, center.y) + d(pos.z, center.z);
                    if distance <= limit
                        && accept(record.kind)
                        && Self::occupancy_accepts(occupancy, record)
                    {
                        found.push((distance, pos, record.kind));
                    }
                }
            }
        }
        found.sort_by_key(|(distance, pos, _)| (*distance, pos.x, pos.y, pos.z));
        found
            .into_iter()
            .map(|(_, pos, kind)| (pos, kind))
            .collect()
    }

    /// Points of `kind` whose x and z lie within `radius` of `center`, at
    /// any height, closest first.
    pub fn in_square(
        &self,
        center: BlockPos,
        radius: i32,
        kind: PointOfInterestType,
    ) -> Vec<BlockPos> {
        let chunk_radius = radius.div_euclid(16) + 1;
        let middle = ChunkPos::of_block(center);
        let mut found: Vec<(i64, BlockPos)> = Vec::new();
        for x in middle.x - chunk_radius..=middle.x + chunk_radius {
            for z in middle.z - chunk_radius..=middle.z + chunk_radius {
                let Some(list) = self.chunk_records(ChunkPos::new(x, z)) else {
                    continue;
                };
                for record in list {
                    let pos = record.pos;
                    if record.kind != kind
                        || (pos.x - center.x).abs() > radius
                        || (pos.z - center.z).abs() > radius
                    {
                        continue;
                    }
                    let d = |a: i32, b: i32| ((a - b) as i64).pow(2);
                    let distance = d(pos.x, center.x) + d(pos.y, center.y) + d(pos.z, center.z);
                    found.push((distance, pos));
                }
            }
        }
        found.sort_by_key(|(distance, pos)| (*distance, pos.x, pos.y, pos.z));
        found.into_iter().map(|(_, pos)| pos).collect()
    }

    /// How many sections (counting diagonals) the section holding `pos` is
    /// from one with an occupied village point of interest, up to
    /// [`NO_VILLAGE`] for none within reach.
    pub fn sections_to_village(&self, pos: BlockPos) -> i32 {
        let section = |p: BlockPos| [p.x >> 4, p.y >> 4, p.z >> 4];
        let at = section(pos);
        let reach = NO_VILLAGE - 1;
        let mut best = NO_VILLAGE;
        for x in at[0] - reach..=at[0] + reach {
            for z in at[2] - reach..=at[2] + reach {
                let Some(list) = self.chunk_records(ChunkPos::new(x, z)) else {
                    continue;
                };
                for record in list {
                    if record.claims() == 0
                        || !tuff_datapack::tags::is(
                            tuff_vanilla_data::tag::point_of_interest_type::key::VILLAGE,
                            record.kind,
                        )
                    {
                        continue;
                    }
                    let other = section(record.pos);
                    let distance = (0..3).map(|i| (other[i] - at[i]).abs()).max().unwrap_or(0);
                    best = best.min(distance);
                }
            }
        }
        best
    }

    /// Takes or gives back a ticket on the point at `pos`, wherever it is.
    /// A point another region owns changes in the mirror at once, and the
    /// change waits in the outbox for that region.
    pub fn ticket(&mut self, pos: BlockPos, ticket: Ticket) -> bool {
        let chunk = ChunkPos::of_block(pos);
        if !self.owns(chunk) {
            self.outbox.push((pos, ticket));
            return self
                .mirror
                .get_mut(&chunk)
                .and_then(|list| list.iter_mut().find(|r| r.pos == pos))
                .is_some_and(|record| apply(record, ticket));
        }
        let Some(list) = self.own.get_mut(&chunk) else {
            if self.region.is_some() {
                let pending = self.pending.entry(chunk).or_default();
                if pending.len() < MAX_PENDING {
                    pending.push((pos, ticket));
                }
            }
            return false;
        };
        let changed = list
            .iter_mut()
            .find(|r| r.pos == pos)
            .is_some_and(|record| apply(record, ticket));
        if changed {
            self.unsaved.insert(chunk);
            self.unmirrored.insert(chunk);
        }
        changed
    }

    /// Ticket changes another region sent for this one's points. Changes
    /// for chunks not loaded wait for them.
    pub fn apply_tickets(&mut self, changes: Vec<(BlockPos, Ticket)>) {
        for (pos, ticket) in changes {
            if self.owns(ChunkPos::of_block(pos)) {
                self.ticket(pos, ticket);
            }
        }
    }

    /// An entity as a region finds it at the start of a tick: one it did
    /// not count yet (loaded, summoned, handed over) holds what it claims
    /// without taking anything, since its tickets were taken where it
    /// first claimed them, except on points never saved with tickets;
    /// otherwise as [`Self::hold`].
    pub fn arrive(&mut self, entity: i32, claims: Vec<BlockPos>) {
        if self.holders.contains_key(&entity) {
            self.hold(entity, claims);
        } else if !claims.is_empty() {
            for pos in &claims {
                if self.uncounted.contains(&ChunkPos::of_block(*pos)) {
                    self.ticket(*pos, Ticket::Take);
                }
            }
            self.holders.insert(entity, claims);
        }
    }

    /// Counts what an entity claims now against what it held: tickets are
    /// given back for places it let go and taken on new ones.
    pub fn hold(&mut self, entity: i32, claims: Vec<BlockPos>) {
        let before = self.holders.get(&entity).map_or(&[][..], Vec::as_slice);
        if before == claims.as_slice() {
            return;
        }
        let mut gone = before.to_vec();
        let mut added = Vec::new();
        for pos in &claims {
            match gone.iter().position(|p| p == pos) {
                Some(index) => {
                    gone.swap_remove(index);
                }
                None => added.push(*pos),
            }
        }
        for pos in gone {
            self.ticket(pos, Ticket::Release);
        }
        for pos in added {
            self.ticket(pos, Ticket::Take);
        }
        if claims.is_empty() {
            self.holders.remove(&entity);
        } else {
            self.holders.insert(entity, claims);
        }
    }

    /// An entity left for good (died, was discarded): its tickets are
    /// given back.
    pub fn release_holder(&mut self, entity: i32) {
        for pos in self.holders.remove(&entity).unwrap_or_default() {
            self.ticket(pos, Ticket::Release);
        }
    }

    /// An entity moved on (handed over, unloaded, sent elsewhere) keeping
    /// its tickets.
    pub fn forget_holder(&mut self, entity: i32) {
        self.holders.remove(&entity);
    }

    /// Forgets the holders `present` no longer knows.
    pub fn forget_absent_holders(&mut self, present: impl Fn(i32) -> bool) {
        self.holders.retain(|id, _| present(*id));
    }

    pub fn take_outbox(&mut self) -> Vec<(BlockPos, Ticket)> {
        std::mem::take(&mut self.outbox)
    }

    /// Own chunks whose records changed since last asked, to save.
    pub fn take_unsaved(&mut self) -> Vec<ChunkPos> {
        self.unsaved.drain().collect()
    }

    pub fn is_unsaved(&self, chunk: ChunkPos) -> bool {
        self.unsaved.contains(&chunk)
    }

    /// Own chunks loaded, unloaded or changed since last asked, for the
    /// neighbours' mirrors.
    pub fn take_unmirrored(&mut self) -> Vec<ChunkPos> {
        self.unmirrored.drain().collect()
    }

    /// A neighbour's records of one of its chunks, `None` once it unloads.
    pub fn set_mirror(&mut self, chunk: ChunkPos, records: Option<&[Record]>) {
        if self.owns(chunk) {
            return;
        }
        match records {
            Some(records) if !records.is_empty() => {
                self.mirror.insert(chunk, records.to_vec());
            }
            _ => {
                self.mirror.remove(&chunk);
            }
        }
    }

    /// A neighbour stopped: its mirrored points go.
    pub fn drop_mirror_of(&mut self, region: RegionPos) {
        self.mirror.retain(|chunk, _| chunk.region() != region);
    }
}

#[cfg(test)]
mod tests {
    use tuff_vanilla_data::Block;

    use super::*;
    use tuff_world_model::chunk::ChunkShape;

    fn claim(store: &mut PoiStore, entity: i32, pos: &[BlockPos]) {
        store.hold(entity, pos.to_vec());
    }

    #[tuff_core::watchdog]
    #[test]
    fn village_distance_counts_sections_to_the_nearest_claimed_village_point() {
        let mut store = PoiStore::default();
        let bed = BlockPos::new(40, 70, 8);
        let beehive = BlockPos::new(8, 70, 8);
        store.install(
            ChunkPos::new(2, 0),
            vec![Record::new(bed, PointOfInterestType::Home)],
        );
        store.install(
            ChunkPos::new(0, 0),
            vec![Record::new(beehive, PointOfInterestType::Beehive)],
        );
        let here = BlockPos::new(8, 70, 8);
        assert_eq!(store.sections_to_village(here), NO_VILLAGE, "unclaimed");
        claim(&mut store, 1, &[bed, beehive]);
        assert_eq!(store.sections_to_village(here), 2);
        assert_eq!(store.sections_to_village(bed), 0);
        assert_eq!(store.sections_to_village(BlockPos::new(40, 140, 8)), 4);
        assert_eq!(
            store.sections_to_village(BlockPos::new(400, 70, 8)),
            NO_VILLAGE
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn chunks_are_scanned_for_their_points_of_interest() {
        let mut chunk = Chunk::uniform(ChunkShape::OVERWORLD, Block::Stone.default_state(), 0);
        chunk.set(3, 70, 5, Block::Composter.default_state());
        let bell = Block::Bell.default_state();
        chunk.set(4, 71, 5, bell);
        let pos = ChunkPos::new(2, -1);
        let mut found = scan(pos, &chunk);
        found.sort_by_key(|(at, _)| at.y);
        assert_eq!(
            found,
            vec![
                (BlockPos::new(35, 70, -11), PointOfInterestType::Farmer),
                (BlockPos::new(36, 71, -11), PointOfInterestType::Meeting),
            ]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn searches_find_the_closest_free_points_and_follow_block_changes() {
        let mut store = PoiStore::default();
        let near = BlockPos::new(2, 64, 0);
        let far = BlockPos::new(10, 64, 0);
        let composter = Block::Composter.default_state();
        store.block_changed(near, composter);
        store.block_changed(far, composter);
        let farmer = |kind| kind == PointOfInterestType::Farmer;
        let origin = BlockPos::new(0, 64, 0);
        let found = store.closest_first(origin, 48, Occupancy::HasSpace, farmer);
        assert_eq!(
            found.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            vec![near, far]
        );
        claim(&mut store, 7, &[near]);
        let found = store.closest_first(origin, 48, Occupancy::HasSpace, farmer);
        assert_eq!(found.iter().map(|(p, _)| *p).collect::<Vec<_>>(), vec![far]);
        assert!(
            store
                .closest_first(origin, 5, Occupancy::HasSpace, farmer)
                .is_empty()
        );
        store.hold(7, Vec::new());
        assert_eq!(store.claims(near), 0);
        store.block_changed(near, Block::Air.default_state());
        assert_eq!(store.kind_at(near), None);
        assert_eq!(store.kind_at(far), Some(PointOfInterestType::Farmer));
    }

    #[tuff_core::watchdog]
    #[test]
    fn holders_take_tickets_only_for_new_claims_and_give_them_back_on_death() {
        let mut store = PoiStore::default();
        let composter = BlockPos::new(1, 64, 1);
        let bell = BlockPos::new(3, 64, 1);
        store.block_changed(composter, Block::Composter.default_state());
        store.block_changed(bell, Block::Bell.default_state());
        store.arrive(1, vec![composter]);
        assert_eq!(store.claims(composter), 0, "arrivals take nothing");
        store.mark_uncounted(ChunkPos::new(0, 0));
        store.arrive(5, vec![bell]);
        assert_eq!(store.claims(bell), 1, "unless the point was never saved");
        store.release_holder(5);
        store.saved(ChunkPos::new(0, 0));
        store.hold(1, vec![composter, bell]);
        assert_eq!(store.claims(bell), 1);
        store.hold(1, vec![bell, composter]);
        assert_eq!(store.claims(bell), 1, "same places in another order");
        store.forget_holder(1);
        store.release_holder(1);
        assert_eq!(store.claims(bell), 1, "moved on: the ticket stays");
        claim(&mut store, 2, &[composter]);
        assert_eq!(store.claims(composter), 1);
        assert!(!store.has_space(composter), "a workstation takes one");
        store.release_holder(2);
        assert_eq!(store.claims(composter), 0);
        assert!(store.take_unsaved().contains(&ChunkPos::new(0, 0)));
        assert!(store.take_unsaved().is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn tickets_on_other_regions_points_go_out_and_wait_for_unloaded_chunks() {
        let region = RegionPos::new(0, 0);
        let mut store = PoiStore::for_region(region);
        let foreign = BlockPos::new(REGION_CHUNKS * 16 + 2, 64, 5);
        let foreign_chunk = ChunkPos::of_block(foreign);
        assert!(mirrored_in(region, foreign_chunk));
        assert!(!mirrored_in(region, ChunkPos::new(3, 3)));
        assert!(!mirrored_in(
            region,
            ChunkPos::new(REGION_CHUNKS + MIRROR_CHUNKS + 1, 0)
        ));
        assert!(mirrored_in(
            region,
            ChunkPos::new(-MIRROR_CHUNKS, -MIRROR_CHUNKS)
        ));
        store.set_mirror(
            foreign_chunk,
            Some(&[Record::new(foreign, PointOfInterestType::Meeting)]),
        );
        let origin = BlockPos::new(REGION_CHUNKS * 16 - 2, 64, 5);
        assert_eq!(
            store.closest_first(origin, 16, Occupancy::Any, |_| true),
            vec![(foreign, PointOfInterestType::Meeting)]
        );
        assert!(store.ticket(foreign, Ticket::Take));
        assert_eq!(store.claims(foreign), 1, "the mirror follows at once");
        assert_eq!(store.take_outbox(), vec![(foreign, Ticket::Take)]);
        assert!(store.take_unsaved().is_empty());

        let unloaded = BlockPos::new(40, 64, 40);
        assert!(!store.ticket(unloaded, Ticket::Take));
        store.install(
            ChunkPos::of_block(unloaded),
            vec![Record::new(unloaded, PointOfInterestType::Home)],
        );
        assert_eq!(store.claims(unloaded), 1, "applied once loaded");
        assert!(store.is_unsaved(ChunkPos::of_block(unloaded)));
        store.drop_mirror_of(foreign_chunk.region());
        assert_eq!(store.kind_at(foreign), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn merging_keeps_saved_tickets_of_points_still_there() {
        let bed = BlockPos::new(1, 64, 1);
        let gone = BlockPos::new(2, 64, 1);
        let saved = [
            Record {
                free: 0,
                ..Record::new(bed, PointOfInterestType::Home)
            },
            Record::new(gone, PointOfInterestType::Meeting),
        ];
        let (records, changed) = merge(vec![(bed, PointOfInterestType::Home)], Some(&saved));
        assert_eq!(records, vec![saved[0]]);
        assert!(changed);
        let (records, changed) = merge(vec![(bed, PointOfInterestType::Home)], Some(&records));
        assert_eq!(records[0].free, 0);
        assert!(!changed);
        let (_, changed) = merge(Vec::new(), None);
        assert!(!changed);
        let (_, changed) = merge(vec![(bed, PointOfInterestType::Home)], None);
        assert!(changed, "points never saved are written");
    }
}
