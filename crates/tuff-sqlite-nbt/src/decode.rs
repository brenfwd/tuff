//! Decoding stored NBT blobs and rendering them as JSON, SNBT, single
//! values and block states, with hard bounds on every allocation.

use std::borrow::Cow;
use std::fmt::{self, Write as _};

use tuff_nbt::{Compression, NamedTag, NbtLimits, Tag, lz4};

/// Largest decompressed NBT accepted from one blob.
pub const MAX_NBT_BYTES: usize = 32 << 20;
/// Deepest nesting of lists and compounds accepted.
pub const MAX_DEPTH: usize = 512;
/// Largest JSON or SNBT text produced from one blob.
pub const MAX_TEXT_BYTES: usize = 128 << 20;

const SECTOR_GZIP: u8 = 1;
const SECTOR_ZLIB: u8 = 2;
const SECTOR_NONE: u8 = 3;
const SECTOR_LZ4: u8 = 4;
const SECTOR_EXTERNAL_FLAG: u8 = 128;

/// Decodes a blob from a server file: a region sector (compression id
/// byte then the payload, as in `chunks`, `entities` and `pois`), or a
/// whole NBT file that is gzip, zlib or uncompressed (as in `documents`).
/// Uncompressed NBT starts with a tag id, and a root compound's id (10)
/// never collides with a sector compression id.
pub fn decode(blob: &[u8]) -> Result<Tag, String> {
    let raw: Cow<'_, [u8]> = match blob {
        [] => return Err("empty blob".into()),
        [SECTOR_GZIP, rest @ ..] => inflate(Compression::Gzip, rest)?,
        [SECTOR_ZLIB, rest @ ..] => inflate(Compression::Zlib, rest)?,
        [SECTOR_NONE, rest @ ..] => Cow::Borrowed(rest),
        [SECTOR_LZ4, rest @ ..] => {
            Cow::Owned(lz4::decompress_stream(rest, MAX_NBT_BYTES).map_err(|err| err.to_string())?)
        }
        [id, ..] if id & SECTOR_EXTERNAL_FLAG != 0 && (id & !SECTOR_EXTERNAL_FLAG) <= 4 => {
            return Err("the chunk is stored in an external file".into());
        }
        _ => inflate(Compression::detect(blob), blob)?,
    };
    if raw.len() > MAX_NBT_BYTES {
        return Err(format!("NBT is larger than {MAX_NBT_BYTES} bytes"));
    }
    let limits = NbtLimits {
        max_depth: MAX_DEPTH,
        max_bytes: MAX_NBT_BYTES,
    };
    let mut input = &raw[..];
    let named = NamedTag::read(&mut input, limits).map_err(|err| err.to_string())?;
    Ok(named.tag)
}

fn inflate(compression: Compression, data: &[u8]) -> Result<Cow<'_, [u8]>, String> {
    compression
        .decompress(data, MAX_NBT_BYTES)
        .map_err(|err| err.to_string())
}

struct Capped(String);

impl fmt::Write for Capped {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.0.len() + s.len() > MAX_TEXT_BYTES {
            return Err(fmt::Error);
        }
        self.0.push_str(s);
        Ok(())
    }
}

fn too_large(_: fmt::Error) -> String {
    format!("output is larger than {MAX_TEXT_BYTES} bytes")
}

/// SNBT text, as `/data get` prints it on one line.
pub fn to_snbt(tag: &Tag) -> Result<String, String> {
    let mut out = Capped(String::new());
    write!(out, "{tag}").map_err(too_large)?;
    Ok(out.0)
}

/// JSON: compounds become objects, lists and all arrays become arrays,
/// every integer type (longs too) becomes a plain number, floats and
/// doubles become numbers, with NaN and infinities as the strings `"NaN"`,
/// `"Infinity"` and `"-Infinity"`.
pub fn to_json(tag: &Tag) -> Result<String, String> {
    let mut out = Capped(String::new());
    write_json(tag, &mut out).map_err(too_large)?;
    Ok(out.0)
}

