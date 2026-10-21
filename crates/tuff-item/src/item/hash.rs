//! The client's view of component values in Click Container: instead of the
//! value, it sends a CRC32C over a tagged byte encoding of the value's
//! serialized form, and the server compares that with its own stacks.

use tuff_nbt::Tag;

const CRC32C_POLY: u32 = 0x82F6_3B78;
const TAG_MAP_START: u8 = 2;
const TAG_MAP_END: u8 = 3;
const TAG_LIST_START: u8 = 4;
const TAG_LIST_END: u8 = 5;
const TAG_BYTE: u8 = 6;
const TAG_SHORT: u8 = 7;
const TAG_INT: u8 = 8;
const TAG_LONG: u8 = 9;
const TAG_FLOAT: u8 = 10;
const TAG_DOUBLE: u8 = 11;
const TAG_STRING: u8 = 12;
const TAG_BOOLEAN: u8 = 13;
const TAG_BYTE_ARRAY_START: u8 = 14;
const TAG_BYTE_ARRAY_END: u8 = 15;
const TAG_INT_ARRAY_START: u8 = 16;
const TAG_INT_ARRAY_END: u8 = 17;
const TAG_LONG_ARRAY_START: u8 = 18;
const TAG_LONG_ARRAY_END: u8 = 19;

/// A value in the shape the game's serializers give it: NBT-like, but with
/// a distinct boolean, and maps whose entry order does not matter.
#[derive(Debug, Clone, PartialEq)]
pub enum Canon {
    Bool(bool),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Str(String),
    List(Vec<Canon>),
    Map(Vec<(String, Canon)>),
    Bytes(Vec<i8>),
    Ints(Vec<i32>),
    Longs(Vec<i64>),
}

impl Canon {
    pub fn str(value: impl Into<String>) -> Self {
        Self::Str(value.into())
    }

    /// The form an NBT tag takes when converted to the hashing serializer.
    pub fn from_nbt(tag: &Tag) -> Self {
        match tag {
            Tag::Byte(v) => Self::Byte(*v),
            Tag::Short(v) => Self::Short(*v),
            Tag::Int(v) => Self::Int(*v),
            Tag::Long(v) => Self::Long(*v),
            Tag::Float(v) => Self::Float(*v),
            Tag::Double(v) => Self::Double(*v),
            Tag::ByteArray(v) => Self::Bytes(v.clone()),
            Tag::String(v) => Self::Str(v.clone()),
            Tag::List(items) => Self::List(items.iter().map(Self::from_nbt).collect()),
            Tag::Compound(map) => Self::Map(
                map.iter()
                    .map(|(key, value)| (key.to_owned(), Self::from_nbt(value)))
                    .collect(),
            ),
            Tag::IntArray(v) => Self::Ints(v.clone()),
            Tag::LongArray(v) => Self::Longs(v.clone()),
        }
    }

    /// The checksum the client sends for this value.
    pub fn hash(&self) -> u32 {
        let mut bytes = Vec::new();
        match self {
            Self::Bool(v) => bytes.extend([TAG_BOOLEAN, *v as u8]),
            Self::Byte(v) => bytes.extend([TAG_BYTE, *v as u8]),
            Self::Short(v) => {
                bytes.push(TAG_SHORT);
                bytes.extend(v.to_le_bytes());
            }
            Self::Int(v) => {
                bytes.push(TAG_INT);
                bytes.extend(v.to_le_bytes());
            }
            Self::Long(v) => {
                bytes.push(TAG_LONG);
                bytes.extend(v.to_le_bytes());
            }
            Self::Float(v) => {
                bytes.push(TAG_FLOAT);
                bytes.extend(v.to_bits().to_le_bytes());
            }
            Self::Double(v) => {
                bytes.push(TAG_DOUBLE);
                bytes.extend(v.to_bits().to_le_bytes());
            }
            Self::Str(v) => {
                bytes.push(TAG_STRING);
                let units: Vec<u16> = v.encode_utf16().collect();
                bytes.extend((units.len() as i32).to_le_bytes());
                for unit in units {
                    bytes.extend(unit.to_le_bytes());
                }
            }
            Self::List(items) => {
                bytes.push(TAG_LIST_START);
                for item in items {
                    bytes.extend(item.hash().to_le_bytes());
                }
                bytes.push(TAG_LIST_END);
            }
            Self::Map(entries) => {
                let mut hashed: Vec<(u32, u32)> = entries
                    .iter()
                    .map(|(key, value)| (Self::Str(key.clone()).hash(), value.hash()))
                    .collect();
                hashed.sort_unstable();
                bytes.push(TAG_MAP_START);
                for (key, value) in hashed {
                    bytes.extend(key.to_le_bytes());
                    bytes.extend(value.to_le_bytes());
                }
                bytes.push(TAG_MAP_END);
            }
            Self::Bytes(items) => {
                bytes.push(TAG_BYTE_ARRAY_START);
                bytes.extend(items.iter().map(|b| *b as u8));
                bytes.push(TAG_BYTE_ARRAY_END);
            }
            Self::Ints(items) => {
                bytes.push(TAG_INT_ARRAY_START);
                for item in items {
                    bytes.extend(item.to_le_bytes());
                }
                bytes.push(TAG_INT_ARRAY_END);
            }
            Self::Longs(items) => {
                bytes.push(TAG_LONG_ARRAY_START);
                for item in items {
                    bytes.extend(item.to_le_bytes());
                }
                bytes.push(TAG_LONG_ARRAY_END);
            }
        }
        crc32c(&bytes)
    }
}

