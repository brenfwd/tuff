//! NBT values with codecs for binary NBT (named file roots and nameless network
//! roots), gzip/zlib compression, and SNBT text.

pub mod lz4;
mod structure;
pub use structure::{snbt_to_structure, structure_to_snbt};

use std::borrow::Cow;
use std::fmt::{self, Write as _};
use std::io::{Read, Write};
use std::str::FromStr;

use bytes::BufMut;
use flate2::Compression as Level;
use flate2::read::{MultiGzDecoder, ZlibDecoder};
use flate2::write::GzEncoder;
use indexmap::IndexMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NbtError {
    #[error("unexpected end of input")]
    UnexpectedEof,
    #[error("invalid tag type {0}")]
    InvalidTagType(u8),
    #[error("unexpected End tag")]
    UnexpectedEnd,
    #[error("list of End tags has non-zero length")]
    UntypedList,
    #[error("negative length {0}")]
    NegativeLength(i32),
    #[error("nesting deeper than {0} levels")]
    DepthLimit(usize),
    #[error("size limit exceeded")]
    SizeLimit,
    #[error("invalid modified UTF-8 string")]
    InvalidString,
    #[error("string too long: {0} bytes")]
    StringTooLong(usize),
    #[error("length too large: {0}")]
    LengthOverflow(usize),
    #[error("SNBT syntax error at byte {position}: {reason}")]
    Snbt {
        position: usize,
        reason: &'static str,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, NbtError>;

pub const MAX_DEPTH: usize = 512;

/// Bounds applied while decoding. `max_bytes` caps both the estimated memory
/// of decoded tags and the size of decompressed input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NbtLimits {
    pub max_depth: usize,
    pub max_bytes: usize,
}

impl NbtLimits {
    pub const NETWORK: Self = Self {
        max_depth: MAX_DEPTH,
        max_bytes: 2 * 1024 * 1024,
    };
    pub const TRUSTED: Self = Self {
        max_depth: MAX_DEPTH,
        max_bytes: usize::MAX,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TagType {
    End = 0,
    Byte = 1,
    Short = 2,
    Int = 3,
    Long = 4,
    Float = 5,
    Double = 6,
    ByteArray = 7,
    String = 8,
    List = 9,
    Compound = 10,
    IntArray = 11,
    LongArray = 12,
}

impl TagType {
    pub fn from_id(id: u8) -> Result<Self> {
        Ok(match id {
            0 => Self::End,
            1 => Self::Byte,
            2 => Self::Short,
            3 => Self::Int,
            4 => Self::Long,
            5 => Self::Float,
            6 => Self::Double,
            7 => Self::ByteArray,
            8 => Self::String,
            9 => Self::List,
            10 => Self::Compound,
            11 => Self::IntArray,
            12 => Self::LongArray,
            _ => return Err(NbtError::InvalidTagType(id)),
        })
    }

    pub fn id(self) -> u8 {
        self as u8
    }

    fn min_payload_size(self) -> usize {
        match self {
            Self::End => 0,
            Self::Byte | Self::Compound => 1,
            Self::Short | Self::String => 2,
            Self::Int | Self::Float | Self::ByteArray | Self::IntArray | Self::LongArray => 4,
            Self::List => 5,
            Self::Long | Self::Double => 8,
        }
    }
}

/// A single NBT value. Lists may be heterogeneous (as in vanilla since
/// 1.21.5); the binary encoding wraps mixed elements in `{"": value}`
/// compounds and unwraps them again when reading.
#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    List(Vec<Tag>),
    Compound(Compound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

macro_rules! tag_accessors {
    ($($name:ident -> $ty:ty, $variant:ident($v:ident) => $e:expr;)*) => {
        $(pub fn $name(&self) -> Option<$ty> {
            match self {
                Self::$variant($v) => Some($e),
                _ => None,
            }
        })*
    };
}

impl Tag {
    pub fn tag_type(&self) -> TagType {
        match self {
            Self::Byte(_) => TagType::Byte,
            Self::Short(_) => TagType::Short,
            Self::Int(_) => TagType::Int,
            Self::Long(_) => TagType::Long,
            Self::Float(_) => TagType::Float,
            Self::Double(_) => TagType::Double,
            Self::ByteArray(_) => TagType::ByteArray,
            Self::String(_) => TagType::String,
            Self::List(_) => TagType::List,
            Self::Compound(_) => TagType::Compound,
            Self::IntArray(_) => TagType::IntArray,
            Self::LongArray(_) => TagType::LongArray,
        }
    }

    tag_accessors! {
        as_byte_array -> &[i8], ByteArray(v) => v;
        as_str -> &str, String(v) => v;
        as_list -> &[Tag], List(v) => v;
        as_compound -> &Compound, Compound(v) => v;
        as_int_array -> &[i32], IntArray(v) => v;
        as_long_array -> &[i64], LongArray(v) => v;
    }

    pub fn as_byte(&self) -> Option<i8> {
        self.as_int().map(|v| (v & 0xFF) as u8 as i8)
    }

    pub fn as_short(&self) -> Option<i16> {
        self.as_int().map(|v| (v & 0xFFFF) as u16 as i16)
    }

    pub fn as_int(&self) -> Option<i32> {
        match *self {
            Self::Byte(v) => Some(v.into()),
            Self::Short(v) => Some(v.into()),
            Self::Int(v) => Some(v),
            Self::Long(v) => Some(v as i32),
            Self::Float(v) => Some(v.floor() as i32),
            Self::Double(v) => Some(v.floor() as i32),
            _ => None,
        }
    }

    pub fn as_long(&self) -> Option<i64> {
        match *self {
            Self::Long(v) => Some(v),
            Self::Float(v) => Some(v.floor() as i64),
            Self::Double(v) => Some(v.floor() as i64),
            _ => self.as_int().map(Into::into),
        }
    }

    pub fn as_float(&self) -> Option<f32> {
        match *self {
            Self::Float(v) => Some(v),
            Self::Long(v) => Some(v as f32),
            Self::Double(v) => Some(v as f32),
            _ => self.as_int().map(|v| v as f32),
        }
    }

    pub fn as_double(&self) -> Option<f64> {
        self.as_number()
    }

    pub fn as_bool(&self) -> Option<bool> {
        self.as_byte().map(|v| v != 0)
    }

    pub fn as_list_mut(&mut self) -> Option<&mut Vec<Tag>> {
        match self {
            Self::List(v) => Some(v),
            _ => None,
        }
    }

    pub fn as_compound_mut(&mut self) -> Option<&mut Compound> {
        match self {
            Self::Compound(v) => Some(v),
            _ => None,
        }
    }

    fn as_number(&self) -> Option<f64> {
        match *self {
            Self::Byte(v) => Some(v.into()),
            Self::Short(v) => Some(v.into()),
            Self::Int(v) => Some(v.into()),
            Self::Long(v) => Some(v as f64),
            Self::Float(v) => Some(v.into()),
            Self::Double(v) => Some(v),
            _ => None,
        }
    }

    /// Encodes a UUID the way vanilla stores it: four big-endian ints.
    pub fn uuid(uuid: u128) -> Self {
        Self::IntArray(vec![
            (uuid >> 96) as i32,
            (uuid >> 64) as i32,
            (uuid >> 32) as i32,
            uuid as i32,
        ])
    }

    pub fn as_uuid(&self) -> Option<u128> {
        match self.as_int_array()? {
            &[a, b, c, d] => Some(
                (u128::from(a as u32) << 96)
                    | (u128::from(b as u32) << 64)
                    | (u128::from(c as u32) << 32)
                    | u128::from(d as u32),
            ),
            _ => None,
        }
    }

    /// Reads a network NBT value (type id, no name). An End tag yields `None`.
    /// On success `input` is advanced past the value.
    pub fn read_network(input: &mut &[u8], limits: NbtLimits) -> Result<Option<Tag>> {
        let mut decoder = Decoder::new(input, limits);
        let tag = match TagType::from_id(decoder.u8()?)? {
            TagType::End => None,
            ty => Some(decoder.tag(ty, 0)?),
        };
        *input = decoder.input;
        Ok(tag)
    }

    pub fn write_network(&self, out: &mut impl BufMut) -> Result<()> {
        out.put_u8(self.tag_type().id());
        write_payload(self, out)
    }

    pub fn to_network_bytes(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.write_network(&mut out)?;
        Ok(out)
    }

    pub fn from_snbt(input: &str) -> Result<Tag> {
        let (tag, consumed) = Self::parse_snbt_prefix(input)?;
        let rest = &input[consumed..];
        if !rest.trim_start().is_empty() {
            return Err(NbtError::Snbt {
                position: consumed + (rest.len() - rest.trim_start().len()),
                reason: "trailing characters",
            });
        }
        Ok(tag)
    }

    /// Parses one SNBT value from the start of `input`, returning it and
    /// the number of bytes it occupied, so a value can sit inside other
    /// syntax (a command argument).
    pub fn parse_snbt_prefix(input: &str) -> Result<(Tag, usize)> {
        let mut parser = SnbtParser {
            src: input,
            pos: 0,
            depth: 0,
        };
        let tag = parser.value()?;
        Ok((tag, parser.pos))
    }
}

impl FromStr for Tag {
    type Err = NbtError;

    fn from_str(s: &str) -> Result<Self> {
        Self::from_snbt(s)
    }
}

macro_rules! tag_from {
    ($($ty:ty => $variant:ident),* $(,)?) => {
        $(impl From<$ty> for Tag {
            fn from(v: $ty) -> Self {
                Self::$variant(v)
            }
        })*
    };
}

tag_from! {
    i8 => Byte,
    i16 => Short,
    i32 => Int,
    i64 => Long,
    f32 => Float,
    f64 => Double,
    Vec<i8> => ByteArray,
    String => String,
    Vec<Tag> => List,
    Compound => Compound,
    Vec<i32> => IntArray,
    Vec<i64> => LongArray,
}

impl From<bool> for Tag {
    fn from(v: bool) -> Self {
        Self::Byte(v.into())
    }
}

impl From<&str> for Tag {
    fn from(v: &str) -> Self {
        Self::String(v.to_owned())
    }
}

impl From<Vec<Compound>> for Tag {
    fn from(v: Vec<Compound>) -> Self {
        Self::List(v.into_iter().map(Tag::Compound).collect())
    }
}

/// An insertion-ordered NBT compound.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Compound(IndexMap<String, Tag>);

macro_rules! compound_getters {
    ($($name:ident -> $ty:ty = $as:ident;)*) => {
        $(pub fn $name(&self, key: &str) -> Option<$ty> {
            self.get(key)?.$as()
        })*
    };
}

impl Compound {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self(IndexMap::with_capacity(capacity))
    }

    pub fn with(mut self, key: impl Into<String>, value: impl Into<Tag>) -> Self {
        self.insert(key, value);
        self
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<Tag>) -> Option<Tag> {
        self.0.insert(key.into(), value.into())
    }

    pub fn remove(&mut self, key: &str) -> Option<Tag> {
        self.0.shift_remove(key)
    }

    pub fn get(&self, key: &str) -> Option<&Tag> {
        self.0.get(key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Tag> {
        self.0.get_mut(key)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &Tag)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn iter_mut(&mut self) -> impl ExactSizeIterator<Item = (&str, &mut Tag)> {
        self.0.iter_mut().map(|(k, v)| (k.as_str(), v))
    }

    pub fn keys(&self) -> impl ExactSizeIterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    pub fn values(&self) -> impl ExactSizeIterator<Item = &Tag> {
        self.0.values()
    }

    compound_getters! {
        get_byte -> i8 = as_byte;
        get_short -> i16 = as_short;
        get_int -> i32 = as_int;
        get_long -> i64 = as_long;
        get_float -> f32 = as_float;
        get_double -> f64 = as_double;
        get_bool -> bool = as_bool;
        get_byte_array -> &[i8] = as_byte_array;
        get_str -> &str = as_str;
        get_list -> &[Tag] = as_list;
        get_compound -> &Compound = as_compound;
        get_int_array -> &[i32] = as_int_array;
        get_long_array -> &[i64] = as_long_array;
        get_uuid -> u128 = as_uuid;
    }

    pub fn get_list_mut(&mut self, key: &str) -> Option<&mut Vec<Tag>> {
        self.get_mut(key)?.as_list_mut()
    }

    pub fn get_compound_mut(&mut self, key: &str) -> Option<&mut Compound> {
        self.get_mut(key)?.as_compound_mut()
    }

    fn is_wrapper(&self) -> bool {
        self.0.len() == 1 && self.0.contains_key("")
    }
}

impl<K: Into<String>, V: Into<Tag>> FromIterator<(K, V)> for Compound {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(
            iter.into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

impl IntoIterator for Compound {
    type Item = (String, Tag);
    type IntoIter = indexmap::map::IntoIter<String, Tag>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// A root tag with its name, as stored in files (`level.dat`, region chunks,
/// structure templates).
#[derive(Clone, Debug, PartialEq)]
pub struct NamedTag {
    pub name: String,
    pub tag: Tag,
}

impl NamedTag {
    pub fn new(name: impl Into<String>, tag: impl Into<Tag>) -> Self {
        Self {
            name: name.into(),
            tag: tag.into(),
        }
    }

    /// Reads an uncompressed named root and advances `input` past it.
    pub fn read(input: &mut &[u8], limits: NbtLimits) -> Result<Self> {
        let mut decoder = Decoder::new(input, limits);
        let ty = TagType::from_id(decoder.u8()?)?;
        if ty == TagType::End {
            return Err(NbtError::UnexpectedEnd);
        }
        let name = decoder.string()?;
        let tag = decoder.tag(ty, 0)?;
        *input = decoder.input;
        Ok(Self { name, tag })
    }

    /// Reads the named root of `input`, decoding only the top-level entries
    /// of a root compound whose key is in `keys` and skipping every other
    /// value without allocating. Reading stops once every wanted key has been
    /// seen, so malformed data after that point goes unnoticed. A root that
    /// is not a compound is decoded in full.
    pub fn read_selected(input: &[u8], limits: NbtLimits, keys: &[&str]) -> Result<Self> {
        let mut decoder = Decoder::new(input, limits);
        let ty = TagType::from_id(decoder.u8()?)?;
        if ty == TagType::End {
            return Err(NbtError::UnexpectedEnd);
        }
        let name = decoder.string()?;
        if ty != TagType::Compound {
            return Ok(Self {
                name,
                tag: decoder.tag(ty, 0)?,
            });
        }
        let mut selected = Compound::new();
        let mut missing = keys.iter().filter(|k| !selected.contains_key(k)).count();
        while missing > 0 {
            let entry = TagType::from_id(decoder.u8()?)?;
            if entry == TagType::End {
                break;
            }
            let key = decoder.string()?;
            if keys.contains(&key.as_str()) {
                let value = decoder.tag(entry, 1)?;
                if selected.insert(key, value).is_none() {
                    missing -= 1;
                }
            } else {
                decoder.skip(entry, 1)?;
            }
        }
        Ok(Self {
            name,
            tag: Tag::Compound(selected),
        })
    }

    /// Like [`NamedTag::from_bytes`], but decoding only the wanted top-level
    /// keys as [`NamedTag::read_selected`] does.
    pub fn from_bytes_selected(
        data: &[u8],
        limits: NbtLimits,
        keys: &[&str],
    ) -> Result<(Self, Compression)> {
        let compression = Compression::detect(data);
        let raw = compression.decompress(data, limits.max_bytes)?;
        let named = Self::read_selected(&raw, limits, keys)?;
        Ok((named, compression))
    }

    pub fn write(&self, out: &mut impl BufMut) -> Result<()> {
        out.put_u8(self.tag.tag_type().id());
        write_string(&self.name, out)?;
        write_payload(&self.tag, out)
    }

    /// Decodes a whole file, detecting gzip, zlib, or no compression.
    pub fn from_bytes(data: &[u8], limits: NbtLimits) -> Result<(Self, Compression)> {
        let compression = Compression::detect(data);
        let raw = compression.decompress(data, limits.max_bytes)?;
        let named = Self::read(&mut &raw[..], limits)?;
        Ok((named, compression))
    }

    pub fn to_bytes(&self, compression: Compression) -> Result<Vec<u8>> {
        let mut raw = Vec::new();
        self.write(&mut raw)?;
        compression.compress(raw)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Compression {
    None,
    Gzip,
    Zlib,
}

impl Compression {
    /// Uncompressed NBT starts with a tag id (0..=12), which never collides
    /// with the gzip magic or a zlib header whose CMF byte is above 12.
    pub fn detect(data: &[u8]) -> Self {
        match data {
            [0x1F, 0x8B, ..] => Self::Gzip,
            [cmf @ 0x0D..=0xFF, flg, ..]
                if cmf & 0x0F == 8 && ((u16::from(*cmf) << 8) | u16::from(*flg)) % 31 == 0 =>
            {
                Self::Zlib
            }
            _ => Self::None,
        }
    }

    pub fn decompress(self, data: &[u8], max_len: usize) -> Result<Cow<'_, [u8]>> {
        let limit = u64::try_from(max_len).unwrap_or(u64::MAX).saturating_add(1);
        let mut out = Vec::new();
        match self {
            Self::None => return Ok(Cow::Borrowed(data)),
            Self::Gzip => MultiGzDecoder::new(data)
                .take(limit)
                .read_to_end(&mut out)?,
            Self::Zlib => ZlibDecoder::new(data).take(limit).read_to_end(&mut out)?,
        };
        if out.len() > max_len {
            return Err(NbtError::SizeLimit);
        }
        Ok(Cow::Owned(out))
    }

    pub fn compress(self, raw: Vec<u8>) -> Result<Vec<u8>> {
        Ok(match self {
            Self::None => raw,
            Self::Gzip => {
                let mut encoder = GzEncoder::new(Vec::new(), Level::default());
                encoder.write_all(&raw)?;
                encoder.finish()?
            }
            Self::Zlib => {
                let mut out = Vec::new();
                zlib_compress_append(&raw, &mut out)?;
                out
            }
        })
    }
}

const ZLIB_WINDOW: usize = 32 * 1024;

thread_local! {
    static ZLIB: std::cell::RefCell<(flate2::Compress, Vec<u8>)> = std::cell::RefCell::new((
        flate2::Compress::new(Level::default(), true),
        Vec::with_capacity(ZLIB_WINDOW),
    ));
}

/// Appends the zlib stream of `raw` (default level) to `out`, reusing this
/// thread's deflate state. The state is driven exactly as a fresh streaming
/// encoder is (no-flush calls into a 32 KiB output window, then finish calls
/// with empty input), so the bytes equal a fresh encoder's output.
pub fn zlib_compress_append(raw: &[u8], out: &mut Vec<u8>) -> Result<()> {
    ZLIB.with(|cell| {
        let (state, stage) = &mut *cell.borrow_mut();
        state.reset();
        out.reserve(raw.len() + (raw.len() >> 9) + 64);
        let mut input = raw;
        while !input.is_empty() {
            let before_in = state.total_in();
            stage.clear();
            state
                .compress_vec(input, stage, flate2::FlushCompress::None)
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            let used = (state.total_in() - before_in) as usize;
            if used == 0 && stage.is_empty() {
                return Err(std::io::Error::other("deflate made no progress").into());
            }
            out.extend_from_slice(stage);
            input = &input[used..];
        }
        loop {
            let before_out = state.total_out();
            stage.clear();
            state
                .compress_vec(&[], stage, flate2::FlushCompress::Finish)
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            out.extend_from_slice(stage);
            if state.total_out() == before_out {
                return Ok(());
            }
        }
    })
}

struct Decoder<'a> {
    input: &'a [u8],
    budget: usize,
    max_depth: usize,
}

impl<'a> Decoder<'a> {
    fn new(input: &'a [u8], limits: NbtLimits) -> Self {
        Self {
            input,
            budget: limits.max_bytes,
            max_depth: limits.max_depth,
        }
    }

    fn charge(&mut self, n: usize) -> Result<()> {
        self.budget = self.budget.checked_sub(n).ok_or(NbtError::SizeLimit)?;
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let (head, tail) = self
            .input
            .split_at_checked(n)
            .ok_or(NbtError::UnexpectedEof)?;
        self.input = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let (head, tail) = self
            .input
            .split_first_chunk::<N>()
            .ok_or(NbtError::UnexpectedEof)?;
        self.input = tail;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }

    fn length(&mut self) -> Result<usize> {
        let len = i32::from_be_bytes(self.array()?);
        usize::try_from(len).map_err(|_| NbtError::NegativeLength(len))
    }

    fn string(&mut self) -> Result<String> {
        let len = usize::from(u16::from_be_bytes(self.array()?));
        let bytes = self.take(len)?;
        self.charge(len)?;
        decode_mutf8(bytes)
    }

    fn array_of<T, const N: usize>(&mut self, convert: fn([u8; N]) -> T) -> Result<Vec<T>> {
        let len = self.length()?;
        let bytes = self.take(len.checked_mul(N).ok_or(NbtError::UnexpectedEof)?)?;
        self.charge(bytes.len())?;
        Ok(bytes
            .as_chunks::<N>()
            .0
            .iter()
            .map(|c| convert(*c))
            .collect())
    }

    /// Validates and steps over a value of type `ty` without building it,
    /// holding the same depth bound as [`Decoder::tag`].
    fn skip(&mut self, ty: TagType, depth: usize) -> Result<()> {
        enum Frame {
            List { element: TagType, remaining: usize },
            Compound,
        }

        let mut stack: Vec<Frame> = Vec::new();
        let mut pending = Some(ty);
        loop {
            if let Some(ty) = pending.take() {
                match ty {
                    TagType::End => return Err(NbtError::UnexpectedEnd),
                    TagType::Byte => drop(self.take(1)?),
                    TagType::Short => drop(self.take(2)?),
                    TagType::Int | TagType::Float => drop(self.take(4)?),
                    TagType::Long | TagType::Double => drop(self.take(8)?),
                    TagType::String => {
                        let len = usize::from(u16::from_be_bytes(self.array()?));
                        self.take(len)?;
                    }
                    TagType::ByteArray | TagType::IntArray | TagType::LongArray => {
                        let width = match ty {
                            TagType::ByteArray => 1,
                            TagType::IntArray => 4,
                            _ => 8,
                        };
                        let len = self.length()?;
                        self.take(len.checked_mul(width).ok_or(NbtError::UnexpectedEof)?)?;
                    }
                    TagType::List => {
                        if depth + stack.len() >= self.max_depth {
                            return Err(NbtError::DepthLimit(self.max_depth));
                        }
                        let element = TagType::from_id(self.u8()?)?;
                        let remaining = self.length()?;
                        if element == TagType::End && remaining > 0 {
                            return Err(NbtError::UntypedList);
                        }
                        if remaining.saturating_mul(element.min_payload_size()) > self.input.len() {
                            return Err(NbtError::UnexpectedEof);
                        }
                        stack.push(Frame::List { element, remaining });
                        continue;
                    }
                    TagType::Compound => {
                        if depth + stack.len() >= self.max_depth {
                            return Err(NbtError::DepthLimit(self.max_depth));
                        }
                        stack.push(Frame::Compound);
                        continue;
                    }
                }
            } else {
                match stack.last_mut() {
                    None => unreachable!("loop always holds pending or a frame"),
                    Some(Frame::List { element, remaining }) if *remaining > 0 => {
                        *remaining -= 1;
                        pending = Some(*element);
                        continue;
                    }
                    Some(Frame::List { .. }) => {
                        stack.pop();
                    }
                    Some(Frame::Compound) => {
                        let ty = TagType::from_id(self.u8()?)?;
                        if ty == TagType::End {
                            stack.pop();
                        } else {
                            let len = usize::from(u16::from_be_bytes(self.array()?));
                            self.take(len)?;
                            pending = Some(ty);
                            continue;
                        }
                    }
                }
            }
            if stack.is_empty() {
                return Ok(());
            }
        }
    }

    /// Decodes a value of type `ty` using an explicit heap-allocated stack
    /// rather than native call-stack recursion. Nesting is attacker
    /// controlled (network packets, chunk/world data), so recursing per
    /// level risks a stack overflow well before `max_depth` bytes of input
    /// are even consumed; `max_depth` alone does not bound native stack use
    /// once the frame size for `List`/`Compound` decoding is large enough,
    /// so depth is instead bounded by `stack.len()` against `max_depth`
    /// while the actual work happens in a loop.
    fn tag(&mut self, ty: TagType, _depth: usize) -> Result<Tag> {
        enum Frame {
            List {
                element: TagType,
                remaining: usize,
                items: Vec<Tag>,
            },
            Compound {
                map: IndexMap<String, Tag>,
                key: Option<String>,
            },
        }

        let mut stack: Vec<Frame> = Vec::new();
        let mut pending = Some(ty);

        loop {
            let finished = if let Some(ty) = pending.take() {
                self.charge(size_of::<Tag>())?;
                match ty {
                    TagType::End => return Err(NbtError::UnexpectedEnd),
                    TagType::Byte => Tag::Byte(i8::from_be_bytes(self.array()?)),
                    TagType::Short => Tag::Short(i16::from_be_bytes(self.array()?)),
                    TagType::Int => Tag::Int(i32::from_be_bytes(self.array()?)),
                    TagType::Long => Tag::Long(i64::from_be_bytes(self.array()?)),
                    TagType::Float => Tag::Float(f32::from_be_bytes(self.array()?)),
                    TagType::Double => Tag::Double(f64::from_be_bytes(self.array()?)),
                    TagType::ByteArray => Tag::ByteArray(self.array_of(i8::from_be_bytes)?),
                    TagType::String => Tag::String(self.string()?),
                    TagType::IntArray => Tag::IntArray(self.array_of(i32::from_be_bytes)?),
                    TagType::LongArray => Tag::LongArray(self.array_of(i64::from_be_bytes)?),
                    TagType::List => {
                        if stack.len() >= self.max_depth {
                            return Err(NbtError::DepthLimit(self.max_depth));
                        }
                        let element = TagType::from_id(self.u8()?)?;
                        let len = self.length()?;
                        if element == TagType::End && len > 0 {
                            return Err(NbtError::UntypedList);
                        }
                        if len.saturating_mul(element.min_payload_size()) > self.input.len() {
                            return Err(NbtError::UnexpectedEof);
                        }
                        stack.push(Frame::List {
                            element,
                            remaining: len,
                            items: Vec::new(),
                        });
                        continue;
                    }
                    TagType::Compound => {
                        if stack.len() >= self.max_depth {
                            return Err(NbtError::DepthLimit(self.max_depth));
                        }
                        stack.push(Frame::Compound {
                            map: IndexMap::new(),
                            key: None,
                        });
                        continue;
                    }
                }
            } else {
                // No primitive/container decode is pending: attach the most
                // recently finished value (if any) to its parent frame, then
                // decide what the current top frame needs next.
                match stack.last_mut() {
                    None => unreachable!("loop always holds pending or a frame"),
                    Some(Frame::List { remaining, .. }) if *remaining > 0 => {
                        let Some(Frame::List {
                            element, remaining, ..
                        }) = stack.last_mut()
                        else {
                            unreachable!()
                        };
                        *remaining -= 1;
                        pending = Some(*element);
                        continue;
                    }
                    Some(Frame::List { .. }) => {
                        let Some(Frame::List { items, .. }) = stack.pop() else {
                            unreachable!()
                        };
                        Tag::List(items)
                    }
                    Some(Frame::Compound { key: Some(_), .. }) => {
                        unreachable!("resolved key implies a finished value was pending")
                    }
                    Some(Frame::Compound { key: None, .. }) => {
                        let ty = TagType::from_id(self.u8()?)?;
                        if ty == TagType::End {
                            let Some(Frame::Compound { map, .. }) = stack.pop() else {
                                unreachable!()
                            };
                            Tag::Compound(Compound(map))
                        } else {
                            let read_key = self.string()?;
                            let Some(Frame::Compound { key, .. }) = stack.last_mut() else {
                                unreachable!()
                            };
                            *key = Some(read_key);
                            pending = Some(ty);
                            continue;
                        }
                    }
                }
            };

            match stack.last_mut() {
                None => return Ok(finished),
                Some(Frame::List { items, .. }) => items.push(match finished {
                    Tag::Compound(mut c) if c.is_wrapper() => {
                        c.0.swap_remove("").unwrap_or(Tag::Compound(c))
                    }
                    other => other,
                }),
                Some(Frame::Compound { map, key }) => {
                    map.insert(
                        key.take().expect("pending key before resolving value"),
                        finished,
                    );
                }
            }
        }
    }
}

fn decode_mutf8(bytes: &[u8]) -> Result<String> {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return Ok(s.to_owned());
    }
    let mut units = Vec::with_capacity(bytes.len());
    let mut rest = bytes;
    while let Some((&b0, tail)) = rest.split_first() {
        let (unit, width) = match (b0, tail) {
            (0x00..=0x7F, _) => (u16::from(b0), 1),
            (0xC0..=0xDF, [b1, ..]) if b1 & 0xC0 == 0x80 => {
                ((u16::from(b0 & 0x1F) << 6) | u16::from(b1 & 0x3F), 2)
            }
            (0xE0..=0xEF, [b1, b2, ..]) if b1 & 0xC0 == 0x80 && b2 & 0xC0 == 0x80 => (
                (u16::from(b0 & 0x0F) << 12) | (u16::from(b1 & 0x3F) << 6) | u16::from(b2 & 0x3F),
                3,
            ),
            _ => return Err(NbtError::InvalidString),
        };
        units.push(unit);
        rest = &rest[width..];
    }
    Ok(String::from_utf16_lossy(&units))
}

fn write_string(s: &str, out: &mut impl BufMut) -> Result<()> {
    let encoded: Cow<'_, [u8]> = if s.bytes().all(|b| b != 0 && b < 0xF0) {
        Cow::Borrowed(s.as_bytes())
    } else {
        let mut buf = Vec::with_capacity(s.len() + 4);
        for unit in s.encode_utf16() {
            match unit {
                0x01..=0x7F => buf.push(unit as u8),
                0x00 | 0x80..=0x7FF => {
                    buf.extend([0xC0 | (unit >> 6) as u8, 0x80 | (unit & 0x3F) as u8])
                }
                _ => buf.extend([
                    0xE0 | (unit >> 12) as u8,
                    0x80 | ((unit >> 6) & 0x3F) as u8,
                    0x80 | (unit & 0x3F) as u8,
                ]),
            }
        }
        Cow::Owned(buf)
    };
    let len = u16::try_from(encoded.len()).map_err(|_| NbtError::StringTooLong(encoded.len()))?;
    out.put_u16(len);
    out.put_slice(&encoded);
    Ok(())
}

fn write_len(len: usize, out: &mut impl BufMut) -> Result<()> {
    out.put_i32(i32::try_from(len).map_err(|_| NbtError::LengthOverflow(len))?);
    Ok(())
}

fn write_compound(compound: &Compound, out: &mut impl BufMut) -> Result<()> {
    for (key, value) in &compound.0 {
        out.put_u8(value.tag_type().id());
        write_string(key, out)?;
        write_payload(value, out)?;
    }
    out.put_u8(TagType::End.id());
    Ok(())
}

fn needs_wrapping(tag: &Tag) -> bool {
    !matches!(tag, Tag::Compound(c) if !c.is_wrapper())
}

fn write_payload(tag: &Tag, out: &mut impl BufMut) -> Result<()> {
    match tag {
        Tag::Byte(v) => out.put_i8(*v),
        Tag::Short(v) => out.put_i16(*v),
        Tag::Int(v) => out.put_i32(*v),
        Tag::Long(v) => out.put_i64(*v),
        Tag::Float(v) => out.put_f32(*v),
        Tag::Double(v) => out.put_f64(*v),
        Tag::ByteArray(v) => {
            write_len(v.len(), out)?;
            v.iter().for_each(|&b| out.put_i8(b));
        }
        Tag::String(s) => write_string(s, out)?,
        Tag::List(items) => {
            let element = items.first().map_or(TagType::End, Tag::tag_type);
            let wrap = items.iter().any(|t| {
                t.tag_type() != element || (element == TagType::Compound && needs_wrapping(t))
            });
            out.put_u8(if wrap { TagType::Compound } else { element }.id());
            write_len(items.len(), out)?;
            for item in items {
                match item {
                    Tag::Compound(c) if wrap && !needs_wrapping(item) => write_compound(c, out)?,
                    _ if wrap => {
                        out.put_u8(item.tag_type().id());
                        out.put_u16(0);
                        write_payload(item, out)?;
                        out.put_u8(TagType::End.id());
                    }
                    _ => write_payload(item, out)?,
                }
            }
        }
        Tag::Compound(c) => write_compound(c, out)?,
        Tag::IntArray(v) => {
            write_len(v.len(), out)?;
            v.iter().for_each(|&x| out.put_i32(x));
        }
        Tag::LongArray(v) => {
            write_len(v.len(), out)?;
            v.iter().for_each(|&x| out.put_i64(x));
        }
    }
    Ok(())
}

mod snbt;
use snbt::*;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
