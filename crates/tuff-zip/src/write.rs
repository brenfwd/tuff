use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::write::DeflateEncoder;
use flate2::{Compress, Compression, Crc, FlushCompress, Status};
use rayon::prelude::*;

use super::*;

/// How much of a streamed entry one compression job takes. Blocks are
/// deflated independently (each ends on a full flush), so a large file
/// compresses on every core at the cost of a little ratio.
pub const BLOCK: usize = 1 << 20;

/// An MS-DOS timestamp as zip headers store it (two-second resolution,
/// 1980 to 2107). Times are written in UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DosTime {
    pub date: u16,
    pub time: u16,
}

impl DosTime {
    /// Midnight on 1980-01-01, the earliest time a zip can hold.
    pub const MIN: DosTime = DosTime {
        date: (1 << 5) | 1,
        time: 0,
    };

    pub fn from_unix(secs: u64) -> DosTime {
        let days = (secs / 86_400) as i64;
        let (year, month, day) = civil_from_days(days);
        if year < 1980 {
            return Self::MIN;
        }
        if year > 2107 {
            return DosTime {
                date: (127 << 9) | (12 << 5) | 31,
                time: (23 << 11) | (59 << 5) | 29,
            };
        }
        let in_day = secs % 86_400;
        let (hour, minute, second) = (in_day / 3600, in_day / 60 % 60, in_day % 60);
        DosTime {
            date: (((year - 1980) as u16) << 9) | ((month as u16) << 5) | day as u16,
            time: ((hour as u16) << 11) | ((minute as u16) << 5) | (second as u16 / 2),
        }
    }

    pub fn from_system(time: SystemTime) -> DosTime {
        time.duration_since(UNIX_EPOCH)
            .map_or(Self::MIN, |since| Self::from_unix(since.as_secs()))
    }
}

/// An entry compressed in memory, ready for [`Writer::add`].
pub struct Prepared<'a> {
    method: u16,
    crc: u32,
    size: u64,
    body: Cow<'a, [u8]>,
}

/// Compresses `data` at `level` (0 to 9), storing it instead when deflate
/// does not make it smaller (or `level` is 0).
pub fn prepare(data: &[u8], level: u32) -> io::Result<Prepared<'_>> {
    let mut crc = Crc::new();
    crc.update(data);
    let stored = Prepared {
        method: STORED,
        crc: crc.sum(),
        size: data.len() as u64,
        body: Cow::Borrowed(data),
    };
    if level == 0 {
        return Ok(stored);
    }
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::new(level.min(9)));
    encoder.write_all(data)?;
    let deflated = encoder.finish()?;
    if deflated.len() >= data.len() {
        return Ok(stored);
    }
    Ok(Prepared {
        method: DEFLATED,
        body: Cow::Owned(deflated),
        ..stored
    })
}

/// An archive of in-memory `entries`, in order, all stamped `time`. The
/// entries are compressed in parallel; the bytes depend only on the input.
/// Fails once the archive would pass `max_bytes`.
pub fn write_all(
    entries: &[(&str, &[u8])],
    time: DosTime,
    level: u32,
    max_bytes: u64,
) -> io::Result<Vec<u8>> {
    let prepared: Vec<io::Result<Prepared<'_>>> = entries
        .par_iter()
        .map(|(_, data)| prepare(data, level))
        .collect();
    let mut writer = Writer::new(Vec::new());
    for ((name, _), entry) in entries.iter().zip(prepared) {
        writer.add(name, time, &entry?)?;
        if writer.offset() > max_bytes {
            return Err(too_large());
        }
    }
    let out = writer.finish()?;
    if out.len() as u64 > max_bytes {
        return Err(too_large());
    }
    Ok(out)
}

fn too_large() -> io::Error {
    io::Error::other("zip archive is too large")
}

struct Central {
    name: Box<str>,
    flags: u16,
    method: u16,
    time: DosTime,
    crc: u32,
    compressed: u64,
    size: u64,
    offset: u64,
    external: u32,
    zip64: bool,
}

/// Writes an archive front to back to any [`Write`]: entries whose size is
/// not known up front carry a data descriptor, so the output never needs
/// seeking. Zip64 records are added only where a size, offset or count
/// needs them.
pub struct Writer<W: Write> {
    out: W,
    offset: u64,
    central: Vec<Central>,
    open: Option<Central>,
}

fn flags_for(name: &str) -> u16 {
    if name.is_ascii() { 0 } else { FLAG_UTF8 }
}

fn name_len(name: &str) -> io::Result<u16> {
    u16::try_from(name.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "zip entry name is too long"))
}

trait Put {
    fn u16(&mut self, v: u16);
    fn u32(&mut self, v: u32);
    fn u64(&mut self, v: u64);
}

