//! Minecraft protocol packet framing, compression, and field-level serialization.
//!
//! This module provides three core types for working with the Minecraft protocol
//! wire format:
//!
//! - [`Buffer`] — per-client receive buffer that accumulates TCP bytes and splits
//!   off complete packets (with optional zlib decompression).
//! - [`PacketReader`] — reads Minecraft protocol fields (VarInt, strings, primitives, etc.)
//!   from a split-off packet payload.
//! - [`PacketWriter`] — builds an outbound packet and frames it with the appropriate
//!   length prefix (with optional zlib compression).
//!
//! # Example
//!
//! Build, frame, and parse a Status Response packet (packet ID `0x00`) during login:
//!
//! ```rust
//! use bytes::Bytes;
//! use tuff_wire::{Buffer, PacketReader, PacketWriter};
//!
//! // --- Writing ---
//! let mut writer = PacketWriter::new();
//! writer.write_var_int(0x00);                     // Packet ID
//! writer.write_string(r#"{"description":"A Minecraft Server"}"#);
//! let framed = writer.finish();                   // no compression yet
//!
//! // --- Reading ---
//! let mut recv_buf = Buffer::new();
//! recv_buf.extend(&framed).unwrap();
//!
//! if let Some(payload) = recv_buf.try_split_packet().unwrap() {
//!     let mut reader = PacketReader::new(payload);
//!     let packet_id = reader.read_var_int().unwrap();
//!     let json      = reader.read_string().unwrap();
//!     assert_eq!(packet_id, 0x00);
//!     assert!(json.contains("description"));
//! }
//! ```

use bytes::{Buf, BufMut, Bytes, BytesMut};
use thiserror::Error;

use tuff_nbt::{NbtError, NbtLimits, Tag};

/// Errors produced by [`Buffer`], [`PacketReader`], and [`PacketWriter`].
#[derive(Error, Debug)]
pub enum BufferError {
    /// A read was attempted that exceeds the remaining bytes in the payload.
    #[error("out-of-bounds read of size {0} with {1} remaining")]
    OutOfBoundsRead(usize, usize),
    /// A VarInt encoding is malformed (too many continuation bytes).
    #[error("invalid varint")]
    InvalidVarInt,
    /// A VarLong encoding is malformed (too many continuation bytes).
    #[error("invalid varlong")]
    InvalidVarLong,
    /// A string field contains invalid UTF-8 or a negative length prefix.
    #[error("invalid string")]
    InvalidString,
    /// A string's byte length exceeds the allowed maximum.
    #[error("string too long: {0} bytes")]
    StringTooLong(usize),
    /// Zlib decompression failed.
    #[error("decompression failed: {0}")]
    DecompressionFailed(String),
    /// Zlib compression failed.
    #[error("compression failed: {0}")]
    CompressionFailed(String),
    /// Accepting new data would push the receive buffer past [`MAX_BUFFER_SIZE`].
    #[error("receive buffer full ({0} bytes)")]
    BufferFull(usize),
    /// The `packet_length` VarInt on the wire exceeds [`MAX_PACKET_SIZE`].
    #[error("packet too large: {0} bytes")]
    PacketTooLarge(usize),
    /// The declared `data_length` (uncompressed size) exceeds [`MAX_UNCOMPRESSED_SIZE`].
    #[error("decompressed size too large: {0} bytes")]
    DecompressedSizeTooLarge(usize),
    /// The actual decompressed output does not match the declared `data_length`.
    #[error("decompressed size does not match declared data_length")]
    DecompressedSizeMismatch,
    /// A non-zero `data_length` that is below the compression threshold violates the protocol.
    #[error("invalid data_length: must be 0 or >= compression threshold")]
    InvalidDataLength,
    /// A boolean field holds a value other than `0x00` or `0x01`.
    #[error("invalid boolean: {0:#04x}")]
    InvalidBool(u8),
    /// A length-prefixed byte array is negative or exceeds the allowed maximum.
    #[error("invalid byte array length: {0}")]
    InvalidArrayLength(i32),
    /// An enum field holds an unknown discriminant.
    #[error("invalid enum value: {0}")]
    InvalidEnum(i32),
    /// An NBT field failed to decode or encode.
    #[error("nbt: {0}")]
    Nbt(#[from] NbtError),
}

/// Convenience alias used throughout this module.
pub type Result<T> = std::result::Result<T, BufferError>;

/// Minecraft protocol maximum uncompressed packet body (2 MiB).
const MAX_UNCOMPRESSED_SIZE: usize = 2 * 1024 * 1024;

/// Maximum raw bytes allowed in the per-client receive buffer.
/// Slightly larger than `MAX_UNCOMPRESSED_SIZE` to accommodate VarInt headers
/// and compression overhead.
const MAX_BUFFER_SIZE: usize = MAX_UNCOMPRESSED_SIZE + (512 * 1024);

/// Maximum `packet_length` accepted from the wire.
const MAX_PACKET_SIZE: usize = MAX_UNCOMPRESSED_SIZE;

/// Peeks a VarInt from a byte slice without consuming anything.
/// Returns `Some((value, bytes_consumed))` if a complete VarInt is present,
/// or `None` if more data is needed.
fn peek_var_int(buf: &[u8]) -> Result<Option<(i32, usize)>> {
    let mut value: i32 = 0;
    let mut position = 0;

    for (i, &current_byte) in buf.iter().enumerate() {
        value |= ((current_byte & 0x7F) as i32) << position;

        if (current_byte & 0x80) == 0 {
            return Ok(Some((value, i + 1)));
        }

        position += 7;
        if position >= 32 {
            return Err(BufferError::InvalidVarInt);
        }
    }

    // Ran out of bytes before the VarInt terminated
    Ok(None)
}

use flate2::read::ZlibDecoder;
use std::io::Read;

// ---------------------------------------------------------------------------
// Buffer — per-client receive buffer for framing incoming packets
// ---------------------------------------------------------------------------

/// Per-client receive buffer that accumulates TCP bytes and splits off
/// complete Minecraft protocol packets.
///
/// Handles both uncompressed and compressed (zlib) framing. Enable
/// compression with [`Buffer::set_compression_threshold`] after receiving
/// the Set Compression packet.
#[derive(Default)]
pub struct Buffer {
    bytes: BytesMut,
    /// `None` = compression disabled (pre-login).
    /// `Some(threshold)` = compression enabled; packets with uncompressed
    /// size >= threshold are zlib-compressed.
    compression_threshold: Option<usize>,
    last_frame_len: usize,
    /// Tighter frame bound for unauthenticated states; `None` = protocol maximum.
    packet_limit: Option<usize>,
}

impl Buffer {
    /// Bounds accepted frames (and so the receive buffer) to `limit` bytes,
    /// or back to the protocol maximum with `None`.
    pub fn set_packet_limit(&mut self, limit: Option<usize>) {
        self.packet_limit = limit;
    }

