use std::fs;
use std::path::Path;

use tuff_vanilla_data::version::DATA_VERSION;

use super::restore::*;
use super::*;
use crate::backend::WorldBackend;
use crate::document::Document;
use crate::sqlite::SqliteBackend;
use crate::test_dir::TestDir;
use tuff_nbt::{Compound, Compression, NamedTag};
use tuff_zip::{CENTRAL_HEADER_LEN, DosTime, END_RECORD_LEN, write_all};

const MIB: u64 = 1 << 20;
const CAP: u64 = 256 * MIB;

fn level_dat(version: i32) -> Vec<u8> {
    let root = Compound::new().with("Data", Compound::new().with("DataVersion", version));
    NamedTag::new("", root).to_bytes(Compression::Gzip).unwrap()
}

fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    write_all(entries, DosTime::MIN, 6, u64::MAX).unwrap()
}

fn world_zip(extra: &[(&str, &[u8])]) -> Vec<u8> {
    let level = level_dat(DATA_VERSION);
    let mut entries: Vec<(&str, &[u8])> = vec![("world/level.dat", &level)];
    entries.extend_from_slice(extra);
    archive(&entries)
}

fn write(dir: &TestDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// Offsets of every central directory header.
fn central_headers(bytes: &[u8]) -> Vec<usize> {
    let end = bytes.len() - END_RECORD_LEN as usize;
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
    let count = u16_at(end + 10);
    let mut at = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
    let mut out = Vec::new();
    for _ in 0..count {
        out.push(at);
        at += CENTRAL_HEADER_LEN + u16_at(at + 28) + u16_at(at + 30) + u16_at(at + 32);
    }
    out
}

fn local_header(bytes: &[u8], central: usize) -> usize {
    u32::from_le_bytes(bytes[central + 42..central + 46].try_into().unwrap()) as usize
}

fn put_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn refused(path: &Path, cap: u64) -> String {
    match inspect(path, cap) {
        Err(RestoreError::Invalid(why)) => why,
        other => panic!("{} was not refused as invalid: {other:?}", path.display()),
    }
}

#[tuff_core::watchdog]
#[test]
fn a_real_world_with_a_huge_run_of_one_byte_is_accepted() {
    let world = TestDir::new();
    let out = TestDir::new();
    fs::create_dir_all(world.path().join("region")).unwrap();
    fs::write(world.path().join("level.dat"), level_dat(DATA_VERSION)).unwrap();
    fs::write(
        world.path().join("region/r.0.0.mca"),
        vec![0u8; 24 * MIB as usize],
    )
    .unwrap();
    let plan = Plan {
        source: BackupSource::Folder(world.path().to_path_buf()),
        dir: out.path().to_path_buf(),
        level: "world".to_owned(),
        compression: 9,
        staged: false,
        include_caches: false,
    };
    let (tracker, _) = Tracker::new();
    let (name, _) = run_offline(&plan, &tracker).unwrap();
    let path = out.path().join(&name);
    let zip = tuff_zip::Archive::open(fs::File::open(&path).unwrap(), zip_limits(CAP)).unwrap();
    let region = zip.find("world/region/r.0.0.mca").unwrap();
    assert!(
        region.size > region.compressed * 1000,
        "the run compresses past 1000:1 ({} from {})",
        region.size,
        region.compressed
    );
    assert!(
        tuff_zip::Archive::open(
            fs::File::open(&path).unwrap(),
            tuff_zip::Limits::untrusted(CAP)
        )
        .is_err(),
        "a 1000:1 cap would refuse this world"
    );
    let checked = inspect(&path, CAP).unwrap();
    assert_eq!(checked.kind, Kind::Folder);
    assert_eq!(checked.root, "world/");
    assert_eq!(checked.data_version, DATA_VERSION);
    assert_eq!(checked.files, 2);
}

#[tuff_core::watchdog]
#[test]
fn bombs_and_lying_headers_are_refused() {
    let dir = TestDir::new();
    let zeros = vec![0u8; 4 * MIB as usize];
    let bomb = write(
        &dir,
        "bomb.zip",
        &world_zip(&[("world/a", &zeros), ("world/b", &zeros)]),
    );
    assert!(refused(&bomb, 6 * MIB).contains("limit"));
    assert!(inspect(&bomb, 16 * MIB).unwrap().unpacked > 8 * MIB);

    let mut lying = world_zip(&[("world/a", &zeros)]);
    let central = central_headers(&lying)[1];
    let local = local_header(&lying, central);
    put_u32(&mut lying, central + 24, 1000);
    put_u32(&mut lying, local + 22, 1000);
    let lying = write(&dir, "lying.zip", &lying);
    assert!(refused(&lying, CAP).contains("claims"));

    let mut beyond = world_zip(&[("world/a", &zeros)]);
    let central = central_headers(&beyond)[1];
    let compressed = u32::from_le_bytes(beyond[central + 20..central + 24].try_into().unwrap());
    put_u32(
        &mut beyond,
        central + 24,
        compressed * (DEFLATE_MAX_RATIO as u32 + 1),
    );
    let beyond = write(&dir, "beyond.zip", &beyond);
    assert!(refused(&beyond, CAP).contains("compresses more than"));
}

#[tuff_core::watchdog]
#[test]
fn hostile_and_malformed_archives_are_refused() {
    let dir = TestDir::new();
    let level = level_dat(DATA_VERSION);
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "traversal",
            archive(&[("world/level.dat", &level), ("world/../evil", b"x")]),
            "component",
        ),
        (
            "absolute",
            archive(&[("level.dat", &level), ("/etc/evil", b"x")]),
            "absolute",
        ),
        (
            "backslash",
            archive(&[("world/level.dat", &level), ("world\\..\\evil", b"x")]),
            "backslash",
        ),
        (
            "duplicate",
            archive(&[
                ("world/level.dat", &level),
                ("world/a", b"x"),
                ("world/a", b"y"),
            ]),
            "more than once",
        ),
        (
            "case folded",
            archive(&[
                ("world/level.dat", &level),
                ("world/A", b"x"),
                ("world/a", b"y"),
            ]),
            "more than once",
        ),
        (
            "no world",
            archive(&[("world/region/r.0.0.mca", b"x")]),
            "no world",
        ),
        (
            "two worlds",
            archive(&[("a/level.dat", &level), ("b/level.dat", &level)]),
            "more than one world",
        ),
        (
            "stray",
            archive(&[("world/level.dat", &level), ("other/x", b"x")]),
            "outside the world folder",
        ),
        (
            "newer",
            archive(&[("world/level.dat", &level_dat(DATA_VERSION + 1))]),
            "newer version",
        ),
        (
            "not nbt",
            archive(&[("world/level.dat", b"not nbt at all")]),
            "unreadable",
        ),
        ("garbage", b"definitely not an archive".to_vec(), "neither"),
    ];
    for (name, bytes, why) in cases {
        let path = write(&dir, &format!("{name}.zip"), &bytes);
        let found = refused(&path, CAP);
        assert!(found.contains(why), "{name}: {found}");
    }

    let mut deep = Compound::new().with("DataVersion", DATA_VERSION);
    for _ in 0..100 {
        deep = Compound::new().with("x", deep);
    }
    let deep = NamedTag::new("", Compound::new().with("Data", deep))
        .to_bytes(Compression::Gzip)
        .unwrap();
    let path = write(&dir, "deep.zip", &archive(&[("world/level.dat", &deep)]));
    assert!(refused(&path, CAP).contains("unreadable"));

    let mut link = world_zip(&[("world/link", b"/etc/passwd")]);
    let central = central_headers(&link)[1];
    put_u32(&mut link, central + 38, 0o120_777 << 16);
    let path = write(&dir, "link.zip", &link);
    assert!(refused(&path, CAP).contains("symbolic link"));

    let whole = world_zip(&[("world/a", b"some data")]);
    let path = write(&dir, "truncated.zip", &whole[..whole.len() - 30]);
    refused(&path, CAP);
    let mut corrupt = world_zip(&[("world/a", &vec![7u8; 4096])]);
    let central = central_headers(&corrupt)[1];
    put_u32(&mut corrupt, central + 16, 0xdead_beef);
    let local = local_header(&corrupt, central);
    put_u32(&mut corrupt, local + 14, 0xdead_beef);
    let path = write(&dir, "checksum.zip", &corrupt);
    assert!(refused(&path, CAP).contains("checksum"));
}

