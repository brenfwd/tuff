//! Anvil region files (`r.<x>.<z>.mca`): a 1024-entry sector table, 1024
//! timestamps, then 4 KiB sectors each holding one length-prefixed, compressed
//! chunk. Chunks too large for 255 sectors live in `c.<x>.<z>.mcc` files next
//! to the region. Mirrors vanilla's `RegionFile`, including writing new
//! sectors before the header so a crash never leaves the header pointing at
//! half-written data.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::lz4;
use tuff_nbt::Compression;
use tuff_world_model::pos::ChunkPos;

pub const SECTOR_BYTES: usize = 4096;
const ENTRIES: usize = 1024;
const HEADER_SECTORS: usize = 2;
const CHUNK_HEADER_BYTES: usize = 5;
const MAX_INLINE_SECTORS: usize = 255;
const EXTERNAL_FLAG: u8 = 0x80;
/// Largest external `.mcc` file read back. Vanilla has no limit; this keeps
/// a hostile file from exhausting memory.
const MAX_EXTERNAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum RegionError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("corrupt chunk: {0}")]
    Corrupt(&'static str),
    #[error("unsupported chunk compression {0}")]
    UnsupportedCompression(u8),
    #[error("decompressed chunk exceeds {0} bytes")]
    TooLarge(usize),
}

/// Per-chunk compression, identified by the byte after the length prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkCompression {
    Gzip = 1,
    Zlib = 2,
    None = 3,
    Lz4 = 4,
}

impl ChunkCompression {
    fn from_id(id: u8) -> Option<Self> {
        Some(match id {
            1 => Self::Gzip,
            2 => Self::Zlib,
            3 => Self::None,
            4 => Self::Lz4,
            _ => return None,
        })
    }
}

/// A chunk's stored bytes, still compressed. Decompression is left to the
/// caller so it can happen off the I/O thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawChunk {
    pub compression: ChunkCompression,
    pub data: Vec<u8>,
}

impl RawChunk {
    /// Compresses NBT bytes the way vanilla does by default (zlib, which
    /// vanilla calls "deflate").
    pub fn compress(nbt: Vec<u8>) -> io::Result<Self> {
        let data = Compression::Zlib
            .compress(nbt)
            .map_err(|err| io::Error::other(err.to_string()))?;
        Ok(Self {
            compression: ChunkCompression::Zlib,
            data,
        })
    }

    /// The backend's opaque blob form: the compression id byte followed by
    /// the stored bytes, as they sit in a region sector after its length.
    pub fn into_blob(self) -> Vec<u8> {
        let mut blob = Vec::with_capacity(self.data.len() + 1);
        blob.push(self.compression as u8);
        blob.extend_from_slice(&self.data);
        blob
    }

    pub fn from_blob(blob: &[u8]) -> Option<Self> {
        let (&id, data) = blob.split_first()?;
        Some(Self {
            compression: ChunkCompression::from_id(id)?,
            data: data.to_vec(),
        })
    }

    /// Fails instead of producing more than `max_len` bytes.
    pub fn decompress(&self, max_len: usize) -> Result<Vec<u8>, RegionError> {
        let nbt = match self.compression {
            ChunkCompression::Gzip => Compression::Gzip.decompress(&self.data, max_len),
            ChunkCompression::Zlib => Compression::Zlib.decompress(&self.data, max_len),
            ChunkCompression::None => {
                if self.data.len() > max_len {
                    return Err(RegionError::TooLarge(max_len));
                }
                return Ok(self.data.clone());
            }
            ChunkCompression::Lz4 => {
                return lz4::decompress_stream(&self.data, max_len).map_err(|err| match err {
                    lz4::Lz4Error::TooLarge(n) => RegionError::TooLarge(n),
                    _ => RegionError::Corrupt("bad lz4 stream"),
                });
            }
        };
        match nbt {
            Ok(bytes) => Ok(bytes.into_owned()),
            Err(tuff_nbt::NbtError::SizeLimit) => Err(RegionError::TooLarge(max_len)),
            Err(_) => Err(RegionError::Corrupt("bad compressed stream")),
        }
    }
}

pub fn region_file_name(x: i32, z: i32) -> String {
    format!("r.{x}.{z}.mca")
}

/// Parses a region file's `x` and `z` back out of its name (`r.<x>.<z>.mca`).
pub fn parse_region_file_name(name: &str) -> Option<(i32, i32)> {
    let rest = name.strip_prefix("r.")?.strip_suffix(".mca")?;
    let (x, z) = rest.split_once('.')?;
    Some((x.parse().ok()?, z.parse().ok()?))
}

fn index(pos: ChunkPos) -> usize {
    (pos.x & 31) as usize + (pos.z & 31) as usize * 32
}

fn now_seconds() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as u32)
}

