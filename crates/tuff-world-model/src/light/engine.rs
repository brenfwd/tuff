use std::collections::{HashSet, VecDeque};

use tuff_vanilla_data::light::Direction;
use tuff_vanilla_data::{Block, BlockState};

use crate::pos::ChunkPos;

use super::data_layer::DataLayer;
use super::sky_sources::{NEGATIVE_INFINITY, SkyLightSources};
use super::storage::Storage;
use super::{AIR, ChunkSource, Pos, SectionPos};

/// A queued propagation step: the level it carries (low four bits), the
/// directions to propagate in (one bit each, by `Direction` order), and flags.
mod entry {
    use tuff_vanilla_data::light::Direction;

    const LEVEL_MASK: u64 = 0x0F;
    const DIRECTIONS_MASK: u64 = 0x3F0;
    const FROM_EMPTY_SHAPE: u64 = 0x400;
    const INCREASE_FROM_EMISSION: u64 = 0x800;

    const fn bit(direction: Direction) -> u64 {
        1 << (direction as u64 + 4)
    }

    const fn with_level(entry: u64, level: u8) -> u64 {
        (entry & !LEVEL_MASK) | (level as u64 & LEVEL_MASK)
    }

    fn empty_flag(from_empty_shape: bool) -> u64 {
        if from_empty_shape {
            FROM_EMPTY_SHAPE
        } else {
            0
        }
    }

    pub const fn decrease_skip_one_direction(level: u8, direction: Direction) -> u64 {
        with_level(DIRECTIONS_MASK & !bit(direction), level)
    }

    pub const fn decrease_all_directions(level: u8) -> u64 {
        with_level(DIRECTIONS_MASK, level)
    }

    pub fn increase_light_from_emission(level: u8, from_empty_shape: bool) -> u64 {
        with_level(
            DIRECTIONS_MASK | INCREASE_FROM_EMISSION | empty_flag(from_empty_shape),
            level,
        )
    }

    pub fn increase_skip_one_direction(
        level: u8,
        from_empty_shape: bool,
        direction: Direction,
    ) -> u64 {
        with_level(
            (DIRECTIONS_MASK & !bit(direction)) | empty_flag(from_empty_shape),
            level,
        )
    }

    pub fn increase_only_one_direction(
        level: u8,
        from_empty_shape: bool,
        direction: Direction,
    ) -> u64 {
        with_level(bit(direction) | empty_flag(from_empty_shape), level)
    }

    pub fn increase_sky_source_in_directions(
        down: bool,
        north: bool,
        south: bool,
        west: bool,
        east: bool,
    ) -> u64 {
        let mut entry = with_level(0, 15);
        for (enabled, direction) in [
            (down, Direction::Down),
            (north, Direction::North),
            (south, Direction::South),
            (west, Direction::West),
            (east, Direction::East),
        ] {
            if enabled {
                entry |= bit(direction);
            }
        }
        entry
    }

    pub const fn from_level(entry: u64) -> u8 {
        (entry & LEVEL_MASK) as u8
    }

    pub const fn is_from_empty_shape(entry: u64) -> bool {
        entry & FROM_EMPTY_SHAPE != 0
    }

    pub const fn is_increase_from_emission(entry: u64) -> bool {
        entry & INCREASE_FROM_EMISSION != 0
    }

    pub const fn should_propagate_in_direction(entry: u64, direction: Direction) -> bool {
        entry & bit(direction) != 0
    }
}

const PULL_LIGHT_IN: u64 = entry::decrease_all_directions(1);
const REMOVE_TOP_SKY_SOURCE: u64 = entry::decrease_all_directions(15);
const REMOVE_SKY_SOURCE: u64 = entry::decrease_skip_one_direction(15, Direction::Up);

fn add_sky_source() -> u64 {
    entry::increase_skip_one_direction(15, false, Direction::Up)
}

fn is_source_level(level: u8) -> bool {
    level == 15
}

fn opacity(state: BlockState) -> u8 {
    state.light_dampening().max(1)
}

/// Light cannot cross from `from` into its neighbour `to` in `direction`.
fn shape_occludes(from: BlockState, to: BlockState, direction: Direction) -> bool {
    from.light_face(direction)
        .occludes(to.light_face(direction.opposite()))
}

