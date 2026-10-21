// SPDX-License-Identifier: MIT
use std::collections::BTreeMap;

use crate::edit::{Clip, UNAVAILABLE};

pub const MAX_COMPRESSED: usize = 8 << 20;
pub const MAX_DECOMPRESSED: usize = 32 << 20;
pub const MAX_VOLUME: u64 = 16_000_000;
pub const MAX_PALETTE: usize = 65_000;
pub const MAX_DEPTH: usize = 24;
const DATA_VERSION: i32 = 4440;

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for b in data {
        crc ^= *b as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

pub fn gzip_store(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
    let mut chunks = data.chunks(65535).peekable();
    if chunks.peek().is_none() {
        out.extend([1, 0, 0, 0xff, 0xff]);
    }
    while let Some(c) = chunks.next() {
        out.push(chunks.peek().is_none() as u8);
        out.extend((c.len() as u16).to_le_bytes());
        out.extend((!(c.len() as u16)).to_le_bytes());
        out.extend(c);
    }
    out.extend(crc32(data).to_le_bytes());
    out.extend((data.len() as u32).to_le_bytes());
    out
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
    count: u32,
}

impl Bits<'_> {
    fn need(&mut self, n: u32) -> Result<(), String> {
        while self.count < n {
            let b = *self.data.get(self.pos).ok_or("truncated data")?;
            self.pos += 1;
            self.bit |= (b as u32) << self.count;
            self.count += 8;
        }
        Ok(())
    }

    fn take(&mut self, n: u32) -> Result<u32, String> {
        if n == 0 {
            return Ok(0);
        }
        self.need(n)?;
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.count -= n;
        Ok(v)
    }
}

struct Huff {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huff {
    fn new(lengths: &[u8]) -> Huff {
        let mut counts = [0u16; 16];
        for l in lengths {
            counts[*l as usize] += 1;
        }
        counts[0] = 0;
        let mut offs = [0u16; 16];
        for i in 1..16 {
            offs[i] = offs[i - 1] + counts[i - 1];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (s, l) in lengths.iter().enumerate() {
            if *l != 0 {
                symbols[offs[*l as usize] as usize] = s as u16;
                offs[*l as usize] += 1;
            }
        }
        Huff { counts, symbols }
    }

    fn decode(&self, bits: &mut Bits) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.take(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return self
                    .symbols
                    .get((index + (code - first)) as usize)
                    .copied()
                    .ok_or_else(|| "bad huffman code".to_owned());
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("bad huffman code".to_owned())
    }
}

const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn inflate(data: &[u8], limit: usize) -> Result<(Vec<u8>, usize), String> {
    let mut bits = Bits {
        data,
        pos: 0,
        bit: 0,
        count: 0,
    };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            0 => {
                bits.bit = 0;
                bits.count = 0;
                let h = data.get(bits.pos..bits.pos + 4).ok_or("truncated data")?;
                let len = u16::from_le_bytes([h[0], h[1]]) as usize;
                if len as u16 != !u16::from_le_bytes([h[2], h[3]]) {
                    return Err("bad stored block".to_owned());
                }
                bits.pos += 4;
                if out.len() + len > limit {
                    return Err("schematic is too large when decompressed".to_owned());
                }
                out.extend(data.get(bits.pos..bits.pos + len).ok_or("truncated data")?);
                bits.pos += len;
            }
            t @ (1 | 2) => {
                let (lit, dist) = if t == 1 {
                    let mut l = [8u8; 288];
                    l[144..256].fill(9);
                    l[256..280].fill(7);
                    (Huff::new(&l), Huff::new(&[5u8; 30]))
                } else {
                    let nlen = bits.take(5)? as usize + 257;
                    let ndist = bits.take(5)? as usize + 1;
                    let ncode = bits.take(4)? as usize + 4;
                    if nlen > 286 || ndist > 30 {
                        return Err("bad huffman table".to_owned());
                    }
                    const ORDER: [usize; 19] = [
                        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                    ];
                    let mut cl = [0u8; 19];
                    for o in ORDER.iter().take(ncode) {
                        cl[*o] = bits.take(3)? as u8;
                    }
                    let clh = Huff::new(&cl);
                    let mut lens = vec![0u8; nlen + ndist];
                    let mut i = 0;
                    while i < nlen + ndist {
                        let s = clh.decode(&mut bits)?;
                        let (v, rep) = match s {
                            0..=15 => (s as u8, 1),
                            16 => {
                                if i == 0 {
                                    return Err("bad huffman table".to_owned());
                                }
                                (lens[i - 1], 3 + bits.take(2)? as usize)
                            }
                            17 => (0, 3 + bits.take(3)? as usize),
                            _ => (0, 11 + bits.take(7)? as usize),
                        };
                        if i + rep > nlen + ndist {
                            return Err("bad huffman table".to_owned());
                        }
                        lens[i..i + rep].fill(v);
                        i += rep;
                    }
                    (Huff::new(&lens[..nlen]), Huff::new(&lens[nlen..]))
                };
                loop {
                    let s = lit.decode(&mut bits)? as usize;
                    if s < 256 {
                        if out.len() >= limit {
                            return Err("schematic is too large when decompressed".to_owned());
                        }
                        out.push(s as u8);
                    } else if s == 256 {
                        break;
                    } else {
                        let s = s - 257;
                        if s >= 29 {
                            return Err("bad length code".to_owned());
                        }
                        let len = LBASE[s] as usize + bits.take(LEXT[s] as u32)? as usize;
                        let d = dist.decode(&mut bits)? as usize;
                        if d >= 30 {
                            return Err("bad distance code".to_owned());
                        }
                        let d = DBASE[d] as usize + bits.take(DEXT[d] as u32)? as usize;
                        if d > out.len() {
                            return Err("bad distance".to_owned());
                        }
                        if out.len() + len > limit {
                            return Err("schematic is too large when decompressed".to_owned());
                        }
                        for _ in 0..len {
                            out.push(out[out.len() - d]);
                        }
                    }
                }
            }
            _ => return Err("bad block type".to_owned()),
        }
        if last {
            return Ok((out, bits.pos));
        }
    }
}

