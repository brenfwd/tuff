//! Decoder for the lz4-java `LZ4BlockOutputStream` framing that vanilla uses
//! for region chunks written with `region-file-compression=lz4`. Each block is
//! `"LZ4Block"`, a method/level token, little-endian compressed length,
//! decompressed length and XXH32 checksum, then the payload. An empty block
//! ends the stream.

use thiserror::Error;

const MAGIC: &[u8; 8] = b"LZ4Block";
const HEADER_LEN: usize = MAGIC.len() + 13;
const METHOD_RAW: u8 = 0x10;
const METHOD_LZ4: u8 = 0x20;
const CHECKSUM_SEED: u32 = 0x9747_B28C;
const CHECKSUM_MASK: u32 = 0x0FFF_FFFF;
const MIN_MATCH: usize = 4;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum Lz4Error {
    #[error("truncated LZ4 stream")]
    Truncated,
    #[error("bad LZ4 block header")]
    BadHeader,
    #[error("LZ4 output exceeds {0} bytes")]
    TooLarge(usize),
    #[error("corrupt LZ4 block")]
    Corrupt,
    #[error("LZ4 block checksum mismatch")]
    Checksum,
}

/// Decodes a whole block stream, failing rather than producing more than
/// `max_len` bytes. Declared lengths are checked against the budget before
/// anything is allocated.
pub fn decompress_stream(mut input: &[u8], max_len: usize) -> Result<Vec<u8>, Lz4Error> {
    let mut out = Vec::new();
    loop {
        if input.is_empty() {
            return Ok(out);
        }
        if input.len() < HEADER_LEN {
            return Err(Lz4Error::Truncated);
        }
        if &input[..MAGIC.len()] != MAGIC {
            return Err(Lz4Error::BadHeader);
        }
        let token = input[8];
        let le = |at: usize| u32::from_le_bytes(input[at..at + 4].try_into().unwrap());
        let compressed = le(9) as usize;
        let decompressed = le(13) as usize;
        let checksum = le(17);
        input = &input[HEADER_LEN..];
        if decompressed == 0 {
            if compressed != 0 {
                return Err(Lz4Error::BadHeader);
            }
            return Ok(out);
        }
        if decompressed > max_len.saturating_sub(out.len()) {
            return Err(Lz4Error::TooLarge(max_len));
        }
        if compressed > input.len() {
            return Err(Lz4Error::Truncated);
        }
        let (payload, rest) = input.split_at(compressed);
        input = rest;
        let start = out.len();
        match token & 0xF0 {
            METHOD_RAW if compressed == decompressed => out.extend_from_slice(payload),
            METHOD_LZ4 => decompress_block(payload, decompressed, &mut out)?,
            _ => return Err(Lz4Error::BadHeader),
        }
        if xxh32(&out[start..], CHECKSUM_SEED) & CHECKSUM_MASK != checksum {
            return Err(Lz4Error::Checksum);
        }
    }
}

