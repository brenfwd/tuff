//! The binary packet recording format (`.tuffrec`).
//!
//! ```text
//! magic    "TUFFREC\0"
//! version  u16 LE
//! hlen     u32 LE, then hlen header bytes
//! block*   raw_len u32 LE, packed_len u32 LE, records u32 LE, crc32 u32 LE,
//!          packed_len zlib bytes inflating to raw_len bytes of records
//! end      a block header of four zeroes
//! ```
//!
//! A record is a tag byte, a zigzag varint time delta in nanoseconds from the
//! previous record, a varint payload length and the payload: a decompressed
//! packet (id and body) or an event. With [`FLAG_DEDUPE`] set in the header
//! (global traces), a packet of [`DEDUPE_MIN`] to [`DEDUPE_MAX`] bytes that
//! was already written within the sliding window of the last
//! [`WINDOW_ENTRIES`] such packets (at most [`WINDOW_BYTES`] bytes) is
//! written as a reference record: the reference tag bit and a varint
//! sequence number of the earlier packet. Both sides number the full
//! packets in file order, so a reader resolves references itself. Every length read from a file is checked
//! against a cap before anything is allocated for it, and inflation is
//! bounded by the declared raw length.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};

use bytes::Bytes;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

use super::{ConnState, Direction};

pub const MAGIC: &[u8; 8] = b"TUFFREC\0";
pub const VERSION: u16 = 1;
pub const EXTENSION: &str = "tuffrec";

pub const MAX_HEADER: usize = 64 * 1024;
pub const MAX_RECORD: usize = 8 * 1024 * 1024;
pub const MAX_BLOCK_RAW: usize = 16 * 1024 * 1024;
const PACKED_SLACK: usize = 1024;
const BLOCK_TARGET: usize = 64 * 1024;
const BLOCK_HEADER: usize = 16;

pub const FLAG_DEDUPE: u32 = 1;
pub const DEDUPE_MIN: usize = 32;
pub const DEDUPE_MAX: usize = 64 * 1024;
pub const WINDOW_ENTRIES: usize = 4096;
pub const WINDOW_BYTES: usize = 8 * 1024 * 1024;

pub const EVENT_CONNECTION: u8 = 0;
pub const EVENT_DROPPED: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag(u8);

impl Tag {
    const STATE: u8 = 0b111;
    const CLIENTBOUND: u8 = 1 << 3;
    const REDACTED: u8 = 1 << 4;
    const CHAT: u8 = 1 << 5;
    const EVENT: u8 = 1 << 6;
    const REFERENCE: u8 = 1 << 7;

    pub fn packet(state: ConnState, direction: Direction) -> Self {
        let bound = match direction {
            Direction::Serverbound => 0,
            Direction::Clientbound => Self::CLIENTBOUND,
        };
        Self(state as u8 | bound)
    }

    pub const fn event() -> Self {
        Self(Self::EVENT)
    }

    pub fn redacted(self) -> Self {
        Self(self.0 | Self::REDACTED)
    }

    pub fn chat(self) -> Self {
        Self(self.0 | Self::CHAT)
    }

    pub fn from_byte(byte: u8) -> Option<Self> {
        if byte & Self::REFERENCE != 0 && byte & (Self::EVENT | Self::REDACTED) != 0 {
            return None;
        }
        if byte & Self::STATE > ConnState::Play as u8 {
            return None;
        }
        Some(Self(byte))
    }

    pub const fn byte(self) -> u8 {
        self.0
    }

    pub fn state(self) -> ConnState {
        ConnState::from_u8(self.0 & Self::STATE)
    }

    pub fn direction(self) -> Direction {
        if self.0 & Self::CLIENTBOUND != 0 {
            Direction::Clientbound
        } else {
            Direction::Serverbound
        }
    }

    pub fn is_redacted(self) -> bool {
        self.0 & Self::REDACTED != 0
    }

