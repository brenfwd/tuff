//! Points of interest across region borders: sending neighbours the
//! mirror of points near them, passing tickets on other regions' points to
//! their owners, and marking chunks whose points changed for saving.

use std::collections::HashMap;

use super::RegionActor;
use crate::world::messages::{RegionHandle, RegionInternal};
use crate::world::poi::{Ticket, mirrored_in};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, RegionPos};

impl RegionActor {
    /// Everything a neighbour that just started should mirror.
    pub(super) fn mirror_pois_to(&self, handle: &RegionHandle) {
        for chunk in self.chunk_io.pois.own_chunks() {
            if mirrored_in(handle.pos, chunk) {
                handle.internal(RegionInternal::PoiMirror {
                    pos: chunk,
                    records: self.chunk_io.poi_mirror(chunk),
                });
            }
        }
    }

    pub(super) fn poi_tickets(&mut self, changes: Vec<(BlockPos, Ticket)>) {
        self.chunk_io.pois.apply_tickets(changes);
        self.sync_pois();
    }

    /// Marks chunks whose points changed for saving, refreshes the
    /// neighbours' mirrors and sends tickets on their points to them.
    pub(super) fn sync_pois(&mut self) {
        for chunk in self.chunk_io.pois.take_unsaved() {
            if let Some(column) = self.columns.get_mut(&chunk) {
                column.pois_dirty = true;
            }
        }
        for chunk in self.chunk_io.pois.take_unmirrored() {
            let mut records = None;
            for neighbor in self.border.neighbors.values() {
                if mirrored_in(neighbor.pos, chunk) {
                    let records = records.get_or_insert_with(|| self.chunk_io.poi_mirror(chunk));
                    neighbor.internal(RegionInternal::PoiMirror {
                        pos: chunk,
                        records: records.clone(),
                    });
                }
            }
        }
        let outbox = self.chunk_io.pois.take_outbox();
        if outbox.is_empty() {
            return;
        }
        let mut by_region: HashMap<RegionPos, Vec<(BlockPos, Ticket)>> = HashMap::new();
        for (pos, ticket) in outbox {
            by_region
                .entry(ChunkPos::of_block(pos).region())
                .or_default()
                .push((pos, ticket));
        }
        for (region, changes) in by_region {
            let sent = self
                .border
                .neighbors
                .get(&region)
                .is_some_and(|neighbor| neighbor.internal(RegionInternal::PoiTickets(changes)));
            if !sent {
                log::debug!(
                    "region {:?}: dropping point of interest tickets for {region:?}, which is not running next to it",
                    self.pos
                );
            }
        }
    }
}