fn server_file(dir: &TestDir, name: &str, version: i32) -> std::path::PathBuf {
    let path = dir.path().join(name);
    let backend = SqliteBackend::open_with(&path, crate::sqlite::Locking::Exclusive).unwrap();
    backend
        .documents()
        .write(
            Document::Level,
            Compound::new().with("Data", Compound::new().with("DataVersion", version)),
        )
        .unwrap();
    drop(backend);
    path
}

#[tuff_core::watchdog]
#[test]
fn server_files_are_checked_without_trusting_them() {
    let dir = TestDir::new();
    let good = server_file(&dir, "good.mcserver", DATA_VERSION);
    let checked = inspect(&good, CAP).unwrap();
    assert_eq!(checked.kind, Kind::ServerFile);
    assert_eq!(checked.data_version, DATA_VERSION);
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(leftovers, ["good.mcserver"], "the check writes nothing");

    let newer = server_file(&dir, "newer.mcserver", DATA_VERSION + 1);
    assert!(refused(&newer, CAP).contains("newer version"));

    let plain = dir.path().join("plain.db");
    let conn = rusqlite::Connection::open(&plain).unwrap();
    conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
        .unwrap();
    drop(conn);
    assert!(refused(&plain, CAP).contains("not a server file"));

    let mut damaged = fs::read(&good).unwrap();
    let page = 4096;
    for b in &mut damaged[page + 8..2 * page] {
        *b = 0xA5;
    }
    let damaged = write(&dir, "damaged.mcserver", &damaged);
    refused(&damaged, CAP);

    let mut header_only = b"SQLite format 3\0".to_vec();
    header_only.extend_from_slice(&[0xFF; 200]);
    let header_only = write(&dir, "header.mcserver", &header_only);
    refused(&header_only, CAP);
}