/// Whether a horizontal step in `direction` arrived at a section edge.
fn crossed_section_edge(direction: Direction, x: i32, z: i32) -> bool {
    match direction {
        Direction::North => z == 15,
        Direction::South => z == 0,
        Direction::West => x == 15,
        Direction::East => x == 0,
        _ => false,
    }
}

/// The propagation engine for one light layer.
pub(super) struct LayerEngine {
    sky: bool,
    pub(super) storage: Storage,
    nodes_to_check: HashSet<Pos>,
    decrease: VecDeque<(Pos, u64)>,
    increase: VecDeque<(Pos, u64)>,
    empty_sources: SkyLightSources,
}

impl LayerEngine {
    pub fn new(sky: bool, min_section: i32, section_count: usize) -> Self {
        Self {
            sky,
            storage: Storage::new(sky, min_section, section_count),
            nodes_to_check: HashSet::new(),
            decrease: VecDeque::new(),
            increase: VecDeque::new(),
            empty_sources: SkyLightSources::default(),
        }
    }

    fn state(&self, source: &dyn ChunkSource, pos: Pos) -> BlockState {
        match source.chunk(pos.column()) {
            Some(chunk) => chunk.blocks.get(pos.x, pos.y, pos.z),
            None => Block::Bedrock.default_state(),
        }
    }

    pub fn check_block(&mut self, pos: Pos) {
        self.nodes_to_check.insert(pos);
    }

    pub fn has_light_work(&self) -> bool {
        self.storage.has_inconsistencies()
            || !self.nodes_to_check.is_empty()
            || !self.decrease.is_empty()
            || !self.increase.is_empty()
    }

    pub fn run_light_updates(&mut self, source: &dyn ChunkSource) -> usize {
        for pos in std::mem::take(&mut self.nodes_to_check) {
            self.check_node(source, pos);
        }
        let count = self.propagate_decreases(source) + self.propagate_increases(source);
        self.storage.mark_new_inconsistencies();
        count
    }

    fn propagate_increases(&mut self, source: &dyn ChunkSource) -> usize {
        let mut count = 0;
        while let Some((pos, entry)) = self.increase.pop_front() {
            let mut stored = self.storage.stored_level(pos);
            let from = entry::from_level(entry);
            if entry::is_increase_from_emission(entry) && stored < from {
                self.storage.set_stored_level(pos, from);
                stored = from;
            }
            if stored == from {
                self.propagate_increase(source, pos, entry, stored);
            }
            count += 1;
        }
        count
    }

    fn propagate_decreases(&mut self, source: &dyn ChunkSource) -> usize {
        let mut count = 0;
        while let Some((pos, entry)) = self.decrease.pop_front() {
            self.propagate_decrease(source, pos, entry);
            count += 1;
        }
        count
    }

    fn enqueue_decrease(&mut self, pos: Pos, entry: u64) {
        self.decrease.push_back((pos, entry));
    }

    fn enqueue_increase(&mut self, pos: Pos, entry: u64) {
        self.increase.push_back((pos, entry));
    }

    fn check_node(&mut self, source: &dyn ChunkSource, pos: Pos) {
        if self.sky {
            self.check_sky_node(source, pos);
        } else {
            self.check_block_node(source, pos);
        }
    }

    fn propagate_increase(&mut self, source: &dyn ChunkSource, pos: Pos, entry: u64, level: u8) {
        let empty_sections_below = if self.sky {
            self.count_empty_sections_below_if_at_border(pos)
        } else {
            0
        };
        let mut from_state = None;
        for direction in Direction::ALL {
            if !entry::should_propagate_in_direction(entry, direction) {
                continue;
            }
            let neighbor = pos.offset(direction);
            if !self.storage.storing_light_for_section(neighbor.section()) {
                continue;
            }
            let stored = self.storage.stored_level(neighbor);
            if level.saturating_sub(1) <= stored {
                continue;
            }
            let state = self.state(source, neighbor);
            let new_level = level.saturating_sub(opacity(state));
            if new_level <= stored {
                continue;
            }
            let from = *from_state.get_or_insert_with(|| {
                if entry::is_from_empty_shape(entry) {
                    AIR
                } else {
                    self.state(source, pos)
                }
            });
            if shape_occludes(from, state, direction) {
                continue;
            }
            self.storage.set_stored_level(neighbor, new_level);
            if new_level > 1 {
                self.enqueue_increase(
                    neighbor,
                    entry::increase_skip_one_direction(
                        new_level,
                        state.is_empty_light_shape(),
                        direction.opposite(),
                    ),
                );
            }
            if self.sky {
                self.propagate_from_empty_sections(
                    neighbor,
                    direction,
                    new_level,
                    true,
                    empty_sections_below,
                );
            }
        }
    }

