//! A deterministic zip writer and a bounded reader for resource packs,
//! over [`tuff_zip`].
//!
//! The writer emits entries in the order given, with a fixed timestamp and
//! a fixed deflate level, so equal inputs always give equal bytes. The
//! reader treats its input as hostile: every size is checked against hard
//! caps before anything is allocated or inflated.

use std::io::{self, Read};

use bytes::Bytes;

use tuff_zip::{self as core_zip, DosTime};

const LEVEL: u32 = 6;

pub const MAX_ENTRIES: usize = u16::MAX as usize - 1;

/// Limits for [`read`].
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_entries: usize,
    pub max_entry_bytes: usize,
    pub max_total_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 16 * 1024,
            max_entry_bytes: 64 * 1024 * 1024,
            max_total_bytes: crate::resource_pack::MAX_PACK_BYTES,
        }
    }
}

/// Zips `entries` in order. Each entry is deflated when that is smaller,
/// stored otherwise.
pub fn write<'a>(
    entries: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    max_bytes: usize,
) -> io::Result<Bytes> {
    let entries: Vec<(&str, &[u8])> = entries.into_iter().collect();
    if entries.len() > MAX_ENTRIES {
        return Err(io::Error::other("zip archive has too many entries"));
    }
    if entries
        .iter()
        .any(|(name, _)| name.len() > u16::MAX as usize)
    {
        return Err(io::Error::other("zip archive is too large"));
    }
    core_zip::write_all(&entries, DosTime::MIN, LEVEL, max_bytes as u64).map(Bytes::from)
}

fn malformed(what: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("malformed zip: {what}"))
}

/// Every file in a zip archive, by name, in directory order. Directory
/// entries are skipped. Fails on anything outside `limits`, on unsupported
/// methods, on size or checksum mismatches and on truncation.
pub fn read(data: &[u8], limits: Limits) -> io::Result<Vec<(String, Bytes)>> {
    let archive = core_zip::Archive::open(
        data,
        core_zip::Limits {
            max_entries: limits.max_entries,
            max_entry_bytes: limits.max_entry_bytes as u64,
            max_total_bytes: limits.max_total_bytes as u64,
            max_ratio: None,
            strict: false,
        },
    )
    .map_err(malformed)?;
    let mut files = Vec::with_capacity(archive.entries().len());
    for entry in archive.entries() {
        let mut content = Vec::with_capacity((entry.size as usize).min(1 << 20));
        archive
            .reader(entry)
            .and_then(|mut reader| reader.read_to_end(&mut content))
            .map_err(malformed)?;
        if !entry.is_dir() {
            files.push((entry.name.clone(), Bytes::from(content)));
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Bytes {
        let big = vec![b'a'; 4096];
        write(
            [
                ("pack.mcmeta", b"{\"pack\":{}}".as_slice()),
                ("assets/ex/big.txt", big.as_slice()),
                ("assets/ex/empty", b"".as_slice()),
            ],
            usize::MAX,
        )
        .unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn round_trips_and_is_deterministic() {
        let zip = sample();
        assert_eq!(zip, sample());
        let files = read(&zip, Limits::default()).unwrap();
        let names: Vec<_> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["pack.mcmeta", "assets/ex/big.txt", "assets/ex/empty"]
        );
        assert_eq!(files[1].1.len(), 4096);
        assert!(zip.len() < 4096, "repetitive data is deflated");
        assert_eq!(files[0].1.as_ref(), b"{\"pack\":{}}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn writer_enforces_its_cap() {
        let data = vec![7u8; 100];
        assert!(write([("a", data.as_slice())], 50).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn reader_rejects_truncated_and_malformed() {
        let zip = sample();
        for cut in [0, 10, zip.len() / 2, zip.len() - 1] {
            assert!(
                read(&zip[..cut], Limits::default()).is_err(),
                "cut at {cut}"
            );
        }
        let mut corrupt = zip.to_vec();
        let body = 30 + "pack.mcmeta".len();
        corrupt[body] ^= 0xff;
        assert!(read(&corrupt, Limits::default()).is_err());
        assert!(read(b"not a zip at all, just text", Limits::default()).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn reader_refuses_bombs_and_lying_sizes() {
        let bomb = vec![0u8; 8 * 1024 * 1024];
        let zip = write([("bomb", bomb.as_slice())], usize::MAX).unwrap();
        assert!(zip.len() < 64 * 1024);
        let tight = Limits {
            max_entry_bytes: 1024 * 1024,
            ..Limits::default()
        };
        assert!(read(&zip, tight).is_err());
        let total = Limits {
            max_total_bytes: 1024,
            ..Limits::default()
        };
        assert!(read(&zip, total).is_err());
        let few = Limits {
            max_entries: 0,
            ..Limits::default()
        };
        assert!(read(&sample(), few).is_err());

        let mut lying = zip.to_vec();
        let end = lying.len() - 22;
        let directory = u32::from_le_bytes(lying[end + 16..end + 20].try_into().unwrap()) as usize;
        lying[directory + 24..directory + 28].copy_from_slice(&16u32.to_le_bytes());
        assert!(read(&lying, Limits::default()).is_err());
    }

    /// A reference writer for the exact pack bytes; the pack's hash is what
    /// clients cache it by, so the bytes must not move.
    fn reference_write(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        fn common(out: &mut Vec<u8>, method: u16, crc: u32, compressed: u32, size: u32, name: u16) {
            for v in [method, 0, (1 << 5) | 1] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            for v in [crc, compressed, size] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&name.to_le_bytes());
        }
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let mut crc = flate2::Crc::new();
            crc.update(data);
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(6));
            encoder.write_all(data).unwrap();
            let deflated = encoder.finish().unwrap();
            let (method, body) = if deflated.len() < data.len() {
                (8u16, deflated.as_slice())
            } else {
                (0u16, *data)
            };
            let offset = out.len() as u32;
            let fields = (
                method,
                crc.sum(),
                body.len() as u32,
                data.len() as u32,
                name.len() as u16,
            );
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            common(&mut out, fields.0, fields.1, fields.2, fields.3, fields.4);
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(body);
            central.push((fields, offset, *name));
        }
        let directory = out.len() as u32;
        for (fields, offset, name) in &central {
            out.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            for v in [20u16, 20, 0] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            common(&mut out, fields.0, fields.1, fields.2, fields.3, fields.4);
            for v in [0u16, 0, 0, 0] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
        }
        let directory_len = out.len() as u32 - directory;
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        for v in [0u16, 0, central.len() as u16, central.len() as u16] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&directory_len.to_le_bytes());
        out.extend_from_slice(&directory.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[tuff_core::watchdog]
    #[test]
    fn output_matches_the_reference_writer_byte_for_byte() {
        let big = vec![b'a'; 4096];
        let noise: Vec<u8> = (0..5000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let entries: Vec<(&str, &[u8])> = vec![
            ("pack.mcmeta", b"{\"pack\":{}}"),
            ("assets/ex/big.txt", &big),
            ("assets/ex/noise.bin", &noise),
            ("assets/ex/empty", b""),
        ];
        assert_eq!(
            write(entries.iter().copied(), usize::MAX).unwrap().as_ref(),
            reference_write(&entries).as_slice()
        );
    }
}
