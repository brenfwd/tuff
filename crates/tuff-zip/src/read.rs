use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use flate2::Crc;
use flate2::read::DeflateDecoder;

use super::*;

/// The longest entry name a strict archive may hold.
pub const MAX_NAME_LEN: usize = 1024;
const TAIL: u64 = END_RECORD_LEN + U16_MAX;

/// Random-access bytes an archive is read from.
pub trait Source {
    fn size(&self) -> io::Result<u64>;
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;
}

impl Source for [u8] {
    fn size(&self) -> io::Result<u64> {
        Ok(self.len() as u64)
    }

    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| io::ErrorKind::UnexpectedEof)?;
        let bytes = start
            .checked_add(buf.len())
            .and_then(|end| self.get(start..end))
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        buf.copy_from_slice(bytes);
        Ok(())
    }
}

impl<S: Source + ?Sized> Source for &S {
    fn size(&self) -> io::Result<u64> {
        (**self).size()
    }

    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        (**self).read_exact_at(buf, offset)
    }
}

impl Source for File {
    fn size(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    #[cfg(unix)]
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset)
    }

    #[cfg(windows)]
    fn read_exact_at(&self, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
        while !buf.is_empty() {
            match std::os::windows::fs::FileExt::seek_read(self, buf, offset) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    buf = &mut buf[n..];
                    offset += n as u64;
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

/// What an archive may hold before it is refused.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_entries: usize,
    /// Largest uncompressed entry.
    pub max_entry_bytes: u64,
    /// Largest sum of uncompressed sizes, declared and inflated alike.
    pub max_total_bytes: u64,
    /// Largest uncompressed-to-compressed ratio of one entry.
    pub max_ratio: Option<u64>,
    /// Refuses, when opening: names that are not plain relative UTF-8
    /// paths (absolute, `..`, backslashes, drive letters), duplicate and
    /// case-folded duplicate names, a file that is also a directory,
    /// symbolic links and special files, encryption, unknown methods and
    /// overlapping entries. Without it, the first of duplicate names wins
    /// and other problems surface only when an entry is read.
    pub strict: bool,
}

impl Limits {
    /// Archives from outside the server (uploaded backups): strict, at
    /// most a million entries and 1000:1 per entry.
    pub const fn untrusted(max_total_bytes: u64) -> Limits {
        Limits {
            max_entries: 1_000_000,
            max_entry_bytes: max_total_bytes,
            max_total_bytes,
            max_ratio: Some(1000),
            strict: true,
        }
    }

    /// Only an entry count; callers bound each read themselves.
    pub const fn lenient(max_entries: usize) -> Limits {
        Limits {
            max_entries,
            max_entry_bytes: u64::MAX,
            max_total_bytes: u64::MAX,
            max_ratio: None,
            strict: false,
        }
    }
}

/// One entry of the central directory.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// Declared uncompressed size, enforced while reading.
    pub size: u64,
    pub compressed: u64,
    pub modified: DosTime,
    method: u16,
    flags: u16,
    crc: u32,
    local: u64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }
}

/// A zip archive's directory over a [`Source`].
pub struct Archive<S> {
    source: S,
    entries: Vec<Entry>,
    index: HashMap<String, usize>,
    directory_offset: u64,
    limits: Limits,
    inflated: AtomicU64,
}

impl<S> std::fmt::Debug for Archive<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Archive")
            .field("entries", &self.entries.len())
            .finish()
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    bytes
        .get(at..at + 2)
        .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    bytes.get(at..at + 8).map_or(0, |b| {
        u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    })
}

/// Why `name` cannot be extracted safely, if it cannot: it must be a
/// relative path of `/`-separated components, none empty, `.` or `..`,
/// with no backslash, colon or control character.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(format!("{name:?} is empty or too long"));
    }
    if name.starts_with('/') {
        return Err(format!("{name:?} is an absolute path"));
    }
    if name
        .chars()
        .any(|c| c == '\\' || c == ':' || c.is_control())
    {
        return Err(format!(
            "{name:?} holds a backslash, colon or control character"
        ));
    }
    let body = name.strip_suffix('/').unwrap_or(name);
    if body
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("{name:?} has an empty, . or .. component"));
    }
    Ok(())
}

struct SourceReader<'a, S: ?Sized> {
    source: &'a S,
    pos: u64,
    end: u64,
}

