//! Chunk columns and their network encoding
//! (https://minecraft.wiki/w/Java_Edition_protocol/Chunk_format).
//!
//! Sections sit behind `Arc`s, so cloning a [`Chunk`] is cheap and yields an
//! immutable snapshot: saving, packet encoding, and neighbouring regions all
//! work from snapshots on other threads while the owner keeps mutating its
//! copy (only the touched section is copied on write).

use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use tuff_vanilla_data::data_registry::WORLDGEN_BIOME;
use tuff_vanilla_data::{Block, BlockState};

use crate::block_pos::BlockPos;
use crate::light::{ChunkLight, SectionLight};
use tuff_nbt::{Compound, Tag, TagType};
use tuff_wire::PacketWriter;

pub const SECTION_VOLUME: usize = 4096;
pub const BIOME_VOLUME: usize = 64;
pub const LIGHT_BYTES: usize = 2048;
pub const MASK_WORDS: usize = SECTION_VOLUME / 64;
/// One bit per block of a section, bit `i & 63` of word `i >> 6`.
pub type BlockMask = [u64; MASK_WORDS];
const WORLD_SURFACE: i32 = 1;
const MOTION_BLOCKING: i32 = 4;
const MOTION_BLOCKING_NO_LEAVES: i32 = 5;
const MIN_INDIRECT_BLOCK_BITS: u32 = 4;
const MAX_INDIRECT_BIOME_BITS: u32 = 3;

/// The vertical extent of a dimension's chunks: `height` is a multiple of
/// 16 and `min_y` a multiple of 16, as the game requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChunkShape {
    pub min_y: i32,
    pub height: i32,
    /// Whether this is the `minecraft:overworld` dimension itself, which
    /// the data upgrades treat differently from every other dimension.
    pub overworld: bool,
}

impl ChunkShape {
    pub const OVERWORLD: Self = Self {
        min_y: -64,
        height: 384,
        overworld: true,
    };

    pub const fn new(min_y: i32, height: i32) -> Self {
        Self {
            min_y,
            height,
            overworld: false,
        }
    }

    pub const fn with_overworld(self, overworld: bool) -> Self {
        Self { overworld, ..self }
    }

    pub const fn sections(self) -> usize {
        (self.height / 16) as usize
    }

    pub const fn min_section(self) -> i32 {
        self.min_y >> 4
    }

    /// Sections with light data: one below and one above the world.
    pub const fn light_sections(self) -> usize {
        self.sections() + 2
    }

    /// One past the highest block y.
    pub const fn max_y(self) -> i32 {
        self.min_y + self.height
    }

    pub const fn contains_y(self, y: i32) -> bool {
        y >= self.min_y && y < self.max_y()
    }

    /// Bits per heightmap entry: enough for `0..=height`.
    pub fn heightmap_bits(self) -> u32 {
        ceil_log2(self.height as usize + 1)
    }

    /// Y of the bottom of the section at `index`, counted from the bottom
    /// of the world.
    pub const fn section_base(self, index: usize) -> i32 {
        self.min_y + index as i32 * 16
    }
}

/// Whether a block stops movement (the motion-blocking heightmap's rule).
pub fn blocks_motion(state: BlockState) -> bool {
    !is_air(state)
        && (!state.collision_boxes().is_empty()
            || state.is_liquid()
            || state.get("waterlogged") == Some("true"))
}

#[inline]
pub fn is_air(state: BlockState) -> bool {
    state.flags() & BlockState::FLAG_NOT_AIR == 0
}

fn is_fluid(state: BlockState) -> bool {
    !state.fluid_state().is_empty()
}

/// Bits needed to index `count` distinct values (`ceil(log2(count))`).
pub fn ceil_log2(count: usize) -> u32 {
    usize::BITS - count.saturating_sub(1).leading_zeros()
}

pub fn biome_id(name: &str) -> Option<u16> {
    WORLDGEN_BIOME
        .entries
        .binary_search(&name)
        .ok()
        .map(|id| id as u16)
}

pub fn biome_name(id: u16) -> &'static str {
    WORLDGEN_BIOME
        .entries
        .get(id as usize)
        .copied()
        .unwrap_or("minecraft:plains")
}

pub fn plains() -> u16 {
    biome_id("minecraft:plains").expect("plains biome")
}

/// Indices wider than this many bits (`1 << MAX_PALETTE_LOG`) store the
/// states themselves instead.
const MAX_PALETTE_LOG: u32 = 3;

/// Matching palette slots up to this many are found with word-wide field
/// compares; more are found with a per-entry table read.
const SWAR_SLOTS: usize = 4;

#[derive(Clone, Debug)]
enum Blocks {
    Uniform(BlockState),
    Paletted(Paletted),
    /// More distinct states than the widest palette holds.
    Direct(Box<[BlockState; SECTION_VOLUME]>),
}

/// Palette indices of `1 << log` bits (1, 2, 4 or 8) packed into longs
/// without spanning two, low bits first: the protocol's layout, so 4 and 8
/// bit sections are sent as they are stored. Entries past `len` and entries
/// no block uses any more are free; a full palette drops the unused ones
/// before it widens.
#[derive(Clone, Debug)]
struct Paletted {
    log: u32,
    mask: u64,
    len: u16,
    /// The slot last written, tried first.
    last: u16,
    palette: Box<[BlockState]>,
    words: Box<[u64]>,
}

fn palette_log(len: usize) -> Option<u32> {
    (0..=MAX_PALETTE_LOG).find(|log| len <= 1 << (1 << log))
}

/// Squeezes eight one-byte entries of `BITS` bits each into the low
/// `8 * BITS` bits, first entry lowest.
#[inline(always)]
fn gather8<const BITS: u32>(mut x: u64) -> u64 {
    match BITS {
        4 => {
            x = (x | x >> 4) & 0x00FF_00FF_00FF_00FF;
            x = (x | x >> 8) & 0x0000_FFFF_0000_FFFF;
            (x | x >> 16) & 0xFFFF_FFFF
        }
        2 => {
            x = (x | x >> 6) & 0x000F_000F_000F_000F;
            x = (x | x >> 12) & 0x0000_00FF_0000_00FF;
            (x | x >> 24) & 0xFFFF
        }
        _ => {
            x = (x | x >> 7) & 0x0003_0003_0003_0003;
            x = (x | x >> 14) & 0x0000_000F_0000_000F;
            (x | x >> 28) & 0xFF
        }
    }
}

