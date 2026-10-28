use std::collections::HashSet;

use super::*;
use tuff_world_model::chunk::{ChunkShape, SECTION_VOLUME, plains};

const SHAPE: ChunkShape = ChunkShape::OVERWORLD;
const POS: ChunkPos = ChunkPos::new(3, -2);

fn stone() -> BlockState {
    Block::Stone.default_state()
}

fn solid_chunk() -> Chunk {
    Chunk::uniform(SHAPE, stone(), plains())
}

fn surrounded() -> [Option<Chunk>; 4] {
    std::array::from_fn(|_| Some(solid_chunk()))
}

fn overworld_plan(mode: Mode) -> Arc<Plan> {
    let (_, hidden, replacement) = DIMENSIONS[0];
    Arc::new(
        Plan::new(
            mode,
            64,
            2,
            &parse_blocks(hidden).unwrap(),
            &parse_blocks(replacement).unwrap(),
            7,
        )
        .unwrap(),
    )
}

fn sent(chunk: &Chunk, around: &[Option<Chunk>; 4], plan: &Arc<Plan>) -> Chunk {
    obfuscate(chunk, POS, around, plan, &mut Cache::default()).0
}

fn every_block(mut f: impl FnMut(i32, i32, i32)) {
    for y in SHAPE.min_y..SHAPE.max_y() {
        for z in 0..16 {
            for x in 0..16 {
                f(x, y, z);
            }
        }
    }
}

fn section_of(y: i32) -> usize {
    ((y - SHAPE.min_y) / 16) as usize
}

#[tuff_core::watchdog]
#[test]
fn enclosed_ores_are_sent_as_their_surroundings_and_exposed_ones_as_they_are() {
    let diamond = Block::DiamondOre.default_state();
    let deep = Block::DeepslateDiamondOre.default_state();
    let mut chunk = solid_chunk();
    chunk.set(5, 10, 5, diamond);
    chunk.set(8, 10, 8, diamond);
    chunk.set(8, 11, 8, Block::Air.default_state());
    chunk.set(2, 20, 2, Block::Granite.default_state());
    chunk.set(2, 21, 2, Block::IronOre.default_state());
    for (x, y, z) in offsets(1) {
        chunk.set(10 + x, -30 + y, 10 + z, Block::Deepslate.default_state());
    }
    chunk.set(10, -30, 10, deep);
    chunk.set(1, 90, 1, diamond);
    let plan = overworld_plan(Mode::Hide);
    let out = sent(&chunk, &surrounded(), &plan);

    assert_eq!(out.get(5, 10, 5), stone());
    assert_eq!(out.get(8, 10, 8), diamond, "an ore touching air shows");
    assert_eq!(out.get(2, 21, 2), stone(), "stone is preferred to granite");
    assert_eq!(out.get(10, -30, 10), Block::Deepslate.default_state());
    assert_eq!(out.get(1, 90, 1), diamond, "above the highest hidden y");

    let mut differing = 0;
    every_block(|x, y, z| {
        let (real, shown) = (chunk.get(x, y, z), out.get(x, y, z));
        if real != shown {
            differing += 1;
            assert!(plan.hides(real), "{real:?} at {x} {y} {z} was not hidden");
            assert!(occludes(shown));
        }
    });
    assert_eq!(differing, 3);
    assert_eq!(out.light(), chunk.light());
}

#[tuff_core::watchdog]
#[test]
fn a_hidden_block_with_only_hidden_neighbours_takes_the_fallback() {
    let mut chunk = solid_chunk();
    for (x, y, z) in offsets(1) {
        chunk.set(6 + x, y, 6 + z, Block::CoalOre.default_state());
    }
    chunk.set(6, 0, 6, Block::GoldOre.default_state());
    let out = sent(&chunk, &surrounded(), &overworld_plan(Mode::Hide));
    assert_eq!(out.get(6, 0, 6), stone());
    assert_eq!(out.get(6, 1, 6), stone());
}

#[tuff_core::watchdog]
#[test]
fn edges_read_the_neighbouring_chunks_and_sections() {
    let diamond = Block::DiamondOre.default_state();
    let mut chunk = solid_chunk();
    chunk.set(0, 5, 3, diamond);
    chunk.set(15, 6, 3, diamond);
    chunk.set(4, 15, 4, diamond);
    chunk.set(4, 16, 4, Block::Glass.default_state());
    chunk.set(9, SHAPE.min_y, 9, diamond);
    let plan = overworld_plan(Mode::Hide);

    let out = sent(&chunk, &surrounded(), &plan);
    assert_eq!(out.get(0, 5, 3), stone());
    assert_eq!(out.get(15, 6, 3), stone());
    assert_eq!(
        out.get(4, 15, 4),
        diamond,
        "glass above, in the next section"
    );
    assert_eq!(
        out.get(9, SHAPE.min_y, 9),
        stone(),
        "nothing sees from below the world"
    );

    let mut open_west = surrounded();
    open_west[0] = None;
    let mut east = solid_chunk();
    east.set(0, 6, 3, Block::Water.default_state());
    open_west[1] = Some(east);
    let out = sent(&chunk, &open_west, &plan);
    assert_eq!(
        out.get(0, 5, 3),
        diamond,
        "an unknown neighbour counts as open"
    );
    assert_eq!(out.get(15, 6, 3), diamond, "water in the next chunk");
}