fn write_json(tag: &Tag, out: &mut Capped) -> fmt::Result {
    match tag {
        Tag::Byte(v) => write!(out, "{v}"),
        Tag::Short(v) => write!(out, "{v}"),
        Tag::Int(v) => write!(out, "{v}"),
        Tag::Long(v) => write!(out, "{v}"),
        Tag::Float(v) => write_float(f64::from(*v), &format!("{v:?}"), out),
        Tag::Double(v) => write_float(*v, &format!("{v:?}"), out),
        Tag::ByteArray(a) => write_numbers(a, out),
        Tag::IntArray(a) => write_numbers(a, out),
        Tag::LongArray(a) => write_numbers(a, out),
        Tag::String(s) => write_json_string(s, out),
        Tag::List(items) => {
            out.write_char('[')?;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.write_char(',')?;
                }
                write_json(item, out)?;
            }
            out.write_char(']')
        }
        Tag::Compound(c) => {
            out.write_char('{')?;
            for (i, (key, value)) in c.iter().enumerate() {
                if i > 0 {
                    out.write_char(',')?;
                }
                write_json_string(key, out)?;
                out.write_char(':')?;
                write_json(value, out)?;
            }
            out.write_char('}')
        }
    }
}

fn write_float(v: f64, text: &str, out: &mut Capped) -> fmt::Result {
    if v.is_nan() {
        out.write_str("\"NaN\"")
    } else if v.is_infinite() {
        out.write_str(if v > 0.0 {
            "\"Infinity\""
        } else {
            "\"-Infinity\""
        })
    } else {
        out.write_str(text)
    }
}

fn write_numbers<T: fmt::Display>(items: &[T], out: &mut Capped) -> fmt::Result {
    out.write_char('[')?;
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.write_char(',')?;
        }
        write!(out, "{item}")?;
    }
    out.write_char(']')
}

fn write_json_string(s: &str, out: &mut Capped) -> fmt::Result {
    out.write_char('"')?;
    for c in s.chars() {
        match c {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\n' => out.write_str("\\n")?,
            '\r' => out.write_str("\\r")?,
            '\t' => out.write_str("\\t")?,
            c if u32::from(c) < 0x20 => write!(out, "\\u{:04x}", u32::from(c))?,
            c => out.write_char(c)?,
        }
    }
    out.write_char('"')
}

/// What a path points at: a tag, or one element of a typed array (which
/// has no tag of its own).
#[derive(Debug, PartialEq)]
pub enum Found<'a> {
    Tag(&'a Tag),
    Element(i64),
}

/// Follows a path such as `Pos[1]`, `Inventory[0].id` or
/// `sections[0].block_states.palette[2].Name` (a leading `$` is allowed,
/// keys may be quoted with `"`). Negative indexes count from the end.
pub fn get_path<'a>(tag: &'a Tag, path: &str) -> Result<Option<Found<'a>>, String> {
    let mut rest = path.strip_prefix('$').unwrap_or(path);
    let mut current = tag;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('[') {
            let end = after.find(']').ok_or("unclosed '[' in path")?;
            let index: i64 = after[..end]
                .trim()
                .parse()
                .map_err(|_| format!("bad index '{}' in path", &after[..end]))?;
            rest = &after[end + 1..];
            let element = match current {
                Tag::List(v) => match resolve_index(index, v.len()) {
                    Some(i) => {
                        current = &v[i];
                        continue;
                    }
                    None => None,
                },
                Tag::ByteArray(v) => resolve_index(index, v.len()).map(|i| i64::from(v[i])),
                Tag::IntArray(v) => resolve_index(index, v.len()).map(|i| i64::from(v[i])),
                Tag::LongArray(v) => resolve_index(index, v.len()).map(|i| v[i]),
                _ => None,
            };
            return Ok(element.filter(|_| rest.is_empty()).map(Found::Element));
        }
        let after = rest.strip_prefix('.').unwrap_or(rest);
        let (key, tail) = if let Some(quoted) = after.strip_prefix('"') {
            let end = quoted.find('"').ok_or("unclosed '\"' in path")?;
            (&quoted[..end], &quoted[end + 1..])
        } else {
            let end = after.find(['.', '[']).unwrap_or(after.len());
            (&after[..end], &after[end..])
        };
        if key.is_empty() {
            return Err(format!("empty key in path '{path}'"));
        }
        rest = tail;
        current = match current {
            Tag::Compound(c) => match c.get(key) {
                Some(tag) => tag,
                None => return Ok(None),
            },
            _ => return Ok(None),
        };
    }
    Ok(Some(Found::Tag(current)))
}