impl<S: Source + ?Sized> Read for SourceReader<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = (buf.len() as u64).min(self.end.saturating_sub(self.pos)) as usize;
        if n == 0 {
            return Ok(0);
        }
        self.source.read_exact_at(&mut buf[..n], self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

struct End {
    entries: u64,
    directory_len: u64,
    directory_offset: u64,
    /// Where the end records start; the directory must end before it.
    records: u64,
}

fn find_end<S: Source + ?Sized>(source: &S, size: u64) -> io::Result<End> {
    if size < END_RECORD_LEN {
        return Err(invalid("not a zip archive"));
    }
    let tail_len = size.min(TAIL);
    let tail_start = size - tail_len;
    let mut tail = vec![0; tail_len as usize];
    source.read_exact_at(&mut tail, tail_start)?;
    let last = tail.len() - END_RECORD_LEN as usize;
    let at = (0..=last)
        .rev()
        .find(|&at| {
            u32_at(&tail, at) == END_RECORD
                && at + END_RECORD_LEN as usize + usize::from(u16_at(&tail, at + 20)) == tail.len()
        })
        .ok_or_else(|| invalid("not a zip archive"))?;
    let end = &tail[at..];
    let (disk, directory_disk) = (u16_at(end, 4), u16_at(end, 6));
    let (on_disk, total) = (u16_at(end, 8), u16_at(end, 10));
    if disk != 0 || directory_disk != 0 || on_disk != total {
        return Err(invalid("multi-disk zip archives are not supported"));
    }
    let record = tail_start + at as u64;
    let mut found = End {
        entries: u64::from(total),
        directory_len: u64::from(u32_at(end, 12)),
        directory_offset: u64::from(u32_at(end, 16)),
        records: record,
    };
    let mut locator = [0u8; ZIP64_LOCATOR_LEN as usize];
    let has_locator = record >= ZIP64_LOCATOR_LEN
        && source
            .read_exact_at(&mut locator, record - ZIP64_LOCATOR_LEN)
            .is_ok()
        && u32_at(&locator, 0) == ZIP64_LOCATOR;
    if !has_locator {
        if found.entries == U16_MAX
            || found.directory_len == U32_MAX
            || found.directory_offset == U32_MAX
        {
            return Err(invalid("the zip64 end record is missing"));
        }
        return Ok(found);
    }
    let zip64_at = u64_at(&locator, 8);
    if u32_at(&locator, 4) != 0 || u32_at(&locator, 16) > 1 {
        return Err(invalid("multi-disk zip archives are not supported"));
    }
    let locator_at = record - ZIP64_LOCATOR_LEN;
    if zip64_at
        .checked_add(ZIP64_END_RECORD_LEN)
        .is_none_or(|end| end > locator_at)
    {
        return Err(invalid("the zip64 end record lies outside the archive"));
    }
    let mut zip64 = [0u8; ZIP64_END_RECORD_LEN as usize];
    source.read_exact_at(&mut zip64, zip64_at)?;
    if u32_at(&zip64, 0) != ZIP64_END_RECORD {
        return Err(invalid("bad zip64 end record"));
    }
    let (on_disk, total) = (u64_at(&zip64, 24), u64_at(&zip64, 32));
    if u32_at(&zip64, 16) != 0 || u32_at(&zip64, 20) != 0 || on_disk != total {
        return Err(invalid("multi-disk zip archives are not supported"));
    }
    found = End {
        entries: total,
        directory_len: u64_at(&zip64, 40),
        directory_offset: u64_at(&zip64, 48),
        records: zip64_at,
    };
    Ok(found)
}

/// Fills in the fields a zip64 extra field carries (those saturated in
/// the fixed header), in the order the format lists them.
fn apply_zip64(
    extra: &[u8],
    size: &mut u64,
    compressed: &mut u64,
    local: &mut u64,
) -> Result<(), &'static str> {
    let mut at = 0;
    while at + 4 <= extra.len() {
        let (id, len) = (u16_at(extra, at), usize::from(u16_at(extra, at + 2)));
        let data = extra.get(at + 4..at + 4 + len).ok_or("bad extra field")?;
        at += 4 + len;
        if id != ZIP64_EXTRA {
            continue;
        }
        let mut cursor = 0;
        for field in [size, compressed, local] {
            if *field == U32_MAX {
                if cursor + 8 > data.len() {
                    return Err("short zip64 extra field");
                }
                *field = u64_at(data, cursor);
                cursor += 8;
            }
        }
        return Ok(());
    }
    if *size == U32_MAX || *compressed == U32_MAX || *local == U32_MAX {
        return Err("missing zip64 extra field");
    }
    Ok(())
}