    fn propagate_decrease(&mut self, source: &dyn ChunkSource, pos: Pos, entry: u64) {
        let empty_sections_below = if self.sky {
            self.count_empty_sections_below_if_at_border(pos)
        } else {
            0
        };
        let from = entry::from_level(entry);
        for direction in Direction::ALL {
            if !entry::should_propagate_in_direction(entry, direction) {
                continue;
            }
            let neighbor = pos.offset(direction);
            if !self.storage.storing_light_for_section(neighbor.section()) {
                continue;
            }
            let stored = self.storage.stored_level(neighbor);
            if stored == 0 {
                continue;
            }
            if (stored as i32) > from as i32 - 1 {
                self.enqueue_increase(
                    neighbor,
                    entry::increase_only_one_direction(stored, false, direction.opposite()),
                );
                continue;
            }
            if self.sky {
                self.storage.set_stored_level(neighbor, 0);
                self.enqueue_decrease(
                    neighbor,
                    entry::decrease_skip_one_direction(stored, direction.opposite()),
                );
                self.propagate_from_empty_sections(
                    neighbor,
                    direction,
                    stored,
                    false,
                    empty_sections_below,
                );
            } else {
                let state = self.state(source, neighbor);
                let emission = self.emission(neighbor, state);
                self.storage.set_stored_level(neighbor, 0);
                if emission < stored {
                    self.enqueue_decrease(
                        neighbor,
                        entry::decrease_skip_one_direction(stored, direction.opposite()),
                    );
                }
                if emission > 0 {
                    self.enqueue_increase(
                        neighbor,
                        entry::increase_light_from_emission(emission, state.is_empty_light_shape()),
                    );
                }
            }
        }
    }

    // Block light.

    fn emission(&self, pos: Pos, state: BlockState) -> u8 {
        let emission = state.light_emission();
        if emission == 0 || !self.storage.light_on_in_section(pos.section()) {
            0
        } else {
            emission
        }
    }

    fn check_block_node(&mut self, source: &dyn ChunkSource, pos: Pos) {
        if !self.storage.storing_light_for_section(pos.section()) {
            return;
        }
        let state = self.state(source, pos);
        let emission = self.emission(pos, state);
        let stored = self.storage.stored_level(pos);
        if emission < stored {
            self.storage.set_stored_level(pos, 0);
            self.enqueue_decrease(pos, entry::decrease_all_directions(stored));
        } else {
            self.enqueue_decrease(pos, PULL_LIGHT_IN);
        }
        if emission > 0 {
            self.enqueue_increase(
                pos,
                entry::increase_light_from_emission(emission, state.is_empty_light_shape()),
            );
        }
    }

    pub fn set_light_enabled(&mut self, source: &dyn ChunkSource, column: ChunkPos, enabled: bool) {
        self.storage.set_light_enabled(column, enabled);
        if !self.sky || !enabled {
            return;
        }
        let highest = self
            .sky_sources(source, column)
            .unwrap_or(&self.empty_sources)
            .highest_lowest_source_y();
        let first_unlit = (highest.wrapping_sub(1) >> 4).wrapping_add(1);
        let top = self.storage.top_section_y(column);
        let bottom = self.storage.bottom_section_y().max(first_unlit);
        let mut y = top - 1;
        while y >= bottom {
            if let Some(layer) = self
                .storage
                .layer_mut(SectionPos::new(column.x, y, column.z))
                && layer.is_empty()
            {
                layer.fill(15);
            }
            y -= 1;
        }
    }