    /// Creates an empty receive buffer.
    pub fn new() -> Self {
        Default::default()
    }

    /// Creates a receive buffer with the given pre-allocated capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: BytesMut::with_capacity(capacity),
            compression_threshold: None,
            last_frame_len: 0,
            packet_limit: None,
        }
    }

    /// Enable compression with the given threshold (from Set Compression packet).
    pub fn set_compression_threshold(&mut self, threshold: usize) {
        self.compression_threshold = Some(threshold);
    }

    /// Append raw bytes received from the network.
    ///
    /// Returns `Err(BufferFull)` if accepting the data would exceed the
    /// maximum receive-buffer size, preventing memory-exhaustion attacks.
    pub fn extend(&mut self, data: &[u8]) -> Result<()> {
        let new_len = self.bytes.len() + data.len();
        let cap = self
            .packet_limit
            .map_or(MAX_BUFFER_SIZE, |limit| limit.saturating_mul(2) + 64);
        if new_len > cap {
            return Err(BufferError::BufferFull(new_len));
        }
        self.bytes.put_slice(data);
        Ok(())
    }

    /// The framed size (length prefix included, as on the wire before any
    /// decryption) of the packet [`Buffer::try_split_packet`] returned last.
    pub fn last_frame_len(&self) -> usize {
        self.last_frame_len
    }

    /// Returns the number of unprocessed bytes in the buffer.
    pub fn remaining(&self) -> usize {
        self.bytes.remaining()
    }

    /// Try to split off a complete packet from the front of the buffer.
    ///
    /// **Without compression:**
    /// `[VarInt packet_length] [packet_length bytes of (Packet ID + Data)]`
    ///
    /// **With compression:**
    /// `[VarInt packet_length] [VarInt data_length] [compressed or raw payload]`
    /// - `data_length == 0` → payload is uncompressed
    /// - `data_length > 0`  → payload is zlib-compressed; data_length is the
    ///   uncompressed size
    ///
    /// Returns:
    /// - `Ok(Some(payload))` — the decompressed (Packet ID + Data) bytes.
    /// - `Ok(None)` — not enough data yet; the buffer is untouched.
    /// - `Err(...)` — malformed data.
    pub fn try_split_packet(&mut self) -> std::result::Result<Option<Bytes>, BufferError> {
        let buf = &self.bytes[..];

        // 1. Peek the outer Packet Length VarInt
        let Some((packet_len, header_size)) = peek_var_int(buf)? else {
            return Ok(None);
        };

        if packet_len < 0 {
            return Err(BufferError::InvalidVarInt);
        }
        let packet_len = packet_len as usize;

        // Reject packets exceeding the protocol maximum before waiting
        // for the full body, which would allow unbounded buffer growth.
        if packet_len > self.packet_limit.unwrap_or(MAX_PACKET_SIZE) {
            return Err(BufferError::PacketTooLarge(packet_len));
        }

        let total = header_size + packet_len;

        if buf.len() < total {
            return Ok(None);
        }

        self.last_frame_len = total;
        self.bytes.advance(header_size);
        let inner = self.bytes.split_to(packet_len);

        if self.compression_threshold.is_none() {
            // No compression — inner is raw (Packet ID + Data)
            return Ok(Some(inner.freeze()));
        }

        // Compression enabled — inner starts with a Data Length VarInt
        let inner_slice = &inner[..];
        let Some((data_length, dl_size)) = peek_var_int(inner_slice)? else {
            return Err(BufferError::InvalidVarInt);
        };

        let payload_bytes = &inner_slice[dl_size..];

        if data_length == 0 {
            // Uncompressed — the rest is raw (Packet ID + Data)
            Ok(Some(Bytes::copy_from_slice(payload_bytes)))
        } else {
            // Compressed — zlib decompress
            let data_length = data_length as usize;

            // Reject decompression bombs -- cap at protocol maximum.
            if data_length > MAX_UNCOMPRESSED_SIZE {
                return Err(BufferError::DecompressedSizeTooLarge(data_length));
            }

            // Protocol requires data_length >= threshold when non-zero.
            if data_length < self.compression_threshold.unwrap_or(0) {
                return Err(BufferError::InvalidDataLength);
            }

            let mut decompressed = vec![0u8; data_length];
            let mut decoder = ZlibDecoder::new(payload_bytes);
            decoder
                .read_exact(&mut decompressed)
                .map_err(|e| BufferError::DecompressionFailed(e.to_string()))?;

            // Ensure the zlib stream is fully consumed -- a mismatch
            // means the peer lied about the uncompressed size.
            let mut trailing = [0u8; 1];
            if decoder
                .read(&mut trailing)
                .map_err(|e| BufferError::DecompressionFailed(e.to_string()))?
                != 0
            {
                return Err(BufferError::DecompressedSizeMismatch);
            }

            Ok(Some(Bytes::from(decompressed)))
        }
    }
}

/// Reads Minecraft protocol fields from a packet payload returned by
/// [`Buffer::try_split_packet`].
///
/// All multi-byte integers are read in **big-endian** byte order, matching
/// the Minecraft protocol specification.
pub struct PacketReader {
    bytes: Bytes,
    pos: usize,
}