const UNIX_TYPE: u32 = 0o170_000;
const UNIX_FILE: u32 = 0o100_000;
const UNIX_DIR: u32 = 0o040_000;

impl<S: Source> Archive<S> {
    /// Reads the central directory, refusing what `limits` rule out.
    pub fn open(source: S, limits: Limits) -> io::Result<Self> {
        let size = source.size()?;
        let end = find_end(&source, size)?;
        if end.entries > limits.max_entries as u64 {
            return Err(invalid(format!(
                "archive has {} entries, over the limit of {}",
                end.entries, limits.max_entries
            )));
        }
        let directory_end = end
            .directory_offset
            .checked_add(end.directory_len)
            .filter(|&e| e <= end.records)
            .ok_or_else(|| invalid("central directory lies outside the archive"))?;
        if end.entries.saturating_mul(CENTRAL_HEADER_LEN as u64) > end.directory_len {
            return Err(invalid("truncated central directory"));
        }
        let truncated = |err: io::Error| {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                invalid("truncated central directory")
            } else {
                err
            }
        };
        let mut reader = BufReader::with_capacity(
            64 * 1024,
            SourceReader {
                source: &source,
                pos: end.directory_offset,
                end: directory_end,
            },
        );
        let mut entries = Vec::with_capacity(end.entries as usize);
        let mut declared = 0u64;
        let mut header = [0u8; CENTRAL_HEADER_LEN];
        for _ in 0..end.entries {
            reader.read_exact(&mut header).map_err(truncated)?;
            if u32_at(&header, 0) != CENTRAL_HEADER {
                return Err(invalid("bad central directory entry"));
            }
            let name_len = usize::from(u16_at(&header, 28));
            let extra_len = usize::from(u16_at(&header, 30));
            let comment_len = u64::from(u16_at(&header, 32));
            let mut raw_name = vec![0; name_len];
            reader.read_exact(&mut raw_name).map_err(truncated)?;
            let mut extra = vec![0; extra_len];
            reader.read_exact(&mut extra).map_err(truncated)?;
            let skipped = io::copy(&mut (&mut reader).take(comment_len), &mut io::sink())?;
            if skipped != comment_len {
                return Err(invalid("truncated central directory"));
            }
            let mut entry = Entry {
                name: String::new(),
                size: u64::from(u32_at(&header, 24)),
                compressed: u64::from(u32_at(&header, 20)),
                modified: DosTime {
                    time: u16_at(&header, 12),
                    date: u16_at(&header, 14),
                },
                method: u16_at(&header, 10),
                flags: u16_at(&header, 8),
                crc: u32_at(&header, 16),
                local: u64::from(u32_at(&header, 42)),
            };
            apply_zip64(
                &extra,
                &mut entry.size,
                &mut entry.compressed,
                &mut entry.local,
            )
            .map_err(invalid)?;
            entry.name = if limits.strict {
                String::from_utf8(raw_name).map_err(|_| invalid("an entry name is not UTF-8"))?
            } else {
                String::from_utf8_lossy(&raw_name).into_owned()
            };
            if limits.strict {
                let external = u32_at(&header, 38) >> 16;
                check_strict(&entry, external, u16_at(&header, 34), &limits)?;
            }
            declared = declared.saturating_add(entry.size);
            if declared > limits.max_total_bytes {
                return Err(invalid(format!(
                    "archive declares more than its {} byte limit",
                    limits.max_total_bytes
                )));
            }
            entries.push(entry);
        }
        let mut index = HashMap::with_capacity(entries.len());
        for (i, entry) in entries.iter().enumerate() {
            index.entry(entry.name.clone()).or_insert(i);
        }
        if limits.strict {
            check_names(&entries)?;
            check_layout(&entries, end.directory_offset)?;
        }
        Ok(Self {
            source,
            entries,
            index,
            directory_offset: end.directory_offset,
            limits,
            inflated: AtomicU64::new(0),
        })
    }

    /// Every entry, directories included, in directory order.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The first entry called `name`.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.index.get(name).map(|&i| &self.entries[i])
    }

    /// A reader of `entry`'s contents that fails as soon as they pass its
    /// declared size or the archive's total limit, and at the end unless
    /// size and checksum match.
    pub fn reader(&self, entry: &Entry) -> io::Result<EntryReader<'_, S>> {
        let name = &entry.name;
        if entry.flags & FLAG_ENCRYPTED != 0 {
            return Err(invalid(format!("{name:?} is encrypted")));
        }
        if entry.method != STORED && entry.method != DEFLATED {
            return Err(invalid(format!(
                "{name:?} uses unsupported compression method {}",
                entry.method
            )));
        }
        if entry.size > self.limits.max_entry_bytes {
            return Err(invalid(format!(
                "{name:?} claims {} bytes, over the {} byte limit",
                entry.size, self.limits.max_entry_bytes
            )));
        }
        let mut header = [0u8; LOCAL_HEADER_LEN as usize];
        self.source
            .read_exact_at(&mut header, entry.local)
            .map_err(|_| invalid(format!("{name:?} lies outside the archive")))?;
        if u32_at(&header, 0) != LOCAL_HEADER {
            return Err(invalid(format!("{name:?} has a bad local header")));
        }
        let local_name = u64::from(u16_at(&header, 26));
        let local_extra = u64::from(u16_at(&header, 28));
        if self.limits.strict {
            let mut local = vec![0; local_name as usize];
            self.source
                .read_exact_at(&mut local, entry.local + LOCAL_HEADER_LEN)
                .map_err(|_| invalid(format!("{name:?} lies outside the archive")))?;
            if local != name.as_bytes() {
                return Err(invalid(format!("{name:?} has a different local name")));
            }
        }
        let start = entry.local + LOCAL_HEADER_LEN + local_name + local_extra;
        let end = start
            .checked_add(entry.compressed)
            .filter(|&end| end <= self.directory_offset)
            .ok_or_else(|| invalid(format!("{name:?} is truncated")))?;
        let data = SourceReader {
            source: &self.source,
            pos: start,
            end,
        };
        let inner = match entry.method {
            STORED if entry.compressed != entry.size => {
                return Err(invalid(format!("{name:?} has mismatched stored sizes")));
            }
            STORED => Inner::Stored(data),
            _ => Inner::Deflated(Box::new(DeflateDecoder::new(data))),
        };
        Ok(EntryReader {
            inner,
            name: name.clone(),
            expected: entry.size,
            crc_expected: entry.crc,
            produced: 0,
            crc: Crc::new(),
            total: &self.inflated,
            max_total: self.limits.max_total_bytes,
            done: false,
        })
    }

    /// The contents of `name`, refusing anything that claims or turns out
    /// to hold more than `max` bytes.
    pub fn read(&self, name: &str, max: usize) -> io::Result<Vec<u8>> {
        let entry = self
            .find(name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, name.to_owned()))?;
        if entry.size > max as u64 {
            return Err(invalid(format!(
                "{name:?} claims {} bytes, over the {max} byte limit",
                entry.size
            )));
        }
        let mut out = Vec::with_capacity((entry.size as usize).min(1 << 20));
        self.reader(entry)?.read_to_end(&mut out)?;
        Ok(out)
    }

    /// Unpacks a strict archive into the existing, empty directory `dir`,
    /// never overwriting a file. `progress` hears each chunk written and
    /// may stop the extraction by failing. Returns the bytes written.
    pub fn extract_to(
        &self,
        dir: &Path,
        mut progress: impl FnMut(u64) -> io::Result<()>,
    ) -> io::Result<u64> {
        if !self.limits.strict {
            return Err(io::Error::other("only strict archives can be extracted"));
        }
        let mut written = 0;
        let mut buf = vec![0; 64 * 1024];
        for entry in &self.entries {
            let path: PathBuf = entry
                .name
                .trim_end_matches('/')
                .split('/')
                .fold(dir.to_path_buf(), |path, part| path.join(part));
            if entry.is_dir() {
                fs::create_dir_all(&path)?;
                continue;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out = File::create_new(&path)?;
            let mut reader = self.reader(entry)?;
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                written += n as u64;
                progress(n as u64)?;
            }
            out.sync_all()?;
        }
        Ok(written)
    }
}

