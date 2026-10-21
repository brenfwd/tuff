use tuff_vanilla_data::registry::PointOfInterestType;

use crate::BlockPos;

/// One point of interest and how many more claims it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub pos: BlockPos,
    pub kind: PointOfInterestType,
    pub free: u32,
}

impl Record {
    /// An unclaimed point.
    pub fn new(pos: BlockPos, kind: PointOfInterestType) -> Self {
        Self {
            pos,
            kind,
            free: kind.info().max_tickets,
        }
    }

    pub fn claims(&self) -> u32 {
        self.kind.info().max_tickets.saturating_sub(self.free)
    }
}