    pub fn propagate_light_sources(&mut self, source: &dyn ChunkSource, column: ChunkPos) {
        if self.sky {
            self.propagate_sky_sources(source, column);
            return;
        }
        self.set_light_enabled(source, column, true);
        let Some(chunk) = source.chunk(column) else {
            return;
        };
        let (base_x, base_z) = (column.x << 4, column.z << 4);
        let shape = chunk.blocks.shape();
        for (index, section) in chunk.blocks.sections().enumerate() {
            if section.has_only_air() {
                continue;
            }
            let base_y = shape.section_base(index);
            section.for_each_matching(
                |state| state.light_emission() != 0,
                |index, state| {
                    let (x, y, z) = (index as i32 & 15, index as i32 >> 8, index as i32 >> 4 & 15);
                    self.enqueue_increase(
                        Pos::new(base_x + x, base_y + y, base_z + z),
                        entry::increase_light_from_emission(
                            state.light_emission(),
                            state.is_empty_light_shape(),
                        ),
                    );
                },
            );
        }
    }

    // Sky light.

    fn sky_sources<'a>(
        &self,
        source: &'a dyn ChunkSource,
        column: ChunkPos,
    ) -> Option<&'a SkyLightSources> {
        source.chunk(column).map(|chunk| chunk.sky_sources)
    }

    fn lowest_source_y(&self, source: &dyn ChunkSource, x: i32, z: i32, default: i32) -> i32 {
        match self.sky_sources(source, ChunkPos::new(x >> 4, z >> 4)) {
            Some(sources) => sources.lowest_source_y(x & 15, z & 15),
            None => default,
        }
    }

    fn check_sky_node(&mut self, source: &dyn ChunkSource, pos: Pos) {
        let section = pos.section();
        let lowest = if self.storage.light_on_in_section(section) {
            self.lowest_source_y(source, pos.x, pos.z, i32::MAX)
        } else {
            i32::MAX
        };
        if lowest != i32::MAX {
            self.update_sources_in_column(source, pos.x, pos.z, lowest);
        }
        if !self.storage.storing_light_for_section(section) {
            return;
        }
        if pos.y >= lowest {
            self.enqueue_decrease(pos, REMOVE_SKY_SOURCE);
            self.enqueue_increase(pos, add_sky_source());
            return;
        }
        let stored = self.storage.stored_level(pos);
        if stored == 0 {
            self.enqueue_decrease(pos, PULL_LIGHT_IN);
        } else {
            self.storage.set_stored_level(pos, 0);
            self.enqueue_decrease(pos, entry::decrease_all_directions(stored));
        }
    }

    fn update_sources_in_column(&mut self, source: &dyn ChunkSource, x: i32, z: i32, lowest: i32) {
        let bottom = self.storage.bottom_section_y() << 4;
        self.remove_sources_below(x, z, lowest, bottom);
        self.add_sources_above(source, x, z, lowest, bottom);
    }

    fn remove_sources_below(&mut self, x: i32, z: i32, lowest: i32, bottom: i32) {
        if lowest <= bottom {
            return;
        }
        let top = lowest - 1;
        let mut section_y = top >> 4;
        while self.storage.has_light_data_at_or_below(section_y) {
            if self
                .storage
                .storing_light_for_section(SectionPos::new(x >> 4, section_y, z >> 4))
            {
                let base = section_y << 4;
                let mut y = (base + 15).min(top);
                while y >= base {
                    let pos = Pos::new(x, y, z);
                    if !is_source_level(self.storage.stored_level(pos)) {
                        return;
                    }
                    self.storage.set_stored_level(pos, 0);
                    self.enqueue_decrease(
                        pos,
                        if y == top {
                            REMOVE_TOP_SKY_SOURCE
                        } else {
                            REMOVE_SKY_SOURCE
                        },
                    );
                    y -= 1;
                }
            }
            section_y -= 1;
        }
    }

    fn add_sources_above(
        &mut self,
        source: &dyn ChunkSource,
        x: i32,
        z: i32,
        lowest: i32,
        bottom: i32,
    ) {
        let neighbors = [(-1, 0), (1, 0), (0, -1), (0, 1)]
            .map(|(dx, dz)| self.lowest_source_y(source, x + dx, z + dz, NEGATIVE_INFINITY));
        let neighbors_max = neighbors.into_iter().max().unwrap_or(NEGATIVE_INFINITY);
        let start = lowest.max(bottom);
        let mut section = SectionPos::new(x >> 4, start >> 4, z >> 4);
        while !self.storage.is_above_data(section) {
            if self.storage.storing_light_for_section(section) {
                let base = section.y << 4;
                for y in base.max(start)..=base + 15 {
                    let pos = Pos::new(x, y, z);
                    if is_source_level(self.storage.stored_level(pos)) {
                        return;
                    }
                    self.storage.set_stored_level(pos, 15);
                    if y < neighbors_max || y == lowest {
                        self.enqueue_increase(pos, add_sky_source());
                    }
                }
            }
            section = section.offset(0, 1, 0);
        }
    }

    fn count_empty_sections_below_if_at_border(&self, pos: Pos) -> i32 {
        if pos.y & 15 != 0 {
            return 0;
        }
        let (x, z) = (pos.x & 15, pos.z & 15);
        if x != 0 && x != 15 && z != 0 && z != 15 {
            return 0;
        }
        let section = pos.section();
        let mut count = 0;
        while !self
            .storage
            .storing_light_for_section(section.offset(0, -count - 1, 0))
            && self
                .storage
                .has_light_data_at_or_below(section.y - count - 1)
        {
            count += 1;
        }
        count
    }

    fn propagate_from_empty_sections(
        &mut self,
        pos: Pos,
        direction: Direction,
        level: u8,
        increase: bool,
        empty_sections: i32,
    ) {
        if empty_sections == 0 || !crossed_section_edge(direction, pos.x & 15, pos.z & 15) {
            return;
        }
        let top = (pos.y >> 4) - 1;
        let bottom = top - empty_sections + 1;
        for section_y in (bottom..=top).rev() {
            if !self.storage.storing_light_for_section(SectionPos::new(
                pos.x >> 4,
                section_y,
                pos.z >> 4,
            )) {
                continue;
            }
            let base = section_y << 4;
            for local_y in (0..16).rev() {
                let target = Pos::new(pos.x, base + local_y, pos.z);
                if increase {
                    self.storage.set_stored_level(target, level);
                    if level > 1 {
                        self.enqueue_increase(
                            target,
                            entry::increase_skip_one_direction(level, true, direction.opposite()),
                        );
                    }
                } else {
                    self.storage.set_stored_level(target, 0);
                    self.enqueue_decrease(
                        target,
                        entry::decrease_skip_one_direction(level, direction.opposite()),
                    );
                }
            }
        }
    }

    fn propagate_sky_sources(&mut self, source: &dyn ChunkSource, column: ChunkPos) {
        self.storage.set_light_enabled(column, true);
        let empty = &self.empty_sources;
        let sources = |x: i32, z: i32| -> &SkyLightSources {
            source
                .chunk(ChunkPos::new(x, z))
                .map_or(empty, |chunk| chunk.sky_sources)
        };
        let center = sources(column.x, column.z);
        let north = sources(column.x, column.z - 1);
        let south = sources(column.x, column.z + 1);
        let west = sources(column.x - 1, column.z);
        let east = sources(column.x + 1, column.z);
        let mut lowest = [0i32; 256];
        let mut lowest_north = [0i32; 256];
        let mut lowest_south = [0i32; 256];
        let mut lowest_west = [0i32; 256];
        let mut lowest_east = [0i32; 256];
        let mut neighbors_max = [0i32; 256];
        let (mut min_lowest, mut max_lowest, mut max_push) = (i32::MAX, i32::MIN, i32::MIN);
        for z in 0..16 {
            for x in 0..16 {
                let i = (z << 4 | x) as usize;
                lowest[i] = center.lowest_source_y(x, z);
                lowest_north[i] = if z == 0 {
                    north.lowest_source_y(x, 15)
                } else {
                    center.lowest_source_y(x, z - 1)
                };
                lowest_south[i] = if z == 15 {
                    south.lowest_source_y(x, 0)
                } else {
                    center.lowest_source_y(x, z + 1)
                };
                lowest_west[i] = if x == 0 {
                    west.lowest_source_y(15, z)
                } else {
                    center.lowest_source_y(x - 1, z)
                };
                lowest_east[i] = if x == 15 {
                    east.lowest_source_y(0, z)
                } else {
                    center.lowest_source_y(x + 1, z)
                };
                neighbors_max[i] = lowest_north[i]
                    .max(lowest_south[i])
                    .max(lowest_west[i].max(lowest_east[i]));
                min_lowest = min_lowest.min(lowest[i]);
                max_lowest = max_lowest.max(lowest[i]);
                max_push = max_push
                    .max(lowest[i])
                    .max(neighbors_max[i].saturating_sub(1));
            }
        }
        let top = self.storage.top_section_y(column);
        let bottom = self.storage.bottom_section_y();
        let (base_x, base_z) = (column.x << 4, column.z << 4);
        let mut pending = Vec::new();
        let mut section_y = top - 1;
        while section_y >= bottom {
            let Some(layer) = self
                .storage
                .layer_mut(SectionPos::new(column.x, section_y, column.z))
            else {
                section_y -= 1;
                continue;
            };
            let base_y = section_y << 4;
            let top_y = base_y + 15;
            if min_lowest > top_y {
                break;
            }
            for local_y in 0..16 {
                let y = base_y + local_y;
                if y < min_lowest {
                    continue;
                }
                if y >= max_lowest {
                    layer.fill_slice(local_y, 15);
                    continue;
                }
                for z in 0..16 {
                    for x in 0..16 {
                        if lowest[(z << 4 | x) as usize] <= y {
                            layer.set(x, local_y, z, 15);
                        }
                    }
                }
            }
            if max_push >= base_y {
                for z in 0..16 {
                    for x in 0..16 {
                        let i = (z << 4 | x) as usize;
                        let lowest = lowest[i];
                        if lowest > top_y {
                            continue;
                        }
                        let mut push = |y: i32| {
                            pending.push((
                                Pos::new(base_x + x, y, base_z + z),
                                entry::increase_sky_source_in_directions(
                                    y == lowest,
                                    y < lowest_north[i],
                                    y < lowest_south[i],
                                    y < lowest_west[i],
                                    y < lowest_east[i],
                                ),
                            ));
                        };
                        let range_top = top_y.min(neighbors_max[i].saturating_sub(1));
                        for y in (base_y.max(lowest)..=range_top).rev() {
                            push(y);
                        }
                        if lowest >= neighbors_max[i] && (base_y..=top_y).contains(&lowest) {
                            push(lowest);
                        }
                    }
                }
            }
            if min_lowest >= base_y {
                break;
            }
            section_y -= 1;
        }
        self.increase.extend(pending);
    }

    pub fn layer_data(&self, section: SectionPos) -> Option<&DataLayer> {
        self.storage.data_layer_data(section)
    }
}