pub fn gunzip(data: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if data.len() > MAX_COMPRESSED {
        return Err("schematic file is too large".to_owned());
    }
    if data.len() < 18 || data[0] != 0x1f || data[1] != 0x8b || data[2] != 8 {
        return Err("not a gzip file".to_owned());
    }
    let flg = data[3];
    let mut p = 10;
    if flg & 4 != 0 {
        let n = u16::from_le_bytes([
            *data.get(p).ok_or("truncated data")?,
            *data.get(p + 1).ok_or("truncated data")?,
        ]);
        p += 2 + n as usize;
    }
    for bit in [8u8, 16] {
        if flg & bit != 0 {
            let z = data
                .get(p..)
                .ok_or("truncated data")?
                .iter()
                .position(|b| *b == 0);
            p += z.ok_or("truncated data")? + 1;
        }
    }
    if flg & 2 != 0 {
        p += 2;
    }
    let body = data.get(p..).ok_or("truncated data")?;
    let (out, used) = inflate(body, limit)?;
    let t = body.get(used..used + 8).ok_or("truncated data")?;
    if u32::from_le_bytes([t[0], t[1], t[2], t[3]]) != crc32(&out) {
        return Err("schematic checksum mismatch".to_owned());
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Bytes(Vec<u8>),
    Str(String),
    List(Vec<Tag>),
    Compound(BTreeMap<String, Tag>),
    Ints(Vec<i32>),
    Longs(Vec<i64>),
}

impl Tag {
    fn get(&self, key: &str) -> Option<&Tag> {
        match self {
            Tag::Compound(m) => m.get(key),
            _ => None,
        }
    }

    fn int(&self) -> Option<i32> {
        match self {
            Tag::Int(v) => Some(*v),
            Tag::Short(v) => Some(*v as i32),
            Tag::Byte(v) => Some(*v as i32),
            _ => None,
        }
    }

    fn dim(&self) -> Option<i32> {
        match self {
            Tag::Short(v) => Some(*v as u16 as i32),
            other => other.int(),
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("truncated data")?;
        let s = self.data.get(self.pos..end).ok_or("truncated data")?;
        self.pos = end;
        Ok(s)
    }

    fn array_len(&mut self, elem: usize) -> Result<usize, String> {
        let n = i32::from_be_bytes(self.bytes(4)?.try_into().unwrap());
        if n < 0 || (n as usize).saturating_mul(elem) > self.data.len() - self.pos {
            return Err("bad array length".to_owned());
        }
        Ok(n as usize)
    }

    fn string(&mut self) -> Result<String, String> {
        let n = u16::from_be_bytes(self.bytes(2)?.try_into().unwrap()) as usize;
        Ok(String::from_utf8_lossy(self.bytes(n)?).into_owned())
    }

    fn tag(&mut self, kind: u8, depth: usize) -> Result<Tag, String> {
        if depth > MAX_DEPTH {
            return Err("schematic is nested too deeply".to_owned());
        }
        Ok(match kind {
            1 => Tag::Byte(self.bytes(1)?[0] as i8),
            2 => Tag::Short(i16::from_be_bytes(self.bytes(2)?.try_into().unwrap())),
            3 => Tag::Int(i32::from_be_bytes(self.bytes(4)?.try_into().unwrap())),
            4 => Tag::Long(i64::from_be_bytes(self.bytes(8)?.try_into().unwrap())),
            5 => Tag::Float(f32::from_be_bytes(self.bytes(4)?.try_into().unwrap())),
            6 => Tag::Double(f64::from_be_bytes(self.bytes(8)?.try_into().unwrap())),
            7 => {
                let n = self.array_len(1)?;
                Tag::Bytes(self.bytes(n)?.to_vec())
            }
            8 => Tag::Str(self.string()?),
            9 => {
                let k = self.bytes(1)?[0];
                let n = self.array_len(1)?;
                if k == 0 && n > 0 {
                    return Err("bad list type".to_owned());
                }
                let mut v = Vec::new();
                for _ in 0..n {
                    v.push(self.tag(k, depth + 1)?);
                }
                Tag::List(v)
            }
            10 => {
                let mut m = BTreeMap::new();
                loop {
                    let k = self.bytes(1)?[0];
                    if k == 0 {
                        break;
                    }
                    let name = self.string()?;
                    m.insert(name, self.tag(k, depth + 1)?);
                }
                Tag::Compound(m)
            }
            11 => {
                let n = self.array_len(4)?;
                let b = self.bytes(n * 4)?;
                Tag::Ints(
                    b.as_chunks::<4>()
                        .0
                        .iter()
                        .map(|c| i32::from_be_bytes(*c))
                        .collect(),
                )
            }
            12 => {
                let n = self.array_len(8)?;
                let b = self.bytes(n * 8)?;
                Tag::Longs(
                    b.as_chunks::<8>()
                        .0
                        .iter()
                        .map(|c| i64::from_be_bytes(*c))
                        .collect(),
                )
            }
            _ => return Err("unknown nbt tag".to_owned()),
        })
    }
}

pub fn parse_nbt(data: &[u8]) -> Result<Tag, String> {
    let mut r = Reader { data, pos: 0 };
    if r.bytes(1)?[0] != 10 {
        return Err("schematic root is not a compound".to_owned());
    }
    r.string()?;
    r.tag(10, 0)
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u16).to_be_bytes());
    out.extend(s.as_bytes());
}

