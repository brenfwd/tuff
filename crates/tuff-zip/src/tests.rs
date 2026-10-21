use std::io::{self, Read};

use super::*;

const GIB: u64 = 1 << 30;

fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect()
}

fn strict() -> Limits {
    Limits::untrusted(64 * GIB)
}

fn read_all<S: Source>(archive: &Archive<S>, name: &str) -> io::Result<Vec<u8>> {
    let entry = archive.find(name).expect("entry exists").clone();
    let mut out = Vec::new();
    archive.reader(&entry)?.read_to_end(&mut out)?;
    Ok(out)
}

fn end_record(bytes: &[u8]) -> usize {
    bytes.len() - END_RECORD_LEN as usize
}

/// Offsets of every central directory header, in order.
fn central_headers(bytes: &[u8]) -> Vec<usize> {
    let end = end_record(bytes);
    let count = u16::from_le_bytes(bytes[end + 10..end + 12].try_into().unwrap());
    let mut at = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
    let mut out = Vec::new();
    for _ in 0..count {
        out.push(at);
        let len =
            |o: usize| u16::from_le_bytes(bytes[at + o..at + o + 2].try_into().unwrap()) as usize;
        at += CENTRAL_HEADER_LEN + len(28) + len(30) + len(32);
    }
    out
}

fn archive_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    write_all(files, DosTime::MIN, 6, u64::MAX).unwrap()
}

#[tuff_core::watchdog]
#[test]
fn pipeline_round_trips_small_large_and_empty_entries() {
    let random = noise(3 * BLOCK + 12_345, 7);
    let text = "the quick brown fox\n".repeat(200_000);
    let exact = noise(BLOCK, 9);
    let small_noise = noise(1000, 3);
    let mut pipeline = Pipeline::new(Writer::new(Vec::new()), 6);
    let time = DosTime::from_unix(1_790_000_000);
    pipeline.add_directory("world/", time).unwrap();
    for (name, data) in [
        ("world/random.bin", random.as_slice()),
        ("world/text.txt", text.as_bytes()),
        ("world/exact.bin", exact.as_slice()),
        ("world/small_noise.bin", small_noise.as_slice()),
        ("world/empty", b"".as_slice()),
        ("world/caf\u{e9}.txt", b"accent".as_slice()),
    ] {
        assert_eq!(
            pipeline.add_reader(name, time, data).unwrap(),
            data.len() as u64
        );
    }
    let bytes = pipeline.finish().unwrap();
    assert!(bytes.len() < random.len() + exact.len() + text.len() / 10);

    let archive = Archive::open(bytes.as_slice(), strict()).unwrap();
    assert_eq!(archive.entries().len(), 7);
    assert!(archive.find("world/").unwrap().is_dir());
    assert_eq!(read_all(&archive, "world/random.bin").unwrap(), random);
    assert_eq!(
        read_all(&archive, "world/text.txt").unwrap(),
        text.as_bytes()
    );
    assert_eq!(read_all(&archive, "world/exact.bin").unwrap(), exact);
    assert_eq!(
        read_all(&archive, "world/small_noise.bin").unwrap(),
        small_noise
    );
    assert_eq!(read_all(&archive, "world/empty").unwrap(), b"");
    assert_eq!(
        read_all(&archive, "world/caf\u{e9}.txt").unwrap(),
        b"accent"
    );
    let entry = archive.find("world/small_noise.bin").unwrap();
    assert_eq!(
        entry.compressed, entry.size,
        "incompressible small entries are stored"
    );
    assert_eq!(entry.modified, time);
    let text_entry = archive.find("world/text.txt").unwrap();
    assert!(text_entry.compressed < text_entry.size / 10);
}

#[tuff_core::watchdog]
#[test]
fn streamed_entries_carry_a_zip64_data_descriptor() {
    let data = noise(BLOCK + 10, 11);
    let mut pipeline = Pipeline::new(Writer::new(Vec::new()), 1);
    pipeline
        .add_reader("big", DosTime::MIN, data.as_slice())
        .unwrap();
    let bytes = pipeline.finish().unwrap();
    let flags = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
    assert_ne!(flags & FLAG_DESCRIPTOR, 0);
    let header = central_headers(&bytes)[0];
    let compressed = u32::from_le_bytes(bytes[header + 20..header + 24].try_into().unwrap());
    let descriptor = header - 24;
    assert_eq!(
        u32::from_le_bytes(bytes[descriptor..descriptor + 4].try_into().unwrap()),
        DESCRIPTOR
    );
    assert_eq!(
        u64::from_le_bytes(bytes[descriptor + 8..descriptor + 16].try_into().unwrap()),
        u64::from(compressed)
    );
    assert_eq!(
        u64::from_le_bytes(bytes[descriptor + 16..descriptor + 24].try_into().unwrap()),
        data.len() as u64
    );
}