/// The inverse of `gather8`.
#[inline(always)]
fn spread8<const BITS: u32>(mut x: u64) -> u64 {
    match BITS {
        4 => {
            x = (x | x << 16) & 0x0000_FFFF_0000_FFFF;
            x = (x | x << 8) & 0x00FF_00FF_00FF_00FF;
            (x | x << 4) & 0x0F0F_0F0F_0F0F_0F0F
        }
        2 => {
            x = (x | x << 24) & 0x0000_00FF_0000_00FF;
            x = (x | x << 12) & 0x000F_000F_000F_000F;
            (x | x << 6) & 0x0303_0303_0303_0303
        }
        _ => {
            x = (x | x << 28) & 0x0000_000F_0000_000F;
            x = (x | x << 14) & 0x0003_0003_0003_0003;
            (x | x << 7) & 0x0101_0101_0101_0101
        }
    }
}

fn pack_narrow<const BITS: u32>(slots: &[u8; SECTION_VOLUME]) -> Box<[u64]> {
    let groups = (8 / BITS) as usize;
    slots
        .as_chunks::<8>()
        .0
        .chunks_exact(groups)
        .map(|word| {
            word.iter().enumerate().fold(0u64, |acc, (g, bytes)| {
                acc | gather8::<BITS>(u64::from_le_bytes(*bytes)) << (g as u32 * 8 * BITS)
            })
        })
        .collect()
}

/// Packs one-byte slots into words the way the protocol stores them: whole
/// entries only, first entry in the low bits.
fn pack_slots(log: u32, slots: &[u8; SECTION_VOLUME]) -> Box<[u64]> {
    match log {
        0 => pack_narrow::<1>(slots),
        1 => pack_narrow::<2>(slots),
        2 => pack_narrow::<4>(slots),
        _ => slots
            .as_chunks::<8>()
            .0
            .iter()
            .map(|bytes| u64::from_le_bytes(*bytes))
            .collect(),
    }
}

fn unpack_narrow<const BITS: u32>(words: &[u64]) -> Box<[u8; SECTION_VOLUME]> {
    let groups = (8 / BITS) as usize;
    let field = (1u64 << (8 * BITS)) - 1;
    let mut slots = Box::new([0u8; SECTION_VOLUME]);
    for (out, word) in slots
        .as_chunks_mut::<8>()
        .0
        .chunks_exact_mut(groups)
        .zip(words)
    {
        for (g, bytes) in out.iter_mut().enumerate() {
            let x = spread8::<BITS>((word >> (g as u32 * 8 * BITS)) & field);
            *bytes = x.to_le_bytes();
        }
    }
    slots
}

fn unpack_slots(log: u32, words: &[u64]) -> Box<[u8; SECTION_VOLUME]> {
    match log {
        0 => unpack_narrow::<1>(words),
        1 => unpack_narrow::<2>(words),
        2 => unpack_narrow::<4>(words),
        _ => {
            let mut slots = Box::new([0u8; SECTION_VOLUME]);
            for (out, word) in slots.as_chunks_mut::<8>().0.iter_mut().zip(words) {
                *out = word.to_le_bytes();
            }
            slots
        }
    }
}

impl Paletted {
    fn new(log: u32, palette: &[BlockState], slots: &[u8; SECTION_VOLUME]) -> Self {
        let bits = 1usize << log;
        let mut table = vec![palette[0]; 1 << bits].into_boxed_slice();
        table[..palette.len()].copy_from_slice(palette);
        Self {
            log,
            mask: (1u64 << bits) - 1,
            len: palette.len() as u16,
            last: 0,
            palette: table,
            words: pack_slots(log, slots),
        }
    }

    /// A two-bit section holding `other` everywhere but at `at`, where it
    /// holds `state`.
    fn pair(other: BlockState, state: BlockState, at: usize) -> Self {
        let mut words = vec![0u64; SECTION_VOLUME / 32].into_boxed_slice();
        words[at >> 5] |= 1 << ((at & 31) * 2);
        Self {
            log: 1,
            mask: 3,
            len: 2,
            last: 0,
            palette: vec![other, state, other, other].into_boxed_slice(),
            words,
        }
    }

    /// The word holding entry `i` and the entry's shift within it.
    #[inline]
    fn locate(&self, i: usize) -> (usize, u32) {
        let per_word_log = 6 - self.log;
        (
            i >> per_word_log,
            ((i & ((1 << per_word_log) - 1)) as u32) << self.log,
        )
    }

    #[inline]
    fn mask(&self) -> u64 {
        self.mask
    }

    #[inline]
    fn index(&self, i: usize) -> usize {
        let (word, shift) = self.locate(i);
        ((self.words[word] >> shift) & self.mask()) as usize
    }

    #[inline]
    fn get(&self, i: usize) -> BlockState {
        self.palette[self.index(i)]
    }

    /// Points entry `i` at `slot` and returns the slot it pointed at.
    #[inline]
    fn replace(&mut self, i: usize, slot: usize) -> usize {
        let (word, shift) = self.locate(i);
        let mask = self.mask();
        let word = &mut self.words[word];
        let old = (*word >> shift) & mask;
        *word = (*word & !(mask << shift)) | (slot as u64) << shift;
        old as usize
    }

    fn used(&self) -> &[BlockState] {
        &self.palette[..self.len as usize]
    }

