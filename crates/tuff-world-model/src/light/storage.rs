use std::collections::{HashMap, HashSet};

use crate::pos::ChunkPos;

use super::data_layer::DataLayer;
use super::{Pos, SectionPos};

const HAS_DATA: u8 = 32;
const NEIGHBOR_COUNT: u8 = 31;

/// Which sections of one light layer hold light data, and that data.
///
/// A section stores light while it or any of its 26 neighbours contains
/// blocks. Sky light additionally tracks each column's top stored section:
/// everything above it is implicitly full sky light.
pub(super) struct Storage {
    sky: bool,
    layers: HashMap<SectionPos, DataLayer>,
    /// Sky only: one past each column's highest stored section.
    top_sections: HashMap<ChunkPos, i32>,
    /// Sky only: the lowest section y ever stored.
    lowest_y: i32,
    section_states: HashMap<SectionPos, u8>,
    columns_with_sources: HashSet<ChunkPos>,
    /// Per column, the light sections (bit `y - min_section`) touched since the
    /// last [`Self::take_affected`]; sections outside the world's light range are
    /// not recorded.
    affected: HashMap<ChunkPos, u64>,
    min_section: i32,
    section_count: u32,
    last_affected: Option<(SectionPos, SectionPos)>,
    queued: HashMap<SectionPos, DataLayer>,
    columns_to_retain_queued: HashSet<ChunkPos>,
    to_remove: HashSet<SectionPos>,
    has_inconsistencies: bool,
}

impl Storage {
    pub fn new(sky: bool, min_section: i32, section_count: usize) -> Self {
        Self {
            sky,
            layers: HashMap::new(),
            top_sections: HashMap::new(),
            lowest_y: i32::MAX,
            section_states: HashMap::new(),
            columns_with_sources: HashSet::new(),
            affected: HashMap::new(),
            min_section,
            section_count: section_count.min(64) as u32,
            last_affected: None,
            queued: HashMap::new(),
            columns_to_retain_queued: HashSet::new(),
            to_remove: HashSet::new(),
            has_inconsistencies: false,
        }
    }

    pub fn storing_light_for_section(&self, section: SectionPos) -> bool {
        self.layers.contains_key(&section)
    }

    pub fn layer_mut(&mut self, section: SectionPos) -> Option<&mut DataLayer> {
        self.layers.get_mut(&section)
    }

    /// The data a client or the saved chunk should see for `section`.
    pub fn data_layer_data(&self, section: SectionPos) -> Option<&DataLayer> {
        self.queued
            .get(&section)
            .or_else(|| self.layers.get(&section))
    }

    pub fn stored_level(&self, pos: Pos) -> u8 {
        let (x, y, z) = pos.section_relative();
        self.layers[&pos.section()].get(x, y, z)
    }

    pub fn set_stored_level(&mut self, pos: Pos, level: u8) {
        let (x, y, z) = pos.section_relative();
        self.layers
            .get_mut(&pos.section())
            .expect("light stored for section")
            .set(x, y, z, level);
        let low = Pos::new(pos.x - 1, pos.y - 1, pos.z - 1).section();
        let high = Pos::new(pos.x + 1, pos.y + 1, pos.z + 1).section();
        if self.last_affected == Some((low, high)) {
            return;
        }
        self.last_affected = Some((low, high));
        self.mark_affected(low, high);
    }

    /// Records every section in the box `low..=high` as affected.
    fn mark_affected(&mut self, low: SectionPos, high: SectionPos) {
        let first = (low.y - self.min_section).max(0);
        let last = (high.y - self.min_section).min(self.section_count as i32 - 1);
        if first > last {
            return;
        }
        let mask = (u64::MAX >> (63 - (last - first) as u32)) << first;
        for z in low.z..=high.z {
            for x in low.x..=high.x {
                *self.affected.entry(ChunkPos::new(x, z)).or_default() |= mask;
            }
        }
    }

    fn mark_section_and_neighbors_affected(&mut self, section: SectionPos) {
        self.mark_affected(section.offset(-1, -1, -1), section.offset(1, 1, 1));
    }

    /// The affected light sections per column, as masks over the light
    /// section index from the bottom of the world's light range.
    pub fn take_affected(&mut self) -> HashMap<ChunkPos, u64> {
        self.last_affected = None;
        std::mem::take(&mut self.affected)
    }

    pub fn has_inconsistencies(&self) -> bool {
        self.has_inconsistencies
    }