#[tuff_core::watchdog]
#[test]
fn fill_mode_turns_enclosed_stone_into_random_hidden_blocks() {
    let mut chunk = solid_chunk();
    for x in 0..16 {
        for z in 0..16 {
            chunk.set(x, 40, z, Block::Air.default_state());
        }
    }
    chunk.set(3, 30, 3, Block::DiamondOre.default_state());
    let plan = overworld_plan(Mode::Fill);
    let out = sent(&chunk, &surrounded(), &plan);
    assert_eq!(out.get(5, 39, 5), stone(), "stone under air shows");
    assert_eq!(out.get(5, 41, 5), stone(), "stone over air shows");
    assert_eq!(out.get(5, 70, 5), stone(), "above the highest hidden y");
    let mut kinds = HashSet::new();
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                let shown = out.get(x, y, z);
                assert!(plan.pool.contains(&shown), "{shown:?}");
                assert!(!shown.block().creates_block_entity());
                kinds.insert(shown);
            }
        }
    }
    assert!(kinds.len() > 10, "{}", kinds.len());
    let again = sent(&chunk, &surrounded(), &plan);
    assert_eq!(
        again.section(section_of(20)).block_array(),
        out.section(section_of(20)).block_array(),
        "stable for one seed"
    );
}

#[tuff_core::watchdog]
#[test]
fn unchanged_sections_come_from_the_cache() {
    let mut chunk = solid_chunk();
    chunk.set(5, 10, 5, Block::DiamondOre.default_state());
    chunk.set(5, 50, 5, Block::DiamondOre.default_state());
    let around = surrounded();
    let plan = overworld_plan(Mode::Fill);
    let mut cache = Cache::default();
    let counted = (0..SHAPE.sections())
        .filter(|i| SHAPE.section_base(*i) <= 64)
        .count() as u32;
    let (first, stats) = obfuscate(&chunk, POS, &around, &plan, &mut cache);
    assert_eq!((stats.computed, stats.reused), (counted, 0));
    assert!(stats.replaced > 0);

    let (second, stats) = obfuscate(&chunk, POS, &around, &plan, &mut cache);
    assert_eq!(
        (stats.computed, stats.reused, stats.replaced),
        (0, counted, 0)
    );
    for index in 0..SHAPE.sections() {
        assert!(Arc::ptr_eq(
            first.shared_section(index),
            second.shared_section(index)
        ));
    }

    let mut edited = chunk.clone();
    edited.set(5, 10, 5, Block::Air.default_state());
    let section = section_of(10);
    let (third, stats) = obfuscate(&edited, POS, &around, &plan, &mut cache);
    assert_eq!(stats.computed, 3, "the section and the two it touches");
    assert!(Arc::ptr_eq(
        first.shared_section(section + 2),
        third.shared_section(section + 2)
    ));

    let mut moved = around.clone();
    let mut west = around[0].clone().unwrap();
    west.set(15, 50, 5, Block::Air.default_state());
    moved[0] = Some(west);
    let (_, stats) = obfuscate(&edited, POS, &moved, &plan, &mut cache);
    assert_eq!(stats.computed, 1, "only the section beside the change");

    let other = overworld_plan(Mode::Fill);
    let (_, stats) = obfuscate(&edited, POS, &moved, &other, &mut cache);
    assert_eq!(stats.computed, counted, "a new plan starts over");
}

#[tuff_core::watchdog]
#[test]
fn sections_without_hidden_blocks_stay_shared() {
    let mut chunk = solid_chunk();
    chunk.set(5, 10, 5, Block::DiamondOre.default_state());
    let (out, stats) = obfuscate(
        &chunk,
        POS,
        &surrounded(),
        &overworld_plan(Mode::Hide),
        &mut Cache::default(),
    );
    assert_eq!(stats.replaced, 1);
    for index in 0..SHAPE.sections() {
        assert_eq!(
            Arc::ptr_eq(chunk.shared_section(index), out.shared_section(index)),
            index != section_of(10)
        );
    }
    let plain = solid_chunk();
    let (out, _) = obfuscate(
        &plain,
        POS,
        &surrounded(),
        &overworld_plan(Mode::Hide),
        &mut Cache::default(),
    );
    assert_eq!(out.stamp(), plain.stamp(), "nothing hidden: the same chunk");
}