fn put_tag(out: &mut Vec<u8>, name: &str, tag: &Tag) {
    let kind = match tag {
        Tag::Byte(_) => 1,
        Tag::Short(_) => 2,
        Tag::Int(_) => 3,
        Tag::Long(_) => 4,
        Tag::Float(_) => 5,
        Tag::Double(_) => 6,
        Tag::Bytes(_) => 7,
        Tag::Str(_) => 8,
        Tag::List(_) => 9,
        Tag::Compound(_) => 10,
        Tag::Ints(_) => 11,
        Tag::Longs(_) => 12,
    };
    out.push(kind);
    put_str(out, name);
    put_payload(out, tag);
}

fn put_payload(out: &mut Vec<u8>, tag: &Tag) {
    match tag {
        Tag::Byte(v) => out.push(*v as u8),
        Tag::Short(v) => out.extend(v.to_be_bytes()),
        Tag::Int(v) => out.extend(v.to_be_bytes()),
        Tag::Long(v) => out.extend(v.to_be_bytes()),
        Tag::Float(v) => out.extend(v.to_be_bytes()),
        Tag::Double(v) => out.extend(v.to_be_bytes()),
        Tag::Bytes(v) => {
            out.extend((v.len() as i32).to_be_bytes());
            out.extend(v);
        }
        Tag::Str(s) => put_str(out, s),
        Tag::List(v) => {
            out.push(0);
            out.extend((v.len() as i32).to_be_bytes());
            assert!(v.is_empty());
        }
        Tag::Compound(m) => {
            for (k, v) in m {
                put_tag(out, k, v);
            }
            out.push(0);
        }
        Tag::Ints(v) => {
            out.extend((v.len() as i32).to_be_bytes());
            for i in v {
                out.extend(i.to_be_bytes());
            }
        }
        Tag::Longs(v) => {
            out.extend((v.len() as i32).to_be_bytes());
            for i in v {
                out.extend(i.to_be_bytes());
            }
        }
    }
}