    /// Applies queued section removals and queued data.
    pub fn mark_new_inconsistencies(&mut self) {
        if !self.has_inconsistencies {
            return;
        }
        self.has_inconsistencies = false;
        let to_remove: Vec<SectionPos> = self.to_remove.iter().copied().collect();
        for &section in &to_remove {
            let queued = self.queued.remove(&section);
            let removed = self.layers.remove(&section);
            if self.columns_to_retain_queued.contains(&section.column())
                && let Some(layer) = queued.or(removed)
            {
                self.queued.insert(section, layer);
            }
        }
        for &section in &to_remove {
            self.on_node_removed(section);
        }
        self.to_remove.clear();
        let ready: Vec<SectionPos> = self
            .queued
            .keys()
            .copied()
            .filter(|section| self.storing_light_for_section(*section))
            .collect();
        for section in ready {
            let layer = self.queued.remove(&section).expect("queued layer");
            self.layers.insert(section, layer);
        }
    }

    pub fn set_light_enabled(&mut self, column: ChunkPos, enabled: bool) {
        if enabled {
            self.columns_with_sources.insert(column);
        } else {
            self.columns_with_sources.remove(&column);
        }
    }

    pub fn light_on_in_section(&self, section: SectionPos) -> bool {
        self.columns_with_sources.contains(&section.column())
    }

    pub fn light_on_in_column(&self, column: ChunkPos) -> bool {
        self.columns_with_sources.contains(&column)
    }

    pub fn retain_data(&mut self, column: ChunkPos, retain: bool) {
        if retain {
            self.columns_to_retain_queued.insert(column);
        } else {
            self.columns_to_retain_queued.remove(&column);
        }
    }

    pub fn queue_section_data(&mut self, section: SectionPos, layer: Option<DataLayer>) {
        match layer {
            None => {
                self.queued.remove(&section);
            }
            Some(layer) => {
                self.queued.insert(section, layer);
                self.has_inconsistencies = true;
            }
        }
    }