#[tuff_core::watchdog]
#[test]
fn more_than_65535_entries_use_the_zip64_end_records() {
    let count = 70_000usize;
    let names: Vec<String> = (0..count).map(|i| format!("d/{i}")).collect();
    let mut writer = Writer::new(Vec::new());
    for name in &names {
        writer
            .add(name, DosTime::MIN, &prepare(name.as_bytes(), 0).unwrap())
            .unwrap();
    }
    let bytes = writer.finish().unwrap();
    let end = end_record(&bytes);
    assert_eq!(
        u16::from_le_bytes(bytes[end + 10..end + 12].try_into().unwrap()),
        u16::MAX
    );
    let archive = Archive::open(bytes.as_slice(), strict()).unwrap();
    assert_eq!(archive.entries().len(), count);
    assert_eq!(read_all(&archive, "d/69999").unwrap(), b"d/69999");
    assert!(Archive::open(bytes.as_slice(), Limits::lenient(count - 1)).is_err());
}

/// `base` bytes of zeros followed by `tail`, without allocating the zeros.
struct Shifted<'a> {
    base: u64,
    tail: &'a [u8],
}

impl Source for Shifted<'_> {
    fn size(&self) -> io::Result<u64> {
        Ok(self.base + self.tail.len() as u64)
    }

    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        for (i, byte) in buf.iter_mut().enumerate() {
            let at = offset + i as u64;
            *byte = if at < self.base {
                0
            } else {
                *self
                    .tail
                    .get((at - self.base) as usize)
                    .ok_or(io::ErrorKind::UnexpectedEof)?
            };
        }
        Ok(())
    }
}

#[tuff_core::watchdog]
#[test]
fn entries_past_4_gib_get_zip64_offsets() {
    let base = 5 * GIB;
    let streamed = noise(2 * BLOCK, 5);
    let mut pipeline = Pipeline::new(Writer::starting_at(Vec::new(), base), 6);
    pipeline
        .add_reader("a.txt", DosTime::MIN, b"first".as_slice())
        .unwrap();
    pipeline
        .add_reader("b.bin", DosTime::MIN, streamed.as_slice())
        .unwrap();
    let bytes = pipeline.finish().unwrap();
    let source = Shifted { base, tail: &bytes };
    let archive = Archive::open(&source, strict()).unwrap();
    let entry = archive.find("a.txt").unwrap();
    assert_eq!(entry.size, 5);
    assert_eq!(read_all(&archive, "a.txt").unwrap(), b"first");
    assert_eq!(read_all(&archive, "b.bin").unwrap(), streamed);
}

#[tuff_core::watchdog]
#[test]
fn an_entry_over_4_gib_round_trips_through_a_sparse_source() {
    let size = 4 * GIB + 3 * BLOCK as u64 + 17;
    let mut pipeline = Pipeline::new(Writer::new(Vec::new()), 1);
    pipeline
        .add_reader("zeros", DosTime::MIN, io::repeat(0).take(size))
        .unwrap();
    pipeline
        .add_reader("after", DosTime::MIN, b"tail".as_slice())
        .unwrap();
    let bytes = pipeline.finish().unwrap();
    assert!((bytes.len() as u64) < size / 100);
    let header = central_headers(&bytes)[0];
    assert_eq!(
        u32::from_le_bytes(bytes[header + 24..header + 28].try_into().unwrap()),
        u32::MAX,
        "the size lives in the zip64 extra field"
    );
    let limits = Limits {
        max_ratio: None,
        ..Limits::untrusted(8 * GIB)
    };
    let archive = Archive::open(bytes.as_slice(), limits).unwrap();
    let entry = archive.find("zeros").unwrap().clone();
    assert_eq!(entry.size, size);
    let mut reader = archive.reader(&entry).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        assert!(buf[..n].iter().all(|&b| b == 0));
        total += n as u64;
    }
    assert_eq!(total, size);
    assert_eq!(read_all(&archive, "after").unwrap(), b"tail");
}

