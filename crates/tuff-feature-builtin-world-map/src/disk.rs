//! The persistent tier of the tile cache. It lives in the world's extra
//! files (plain files below a vanilla world's root, `sqlar` rows inside a
//! `.mcserver` world), so it travels with the world and its backups; a
//! hard size budget with least-recently-used deletion keeps that cost
//! bounded.
//!
//! One file per tile holds the level-0 samples (and the row north of the
//! tile) and the encoded PNG; a zoomed tile holds only its PNG. A small
//! index lists every tile with its size and last use. It is written to
//! two slots in turn, each with a sequence number and a CRC, so a torn or
//! refused write never loses the last good index. Everything read back is
//! untrusted: sizes are capped, checksums and keys checked, and anything
//! wrong is ignored.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write as _};

use bytes::Bytes;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use tuff_storage::backend::WorldBackend;

use crate::png;
use crate::surface::Sample;
use crate::tile::{BaseTile, MAX_LEVEL, PIXELS, Rect, SIDE, TileKey};

pub const ROOT: &str = "tuff/world-map/v1/";
pub const SLOTS: [&str; 2] = ["tuff/world-map/v1/index.0", "tuff/world-map/v1/index.1"];
pub const MAX_TILE_BYTES: usize = 4 << 20;
const MAX_INDEX_BYTES: usize = 8 << 20;
const MAX_DIMS: usize = 1024;
const MAX_DIM_NAME: usize = 256;
const TILE_MAGIC: [u8; 4] = *b"TMTL";
const INDEX_MAGIC: [u8; 4] = *b"TMIX";
const FORMAT: u8 = 1;
const HAS_SAMPLES: u8 = 1;
const HAS_PNG: u8 = 2;
const SAMPLE_COUNT: usize = SIDE + PIXELS;
const SAMPLE_BYTES: usize = SAMPLE_COUNT * 4;
const ENTRY_BYTES: usize = 2 + 1 + 4 + 4 + 4 + 8;

/// A tile as the index names it: `dim` indexes [`DiskIndex::dims`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DiskKey {
    pub dim: u16,
    pub level: u8,
    pub x: i32,
    pub z: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub size: u32,
    /// A logical clock reading; larger is more recent.
    pub used: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskIndex {
    pub seq: u64,
    pub clock: u64,
    pub dims: Vec<String>,
    pub entries: HashMap<DiskKey, Meta>,
}

impl DiskIndex {
    /// The index of dimension `name`, added if new.
    pub fn dim(&mut self, name: &str) -> u16 {
        match self.dims.iter().position(|d| d == name) {
            Some(i) => i as u16,
            None => {
                self.dims.push(name.to_owned());
                (self.dims.len() - 1) as u16
            }
        }
    }

    pub fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    pub fn path(&self, key: DiskKey) -> String {
        tile_path(&self.dims[usize::from(key.dim)], key.level, key.x, key.z)
    }

    /// Bytes the cache takes in storage: every tile and one index slot.
    pub fn total(&self) -> u64 {
        self.entries
            .values()
            .map(|m| u64::from(m.size))
            .sum::<u64>()
            + self.encoded_len() as u64
    }