impl PacketReader {
    /// Wraps a packet payload for field-level reading.
    pub fn new(bytes: Bytes) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Returns the number of bytes remaining to be read.
    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn require(&self, n: usize) -> Result<()> {
        let rem = self.remaining();
        if rem < n {
            Err(BufferError::OutOfBoundsRead(n, rem))
        } else {
            Ok(())
        }
    }

    fn take(&mut self, n: usize) -> Result<&[u8]> {
        self.require(n)?;
        let start = self.pos;
        self.pos += n;
        Ok(&self.bytes[start..self.pos])
    }

    /// Reads a signed byte (`i8`).
    pub fn read_byte(&mut self) -> Result<i8> {
        Ok(self.take(1)?[0] as i8)
    }

    /// Reads an unsigned byte (`u8`).
    pub fn read_unsigned_byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    /// Reads a signed 16-bit integer (`i16`, big-endian).
    pub fn read_short(&mut self) -> Result<i16> {
        let b = self.take(2)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }

    /// Reads an unsigned 16-bit integer (`u16`, big-endian).
    pub fn read_unsigned_short(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    /// Reads a signed 32-bit integer (`i32`, big-endian).
    pub fn read_int(&mut self) -> Result<i32> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Reads a signed 64-bit integer (`i64`, big-endian).
    pub fn read_long(&mut self) -> Result<i64> {
        let b = self.take(8)?;
        Ok(i64::from_be_bytes(b.try_into().unwrap()))
    }

    /// Reads a 32-bit floating-point value (`f32`, big-endian).
    pub fn read_float(&mut self) -> Result<f32> {
        let b = self.take(4)?;
        Ok(f32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Reads a 64-bit floating-point value (`f64`, big-endian).
    pub fn read_double(&mut self) -> Result<f64> {
        let b = self.take(8)?;
        Ok(f64::from_be_bytes(b.try_into().unwrap()))
    }

    /// Reads a VarInt-encoded signed 32-bit integer (1–5 bytes).
    pub fn read_var_int(&mut self) -> Result<i32> {
        let mut value: i32 = 0;
        let mut position = 0;

        loop {
            let current_byte = self.read_unsigned_byte()?;
            value |= ((current_byte & 0x7F) as i32) << position;

            if (current_byte & 0x80) == 0 {
                break;
            }

            position += 7;
            if position >= 32 {
                return Err(BufferError::InvalidVarInt);
            }
        }

        Ok(value)
    }

    /// Reads a VarLong-encoded signed 64-bit integer (1–10 bytes).
    pub fn read_var_long(&mut self) -> Result<i64> {
        let mut value: i64 = 0;
        let mut position = 0;

        loop {
            let current_byte = self.read_unsigned_byte()?;
            value |= ((current_byte & 0x7F) as i64) << position;

            if (current_byte & 0x80) == 0 {
                break;
            }

            position += 7;
            if position >= 64 {
                return Err(BufferError::InvalidVarLong);
            }
        }

        Ok(value)
    }

    /// Reads a VarInt-prefixed UTF-8 string using the default protocol
    /// maximum of 32 767 characters × 4 bytes each.
    pub fn read_string(&mut self) -> Result<String> {
        self.read_string_chars(32767)
    }

    /// Reads a VarInt-prefixed UTF-8 string, rejecting any string whose
    /// byte length exceeds `max_bytes`.
    pub fn read_string_with_max(&mut self, max_bytes: usize) -> Result<String> {
        let len = self.read_var_int()?;
        if len < 0 {
            return Err(BufferError::InvalidString);
        }
        let len = len as usize;
        if len > max_bytes {
            return Err(BufferError::StringTooLong(len));
        }
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| BufferError::InvalidString)
    }

    /// Reads a VarInt-prefixed UTF-8 string of at most `max_chars` UTF-16 code
    /// units: the encoded bytes are bounded at three per character first, then
    /// the decoded length is checked.
    pub fn read_string_chars(&mut self, max_chars: usize) -> Result<String> {
        let text = self.read_string_with_max(max_chars.saturating_mul(3))?;
        let units = text.encode_utf16().count();
        if units > max_chars {
            return Err(BufferError::StringTooLong(units));
        }
        Ok(text)
    }

    /// Reads a boolean encoded as a single `0x00` / `0x01` byte.
    pub fn read_bool(&mut self) -> Result<bool> {
        match self.read_unsigned_byte()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(BufferError::InvalidBool(other)),
        }
    }

    /// Reads a UUID encoded as a 128-bit big-endian integer.
    pub fn read_uuid(&mut self) -> Result<u128> {
        let b = self.take(16)?;
        Ok(u128::from_be_bytes(b.try_into().unwrap()))
    }

    /// Reads a VarInt-length-prefixed byte array as a zero-copy [`Bytes`] handle,
    /// rejecting arrays longer than `max_len`.
    pub fn read_prefixed_bytes(&mut self, max_len: usize) -> Result<Bytes> {
        let len = self.read_var_int()?;
        if len < 0 || len as usize > max_len {
            return Err(BufferError::InvalidArrayLength(len));
        }
        self.read_bytes(len as usize)
    }

    /// Reads `n` raw bytes, returning them as a zero-copy [`Bytes`] handle.
    pub fn read_bytes(&mut self, n: usize) -> Result<Bytes> {
        self.require(n)?;
        let start = self.pos;
        self.pos += n;
        Ok(self.bytes.slice(start..self.pos))
    }

    /// Reads a network NBT value under [`NbtLimits::NETWORK`]; `None` for an End tag.
    pub fn read_nbt(&mut self) -> Result<Option<Tag>> {
        let mut input = &self.bytes[self.pos..];
        let before = input.len();
        let tag = Tag::read_network(&mut input, NbtLimits::NETWORK)?;
        self.pos += before - input.len();
        Ok(tag)
    }
}

/// Builds an outbound Minecraft protocol packet and frames it with the
/// appropriate length prefix.
///
/// All multi-byte integers are written in **big-endian** byte order.
/// Call [`PacketWriter::finish`] (pre-compression) or
/// [`PacketWriter::finish_compressed`] (post-compression) to produce
/// the final framed [`Bytes`] ready for the wire.
#[derive(Default)]
pub struct PacketWriter {
    bytes: BytesMut,
}

impl PacketWriter {
    /// Creates an empty packet builder.
    pub fn new() -> Self {
        Default::default()
    }

