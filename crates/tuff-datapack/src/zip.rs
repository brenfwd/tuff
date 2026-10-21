//! The zip archives data packs ship as, read through [`tuff_zip`]:
//! archives are untrusted, so an entry is only inflated after its declared
//! size passes the caller's limit, inflation stops one byte past the
//! declared size, and the result must match both the declared size and its
//! CRC-32. Multi-disk and encrypted archives are refused; entries using
//! other compression methods cannot be read.

use std::io;

use tuff_zip::{self as core_zip, DosTime, Limits};

/// An archive's directory over its bytes.
#[derive(Debug)]
pub struct Archive<'a> {
    inner: core_zip::Archive<&'a [u8]>,
    names: Vec<String>,
}

impl<'a> Archive<'a> {
    /// Reads the central directory of `bytes`, refusing more than
    /// `max_entries` entries.
    pub fn open(bytes: &'a [u8], max_entries: usize) -> io::Result<Self> {
        let inner = core_zip::Archive::open(bytes, Limits::lenient(max_entries))?;
        let mut names: Vec<String> = inner
            .entries()
            .iter()
            .filter(|entry| !entry.is_dir())
            .map(|entry| entry.name.clone())
            .collect();
        names.sort();
        names.dedup();
        Ok(Self { inner, names })
    }

    /// Every file's name, sorted.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn contains(&self, name: &str) -> bool {
        self.inner.find(name).is_some_and(|entry| !entry.is_dir())
    }

    /// The declared size of `name`, if it exists.
    pub fn size(&self, name: &str) -> Option<u64> {
        self.inner
            .find(name)
            .filter(|entry| !entry.is_dir())
            .map(|entry| entry.size)
    }

    /// The contents of `name`, refusing anything that claims or turns out
    /// to hold more than `max` bytes.
    pub fn read(&self, name: &str, max: usize) -> io::Result<Vec<u8>> {
        if !self.contains(name) {
            return Err(io::Error::new(io::ErrorKind::NotFound, name.to_owned()));
        }
        self.inner.read(name, max)
    }
}

