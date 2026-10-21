use super::*;
use tuff_wire::PacketReader;

fn flat_in(shape: ChunkShape) -> Chunk {
    let mut chunk = Chunk::empty(shape);
    let layers = [
        (Block::Bedrock, 1),
        (Block::Stone, 128),
        (Block::Dirt, 6),
        (Block::GrassBlock, 1),
    ];
    let mut y = shape.min_y;
    for (block, thickness) in layers {
        for _ in 0..thickness {
            for z in 0..16 {
                for x in 0..16 {
                    chunk.set(x, y, z, block.default_state());
                }
            }
            y += 1;
        }
    }
    chunk
}

#[tuff_core::watchdog]
#[test]
fn ticking_sections_follow_block_changes() {
    let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
    assert_eq!(chunk.next_ticking_section(0), None);
    let grass = Block::GrassBlock.default_state();
    chunk.set(3, 64, 5, grass);
    chunk.set(0, 319, 0, grass);
    let low = ((64 - ChunkShape::OVERWORLD.min_y) / 16) as usize;
    let top = ChunkShape::OVERWORLD.sections() - 1;
    assert_eq!(chunk.next_ticking_section(0), Some(low));
    assert_eq!(chunk.next_ticking_section(low + 1), Some(top));
    assert_eq!(chunk.next_ticking_section(top + 1), None);
    chunk.set(3, 64, 5, Block::Stone.default_state());
    assert_eq!(chunk.next_ticking_section(0), Some(top));
}

fn flat() -> Chunk {
    flat_in(ChunkShape::OVERWORLD)
}

#[tuff_core::watchdog]
#[test]
fn packing_never_splits_entries_across_longs() {
    let words = pack(15, (0..5).map(|_| 0x7FFF));
    assert_eq!(words.len(), 2);
    assert_eq!(words[0], (1u64 << 60) - 1);
    assert_eq!(words[1], 0x7FFF);
    let signed: Vec<i64> = words.iter().map(|w| *w as i64).collect();
    assert_eq!(unpack(15, 5, &signed).unwrap(), vec![0x7FFF; 5]);
    assert!(unpack(15, 5, &signed[..1]).is_none());
}

#[tuff_core::watchdog]
#[test]
fn snapshots_are_copy_on_write() {
    let mut chunk = flat();
    let snapshot = chunk.clone();
    chunk.set(0, 100, 0, Block::Stone.default_state());
    assert!(is_air(snapshot.get(0, 100, 0)));
    assert_eq!(chunk.get(0, 100, 0).block(), Block::Stone);
    assert!(Arc::ptr_eq(&chunk.sections[0], &snapshot.sections[0]));
}

#[tuff_core::watchdog]
#[test]
fn encoded_chunk_has_expected_structure() {
    for shape in [ChunkShape::OVERWORLD, ChunkShape::new(0, 256)] {
        encoded_chunk_has_expected_structure_in(shape);
    }
}