fn put_varint(out: &mut Vec<u8>, mut v: u32) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn varints(data: &[u8], count: usize) -> Result<Vec<u32>, String> {
    let mut out = Vec::with_capacity(count.min(data.len()));
    let mut it = data.iter();
    while out.len() < count {
        let (mut v, mut shift) = (0u32, 0);
        loop {
            let b = *it.next().ok_or("block data is shorter than the volume")?;
            v |= ((b & 0x7f) as u32) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 28 {
                return Err("bad varint in block data".to_owned());
            }
        }
        out.push(v);
    }
    Ok(out)
}

pub fn encode(clip: &Clip) -> Vec<u8> {
    let mut palette: Vec<&str> = Vec::new();
    let mut map: BTreeMap<&str, i32> = BTreeMap::new();
    let mut data = Vec::new();
    let air = "minecraft:air";
    for i in &clip.idx {
        let s = clip
            .palette
            .get(*i as usize)
            .map(String::as_str)
            .unwrap_or(air);
        let n = *map.entry(s).or_insert_with(|| {
            palette.push(s);
            palette.len() as i32 - 1
        });
        put_varint(&mut data, n as u32);
    }
    let pal: BTreeMap<String, Tag> = map
        .iter()
        .map(|(k, v)| (k.to_string(), Tag::Int(*v)))
        .collect();
    let blocks = Tag::Compound(BTreeMap::from([
        ("Palette".to_owned(), Tag::Compound(pal)),
        ("Data".to_owned(), Tag::Bytes(data)),
    ]));
    let schem = Tag::Compound(BTreeMap::from([
        ("Version".to_owned(), Tag::Int(3)),
        ("DataVersion".to_owned(), Tag::Int(DATA_VERSION)),
        ("Width".to_owned(), Tag::Short(clip.size[0] as i16)),
        ("Height".to_owned(), Tag::Short(clip.size[1] as i16)),
        ("Length".to_owned(), Tag::Short(clip.size[2] as i16)),
        (
            "Offset".to_owned(),
            Tag::Ints(clip.offset.iter().map(|o| -o).collect()),
        ),
        ("Blocks".to_owned(), blocks),
    ]));
    let mut out = Vec::new();
    put_tag(
        &mut out,
        "",
        &Tag::Compound(BTreeMap::from([("Schematic".to_owned(), schem)])),
    );
    gzip_store(&out)
}

const LEGACY: &[(u8, &str)] = &[
    (0, "minecraft:air"),
    (1, "minecraft:stone"),
    (2, "minecraft:grass_block"),
    (3, "minecraft:dirt"),
    (4, "minecraft:cobblestone"),
    (5, "minecraft:oak_planks"),
    (7, "minecraft:bedrock"),
    (12, "minecraft:sand"),
    (13, "minecraft:gravel"),
    (17, "minecraft:oak_log"),
    (18, "minecraft:oak_leaves"),
    (20, "minecraft:glass"),
    (24, "minecraft:sandstone"),
    (35, "minecraft:white_wool"),
    (41, "minecraft:gold_block"),
    (42, "minecraft:iron_block"),
    (45, "minecraft:bricks"),
    (49, "minecraft:obsidian"),
    (57, "minecraft:diamond_block"),
    (80, "minecraft:snow_block"),
    (98, "minecraft:stone_bricks"),
];

