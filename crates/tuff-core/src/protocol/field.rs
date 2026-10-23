//! Field encodings used by `#[derive(Packet)]`.
//!
//! Types with exactly one wire encoding implement [`WriteField`] and
//! [`ReadField`]. Integers that may be sent either fixed-width or as a
//! VarInt/VarLong deliberately implement neither, so every such field must
//! name its codec (`#[packet(var_int)]`, `#[packet(int)]`, ...). Types whose
//! length comes from the peer ([`Bytes`]) only implement [`ReadBounded`], so
//! a field of that type cannot be decoded without `#[packet(max = N)]`.

use bytes::Bytes;

use crate::protocol::codec::{ReadExt, WriteExt};
use tuff_item::text::TextComponent;
use tuff_wire::{PacketReader, PacketWriter, Result};
use tuff_world_model::BlockPos;

pub trait WriteField {
    fn write(&self, w: &mut PacketWriter);
}

pub trait ReadField: Sized {
    fn read(r: &mut PacketReader) -> Result<Self>;
}

/// Decoding with a caller-supplied upper bound on the length prefix.
pub trait ReadBounded: Sized {
    fn read_bounded(r: &mut PacketReader, max: usize) -> Result<Self>;
}

impl<T: WriteField + ?Sized> WriteField for &T {
    fn write(&self, w: &mut PacketWriter) {
        (**self).write(w);
    }
}

macro_rules! primitive {
    ($($ty:ty => $write:ident, $read:ident;)*) => {$(
        impl WriteField for $ty {
            fn write(&self, w: &mut PacketWriter) {
                w.$write(*self);
            }
        }

        impl ReadField for $ty {
            fn read(r: &mut PacketReader) -> Result<Self> {
                r.$read()
            }
        }
    )*};
}

primitive! {
    bool => write_bool, read_bool;
    i8 => write_byte, read_byte;
    u8 => write_unsigned_byte, read_unsigned_byte;
    i16 => write_short, read_short;
    u16 => write_unsigned_short, read_unsigned_short;
    f32 => write_float, read_float;
    f64 => write_double, read_double;
    u128 => write_uuid, read_uuid;
    BlockPos => write_position, read_position;
}

impl<T: WriteField, const N: usize> WriteField for [T; N] {
    fn write(&self, w: &mut PacketWriter) {
        for value in self {
            value.write(w);
        }
    }
}

impl<T: ReadField + Default + Copy, const N: usize> ReadField for [T; N] {
    fn read(r: &mut PacketReader) -> Result<Self> {
        let mut out = [T::default(); N];
        for value in &mut out {
            *value = T::read(r)?;
        }
        Ok(out)
    }
}

impl WriteField for str {
    fn write(&self, w: &mut PacketWriter) {
        w.write_string(self);
    }
}

impl WriteField for String {
    fn write(&self, w: &mut PacketWriter) {
        w.write_string(self);
    }
}

impl ReadField for String {
    fn read(r: &mut PacketReader) -> Result<Self> {
        r.read_string()
    }
}

impl ReadBounded for String {
    fn read_bounded(r: &mut PacketReader, max: usize) -> Result<Self> {
        r.read_string_chars(max)
    }
}

impl WriteField for Bytes {
    fn write(&self, w: &mut PacketWriter) {
        w.write_prefixed_bytes(self);
    }
}

impl ReadBounded for Bytes {
    fn read_bounded(r: &mut PacketReader, max: usize) -> Result<Self> {
        r.read_prefixed_bytes(max)
    }
}

impl WriteField for TextComponent {
    fn write(&self, w: &mut PacketWriter) {
        w.write_text(self);
    }
}

pub mod var_int {
    use super::*;

    pub fn write(w: &mut PacketWriter, value: &i32) {
        w.write_var_int(*value);
    }

    pub fn read(r: &mut PacketReader) -> Result<i32> {
        r.read_var_int()
    }
}

pub mod int {
    use super::*;

    pub fn write(w: &mut PacketWriter, value: &i32) {
        w.write_int(*value);
    }

    pub fn read(r: &mut PacketReader) -> Result<i32> {
        r.read_int()
    }
}

pub mod var_long {
    use super::*;

    pub fn write(w: &mut PacketWriter, value: &i64) {
        w.write_var_long(*value);
    }

    pub fn read(r: &mut PacketReader) -> Result<i64> {
        r.read_var_long()
    }
}

pub mod long {
    use super::*;

    pub fn write(w: &mut PacketWriter, value: &i64) {
        w.write_long(*value);
    }

    pub fn read(r: &mut PacketReader) -> Result<i64> {
        r.read_long()
    }
}

/// A rotation in degrees, sent as 1/256ths of a full turn.
pub mod angle {
    use super::*;

    pub fn write(w: &mut PacketWriter, value: &f32) {
        w.write_angle(*value);
    }

    pub fn read(r: &mut PacketReader) -> Result<f32> {
        Ok(r.read_unsigned_byte()? as f32 * 360.0 / 256.0)
    }
}

pub mod lp_vec3 {
    use super::*;

    pub fn write(w: &mut PacketWriter, value: &[f64; 3]) {
        w.write_lp_vec3(*value);
    }

    pub fn read(r: &mut PacketReader) -> Result<[f64; 3]> {
        r.read_lp_vec3()
    }
}

/// A Slot. Encode-only: decoding a client's slot goes through
/// [`ReadExt::read_creative_slot`] or `read_hashed_slot`.
pub mod slot {
    use super::*;
    use tuff_item::item::AnyItem;

    pub fn write<'a, V>(w: &mut PacketWriter, value: &'a Option<V>)
    where
        &'a V: Into<AnyItem<'a>>,
    {
        w.write_any_slot(value.as_ref().map(Into::into));
    }
}