    /// `empty` is whether the section now holds only air.
    pub fn update_section_status(&mut self, section: SectionPos, empty: bool) {
        let state = self.state(section);
        let updated = if empty {
            state & !HAS_DATA
        } else {
            state | HAS_DATA
        };
        if state == updated {
            return;
        }
        self.put_section_state(section, updated);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if dx == 0 && dy == 0 && dz == 0 {
                        continue;
                    }
                    let neighbor = section.offset(dx, dy, dz);
                    let state = self.state(neighbor);
                    let count = (state & NEIGHBOR_COUNT) as i32 + if empty { -1 } else { 1 };
                    assert!(
                        (0..=26).contains(&count),
                        "light section neighbor count out of range"
                    );
                    self.put_section_state(neighbor, (state & !NEIGHBOR_COUNT) | count as u8);
                }
            }
        }
    }

    fn state(&self, section: SectionPos) -> u8 {
        self.section_states.get(&section).copied().unwrap_or(0)
    }

    fn put_section_state(&mut self, section: SectionPos, state: u8) {
        if state != 0 {
            if self.section_states.insert(section, state).unwrap_or(0) == 0 {
                self.initialize_section(section);
            }
        } else if self.section_states.remove(&section).unwrap_or(0) != 0 {
            self.to_remove.insert(section);
            self.has_inconsistencies = true;
        }
    }

    fn initialize_section(&mut self, section: SectionPos) {
        if self.to_remove.remove(&section) {
            return;
        }
        let layer = self.create_data_layer(section);
        self.layers.insert(section, layer);
        self.on_node_added(section);
        self.mark_section_and_neighbors_affected(section);
        self.has_inconsistencies = true;
    }

    /// Queued data wins. Otherwise a new sky section copies the light entering
    /// it from above: full if nothing is stored above, else the bottom slice
    /// of the nearest stored section above.
    fn create_data_layer(&mut self, section: SectionPos) -> DataLayer {
        if let Some(layer) = self.queued.remove(&section) {
            return layer;
        }
        if !self.sky {
            return DataLayer::default();
        }
        if self.is_above_data(section) {
            return if self.light_on_in_section(section) {
                DataLayer::filled(15)
            } else {
                DataLayer::default()
            };
        }
        let mut above = section.offset(0, 1, 0);
        loop {
            if let Some(layer) = self.layers.get(&above) {
                return layer.repeat_first_slice();
            }
            above = above.offset(0, 1, 0);
        }
    }

    fn on_node_added(&mut self, section: SectionPos) {
        if !self.sky {
            return;
        }
        if self.lowest_y > section.y {
            self.lowest_y = section.y;
        }
        let column = section.column();
        if self.top_section_y(column) < section.y + 1 {
            self.top_sections.insert(column, section.y + 1);
        }
    }

    fn on_node_removed(&mut self, section: SectionPos) {
        if !self.sky {
            return;
        }
        let column = section.column();
        let mut y = section.y;
        if self.top_section_y(column) != y + 1 {
            return;
        }
        let mut current = section;
        while !self.storing_light_for_section(current) && self.has_light_data_at_or_below(y) {
            y -= 1;
            current = current.offset(0, -1, 0);
        }
        if self.storing_light_for_section(current) {
            self.top_sections.insert(column, y + 1);
        } else {
            self.top_sections.remove(&column);
        }
    }

    pub fn has_light_data_at_or_below(&self, y: i32) -> bool {
        y >= self.lowest_y
    }

    pub fn is_above_data(&self, section: SectionPos) -> bool {
        let top = self.top_section_y(section.column());
        top == self.lowest_y || section.y >= top
    }

    /// One past the column's highest stored section; `bottom_section_y()`
    /// if nothing in the column is stored.
    pub fn top_section_y(&self, column: ChunkPos) -> i32 {
        self.top_sections
            .get(&column)
            .copied()
            .unwrap_or(self.lowest_y)
    }

    pub fn bottom_section_y(&self) -> i32 {
        self.lowest_y
    }

    pub fn light_value(&self, pos: Pos) -> u8 {
        let section = pos.section();
        if !self.sky {
            let (x, y, z) = pos.section_relative();
            return self
                .layers
                .get(&section)
                .map_or(0, |layer| layer.get(x, y, z));
        }
        let top = self.top_section_y(section.column());
        if top == self.lowest_y || section.y >= top {
            return 15;
        }
        let (x, _, z) = pos.section_relative();
        let mut current = section;
        let mut local_y = pos.section_relative().1;
        loop {
            if let Some(layer) = self.layers.get(&current) {
                return layer.get(x, local_y, z);
            }
            local_y = 0;
            current = current.offset(0, 1, 0);
            if current.y >= top {
                return 15;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    use super::*;

    const MIN: i32 = -5;
    const COUNT: usize = 28;

    fn reference_mask(sections: &HashSet<SectionPos>) -> HashMap<ChunkPos, u64> {
        let mut masks: HashMap<ChunkPos, u64> = HashMap::new();
        for section in sections {
            let index = section.y - MIN;
            if (0..COUNT as i32).contains(&index) {
                *masks.entry(section.column()).or_default() |= 1 << index;
            }
        }
        masks
    }

    fn boxed(around: SectionPos, into: &mut HashSet<SectionPos>) {
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    into.insert(around.offset(dx, dy, dz));
                }
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn affected_masks_equal_the_section_set_they_replace() {
        for seed in 0..40 {
            let mut rng = StdRng::seed_from_u64(seed);
            let mut storage = Storage::new(seed % 2 == 0, MIN, COUNT);
            let mut created = Vec::new();
            let mut expected = HashSet::new();
            for _ in 0..rng.gen_range(1..4) {
                let section = SectionPos::new(
                    rng.gen_range(-3..3),
                    *[
                        MIN - 1,
                        MIN,
                        MIN + 1,
                        0,
                        20,
                        MIN + COUNT as i32 - 2,
                        MIN + COUNT as i32 - 1,
                    ]
                    .get(rng.gen_range(0..7))
                    .unwrap(),
                    rng.gen_range(-3..3),
                );
                if created.contains(&section) {
                    continue;
                }
                storage.update_section_status(section, false);
                created.push(section);
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        for dz in -1..=1 {
                            boxed(section.offset(dx, dy, dz), &mut expected);
                        }
                    }
                }
            }
            assert_eq!(
                storage.take_affected(),
                reference_mask(&expected),
                "seed {seed}"
            );
            for round in 0..30 {
                let mut expected = HashSet::new();
                for _ in 0..rng.gen_range(0..12) {
                    let section = created[rng.gen_range(0..created.len())].offset(
                        rng.gen_range(-1..=1),
                        rng.gen_range(-1..=1),
                        rng.gen_range(-1..=1),
                    );
                    if !storage.storing_light_for_section(section) {
                        continue;
                    }
                    let pos = Pos::new(
                        (section.x << 4) + rng.gen_range(0..16),
                        (section.y << 4) + rng.gen_range(0..16),
                        (section.z << 4) + rng.gen_range(0..16),
                    );
                    storage.set_stored_level(pos, rng.gen_range(0..16));
                    let low = Pos::new(pos.x - 1, pos.y - 1, pos.z - 1).section();
                    let high = Pos::new(pos.x + 1, pos.y + 1, pos.z + 1).section();
                    for x in low.x..=high.x {
                        for y in low.y..=high.y {
                            for z in low.z..=high.z {
                                expected.insert(SectionPos::new(x, y, z));
                            }
                        }
                    }
                }
                assert_eq!(
                    storage.take_affected(),
                    reference_mask(&expected),
                    "seed {seed} round {round}"
                );
            }
        }
    }
}