impl Put for Vec<u8> {
    fn u16(&mut self, v: u16) {
        self.extend_from_slice(&v.to_le_bytes());
    }

    fn u32(&mut self, v: u32) {
        self.extend_from_slice(&v.to_le_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.extend_from_slice(&v.to_le_bytes());
    }
}

fn clamp32(v: u64) -> u32 {
    v.min(U32_MAX) as u32
}

impl<W: Write> Writer<W> {
    pub fn new(out: W) -> Self {
        Self::starting_at(out, 0)
    }

    /// A writer whose offsets count from `offset`, for an archive appended
    /// to that many bytes of other data.
    pub fn starting_at(out: W, offset: u64) -> Self {
        Self {
            out,
            offset,
            central: Vec::new(),
            open: None,
        }
    }

    /// Bytes written so far, including the starting offset.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn entries(&self) -> usize {
        self.central.len()
    }

    fn emit(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.offset += bytes.len() as u64;
        Ok(())
    }

    fn idle(&self) -> io::Result<()> {
        match self.open {
            Some(_) => Err(io::Error::other("a streamed zip entry is still open")),
            None => Ok(()),
        }
    }

    /// Adds a whole entry compressed by [`prepare`].
    pub fn add(&mut self, name: &str, time: DosTime, entry: &Prepared<'_>) -> io::Result<()> {
        self.idle()?;
        let len = name_len(name)?;
        let compressed = entry.body.len() as u64;
        let zip64 = entry.size >= U32_MAX || compressed >= U32_MAX;
        let mut header = Vec::with_capacity(LOCAL_HEADER_LEN as usize + name.len() + 20);
        header.u32(LOCAL_HEADER);
        header.u16(if zip64 { VERSION_ZIP64 } else { VERSION });
        header.u16(flags_for(name));
        header.u16(entry.method);
        header.u16(time.time);
        header.u16(time.date);
        header.u32(entry.crc);
        header.u32(if zip64 { u32::MAX } else { compressed as u32 });
        header.u32(if zip64 { u32::MAX } else { entry.size as u32 });
        header.u16(len);
        header.u16(if zip64 { 20 } else { 0 });
        header.extend_from_slice(name.as_bytes());
        if zip64 {
            header.u16(ZIP64_EXTRA);
            header.u16(16);
            header.u64(entry.size);
            header.u64(compressed);
        }
        let offset = self.offset;
        self.emit(&header)?;
        self.emit(&entry.body)?;
        self.central.push(Central {
            name: name.into(),
            flags: flags_for(name),
            method: entry.method,
            time,
            crc: entry.crc,
            compressed,
            size: entry.size,
            offset,
            external: 0,
            zip64,
        });
        Ok(())
    }