pub fn decode(file: &[u8]) -> Result<Clip, String> {
    let raw = gunzip(file, MAX_DECOMPRESSED)?;
    let root = parse_nbt(&raw)?;
    let root = root.get("Schematic").unwrap_or(&root);
    let dims = ["Width", "Height", "Length"].map(|k| root.get(k).and_then(Tag::dim));
    let [Some(w), Some(h), Some(l)] = dims else {
        return Err("schematic has no size".to_owned());
    };
    let volume = w as u64 * h as u64 * l as u64;
    if w <= 0 || h <= 0 || l <= 0 || volume > MAX_VOLUME {
        return Err(format!(
            "schematic volume is out of range (max {MAX_VOLUME})"
        ));
    }
    let volume = volume as usize;
    let (pal_tag, data) = match root.get("Blocks") {
        Some(b) => (b.get("Palette"), b.get("Data")),
        None => (root.get("Palette"), root.get("BlockData")),
    };
    let mut palette: Vec<String> = Vec::new();
    let idx: Vec<u16> = if let (Some(Tag::Compound(p)), Some(Tag::Bytes(d))) = (pal_tag, data) {
        if p.len() > MAX_PALETTE {
            return Err("schematic palette is too large".to_owned());
        }
        let mut slots: Vec<Option<String>> = vec![None; p.len()];
        for (state, n) in p {
            let n = n.int().filter(|n| *n >= 0 && (*n as usize) < p.len());
            let n = n.ok_or("palette index out of range")?;
            slots[n as usize] = Some(state.clone());
        }
        for s in slots {
            palette.push(s.ok_or("palette has gaps")?);
        }
        let v = varints(d, volume)?;
        v.into_iter()
            .map(|n| {
                if (n as usize) < palette.len() {
                    Ok(n as u16)
                } else {
                    Err("block data references a missing palette entry".to_owned())
                }
            })
            .collect::<Result<_, _>>()?
    } else if let Some(Tag::Bytes(ids)) = root.get("Blocks") {
        if ids.len() < volume {
            return Err("block data is shorter than the volume".to_owned());
        }
        for (_, name) in LEGACY {
            palette.push((*name).to_owned());
        }
        ids[..volume]
            .iter()
            .map(|id| {
                LEGACY
                    .iter()
                    .position(|(k, _)| k == id)
                    .map_or(UNAVAILABLE, |p| p as u16)
            })
            .collect()
    } else {
        return Err("schematic has no block data".to_owned());
    };
    let offset = match root.get("Offset") {
        Some(Tag::Ints(o)) if o.len() == 3 => [-o[0], -o[1], -o[2]],
        _ => [0; 3],
    };
    Ok(Clip {
        palette,
        size: [w, h, l],
        idx,
        offset,
        origin: [0; 3],
        biomes: None,
    })
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && !name.starts_with('.')
}

const INDEX: &str = "schem/index";

fn key(name: &str) -> String {
    format!("schem/f/{name}")
}