fn check_strict(entry: &Entry, file_type: u32, disk: u16, limits: &Limits) -> io::Result<()> {
    let name = &entry.name;
    validate_name(name).map_err(invalid)?;
    if entry.flags & FLAG_ENCRYPTED != 0 {
        return Err(invalid(format!("{name:?} is encrypted")));
    }
    if entry.method != STORED && entry.method != DEFLATED {
        return Err(invalid(format!(
            "{name:?} uses unsupported compression method {}",
            entry.method
        )));
    }
    if !matches!(file_type & UNIX_TYPE, 0 | UNIX_FILE | UNIX_DIR) {
        return Err(invalid(format!(
            "{name:?} is a symbolic link or special file"
        )));
    }
    if disk != 0 {
        return Err(invalid("multi-disk zip archives are not supported"));
    }
    if entry.is_dir() && entry.size != 0 {
        return Err(invalid(format!("directory {name:?} has contents")));
    }
    if entry.method == STORED && entry.compressed != entry.size {
        return Err(invalid(format!("{name:?} has mismatched stored sizes")));
    }
    if entry.size > limits.max_entry_bytes {
        return Err(invalid(format!(
            "{name:?} claims {} bytes, over the {} byte limit",
            entry.size, limits.max_entry_bytes
        )));
    }
    if let Some(ratio) = limits.max_ratio
        && entry.size > entry.compressed.saturating_mul(ratio).max(ratio)
    {
        return Err(invalid(format!("{name:?} compresses more than {ratio}:1")));
    }
    Ok(())
}