fn folder_world(root: &Path, marker: &str) {
    fs::create_dir_all(root.join("region")).unwrap();
    fs::write(root.join("level.dat"), level_dat(DATA_VERSION)).unwrap();
    fs::write(root.join("region/r.0.0.mca"), marker).unwrap();
}

#[tuff_core::watchdog]
#[test]
fn a_staged_restore_replaces_the_world_only_when_applied() {
    let instance = TestDir::new();
    let world = instance.path().join("world");
    folder_world(&world, "backed up");
    let backups = instance.path().join("backups");
    let plan = Plan {
        source: BackupSource::Folder(world.clone()),
        dir: backups.clone(),
        level: "world".to_owned(),
        compression: 6,
        staged: true,
        include_caches: false,
    };
    let (tracker, _) = Tracker::new();
    let (name, _) = run_offline(&plan, &tracker).unwrap();
    fs::write(world.join("region/r.0.0.mca"), "changed later").unwrap();

    let server = server_file(&instance, "other.mcserver", DATA_VERSION);
    assert!(matches!(
        stage(&server, "other", &world, Kind::Folder, CAP, "t", |_, _| Ok(
            ()
        )),
        Err(RestoreError::Mismatch(_))
    ));
    let mut seen = 0;
    let staged = stage(
        &backups.join(&name),
        &name,
        &world,
        Kind::Folder,
        CAP,
        "alex",
        |done, _| {
            seen = done;
            Ok(())
        },
    )
    .unwrap();
    assert!(seen > 0);
    assert_eq!(staged.backup, name);
    assert_eq!(super::restore::staged(&world), Some(staged.clone()));
    assert_eq!(
        fs::read_to_string(world.join("region/r.0.0.mca")).unwrap(),
        "changed later",
        "staging leaves the live world alone"
    );

    let mut probed = false;
    let applied = apply_staged(&world, || {
        probed = true;
        Ok(())
    })
    .unwrap()
    .unwrap();
    assert!(probed);
    assert_eq!(applied.backup, name);
    assert_eq!(
        fs::read_to_string(world.join("region/r.0.0.mca")).unwrap(),
        "backed up"
    );
    let previous = applied.previous.unwrap();
    assert!(
        previous
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("world-before-restore-")
    );
    assert_eq!(
        fs::read_to_string(previous.join("region/r.0.0.mca")).unwrap(),
        "changed later"
    );
    assert_eq!(super::restore::staged(&world), None);
    assert_eq!(
        apply_staged(&world, || panic!("nothing to apply")).unwrap(),
        None
    );
}