#[cfg(test)]
mod reference_tests {
    use super::*;

    impl LayerEngine {
        pub(super) fn propagate_sky_sources_reference(
            &mut self,
            source: &dyn ChunkSource,
            column: ChunkPos,
        ) {
            self.storage.set_light_enabled(column, true);
            let empty = &self.empty_sources;
            let sources = |x: i32, z: i32| -> &SkyLightSources {
                source
                    .chunk(ChunkPos::new(x, z))
                    .map_or(empty, |chunk| chunk.sky_sources)
            };
            let center = sources(column.x, column.z);
            let north = sources(column.x, column.z - 1);
            let south = sources(column.x, column.z + 1);
            let west = sources(column.x - 1, column.z);
            let east = sources(column.x + 1, column.z);
            let top = self.storage.top_section_y(column);
            let bottom = self.storage.bottom_section_y();
            let (base_x, base_z) = (column.x << 4, column.z << 4);
            let mut pending = Vec::new();
            let mut section_y = top - 1;
            while section_y >= bottom {
                let Some(layer) = self
                    .storage
                    .layer_mut(SectionPos::new(column.x, section_y, column.z))
                else {
                    section_y -= 1;
                    continue;
                };
                let base_y = section_y << 4;
                let top_y = base_y + 15;
                let mut continues_below = false;
                for z in 0..16 {
                    for x in 0..16 {
                        let lowest = center.lowest_source_y(x, z);
                        if lowest > top_y {
                            continue;
                        }
                        let lowest_north = if z == 0 {
                            north.lowest_source_y(x, 15)
                        } else {
                            center.lowest_source_y(x, z - 1)
                        };
                        let lowest_south = if z == 15 {
                            south.lowest_source_y(x, 0)
                        } else {
                            center.lowest_source_y(x, z + 1)
                        };
                        let lowest_west = if x == 0 {
                            west.lowest_source_y(15, z)
                        } else {
                            center.lowest_source_y(x - 1, z)
                        };
                        let lowest_east = if x == 15 {
                            east.lowest_source_y(0, z)
                        } else {
                            center.lowest_source_y(x + 1, z)
                        };
                        let neighbors_max = lowest_north
                            .max(lowest_south)
                            .max(lowest_west.max(lowest_east));
                        let mut y = top_y;
                        while y >= base_y.max(lowest) {
                            layer.set(x, y & 15, z, 15);
                            if y == lowest || y < neighbors_max {
                                pending.push((
                                    Pos::new(base_x + x, y, base_z + z),
                                    entry::increase_sky_source_in_directions(
                                        y == lowest,
                                        y < lowest_north,
                                        y < lowest_south,
                                        y < lowest_west,
                                        y < lowest_east,
                                    ),
                                ));
                            }
                            y -= 1;
                        }
                        if lowest < base_y {
                            continues_below = true;
                        }
                    }
                }
                if !continues_below {
                    break;
                }
                section_y -= 1;
            }
            self.increase.extend(pending);
        }
    }
    use std::collections::HashMap;

    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};

    use crate::chunk::{Chunk, ChunkShape};
    use crate::light::{LevelLight, LightChunk};

    const SHAPE: ChunkShape = ChunkShape::OVERWORLD;

    struct World(HashMap<ChunkPos, (Chunk, SkyLightSources)>);

    impl ChunkSource for World {
        fn chunk(&self, pos: ChunkPos) -> Option<LightChunk<'_>> {
            self.0.get(&pos).map(|(blocks, sky_sources)| LightChunk {
                blocks,
                sky_sources,
            })
        }
    }

    fn random_chunk(rng: &mut StdRng) -> Chunk {
        let mut chunk = Chunk::empty(SHAPE);
        let stone = Block::Stone.default_state();
        let water = Block::Water.default_state();
        let base = rng.gen_range(SHAPE.min_y - 20..SHAPE.min_y + SHAPE.height);
        let rough = rng.gen_range(0..40);
        for z in 0..16 {
            for x in 0..16 {
                let height = match rng.gen_range(0..12) {
                    0 => SHAPE.min_y - 1,
                    1 => SHAPE.min_y + SHAPE.height - 1,
                    _ => base + rng.gen_range(-rough..=rough),
                };
                let top = height.clamp(SHAPE.min_y - 1, SHAPE.min_y + SHAPE.height - 1);
                let from = (top - rng.gen_range(0..4)).max(SHAPE.min_y);
                for y in from..=top {
                    chunk.set(x, y, z, stone);
                }
                if rng.gen_range(0..6) == 0 {
                    let y = rng.gen_range(SHAPE.min_y..SHAPE.min_y + SHAPE.height);
                    chunk.set(x, y, z, if rng.gen_bool(0.5) { stone } else { water });
                }
            }
        }
        chunk
    }

    fn engines(seed: u64) -> (World, ChunkPos, Vec<ChunkPos>) {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut world = World(HashMap::new());
        let mut positions = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                if (dx, dz) != (0, 0) && rng.gen_bool(0.3) {
                    continue;
                }
                let chunk = random_chunk(&mut rng);
                let sources = SkyLightSources::from_chunk(&chunk);
                let pos = ChunkPos::new(dx, dz);
                world.0.insert(pos, (chunk, sources));
                positions.push(pos);
            }
        }
        (world, ChunkPos::new(0, 0), positions)
    }

    #[tuff_core::watchdog]
    #[test]
    fn slice_wise_sky_sources_equal_the_per_cell_reference() {
        for seed in 0..24 {
            let (world, _, positions) = engines(seed);
            let mut fast = LevelLight::new(SHAPE, true);
            let mut reference = LevelLight::new(SHAPE, true);
            for &pos in &positions {
                fast.initialize_light(&world, pos, false);
                reference.initialize_light(&world, pos, false);
            }
            for &pos in &positions {
                fast.sky
                    .as_mut()
                    .unwrap()
                    .propagate_sky_sources(&world, pos);
                reference
                    .sky
                    .as_mut()
                    .unwrap()
                    .propagate_sky_sources_reference(&world, pos);
                let (fast, reference) =
                    (fast.sky.as_ref().unwrap(), reference.sky.as_ref().unwrap());
                assert_eq!(fast.increase, reference.increase, "seed {seed} queue");
                for &column in &positions {
                    for y in -5..=25 {
                        let section = SectionPos::new(column.x, y, column.z);
                        assert_eq!(
                            fast.layer_data(section),
                            reference.layer_data(section),
                            "seed {seed} section {section:?}"
                        );
                    }
                }
            }
        }
    }
}