#[tuff_core::watchdog]
#[test]
fn settings_parse_and_validate() {
    assert_eq!(
        parse_blocks("minecraft:stone, deepslate  diamond_ore").unwrap(),
        vec![Block::Stone, Block::Deepslate, Block::DiamondOre]
    );
    assert!(parse_blocks("stone,not_a_block").is_err());
    let store = ConfigStore::new();
    assert_eq!(store.get_bool(ENABLED), Some(false));
    assert!(store.set_text(&hidden_key("overworld"), "nope").is_err());
    assert!(store.set_text(MODE, "random").is_err());
    assert!(store.set_text(UPDATE_RADIUS, "0").is_err());
    store.set_text(MODE, "fill").unwrap();
    store.set_text(UPDATE_RADIUS, "4").unwrap();
    for (dimension, hidden, replacement) in DIMENSIONS {
        parse_blocks(hidden).unwrap();
        assert!(
            !parse_blocks(replacement).unwrap().is_empty(),
            "{dimension}"
        );
    }
    assert!(Plan::new(Mode::Hide, 64, 2, &[], &[Block::EndStone], 0).is_none());
    assert!(Plan::new(Mode::Hide, 64, 2, &[Block::Air], &[], 0).is_none());
}

#[tuff_core::watchdog]
#[test]
fn offsets_cover_the_radius_along_the_axes() {
    assert_eq!(offsets(1).count(), 7);
    assert_eq!(offsets(2).count(), 25);
    assert!(offsets(2).all(|(x, y, z)| x.abs() + y.abs() + z.abs() <= 2));
}

#[tuff_core::watchdog]
#[test]
fn ores_and_stone_occlude_and_glass_and_water_do_not() {
    assert!(occludes(stone()));
    assert!(occludes(Block::DiamondOre.default_state()));
    assert!(occludes(Block::Netherrack.default_state()));
    assert!(!occludes(Block::Air.default_state()));
    assert!(!occludes(Block::Glass.default_state()));
    assert!(!occludes(Block::Water.default_state()));
    assert!(!occludes(Block::Chest.default_state()));
}

fn reference(chunk: &Chunk, around: &[Option<Chunk>; 4], plan: &Plan) -> Chunk {
    let mut out = chunk.clone();
    let side = |x: i32, y: i32, z: i32| {
        if y < SHAPE.min_y {
            return Side::Floor;
        }
        if y >= SHAPE.max_y() {
            return Side::Open;
        }
        let neighbour = match (x, z) {
            (-1, _) => around[0].as_ref(),
            (16, _) => around[1].as_ref(),
            (_, -1) => around[2].as_ref(),
            (_, 16) => around[3].as_ref(),
            _ => Some(chunk),
        };
        neighbour.map_or(Side::Open, |c| Side::Block(c.get(x & 15, y, z & 15)))
    };
    every_block(|x, y, z| {
        let state = chunk.get(x, y, z);
        if y > plan.max_y || !plan.may_alter(state) {
            return;
        }
        let sides = [
            side(x, y - 1, z),
            side(x, y + 1, z),
            side(x - 1, y, z),
            side(x + 1, y, z),
            side(x, y, z - 1),
            side(x, y, z + 1),
        ];
        let enclosed = sides.iter().all(|side| match side {
            Side::Block(state) => occludes(*state),
            Side::Floor | Side::Closed => true,
            Side::Open => false,
        });
        if enclosed {
            let at = (POS.x * 16 + x, y, POS.z * 16 + z);
            out.set(x, y, z, plan.stand_in(|| sides, at.0, at.1, at.2));
        }
    });
    out
}

fn random_chunk(rng: &mut rand::rngs::StdRng) -> Chunk {
    use rand::Rng;
    let mix = [
        Block::DiamondOre.default_state(),
        Block::CoalOre.default_state(),
        Block::DeepslateIronOre.default_state(),
        Block::Deepslate.default_state(),
        Block::OakPlanks.default_state(),
        Block::Chest.default_state(),
        Block::Glass.default_state(),
        Block::Water.default_state(),
        Block::Air.default_state(),
    ];
    let mut chunk = solid_chunk();
    for section in 0..SHAPE.sections() {
        let base = SHAPE.section_base(section);
        let kind = rng.gen_range(0..6);
        for i in 0..SECTION_VOLUME as i32 {
            let (x, y, z) = (i & 15, base + (i >> 8), (i >> 4) & 15);
            let state = match kind {
                0 => continue,
                1 => BlockState::from_id(1 + (i as u32 * 7) % 400).unwrap(),
                2 if rng.gen_bool(0.02) => mix[rng.gen_range(0..mix.len())],
                2 => continue,
                _ if rng.gen_bool(0.25) => mix[rng.gen_range(0..mix.len())],
                _ => continue,
            };
            chunk.set(x, y, z, state);
        }
    }
    chunk
}