/// Unprefixed bytes filling the rest of the packet.
pub mod rest {
    use super::*;

    pub fn write(w: &mut PacketWriter, value: &[u8]) {
        w.write_bytes(value);
    }

    pub fn read(r: &mut PacketReader) -> Result<Bytes> {
        r.read_bytes(r.remaining())
    }
}

#[cfg(test)]
mod tests {
    use tuff_vanilla_data::packet::status::{Clientbound, Serverbound};

    use super::*;
    use crate::protocol::{Encode, Packet, encode_packet};
    use tuff_wire::Buffer;

    #[derive(Debug, Clone, PartialEq, Packet)]
    #[packet(id = Serverbound::PingRequest)]
    struct Everything {
        #[packet(var_int)]
        var_int: i32,
        #[packet(int)]
        int: i32,
        #[packet(var_long)]
        var_long: i64,
        #[packet(long)]
        long: i64,
        #[packet(angle)]
        angle: f32,
        #[packet(lp_vec3)]
        velocity: [f64; 3],
        position: [f64; 3],
        block: BlockPos,
        uuid: u128,
        flag: bool,
        byte: i8,
        #[packet(max = 8)]
        name: String,
        #[packet(max = 4)]
        key: Bytes,
        #[packet(rest)]
        rest: Bytes,
    }

    #[derive(Debug, PartialEq, Packet)]
    #[packet(id = Serverbound::StatusRequest)]
    struct Tuple(#[packet(var_int)] i32, String);

    #[derive(Debug, PartialEq, Packet)]
    #[packet(id = Clientbound::PongResponse)]
    struct Unit;

    #[derive(Packet)]
    #[packet(id = Clientbound::StatusResponse, encode_only)]
    struct Borrowed<'a> {
        text: &'a TextComponent,
        name: &'a str,
        #[packet(rest)]
        data: &'a [u8],
    }

    fn everything() -> Everything {
        Everything {
            var_int: 300,
            int: -7,
            var_long: 1 << 40,
            long: -1,
            angle: 90.0,
            velocity: [0.5, -0.25, 0.0],
            position: [1.5, 64.0, -3.25],
            block: BlockPos::new(-5, 70, 12),
            uuid: 0x069a79f444e94726a5befca90e38aaf5,
            flag: true,
            byte: -3,
            name: "Notch".into(),
            key: Bytes::from_static(&[1, 2, 3]),
            rest: Bytes::from_static(&[9, 8, 7, 6]),
        }
    }

    fn reader(w: PacketWriter) -> PacketReader {
        let mut buf = Buffer::new();
        buf.extend(&w.finish()).unwrap();
        PacketReader::new(buf.try_split_packet().unwrap().unwrap())
    }

    fn body(w: PacketWriter) -> Vec<u8> {
        let mut r = reader(w);
        r.read_bytes(r.remaining()).unwrap().to_vec()
    }

    fn decode<P: Packet>(w: PacketWriter) -> Result<P> {
        let mut r = reader(w);
        assert_eq!(r.read_var_int().unwrap(), P::ID);
        let packet = P::decode(&mut r)?;
        assert_eq!(r.remaining(), 0);
        Ok(packet)
    }

    #[tuff_core::watchdog]
    #[test]
    fn derived_encode_writes_fields_in_order() {
        let p = everything();
        let mut expected = PacketWriter::new();
        expected.write_var_int(Serverbound::PingRequest.id());
        expected.write_var_int(p.var_int);
        expected.write_int(p.int);
        expected.write_var_long(p.var_long);
        expected.write_long(p.long);
        expected.write_angle(p.angle);
        expected.write_lp_vec3(p.velocity);
        for c in p.position {
            expected.write_double(c);
        }
        expected.write_position(p.block);
        expected.write_uuid(p.uuid);
        expected.write_bool(p.flag);
        expected.write_byte(p.byte);
        expected.write_string(&p.name);
        expected.write_prefixed_bytes(&p.key);
        expected.write_bytes(&p.rest);
        assert_eq!(body(encode_packet(&p)), body(expected));
    }

    #[tuff_core::watchdog]
    #[test]
    fn derived_packets_roundtrip() {
        let p = Everything {
            velocity: [0.0; 3],
            ..everything()
        };
        assert_eq!(decode::<Everything>(encode_packet(&p)).unwrap(), p);
        let t = Tuple(-1, "x".into());
        assert_eq!(decode::<Tuple>(encode_packet(&t)).unwrap(), t);
        assert_eq!(decode::<Unit>(encode_packet(&Unit)).unwrap(), Unit);
    }

    #[tuff_core::watchdog]
    #[test]
    fn derived_encode_only_accepts_borrowed_fields() {
        let text = TextComponent::new("hi");
        let p = Borrowed {
            text: &text,
            name: "a",
            data: &[1, 2],
        };
        let mut expected = PacketWriter::new();
        expected.write_var_int(<Borrowed as Encode>::ID);
        expected.write_text(&text);
        expected.write_string("a");
        expected.write_bytes(&[1, 2]);
        assert_eq!(body(encode_packet(&p)), body(expected));
    }

    #[tuff_core::watchdog]
    #[test]
    fn derived_decode_enforces_max() {
        let long_name = Everything {
            name: "x".repeat(9),
            ..everything()
        };
        assert!(decode::<Everything>(encode_packet(&long_name)).is_err());
        let long_key = Everything {
            key: Bytes::from_static(&[0; 5]),
            ..everything()
        };
        assert!(decode::<Everything>(encode_packet(&long_key)).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn derived_decode_rejects_truncated_input() {
        let mut w = PacketWriter::new();
        w.write_var_int(Serverbound::StatusRequest.id());
        w.write_var_int(5);
        assert!(decode::<Tuple>(w).is_err());
    }
}