    /// The slot holding `state`, added if need be, or `None` if the palette
    /// is full without it.
    #[inline]
    fn slot_of(&mut self, state: BlockState) -> Option<usize> {
        let last = self.last as usize;
        if self.palette[last] == state && last < self.len as usize {
            return Some(last);
        }
        let slot = match self.used().iter().position(|s| *s == state) {
            Some(slot) => slot,
            None => {
                let len = self.len as usize;
                if len == self.palette.len() {
                    return None;
                }
                self.palette[len] = state;
                self.len += 1;
                len
            }
        };
        self.last = slot as u16;
        Some(slot)
    }

    fn select(&self, pred: impl Fn(BlockState) -> bool) -> BlockMask {
        let mut hits = [0u64; 256];
        let mut count = 0;
        for (slot, state) in self.used().iter().enumerate() {
            let hit = pred(*state);
            hits[slot] = u64::from(hit);
            count += usize::from(hit);
        }
        if count == 0 || count == self.used().len() {
            return [if count == 0 { 0 } else { !0 }; MASK_WORDS];
        }
        match self.log {
            0 => self.select_packed::<1>(&hits),
            1 => self.select_packed::<2>(&hits),
            2 => self.select_packed::<4>(&hits),
            _ => self.select_packed::<8>(&hits),
        }
    }

    /// Calls `f` with the index and state of every entry whose state
    /// satisfies `pred`, in increasing index order.
    fn for_each_matching(
        &self,
        pred: impl Fn(BlockState) -> bool,
        f: &mut impl FnMut(usize, BlockState),
    ) {
        let mut table = [false; 256];
        let mut hit_slots = [0u8; SWAR_SLOTS];
        let mut hit_count = 0;
        for (slot, state) in self.used().iter().enumerate() {
            if pred(*state) {
                table[slot] = true;
                if hit_count < SWAR_SLOTS {
                    hit_slots[hit_count] = slot as u8;
                }
                hit_count += 1;
            }
        }
        if hit_count == 0 {
            return;
        }
        let hit_slots = &hit_slots[..hit_count.min(SWAR_SLOTS)];
        let many = hit_count > SWAR_SLOTS;
        match self.log {
            0 => self.matching_packed::<1>(&table, hit_slots, many, f),
            1 => self.matching_packed::<2>(&table, hit_slots, many, f),
            2 => self.matching_packed::<4>(&table, hit_slots, many, f),
            _ => self.matching_packed::<8>(&table, hit_slots, many, f),
        }
    }

    fn matching_packed<const BITS: usize>(
        &self,
        table: &[bool; 256],
        hit_slots: &[u8],
        many: bool,
        f: &mut impl FnMut(usize, BlockState),
    ) {
        let per_word = 64 / BITS;
        let ones = u64::MAX / ((1u64 << BITS) - 1);
        let high = ones << (BITS - 1);
        let low = !high;
        for (w, word) in self.words.iter().enumerate() {
            let base = w * per_word;
            if many {
                let mut rest = *word;
                for field in 0..per_word {
                    let slot = (rest & self.mask) as usize;
                    rest >>= BITS;
                    if table[slot] {
                        f(base + field, self.palette[slot]);
                    }
                }
                continue;
            }
            let mut found = 0u64;
            for &slot in hit_slots {
                let x = *word ^ (u64::from(slot) * ones);
                found |= !(((x & low).wrapping_add(low)) | x) & high;
            }
            while found != 0 {
                let field = found.trailing_zeros() as usize / BITS;
                found &= found - 1;
                let slot = ((*word >> (field * BITS)) & self.mask) as usize;
                f(base + field, self.palette[slot]);
            }
        }
    }

    fn select_packed<const BITS: usize>(&self, hits: &[u64; 256]) -> BlockMask {
        let per_word = 64 / BITS;
        let mask = (1u64 << BITS) - 1;
        let mut out = [0u64; MASK_WORDS];
        for (target, words) in out.iter_mut().zip(self.words.as_chunks::<BITS>().0) {
            let mut bit = 0;
            for word in words {
                let mut word = *word;
                for _ in 0..per_word {
                    *target |= hits[(word & mask) as usize] << bit;
                    word >>= BITS;
                    bit += 1;
                }
            }
        }
        out
    }

    /// Every entry's slot, unpacked a word at a time.
    fn slots(&self) -> Box<[u8; SECTION_VOLUME]> {
        unpack_slots(self.log, &self.words)
    }

    /// The blocks with `state` at `at`, keeping only the palette entries
    /// still in use and widening if they do not fit. A palette left more
    /// than three quarters full widens a step early, so a section gaining
    /// states one by one is not compacted again and again.
    fn regrown(&self, at: usize, state: BlockState) -> Blocks {
        let mut slots = self.slots();
        let mut used = [false; 256];
        for (i, slot) in slots.iter().enumerate() {
            if i != at {
                used[*slot as usize] = true;
            }
        }
        let mut remap = [0u16; 256];
        let mut palette = Vec::with_capacity(self.len as usize + 1);
        for (slot, entry) in self.used().iter().enumerate() {
            if used[slot] {
                remap[slot] = palette.len() as u16;
                palette.push(*entry);
            }
        }
        palette.push(state);
        let fresh = palette.len() - 1;
        let log = palette_log(palette.len() + palette.len() / 4).or(palette_log(palette.len()));
        match log {
            Some(log) => {
                for slot in slots.iter_mut() {
                    *slot = remap[*slot as usize] as u8;
                }
                debug_assert!(fresh < 256);
                slots[at] = fresh as u8;
                Blocks::Paletted(Paletted::new(log, &palette, &slots))
            }
            None => Blocks::Direct(Box::new(std::array::from_fn(|i| {
                if i == at {
                    state
                } else {
                    self.palette[slots[i] as usize]
                }
            }))),
        }
    }
}

impl Blocks {
    #[inline]
    fn get(&self, index: usize) -> BlockState {
        match self {
            Blocks::Uniform(state) => *state,
            Blocks::Paletted(paletted) => paletted.get(index),
            Blocks::Direct(states) => states[index],
        }
    }
}

