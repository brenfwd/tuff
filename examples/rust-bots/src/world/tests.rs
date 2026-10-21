// SPDX-License-Identifier: MIT
use std::cell::RefCell;
use std::rc::Rc;

use super::*;

pub(crate) struct Terrain {
    pub cells: std::collections::HashMap<(i32, i32, i32), Class>,
    pub names: std::collections::HashMap<(i32, i32, i32), &'static str>,
    pub w: i32,
    pub d: i32,
    pub ores: std::collections::BTreeSet<(i32, i32, i32)>,
}

impl Terrain {
    pub fn new(w: i32, d: i32) -> Self {
        Self {
            cells: Default::default(),
            names: Default::default(),
            w,
            d,
            ores: Default::default(),
        }
    }

    pub fn class(&self, p: (i32, i32, i32)) -> Class {
        if let Some(c) = self.cells.get(&p) {
            return *c;
        }
        let (x, y, z) = p;
        if x < 0 || z < 0 || x >= self.w || z >= self.d || !(-40..40).contains(&y) {
            Class::NONE
        } else if y == 0 {
            Class::STANDABLE
        } else {
            Class::PASSABLE
        }
    }

    pub fn set(&mut self, p: (i32, i32, i32), c: Class) {
        self.cells.insert(p, c);
        self.names.remove(&p);
    }

    /// A named block: its class and the state name breaking looks up.
    pub fn block(&mut self, p: (i32, i32, i32), name: &'static str, c: Class) {
        self.cells.insert(p, c);
        self.names.insert(p, name);
    }

    pub fn state(&self, p: (i32, i32, i32)) -> Option<Rc<str>> {
        if let Some(name) = self.names.get(&p) {
            return Some(Rc::from(*name));
        }
        let c = self.class(p);
        if c == Class::NONE {
            None
        } else if c.standable() {
            Some(Rc::from("minecraft:stone"))
        } else if c.passable() && !c.water() && !c.climbable() {
            Some(Rc::from("minecraft:air"))
        } else {
            None
        }
    }

    pub fn wall(&mut self, p: (i32, i32, i32)) {
        self.set(p, Class::STANDABLE);
    }
}

impl Terrain {
    pub fn ore(&mut self, p: (i32, i32, i32)) {
        self.wall(p);
        self.ores.insert(p);
    }

    pub fn ores_in(&self, key: SectionKey) -> Vec<(i32, i32, i32)> {
        self.ores
            .iter()
            .copied()
            .filter(|p| (p.0 >> 4, p.1 >> 4, p.2 >> 4) == key)
            .collect()
    }

    pub fn dig(&mut self, p: (i32, i32, i32)) {
        self.ores.remove(&p);
        self.set(p, Class::PASSABLE);
    }
}

impl Blocks for Terrain {
    fn class_at(&mut self, x: i32, y: i32, z: i32) -> Result<Class, Pending> {
        Ok(self.class((x, y, z)))
    }

    fn state_at(&mut self, x: i32, y: i32, z: i32) -> Result<Option<Rc<str>>, Pending> {
        Ok(self.state((x, y, z)))
    }

    fn find(&mut self, key: SectionKey, _: &[String]) -> Result<Vec<(i32, i32, i32)>, Pending> {
        Ok(self.ores_in(key))
    }
}

/// Break times by block name, as a bot's hotbar would give them.
pub(crate) struct TableTools(pub Vec<(&'static str, u32)>);

impl crate::path::Tools for TableTools {
    fn break_tool(&mut self, state: &str) -> Option<crate::path::BreakTool> {
        let name = name_of(state);
        self.0
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, ticks)| crate::path::BreakTool {
                ticks: *ticks,
                slot: 0,
            })
    }
}

pub(crate) struct TerrainSource(pub Rc<RefCell<Terrain>>);

impl SectionSource for TerrainSource {
    fn load(&mut self, key: SectionKey) -> Option<Box<[Class]>> {
        let t = self.0.borrow();
        let mut out = Vec::with_capacity(SECTION_VOLUME);
        for ly in 0..16 {
            for lz in 0..16 {
                for lx in 0..16 {
                    out.push(t.class((key.0 * 16 + lx, key.1 * 16 + ly, key.2 * 16 + lz)));
                }
            }
        }
        Some(out.into_boxed_slice())
    }
}

fn traits(solid: bool, air: bool, liquid: bool) -> Traits {
    Traits {
        solid,
        air,
        liquid,
        replaceable: false,
    }
}