/// Encodes `data` as a block stream of stored (uncompressed) blocks, which
/// every lz4-java reader accepts. Vanilla worlds written with LZ4 stay
/// readable by vanilla; only the space saving is forgone.
pub fn compress_stream(data: &[u8]) -> Vec<u8> {
    const BLOCK: usize = 1 << 16;
    let mut out = Vec::with_capacity(data.len() + HEADER_LEN * (data.len() / BLOCK + 2));
    for chunk in data.chunks(BLOCK).chain(std::iter::once(&[][..])) {
        out.extend_from_slice(MAGIC);
        out.push(METHOD_RAW);
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
        out.extend_from_slice(&(xxh32(chunk, CHECKSUM_SEED) & CHECKSUM_MASK).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    out
}

fn next(input: &mut &[u8]) -> Result<u8, Lz4Error> {
    let (&b, rest) = input.split_first().ok_or(Lz4Error::Corrupt)?;
    *input = rest;
    Ok(b)
}

fn length(nibble: u8, input: &mut &[u8]) -> Result<usize, Lz4Error> {
    let mut n = nibble as usize;
    if nibble == 15 {
        loop {
            let b = next(input)?;
            n = n.checked_add(b as usize).ok_or(Lz4Error::Corrupt)?;
            if b != 255 {
                break;
            }
        }
    }
    Ok(n)
}

/// Appends exactly `len` bytes decoded from one raw LZ4 block to `out`.
fn decompress_block(mut input: &[u8], len: usize, out: &mut Vec<u8>) -> Result<(), Lz4Error> {
    let start = out.len();
    let end = start + len;
    out.reserve(len);
    loop {
        let token = next(&mut input)?;
        let literals = length(token >> 4, &mut input)?;
        if literals > input.len() || literals > end - out.len() {
            return Err(Lz4Error::Corrupt);
        }
        out.extend_from_slice(&input[..literals]);
        input = &input[literals..];
        if input.is_empty() {
            return if out.len() == end {
                Ok(())
            } else {
                Err(Lz4Error::Corrupt)
            };
        }
        let offset = next(&mut input)? as usize | (next(&mut input)? as usize) << 8;
        let matched = length(token & 0x0F, &mut input)? + MIN_MATCH;
        if offset == 0 || offset > out.len() - start || matched > end - out.len() {
            return Err(Lz4Error::Corrupt);
        }
        let from = out.len() - offset;
        for i in 0..matched {
            out.push(out[from + i]);
        }
    }
}

fn xxh32(data: &[u8], seed: u32) -> u32 {
    const P1: u32 = 2_654_435_761;
    const P2: u32 = 2_246_822_519;
    const P3: u32 = 3_266_489_917;
    const P4: u32 = 668_265_263;
    const P5: u32 = 374_761_393;
    let word = |b: &[u8]| u32::from_le_bytes(b[..4].try_into().unwrap());
    let round = |acc: u32, lane: u32| {
        acc.wrapping_add(lane.wrapping_mul(P2))
            .rotate_left(13)
            .wrapping_mul(P1)
    };
    let mut rest = data;
    let mut hash = if data.len() >= 16 {
        let mut v = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        while rest.len() >= 16 {
            for (i, lane) in v.iter_mut().enumerate() {
                *lane = round(*lane, word(&rest[i * 4..]));
            }
            rest = &rest[16..];
        }
        v[0].rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18))
    } else {
        seed.wrapping_add(P5)
    };
    hash = hash.wrapping_add(data.len() as u32);
    while rest.len() >= 4 {
        hash = hash
            .wrapping_add(word(rest).wrapping_mul(P3))
            .rotate_left(17)
            .wrapping_mul(P4);
        rest = &rest[4..];
    }
    for &b in rest {
        hash = hash
            .wrapping_add((b as u32).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
    }
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(P2);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(P3);
    hash ^ (hash >> 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(method: u8, payload: &[u8], decoded: &[u8]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.push(method);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(decoded.len() as u32).to_le_bytes());
        out.extend_from_slice(&(xxh32(decoded, CHECKSUM_SEED) & CHECKSUM_MASK).to_le_bytes());
        out.extend_from_slice(payload);
        out
    }

    fn end() -> Vec<u8> {
        block(METHOD_RAW, &[], &[])
    }

    #[tuff_core::watchdog]
    #[test]
    fn xxh32_matches_reference_vectors() {
        assert_eq!(xxh32(b"", 0), 0x02CC_5D05);
        assert_eq!(xxh32(b"a", 0), 0x550D_7456);
        assert_eq!(
            xxh32(b"Nobody inspects the spammish repetition", 0),
            0xE229_3B2F
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn decodes_raw_and_compressed_blocks() {
        let mut stream = block(METHOD_RAW, b"hello", b"hello");
        // One literal 'a', then a 9-byte overlapping match at offset 1.
        let decoded = b"aaaaaaaaaab";
        stream.extend(block(METHOD_LZ4, &[0x15, b'a', 1, 0, 0x10, b'b'], decoded));
        stream.extend(end());
        let out = decompress_stream(&stream, 1024).unwrap();
        assert_eq!(out, b"helloaaaaaaaaaab");
    }

    #[tuff_core::watchdog]
    #[test]
    fn compress_stream_round_trips() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let encoded = compress_stream(&data);
        assert_eq!(decompress_stream(&encoded, data.len()).unwrap(), data);
        assert!(
            decompress_stream(&compress_stream(&[]), 16)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            decompress_stream(&encoded, 100),
            Err(Lz4Error::TooLarge(100))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn rejects_bad_checksum_and_truncation() {
        let mut stream = block(METHOD_RAW, b"hello", b"hello");
        stream[HEADER_LEN - 1] ^= 1;
        assert_eq!(decompress_stream(&stream, 1024), Err(Lz4Error::Checksum));
        let stream = block(METHOD_RAW, b"hello", b"hello");
        assert_eq!(
            decompress_stream(&stream[..stream.len() - 1], 1024),
            Err(Lz4Error::Truncated)
        );
        assert_eq!(
            decompress_stream(&stream[..10], 1024),
            Err(Lz4Error::Truncated)
        );
        assert_eq!(
            decompress_stream(b"NotLZ4Block..........", 1024),
            Err(Lz4Error::BadHeader)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn enforces_output_limit_before_allocating() {
        let mut header = MAGIC.to_vec();
        header.push(METHOD_LZ4);
        header.extend_from_slice(&4u32.to_le_bytes());
        header.extend_from_slice(&u32::MAX.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&[0xF0, 0, 0, 0]);
        assert_eq!(
            decompress_stream(&header, 1 << 20),
            Err(Lz4Error::TooLarge(1 << 20))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn rejects_malformed_sequences() {
        let bad_offset = [0x04, 5, 0];
        let decoded = [0u8; 8];
        let stream = block(METHOD_LZ4, &bad_offset, &decoded);
        assert_eq!(decompress_stream(&stream, 1024), Err(Lz4Error::Corrupt));
        let overlong_match = [0x1F, b'a', 1, 0, 255, 255, 255, 10, 0x00];
        let stream = block(METHOD_LZ4, &overlong_match, &decoded);
        assert_eq!(decompress_stream(&stream, 1024), Err(Lz4Error::Corrupt));
    }
}