fn encoded_chunk_has_expected_structure_in(shape: ChunkShape) {
    let chunk = flat_in(shape);
    let body = chunk.encode(3, -2, &[]);
    let bits = shape.heightmap_bits();
    assert_eq!(bits, 9);
    let mut r = PacketReader::new(body.clone());
    assert_eq!(r.read_int().unwrap(), 3);
    assert_eq!(r.read_int().unwrap(), -2);
    assert_eq!(r.read_var_int().unwrap(), 3);
    for _ in 0..3 {
        r.read_var_int().unwrap();
        let longs = r.read_var_int().unwrap();
        assert_eq!(longs, 256usize.div_ceil((64 / bits) as usize) as i32);
        let first = r.read_long().unwrap() as u64;
        assert_eq!(first & ((1 << bits) - 1), 136);
        for _ in 1..longs {
            r.read_long().unwrap();
        }
    }
    let data_len = r.read_var_int().unwrap() as usize;
    let mut sections = PacketReader::new(r.read_bytes(data_len).unwrap());
    for index in 0..shape.sections() {
        let count = sections.read_short().unwrap();
        sections.read_short().unwrap();
        let bits = sections.read_unsigned_byte().unwrap() as u32;
        if let Some(per_long) = 64_u32.checked_div(bits) {
            let palette = sections.read_var_int().unwrap();
            for _ in 0..palette {
                sections.read_var_int().unwrap();
            }
            for _ in 0..4096usize.div_ceil(per_long as usize) {
                sections.read_long().unwrap();
            }
        } else {
            sections.read_var_int().unwrap();
        }
        assert_eq!(sections.read_unsigned_byte().unwrap(), 0);
        assert_eq!(sections.read_var_int().unwrap(), plains() as i32);
        let expected = match index {
            0..8 => 4096,
            8 => 16 * 16 * 8,
            _ => 0,
        };
        assert_eq!(count, expected, "section {index}");
    }
    assert_eq!(sections.remaining(), 0);
    assert_eq!(r.read_var_int().unwrap(), 0);
    for _ in 0..4 {
        let mask = r.read_var_int().unwrap() as usize;
        r.read_bytes(mask).unwrap();
    }
    assert_eq!(r.read_var_int().unwrap(), 0);
    assert_eq!(r.read_var_int().unwrap(), 0);
    assert_eq!(r.remaining(), 0);
}

#[tuff_core::watchdog]
#[test]
fn block_entities_are_listed_with_absent_nbt_when_empty() {
    let body = flat().encode(
        0,
        0,
        &[
            (BlockPos::new(-15, -64, 17), 1, Compound::new()),
            (BlockPos::new(2, 70, 3), 32, Compound::new().with("a", 1)),
        ],
    );
    let mut r = PacketReader::new(body);
    r.read_int().unwrap();
    r.read_int().unwrap();
    for _ in 0..r.read_var_int().unwrap() {
        r.read_var_int().unwrap();
        for _ in 0..r.read_var_int().unwrap() {
            r.read_long().unwrap();
        }
    }
    let sections = r.read_var_int().unwrap() as usize;
    r.read_bytes(sections).unwrap();
    assert_eq!(r.read_var_int().unwrap(), 2);
    assert_eq!(r.read_unsigned_byte().unwrap(), 1 << 4 | 1);
    assert_eq!(r.read_short().unwrap(), -64);
    assert_eq!(r.read_var_int().unwrap(), 1);
    assert_eq!(r.read_nbt().unwrap(), None);
    assert_eq!(r.read_unsigned_byte().unwrap(), 2 << 4 | 3);
    assert_eq!(r.read_short().unwrap(), 70);
    assert_eq!(r.read_var_int().unwrap(), 32);
    assert_eq!(
        r.read_nbt().unwrap(),
        Some(Tag::Compound(Compound::new().with("a", 1)))
    );
}

#[tuff_core::watchdog]
#[test]
fn blocks_outside_the_shape_are_air_and_cannot_be_set() {
    let shape = ChunkShape::new(0, 256);
    let mut chunk = Chunk::empty(shape);
    assert_eq!(chunk.sections().len(), 16);
    assert!(chunk.set(0, -1, 0, Block::Stone.default_state()).is_none());
    assert!(chunk.set(0, 256, 0, Block::Stone.default_state()).is_none());
    assert!(chunk.set(0, 255, 0, Block::Stone.default_state()).is_some());
    assert_eq!(chunk.get(0, 255, 0).block(), Block::Stone);
    assert!(is_air(chunk.get(0, -1, 0)));
    let older = Chunk::empty(shape);
    assert_eq!(
        older.changed_blocks(&chunk),
        vec![(0, 255, 0, Block::Stone.default_state())]
    );
}

#[tuff_core::watchdog]
#[test]
fn palette_regrowth_keeps_every_block_while_states_churn() {
    let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
    let mut reference = vec![chunk.get(0, 0, 0); SECTION_VOLUME];
    let air = reference[0];
    for i in 0..2000usize {
        let state = BlockState::from_id((i * 7 % 400) as u32).unwrap_or(air);
        let at = (i * 131) % SECTION_VOLUME;
        let (x, y, z) = ((at & 15) as i32, (at >> 8) as i32, ((at >> 4) & 15) as i32);
        chunk.set(x, y, z, state);
        reference[at] = state;
    }
    for (at, state) in reference.iter().enumerate() {
        let (x, y, z) = ((at & 15) as i32, (at >> 8) as i32, ((at >> 4) & 15) as i32);
        assert_eq!(chunk.get(x, y, z), *state, "block {at}");
    }
}