/// A zip archive holding `files` (name, contents), each deflated unless
/// that would not make it smaller. For tests and tools that build packs.
pub fn write(files: &[(&str, &[u8])]) -> Vec<u8> {
    core_zip::write_all(files, DosTime::MIN, 6, u64::MAX).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: usize = 1024 * 1024;

    fn sample() -> Vec<u8> {
        write(&[
            ("pack.mcmeta", br#"{"pack":{"description":"x"}}"#),
            (
                "data/a/function/f.mcfunction",
                "say hi\n".repeat(50).as_bytes(),
            ),
            ("data/", b""),
        ])
    }

    fn find_end_record(bytes: &[u8]) -> usize {
        bytes.len() - 22
    }

    #[tuff_core::watchdog]
    #[test]
    fn reads_stored_and_deflated_entries() {
        let bytes = sample();
        let archive = Archive::open(&bytes, 16).unwrap();
        assert_eq!(
            archive.names(),
            ["data/a/function/f.mcfunction", "pack.mcmeta"].map(String::from)
        );
        assert_eq!(
            archive.read("pack.mcmeta", MB).unwrap(),
            br#"{"pack":{"description":"x"}}"#
        );
        assert_eq!(
            archive.read("data/a/function/f.mcfunction", MB).unwrap(),
            "say hi\n".repeat(50).as_bytes()
        );
        assert_eq!(
            archive.read("missing", MB).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn refuses_entries_over_the_limit_before_inflating() {
        let big = vec![0u8; 4 * MB];
        let bytes = write(&[("bomb", &big)]);
        assert!(bytes.len() < 64 * 1024, "zeros compress well");
        let archive = Archive::open(&bytes, 16).unwrap();
        let err = archive.read("bomb", MB).unwrap_err();
        assert!(err.to_string().contains("over the"), "{err}");
        assert_eq!(archive.read("bomb", 4 * MB).unwrap().len(), 4 * MB);
    }

    fn patch_u32(bytes: &mut [u8], at: usize, value: u32) {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn central_header(bytes: &[u8]) -> usize {
        let end = find_end_record(bytes);
        u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_lying_size_stops_inflation_one_byte_past_the_claim() {
        let big = vec![0u8; 4 * MB];
        let mut bytes = write(&[("bomb", &big)]);
        let header = central_header(&bytes);
        patch_u32(&mut bytes, header + 24, 1000);
        let archive = Archive::open(&bytes, 16).unwrap();
        let err = archive.read("bomb", MB).unwrap_err();
        assert!(err.to_string().contains("1001+"), "{err}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn checksums_and_truncation_are_checked() {
        let contents = "x".repeat(1000);
        let mut bytes = write(&[("f", contents.as_bytes())]);
        let header = central_header(&bytes);
        patch_u32(&mut bytes, header + 16, 0xDEAD_BEEF);
        let archive = Archive::open(&bytes, 16).unwrap();
        assert!(
            archive
                .read("f", MB)
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );

        let mut bytes = write(&[("f", contents.as_bytes())]);
        let header = central_header(&bytes);
        patch_u32(&mut bytes, header + 20, 1 << 30);
        let archive = Archive::open(&bytes, 16).unwrap();
        assert!(
            archive
                .read("f", MB)
                .unwrap_err()
                .to_string()
                .contains("truncated")
        );

        let mut bytes = write(&[("f", contents.as_bytes())]);
        let header = central_header(&bytes);
        patch_u32(&mut bytes, header + 42, 1 << 30);
        let archive = Archive::open(&bytes, 16).unwrap();
        assert!(
            archive
                .read("f", MB)
                .unwrap_err()
                .to_string()
                .contains("outside")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn malformed_archives_are_refused() {
        assert!(Archive::open(b"", 16).is_err());
        assert!(Archive::open(b"PK\x03\x04 not really", 16).is_err());
        let bytes = sample();
        for cut in [1, 10, 22, 40, bytes.len() / 2] {
            assert!(
                Archive::open(&bytes[..bytes.len() - cut], 16).is_err(),
                "cut {cut}"
            );
        }
        assert!(
            Archive::open(&bytes, 2)
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
        let mut garbage = bytes.clone();
        let header = central_header(&garbage);
        garbage[header] = b'X';
        assert!(Archive::open(&garbage, 16).is_err());
        let mut far = bytes.clone();
        let end = find_end_record(&far);
        patch_u32(&mut far, end + 16, u32::MAX - 1);
        assert!(Archive::open(&far, 16).is_err());
        let mut zip64 = bytes;
        let end = find_end_record(&zip64);
        patch_u32(&mut zip64, end + 12, u32::MAX);
        assert!(
            Archive::open(&zip64, 16)
                .unwrap_err()
                .to_string()
                .contains("zip64")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn encrypted_and_unknown_methods_cannot_be_read() {
        let mut bytes = write(&[("f", b"abc")]);
        let header = central_header(&bytes);
        bytes[header + 8] |= 1;
        let archive = Archive::open(&bytes, 16).unwrap();
        assert!(
            archive
                .read("f", MB)
                .unwrap_err()
                .to_string()
                .contains("encrypted")
        );
        let mut bytes = write(&[("f", b"abc")]);
        let header = central_header(&bytes);
        bytes[header + 10] = 14;
        let archive = Archive::open(&bytes, 16).unwrap();
        assert!(
            archive
                .read("f", MB)
                .unwrap_err()
                .to_string()
                .contains("method")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn random_corruption_never_panics() {
        let bytes = sample();
        let mut state = 0x2545_f491_u32;
        for _ in 0..2000 {
            let mut copy = bytes.clone();
            for _ in 0..4 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let at = state as usize % copy.len();
                copy[at] = (state >> 8) as u8;
            }
            if let Ok(archive) = Archive::open(&copy, 16) {
                for name in archive.names() {
                    let _ = archive.read(name, MB);
                }
            }
        }
    }
}