#[tuff_core::watchdog]
#[test]
fn obfuscation_matches_a_block_by_block_reference() {
    use rand::{Rng, SeedableRng};
    let mut rng = rand::rngs::StdRng::seed_from_u64(11);
    let mut replaced = 0;
    for round in 0..12 {
        let mode = if round % 2 == 0 {
            Mode::Hide
        } else {
            Mode::Fill
        };
        let (_, hidden, replacement) = DIMENSIONS[0];
        let plan = Arc::new(
            Plan::new(
                mode,
                [64, 20, -60, 319][round % 4],
                2,
                &parse_blocks(hidden).unwrap(),
                &parse_blocks(replacement).unwrap(),
                round as u64,
            )
            .unwrap(),
        );
        let chunk = random_chunk(&mut rng);
        let around: [Option<Chunk>; 4] = std::array::from_fn(|_| match rng.gen_range(0..3) {
            0 => None,
            1 => Some(solid_chunk()),
            _ => Some(random_chunk(&mut rng)),
        });
        let expected = reference(&chunk, &around, &plan);
        let actual = sent(&chunk, &around, &plan);
        every_block(|x, y, z| {
            replaced += usize::from(expected.get(x, y, z) != chunk.get(x, y, z));
            assert_eq!(
                actual.get(x, y, z),
                expected.get(x, y, z),
                "round {round} {mode:?} at {x} {y} {z}"
            );
        });
        for index in 0..SHAPE.sections() {
            let section = actual.shared_section(index);
            if Arc::ptr_eq(section, chunk.shared_section(index)) {
                continue;
            }
            let present: HashSet<BlockState> =
                (0..SECTION_VOLUME).map(|i| section.get(i)).collect();
            assert!(
                section
                    .palette()
                    .iter()
                    .all(|state| present.contains(state)),
                "round {round}: a palette names a block the section does not hold"
            );
        }
    }
    assert!(replaced > 10_000, "only {replaced} blocks replaced");
}

fn edge_ore_sent(side: usize, dense: bool, present: bool) -> BlockState {
    let diamond = Block::DiamondOre.default_state();
    let mut chunk = solid_chunk();
    let (x, z) = [(0, 5), (15, 5), (5, 0), (5, 15)][side];
    chunk.set(x, 10, z, diamond);
    if dense {
        for i in 0..400 {
            chunk.set(1 + i % 14, 20 + i / 196, 1 + (i / 14) % 14, diamond);
        }
    }
    let plan = overworld_plan(Mode::Hide);
    let mut around: [Option<Chunk>; 4] = std::array::from_fn(|_| Some(solid_chunk()));
    let mut closed = [false; 4];
    if present {
        let mut open = solid_chunk();
        let (ox, oz) = [(15, 5), (0, 5), (5, 15), (5, 0)][side];
        open.set(ox, 10, oz, Block::Air.default_state());
        around[side] = Some(open);
    } else {
        around[side] = None;
        closed[side] = true;
    }
    let (out, _) = obfuscate_closed(&chunk, POS, &around, closed, &plan, &mut Cache::default());
    out.get(x, 10, z)
}

#[tuff_core::watchdog]
#[test]
fn a_closed_edge_hides_ore_on_every_side_in_both_paths() {
    for side in 0..4 {
        for dense in [false, true] {
            assert_eq!(edge_ore_sent(side, dense, false), stone(), "{side} {dense}");
            assert_eq!(
                edge_ore_sent(side, dense, true),
                Block::DiamondOre.default_state(),
                "{side} {dense}"
            );
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn the_cache_recomputes_when_a_closed_side_gets_its_neighbour() {
    let diamond = Block::DiamondOre.default_state();
    let mut chunk = solid_chunk();
    chunk.set(0, 10, 5, diamond);
    let plan = overworld_plan(Mode::Hide);
    let mut around: [Option<Chunk>; 4] = std::array::from_fn(|_| Some(solid_chunk()));
    around[0] = None;
    let mut cache = Cache::default();
    let (first, _) = obfuscate_closed(
        &chunk,
        POS,
        &around,
        [true, false, false, false],
        &plan,
        &mut cache,
    );
    assert_eq!(first.get(0, 10, 5), stone());
    let mut open = solid_chunk();
    open.set(15, 10, 5, Block::Air.default_state());
    around[0] = Some(open);
    let (second, stats) = obfuscate_closed(&chunk, POS, &around, [false; 4], &plan, &mut cache);
    assert_eq!(second.get(0, 10, 5), diamond);
    assert!(stats.computed > 0);
}