fn check_names(entries: &[Entry]) -> io::Result<()> {
    let mut seen = HashSet::with_capacity(entries.len());
    let mut files = HashSet::new();
    let mut dirs = HashSet::new();
    for entry in entries {
        let folded = entry.name.to_lowercase();
        let body = folded.strip_suffix('/').unwrap_or(&folded).to_owned();
        for (at, _) in body.match_indices('/') {
            dirs.insert(body[..at].to_owned());
        }
        if !seen.insert(body.clone()) {
            return Err(invalid(format!(
                "{:?} appears more than once (ignoring case)",
                entry.name
            )));
        }
        if entry.is_dir() {
            dirs.insert(body);
        } else {
            files.insert(body);
        }
    }
    if let Some(both) = files.iter().find(|f| dirs.contains(*f)) {
        return Err(invalid(format!("{both:?} is both a file and a directory")));
    }
    Ok(())
}

fn check_layout(entries: &[Entry], directory_offset: u64) -> io::Result<()> {
    let mut spans: Vec<(u64, u64)> = entries
        .iter()
        .map(|e| {
            let end = e
                .local
                .saturating_add(LOCAL_HEADER_LEN + e.name.len() as u64)
                .saturating_add(e.compressed);
            (e.local, end)
        })
        .collect();
    spans.sort_unstable();
    let mut previous_end = 0;
    for (start, end) in spans {
        if start < previous_end || end > directory_offset {
            return Err(invalid("zip entries overlap or run into the directory"));
        }
        previous_end = end;
    }
    Ok(())
}

enum Inner<'a, S: ?Sized> {
    Stored(SourceReader<'a, S>),
    Deflated(Box<DeflateDecoder<SourceReader<'a, S>>>),
}

/// Reads one entry; see [`Archive::reader`].
pub struct EntryReader<'a, S> {
    inner: Inner<'a, S>,
    name: String,
    expected: u64,
    crc_expected: u32,
    produced: u64,
    crc: Crc,
    total: &'a AtomicU64,
    max_total: u64,
    done: bool,
}

impl<S: Source> Read for EntryReader<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.done {
            return Ok(0);
        }
        let room = (self.expected - self.produced).saturating_add(1);
        let want = (buf.len() as u64).min(room) as usize;
        let name = &self.name;
        let n = match &mut self.inner {
            Inner::Stored(reader) => reader.read(&mut buf[..want]),
            Inner::Deflated(reader) => reader.read(&mut buf[..want]),
        }
        .map_err(|err| invalid(format!("{name:?} is corrupt: {err}")))?;
        if n == 0 {
            if self.produced != self.expected {
                return Err(invalid(format!(
                    "{name:?} inflates to {} bytes, not the {} it claims",
                    self.produced, self.expected
                )));
            }
            if self.crc.sum() != self.crc_expected {
                return Err(invalid(format!("{name:?} fails its checksum")));
            }
            self.done = true;
            return Ok(0);
        }
        let produced = self.produced + n as u64;
        if produced > self.expected {
            return Err(invalid(format!(
                "{name:?} inflates to {}+ bytes, not the {} it claims",
                self.expected + 1,
                self.expected
            )));
        }
        let before = self.total.fetch_add(n as u64, Ordering::Relaxed);
        if before.saturating_add(n as u64) > self.max_total {
            return Err(invalid(format!(
                "archive inflates past its {} byte limit",
                self.max_total
            )));
        }
        self.crc.update(&buf[..n]);
        self.produced = produced;
        Ok(n)
    }
}
