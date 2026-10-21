use std::time::{Duration, Instant};

use tuff_vanilla_data::entity::MobCategory;

use super::RegionActor;
use crate::metrics::{RegionReport, Report, millis};
use tuff_timings::Tag;

impl RegionActor {
    /// The timings tag naming this region.
    pub(super) fn timings_tag(&self) -> Tag {
        Tag::Region {
            dimension: self.dimension.index() as u16,
            x: self.pos.x,
            z: self.pos.z,
        }
    }

    /// Adds one tick's duration to the metrics window and, once a report
    /// is due, summarizes the region and queues it without waiting.
    pub(super) fn report_metrics(&mut self, tick: u64, busy: Duration) {
        let Some(stats) = self
            .metrics
            .record(self.shared.metrics.pace(), busy, Instant::now())
        else {
            return;
        };
        let mut by_category = vec![0u64; MobCategory::COUNT];
        for tracked in self.ents.entities.values() {
            by_category[usize::from(tracked.entity.base().kind.category().id())] += 1;
        }
        let (mut block_entities, mut scheduled_ticks) = (0, 0);
        for column in self.columns.values() {
            block_entities +=
                column.meta.block_entities.len() + column.meta.raw_block_entities.len();
            scheduled_ticks += column.block_ticks.len() + column.fluid_ticks.len();
        }
        let players = self
            .ents
            .presence
            .values()
            .filter(|player| player.chunk.region() == self.pos)
            .count();
        self.shared
            .metrics
            .report(Report::Region(Box::new(RegionReport {
                dimension: self.dimension().key.clone(),
                x: self.pos.x,
                z: self.pos.z,
                tick,
                ticks: stats.ticks,
                mspt_mean: millis(stats.mean),
                mspt_max: millis(stats.max),
                chunks: self.columns.len() as u64,
                loading: self.chunk_io.loading.len() as u64,
                generating: self.chunk_io.generating.len() as u64,
                entities: self.ents.entities.len() as u64,
                entities_by_category: by_category,
                block_entities: block_entities as u64,
                scheduled_ticks: scheduled_ticks as u64,
                players: players as u64,
            })));
    }
}