pub struct RegionFile {
    dir: PathBuf,
    file: File,
    offsets: Box<[u32; ENTRIES]>,
    timestamps: Box<[u32; ENTRIES]>,
    used: Vec<bool>,
}

impl RegionFile {
    /// Opens `r.<x>.<z>.mca` in `dir`. A missing file yields `None` unless
    /// `create` is set, in which case the directory and file are created.
    pub fn open(dir: &Path, x: i32, z: i32, create: bool) -> io::Result<Option<Self>> {
        let path = dir.join(region_file_name(x, z));
        if create {
            fs::create_dir_all(dir)?;
        }
        let file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound && !create => return Ok(None),
            Err(err) => return Err(err),
        };
        Self::from_file(dir.to_path_buf(), file).map(Some)
    }

    fn from_file(dir: PathBuf, mut file: File) -> io::Result<Self> {
        let size = file.metadata()?.len();
        let mut header = vec![0u8; SECTOR_BYTES * HEADER_SECTORS];
        let mut read = 0;
        while read < header.len() {
            match file.read(&mut header[read..])? {
                0 => break,
                n => read += n,
            }
        }
        let mut region = Self {
            dir,
            file,
            offsets: Box::new([0; ENTRIES]),
            timestamps: Box::new([0; ENTRIES]),
            used: vec![true; HEADER_SECTORS],
        };
        let word = |i: usize| u32::from_be_bytes(header[i * 4..i * 4 + 4].try_into().unwrap());
        for i in 0..ENTRIES {
            region.timestamps[i] = word(ENTRIES + i);
            let offset = word(i);
            let (sector, count) = ((offset >> 8) as usize, (offset & 0xFF) as usize);
            let valid = offset != 0
                && sector >= HEADER_SECTORS
                && count > 0
                && ((sector + count) * SECTOR_BYTES) as u64
                    <= size.next_multiple_of(SECTOR_BYTES as u64);
            if valid {
                region.offsets[i] = offset;
                region.mark(sector, count, true);
            }
        }
        Ok(region)
    }

    fn mark(&mut self, sector: usize, count: usize, used: bool) {
        if self.used.len() < sector + count {
            self.used.resize(sector + count, false);
        }
        self.used[sector..sector + count].fill(used);
    }

    /// First-fit, like vanilla's `RegionBitmap`.
    fn allocate(&mut self, count: usize) -> usize {
        let mut run = 0;
        for (i, used) in self.used.iter().enumerate() {
            if *used {
                run = 0;
            } else {
                run += 1;
                if run == count {
                    let start = i + 1 - count;
                    self.mark(start, count, true);
                    return start;
                }
            }
        }
        let start = self.used.len() - run;
        self.mark(start, count, true);
        start
    }

    pub fn contains(&self, pos: ChunkPos) -> bool {
        self.offsets[index(pos)] != 0
    }

    /// Every chunk position this file has an entry for, given the region's
    /// own coordinates (a region file only stores the low 5 bits of each
    /// axis internally).
    pub fn positions(&self, region_x: i32, region_z: i32) -> Vec<ChunkPos> {
        self.offsets
            .iter()
            .enumerate()
            .filter(|&(_, &offset)| offset != 0)
            .map(|(i, _)| {
                let (lx, lz) = ((i % 32) as i32, (i / 32) as i32);
                ChunkPos::new(region_x * 32 + lx, region_z * 32 + lz)
            })
            .collect()
    }

    fn external_path(&self, pos: ChunkPos) -> PathBuf {
        self.dir.join(format!("c.{}.{}.mcc", pos.x, pos.z))
    }

    /// Reads a chunk's compressed bytes. Damaged entries yield an error rather
    /// than a panic; the caller decides whether to regenerate.
    pub fn read(&mut self, pos: ChunkPos) -> Result<Option<RawChunk>, RegionError> {
        let offset = self.offsets[index(pos)];
        if offset == 0 {
            return Ok(None);
        }
        let (sector, count) = ((offset >> 8) as usize, (offset & 0xFF) as usize);
        let mut buf = vec![0u8; count * SECTOR_BYTES];
        self.file
            .seek(SeekFrom::Start((sector * SECTOR_BYTES) as u64))?;
        let mut filled = 0;
        while filled < buf.len() {
            match self.file.read(&mut buf[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        if filled < CHUNK_HEADER_BYTES {
            return Err(RegionError::Corrupt("truncated chunk header"));
        }
        let length = u32::from_be_bytes(buf[..4].try_into().unwrap()) as usize;
        let version = buf[4];
        if length == 0 {
            return Err(RegionError::Corrupt("allocated chunk has no stream"));
        }
        let compression = ChunkCompression::from_id(version & !EXTERNAL_FLAG).ok_or(
            RegionError::UnsupportedCompression(version & !EXTERNAL_FLAG),
        )?;
        if version & EXTERNAL_FLAG != 0 {
            let file = File::open(self.external_path(pos))?;
            let mut data = Vec::new();
            file.take(MAX_EXTERNAL_BYTES + 1).read_to_end(&mut data)?;
            if data.len() as u64 > MAX_EXTERNAL_BYTES {
                return Err(RegionError::TooLarge(MAX_EXTERNAL_BYTES as usize));
            }
            return Ok(Some(RawChunk { compression, data }));
        }
        let end = CHUNK_HEADER_BYTES - 1 + length;
        if end > filled {
            return Err(RegionError::Corrupt("truncated chunk stream"));
        }
        buf.truncate(end);
        buf.drain(..CHUNK_HEADER_BYTES);
        Ok(Some(RawChunk {
            compression,
            data: buf,
        }))
    }

    pub fn write(&mut self, pos: ChunkPos, chunk: &RawChunk) -> io::Result<()> {
        let i = index(pos);
        let old = self.offsets[i];
        let needed = (CHUNK_HEADER_BYTES + chunk.data.len()).div_ceil(SECTOR_BYTES);
        let external = needed > MAX_INLINE_SECTORS;
        let count = if external { 1 } else { needed };
        let mut body = Vec::with_capacity(count * SECTOR_BYTES);
        if external {
            let path = self.external_path(pos);
            let temp = path.with_extension("mcc.tmp");
            fs::write(&temp, &chunk.data)?;
            fs::rename(&temp, &path)?;
            body.extend_from_slice(&1u32.to_be_bytes());
            body.push(chunk.compression as u8 | EXTERNAL_FLAG);
        } else {
            body.extend_from_slice(&(chunk.data.len() as u32 + 1).to_be_bytes());
            body.push(chunk.compression as u8);
            body.extend_from_slice(&chunk.data);
        }
        body.resize(count * SECTOR_BYTES, 0);
        let sector = self.allocate(count);
        self.file
            .seek(SeekFrom::Start((sector * SECTOR_BYTES) as u64))?;
        self.file.write_all(&body)?;
        self.offsets[i] = ((sector as u32) << 8) | count as u32;
        self.timestamps[i] = now_seconds();
        self.write_header()?;
        if !external {
            match fs::remove_file(self.external_path(pos)) {
                Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
                _ => {}
            }
        }
        if old != 0 {
            self.mark((old >> 8) as usize, (old & 0xFF) as usize, false);
        }
        Ok(())
    }

    pub fn clear(&mut self, pos: ChunkPos) -> io::Result<()> {
        let i = index(pos);
        let old = self.offsets[i];
        if old == 0 {
            return Ok(());
        }
        self.offsets[i] = 0;
        self.timestamps[i] = now_seconds();
        self.write_header()?;
        match fs::remove_file(self.external_path(pos)) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        self.mark((old >> 8) as usize, (old & 0xFF) as usize, false);
        Ok(())
    }

    fn write_header(&mut self) -> io::Result<()> {
        let mut header = Vec::with_capacity(SECTOR_BYTES * HEADER_SECTORS);
        for word in self.offsets.iter().chain(self.timestamps.iter()) {
            header.extend_from_slice(&word.to_be_bytes());
        }
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header)
    }

    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;
    use tuff_nbt::{NamedTag, NbtLimits};

    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/vanilla-26.3"
    );
    const LIMIT: usize = 16 * 1024 * 1024;

    fn fixture_region(name: &str) -> (TestDir, RegionFile) {
        let dir = TestDir::new();
        fs::copy(Path::new(FIXTURES).join(name), dir.path().join("r.0.0.mca")).unwrap();
        let region = RegionFile::open(dir.path(), 0, 0, false).unwrap().unwrap();
        (dir, region)
    }

    fn read_nbt(region: &mut RegionFile, pos: ChunkPos) -> NamedTag {
        let raw = region.read(pos).unwrap().unwrap();
        let bytes = raw.decompress(LIMIT).unwrap();
        NamedTag::read(&mut &bytes[..], NbtLimits::TRUSTED).unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn reads_vanilla_deflate_and_lz4_chunks() {
        for name in ["r.0.0.deflate.mca", "r.0.0.lz4.mca"] {
            let (_dir, mut region) = fixture_region(name);
            let chunk = read_nbt(&mut region, ChunkPos::new(10, 10));
            let root = chunk.tag.as_compound().unwrap();
            assert_eq!(root.get_int("xPos"), Some(10), "{name}");
            assert_eq!(root.get_str("Status"), Some("minecraft:full"), "{name}");
            assert!(region.read(ChunkPos::new(5, 5)).unwrap().is_none());
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn missing_file_is_none_unless_created() {
        let dir = TestDir::new();
        assert!(
            RegionFile::open(dir.path(), 3, -2, false)
                .unwrap()
                .is_none()
        );
        let region = RegionFile::open(&dir.path().join("sub"), 3, -2, true).unwrap();
        assert!(region.is_some());
        assert!(dir.path().join("sub/r.3.-2.mca").exists());
    }

    #[tuff_core::watchdog]
    #[test]
    fn write_read_round_trip_reuses_freed_sectors() {
        let dir = TestDir::new();
        let mut region = RegionFile::open(dir.path(), -1, 0, true).unwrap().unwrap();
        let pos = ChunkPos::new(-5, 7);
        let mut seed = 1u64;
        let noise = (0..80_000)
            .map(|_| {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (seed >> 56) as u8
            })
            .collect();
        let big = RawChunk::compress(noise).unwrap();
        assert!(big.data.len() > 10 * SECTOR_BYTES);
        region.write(pos, &big).unwrap();
        assert_eq!(region.read(pos).unwrap().unwrap(), big);
        let small = RawChunk::compress(vec![1, 2, 3]).unwrap();
        region.write(pos, &small).unwrap();
        region.write(ChunkPos::new(-6, 7), &small).unwrap();
        region.sync().unwrap();
        drop(region);
        let mut reopened = RegionFile::open(dir.path(), -1, 0, false).unwrap().unwrap();
        assert_eq!(reopened.read(pos).unwrap().unwrap(), small);
        assert_eq!(reopened.read(ChunkPos::new(-6, 7)).unwrap().unwrap(), small);
        assert_eq!(reopened.offsets[index(ChunkPos::new(-6, 7))] >> 8, 2);
        reopened.clear(pos).unwrap();
        assert!(reopened.read(pos).unwrap().is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn oversized_chunks_use_external_files() {
        let dir = TestDir::new();
        let mut region = RegionFile::open(dir.path(), 0, 0, true).unwrap().unwrap();
        let pos = ChunkPos::new(1, 2);
        let huge = RawChunk {
            compression: ChunkCompression::None,
            data: vec![7; MAX_INLINE_SECTORS * SECTOR_BYTES + 1],
        };
        region.write(pos, &huge).unwrap();
        assert!(dir.path().join("c.1.2.mcc").exists());
        assert_eq!(region.read(pos).unwrap().unwrap(), huge);
        region
            .write(pos, &RawChunk::compress(vec![1]).unwrap())
            .unwrap();
        assert!(!dir.path().join("c.1.2.mcc").exists());
    }

    #[tuff_core::watchdog]
    #[test]
    fn damaged_headers_and_streams_are_rejected() {
        let dir = TestDir::new();
        let mut header = vec![0u8; SECTOR_BYTES * 3];
        header[0..4].copy_from_slice(&((1u32 << 8) | 1).to_be_bytes());
        header[4..8].copy_from_slice(&((50u32 << 8) | 1).to_be_bytes());
        header[8..12].copy_from_slice(&((2u32 << 8) | 1).to_be_bytes());
        let sector = 2 * SECTOR_BYTES;
        header[sector..sector + 4].copy_from_slice(&5000u32.to_be_bytes());
        header[sector + 4] = 2;
        fs::write(dir.path().join("r.0.0.mca"), &header).unwrap();
        let mut region = RegionFile::open(dir.path(), 0, 0, false).unwrap().unwrap();
        assert!(!region.contains(ChunkPos::new(0, 0)), "overlaps header");
        assert!(!region.contains(ChunkPos::new(1, 0)), "past end of file");
        assert!(matches!(
            region.read(ChunkPos::new(2, 0)),
            Err(RegionError::Corrupt(_))
        ));
        header[sector..sector + 4].copy_from_slice(&3u32.to_be_bytes());
        header[sector + 4] = 99;
        fs::write(dir.path().join("r.0.0.mca"), &header).unwrap();
        let mut region = RegionFile::open(dir.path(), 0, 0, false).unwrap().unwrap();
        assert!(matches!(
            region.read(ChunkPos::new(2, 0)),
            Err(RegionError::UnsupportedCompression(99))
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn decompression_is_bounded() {
        let bomb = RawChunk::compress(vec![0; 1 << 20]).unwrap();
        assert!(bomb.data.len() < 8 * 1024);
        assert!(matches!(
            bomb.decompress(1 << 16),
            Err(RegionError::TooLarge(_))
        ));
        let garbage = RawChunk {
            compression: ChunkCompression::Zlib,
            data: vec![0x78, 0x9C, 1, 2, 3],
        };
        assert!(matches!(
            garbage.decompress(1 << 16),
            Err(RegionError::Corrupt(_))
        ));
        let truncated = RawChunk {
            data: bomb.data[..bomb.data.len() / 2].to_vec(),
            ..bomb
        };
        assert!(truncated.decompress(1 << 21).is_err());
    }
}
