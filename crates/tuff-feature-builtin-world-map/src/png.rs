//! Indexed-colour PNGs whose palette is the 256 packed map colours, so a
//! tile's pixels are written as they are.

use std::io::{Read, Write};
use std::sync::OnceLock;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use tuff_vanilla_data::map::argb;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

struct Palette {
    rgb: [u8; 768],
    alpha: Vec<u8>,
}

fn palette() -> &'static Palette {
    static PALETTE: OnceLock<Palette> = OnceLock::new();
    PALETTE.get_or_init(|| {
        let mut rgb = [0u8; 768];
        let mut alpha = [0u8; 256];
        for i in 0..256 {
            let color = argb(i as u8);
            rgb[i * 3] = (color >> 16) as u8;
            rgb[i * 3 + 1] = (color >> 8) as u8;
            rgb[i * 3 + 2] = color as u8;
            alpha[i] = (color >> 24) as u8;
        }
        let used = alpha.iter().rposition(|&a| a != 255).map_or(0, |i| i + 1);
        Palette {
            rgb,
            alpha: alpha[..used].to_vec(),
        }
    })
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = flate2::Crc::new();
    crc.update(&out[start..]);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}

/// Encodes `pixels` (packed map colours, row by row) as a `side` x `side`
/// PNG.
pub fn encode(pixels: &[u8], side: usize) -> Vec<u8> {
    assert_eq!(pixels.len(), side * side);
    let palette = palette();
    let mut out = Vec::with_capacity(2048);
    out.extend_from_slice(&SIGNATURE);
    let mut header = [0u8; 13];
    header[0..4].copy_from_slice(&(side as u32).to_be_bytes());
    header[4..8].copy_from_slice(&(side as u32).to_be_bytes());
    header[8] = 8;
    header[9] = 3;
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"PLTE", &palette.rgb);
    chunk(&mut out, b"tRNS", &palette.alpha);
    let mut zlib = ZlibEncoder::new(Vec::with_capacity(pixels.len() / 8), Compression::default());
    let mut row = Vec::with_capacity(side + 1);
    for line in pixels.chunks_exact(side) {
        row.clear();
        row.push(0);
        row.extend_from_slice(line);
        zlib.write_all(&row).expect("writing to memory");
    }
    let data = zlib.finish().expect("writing to memory");
    chunk(&mut out, b"IDAT", &data);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// The pixels of a `side` x `side` PNG laid out as [`encode`] writes it