fn refused(bytes: &[u8], needle: &str) {
    let err = Archive::open(bytes, strict()).unwrap_err();
    assert!(err.to_string().contains(needle), "{err} lacks {needle:?}");
    assert!(Archive::open(bytes, Limits::lenient(100)).is_ok());
}

#[tuff_core::watchdog]
#[test]
fn strict_archives_refuse_unsafe_names() {
    for name in [
        "../x",
        "a/../../x",
        "/etc/passwd",
        "a\\b",
        "C:/x",
        "a//b",
        "./a",
        "a/./b",
        "a\u{0}b",
    ] {
        refused(&archive_of(&[(name, b"x")]), "");
    }
    refused(&archive_of(&[("a", b"1"), ("a", b"2")]), "more than once");
    refused(
        &archive_of(&[("Level.dat", b"1"), ("level.dat", b"2")]),
        "more than once",
    );
    refused(
        &archive_of(&[("a", b"1"), ("a/b", b"2")]),
        "both a file and a directory",
    );
    refused(
        &archive_of(&[("a/b", b"1"), ("A", b"2")]),
        "both a file and a directory",
    );
    assert!(
        Archive::open(
            archive_of(&[("a/", b""), ("a/b", b"1")]).as_slice(),
            strict()
        )
        .is_ok()
    );
}

#[tuff_core::watchdog]
#[test]
fn strict_archives_refuse_links_and_special_files() {
    for mode in [0o120_777u32, 0o020_644, 0o060_644, 0o010_644] {
        let mut bytes = archive_of(&[("link", b"/etc/passwd")]);
        let header = central_headers(&bytes)[0];
        bytes[header + 38..header + 42].copy_from_slice(&(mode << 16).to_le_bytes());
        refused(&bytes, "symbolic link");
    }
    let mut bytes = archive_of(&[("file", b"plain")]);
    let header = central_headers(&bytes)[0];
    bytes[header + 38..header + 42].copy_from_slice(&(0o100_644u32 << 16).to_le_bytes());
    assert!(Archive::open(bytes.as_slice(), strict()).is_ok());
}

#[tuff_core::watchdog]
#[test]
fn bombs_are_refused_by_ratio_declared_total_and_inflated_total() {
    let zeros = vec![0u8; 8 << 20];
    let bomb = archive_of(&[("bomb", &zeros)]);
    refused(&bomb, "1000:1");

    let tight = Limits {
        max_ratio: None,
        ..Limits::untrusted(1 << 20)
    };
    assert!(
        Archive::open(bomb.as_slice(), tight)
            .unwrap_err()
            .to_string()
            .contains("over the")
    );

    let text = "abc".repeat(1000);
    let mut lying = archive_of(&[("a", text.as_bytes()), ("b", text.as_bytes())]);
    for header in central_headers(&lying) {
        lying[header + 24..header + 28].copy_from_slice(&10u32.to_le_bytes());
    }
    let archive = Archive::open(lying.as_slice(), strict()).unwrap();
    let err = read_all(&archive, "a").unwrap_err();
    assert!(err.to_string().contains("11+"), "{err}");

    let small_total = Limits {
        max_total_bytes: 4000,
        max_entry_bytes: 4000,
        ..strict()
    };
    let honest = archive_of(&[("a", text.as_bytes()), ("b", text.as_bytes())]);
    assert!(Archive::open(honest.as_slice(), small_total).is_err());
}

#[tuff_core::watchdog]
#[test]
fn inflating_counts_against_the_archive_total() {
    let text = "abc".repeat(1000);
    let bytes = archive_of(&[("a", text.as_bytes()), ("b", text.as_bytes())]);
    let mut limits = strict();
    limits.max_total_bytes = 6000;
    let archive = Archive::open(bytes.as_slice(), limits).unwrap();
    read_all(&archive, "a").unwrap();
    read_all(&archive, "b").unwrap();
    let err = read_all(&archive, "a").unwrap_err();
    assert!(err.to_string().contains("byte limit"), "{err}");
}

