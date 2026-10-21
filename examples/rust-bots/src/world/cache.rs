// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::rc::Rc;

use super::{Blocks, Class, Pending};

pub const SECTION_VOLUME: usize = 16 * 16 * 16;
const NEGATIVE_TTL_TICKS: u64 = 100;
pub const DEFAULT_CAPACITY: usize = 4096;
pub const MIN_Y: i32 = -64;
pub const MAX_Y: i32 = 320;

/// A 16^3 section by its section coordinates.
pub type SectionKey = (i32, i32, i32);

pub trait SectionSource {
    /// The classes of one section, x fastest, then z, then y; none when the
    /// read failed.
    fn load(&mut self, key: SectionKey) -> Option<Box<[Class]>>;

    /// The block states of the section `load` just returned, when the
    /// source knows them.
    fn take_states(&mut self) -> Option<States> {
        None
    }

    /// Block positions in one section matching these names; none when
    /// the source cannot tell.
    fn find(&mut self, _key: SectionKey, _wanted: &[String]) -> Vec<(i32, i32, i32)> {
        Vec::new()
    }
}

/// A section's block states: a palette and one index per block, in the
/// same order as the classes.
pub struct States {
    pub palette: Box<[Rc<str>]>,
    pub indices: Box<[u16]>,
}

struct Entry {
    data: Option<Box<[Class]>>,
    states: Option<States>,
    used: u64,
    loaded: u64,
}

/// Sections read on demand, bounded by an LRU capacity and a per-tick read
/// budget so many bots cannot starve the host.
pub struct BlockCache<S> {
    source: S,
    sections: HashMap<SectionKey, Entry>,
    capacity: usize,
    tick: u64,
    clock: u64,
    reads_left: usize,
    reads: u64,
}

pub fn section_of(x: i32, y: i32, z: i32) -> SectionKey {
    (x >> 4, y >> 4, z >> 4)
}

fn index_in_section(x: i32, y: i32, z: i32) -> usize {
    (((y & 15) << 8) | ((z & 15) << 4) | (x & 15)) as usize
}

impl<S: SectionSource> BlockCache<S> {
    pub fn new(source: S) -> Self {
        Self::with_capacity(source, DEFAULT_CAPACITY)
    }

    pub fn with_capacity(source: S, capacity: usize) -> Self {
        Self {
            source,
            sections: HashMap::new(),
            capacity: capacity.max(1),
            tick: 0,
            clock: 0,
            reads_left: usize::MAX,
            reads: 0,
        }
    }

    pub fn source(&mut self) -> &mut S {
        &mut self.source
    }

    /// Starts a tick with `reads` section reads allowed.
    pub fn begin_tick(&mut self, reads: usize) {
        self.tick += 1;
        self.reads_left = reads;
    }

    pub fn len(&self) -> usize {
        self.sections.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// Section reads made since the cache was created.
    pub fn reads(&self) -> u64 {
        self.reads
    }

    pub fn invalidate_block(&mut self, x: i32, y: i32, z: i32) {
        self.sections.remove(&section_of(x, y, z));
    }

    /// Drops every section a box of blocks touches.
    pub fn invalidate_box(&mut self, min: (i32, i32, i32), max: (i32, i32, i32)) {
        let (a, b) = (
            section_of(min.0, min.1, min.2),
            section_of(max.0, max.1, max.2),
        );
        for sx in a.0..=b.0 {
            for sy in a.1..=b.1 {
                for sz in a.2..=b.2 {
                    self.sections.remove(&(sx, sy, sz));
                }
            }
        }
    }

    pub fn invalidate_all(&mut self) {
        self.sections.clear();
    }

    fn evict(&mut self) {
        let keep = self.capacity - self.capacity / 8;
        if self.sections.len() <= keep {
            return;
        }
        let mut by_use: Vec<(u64, SectionKey)> =
            self.sections.iter().map(|(k, e)| (e.used, *k)).collect();
        by_use.sort_unstable();
        let drop = self.sections.len() - keep;
        for (_, key) in by_use.into_iter().take(drop) {
            self.sections.remove(&key);
        }
    }

    fn ensure(&mut self, key: SectionKey) -> Result<(), Pending> {
        self.clock += 1;
        let now = self.clock;
        if let Some(entry) = self.sections.get_mut(&key) {
            let stale = entry.data.is_none()
                && self.tick.saturating_sub(entry.loaded) >= NEGATIVE_TTL_TICKS;
            if !stale {
                entry.used = now;
                return Ok(());
            }
        }
        if self.reads_left == 0 {
            return Err(Pending);
        }
        self.reads_left -= 1;
        self.reads += 1;
        let data = self.source.load(key);
        let states = data.as_ref().and_then(|_| self.source.take_states());
        self.sections.insert(
            key,
            Entry {
                data,
                states,
                used: now,
                loaded: self.tick,
            },
        );
        if self.sections.len() > self.capacity {
            self.evict();
        }
        Ok(())
    }
}

impl<S: SectionSource> Blocks for BlockCache<S> {
    fn class_at(&mut self, x: i32, y: i32, z: i32) -> Result<Class, Pending> {
        if !(MIN_Y..MAX_Y).contains(&y) {
            return Ok(Class::NONE);
        }
        let key = section_of(x, y, z);
        self.ensure(key)?;
        Ok(self
            .sections
            .get(&key)
            .and_then(|e| e.data.as_ref())
            .map_or(Class::NONE, |d| d[index_in_section(x, y, z)]))
    }

    fn state_at(&mut self, x: i32, y: i32, z: i32) -> Result<Option<Rc<str>>, Pending> {
        if !(MIN_Y..MAX_Y).contains(&y) {
            return Ok(None);
        }
        let key = section_of(x, y, z);
        self.ensure(key)?;
        Ok(self
            .sections
            .get(&key)
            .and_then(|e| e.states.as_ref())
            .and_then(|s| {
                let i = *s.indices.get(index_in_section(x, y, z))?;
                s.palette.get(usize::from(i)).cloned()
            }))
    }

    fn find(
        &mut self,
        section: SectionKey,
        wanted: &[String],
    ) -> Result<Vec<(i32, i32, i32)>, Pending> {
        if self.reads_left == 0 {
            return Err(Pending);
        }
        self.reads_left -= 1;
        self.reads += 1;
        Ok(self.source.find(section, wanted))
    }

    fn refresh(&mut self, around: &[(i32, i32, i32)]) {
        for &(x, y, z) in around {
            self.invalidate_box((x - 1, y - 1, z - 1), (x + 1, y + 2, z + 1));
        }
    }
}