#[tuff_core::watchdog]
#[test]
fn sections_with_many_states_use_direct_palette() {
    let mut chunk = Chunk::empty(ChunkShape::OVERWORLD);
    for (i, state) in (0..300).filter_map(BlockState::from_id).enumerate() {
        chunk.set(i as i32 & 15, (i as i32 >> 4) & 15, 0, state);
    }
    let mut w = PacketWriter::new();
    chunk.sections[4].encode(&mut w);
    let mut r = PacketReader::new(w.into_bytes());
    r.read_short().unwrap();
    r.read_short().unwrap();
    assert_eq!(
        r.read_unsigned_byte().unwrap() as u32,
        ceil_log2(BlockState::COUNT)
    );
}

/// Blocks and counts of an encoded section, read back as a client would.
fn decode_section(bytes: Bytes) -> (i16, i16, Vec<u32>) {
    let mut r = PacketReader::new(bytes);
    let non_air = r.read_short().unwrap();
    let fluids = r.read_short().unwrap();
    let bits = r.read_unsigned_byte().unwrap() as u32;
    let ids = if bits == 0 {
        vec![r.read_var_int().unwrap() as u32; SECTION_VOLUME]
    } else {
        let palette: Option<Vec<u32>> = (bits <= 8).then(|| {
            (0..r.read_var_int().unwrap())
                .map(|_| r.read_var_int().unwrap() as u32)
                .collect()
        });
        let words: Vec<i64> = (0..SECTION_VOLUME.div_ceil((64 / bits) as usize))
            .map(|_| r.read_long().unwrap())
            .collect();
        let values = unpack(bits, SECTION_VOLUME, &words).unwrap();
        match palette {
            Some(palette) => values.iter().map(|v| palette[*v as usize]).collect(),
            None => values.iter().map(|v| *v as u32).collect(),
        }
    };
    (non_air, fluids, ids)
}

fn check_section(section: &Section, expected: &[BlockState; SECTION_VOLUME]) {
    for (i, state) in expected.iter().enumerate() {
        assert_eq!(section.get(i), *state, "block {i}");
    }
    let rebuilt = Section::new(Box::new(*expected), Box::new([plains(); BIOME_VOLUME]));
    assert_eq!(*section, rebuilt);
    assert_eq!(section.non_air, rebuilt.non_air);
    assert_eq!(section.ticking, rebuilt.ticking);
    assert_eq!(section.fluids, rebuilt.fluids);
    assert!(expected.iter().all(|s| section.palette().contains(s)));
    let (palette, indices) = section.block_palette();
    if palette.len() > 1 {
        for (i, state) in expected.iter().enumerate() {
            assert_eq!(palette[indices[i] as usize], *state);
        }
    }
    let mut w = PacketWriter::new();
    section.encode(&mut w);
    let (non_air, fluids, ids) = decode_section(w.into_bytes());
    assert_eq!(
        non_air as usize,
        expected.iter().filter(|s| !is_air(**s)).count()
    );
    assert_eq!(
        fluids as usize,
        expected
            .iter()
            .filter(|s| !s.fluid_state().is_empty())
            .count()
    );
    let sent: Vec<u32> = expected.iter().map(|s| s.id()).collect();
    assert_eq!(ids, sent);
}