const fn crc32c_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                CRC32C_POLY ^ (crc >> 1)
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC32C_TABLE: [u32; 256] = crc32c_table();

pub fn crc32c(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc = CRC32C_TABLE[((crc ^ *byte as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_nbt::Compound;

    #[tuff_core::watchdog]
    #[test]
    fn crc32c_matches_the_standard_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
    }

    /// Expected values were produced by the game's own hashing serializer.
    #[tuff_core::watchdog]
    #[test]
    fn scalar_hashes_match_the_game() {
        let cases: [(Canon, i32); 15] = [
            (Canon::Int(3), -499649379),
            (Canon::Int(0), -2146075228),
            (Canon::Byte(1), 1791337955),
            (Canon::Short(7), 1861815894),
            (Canon::Long(9), -55626367),
            (Canon::Float(1.5), -1709970540),
            (Canon::Double(2.5), 365488765),
            (Canon::str("x"), -1651626295),
            (Canon::str("minecraft:unbreaking"), 1831557335),
            (Canon::str("a\u{e9}\u{1F600}"), 1780698129),
            (Canon::Bool(true), -1019818302),
            (Canon::Bool(false), 828198337),
            (Canon::Map(Vec::new()), -982207288),
            (Canon::List(Vec::new()), -1978007022),
            (Canon::Bytes(vec![1, 2, 3]), -391352252),
        ];
        for (value, expected) in cases {
            assert_eq!(value.hash() as i32, expected, "{value:?}");
        }
        assert_eq!(Canon::Ints(vec![1, -2]).hash() as i32, -83930571);
        assert_eq!(Canon::Longs(vec![5]).hash() as i32, -724072660);
    }

    #[tuff_core::watchdog]
    #[test]
    fn collections_hash_elements_and_sort_map_entries() {
        let list = Canon::List(vec![Canon::Int(1), Canon::Int(2)]);
        assert_eq!(list.hash() as i32, -1373960668);
        let map = Canon::Map(vec![
            ("minecraft:unbreaking".into(), Canon::Int(3)),
            ("minecraft:efficiency".into(), Canon::Int(5)),
        ]);
        assert_eq!(map.hash() as i32, -1704577720);
        let reordered = Canon::Map(vec![
            ("minecraft:efficiency".into(), Canon::Int(5)),
            ("minecraft:unbreaking".into(), Canon::Int(3)),
        ]);
        assert_eq!(reordered.hash(), map.hash());
    }

    #[tuff_core::watchdog]
    #[test]
    fn nbt_converts_like_the_game() {
        let nbt = Compound::new()
            .with("a", 1)
            .with("b", 2i8)
            .with("s", "hi")
            .with("l", Tag::List(vec![Tag::String("q".into())]))
            .with("ia", Tag::IntArray(vec![3, 4]));
        assert_eq!(
            Canon::from_nbt(&Tag::Compound(nbt)).hash() as i32,
            -2075249091
        );
    }
}