    /// Adds a directory entry; `name` ends in `/`.
    pub fn add_directory(&mut self, name: &str, time: DosTime) -> io::Result<()> {
        if !name.ends_with('/') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a zip directory name ends in /",
            ));
        }
        self.add(name, time, &prepare(&[], 0)?)?;
        if let Some(entry) = self.central.last_mut() {
            entry.external = 0x10;
        }
        Ok(())
    }

    /// Starts an entry of unknown size: deflated data follows through
    /// [`Self::write_data`] and [`Self::end`] writes its descriptor.
    pub(crate) fn begin(&mut self, name: &str, time: DosTime) -> io::Result<()> {
        self.idle()?;
        let len = name_len(name)?;
        let flags = flags_for(name) | FLAG_DESCRIPTOR;
        let mut header = Vec::with_capacity(LOCAL_HEADER_LEN as usize + name.len() + 20);
        header.u32(LOCAL_HEADER);
        header.u16(VERSION_ZIP64);
        header.u16(flags);
        header.u16(DEFLATED);
        header.u16(time.time);
        header.u16(time.date);
        header.u32(0);
        header.u32(u32::MAX);
        header.u32(u32::MAX);
        header.u16(len);
        header.u16(20);
        header.extend_from_slice(name.as_bytes());
        header.u16(ZIP64_EXTRA);
        header.u16(16);
        header.u64(0);
        header.u64(0);
        let offset = self.offset;
        self.emit(&header)?;
        self.open = Some(Central {
            name: name.into(),
            flags,
            method: DEFLATED,
            time,
            crc: 0,
            compressed: 0,
            size: 0,
            offset,
            external: 0,
            zip64: true,
        });
        Ok(())
    }

    pub(crate) fn write_data(&mut self, data: &[u8]) -> io::Result<()> {
        let Some(open) = self.open.as_mut() else {
            return Err(io::Error::other("no streamed zip entry is open"));
        };
        open.compressed += data.len() as u64;
        self.emit(data)
    }

    pub(crate) fn end(&mut self, crc: u32, size: u64) -> io::Result<()> {
        let Some(mut open) = self.open.take() else {
            return Err(io::Error::other("no streamed zip entry is open"));
        };
        open.crc = crc;
        open.size = size;
        let mut descriptor = Vec::with_capacity(24);
        descriptor.u32(DESCRIPTOR);
        descriptor.u32(crc);
        descriptor.u64(open.compressed);
        descriptor.u64(size);
        self.emit(&descriptor)?;
        self.central.push(open);
        Ok(())
    }

    /// Writes the central directory and end records and returns the output.
    pub fn finish(mut self) -> io::Result<W> {
        self.idle()?;
        let directory_offset = self.offset;
        let entries = std::mem::take(&mut self.central);
        let mut buf = Vec::with_capacity(64 * 1024);
        for entry in &entries {
            let mut extra = Vec::new();
            if entry.size >= U32_MAX {
                extra.u64(entry.size);
            }
            if entry.compressed >= U32_MAX {
                extra.u64(entry.compressed);
            }
            if entry.offset >= U32_MAX {
                extra.u64(entry.offset);
            }
            let version = if entry.zip64 || !extra.is_empty() {
                VERSION_ZIP64
            } else {
                VERSION
            };
            buf.u32(CENTRAL_HEADER);
            buf.u16(version);
            buf.u16(version);
            buf.u16(entry.flags);
            buf.u16(entry.method);
            buf.u16(entry.time.time);
            buf.u16(entry.time.date);
            buf.u32(entry.crc);
            buf.u32(clamp32(entry.compressed));
            buf.u32(clamp32(entry.size));
            buf.u16(entry.name.len() as u16);
            buf.u16(if extra.is_empty() {
                0
            } else {
                extra.len() as u16 + 4
            });
            buf.u16(0);
            buf.u16(0);
            buf.u16(0);
            buf.u32(entry.external);
            buf.u32(clamp32(entry.offset));
            buf.extend_from_slice(entry.name.as_bytes());
            if !extra.is_empty() {
                buf.u16(ZIP64_EXTRA);
                buf.u16(extra.len() as u16);
                buf.extend_from_slice(&extra);
            }
            if buf.len() >= 60 * 1024 {
                self.emit(&buf)?;
                buf.clear();
            }
        }
        self.emit(&buf)?;
        buf.clear();
        let count = entries.len() as u64;
        let directory_len = self.offset - directory_offset;
        if count >= U16_MAX || directory_len >= U32_MAX || directory_offset >= U32_MAX {
            let record_offset = self.offset;
            buf.u32(ZIP64_END_RECORD);
            buf.u64(ZIP64_END_RECORD_LEN - 12);
            buf.u16(VERSION_ZIP64);
            buf.u16(VERSION_ZIP64);
            buf.u32(0);
            buf.u32(0);
            buf.u64(count);
            buf.u64(count);
            buf.u64(directory_len);
            buf.u64(directory_offset);
            buf.u32(ZIP64_LOCATOR);
            buf.u32(0);
            buf.u64(record_offset);
            buf.u32(1);
        }
        let count16 = count.min(U16_MAX) as u16;
        buf.u32(END_RECORD);
        buf.u16(0);
        buf.u16(0);
        buf.u16(count16);
        buf.u16(count16);
        buf.u32(clamp32(directory_len));
        buf.u32(clamp32(directory_offset));
        buf.u16(0);
        self.emit(&buf)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

struct Compressed {
    crc: Crc,
    size: u64,
    deflated: Vec<u8>,
    raw: Option<Vec<u8>>,
}

struct Pending {
    start: Option<(String, DosTime)>,
    last: bool,
    done: mpsc::Receiver<io::Result<Compressed>>,
}

/// Streams entries into a [`Writer`], deflating each [`BLOCK`] of input on
/// rayon while earlier blocks are written. Memory stays bounded by a small
/// window of blocks in flight, whatever the entry sizes. An entry that fits
/// one block is stored when deflate does not shrink it; larger ones are
/// always deflated (incompressible data costs a few bytes per 64 KiB).
/// Blocks on rayon results, so drive it from a thread outside the pool.
pub struct Pipeline<W: Write> {
    writer: Writer<W>,
    level: u32,
    window: VecDeque<Pending>,
    in_flight: usize,
    open: Option<(Crc, u64)>,
}

fn read_block(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut block = Vec::with_capacity(BLOCK);
    reader.take(BLOCK as u64).read_to_end(&mut block)?;
    Ok(block)
}

fn deflate_block(raw: &[u8], level: u32, finish: bool) -> io::Result<Vec<u8>> {
    let mut compress = Compress::new(Compression::new(level.min(9)), false);
    let mut out = Vec::with_capacity(raw.len() + raw.len() / 64 + 64);
    let flush = if finish {
        FlushCompress::Finish
    } else {
        FlushCompress::Full
    };
    loop {
        let (read, wrote) = (compress.total_in(), out.len());
        let status = compress
            .compress_vec(&raw[read as usize..], &mut out, flush)
            .map_err(io::Error::other)?;
        let consumed = compress.total_in() as usize == raw.len();
        let room = out.len() < out.capacity();
        match status {
            Status::StreamEnd => return Ok(out),
            _ if !finish && consumed && room => return Ok(out),
            _ if !room => out.reserve(64 * 1024),
            _ if compress.total_in() == read && out.len() == wrote => {
                return Err(io::Error::other("deflate made no progress"));
            }
            _ => {}
        }
    }
}

fn compress(raw: Vec<u8>, level: u32, last: bool, single: bool) -> io::Result<Compressed> {
    let mut crc = Crc::new();
    crc.update(&raw);
    let size = raw.len() as u64;
    let deflated = if single && level == 0 {
        Vec::new()
    } else {
        deflate_block(&raw, level, last)?
    };
    Ok(Compressed {
        crc,
        size,
        deflated,
        raw: single.then_some(raw),
    })
}

impl<W: Write> Pipeline<W> {
    /// Compresses at `level` (0 to 9).
    pub fn new(writer: Writer<W>, level: u32) -> Self {
        Self {
            writer,
            level: level.min(9),
            window: VecDeque::new(),
            in_flight: (rayon::current_num_threads() * 2).max(4),
            open: None,
        }
    }

    /// Adds an entry holding everything `reader` yields, returning its
    /// size. An error leaves the archive unusable.
    pub fn add_reader(
        &mut self,
        name: &str,
        time: DosTime,
        mut reader: impl Read,
    ) -> io::Result<u64> {
        let mut current = read_block(&mut reader)?;
        let mut first = true;
        let mut total = 0u64;
        loop {
            let next = if current.len() == BLOCK {
                read_block(&mut reader)?
            } else {
                Vec::new()
            };
            let last = next.is_empty();
            total += current.len() as u64;
            self.submit(first.then(|| (name.to_owned(), time)), last, current)?;
            if last {
                return Ok(total);
            }
            first = false;
            current = next;
        }
    }

    pub fn add_directory(&mut self, name: &str, time: DosTime) -> io::Result<()> {
        while !self.window.is_empty() {
            self.drain_one()?;
        }
        self.writer.add_directory(name, time)
    }

    fn submit(
        &mut self,
        start: Option<(String, DosTime)>,
        last: bool,
        raw: Vec<u8>,
    ) -> io::Result<()> {
        while self.window.len() >= self.in_flight {
            self.drain_one()?;
        }
        let (tx, done) = mpsc::sync_channel(1);
        let (level, single) = (self.level, start.is_some() && last);
        rayon::spawn(move || {
            let _ = tx.send(compress(raw, level, last, single));
        });
        self.window.push_back(Pending { start, last, done });
        Ok(())
    }

    fn drain_one(&mut self) -> io::Result<()> {
        let Some(pending) = self.window.pop_front() else {
            return Ok(());
        };
        let block = pending
            .done
            .recv()
            .map_err(|_| io::Error::other("a zip compression job failed"))??;
        match (pending.start, pending.last) {
            (Some((name, time)), true) => {
                let raw = block.raw.unwrap_or_default();
                let stored = self.level == 0 || block.deflated.len() >= raw.len();
                let entry = Prepared {
                    method: if stored { STORED } else { DEFLATED },
                    crc: block.crc.sum(),
                    size: block.size,
                    body: Cow::Owned(if stored { raw } else { block.deflated }),
                };
                self.writer.add(&name, time, &entry)
            }
            (Some((name, time)), false) => {
                self.writer.begin(&name, time)?;
                self.writer.write_data(&block.deflated)?;
                self.open = Some((block.crc, block.size));
                Ok(())
            }
            (None, last) => {
                let Some((crc, size)) = self.open.as_mut() else {
                    return Err(io::Error::other("a zip block has no entry"));
                };
                crc.combine(&block.crc);
                *size += block.size;
                self.writer.write_data(&block.deflated)?;
                if last && let Some((crc, size)) = self.open.take() {
                    self.writer.end(crc.sum(), size)?;
                }
                Ok(())
            }
        }
    }

    /// Bytes written so far.
    pub fn offset(&self) -> u64 {
        self.writer.offset()
    }

    pub fn finish(mut self) -> io::Result<W> {
        while !self.window.is_empty() {
            self.drain_one()?;
        }
        self.writer.finish()
    }
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