impl PartialEq for Blocks {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Blocks::Uniform(a), Blocks::Uniform(b)) => a == b,
            _ => (0..SECTION_VOLUME).all(|i| self.get(i) == other.get(i)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Biomes {
    Uniform(u16),
    Mixed(Box<[u16; BIOME_VOLUME]>),
}

impl Biomes {
    fn new(biomes: Box<[u16; BIOME_VOLUME]>) -> Self {
        if biomes.iter().all(|b| *b == biomes[0]) {
            Biomes::Uniform(biomes[0])
        } else {
            Biomes::Mixed(biomes)
        }
    }
}

/// A 16x16x16 cube of blocks with its 4x4x4 biome grid. Block indices are
/// `y << 8 | z << 4 | x`; biome indices `y << 4 | z << 2 | x`.
#[derive(Clone, Debug)]
pub struct Section {
    blocks: Blocks,
    biomes: Biomes,
    /// How many blocks tick randomly, so sections without any are skipped.
    ticking: u16,
    non_air: u16,
    fluids: u16,
}

impl PartialEq for Section {
    fn eq(&self, other: &Self) -> bool {
        self.biomes == other.biomes && self.blocks == other.blocks
    }
}

const TICKING: u8 = 1;
const SOLID: u8 = 2;
const FLUID: u8 = 4;

const _: () = {
    assert!(TICKING == BlockState::FLAG_TICKS_RANDOMLY);
    assert!(SOLID == BlockState::FLAG_NOT_AIR);
    assert!(FLUID == BlockState::FLAG_FLUID);
};

/// What a section counts of `state`, one table read.
#[inline]
fn counted(state: BlockState) -> u8 {
    state.flags() & (TICKING | SOLID | FLUID)
}

/// Whether a random tick at a block in `state` does anything.
pub fn ticks_randomly(state: BlockState) -> bool {
    state.is_randomly_ticking() || state.is_fluid_randomly_ticking()
}

impl Section {
    pub fn uniform(state: BlockState, biome: u16) -> Self {
        let mut section = Self {
            blocks: Blocks::Uniform(state),
            biomes: Biomes::Uniform(biome),
            ticking: 0,
            non_air: 0,
            fluids: 0,
        };
        section.count(counted(state), SECTION_VOLUME as u16);
        section
    }

    /// Builds a section, collapsing uniform contents.
    pub fn new(
        blocks: Box<[BlockState; SECTION_VOLUME]>,
        biomes: Box<[u16; BIOME_VOLUME]>,
    ) -> Self {
        let mut palette: Vec<BlockState> = Vec::new();
        let mut counts = [0u16; 256];
        let mut indices = [0u8; SECTION_VOLUME];
        let mut last: Option<(BlockState, u8)> = None;
        let mut direct = false;
        for (i, state) in blocks.iter().enumerate() {
            let slot = match last {
                Some((prev, slot)) if prev == *state => slot,
                _ => match palette.iter().position(|p| p == state) {
                    Some(slot) => slot as u8,
                    None if palette.len() == 256 => {
                        direct = true;
                        break;
                    }
                    None => {
                        palette.push(*state);
                        (palette.len() - 1) as u8
                    }
                },
            };
            last = Some((*state, slot));
            indices[i] = slot;
            counts[slot as usize] += 1;
        }
        let mut section = Self {
            blocks: Blocks::Uniform(blocks[0]),
            biomes: Biomes::new(biomes),
            ticking: 0,
            non_air: 0,
            fluids: 0,
        };
        if direct {
            for state in blocks.iter() {
                section.count(counted(*state), 1);
            }
            section.blocks = Blocks::Direct(blocks);
            return section;
        }
        for (state, count) in palette.iter().zip(counts) {
            section.count(counted(*state), count);
        }
        if let Some(log) = palette_log(palette.len()).filter(|_| palette.len() > 1) {
            section.blocks = Blocks::Paletted(Paletted::new(log, &palette, &indices));
        }
        section
    }

    /// This section's biomes with `blocks` in place of its blocks.
    pub fn with_blocks(&self, blocks: Box<[BlockState; SECTION_VOLUME]>) -> Self {
        let mut section = Self::new(blocks, Box::new([0; BIOME_VOLUME]));
        section.biomes = self.biomes.clone();
        section
    }

    /// This section with `changes` (block index, state) applied, its
    /// palette holding only the states still in use.
    pub fn with_changes(&self, changes: &[(u16, BlockState)]) -> Self {
        let (mut palette, mut slots) = match &self.blocks {
            Blocks::Uniform(state) => (vec![*state], Box::new([0u8; SECTION_VOLUME])),
            Blocks::Paletted(paletted) => (paletted.used().to_vec(), paletted.slots()),
            Blocks::Direct(_) => return self.with_changed_array(changes),
        };
        let mut section = Self {
            blocks: Blocks::Uniform(palette[0]),
            biomes: self.biomes.clone(),
            ticking: self.ticking,
            non_air: self.non_air,
            fluids: self.fluids,
        };
        let mut recent = [(palette[0], 0u8); 64];
        for &(i, state) in changes {
            let entry = &mut recent[state.key() as usize & 63];
            if entry.0 != state {
                let slot = match palette.iter().position(|s| *s == state) {
                    Some(slot) => slot,
                    None if palette.len() == 256 => return self.with_changed_array(changes),
                    None => {
                        palette.push(state);
                        palette.len() - 1
                    }
                };
                *entry = (state, slot as u8);
            }
            let slot = &mut slots[i as usize];
            let (before, after) = (counted(palette[*slot as usize]), counted(state));
            if before != after {
                section.uncount(before);
                section.count(after, 1);
            }
            *slot = entry.1;
        }
        let mut in_use = [false; 256];
        for slot in slots.iter() {
            in_use[*slot as usize] = true;
        }
        let mut remap = [0u8; 256];
        let mut used = Vec::with_capacity(palette.len());
        for (slot, state) in palette.iter().enumerate() {
            if in_use[slot] {
                remap[slot] = used.len() as u8;
                used.push(*state);
            }
        }
        section.blocks = Blocks::Uniform(used[0]);
        if let Some(log) = palette_log(used.len()).filter(|_| used.len() > 1) {
            for slot in slots.iter_mut() {
                *slot = remap[*slot as usize];
            }
            section.blocks = Blocks::Paletted(Paletted::new(log, &used, &slots));
        }
        section
    }

    fn with_changed_array(&self, changes: &[(u16, BlockState)]) -> Self {
        let mut blocks = self.block_array();
        for &(i, state) in changes {
            blocks[i as usize] = state;
        }
        self.with_blocks(blocks)
    }

    pub fn set_biomes(&mut self, biomes: Box<[u16; BIOME_VOLUME]>) {
        self.biomes = Biomes::new(biomes);
    }

    fn count(&mut self, flags: u8, n: u16) {
        self.ticking += n * u16::from(flags & TICKING);
        self.non_air += n * u16::from(flags & SOLID != 0);
        self.fluids += n * u16::from(flags & FLUID != 0);
    }

    fn uncount(&mut self, flags: u8) {
        self.ticking -= u16::from(flags & TICKING);
        self.non_air -= u16::from(flags & SOLID != 0);
        self.fluids -= u16::from(flags & FLUID != 0);
    }

    /// One bit per block, in block index order, set where `pred` holds.
    pub fn select(&self, pred: impl Fn(BlockState) -> bool) -> BlockMask {
        match &self.blocks {
            Blocks::Uniform(state) => [if pred(*state) { !0 } else { 0 }; MASK_WORDS],
            Blocks::Paletted(paletted) => paletted.select(pred),
            Blocks::Direct(states) => {
                let mut out = [0u64; MASK_WORDS];
                for (i, state) in states.iter().enumerate() {
                    out[i >> 6] |= u64::from(pred(*state)) << (i & 63);
                }
                out
            }
        }
    }

    /// Calls `f` with the index and state of every block satisfying `pred`,
    /// in increasing index order, without visiting palette entries that
    /// cannot match.
    pub fn for_each_matching(
        &self,
        pred: impl Fn(BlockState) -> bool,
        mut f: impl FnMut(usize, BlockState),
    ) {
        match &self.blocks {
            Blocks::Uniform(state) => {
                if pred(*state) {
                    (0..SECTION_VOLUME).for_each(|i| f(i, *state));
                }
            }
            Blocks::Paletted(paletted) => paletted.for_each_matching(pred, &mut f),
            Blocks::Direct(states) => {
                for (i, state) in states.iter().enumerate() {
                    if pred(*state) {
                        f(i, *state);
                    }
                }
            }
        }
    }

    /// Every block, expanded from a uniform section if need be.
    pub fn block_array(&self) -> Box<[BlockState; SECTION_VOLUME]> {
        match &self.blocks {
            Blocks::Uniform(state) => Box::new([*state; SECTION_VOLUME]),
            Blocks::Direct(states) => states.clone(),
            Blocks::Paletted(paletted) => Box::new(std::array::from_fn(|i| paletted.get(i))),
        }
    }

    /// Whether any block in the section ticks randomly.
    pub fn is_randomly_ticking(&self) -> bool {
        self.ticking > 0
    }

    /// Blocks that are not air.
    pub fn non_air(&self) -> u16 {
        self.non_air
    }

    /// Bytes the section's blocks and biomes own on the heap.
    pub fn heap_bytes(&self) -> usize {
        let blocks = match &self.blocks {
            Blocks::Uniform(_) => 0,
            Blocks::Paletted(paletted) => {
                size_of_val(&*paletted.palette) + size_of_val(&*paletted.words)
            }
            Blocks::Direct(states) => size_of_val(&**states),
        };
        let biomes = match &self.biomes {
            Biomes::Uniform(_) => 0,
            Biomes::Mixed(biomes) => size_of_val(&**biomes),
        };
        blocks + biomes
    }

    #[inline]
    pub fn get(&self, index: usize) -> BlockState {
        self.blocks.get(index)
    }

    pub fn biome(&self, index: usize) -> u16 {
        match &self.biomes {
            Biomes::Uniform(biome) => *biome,
            Biomes::Mixed(biomes) => biomes[index],
        }
    }

    /// Stores `state` at `index` and returns the block it replaced.
    pub fn set(&mut self, index: usize, state: BlockState) -> BlockState {
        let old = match &mut self.blocks {
            Blocks::Uniform(current) => {
                let current = *current;
                if current == state {
                    return state;
                }
                self.blocks = Blocks::Paletted(Paletted::pair(current, state, index));
                current
            }
            Blocks::Paletted(paletted) => match paletted.slot_of(state) {
                Some(slot) => paletted.palette[paletted.replace(index, slot)],
                None => {
                    let old = paletted.get(index);
                    self.blocks = paletted.regrown(index, state);
                    old
                }
            },
            Blocks::Direct(states) => std::mem::replace(&mut states[index], state),
        };
        let (before, after) = (counted(old), counted(state));
        if before != after {
            self.uncount(before);
            self.count(after, 1);
        }
        old
    }

    /// The only state in the section, if every block has it and the
    /// section stores it that way.
    pub fn uniform_state(&self) -> Option<BlockState> {
        match &self.blocks {
            Blocks::Uniform(state) => Some(*state),
            _ => None,
        }
    }

    /// Every state the section may hold: a superset of those it does, in no
    /// particular order and possibly repeated, for skipping sections cheaply.
    pub fn palette(&self) -> &[BlockState] {
        match &self.blocks {
            Blocks::Uniform(state) => std::slice::from_ref(state),
            Blocks::Paletted(paletted) => paletted.used(),
            Blocks::Direct(states) => &states[..],
        }
    }

    /// Every block is air.
    pub fn is_empty(&self) -> bool {
        self.non_air == 0
    }

    /// Every block is air, even if the section mixes air kinds.
    pub fn has_only_air(&self) -> bool {
        self.non_air == 0
    }

    /// Distinct values in first-seen order and each entry's palette index.
    pub fn block_palette(&self) -> (Vec<BlockState>, Vec<u16>) {
        match &self.blocks {
            Blocks::Uniform(state) => (vec![*state], Vec::new()),
            Blocks::Direct(states) => palette(states.iter().copied()),
            Blocks::Paletted(paletted) => {
                let mut remap = [u16::MAX; 256];
                let mut distinct: Vec<BlockState> = Vec::new();
                let indices = (0..SECTION_VOLUME)
                    .map(|i| {
                        let slot = paletted.index(i);
                        if remap[slot] == u16::MAX {
                            let state = paletted.palette[slot];
                            remap[slot] = match distinct.iter().position(|s| *s == state) {
                                Some(index) => index as u16,
                                None => {
                                    distinct.push(state);
                                    (distinct.len() - 1) as u16
                                }
                            };
                        }
                        remap[slot]
                    })
                    .collect();
                (distinct, indices)
            }
        }
    }

    /// Replaces biome cells where `pick` (given the quart index and the
    /// current biome) returns a different one; returns how many changed.
    fn replace_biomes(&mut self, mut pick: impl FnMut(usize, u16) -> Option<u16>) -> usize {
        let mut cells = Box::new([0u16; BIOME_VOLUME]);
        let mut changed = 0;
        for (i, cell) in cells.iter_mut().enumerate() {
            let current = self.biome(i);
            *cell = match pick(i, current) {
                Some(new) if new != current => {
                    changed += 1;
                    new
                }
                _ => current,
            };
        }
        if changed > 0 {
            self.biomes = Biomes::new(cells);
        }
        changed
    }

    pub fn biome_palette(&self) -> (Vec<u16>, Vec<u16>) {
        match &self.biomes {
            Biomes::Uniform(biome) => (vec![*biome], Vec::new()),
            Biomes::Mixed(biomes) => palette(biomes.iter().copied()),
        }
    }

    fn encode(&self, out: &mut PacketWriter) {
        out.write_short(self.non_air as i16);
        out.write_short(self.fluids as i16);
        match &self.blocks {
            Blocks::Uniform(state) => {
                out.write_unsigned_byte(0);
                out.write_var_int(state.id() as i32);
            }
            Blocks::Paletted(paletted) => {
                let bits = (1u32 << paletted.log).max(MIN_INDIRECT_BLOCK_BITS);
                out.write_unsigned_byte(bits as u8);
                out.write_var_int(paletted.len as i32);
                for state in paletted.used() {
                    out.write_var_int(state.id() as i32);
                }
                if bits == 1 << paletted.log {
                    for word in &paletted.words {
                        out.write_long(*word as i64);
                    }
                } else {
                    write_packed(
                        out,
                        bits,
                        (0..SECTION_VOLUME).map(|i| paletted.index(i) as u64),
                    );
                }
            }
            Blocks::Direct(states) => {
                let bits = ceil_log2(BlockState::COUNT);
                out.write_unsigned_byte(bits as u8);
                write_packed(out, bits, states.iter().map(|s| s.id() as u64));
            }
        }
        self.encode_biomes(out);
    }

    fn encode_biomes(&self, out: &mut PacketWriter) {
        let (biomes, indices) = self.biome_palette();
        if biomes.len() == 1 {
            out.write_unsigned_byte(0);
            out.write_var_int(biomes[0] as i32);
        } else if ceil_log2(biomes.len()) <= MAX_INDIRECT_BIOME_BITS {
            let bits = ceil_log2(biomes.len());
            out.write_unsigned_byte(bits as u8);
            out.write_var_int(biomes.len() as i32);
            for biome in &biomes {
                out.write_var_int(*biome as i32);
            }
            write_packed(out, bits, indices.iter().map(|i| *i as u64));
        } else {
            let bits = ceil_log2(WORLDGEN_BIOME.entries.len());
            out.write_unsigned_byte(bits as u8);
            write_packed(
                out,
                bits,
                indices.iter().map(|i| biomes[*i as usize] as u64),
            );
        }
    }
}

/// A 16-wide column of blocks spanning its dimension's [`ChunkShape`].
#[derive(Clone, Debug)]
pub struct Chunk {
    shape: ChunkShape,
    sections: Vec<Arc<Section>>,
    /// One bit per section that has randomly ticking blocks, so random
    /// ticks skip idle sections without touching them.
    ticking: [u64; TICKING_WORDS],
    /// Changes whenever a block changes, unique across all chunks, so a
    /// reader can tell whether a chunk is still the one it looked at.
    stamp: u64,
    light: Option<Arc<ChunkLight>>,
    /// Per column (`z << 4 | x`), one above the highest block that blocks
    /// motion, filled on first use and kept current by [`Chunk::set`].
    heights: std::sync::OnceLock<Box<[i32; 256]>>,
}

impl PartialEq for Chunk {
    fn eq(&self, other: &Self) -> bool {
        self.shape == other.shape && self.sections == other.sections && self.light == other.light
    }
}

fn next_stamp() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The game's tallest dimension is 4064 blocks: 254 sections.
const TICKING_WORDS: usize = 4;

impl Chunk {
    pub fn empty(shape: ChunkShape) -> Self {
        Self::uniform(shape, Block::Air.default_state(), plains())
    }

    pub fn uniform(shape: ChunkShape, state: BlockState, biome: u16) -> Self {
        let section = Arc::new(Section::uniform(state, biome));
        Self::with_sections(shape, vec![section; shape.sections()], None)
    }

    fn with_sections(
        shape: ChunkShape,
        sections: Vec<Arc<Section>>,
        light: Option<Arc<ChunkLight>>,
    ) -> Self {
        assert!(sections.len() <= TICKING_WORDS * 64, "too many sections");
        let mut chunk = Self {
            shape,
            sections,
            ticking: [0; TICKING_WORDS],
            stamp: next_stamp(),
            light,
            heights: std::sync::OnceLock::new(),
        };
        for index in 0..chunk.sections.len() {
            chunk.mark_ticking(index);
        }
        chunk
    }

    fn mark_ticking(&mut self, index: usize) {
        let bit = 1u64 << (index % 64);
        if self.sections[index].is_randomly_ticking() {
            self.ticking[index / 64] |= bit;
        } else {
            self.ticking[index / 64] &= !bit;
        }
    }

    /// Changes on every block change; see the field.
    pub fn stamp(&self) -> u64 {
        self.stamp
    }

    /// How many sections have randomly ticking blocks.
    pub fn ticking_sections(&self) -> u32 {
        self.ticking.iter().map(|word| word.count_ones()).sum()
    }

    /// The lowest section at or above `from` with randomly ticking blocks.
    pub fn next_ticking_section(&self, from: usize) -> Option<usize> {
        let mut word = from / 64;
        let mut bits = self.ticking.get(word)? & (u64::MAX << (from % 64));
        loop {
            if bits != 0 {
                return Some(word * 64 + bits.trailing_zeros() as usize);
            }
            word += 1;
            bits = *self.ticking.get(word)?;
        }
    }

    /// `sections` lists every section from the bottom of the world up.
    pub fn from_sections(
        shape: ChunkShape,
        sections: Vec<Section>,
        light: Option<ChunkLight>,
    ) -> Self {
        assert_eq!(sections.len(), shape.sections());
        Self::with_sections(
            shape,
            sections.into_iter().map(Arc::new).collect(),
            light.map(Arc::new),
        )
    }

    pub fn shape(&self) -> ChunkShape {
        self.shape
    }

    pub fn sections(&self) -> impl ExactSizeIterator<Item = &Section> + DoubleEndedIterator {
        self.sections.iter().map(|s| &**s)
    }

    pub fn section(&self, index: usize) -> &Section {
        &self.sections[index]
    }

    /// The section at `index` as shared with snapshots of this chunk: two
    /// snapshots hold the same `Arc` exactly while the section is unchanged.
    pub fn shared_section(&self, index: usize) -> &Arc<Section> {
        &self.sections[index]
    }

    /// A copy with the same shape and light but `sections` in place of the
    /// blocks.
    pub fn with_shared_sections(&self, sections: Vec<Arc<Section>>) -> Self {
        assert_eq!(sections.len(), self.sections.len());
        Self::with_sections(self.shape, sections, self.light.clone())
    }

    pub fn light(&self) -> Option<&ChunkLight> {
        self.light.as_deref()
    }

    pub fn set_light(&mut self, light: Option<ChunkLight>) {
        self.light = light.map(Arc::new);
    }

    /// Blocks that differ in `newer`, as chunk-local `x`/`z` and absolute
    /// `y`. Sections still shared between the two snapshots are skipped.
    pub fn changed_blocks(&self, newer: &Chunk) -> Vec<(i32, i32, i32, BlockState)> {
        let mut changed = Vec::new();
        for (index, (old, new)) in self.sections.iter().zip(&newer.sections).enumerate() {
            if Arc::ptr_eq(old, new) {
                continue;
            }
            let base = self.shape.section_base(index);
            for i in 0..SECTION_VOLUME {
                let state = new.get(i);
                if old.get(i) != state {
                    let (x, y, z) = ((i & 15) as i32, (i >> 8) as i32, ((i >> 4) & 15) as i32);
                    changed.push((x, base + y, z, state));
                }
            }
        }
        changed
    }

    /// Blocks that create a block entity, as chunk-local `x`/`z` and
    /// absolute `y`.
    pub fn block_entity_blocks(&self) -> Vec<(i32, i32, i32, BlockState)> {
        let mut found = Vec::new();
        for (index, section) in self.sections.iter().enumerate() {
            let base = self.shape.section_base(index);
            let mut push = |i: usize, state: BlockState| {
                let (x, y, z) = ((i & 15) as i32, (i >> 8) as i32, ((i >> 4) & 15) as i32);
                found.push((x, base + y, z, state));
            };
            if !section
                .palette()
                .iter()
                .any(|state| state.block().creates_block_entity())
            {
                continue;
            }
            for i in 0..SECTION_VOLUME {
                let state = section.get(i);
                if state.block().creates_block_entity() {
                    push(i, state);
                }
            }
        }
        found
    }

    fn locate(&self, x: i32, y: i32, z: i32) -> Option<(usize, usize)> {
        if !self.shape.contains_y(y) {
            return None;
        }
        let local_y = (y - self.shape.min_y) as usize;
        let index = ((local_y & 15) << 8) | (((z & 15) as usize) << 4) | (x & 15) as usize;
        Some((local_y >> 4, index))
    }

    /// Block at chunk-local `x`/`z` (only the low four bits are used) and absolute `y`.
    pub fn get(&self, x: i32, y: i32, z: i32) -> BlockState {
        match self.locate(x, y, z) {
            Some((section, index)) => self.sections[section].get(index),
            None => Block::Air.default_state(),
        }
    }

    /// Biome at chunk-local block `x`/`z` and absolute `y`, at quart
    /// resolution.
    pub fn biome(&self, x: i32, y: i32, z: i32) -> u16 {
        match self.locate(x, y, z) {
            Some((section, _)) => {
                let local_y = ((y - self.shape.min_y) & 15) >> 2;
                self.sections[section]
                    .biome(((local_y << 4) | (((z & 15) >> 2) << 2) | ((x & 15) >> 2)) as usize)
            }
            None => plains(),
        }
    }

    /// Sets every biome cell for which `pick(x, y, z, current)` (chunk-local
    /// block coordinates of the cell's corner) names another biome; returns
    /// how many cells changed.
    pub fn fill_biomes(
        &mut self,
        mut pick: impl FnMut(i32, i32, i32, u16) -> Option<u16>,
    ) -> usize {
        let min_y = self.shape.min_y;
        let mut changed = 0;
        for index in 0..self.sections.len() {
            let base = min_y + index as i32 * 16;
            changed += Arc::make_mut(&mut self.sections[index]).replace_biomes(|i, current| {
                let x = (i & 3) as i32 * 4;
                let z = ((i >> 2) & 3) as i32 * 4;
                let y = base + (i >> 4) as i32 * 4;
                pick(x, y, z, current)
            });
        }
        if changed > 0 {
            self.stamp = next_stamp();
        }
        changed
    }

    /// Every section's biomes as the Chunks Biomes packet carries them.
    pub fn encode_biomes(&self) -> Bytes {
        let mut w = PacketWriter::new();
        for section in &self.sections {
            section.encode_biomes(&mut w);
        }
        w.into_bytes()
    }

    /// Returns the previous state, or `None` if `y` is outside the world.
    pub fn set(&mut self, x: i32, y: i32, z: i32, state: BlockState) -> Option<BlockState> {
        let (section, index) = self.locate(x, y, z)?;
        if self.sections[section].get(index) == state {
            return Some(state);
        }
        let old = Arc::make_mut(&mut self.sections[section]).set(index, state);
        self.mark_ticking(section);
        self.update_height(x, y, z, state);
        self.stamp = next_stamp();
        Some(old)
    }

    fn column_height(&self, x: i32, z: i32, from_y: i32) -> i32 {
        let min_y = self.shape.min_y;
        let mut y = from_y;
        while y >= min_y {
            let Some((section, index)) = self.locate(x, y, z) else {
                break;
            };
            if self.sections[section].is_empty() {
                y = self.shape.section_base(section) - 1;
                continue;
            }
            if blocks_motion(self.sections[section].get(index)) {
                return y + 1;
            }
            y -= 1;
        }
        min_y
    }

    fn update_height(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
        let column = (((z & 15) << 4) | (x & 15)) as usize;
        let Some(heights) = self.heights.get() else {
            return;
        };
        let current = heights[column];
        let top = y + 1;
        let updated = if blocks_motion(state) {
            current.max(top)
        } else if current == top {
            self.column_height(x, z, y - 1)
        } else {
            return;
        };
        if let Some(heights) = self.heights.get_mut() {
            heights[column] = updated;
        }
    }

    /// One above the highest block in the column that blocks motion, or the
    /// world's bottom when there is none.
    pub fn motion_blocking_height(&self, x: i32, z: i32) -> i32 {
        let heights = self.heights.get_or_init(|| {
            let top = self.shape.min_y + self.shape.height - 1;
            let mut heights = Box::new([0; 256]);
            for column in 0..256 {
                heights[column] =
                    self.column_height((column & 15) as i32, (column >> 4) as i32, top);
            }
            heights
        });
        heights[(((z & 15) << 4) | (x & 15)) as usize]
    }

    /// One above the highest non-air block in the column, or the world's
    /// bottom when there is none.
    pub fn world_surface_height(&self, x: i32, z: i32) -> i32 {
        self.top_y(x, z).map_or(self.shape.min_y, |y| y + 1)
    }

    /// Highest non-air block in a column.
    fn top_y(&self, x: i32, z: i32) -> Option<i32> {
        self.top_where(x, z, |state| !is_air(state))
    }

    fn top_where(&self, x: i32, z: i32, matches: impl Fn(BlockState) -> bool) -> Option<i32> {
        for (index, section) in self.sections.iter().enumerate().rev() {
            if section.is_empty() {
                continue;
            }
            let base = self.shape.section_base(index);
            for local_y in (0..16).rev() {
                let i = ((local_y as usize) << 8) | (((z & 15) as usize) << 4) | (x & 15) as usize;
                if matches(section.get(i)) {
                    return Some(base + local_y);
                }
            }
        }
        None
    }

    /// The Chunk Data and Update Light body (everything after the packet ID).
    /// `block_entities` must list every block entity in the chunk, as
    /// `(position, block entity type id, client nbt)`: clients only create
    /// the block entities listed here, and an empty nbt is sent as absent.
    pub fn encode(
        &self,
        chunk_x: i32,
        chunk_z: i32,
        block_entities: &[(BlockPos, u32, Compound)],
    ) -> Bytes {
        let leaves = tuff_vanilla_data::growth::block_tag("minecraft:leaves");
        let min_y = self.shape.min_y;
        let bits = self.shape.heightmap_bits();
        let map = |matches: &dyn Fn(BlockState) -> bool| {
            pack(
                bits,
                (0..256).map(|i| {
                    self.top_where(i & 15, i >> 4, matches)
                        .map_or(0, |y| (y - min_y + 1) as u64)
                }),
            )
        };
        let surface = map(&|state| !is_air(state));
        let motion = map(&|state| blocks_motion(state) || is_fluid(state));
        let no_leaves = map(&|state| {
            (blocks_motion(state) || is_fluid(state)) && !leaves.contains(&state.block())
        });

        let mut w = PacketWriter::new();
        w.write_int(chunk_x);
        w.write_int(chunk_z);
        w.write_var_int(3);
        for (kind, heights) in [
            (WORLD_SURFACE, &surface),
            (MOTION_BLOCKING, &motion),
            (MOTION_BLOCKING_NO_LEAVES, &no_leaves),
        ] {
            w.write_var_int(kind);
            w.write_var_int(heights.len() as i32);
            for word in heights {
                w.write_long(*word as i64);
            }
        }

        let mut sections = PacketWriter::new();
        for section in &self.sections {
            section.encode(&mut sections);
        }
        let sections = sections.into_bytes();
        w.write_var_int(sections.len() as i32);
        w.write_bytes(&sections);
        w.write_var_int(block_entities.len() as i32);
        for (pos, kind, nbt) in block_entities {
            w.write_byte((((pos.x & 15) << 4) | (pos.z & 15)) as i8);
            w.write_short(pos.y as i16);
            w.write_var_int(*kind as i32);
            if nbt.is_empty() {
                w.write_byte(TagType::End as i8);
            } else {
                w.write_nbt(&Tag::Compound(nbt.clone()))
                    .expect("in-memory NBT encodes");
            }
        }

        match &self.light {
            Some(light) => write_light(&mut w, &light.sky, &light.block),
            None => {
                let missing = vec![SectionLight::Missing; self.shape.light_sections()];
                write_light(&mut w, &missing, &missing);
            }
        }
        w.into_bytes()
    }
}

mod light;
pub use light::*;

mod packing;
pub use packing::*;

#[cfg(test)]
mod tests;