#[tuff_core::watchdog]
#[test]
fn overlapping_entries_and_renamed_local_headers_are_refused() {
    let mut bytes = archive_of(&[("a", b"first file"), ("b", b"second file")]);
    let headers = central_headers(&bytes);
    bytes[headers[1] + 42..headers[1] + 46].copy_from_slice(&0u32.to_le_bytes());
    refused(&bytes, "overlap");

    let mut bytes = archive_of(&[("a", b"contents")]);
    bytes[LOCAL_HEADER_LEN as usize] = b'z';
    let archive = Archive::open(bytes.as_slice(), strict()).unwrap();
    assert!(
        read_all(&archive, "a")
            .unwrap_err()
            .to_string()
            .contains("local name")
    );
}

#[tuff_core::watchdog]
#[test]
fn checksum_and_truncation_are_checked_while_reading() {
    let text = "hello world ".repeat(100);
    let mut bytes = archive_of(&[("a", text.as_bytes())]);
    let header = central_headers(&bytes)[0];
    bytes[header + 16] ^= 0xff;
    let archive = Archive::open(bytes.as_slice(), strict()).unwrap();
    assert!(
        read_all(&archive, "a")
            .unwrap_err()
            .to_string()
            .contains("checksum")
    );

    let bytes = archive_of(&[("a", text.as_bytes())]);
    for cut in [1, 10, 22, 40, bytes.len() / 2, bytes.len() - 1] {
        assert!(
            Archive::open(&bytes[..bytes.len() - cut], strict()).is_err(),
            "cut {cut}"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn random_corruption_never_panics() {
    let mut pipeline = Pipeline::new(Writer::new(Vec::new()), 6);
    pipeline.add_directory("d/", DosTime::MIN).unwrap();
    pipeline
        .add_reader("d/a", DosTime::MIN, "say hi\n".repeat(50).as_bytes())
        .unwrap();
    pipeline
        .add_reader("d/b", DosTime::MIN, noise(300, 1).as_slice())
        .unwrap();
    let bytes = pipeline.finish().unwrap();
    let mut state = 0x2545_f491_u32;
    for round in 0..3000 {
        let mut copy = bytes.clone();
        for _ in 0..4 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let at = state as usize % copy.len();
            copy[at] = (state >> 8) as u8;
        }
        let limits = if round % 2 == 0 {
            strict()
        } else {
            Limits::lenient(16)
        };
        if let Ok(archive) = Archive::open(copy.as_slice(), limits) {
            for entry in archive.entries() {
                if let Ok(mut reader) = archive.reader(entry) {
                    let _ = reader.read_to_end(&mut Vec::new());
                }
            }
        }
    }
}

#[tuff_core::watchdog]
#[test]
fn extraction_recreates_files_and_stops_when_asked() {
    let dir = std::env::temp_dir().join(format!("tuff-zip-extract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let bytes = archive_of(&[
        ("w/level.dat", b"level"),
        ("w/region/r.0.0.mca", &noise(5000, 2)),
    ]);
    let archive = Archive::open(bytes.as_slice(), strict()).unwrap();
    assert_eq!(archive.extract_to(&dir, |_| Ok(())).unwrap(), 5005);
    assert_eq!(std::fs::read(dir.join("w/level.dat")).unwrap(), b"level");
    assert_eq!(
        std::fs::read(dir.join("w/region/r.0.0.mca")).unwrap(),
        noise(5000, 2)
    );
    assert!(
        archive.extract_to(&dir, |_| Ok(())).is_err(),
        "never overwrites"
    );

    let other = dir.join("again");
    std::fs::create_dir_all(&other).unwrap();
    let err = archive
        .extract_to(&other, |_| Err(io::Error::other("cancelled")))
        .unwrap_err();
    assert_eq!(err.to_string(), "cancelled");
    assert!(
        Archive::open(bytes.as_slice(), Limits::lenient(16))
            .unwrap()
            .extract_to(&other, |_| Ok(()))
            .is_err()
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tuff_core::watchdog]
#[test]
fn dos_times_are_utc_and_clamped() {
    let time = DosTime::from_unix(1_791_030_896);
    assert_eq!(time.date, ((2026 - 1980) << 9) | (10 << 5) | 3);
    assert_eq!(time.time, (12 << 11) | (34 << 5) | 28);
    assert_eq!(DosTime::from_unix(0), DosTime::MIN);
    assert_eq!(DosTime::from_unix(u64::from(u32::MAX) * 4).date >> 9, 127);
}