    pub fn is_chat(self) -> bool {
        self.0 & Self::CHAT != 0
    }

    pub fn is_event(self) -> bool {
        self.0 & Self::EVENT != 0
    }

    fn referencing(self) -> Self {
        Self(self.0 | Self::REFERENCE)
    }

    fn is_reference(self) -> bool {
        self.0 & Self::REFERENCE != 0
    }

    fn dereferenced(self) -> Self {
        Self(self.0 & !Self::REFERENCE)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub protocol_version: i32,
    pub game_version: String,
    pub start_unix_ns: u64,
    pub player: String,
    pub flags: u32,
}

impl Header {
    pub fn current(player: &str, start_unix_ns: u64) -> Self {
        Self {
            protocol_version: tuff_vanilla_data::version::PROTOCOL_VERSION,
            game_version: tuff_vanilla_data::version::NAME.to_owned(),
            start_unix_ns,
            player: player.to_owned(),
            flags: 0,
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, u64::from(self.protocol_version as u32));
        put_str(&mut out, &self.game_version);
        out.extend_from_slice(&self.start_unix_ns.to_le_bytes());
        put_str(&mut out, &self.player);
        put_varint(&mut out, u64::from(self.flags));
        out
    }

    fn decode(bytes: &[u8]) -> Result<Self, FormatError> {
        let mut r = Cursor::new(bytes);
        let protocol_version = u32::try_from(r.varint()?).map_err(|_| FormatError::Header)? as i32;
        let game_version = r.string()?;
        let start_unix_ns = u64::from_le_bytes(r.take(8)?.try_into().unwrap());
        let player = r.string()?;
        let flags = u32::try_from(r.varint()?).map_err(|_| FormatError::Header)?;
        Ok(Self {
            protocol_version,
            game_version,
            start_unix_ns,
            player,
            flags,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Nanoseconds since the recording started.
    pub ns: i64,
    pub tag: Tag,
    pub payload: Bytes,
}

impl Record {
    /// The packet id, for packet records with a readable id.
    pub fn packet_id(&self) -> Option<i32> {
        if self.tag.is_event() {
            return None;
        }
        let mut r = Cursor::new(&self.payload);
        let id = r.varint().ok()?;
        i32::try_from(id).ok()
    }

    /// The packet body after the id.
    pub fn body(&self) -> Bytes {
        let mut r = Cursor::new(&self.payload);
        match r.varint() {
            Ok(_) => self.payload.slice(r.pos..),
            Err(_) => Bytes::new(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FormatError {
    #[error("not a tuff recording")]
    Magic,
    #[error("recording format version {found} is not supported (this build reads {VERSION})")]
    Version { found: u16 },
    #[error("malformed recording header")]
    Header,
    #[error("block of {0} bytes exceeds the limit")]
    BlockTooLarge(usize),
    #[error("block is corrupt: {0}")]
    Corrupt(&'static str),
    #[error("record of {0} bytes exceeds the limit")]
    RecordTooLarge(usize),
    #[error("recording exceeds the {0} byte limit")]
    TooLarge(usize),
    #[error("read failed: {0}")]
    Io(String),
}

/// The sliding window of recently written full packets, identical on both
/// sides of a recording.
#[derive(Default)]
struct Window {
    entries: VecDeque<Bytes>,
    first: u64,
    bytes: usize,
}

impl Window {
    fn eligible(tag: Tag, payload: &[u8]) -> bool {
        !tag.is_event() && !tag.is_redacted() && (DEDUPE_MIN..=DEDUPE_MAX).contains(&payload.len())
    }

    /// Adds a packet and returns the ones that fell out of the window.
    fn define(&mut self, payload: Bytes) -> Vec<Bytes> {
        self.bytes += payload.len();
        self.entries.push_back(payload);
        let mut evicted = Vec::new();
        while self.entries.len() > WINDOW_ENTRIES || self.bytes > WINDOW_BYTES {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.len();
            self.first += 1;
            evicted.push(old);
        }
        evicted
    }

    fn next_id(&self) -> u64 {
        self.first + self.entries.len() as u64
    }

    fn get(&self, id: u64) -> Option<&Bytes> {
        let offset = usize::try_from(id.checked_sub(self.first)?).ok()?;
        self.entries.get(offset)
    }
}

/// Writer-side dedupe state: the window plus a content index into it.
#[derive(Default)]
struct Dedupe {
    window: Window,
    index: HashMap<Bytes, u64>,
}

/// Appends records to a recording, one compressed block at a time.
pub struct Writer<W: Write> {
    dedupe: Option<Dedupe>,
    out: W,
    block: Vec<u8>,
    records: u32,
    last_ns: i64,
    written: u64,
}

impl<W: Write> Writer<W> {
    pub fn new(mut out: W, header: &Header) -> std::io::Result<Self> {
        let body = header.encode();
        out.write_all(MAGIC)?;
        out.write_all(&VERSION.to_le_bytes())?;
        out.write_all(&(body.len() as u32).to_le_bytes())?;
        out.write_all(&body)?;
        Ok(Self {
            dedupe: (header.flags & FLAG_DEDUPE != 0).then(Dedupe::default),
            out,
            block: Vec::with_capacity(BLOCK_TARGET * 2),
            records: 0,
            last_ns: 0,
            written: (MAGIC.len() + 6 + body.len()) as u64,
        })
    }

    /// Bytes written to the output so far, excluding the open block.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Appends a record; payloads above [`MAX_RECORD`] are cut to an empty
    /// redacted record so a reader never rejects the file for it.
    pub fn push(&mut self, ns: i64, tag: Tag, payload: &[u8]) -> std::io::Result<()> {
        let (tag, payload) = if payload.len() > MAX_RECORD {
            let id_len = payload
                .iter()
                .take(5)
                .position(|b| b & 0x80 == 0)
                .map_or(0, |i| i + 1);
            (tag.redacted(), &payload[..id_len])
        } else {
            (tag, payload)
        };
        let mut reference = Vec::new();
        let (tag, payload) = match &mut self.dedupe {
            Some(dedupe) if Window::eligible(tag, payload) => {
                match dedupe.index.get(payload).copied() {
                    Some(id) if dedupe.window.get(id).is_some() => {
                        put_varint(&mut reference, id);
                        (tag.referencing(), &reference[..])
                    }
                    _ => {
                        let copy = Bytes::copy_from_slice(payload);
                        dedupe.index.insert(copy.clone(), dedupe.window.next_id());
                        for old in dedupe.window.define(copy) {
                            dedupe.index.remove(&old);
                        }
                        (tag, payload)
                    }
                }
            }
            _ => (tag, payload),
        };
        if self.block.len() + payload.len() + 20 > MAX_BLOCK_RAW {
            self.flush_block()?;
        }
        self.block.push(tag.byte());
        put_varint(&mut self.block, zigzag(ns.wrapping_sub(self.last_ns)));
        put_varint(&mut self.block, payload.len() as u64);
        self.block.extend_from_slice(payload);
        self.last_ns = ns;
        self.records += 1;
        if self.block.len() >= BLOCK_TARGET {
            self.flush_block()?;
        }
        Ok(())
    }

    /// Compresses and writes the open block, if it holds any records.
    pub fn flush_block(&mut self) -> std::io::Result<()> {
        if self.records == 0 {
            return Ok(());
        }
        let mut crc = flate2::Crc::new();
        crc.update(&self.block);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
        encoder.write_all(&self.block)?;
        let packed = encoder.finish()?;
        let mut head = [0u8; BLOCK_HEADER];
        head[0..4].copy_from_slice(&(self.block.len() as u32).to_le_bytes());
        head[4..8].copy_from_slice(&(packed.len() as u32).to_le_bytes());
        head[8..12].copy_from_slice(&self.records.to_le_bytes());
        head[12..16].copy_from_slice(&crc.sum().to_le_bytes());
        self.out.write_all(&head)?;
        self.out.write_all(&packed)?;
        self.written += (BLOCK_HEADER + packed.len()) as u64;
        self.block.clear();
        self.records = 0;
        Ok(())
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        self.flush_block()?;
        self.out.flush()
    }

    /// Writes the open block and the end marker.
    pub fn finish(mut self) -> std::io::Result<W> {
        self.flush_block()?;
        self.out.write_all(&[0u8; BLOCK_HEADER])?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Reads a recording block by block.
pub struct Reader<R: Read> {
    window: Option<Window>,
    input: R,
    header: Header,
    last_ns: i64,
    ended: bool,
    truncated: bool,
}

impl<R: Read> Reader<R> {
    pub fn new(mut input: R) -> Result<Self, FormatError> {
        let mut magic = [0u8; 8];
        read_exact(&mut input, &mut magic).map_err(|_| FormatError::Magic)?;
        if &magic != MAGIC {
            return Err(FormatError::Magic);
        }
        let mut fixed = [0u8; 6];
        read_exact(&mut input, &mut fixed).map_err(|_| FormatError::Header)?;
        let found = u16::from_le_bytes([fixed[0], fixed[1]]);
        if found != VERSION {
            return Err(FormatError::Version { found });
        }
        let len = u32::from_le_bytes(fixed[2..6].try_into().unwrap()) as usize;
        if len > MAX_HEADER {
            return Err(FormatError::Header);
        }
        let mut body = vec![0u8; len];
        read_exact(&mut input, &mut body).map_err(|_| FormatError::Header)?;
        let header = Header::decode(&body)?;
        Ok(Self {
            window: (header.flags & FLAG_DEDUPE != 0).then(Window::default),
            input,
            header,
            last_ns: 0,
            ended: false,
            truncated: false,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// True once the input ended without an end marker or mid-block.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// The next block's records, or `None` at the end of the recording.
    pub fn next_block(&mut self) -> Result<Option<Vec<Record>>, FormatError> {
        if self.ended {
            return Ok(None);
        }
        let mut head = [0u8; BLOCK_HEADER];
        match read_exact(&mut self.input, &mut head) {
            Ok(()) => {}
            Err(Short::Eof | Short::Partial) => return Ok(self.cut()),
            Err(Short::Io(err)) => return Err(FormatError::Io(err)),
        }
        let word = |i: usize| u32::from_le_bytes(head[i..i + 4].try_into().unwrap());
        let (raw_len, packed_len, count, crc) =
            (word(0) as usize, word(4) as usize, word(8), word(12));
        if head == [0u8; BLOCK_HEADER] {
            self.ended = true;
            return Ok(None);
        }
        if raw_len > MAX_BLOCK_RAW {
            return Err(FormatError::BlockTooLarge(raw_len));
        }
        if packed_len > raw_len + PACKED_SLACK || packed_len == 0 || count == 0 {
            return Err(FormatError::Corrupt("block lengths"));
        }
        let mut packed = Vec::new();
        let got = (&mut self.input)
            .take(packed_len as u64)
            .read_to_end(&mut packed)
            .map_err(|err| FormatError::Io(err.to_string()))?;
        if got < packed_len {
            return Ok(self.cut());
        }
        let mut raw = Vec::new();
        ZlibDecoder::new(&packed[..])
            .take(raw_len as u64 + 1)
            .read_to_end(&mut raw)
            .map_err(|_| FormatError::Corrupt("inflate"))?;
        if raw.len() != raw_len {
            return Err(FormatError::Corrupt("inflated length"));
        }
        let mut sum = flate2::Crc::new();
        sum.update(&raw);
        if sum.sum() != crc {
            return Err(FormatError::Corrupt("checksum"));
        }
        self.parse(Bytes::from(raw), count).map(Some)
    }

    fn cut(&mut self) -> Option<Vec<Record>> {
        self.ended = true;
        self.truncated = true;
        None
    }

    fn parse(&mut self, raw: Bytes, count: u32) -> Result<Vec<Record>, FormatError> {
        let mut records = Vec::with_capacity((count as usize).min(raw.len() / 3 + 1));
        let mut r = Cursor::new(&raw);
        while r.pos < raw.len() {
            let tag = Tag::from_byte(r.take(1)?[0]).ok_or(FormatError::Corrupt("record tag"))?;
            let dt = unzigzag(r.varint()?);
            let len = usize::try_from(r.varint()?).unwrap_or(usize::MAX);
            if len > MAX_RECORD {
                return Err(FormatError::RecordTooLarge(len));
            }
            let start = r.pos;
            r.take(len)?;
            self.last_ns = self.last_ns.wrapping_add(dt);
            let mut tag = tag;
            let mut payload = raw.slice(start..start + len);
            if tag.is_reference() {
                let window = self
                    .window
                    .as_ref()
                    .ok_or(FormatError::Corrupt("reference"))?;
                let mut body = Cursor::new(&payload);
                let id = body.varint()?;
                if body.pos != payload.len() {
                    return Err(FormatError::Corrupt("reference"));
                }
                payload = window
                    .get(id)
                    .cloned()
                    .ok_or(FormatError::Corrupt("dangling reference"))?;
                tag = tag.dereferenced();
            } else if let Some(window) = &mut self.window
                && Window::eligible(tag, &payload)
            {
                window.define(Bytes::copy_from_slice(&payload));
            }
            records.push(Record {
                ns: self.last_ns,
                tag,
                payload,
            });
        }
        if records.len() != count as usize {
            return Err(FormatError::Corrupt("record count"));
        }
        Ok(records)
    }
}

/// A whole recording held in memory.
#[derive(Debug)]
pub struct Recording {
    pub header: Header,
    pub records: Vec<Record>,
    pub truncated: bool,
}

/// Reads every block, refusing recordings whose records exceed `max_raw`
/// payload bytes in total.
pub fn read_all(input: impl Read, max_raw: usize) -> Result<Recording, FormatError> {
    let mut reader = Reader::new(input)?;
    let mut records = Vec::new();
    let mut total = 0usize;
    while let Some(block) = reader.next_block()? {
        total += block.iter().map(|r| r.payload.len()).sum::<usize>();
        if total > max_raw {
            return Err(FormatError::TooLarge(max_raw));
        }
        records.extend(block);
    }
    Ok(Recording {
        header: reader.header.clone(),
        records,
        truncated: reader.truncated,
    })
}

enum Short {
    Eof,
    Partial,
    Io(String),
}

fn read_exact(input: &mut impl Read, buf: &mut [u8]) -> Result<(), Short> {
    let mut filled = 0;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Err(Short::Eof),
            Ok(0) => return Err(Short::Partial),
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(Short::Io(err.to_string())),
        }
    }
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], FormatError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(FormatError::Corrupt("truncated record"))?;
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn varint(&mut self) -> Result<u64, FormatError> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.take(1)?[0];
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(FormatError::Corrupt("varint"))
    }

    fn string(&mut self) -> Result<String, FormatError> {
        let len = usize::try_from(self.varint()?).unwrap_or(usize::MAX);
        if len > MAX_HEADER {
            return Err(FormatError::Header);
        }
        String::from_utf8(self.take(len)?.to_vec()).map_err(|_| FormatError::Header)
    }
}

pub fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Reads a format varint from the front of `bytes`.
pub fn get_varint(bytes: &[u8]) -> Option<u64> {
    Cursor::new(bytes).varint().ok()
}

fn put_str(out: &mut Vec<u8>, value: &str) {
    put_varint(out, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}

fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

fn unzigzag(value: u64) -> i64 {
    ((value >> 1) as i64) ^ -((value & 1) as i64)
}

#[cfg(test)]
mod tests;