#[tuff_core::watchdog]
#[test]
fn applying_is_safe_to_repeat_and_refuses_a_world_in_use() {
    let instance = TestDir::new();
    let world = instance.path().join("world");
    folder_world(&world, "live");
    let backup = instance.path().join("world-20261003-120000.zip");
    let level = level_dat(DATA_VERSION);
    fs::write(
        &backup,
        archive(&[("level.dat", &level), ("region/r.0.0.mca", b"restored")]),
    )
    .unwrap();
    stage(&backup, "b", &world, Kind::Folder, CAP, "t", |_, _| Ok(())).unwrap();

    let busy = apply_staged(&world, || Err(std::io::ErrorKind::WouldBlock.into()));
    assert!(busy.is_err());
    assert_eq!(
        fs::read_to_string(world.join("region/r.0.0.mca")).unwrap(),
        "live"
    );

    fs::rename(&world, instance.path().join("moved-by-a-crash")).unwrap();
    let applied = apply_staged(&world, || panic!("no world to probe"))
        .unwrap()
        .unwrap();
    assert_eq!(applied.previous, None);
    assert_eq!(
        fs::read_to_string(world.join("region/r.0.0.mca")).unwrap(),
        "restored"
    );

    let paths = stage_paths(&world).unwrap();
    fs::create_dir_all(paths.partial.join("x")).unwrap();
    fs::create_dir_all(&paths.staged).unwrap();
    assert_eq!(apply_staged(&world, || Ok(())).unwrap(), None);
    assert!(!paths.partial.exists() && !paths.staged.exists());
}

#[tuff_core::watchdog]
#[test]
fn a_server_file_restore_moves_the_old_file_and_its_log_aside() {
    let instance = TestDir::new();
    let world = server_file(&instance, "world.mcserver", DATA_VERSION);
    let backup = server_file(
        &instance,
        "world-20261003-120000.mcserver",
        DATA_VERSION - 1,
    );
    fs::write(instance.path().join("world.mcserver-wal"), "stale log").unwrap();
    assert!(matches!(
        stage(&backup, "b", &world, Kind::Folder, CAP, "t", |_, _| Ok(())),
        Err(RestoreError::Mismatch(_))
    ));
    stage(&backup, "b", &world, Kind::ServerFile, CAP, "t", |_, _| {
        Ok(())
    })
    .unwrap();
    let applied = apply_staged(&world, || Ok(())).unwrap().unwrap();
    let previous = applied.previous.unwrap();
    let previous_name = previous.file_name().unwrap().to_str().unwrap().to_owned();
    assert!(
        previous_name.starts_with("world-before-restore-"),
        "{previous_name}"
    );
    assert!(previous_name.ends_with(".mcserver"), "{previous_name}");
    assert!(!instance.path().join("world.mcserver-wal").exists());
    assert_eq!(
        fs::read_to_string(format!("{}-wal", previous.display())).unwrap(),
        "stale log"
    );
    assert_eq!(inspect(&world, CAP).unwrap().data_version, DATA_VERSION - 1);
}

#[tuff_core::watchdog]
#[test]
fn uploads_get_backup_names_and_leftovers_are_cleaned() {
    let dir = TestDir::new();
    let temp = upload_path(dir.path());
    fs::write(&temp, b"zip bytes").unwrap();
    let stale = upload_path(dir.path());
    fs::write(&stale, b"half").unwrap();
    let info = publish_upload(dir.path(), "world", &temp, Kind::Folder).unwrap();
    assert!(parse_name("world", &info.name).is_some(), "{}", info.name);
    assert!(info.name.ends_with(".zip"));
    assert_eq!(list(dir.path(), "world").unwrap(), [info]);
    clean_uploads(dir.path()).unwrap();
    assert!(!stale.exists());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}
