// SPDX-License-Identifier: MIT
//! The host-backed block source, event invalidation and per-tick read
//! budget the plugin shares between its bots.

use std::collections::HashMap;

use tuff_plugin_sdk::types::{BlockPos, Traits};
use tuff_plugin_sdk::world::{BlockState, World};

use std::rc::Rc;

use super::{
    BlockCache, Class, SECTION_VOLUME, SectionKey, SectionSource, States, classify, name_of,
};
use crate::world::Traits as BlockTraits;

/// Section reads all bots may make in one tick.
pub const READS_PER_TICK: usize = 16;

pub struct HostSource {
    world: World,
    palette: HashMap<String, Class>,
    states: Option<States>,
}

impl HostSource {
    pub fn new(dimension: &str) -> Self {
        Self {
            world: World::new(dimension),
            palette: HashMap::new(),
            states: None,
        }
    }

    fn class_of(&mut self, state: &str) -> Class {
        if let Some(class) = self.palette.get(state) {
            return *class;
        }
        let traits = BlockState::new(state).traits().unwrap_or(Traits::empty());
        let class = classify(
            state,
            BlockTraits {
                solid: traits.contains(Traits::SOLID),
                air: traits.contains(Traits::AIR),
                liquid: traits.contains(Traits::LIQUID),
                replaceable: traits.contains(Traits::REPLACEABLE),
            },
        );
        self.palette.insert(state.to_string(), class);
        class
    }
}

impl SectionSource for HostSource {
    fn find(&mut self, key: SectionKey, wanted: &[String]) -> Vec<(i32, i32, i32)> {
        let (bx, by, bz) = (key.0 * 16, key.1 * 16, key.2 * 16);
        let Ok(grid) = self.world.blocks(
            BlockPos {
                x: bx,
                y: by,
                z: bz,
            },
            BlockPos {
                x: bx + 15,
                y: by + 15,
                z: bz + 15,
            },
        ) else {
            return Vec::new();
        };
        (0..SECTION_VOLUME)
            .filter(|i| {
                grid.get(*i)
                    .is_some_and(|state| wanted.iter().any(|w| w == name_of(state)))
            })
            .map(|i| {
                let i = i as i32;
                (bx + (i & 15), by + (i >> 8), bz + ((i >> 4) & 15))
            })
            .collect()
    }

    fn load(&mut self, key: SectionKey) -> Option<Box<[Class]>> {
        let (bx, by, bz) = (key.0 * 16, key.1 * 16, key.2 * 16);
        let grid = self
            .world
            .blocks(
                BlockPos {
                    x: bx,
                    y: by,
                    z: bz,
                },
                BlockPos {
                    x: bx + 15,
                    y: by + 15,
                    z: bz + 15,
                },
            )
            .ok()?;
        let mut out = Vec::with_capacity(SECTION_VOLUME);
        for i in 0..SECTION_VOLUME {
            out.push(match grid.get(i) {
                Some(state) => self.class_of(state),
                None => Class::NONE,
            });
        }
        self.states = Some(States {
            palette: grid
                .palette()
                .iter()
                .map(|s| Rc::from(s.as_str()))
                .collect(),
            indices: grid.indices().into(),
        });
        Some(out.into_boxed_slice())
    }

    fn take_states(&mut self) -> Option<States> {
        self.states.take()
    }
}

/// One cache per dimension.
#[derive(Default)]
pub struct Worlds {
    caches: HashMap<String, BlockCache<HostSource>>,
}

impl Worlds {
    pub fn cache(&mut self, dimension: &str) -> &mut BlockCache<HostSource> {
        self.caches
            .entry(dimension.to_string())
            .or_insert_with(|| BlockCache::new(HostSource::new(dimension)))
    }

    /// Starts a tick; the read budget is shared across dimensions.
    pub fn begin_tick(&mut self) {
        let share = (READS_PER_TICK / self.caches.len().max(1)).max(1);
        for cache in self.caches.values_mut() {
            cache.begin_tick(share);
        }
    }

    pub fn block_changed(&mut self, dimension: &str, at: &BlockPos) {
        if let Some(cache) = self.caches.get_mut(dimension) {
            cache.invalidate_block(at.x, at.y, at.z);
        }
    }

    pub fn exploded(&mut self, dimension: &str, center: (f64, f64, f64), radius: f64) {
        let Some(cache) = self.caches.get_mut(dimension) else {
            return;
        };
        let r = radius.ceil() as i32 + 1;
        let (x, y, z) = (center.0 as i32, center.1 as i32, center.2 as i32);
        cache.invalidate_box((x - r, y - r, z - r), (x + r, y + r, z + r));
    }
}