/// (indexed, 8 bits, unfiltered rows); `None` for anything else. The
/// input is untrusted: chunk CRCs are checked and inflating stops one
/// byte past the exact pixel count.
pub fn decode(png: &[u8], side: usize) -> Option<Vec<u8>> {
    let rest = png.strip_prefix(&SIGNATURE[..])?;
    let mut at = 0;
    let mut header = false;
    let mut idat = Vec::new();
    loop {
        let len = u32::from_be_bytes(rest.get(at..at + 4)?.try_into().ok()?) as usize;
        let body = rest.get(at + 4..at.checked_add(8)?.checked_add(len)?)?;
        let stored = rest.get(at + 8 + len..at + 12 + len)?;
        let mut crc = flate2::Crc::new();
        crc.update(body);
        if crc.sum().to_be_bytes() != stored {
            return None;
        }
        let (kind, data) = body.split_at(4);
        at += 12 + len;
        match kind {
            b"IHDR" => {
                let mut expected = [0u8; 13];
                expected[0..4].copy_from_slice(&u32::try_from(side).ok()?.to_be_bytes());
                expected[4..8].copy_from_slice(&u32::try_from(side).ok()?.to_be_bytes());
                expected[8] = 8;
                expected[9] = 3;
                if header || data != expected {
                    return None;
                }
                header = true;
            }
            b"IDAT" if header => idat.extend_from_slice(data),
            b"IEND" => break,
            _ if header => {}
            _ => return None,
        }
    }
    if at != rest.len() {
        return None;
    }
    let size = side.checked_mul(side + 1)?;
    let mut raw = Vec::with_capacity(size);
    flate2::read::ZlibDecoder::new(&idat[..])
        .take(size as u64 + 1)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() != size {
        return None;
    }
    let mut pixels = Vec::with_capacity(side * side);
    for row in raw.chunks_exact(side + 1) {
        if row[0] != 0 {
            return None;
        }
        pixels.extend_from_slice(&row[1..]);
    }
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::ZlibDecoder;

    fn chunks(png: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(png[..8], SIGNATURE);
        let mut at = 8;
        let mut found = Vec::new();
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = png[at + 4..at + 8].try_into().unwrap();
            let data = png[at + 8..at + 8 + len].to_vec();
            let stored = u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().unwrap());
            let mut crc = flate2::Crc::new();
            crc.update(&png[at + 4..at + 8 + len]);
            assert_eq!(crc.sum(), stored, "crc of {:?}", std::str::from_utf8(&kind));
            found.push((kind, data));
            at += 12 + len;
        }
        found
    }

    #[tuff_core::watchdog]
    #[test]
    fn writes_an_indexed_png_with_the_map_palette() {
        let side = 4;
        let pixels: Vec<u8> = (0..16).map(|i| i * 4 + 1).collect();
        let png = encode(&pixels, side);
        let found = chunks(&png);
        let kinds: Vec<&[u8; 4]> = found.iter().map(|(k, _)| k).collect();
        assert_eq!(kinds, [b"IHDR", b"PLTE", b"tRNS", b"IDAT", b"IEND"]);
        let header = &found[0].1;
        assert_eq!(header[..8], [0, 0, 0, 4, 0, 0, 0, 4]);
        assert_eq!(header[8..], [8, 3, 0, 0, 0]);
        let plte = &found[1].1;
        assert_eq!(plte.len(), 768);
        let grass = argb(5);
        assert_eq!(
            plte[15..18],
            [(grass >> 16) as u8, (grass >> 8) as u8, grass as u8]
        );
        let alpha = &found[2].1;
        assert_eq!(
            alpha[..],
            [0, 0, 0, 0],
            "only the empty colour is transparent"
        );
        let mut raw = Vec::new();
        ZlibDecoder::new(&found[3].1[..])
            .read_to_end(&mut raw)
            .unwrap();
        assert_eq!(raw.len(), side * (side + 1));
        for (y, line) in raw.chunks(side + 1).enumerate() {
            assert_eq!(line[0], 0);
            assert_eq!(&line[1..], &pixels[y * side..(y + 1) * side]);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_empty_tile_is_tiny() {
        let png = encode(&vec![0; 512 * 512], 512);
        assert!(png.len() < 1400, "{}", png.len());
    }

    #[tuff_core::watchdog]
    #[test]
    fn decoding_returns_what_was_encoded_and_refuses_anything_else() {
        let pixels: Vec<u8> = (0..64 * 64).map(|i| (i % 251) as u8).collect();
        let png = encode(&pixels, 64);
        assert_eq!(decode(&png, 64).as_deref(), Some(&pixels[..]));
        assert_eq!(decode(&png, 32), None, "another size");
        for cut in [0, 7, 8, 20, png.len() / 2, png.len() - 1] {
            assert_eq!(decode(&png[..cut], 64), None, "cut at {cut}");
        }
        let mut flipped = png.clone();
        let middle = flipped.len() / 2;
        flipped[middle] ^= 0x10;
        assert_eq!(decode(&flipped, 64), None, "a bad CRC");
        let mut trailing = png.clone();
        trailing.push(0);
        assert_eq!(decode(&trailing, 64), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn decoding_stops_at_the_pixel_count_of_an_inflation_bomb() {
        let mut out = Vec::new();
        out.extend_from_slice(&SIGNATURE);
        let mut header = [0u8; 13];
        header[0..4].copy_from_slice(&4u32.to_be_bytes());
        header[4..8].copy_from_slice(&4u32.to_be_bytes());
        header[8] = 8;
        header[9] = 3;
        chunk(&mut out, b"IHDR", &header);
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::best());
        zlib.write_all(&vec![0u8; 64 << 20]).unwrap();
        chunk(&mut out, b"IDAT", &zlib.finish().unwrap());
        chunk(&mut out, b"IEND", &[]);
        assert!(out.len() < 128 << 10);
        assert_eq!(decode(&out, 4), None);
    }
}