#[test]
fn classify_covers_the_block_kinds() {
    assert_eq!(
        classify("minecraft:air", traits(false, true, false)),
        Class::PASSABLE
    );
    assert_eq!(
        classify("minecraft:stone", traits(true, false, false)),
        Class::STANDABLE
    );
    assert!(classify("minecraft:water[level=0]", traits(false, false, true)).water());
    assert!(classify("minecraft:lava", traits(false, false, true)).danger());
    assert!(
        classify(
            "minecraft:ladder[facing=north]",
            traits(false, false, false)
        )
        .climbable()
    );
    assert!(classify("minecraft:cactus", traits(true, false, false)).danger());
    assert!(classify("minecraft:magma_block", traits(true, false, false)).danger());
    assert!(classify("minecraft:powder_snow", traits(false, false, false)).danger());
    assert!(!classify("minecraft:oak_fence", traits(true, false, false)).standable());
    let fire = classify("minecraft:fire", traits(false, false, false));
    assert!(fire.danger() && !fire.passable());
}

#[test]
fn unloaded_and_out_of_world_blocks_are_walls() {
    struct Nothing;
    impl SectionSource for Nothing {
        fn load(&mut self, _: SectionKey) -> Option<Box<[Class]>> {
            None
        }
    }
    let mut cache = BlockCache::new(Nothing);
    assert_eq!(cache.class_at(0, 70, 0), Ok(Class::NONE));
    assert_eq!(cache.class_at(0, -65, 0), Ok(Class::NONE));
    assert_eq!(cache.class_at(0, 400, 0), Ok(Class::NONE));
}

#[test]
fn reads_stop_at_the_tick_budget_and_resume_next_tick() {
    let world = Rc::new(RefCell::new(Terrain::new(64, 64)));
    let mut cache = BlockCache::new(TerrainSource(world));
    cache.begin_tick(1);
    assert!(cache.class_at(0, 1, 0).is_ok());
    assert!(cache.class_at(1, 1, 0).is_ok());
    assert_eq!(cache.class_at(20, 1, 0), Err(Pending));
    cache.begin_tick(1);
    assert!(cache.class_at(20, 1, 0).is_ok());
    assert_eq!(cache.reads(), 2);
}

#[test]
fn the_least_recently_used_sections_go_first() {
    let world = Rc::new(RefCell::new(Terrain::new(512, 512)));
    let mut cache = BlockCache::with_capacity(TerrainSource(world), 8);
    for i in 0..8 {
        cache.class_at(i * 16, 1, 0).unwrap();
    }
    cache.class_at(0, 1, 0).unwrap();
    cache.class_at(9 * 16, 1, 0).unwrap();
    assert!(cache.len() <= 8);
    let before = cache.reads();
    cache.class_at(0, 1, 0).unwrap();
    assert_eq!(cache.reads(), before);
}

#[test]
fn invalidating_a_block_rereads_its_section() {
    let world = Rc::new(RefCell::new(Terrain::new(64, 64)));
    let mut cache = BlockCache::new(TerrainSource(world.clone()));
    assert_eq!(cache.class_at(3, 1, 3), Ok(Class::PASSABLE));
    world.borrow_mut().wall((3, 1, 3));
    assert_eq!(cache.class_at(3, 1, 3), Ok(Class::PASSABLE));
    cache.invalidate_block(3, 1, 3);
    assert_eq!(cache.class_at(3, 1, 3), Ok(Class::STANDABLE));
}

#[test]
fn falling_blocks_and_fluids_are_marked() {
    let sand = classify("minecraft:sand", traits(true, false, false));
    assert!(sand.standable() && sand.falling());
    assert!(classify("minecraft:lime_concrete_powder", traits(true, false, false)).falling());
    assert!(!classify("minecraft:stone", traits(true, false, false)).falling());
    assert!(classify("minecraft:water[level=0]", traits(false, false, true)).fluid());
    assert!(classify("minecraft:lava[level=0]", traits(false, false, true)).fluid());
    assert!(!classify("minecraft:air", traits(false, true, false)).fluid());
}

#[test]
fn the_cache_keeps_block_states_its_source_reports() {
    struct Named;
    impl SectionSource for Named {
        fn load(&mut self, _: SectionKey) -> Option<Box<[Class]>> {
            Some(vec![Class::STANDABLE; SECTION_VOLUME].into_boxed_slice())
        }

        fn take_states(&mut self) -> Option<States> {
            let mut indices = vec![0u16; SECTION_VOLUME];
            indices[1] = 1;
            Some(States {
                palette: vec![Rc::from("minecraft:stone"), Rc::from("minecraft:dirt")].into(),
                indices: indices.into(),
            })
        }
    }
    let mut cache = BlockCache::new(Named);
    assert_eq!(
        cache.state_at(0, 0, 0).unwrap().as_deref(),
        Some("minecraft:stone")
    );
    assert_eq!(
        cache.state_at(1, 0, 0).unwrap().as_deref(),
        Some("minecraft:dirt")
    );
    assert_eq!(cache.state_at(0, 400, 0).unwrap(), None);
}