    fn encoded_len(&self) -> usize {
        34 + self.dims.iter().map(|d| 2 + d.len()).sum::<usize>() + self.entries.len() * ENTRY_BYTES
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        out.extend_from_slice(&INDEX_MAGIC);
        out.extend_from_slice(&[FORMAT, 0, 0, 0]);
        out.extend_from_slice(&self.seq.to_le_bytes());
        out.extend_from_slice(&self.clock.to_le_bytes());
        out.extend_from_slice(&(self.dims.len() as u16).to_le_bytes());
        for dim in &self.dims {
            out.extend_from_slice(&(dim.len() as u16).to_le_bytes());
            out.extend_from_slice(dim.as_bytes());
        }
        let mut entries: Vec<(&DiskKey, &Meta)> = self.entries.iter().collect();
        entries.sort_unstable_by_key(|(k, _)| **k);
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for (key, meta) in entries {
            out.extend_from_slice(&key.dim.to_le_bytes());
            out.push(key.level);
            out.extend_from_slice(&key.x.to_le_bytes());
            out.extend_from_slice(&key.z.to_le_bytes());
            out.extend_from_slice(&meta.size.to_le_bytes());
            out.extend_from_slice(&meta.used.to_le_bytes());
        }
        let crc = crc(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }

    /// An index read back; `None` unless every byte checks out.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let (body, stored) = bytes.split_last_chunk::<4>()?;
        if crc(body).to_le_bytes() != *stored {
            return None;
        }
        let mut r = Reader(body);
        if r.take(4)? != INDEX_MAGIC || r.take(4)? != [FORMAT, 0, 0, 0] {
            return None;
        }
        let seq = r.u64()?;
        let clock = r.u64()?;
        let dim_count = usize::from(r.u16()?);
        if dim_count > MAX_DIMS {
            return None;
        }
        let mut dims = Vec::with_capacity(dim_count);
        for _ in 0..dim_count {
            let len = usize::from(r.u16()?);
            if len == 0 || len > MAX_DIM_NAME {
                return None;
            }
            let name = std::str::from_utf8(r.take(len)?).ok()?.to_owned();
            if dims.contains(&name) {
                return None;
            }
            dims.push(name);
        }
        let count = r.u32()? as usize;
        if count.checked_mul(ENTRY_BYTES)? != r.0.len() {
            return None;
        }
        let mut entries = HashMap::with_capacity(count);
        for _ in 0..count {
            let key = DiskKey {
                dim: r.u16()?,
                level: r.u8()?,
                x: r.i32()?,
                z: r.i32()?,
            };
            let meta = Meta {
                size: r.u32()?,
                used: r.u64()?,
            };
            if usize::from(key.dim) >= dims.len()
                || key.level > MAX_LEVEL
                || meta.size as usize > MAX_TILE_BYTES
                || meta.used > clock
                || entries.insert(key, meta).is_some()
            {
                return None;
            }
        }
        Some(Self {
            seq,
            clock,
            dims,
            entries,
        })
    }
}

fn crc(bytes: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(bytes);
    crc.sum()
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Some(head)
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.array()?))
    }

    fn i32(&mut self) -> Option<i32> {
        Some(i32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.array()?))
    }
}

/// A dimension key as a directory: `namespace/path` when every segment is
/// plain, else a hex form that can never leave the cache directory.
pub fn dim_dir(key: &str) -> String {
    let mapped = key.replacen(':', "/", 1);
    let plain = mapped.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.".contains(&b))
    });
    if plain && mapped.contains('/') {
        mapped
    } else {
        let hex: String = key.bytes().map(|b| format!("{b:02x}")).collect();
        format!("x{hex}")
    }
}

pub fn tile_path(dim: &str, level: u8, x: i32, z: i32) -> String {
    format!("{ROOT}{}/{level}/{x}.{z}.tile", dim_dir(dim))
}