fn resolve_index(index: i64, len: usize) -> Option<usize> {
    let len = i64::try_from(len).ok()?;
    let i = if index < 0 { len + index } else { index };
    (0..len).contains(&i).then_some(i as usize)
}

/// The block state at world position `(x, y, z)` in a chunk root, written
/// as `name[key=value,...]`, or `None` when the position is outside the
/// chunk or its section is missing.
pub fn block_at(root: &Tag, x: i64, y: i64, z: i64) -> Result<Option<String>, String> {
    let Tag::Compound(root) = root else {
        return Err("chunk root is not a compound".into());
    };
    let chunk_x = root.get("xPos").and_then(Tag::as_int);
    let chunk_z = root.get("zPos").and_then(Tag::as_int);
    if let (Some(cx), Some(cz)) = (chunk_x, chunk_z)
        && (x.div_euclid(16) != i64::from(cx) || z.div_euclid(16) != i64::from(cz))
    {
        return Ok(None);
    }
    let Some(Tag::List(sections)) = root.get("sections") else {
        return Ok(None);
    };
    let section_y = y.div_euclid(16);
    let Some(section) = sections.iter().find_map(|s| match s {
        Tag::Compound(s) if s.get("Y").and_then(Tag::as_int).map(i64::from) == Some(section_y) => {
            Some(s)
        }
        _ => None,
    }) else {
        return Ok(None);
    };
    let Some(Tag::Compound(states)) = section.get("block_states") else {
        return Ok(None);
    };
    let Some(Tag::List(palette)) = states.get("palette") else {
        return Err("section has no block palette".into());
    };
    let entry = match states.get("data") {
        Some(Tag::LongArray(data)) if palette.len() > 1 => {
            let bits = (usize::BITS - (palette.len() - 1).leading_zeros()).max(4) as usize;
            let per_long = 64 / bits;
            let index = ((y.rem_euclid(16) as usize) << 8)
                | ((z.rem_euclid(16) as usize) << 4)
                | (x.rem_euclid(16) as usize);
            let word = *data
                .get(index / per_long)
                .ok_or("block data is shorter than its palette needs")?;
            let value = ((word as u64) >> ((index % per_long) * bits)) & ((1u64 << bits) - 1);
            palette
                .get(value as usize)
                .ok_or("block data points past its palette")?
        }
        _ => palette.first().ok_or("empty block palette")?,
    };
    let Tag::Compound(entry) = entry else {
        return Err("palette entry is not a compound".into());
    };
    let Some(Tag::String(name)) = entry.get("Name") else {
        return Err("palette entry has no name".into());
    };
    let mut out = name.clone();
    if let Some(Tag::Compound(properties)) = entry.get("Properties")
        && !properties.is_empty()
    {
        out.push('[');
        for (i, (key, value)) in properties.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(key);
            out.push('=');
            match value {
                Tag::String(s) => out.push_str(s),
                other => {
                    let _ = write!(out, "{other}");
                }
            }
        }
        out.push(']');
    }
    Ok(Some(out))
}

/// Hyphenated UUID text from the four ints NBT stores a UUID as.
pub fn uuid_text(ints: [i32; 4]) -> String {
    let v = ints
        .iter()
        .fold(0u128, |acc, &i| (acc << 32) | u128::from(i as u32));
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (v >> 96) as u32,
        (v >> 80) as u16,
        (v >> 64) as u16,
        (v >> 48) as u16,
        v & 0xFFFF_FFFF_FFFF
    )
}

/// Parses the JSON form of a UUID int array (`[a,b,c,d]`).
pub fn uuid_from_json(text: &str) -> Option<String> {
    let inner = text.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut ints = [0i32; 4];
    let mut parts = inner.split(',');
    for slot in &mut ints {
        *slot = parts.next()?.trim().parse().ok()?;
    }
    parts.next().is_none().then(|| uuid_text(ints))
}