#[tuff_core::watchdog]
#[test]
fn paletted_sections_match_a_flat_array_through_every_width() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let states: Vec<BlockState> = (0..400u32)
        .map(|i| BlockState::from_id(i * 37 % BlockState::COUNT as u32).unwrap())
        .chain([
            Block::Air.default_state(),
            Block::Water.default_state(),
            Block::GrassBlock.default_state(),
        ])
        .collect();
    let air = Block::Air.default_state();
    let mut section = Section::uniform(air, plains());
    let mut expected = Box::new([air; SECTION_VOLUME]);
    for kinds in [2, 3, 4, 5, 16, 17, 200, 256, 300, states.len()] {
        for _ in 0..3000 {
            let state = states[states.len() - 1 - next(kinds)];
            let i = next(SECTION_VOLUME);
            assert_eq!(section.set(i, state), expected[i]);
            expected[i] = state;
        }
        check_section(&section, &expected);
    }
    for i in 0..SECTION_VOLUME {
        section.set(i, Block::Stone.default_state());
        expected[i] = Block::Stone.default_state();
    }
    section.set(7, Block::Water.default_state());
    expected[7] = Block::Water.default_state();
    check_section(&section, &expected);
}

#[tuff_core::watchdog]
#[test]
fn masks_and_batched_changes_match_a_flat_array_in_every_storage() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let states: Vec<BlockState> = (0..300u32)
        .map(|i| BlockState::from_id(i * 53 % BlockState::COUNT as u32).unwrap())
        .chain([Block::Water.default_state(), Block::Air.default_state()])
        .collect();
    let stone = Block::Stone.default_state();
    for kinds in [1, 2, 3, 5, 16, 17, 200, states.len()] {
        let mut expected = Box::new([stone; SECTION_VOLUME]);
        let mut section = Section::uniform(stone, plains());
        if kinds > 1 {
            for (i, slot) in expected.iter_mut().enumerate() {
                *slot = states[next(kinds)];
                section.set(i, *slot);
            }
        }
        let pred = |state: BlockState| state.id().is_multiple_of(3);
        let mask = section.select(pred);
        for (i, state) in expected.iter().enumerate() {
            assert_eq!(mask[i >> 6] >> (i & 63) & 1 == 1, pred(*state), "block {i}");
        }
        let mut changes: Vec<(u16, BlockState)> = (0..next(4000))
            .map(|_| (next(SECTION_VOLUME) as u16, states[next(states.len())]))
            .collect();
        changes.extend(
            (0..SECTION_VOLUME as u16)
                .filter(|i| i % 5 == 0)
                .map(|i| (i, stone)),
        );
        for (i, state) in &changes {
            expected[*i as usize] = *state;
        }
        let changed = section.with_changes(&changes);
        check_section(&changed, &expected);
        assert!(
            changed.palette().iter().all(|s| expected.contains(s)),
            "{kinds} kinds: the palette lists only states in use"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn palettes_drop_unused_entries_before_widening() {
    let stone = Block::Stone.default_state();
    let mut section = Section::uniform(stone, plains());
    let narrow = section.heap_bytes();
    for (i, block) in [Block::Dirt, Block::Sand, Block::Gravel, Block::Clay]
        .into_iter()
        .enumerate()
    {
        section.set(0, block.default_state());
        assert!(section.palette().len() <= 4, "after {i}");
    }
    assert_eq!(narrow, 0);
    assert!(
        section.heap_bytes() <= 4096 * 2 / 8 + 8,
        "{}",
        section.heap_bytes()
    );
    let mut expected = Box::new([stone; SECTION_VOLUME]);
    expected[0] = Block::Clay.default_state();
    check_section(&section, &expected);
}

/// A palette still crowded after dropping unused entries widens a
/// step early, so a section gaining states one by one is not
/// compacted again on every new state.
#[tuff_core::watchdog]
#[test]
fn crowded_palettes_widen_early() {
    let states: Vec<BlockState> = (0..17).filter_map(BlockState::from_id).collect();
    let mut section = Section::uniform(states[0], plains());
    let mut expected = Box::new([states[0]; SECTION_VOLUME]);
    for i in 1..16 {
        section.set(i, states[i]);
        expected[i] = states[i];
    }
    for i in 1..3 {
        section.set(i, states[0]);
        expected[i] = states[0];
    }
    section.set(4, states[16]);
    expected[4] = states[16];
    assert!(
        section.heap_bytes() > SECTION_VOLUME / 2 + 64,
        "{}",
        section.heap_bytes()
    );
    check_section(&section, &expected);
}

#[tuff_core::watchdog]
#[test]
fn natural_sections_store_four_bit_indices() {
    let mut blocks = Box::new([Block::Stone.default_state(); SECTION_VOLUME]);
    for (i, block) in [
        Block::Air,
        Block::Dirt,
        Block::Granite,
        Block::Diorite,
        Block::Andesite,
        Block::CoalOre,
        Block::IronOre,
        Block::Water,
    ]
    .into_iter()
    .enumerate()
    {
        blocks[i * 300] = block.default_state();
    }
    let section = Section::new(blocks.clone(), Box::new([plains(); BIOME_VOLUME]));
    assert!(section.heap_bytes() <= SECTION_VOLUME / 2 + 64);
    assert!(section.heap_bytes() * 3 < size_of::<[BlockState; SECTION_VOLUME]>());
    check_section(&section, &blocks);
}

#[tuff_core::watchdog]
#[test]
fn mixed_biomes_use_indirect_palette() {
    let mut biomes = Box::new([plains(); BIOME_VOLUME]);
    biomes[5] = biome_id("minecraft:desert").unwrap();
    let section = Section::new(
        Box::new([Block::Air.default_state(); SECTION_VOLUME]),
        biomes,
    );
    let mut w = PacketWriter::new();
    section.encode(&mut w);
    let mut r = PacketReader::new(w.into_bytes());
    r.read_short().unwrap();
    r.read_short().unwrap();
    assert_eq!(r.read_unsigned_byte().unwrap(), 0);
    r.read_var_int().unwrap();
    assert_eq!(r.read_unsigned_byte().unwrap(), 1);
    assert_eq!(r.read_var_int().unwrap(), 2);
}

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

fn naive_matches(section: &Section, pred: impl Fn(BlockState) -> bool) -> Vec<(usize, BlockState)> {
    (0..SECTION_VOLUME)
        .map(|i| (i, section.get(i)))
        .filter(|(_, state)| pred(*state))
        .collect()
}

#[tuff_core::watchdog]
#[test]
fn matching_scan_equals_the_per_block_scan_for_every_storage_width() {
    let emitting: Vec<BlockState> = (0..BlockState::COUNT)
        .filter_map(|id| BlockState::from_id(id as u32))
        .filter(|state| state.light_emission() != 0)
        .collect();
    let dark: Vec<BlockState> = (0..BlockState::COUNT)
        .filter_map(|id| BlockState::from_id(id as u32))
        .filter(|state| state.light_emission() == 0)
        .collect();
    assert!(emitting.len() > 20 && dark.len() > 300);
    let pred = |state: BlockState| state.light_emission() != 0;
    let mut rng = Xorshift(0x9E37_79B9_7F4A_7C15);
    for round in 0..600 {
        let distinct_dark = [0, 1, 2, 5, 14, 15, 30, 200, 300][round % 9];
        let distinct_emit = [0, 1, 2, 3, 4, 5, 9, 40][(round / 9) % 8];
        let mut pool: Vec<BlockState> = (0..distinct_dark)
            .map(|_| dark[rng.below(dark.len())])
            .collect();
        pool.extend((0..distinct_emit).map(|_| emitting[rng.below(emitting.len())]));
        if pool.is_empty() {
            pool.push(dark[0]);
        }
        let mut section = Section::uniform(pool[0], 0);
        let density = 1 + rng.below(64);
        for index in 0..SECTION_VOLUME {
            let state = if rng.below(64) < density {
                pool[rng.below(pool.len())]
            } else {
                pool[0]
            };
            section.set(index, state);
        }
        for _ in 0..rng.below(200) {
            section.set(rng.below(SECTION_VOLUME), pool[rng.below(pool.len())]);
        }
        let mut found = Vec::new();
        section.for_each_matching(pred, |i, state| found.push((i, state)));
        assert_eq!(found, naive_matches(&section, pred), "round {round}");
    }
}

#[tuff_core::watchdog]
#[test]
fn matching_scan_handles_uniform_and_edge_cells() {
    let torch = Block::Torch.default_state();
    let stone = Block::Stone.default_state();
    let pred = |state: BlockState| state.light_emission() != 0;
    for (state, expect) in [(torch, SECTION_VOLUME), (stone, 0)] {
        let section = Section::uniform(state, 0);
        let mut count = 0;
        section.for_each_matching(pred, |i, _| {
            assert_eq!(i, count);
            count += 1;
        });
        assert_eq!(count, expect);
    }
    let mut section = Section::uniform(stone, 0);
    section.set(0, torch);
    section.set(SECTION_VOLUME - 1, torch);
    let mut found = Vec::new();
    section.for_each_matching(pred, |i, _| found.push(i));
    assert_eq!(found, vec![0, SECTION_VOLUME - 1]);
}

fn fold_words(log: u32, index: impl Fn(usize) -> usize) -> Vec<u64> {
    let bits = 1usize << log;
    let per_word = 64 / bits;
    (0..SECTION_VOLUME / per_word)
        .map(|w| {
            (0..per_word).fold(0u64, |word, k| {
                word | (index(w * per_word + k) as u64) << (k * bits)
            })
        })
        .collect()
}

fn next_random(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

#[tuff_core::watchdog]
#[test]
fn width_specialized_packing_matches_the_generic_fold() {
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    for log in 0..=MAX_PALETTE_LOG {
        let limit = 1u64 << (1 << log);
        for round in 0..64 {
            let mut slots = Box::new([0u8; SECTION_VOLUME]);
            for slot in slots.iter_mut() {
                *slot = match round {
                    0 => 0,
                    1 => (limit - 1) as u8,
                    _ => (next_random(&mut rng) % limit) as u8,
                };
            }
            let words = pack_slots(log, &slots);
            assert_eq!(&*words, &fold_words(log, |i| slots[i] as usize)[..]);
            assert_eq!(*unpack_slots(log, &words), *slots);
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn a_pair_section_matches_the_generic_construction() {
    let (a, b) = (Block::Stone.default_state(), Block::Dirt.default_state());
    for index in [0, 1, 31, 32, 33, 2047, 4094, 4095] {
        let pair = Paletted::pair(a, b, index);
        let reference_words = fold_words(1, |i| usize::from(i == index));
        let mut table = [a; 4];
        table[1] = b;
        assert_eq!(&*pair.words, &reference_words[..]);
        assert_eq!(&*pair.palette, &table[..]);
        assert_eq!((pair.log, pair.mask, pair.len, pair.last), (1, 3, 2, 0));
    }
}

#[tuff_core::watchdog]
#[test]
fn heightmaps_use_their_own_predicates() {
    let mut chunk = flat();
    chunk.set(0, 128, 0, Block::OakLeaves.default_state());
    chunk.set(1, 128, 0, Block::Torch.default_state());
    let body = chunk.encode(0, 0, &[]);
    let bits = chunk.shape.heightmap_bits() as u64;
    let mut r = PacketReader::new(body);
    r.read_int().unwrap();
    r.read_int().unwrap();
    assert_eq!(r.read_var_int().unwrap(), 3);
    let mut firsts = Vec::new();
    for expected in [1, 4, 5] {
        assert_eq!(r.read_var_int().unwrap(), expected);
        let longs = r.read_var_int().unwrap();
        let words: Vec<u64> = (0..longs).map(|_| r.read_long().unwrap() as u64).collect();
        let per = 64 / bits;
        let value =
            |col: u64| (words[(col / per) as usize] >> ((col % per) * bits)) & ((1 << bits) - 1);
        firsts.push((value(0), value(1)));
    }
    let [surface, motion, no_leaves] = [firsts[0], firsts[1], firsts[2]];
    let ground = no_leaves.0;
    assert_eq!(ground, 136);
    assert_eq!(surface, (193, 193));
    assert_eq!(motion, (193, ground));
    assert_eq!(no_leaves, (ground, ground));
}