/// A tile file: the level-0 samples and north row (stored as four byte
/// planes, which compress far better than whole samples) and the PNG.
pub fn encode_tile(
    dim: &str,
    key: TileKey,
    samples: Option<(&[Sample], &[Sample])>,
    png: Option<&[u8]>,
) -> Vec<u8> {
    let packed = samples.map(|(samples, north)| {
        let mut planes = vec![0u8; SAMPLE_BYTES];
        for (i, sample) in north.iter().chain(samples.iter()).enumerate() {
            for (plane, byte) in sample.bits().to_le_bytes().into_iter().enumerate() {
                planes[plane * SAMPLE_COUNT + i] = byte;
            }
        }
        let mut zlib =
            ZlibEncoder::new(Vec::with_capacity(SAMPLE_BYTES / 8), Compression::default());
        zlib.write_all(&planes).expect("writing to memory");
        zlib.finish().expect("writing to memory")
    });
    let flags =
        if packed.is_some() { HAS_SAMPLES } else { 0 } | if png.is_some() { HAS_PNG } else { 0 };
    let packed = packed.unwrap_or_default();
    let png = png.unwrap_or_default();
    let mut out = Vec::with_capacity(32 + dim.len() + packed.len() + png.len());
    out.extend_from_slice(&TILE_MAGIC);
    out.extend_from_slice(&[FORMAT, key.level, flags, 0]);
    out.extend_from_slice(&key.x.to_le_bytes());
    out.extend_from_slice(&key.z.to_le_bytes());
    out.extend_from_slice(&(dim.len() as u16).to_le_bytes());
    out.extend_from_slice(dim.as_bytes());
    out.extend_from_slice(&(packed.len() as u32).to_le_bytes());
    out.extend_from_slice(&(png.len() as u32).to_le_bytes());
    out.extend_from_slice(&packed);
    out.extend_from_slice(png);
    let crc = crc(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

/// A persisted tile, ready to cache.
pub enum Persisted {
    Base {
        tile: Box<BaseTile>,
        png: Option<Bytes>,
    },
    Zoomed {
        pixels: Box<[u8]>,
        png: Bytes,
    },
}

/// Why a tile file was ignored.
#[derive(Debug, PartialEq, Eq)]
pub enum Corrupt {
    Size,
    Checksum,
    Header,
    Key,
    Layout,
    Samples,
    Png,
}

/// The tile `key` of dimension `dim` from its file, checked end to end.
/// A level-0 tile's pixels are shaded again from its samples, and its PNG
/// is kept only if it shows exactly those pixels.
pub fn decode_tile(bytes: &[u8], dim: &str, key: TileKey) -> Result<Persisted, Corrupt> {
    if bytes.len() > MAX_TILE_BYTES {
        return Err(Corrupt::Size);
    }
    let (body, stored) = bytes.split_last_chunk::<4>().ok_or(Corrupt::Size)?;
    if crc(body).to_le_bytes() != *stored {
        return Err(Corrupt::Checksum);
    }
    let mut r = Reader(body);
    let header = r.take(8).ok_or(Corrupt::Header)?;
    if header[..4] != TILE_MAGIC || header[4] != FORMAT || header[7] != 0 {
        return Err(Corrupt::Header);
    }
    let (level, flags) = (header[5], header[6]);
    let x = r.i32().ok_or(Corrupt::Header)?;
    let z = r.i32().ok_or(Corrupt::Header)?;
    let name_len = usize::from(r.u16().ok_or(Corrupt::Header)?);
    let name = r.take(name_len).ok_or(Corrupt::Header)?;
    if level != key.level || x != key.x || z != key.z || name != dim.as_bytes() {
        return Err(Corrupt::Key);
    }
    let packed_len = r.u32().ok_or(Corrupt::Layout)? as usize;
    let png_len = r.u32().ok_or(Corrupt::Layout)? as usize;
    if packed_len.checked_add(png_len) != Some(r.0.len())
        || (flags & HAS_SAMPLES != 0) != (packed_len > 0)
        || (flags & HAS_PNG != 0) != (png_len > 0)
        || flags & !(HAS_SAMPLES | HAS_PNG) != 0
        || (level == 0) != (packed_len > 0)
        || (level > 0 && png_len == 0)
    {
        return Err(Corrupt::Layout);
    }
    let packed = r.take(packed_len).ok_or(Corrupt::Layout)?;
    let png_bytes = r.take(png_len).ok_or(Corrupt::Layout)?;
    if level > 0 {
        let pixels = png::decode(png_bytes, SIDE).ok_or(Corrupt::Png)?;
        return Ok(Persisted::Zoomed {
            pixels: pixels.into_boxed_slice(),
            png: Bytes::copy_from_slice(png_bytes),
        });
    }
    let mut planes = Vec::with_capacity(SAMPLE_BYTES);
    ZlibDecoder::new(packed)
        .take(SAMPLE_BYTES as u64 + 1)
        .read_to_end(&mut planes)
        .map_err(|_| Corrupt::Samples)?;
    if planes.len() != SAMPLE_BYTES {
        return Err(Corrupt::Samples);
    }
    let sample = |i: usize| {
        Sample::from_bits(u32::from_le_bytes([
            planes[i],
            planes[SAMPLE_COUNT + i],
            planes[2 * SAMPLE_COUNT + i],
            planes[3 * SAMPLE_COUNT + i],
        ]))
    };
    let mut tile = Box::new(BaseTile::empty());
    for (i, s) in tile.north.iter_mut().enumerate() {
        *s = sample(i);
    }
    for (i, s) in tile.samples.iter_mut().enumerate() {
        *s = sample(SIDE + i);
    }
    tile.shade(Rect::FULL, key.origin());
    let png = (png_len > 0)
        .then(|| png::decode(png_bytes, SIDE))
        .flatten()
        .filter(|pixels| pixels[..] == tile.pixels[..])
        .map(|_| Bytes::copy_from_slice(png_bytes));
    Ok(Persisted::Base { tile, png })
}

/// What the last session left: the newest good index, and files below
/// [`ROOT`] it does not list (from a write whose index never landed).
pub struct Loaded {
    pub index: DiskIndex,
    pub orphans: Vec<String>,
}

pub fn load(backend: &dyn WorldBackend) -> Loaded {
    let index = SLOTS
        .iter()
        .filter_map(|slot| backend.read_extra_file_capped(slot, MAX_INDEX_BYTES).ok())
        .filter_map(|bytes| DiskIndex::decode(&bytes))
        .max_by_key(|index| index.seq)
        .unwrap_or_default();
    let known: HashSet<String> = index.entries.keys().map(|k| index.path(*k)).collect();
    let orphans = backend
        .list_extra_files_under(ROOT)
        .unwrap_or_default()
        .into_iter()
        .filter(|path| !SLOTS.contains(&path.as_str()) && !known.contains(path))
        .collect();
    Loaded { index, orphans }
}

/// A tile's bytes, or why there are none: `Ok(None)` if the file is gone.
pub fn read(backend: &dyn WorldBackend, path: &str) -> io::Result<Option<Vec<u8>>> {
    match backend.read_extra_file_capped(path, MAX_TILE_BYTES) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

/// A tile file to store.
pub struct Write {
    pub key: DiskKey,
    pub used: u64,
    pub bytes: Vec<u8>,
}

/// Which entries to delete so `writes` fit under `budget` bytes, least
/// recently used first; writes that cannot fit at all are returned apart.
pub fn plan(
    index: &DiskIndex,
    budget: u64,
    writes: Vec<Write>,
) -> (Vec<DiskKey>, Vec<Write>, Vec<DiskKey>) {
    let mut projected = index.clone();
    let mut kept = Vec::with_capacity(writes.len());
    let mut skipped = Vec::new();
    for write in writes {
        if write.bytes.len() > MAX_TILE_BYTES || write.bytes.len() as u64 >= budget {
            skipped.push(write.key);
            continue;
        }
        projected.entries.insert(
            write.key,
            Meta {
                size: write.bytes.len() as u32,
                used: write.used,
            },
        );
        kept.push(write);
    }
    let writing: HashSet<DiskKey> = kept.iter().map(|w| w.key).collect();
    let mut candidates: Vec<(u64, DiskKey)> = projected
        .entries
        .iter()
        .filter(|(k, _)| !writing.contains(k))
        .map(|(k, m)| (m.used, *k))
        .collect();
    candidates.sort_unstable();
    let mut evict = Vec::new();
    let mut candidates = candidates.into_iter();
    while projected.total() > budget {
        match candidates.next() {
            Some((_, key)) => {
                projected.entries.remove(&key);
                evict.push(key);
            }
            None => {
                while projected.total() > budget {
                    let Some(write) = kept.pop() else { break };
                    projected.entries.remove(&write.key);
                    skipped.push(write.key);
                }
                break;
            }
        }
    }
    (evict, kept, skipped)
}

/// What a flush did.
#[derive(Debug, Default)]
pub struct Outcome {
    pub deleted: Vec<String>,
    pub evicted: Vec<DiskKey>,
    pub written: Vec<(DiskKey, Meta)>,
    pub unwritten: Vec<DiskKey>,
    pub skipped: Vec<DiskKey>,
    /// The sequence number of the index written, if it was.
    pub index: Option<u64>,
    /// The save gate refused a write; the rest waits for a later flush.
    pub busy: bool,
}

/// One flush, on a blocking thread: deletes `doomed` paths and the
/// entries evicted for `writes`, stores `writes`, then writes the index
/// that results to the slot after `index`'s. It stops at the first
/// refusal from the save gate; nothing it skips leaves the index wrong,
/// since the index is written last and only for what was done.
pub fn flush(
    backend: &dyn WorldBackend,
    mut index: DiskIndex,
    budget: u64,
    doomed: Vec<String>,
    writes: Vec<Write>,
    write_index: bool,
) -> Outcome {
    let (evict, writes, skipped) = plan(&index, budget, writes);
    let mut outcome = Outcome {
        skipped,
        ..Outcome::default()
    };
    let busy = |err: &io::Error| err.kind() == io::ErrorKind::ResourceBusy;
    let mut deletes: Vec<(Option<DiskKey>, String)> =
        doomed.into_iter().map(|p| (None, p)).collect();
    deletes.extend(evict.into_iter().map(|k| (Some(k), index.path(k))));
    for (key, path) in deletes {
        match backend.remove_extra_file(&path) {
            Err(err) if busy(&err) => {
                outcome.busy = true;
                break;
            }
            Err(err) if err.kind() != io::ErrorKind::NotFound => {
                tuff_core::log::debug!("world map: could not delete {path}: {err}");
            }
            _ => {}
        }
        if let Some(key) = key {
            index.entries.remove(&key);
            outcome.evicted.push(key);
        }
        outcome.deleted.push(path);
    }
    let mut writes = writes.into_iter();
    if !outcome.busy {
        for write in writes.by_ref() {
            let path = index.path(write.key);
            match backend.write_extra_file(&path, &write.bytes) {
                Ok(()) => {
                    let meta = Meta {
                        size: write.bytes.len() as u32,
                        used: write.used,
                    };
                    index.entries.insert(write.key, meta);
                    outcome.written.push((write.key, meta));
                }
                Err(err) if busy(&err) => {
                    outcome.busy = true;
                    outcome.unwritten.push(write.key);
                    break;
                }
                Err(err) => {
                    tuff_core::log::debug!("world map: could not write {path}: {err}");
                    outcome.unwritten.push(write.key);
                }
            }
        }
    }
    outcome.unwritten.extend(writes.map(|w| w.key));
    let changed = !outcome.deleted.is_empty() || !outcome.written.is_empty();
    if !outcome.busy && (changed || write_index) {
        index.seq += 1;
        let slot = SLOTS[(index.seq % 2) as usize];
        match backend.write_extra_file(slot, &index.encode()) {
            Ok(()) => outcome.index = Some(index.seq),
            Err(err) => {
                outcome.busy |= busy(&err);
                if !busy(&err) {
                    tuff_core::log::debug!("world map: could not write its index: {err}");
                }
            }
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(level: u8, x: i32, z: i32) -> TileKey {
        TileKey::new(0, level, x, z)
    }

    fn base_file() -> (Vec<u8>, Box<BaseTile>) {
        let mut tile = Box::new(BaseTile::empty());
        for (i, s) in tile.samples.iter_mut().enumerate() {
            *s = Sample::new((i % 60 + 1) as u8, 60 + (i % 7) as i32, (i % 3) as i32);
        }
        tile.north.fill(Sample::new(5, 64, 0));
        tile.shade(Rect::FULL, key(0, -1, 2).origin());
        let png = png::encode(&tile.pixels, SIDE);
        let bytes = encode_tile(
            "minecraft:overworld",
            key(0, -1, 2),
            Some((&tile.samples, &tile.north)),
            Some(&png),
        );
        (bytes, tile)
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_base_tile_round_trips_with_its_png() {
        let (bytes, tile) = base_file();
        match decode_tile(&bytes, "minecraft:overworld", key(0, -1, 2)) {
            Ok(Persisted::Base { tile: back, png }) => {
                assert_eq!(back.samples, tile.samples);
                assert_eq!(back.north, tile.north);
                assert_eq!(back.pixels, tile.pixels);
                assert!(png.is_some());
            }
            _ => panic!("the tile reads back"),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_zoomed_tile_round_trips_through_its_png() {
        let pixels: Vec<u8> = (0..PIXELS).map(|i| (i % 200) as u8 + 4).collect();
        let png = png::encode(&pixels, SIDE);
        let bytes = encode_tile("minecraft:the_nether", key(3, 1, 1), None, Some(&png));
        match decode_tile(&bytes, "minecraft:the_nether", key(3, 1, 1)) {
            Ok(Persisted::Zoomed { pixels: back, .. }) => assert_eq!(back[..], pixels[..]),
            _ => panic!("the tile reads back"),
        }
        assert_eq!(
            decode_tile(&bytes, "minecraft:overworld", key(3, 1, 1)).err(),
            Some(Corrupt::Key)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn damaged_or_foreign_tile_files_are_refused() {
        let (bytes, _) = base_file();
        let dim = "minecraft:overworld";
        assert_eq!(
            decode_tile(&bytes, dim, key(0, -1, 3)).err(),
            Some(Corrupt::Key)
        );
        assert_eq!(
            decode_tile(&bytes, dim, key(1, -1, 2)).err(),
            Some(Corrupt::Key)
        );
        for cut in [0, 3, 4, 20, bytes.len() / 2, bytes.len() - 1] {
            assert!(
                decode_tile(&bytes[..cut], dim, key(0, -1, 2)).is_err(),
                "cut at {cut}"
            );
        }
        let mut flipped = bytes.clone();
        flipped[bytes.len() / 3] ^= 1;
        assert_eq!(
            decode_tile(&flipped, dim, key(0, -1, 2)).err(),
            Some(Corrupt::Checksum)
        );
        assert_eq!(
            decode_tile(&vec![0; MAX_TILE_BYTES + 1], dim, key(0, -1, 2)).err(),
            Some(Corrupt::Size)
        );
    }

    fn sealed(mut body: Vec<u8>) -> Vec<u8> {
        let crc = crc(&body);
        body.extend_from_slice(&crc.to_le_bytes());
        body
    }

    #[tuff_core::watchdog]
    #[test]
    fn samples_that_inflate_past_their_size_are_refused() {
        let dim = "minecraft:overworld";
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::best());
        zlib.write_all(&vec![7u8; 256 << 20]).unwrap();
        let bomb = zlib.finish().unwrap();
        assert!(bomb.len() < MAX_TILE_BYTES);
        let mut body = Vec::new();
        body.extend_from_slice(&TILE_MAGIC);
        body.extend_from_slice(&[FORMAT, 0, HAS_SAMPLES, 0]);
        body.extend_from_slice(&0i32.to_le_bytes());
        body.extend_from_slice(&0i32.to_le_bytes());
        body.extend_from_slice(&(dim.len() as u16).to_le_bytes());
        body.extend_from_slice(dim.as_bytes());
        body.extend_from_slice(&(bomb.len() as u32).to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        let short = {
            let mut zlib = ZlibEncoder::new(Vec::new(), Compression::default());
            zlib.write_all(&[1, 2, 3]).unwrap();
            zlib.finish().unwrap()
        };
        let mut truncated = body.clone();
        body.extend_from_slice(&bomb);
        assert_eq!(
            decode_tile(&sealed(body), dim, key(0, 0, 0)).err(),
            Some(Corrupt::Samples)
        );
        let at = truncated.len() - 8;
        truncated[at..at + 4].copy_from_slice(&(short.len() as u32).to_le_bytes());
        truncated.extend_from_slice(&short);
        assert_eq!(
            decode_tile(&sealed(truncated), dim, key(0, 0, 0)).err(),
            Some(Corrupt::Samples)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_index_round_trips_and_refuses_damage() {
        let mut index = DiskIndex {
            seq: 4,
            ..DiskIndex::default()
        };
        let overworld = index.dim("minecraft:overworld");
        let nether = index.dim("minecraft:the_nether");
        assert_eq!(index.dim("minecraft:overworld"), overworld);
        for i in 0..10 {
            let used = index.tick();
            let dim = if i % 2 == 0 { overworld } else { nether };
            index.entries.insert(
                DiskKey {
                    dim,
                    level: (i % 7) as u8,
                    x: i - 5,
                    z: 3 * i,
                },
                Meta {
                    size: 100 + i as u32,
                    used,
                },
            );
        }
        let bytes = index.encode();
        assert_eq!(bytes.len(), index.encoded_len());
        assert_eq!(DiskIndex::decode(&bytes), Some(index.clone()));
        for cut in [0, 4, 30, bytes.len() - 1] {
            assert_eq!(DiskIndex::decode(&bytes[..cut]), None);
        }
        let mut flipped = bytes.clone();
        flipped[40] ^= 4;
        assert_eq!(DiskIndex::decode(&flipped), None);
        let mut lying = index.clone();
        lying.entries.values_mut().next().unwrap().size = MAX_TILE_BYTES as u32 + 1;
        assert_eq!(
            DiskIndex::decode(&lying.encode()),
            None,
            "an oversized entry"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn dimension_directories_stay_inside_the_cache() {
        assert_eq!(dim_dir("minecraft:overworld"), "minecraft/overworld");
        assert_eq!(dim_dir("pack:worlds/sky"), "pack/worlds/sky");
        assert!(dim_dir("pack:../x").starts_with('x'));
        assert!(dim_dir("pack:a//b").starts_with('x'));
        assert!(dim_dir("Pack:A").starts_with('x'));
        assert_eq!(
            tile_path("minecraft:overworld", 2, -3, 4),
            "tuff/world-map/v1/minecraft/overworld/2/-3.4.tile"
        );
    }

    fn entry(index: &mut DiskIndex, x: i32, size: u32) -> DiskKey {
        let key = DiskKey {
            dim: 0,
            level: 0,
            x,
            z: 0,
        };
        let used = index.tick();
        index.entries.insert(key, Meta { size, used });
        key
    }

    #[tuff_core::watchdog]
    #[test]
    fn writes_evict_the_least_recently_used_entries() {
        let mut index = DiskIndex::default();
        index.dim("minecraft:overworld");
        let oldest = entry(&mut index, 0, 1000);
        let middle = entry(&mut index, 1, 1000);
        let newest = entry(&mut index, 2, 1000);
        let budget = index.total() + 500;
        let fresh = DiskKey {
            dim: 0,
            level: 0,
            x: 9,
            z: 0,
        };
        let used = index.tick();
        let writes = vec![Write {
            key: fresh,
            used,
            bytes: vec![0; 1200],
        }];
        let (evict, kept, skipped) = plan(&index, budget, writes);
        assert_eq!(evict, [oldest]);
        assert_eq!(kept.len(), 1);
        assert!(skipped.is_empty());
        let too_big = vec![Write {
            key: fresh,
            used,
            bytes: vec![0; budget as usize],
        }];
        let (evict, kept, skipped) = plan(&index, budget, too_big);
        assert!(evict.is_empty() && kept.is_empty());
        assert_eq!(skipped, [fresh]);
        let (evict, _, _) = plan(&index, 0, Vec::new());
        assert_eq!(evict, [oldest, middle, newest], "a zero budget empties it");
    }

    fn write_of(index: &mut DiskIndex, x: i32, size: usize) -> Write {
        let used = index.tick();
        Write {
            key: DiskKey {
                dim: 0,
                level: 0,
                x,
                z: 0,
            },
            used,
            bytes: vec![x as u8; size],
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn flushes_alternate_index_slots_and_a_torn_one_falls_back() {
        use tuff_storage::memory::MemoryBackend;
        let backend = MemoryBackend::default();
        let mut index = DiskIndex::default();
        index.dim("minecraft:overworld");
        let first = write_of(&mut index, 1, 100);
        let outcome = flush(
            &backend,
            index.clone(),
            1 << 20,
            Vec::new(),
            vec![first],
            false,
        );
        assert_eq!(outcome.index, Some(1));
        let loaded = load(&backend);
        assert_eq!(loaded.index.entries.len(), 1);
        assert!(loaded.orphans.is_empty());

        let mut index = loaded.index;
        let second = write_of(&mut index, 2, 100);
        let outcome = flush(&backend, index, 1 << 20, Vec::new(), vec![second], false);
        assert_eq!(outcome.index, Some(2));
        assert_eq!(load(&backend).index.entries.len(), 2);

        let newest = backend.read_extra_file(SLOTS[0]).unwrap();
        backend
            .write_extra_file(SLOTS[0], &newest[..newest.len() / 2])
            .unwrap();
        let loaded = load(&backend);
        assert_eq!(loaded.index.seq, 1, "the older slot is used");
        assert_eq!(loaded.index.entries.len(), 1);
        assert_eq!(loaded.orphans, [tile_path("minecraft:overworld", 0, 2, 0)]);

        backend.write_extra_file(SLOTS[1], b"junk").unwrap();
        let loaded = load(&backend);
        assert!(
            loaded.index.entries.is_empty(),
            "no good index is an empty one"
        );
        assert_eq!(loaded.orphans.len(), 2);
        let outcome = flush(
            &backend,
            loaded.index,
            1 << 20,
            loaded.orphans,
            Vec::new(),
            false,
        );
        assert_eq!(outcome.deleted.len(), 2);
        assert!(load(&backend).orphans.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_budget_deletes_least_recently_used_files_before_writing() {
        use tuff_storage::memory::MemoryBackend;
        let backend = MemoryBackend::default();
        let mut index = DiskIndex::default();
        index.dim("minecraft:overworld");
        let writes: Vec<Write> = (0..3).map(|x| write_of(&mut index, x, 1000)).collect();
        let outcome = flush(&backend, index.clone(), 1 << 20, Vec::new(), writes, false);
        assert_eq!(outcome.written.len(), 3);
        let mut index = load(&backend).index;
        let touched = index.tick();
        index
            .entries
            .get_mut(&DiskKey {
                dim: 0,
                level: 0,
                x: 0,
                z: 0,
            })
            .unwrap()
            .used = touched;
        let budget = index.total() + 500;
        let fresh = write_of(&mut index, 9, 1200);
        let outcome = flush(&backend, index, budget, Vec::new(), vec![fresh], false);
        assert_eq!(outcome.evicted.len(), 1);
        assert_eq!(outcome.evicted[0].x, 1, "the least recently used goes");
        let loaded = load(&backend);
        assert!(loaded.index.total() <= budget);
        let mut xs: Vec<i32> = loaded.index.entries.keys().map(|k| k.x).collect();
        xs.sort();
        assert_eq!(xs, [0, 2, 9]);
        assert!(
            backend
                .read_extra_file(&tile_path("minecraft:overworld", 0, 1, 0))
                .is_err()
        );
        assert!(loaded.orphans.is_empty());
    }
}