pub fn names() -> Vec<String> {
    tuff_plugin_sdk::store::get_bytes(INDEX)
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn set_names(names: &[String]) -> bool {
    tuff_plugin_sdk::store::set_bytes(INDEX, names.join("\n").as_bytes()).is_ok()
}

pub fn save(name: &str, clip: &Clip, force: bool) -> Result<usize, String> {
    if !valid_name(name) {
        return Err("Schematic names use letters, digits, _ - . (max 48)".to_owned());
    }
    let mut all = names();
    let exists = all.iter().any(|n| n == name);
    if exists && !force {
        return Err(format!("Schematic {name} exists; use -f to overwrite"));
    }
    let bytes = encode(clip);
    if tuff_plugin_sdk::store::set_bytes(&key(name), &bytes).is_err() {
        return Err("Could not store the schematic (plugin storage quota?)".to_owned());
    }
    if !exists {
        all.push(name.to_owned());
        all.sort();
        set_names(&all);
    }
    Ok(bytes.len())
}

pub fn load(name: &str) -> Result<Clip, String> {
    let bytes = tuff_plugin_sdk::store::get_bytes(&key(name))
        .ok_or(format!("No schematic named {name}"))?;
    decode(&bytes)
}

pub fn delete(name: &str) -> bool {
    let mut all = names();
    let Some(at) = all.iter().position(|n| n == name) else {
        return false;
    };
    all.remove(at);
    let _ = tuff_plugin_sdk::store::remove_bytes(&key(name));
    set_names(&all);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip() -> Clip {
        Clip {
            palette: vec![
                "minecraft:stone".into(),
                "minecraft:oak_stairs[facing=north,half=top]".into(),
            ],
            size: [2, 2, 2],
            idx: vec![0, 1, 1, 0, 0, 0, UNAVAILABLE, 1],
            offset: [1, 0, 2],
            origin: [0; 3],
            biomes: None,
        }
    }

    #[test]
    fn round_trip() {
        let c = clip();
        let d = decode(&encode(&c)).unwrap();
        assert_eq!(d.size, c.size);
        assert_eq!(d.offset, c.offset);
        let name = |c: &Clip, i: usize| c.palette.get(c.idx[i] as usize).cloned();
        for i in 0..8 {
            let want = name(&c, i).unwrap_or("minecraft:air".into());
            assert_eq!(name(&d, i).unwrap(), want);
        }
    }

    #[test]
    fn loads_worldedit_fixture() {
        let d = decode(include_bytes!("../tests/fixture_v3.schem")).unwrap();
        assert_eq!(d.size, [3, 2, 2]);
        let at = |i: usize| d.palette[d.idx[i] as usize].as_str();
        assert_eq!(at(0), "minecraft:stone");
        assert_eq!(at(1), "minecraft:oak_stairs[facing=east,half=bottom]");
        assert_eq!(at(2), "minecraft:stone");
        assert_eq!(at(3), "minecraft:air");
        assert_eq!(at(11), "minecraft:oak_stairs[facing=east,half=bottom]");
        assert_eq!(d.offset, [1, 0, -1]);
    }

    #[test]
    fn rejects_hostile_input() {
        assert!(decode(&[]).is_err());
        assert!(decode(b"not gzip at all, really").is_err());
        let good = encode(&clip());
        assert!(decode(&good[..good.len() - 9]).is_err());
        let mut bad = good.clone();
        let n = bad.len();
        bad[n - 9] ^= 0xff;
        assert!(decode(&bad).is_err());
        assert!(decode(&vec![0x1f; MAX_COMPRESSED + 1]).is_err());
    }

    #[test]
    fn rejects_bomb_and_depth() {
        let zeros = vec![0u8; 1 << 20];
        let gz = gzip_store(&zeros);
        assert!(gunzip(&gz, 1 << 10).is_err());
        let mut nbt = vec![10, 0, 0];
        for _ in 0..40 {
            nbt.extend([10, 0, 1, b'a']);
        }
        nbt.extend(vec![0; 42]);
        assert!(parse_nbt(&nbt).unwrap_err().contains("deeply"));
    }

    #[test]
    fn rejects_bad_sizes_and_lengths() {
        let mut huge = vec![10, 0, 0, 7, 0, 1, b'a', 0x7f, 0xff, 0xff, 0xff];
        assert!(parse_nbt(&huge).is_err());
        huge.truncate(5);
        assert!(parse_nbt(&huge).is_err());
        let mut m = BTreeMap::new();
        m.insert("Width".to_owned(), Tag::Short(-1));
        m.insert("Height".to_owned(), Tag::Short(1));
        m.insert("Length".to_owned(), Tag::Short(1));
        let mut out = Vec::new();
        put_tag(&mut out, "", &Tag::Compound(m.clone()));
        assert!(decode(&gzip_store(&out)).is_err());
        m.insert("Width".to_owned(), Tag::Short(-2));
        m.insert("Height".to_owned(), Tag::Short(-2));
        m.insert("Length".to_owned(), Tag::Short(-2));
        let mut out = Vec::new();
        put_tag(&mut out, "", &Tag::Compound(m));
        assert!(decode(&gzip_store(&out)).err().unwrap().contains("volume"));
    }

    #[test]
    fn names_are_sandboxed() {
        assert!(valid_name("castle_1.v2"));
        assert!(!valid_name("../x"));
        assert!(!valid_name(".hidden"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(""));
    }
}