    /// Number of payload bytes written so far.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether nothing has been written.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The raw written bytes, without any framing.
    pub fn into_bytes(self) -> Bytes {
        self.bytes.freeze()
    }

    /// Finalize the packet **without** compression: prepends the VarInt length
    /// header and returns the complete framed packet ready to send.
    ///
    /// Use this before compression has been negotiated (e.g. during handshake/login).
    pub fn finish(self) -> Bytes {
        let payload = self.bytes;
        let mut framed = BytesMut::with_capacity(5 + payload.len());
        write_var_int_to(&mut framed, payload.len() as i32);
        framed.put(payload);
        framed.freeze()
    }

    /// Finalize the packet **with** compression awareness.
    ///
    /// - If `payload.len() >= threshold`, zlib-compress the payload and frame
    ///   as `[Packet Length] [Data Length (uncompressed size)] [compressed data]`.
    /// - If `payload.len() < threshold`, frame as
    ///   `[Packet Length] [Data Length = 0] [raw payload]` (uncompressed).
    pub fn finish_compressed(self, threshold: usize) -> Result<Bytes> {
        let payload = self.bytes;
        let uncompressed_len = payload.len();

        if uncompressed_len >= threshold {
            let mut compressed = Vec::with_capacity(5 + 5 + uncompressed_len + 64);
            compressed.resize(10, 0);
            tuff_nbt::zlib_compress_append(&payload, &mut compressed)
                .map_err(|e| BufferError::CompressionFailed(e.to_string()))?;

            let data_length_size = var_int_size(uncompressed_len as i32);
            let packet_length = data_length_size + (compressed.len() - 10);
            let header = var_int_size(packet_length as i32) + data_length_size;
            let mut head = BytesMut::with_capacity(header);
            write_var_int_to(&mut head, packet_length as i32);
            write_var_int_to(&mut head, uncompressed_len as i32);
            let start = 10 - header;
            compressed[start..10].copy_from_slice(&head);
            Ok(Bytes::from(compressed).slice(start..))
        } else {
            // Below threshold — send uncompressed with Data Length = 0
            let packet_length = 1 + uncompressed_len; // 1 byte for VarInt(0)

            let mut framed = BytesMut::with_capacity(5 + packet_length);
            write_var_int_to(&mut framed, packet_length as i32);
            write_var_int_to(&mut framed, 0); // Data Length = 0
            framed.put(payload);
            Ok(framed.freeze())
        }
    }

    /// Writes a signed byte (`i8`).
    pub fn write_byte(&mut self, value: i8) {
        self.bytes.put_i8(value);
    }

    /// Writes an unsigned byte (`u8`).
    pub fn write_unsigned_byte(&mut self, value: u8) {
        self.bytes.put_u8(value);
    }

    /// Writes a signed 16-bit integer (`i16`, big-endian).
    pub fn write_short(&mut self, value: i16) {
        self.bytes.put_i16(value);
    }

    /// Writes an unsigned 16-bit integer (`u16`, big-endian).
    pub fn write_unsigned_short(&mut self, value: u16) {
        self.bytes.put_u16(value);
    }

    /// Writes a signed 32-bit integer (`i32`, big-endian).
    pub fn write_int(&mut self, value: i32) {
        self.bytes.put_i32(value);
    }

    /// Writes a signed 64-bit integer (`i64`, big-endian).
    pub fn write_long(&mut self, value: i64) {
        self.bytes.put_i64(value);
    }

    /// Writes a 32-bit floating-point value (`f32`, big-endian).
    pub fn write_float(&mut self, value: f32) {
        self.bytes.put_f32(value);
    }

    /// Writes a 64-bit floating-point value (`f64`, big-endian).
    pub fn write_double(&mut self, value: f64) {
        self.bytes.put_f64(value);
    }

    /// Writes a VarInt-encoded signed 32-bit integer (1–5 bytes).
    pub fn write_var_int(&mut self, value: i32) {
        write_var_int_to(&mut self.bytes, value);
    }

    /// Writes a VarLong-encoded signed 64-bit integer (1–10 bytes).
    pub fn write_var_long(&mut self, value: i64) {
        let mut val = value as u64;
        loop {
            let mut temp = (val & 0x7F) as u8;
            val >>= 7;
            if val != 0 {
                temp |= 0x80;
            }
            self.bytes.put_u8(temp);
            if val == 0 {
                break;
            }
        }
    }

    /// Writes a VarInt-prefixed UTF-8 string.
    pub fn write_string(&mut self, value: &str) {
        self.write_var_int(value.len() as i32);
        self.bytes.put_slice(value.as_bytes());
    }

    /// Writes a VarInt-prefixed UTF-8 string, rejecting one longer than
    /// `max_chars` UTF-16 code units.
    pub fn write_string_chars(&mut self, value: &str, max_chars: usize) -> Result<()> {
        let units = value.encode_utf16().count();
        if units > max_chars {
            return Err(BufferError::StringTooLong(units));
        }
        self.write_string(value);
        Ok(())
    }

    /// Writes a boolean as a single `0x00` / `0x01` byte.
    pub fn write_bool(&mut self, value: bool) {
        self.bytes.put_u8(value as u8);
    }

    /// Writes a UUID as a 128-bit big-endian integer.
    pub fn write_uuid(&mut self, value: u128) {
        self.bytes.put_u128(value);
    }

    /// Writes a VarInt-length-prefixed byte array.
    pub fn write_prefixed_bytes(&mut self, value: &[u8]) {
        self.write_var_int(value.len() as i32);
        self.bytes.put_slice(value);
    }

    /// Writes raw bytes without any prefix.
    pub fn write_bytes(&mut self, value: &[u8]) {
        self.bytes.put_slice(value);
    }

    /// Writes a network NBT value (type id and payload, no root name).
    pub fn write_nbt(&mut self, tag: &Tag) -> Result<()> {
        tag.write_network(&mut self.bytes)?;
        Ok(())
    }
}

/// Helper: encode a VarInt directly into a BytesMut.
fn write_var_int_to(buf: &mut BytesMut, value: i32) {
    let mut val = value as u32;
    loop {
        let mut temp = (val & 0x7F) as u8;
        val >>= 7;
        if val != 0 {
            temp |= 0x80;
        }
        buf.put_u8(temp);
        if val == 0 {
            break;
        }
    }
}

/// Returns the number of bytes a VarInt would occupy when encoded.
fn var_int_size(value: i32) -> usize {
    let mut val = value as u32;
    let mut size = 0;
    loop {
        size += 1;
        val >>= 7;
        if val == 0 {
            break;
        }
    }
    size
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::ZlibEncoder;
    use std::io::Write;
    use tuff_nbt::Compound;

    #[tuff_core::watchdog]
    #[test]
    fn nbt_round_trip_through_packet() {
        let tag = Tag::Compound(Compound::new().with("text", "hi").with("bold", true));
        let mut w = PacketWriter::new();
        w.write_nbt(&tag).unwrap();
        w.write_bytes(&[0x00]);
        w.write_int(7);

        let mut r = PacketReader::new(w.bytes.freeze());
        assert_eq!(r.read_nbt().unwrap(), Some(tag));
        assert_eq!(r.read_nbt().unwrap(), None);
        assert_eq!(r.read_int().unwrap(), 7);
        assert_eq!(r.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn truncated_nbt_does_not_advance() {
        let mut r = PacketReader::new(Bytes::from_static(&[0x03, 0x00]));
        assert!(matches!(r.read_nbt(), Err(BufferError::Nbt(_))));
        assert_eq!(r.remaining(), 2);
    }

    // -----------------------------------------------------------------------
    // Helper: build a framed packet (VarInt length + payload) as raw bytes
    // -----------------------------------------------------------------------
    fn frame_packet(payload: &[u8]) -> Vec<u8> {
        let mut w = PacketWriter::new();
        w.write_bytes(payload);
        w.finish().to_vec()
    }

    // -----------------------------------------------------------------------
    // peek_var_int
    // -----------------------------------------------------------------------

    #[tuff_core::watchdog]
    #[test]
    fn peek_var_int_single_byte() {
        let (val, size) = peek_var_int(&[0x00]).unwrap().unwrap();
        assert_eq!(val, 0);
        assert_eq!(size, 1);

        let (val, size) = peek_var_int(&[0x01]).unwrap().unwrap();
        assert_eq!(val, 1);
        assert_eq!(size, 1);

        let (val, size) = peek_var_int(&[0x7F]).unwrap().unwrap();
        assert_eq!(val, 127);
        assert_eq!(size, 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn peek_var_int_multi_byte() {
        // 128 = 0x80 0x01
        let (val, size) = peek_var_int(&[0x80, 0x01]).unwrap().unwrap();
        assert_eq!(val, 128);
        assert_eq!(size, 2);

        // 300 = 0xAC 0x02
        let (val, size) = peek_var_int(&[0xAC, 0x02]).unwrap().unwrap();
        assert_eq!(val, 300);
        assert_eq!(size, 2);

        // 25565 = 0xDD 0xC7 0x01
        let (val, size) = peek_var_int(&[0xDD, 0xC7, 0x01]).unwrap().unwrap();
        assert_eq!(val, 25565);
        assert_eq!(size, 3);
    }

    #[tuff_core::watchdog]
    #[test]
    fn peek_var_int_negative() {
        // -1 = 0xFF 0xFF 0xFF 0xFF 0x0F
        let (val, size) = peek_var_int(&[0xFF, 0xFF, 0xFF, 0xFF, 0x0F])
            .unwrap()
            .unwrap();
        assert_eq!(val, -1);
        assert_eq!(size, 5);
    }

    #[tuff_core::watchdog]
    #[test]
    fn peek_var_int_incomplete() {
        // 0x80 has continuation bit set but no following byte
        assert!(peek_var_int(&[0x80]).unwrap().is_none());
        assert!(peek_var_int(&[]).unwrap().is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn peek_var_int_too_long() {
        // 6 continuation bytes — exceeds 32-bit limit
        let buf = [0x80, 0x80, 0x80, 0x80, 0x80, 0x01];
        assert!(peek_var_int(&buf).is_err());
    }

    // -----------------------------------------------------------------------
    // Buffer — receive framing
    // -----------------------------------------------------------------------

    #[tuff_core::watchdog]
    #[test]
    fn buffer_try_split_empty() {
        let mut buf = Buffer::new();
        assert!(buf.try_split_packet().unwrap().is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn buffer_try_split_partial_header() {
        let mut buf = Buffer::new();
        // A continuation byte with no terminator
        buf.extend(&[0x80]).unwrap();
        assert!(buf.try_split_packet().unwrap().is_none());
        assert_eq!(buf.remaining(), 1); // nothing consumed
    }

    #[tuff_core::watchdog]
    #[test]
    fn buffer_try_split_partial_payload() {
        let mut buf = Buffer::new();
        // Header says 10 bytes, but we only provide 3
        buf.extend(&[0x0A, 0x01, 0x02, 0x03]).unwrap();
        assert!(buf.try_split_packet().unwrap().is_none());
        assert_eq!(buf.remaining(), 4); // nothing consumed
    }

    #[tuff_core::watchdog]
    #[test]
    fn buffer_try_split_complete_packet() {
        let mut buf = Buffer::new();
        let payload = b"hello";
        buf.extend(&frame_packet(payload)).unwrap();

        let packet = buf.try_split_packet().unwrap().unwrap();
        assert_eq!(&packet[..], payload);
        assert_eq!(buf.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn buffer_try_split_two_packets() {
        let mut buf = Buffer::new();
        buf.extend(&frame_packet(b"first")).unwrap();
        buf.extend(&frame_packet(b"second")).unwrap();

        let p1 = buf.try_split_packet().unwrap().unwrap();
        assert_eq!(&p1[..], b"first");

        let p2 = buf.try_split_packet().unwrap().unwrap();
        assert_eq!(&p2[..], b"second");

        assert!(buf.try_split_packet().unwrap().is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn buffer_incremental_receive() {
        let mut buf = Buffer::new();
        let framed = frame_packet(b"world");

        // Feed one byte at a time
        for (i, &byte) in framed.iter().enumerate() {
            buf.extend(&[byte]).unwrap();
            if i < framed.len() - 1 {
                assert!(buf.try_split_packet().unwrap().is_none());
            }
        }

        let packet = buf.try_split_packet().unwrap().unwrap();
        assert_eq!(&packet[..], b"world");
    }

    // -----------------------------------------------------------------------
    // PacketReader — field-level reading
    // -----------------------------------------------------------------------

    #[tuff_core::watchdog]
    #[test]
    fn reader_primitives() {
        let mut w = PacketWriter::new();
        w.write_byte(-42);
        w.write_unsigned_byte(200);
        w.write_short(-1234);
        w.write_unsigned_short(65000);
        w.write_int(0x12345678);
        w.write_long(0x0102030405060708);
        w.write_float(std::f32::consts::PI);
        w.write_double(std::f64::consts::E);
        let data = w.finish();

        // Skip the length prefix
        let mut buf = Buffer::new();
        buf.extend(&data).unwrap();
        let payload = buf.try_split_packet().unwrap().unwrap();
        let mut r = PacketReader::new(payload);

        assert_eq!(r.read_byte().unwrap(), -42);
        assert_eq!(r.read_unsigned_byte().unwrap(), 200);
        assert_eq!(r.read_short().unwrap(), -1234);
        assert_eq!(r.read_unsigned_short().unwrap(), 65000);
        assert_eq!(r.read_int().unwrap(), 0x12345678);
        assert_eq!(r.read_long().unwrap(), 0x0102030405060708);
        assert!((r.read_float().unwrap() - std::f32::consts::PI).abs() < f32::EPSILON);
        assert!((r.read_double().unwrap() - std::f64::consts::E).abs() < f64::EPSILON);
        assert_eq!(r.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn reader_var_int_roundtrip() {
        let values = [
            0,
            1,
            127,
            128,
            255,
            25565,
            2097151,
            2147483647,
            -1,
            -2147483648,
        ];

        for &val in &values {
            let mut w = PacketWriter::new();
            w.write_var_int(val);
            let data = w.finish();

            let mut buf = Buffer::new();
            buf.extend(&data).unwrap();
            let payload = buf.try_split_packet().unwrap().unwrap();
            let mut r = PacketReader::new(payload);

            assert_eq!(r.read_var_int().unwrap(), val, "roundtrip failed for {val}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn reader_var_long_roundtrip() {
        let values: [i64; 6] = [0, 1, 127, 128, -1, i64::MAX];

        for &val in &values {
            let mut w = PacketWriter::new();
            w.write_var_long(val);
            let data = w.finish();

            let mut buf = Buffer::new();
            buf.extend(&data).unwrap();
            let payload = buf.try_split_packet().unwrap().unwrap();
            let mut r = PacketReader::new(payload);

            assert_eq!(
                r.read_var_long().unwrap(),
                val,
                "roundtrip failed for {val}"
            );
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn reader_string_roundtrip() {
        let long_string = "a".repeat(1000);
        let strings = ["", "hello", "こんにちは", long_string.as_str()];

        for s in &strings {
            let mut w = PacketWriter::new();
            w.write_string(s);
            let data = w.finish();

            let mut buf = Buffer::new();
            buf.extend(&data).unwrap();
            let payload = buf.try_split_packet().unwrap().unwrap();
            let mut r = PacketReader::new(payload);

            assert_eq!(r.read_string().unwrap(), *s);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn reader_out_of_bounds() {
        let r_bytes = Bytes::from_static(&[0x01, 0x02]);
        let mut r = PacketReader::new(r_bytes);

        r.read_unsigned_byte().unwrap();
        r.read_unsigned_byte().unwrap();
        assert!(r.read_unsigned_byte().is_err());
    }

    // -----------------------------------------------------------------------
    // PacketWriter
    // -----------------------------------------------------------------------

    #[tuff_core::watchdog]
    #[test]
    fn writer_finish_prepends_length() {
        let mut w = PacketWriter::new();
        w.write_unsigned_byte(0xFE); // 1 byte payload
        let framed = w.finish();

        // Should be: [0x01 (length=1), 0xFE (payload)]
        assert_eq!(&framed[..], &[0x01, 0xFE]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn writer_empty_packet() {
        let w = PacketWriter::new();
        let framed = w.finish();

        // Length 0
        assert_eq!(&framed[..], &[0x00]);
    }

    // -----------------------------------------------------------------------
    // Compression
    // -----------------------------------------------------------------------

    #[tuff_core::watchdog]
    #[test]
    fn compressed_below_threshold_roundtrip() {
        let threshold = 256;

        // Write a small packet (below threshold)
        let mut w = PacketWriter::new();
        w.write_var_int(0x01); // packet ID
        w.write_string("hello");
        let framed = w.finish_compressed(threshold).unwrap();

        // Read it back with compression enabled
        let mut buf = Buffer::new();
        buf.set_compression_threshold(threshold);
        buf.extend(&framed).unwrap();

        let payload = buf.try_split_packet().unwrap().unwrap();
        let mut r = PacketReader::new(payload);
        assert_eq!(r.read_var_int().unwrap(), 0x01);
        assert_eq!(r.read_string().unwrap(), "hello");
    }

    #[tuff_core::watchdog]
    #[test]
    fn compressed_above_threshold_roundtrip() {
        let threshold = 16;

        // Write a large packet (above threshold)
        let mut w = PacketWriter::new();
        w.write_var_int(0x42); // packet ID
        w.write_string(&"a".repeat(500));
        let framed = w.finish_compressed(threshold).unwrap();

        // Read it back with compression enabled
        let mut buf = Buffer::new();
        buf.set_compression_threshold(threshold);
        buf.extend(&framed).unwrap();

        let payload = buf.try_split_packet().unwrap().unwrap();
        let mut r = PacketReader::new(payload);
        assert_eq!(r.read_var_int().unwrap(), 0x42);
        assert_eq!(r.read_string().unwrap(), "a".repeat(500));
    }

    #[tuff_core::watchdog]
    #[test]
    fn compressed_multiple_packets() {
        let threshold = 32;

        let mut w1 = PacketWriter::new();
        w1.write_var_int(0x01);
        w1.write_string("short");
        let f1 = w1.finish_compressed(threshold).unwrap();

        let mut w2 = PacketWriter::new();
        w2.write_var_int(0x02);
        w2.write_string(&"b".repeat(200));
        let f2 = w2.finish_compressed(threshold).unwrap();

        let mut buf = Buffer::new();
        buf.set_compression_threshold(threshold);
        buf.extend(&f1).unwrap();
        buf.extend(&f2).unwrap();

        let p1 = buf.try_split_packet().unwrap().unwrap();
        assert_eq!(buf.last_frame_len(), f1.len());
        let mut r1 = PacketReader::new(p1);
        assert_eq!(r1.read_var_int().unwrap(), 0x01);
        assert_eq!(r1.read_string().unwrap(), "short");

        let p2 = buf.try_split_packet().unwrap().unwrap();
        assert_eq!(buf.last_frame_len(), f2.len());
        assert!(f2.len() < p2.len());
        let mut r2 = PacketReader::new(p2);
        assert_eq!(r2.read_var_int().unwrap(), 0x02);
        assert_eq!(r2.read_string().unwrap(), "b".repeat(200));
    }

    #[tuff_core::watchdog]
    #[test]
    fn reader_bool_uuid_prefixed_bytes_roundtrip() {
        let uuid = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210_u128;
        let mut w = PacketWriter::new();
        w.write_bool(true);
        w.write_bool(false);
        w.write_uuid(uuid);
        w.write_prefixed_bytes(&[1, 2, 3]);
        let mut buf = Buffer::new();
        buf.extend(&w.finish()).unwrap();
        let mut r = PacketReader::new(buf.try_split_packet().unwrap().unwrap());

        assert!(r.read_bool().unwrap());
        assert!(!r.read_bool().unwrap());
        assert_eq!(r.read_uuid().unwrap(), uuid);
        assert_eq!(&r.read_prefixed_bytes(3).unwrap()[..], &[1, 2, 3]);
        assert_eq!(r.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn reader_rejects_invalid_bool() {
        let mut r = PacketReader::new(Bytes::from_static(&[0x02]));
        assert!(matches!(r.read_bool(), Err(BufferError::InvalidBool(2))));
    }

    #[tuff_core::watchdog]
    #[test]
    fn reader_rejects_oversized_prefixed_bytes() {
        let mut w = PacketWriter::new();
        w.write_prefixed_bytes(&[0; 8]);
        let mut buf = Buffer::new();
        buf.extend(&w.finish()).unwrap();
        let mut r = PacketReader::new(buf.try_split_packet().unwrap().unwrap());
        assert!(matches!(
            r.read_prefixed_bytes(4),
            Err(BufferError::InvalidArrayLength(8))
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn var_int_size_values() {
        assert_eq!(var_int_size(0), 1);
        assert_eq!(var_int_size(1), 1);
        assert_eq!(var_int_size(127), 1);
        assert_eq!(var_int_size(128), 2);
        assert_eq!(var_int_size(16383), 2);
        assert_eq!(var_int_size(16384), 3);
        assert_eq!(var_int_size(2097151), 3);
    }

    // -----------------------------------------------------------------------
    // Security — vulnerability regression tests
    // -----------------------------------------------------------------------

    /// A compressed packet whose `data_length` exceeds 2 MiB must be
    /// rejected *before* the allocation happens (CWE-409).
    #[tuff_core::watchdog]
    #[test]
    fn v1_rejects_decompression_bomb() {
        let threshold = 64;
        let fake_uncompressed_size: i32 = 16 * 1024 * 1024; // 16 MiB >> 2 MiB limit

        // Compress a small payload — the content doesn't matter, only the
        // claimed data_length triggers the check before decompression.
        let small_data = vec![0u8; 128];
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&small_data).unwrap();
        let compressed = encoder.finish().unwrap();

        // Build inner: [VarInt data_length] [compressed bytes]
        let mut inner = BytesMut::new();
        write_var_int_to(&mut inner, fake_uncompressed_size);
        inner.put_slice(&compressed);

        // Build outer: [VarInt packet_length] [inner]
        let mut outer = BytesMut::new();
        write_var_int_to(&mut outer, inner.len() as i32);
        outer.put(inner);

        let mut buf = Buffer::new();
        buf.set_compression_threshold(threshold);
        buf.extend(&outer).unwrap();

        match buf.try_split_packet() {
            Err(BufferError::DecompressedSizeTooLarge(size)) => {
                assert_eq!(size, fake_uncompressed_size as usize);
            }
            other => panic!("expected DecompressedSizeTooLarge, got {other:?}"),
        }
    }

    /// A compressed packet whose zlib stream produces *more* bytes than
    /// `data_length` claims must be rejected.
    #[tuff_core::watchdog]
    #[test]
    fn v1_rejects_decompressed_size_mismatch() {
        let threshold = 16;
        let actual_data = vec![0xABu8; 512];
        let claimed_size: i32 = 256; // Lies — actual decompressed output is 512

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&actual_data).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut inner = BytesMut::new();
        write_var_int_to(&mut inner, claimed_size);
        inner.put_slice(&compressed);

        let mut outer = BytesMut::new();
        write_var_int_to(&mut outer, inner.len() as i32);
        outer.put(inner);

        let mut buf = Buffer::new();
        buf.set_compression_threshold(threshold);
        buf.extend(&outer).unwrap();

        match buf.try_split_packet() {
            Err(BufferError::DecompressedSizeMismatch) => {}
            other => panic!("expected DecompressedSizeMismatch, got {other:?}"),
        }
    }

    /// Extending the buffer past MAX_BUFFER_SIZE must return BufferFull.
    #[tuff_core::watchdog]
    #[test]
    fn v2_rejects_buffer_overflow() {
        let mut buf = Buffer::new();
        let big_chunk = vec![0u8; MAX_BUFFER_SIZE + 1];
        match buf.extend(&big_chunk) {
            Err(BufferError::BufferFull(_)) => {}
            other => panic!("expected BufferFull, got {other:?}"),
        }
    }

    /// Incremental extends that cumulatively exceed the limit must also fail.
    #[tuff_core::watchdog]
    #[test]
    fn v2_rejects_incremental_buffer_overflow() {
        let mut buf = Buffer::new();
        let half = vec![0u8; MAX_BUFFER_SIZE / 2 + 1];
        buf.extend(&half).unwrap();
        match buf.extend(&half) {
            Err(BufferError::BufferFull(_)) => {}
            other => panic!("expected BufferFull, got {other:?}"),
        }
    }

    /// A packet_length VarInt exceeding MAX_PACKET_SIZE must be rejected
    /// immediately, even if the body hasn't arrived yet.
    #[tuff_core::watchdog]
    #[test]
    fn v3_rejects_oversized_packet_length() {
        let huge_len: i32 = (MAX_PACKET_SIZE as i32) + 1;
        let mut raw = BytesMut::new();
        write_var_int_to(&mut raw, huge_len);
        // A few filler bytes so the VarInt header is parseable but the body
        // is obviously incomplete — the size check must fire first.
        raw.put_slice(&[0x00; 16]);

        let mut buf = Buffer::new();
        buf.extend(&raw).unwrap();

        match buf.try_split_packet() {
            Err(BufferError::PacketTooLarge(size)) => {
                assert_eq!(size, huge_len as usize);
            }
            other => panic!("expected PacketTooLarge, got {other:?}"),
        }
    }

    /// Multi-byte strings are limited by UTF-16 units, not bytes.
    #[tuff_core::watchdog]
    #[test]
    fn string_chars_limit_counts_characters() {
        let mut w = PacketWriter::new();
        w.write_string("\u{e9}\u{e9}\u{e9}");
        w.write_string("\u{e9}\u{e9}\u{e9}\u{e9}");
        w.write_string("\u{1F600}\u{1F600}");
        w.write_string("\u{1F600}\u{1F600}\u{1F600}");
        let framed = w.finish();
        let mut buf = Buffer::new();
        buf.extend(&framed).unwrap();
        let payload = buf.try_split_packet().unwrap().unwrap();
        let mut r = PacketReader::new(payload);
        assert!(r.read_string_chars(3).is_ok());
        assert!(matches!(
            r.read_string_chars(3),
            Err(BufferError::StringTooLong(4))
        ));
        assert!(r.read_string_chars(4).is_ok());
        assert!(matches!(
            r.read_string_chars(5),
            Err(BufferError::StringTooLong(6))
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn write_string_chars_rejects_over_limit() {
        let mut w = PacketWriter::new();
        assert!(w.write_string_chars("abcd", 4).is_ok());
        assert!(matches!(
            w.write_string_chars("abcde", 4),
            Err(BufferError::StringTooLong(5))
        ));
    }

    /// A string whose VarInt length exceeds MAX_STRING_BYTES must be rejected.
    #[tuff_core::watchdog]
    #[test]
    fn v4_rejects_oversized_string() {
        const MAX_STRING_BYTES: usize = 32767 * 3;
        let oversized_len = (MAX_STRING_BYTES + 1) as i32;

        // Build a payload: VarInt(oversized_len) + that many 'a' bytes
        let mut w = PacketWriter::new();
        w.write_var_int(oversized_len);
        w.write_bytes(&vec![b'a'; oversized_len as usize]);
        let framed = w.finish();

        let mut buf = Buffer::new();
        buf.extend(&framed).unwrap();
        let payload = buf.try_split_packet().unwrap().unwrap();
        let mut r = PacketReader::new(payload);

        match r.read_string() {
            Err(BufferError::StringTooLong(len)) => {
                assert_eq!(len, oversized_len as usize);
            }
            other => panic!("expected StringTooLong, got {other:?}"),
        }
    }

    /// Strings within the limit must still work.
    #[tuff_core::watchdog]
    #[test]
    fn v4_accepts_string_within_limit() {
        let s = "a".repeat(32767);
        let mut w = PacketWriter::new();
        w.write_string(&s);
        let framed = w.finish();

        let mut buf = Buffer::new();
        buf.extend(&framed).unwrap();
        let payload = buf.try_split_packet().unwrap().unwrap();
        let mut r = PacketReader::new(payload);

        assert_eq!(r.read_string().unwrap(), s);
    }

    /// A compressed packet with `0 < data_length < threshold` violates the
    /// protocol and must be rejected.
    #[tuff_core::watchdog]
    #[test]
    fn v5_rejects_data_length_below_threshold() {
        let threshold = 256;
        let small_data = vec![0xCCu8; 64]; // 64 < 256

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&small_data).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut inner = BytesMut::new();
        write_var_int_to(&mut inner, small_data.len() as i32); // data_length = 64
        inner.put_slice(&compressed);

        let mut outer = BytesMut::new();
        write_var_int_to(&mut outer, inner.len() as i32);
        outer.put(inner);

        let mut buf = Buffer::new();
        buf.set_compression_threshold(threshold);
        buf.extend(&outer).unwrap();

        match buf.try_split_packet() {
            Err(BufferError::InvalidDataLength) => {}
            other => panic!("expected InvalidDataLength, got {other:?}"),
        }
    }

    /// data_length == threshold (the boundary) must be accepted.
    #[tuff_core::watchdog]
    #[test]
    fn v5_accepts_data_length_at_threshold() {
        let threshold = 64;
        let data = vec![0xBBu8; threshold]; // exactly == threshold

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&data).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut inner = BytesMut::new();
        write_var_int_to(&mut inner, data.len() as i32);
        inner.put_slice(&compressed);

        let mut outer = BytesMut::new();
        write_var_int_to(&mut outer, inner.len() as i32);
        outer.put(inner);

        let mut buf = Buffer::new();
        buf.set_compression_threshold(threshold);
        buf.extend(&outer).unwrap();

        let payload = buf.try_split_packet().unwrap().unwrap();
        assert_eq!(&payload[..], &data[..]);
    }
}
