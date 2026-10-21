use std::path::{Path, PathBuf};

use super::*;
use crate::datapack::{discover, meta};
use tuff_datapack::zip;
use tuff_storage::test_dir::TestDir;

fn meta_json(description: &str) -> String {
    let current = meta::Format::current().major;
    format!(
        r#"{{"pack":{{"description":"{description}","min_format":{current},"max_format":{current}}}}}"#
    )
}

fn zip_pack(description: &str) -> Upload {
    Upload::Zip(zip::write(&[
        ("pack.mcmeta", meta_json(description).as_bytes()),
        ("data/a/function/f.mcfunction", b"say hi"),
    ]))
}

fn folder_pack(description: &str) -> Upload {
    Upload::Files(vec![
        (
            "pack.mcmeta".to_owned(),
            meta_json(description).into_bytes(),
        ),
        (
            "data/a/function/g.mcfunction".to_owned(),
            b"say there".to_vec(),
        ),
    ])
}

fn level(enabled: &[&str], disabled: &[&str]) -> Compound {
    let list = |names: &[&str]| Tag::List(names.iter().map(|n| Tag::from(*n)).collect());
    Compound::new().with(
        "Data",
        Compound::new().with(
            "DataPacks",
            Compound::new()
                .with("Enabled", list(enabled))
                .with("Disabled", list(disabled)),
        ),
    )
}

fn vanilla_world() -> (TestDir, Arc<dyn WorldBackend>) {
    let dir = TestDir::new();
    let backend: Arc<dyn WorldBackend> = Arc::new(VanillaBackend::new(dir.path().to_path_buf()));
    (dir, backend)
}

fn sqlite_world() -> (TestDir, Arc<dyn WorldBackend>) {
    let dir = TestDir::new();
    let backend: Arc<dyn WorldBackend> =
        Arc::new(SqliteBackend::open(dir.path().join("world.mcserver")).unwrap());
    (dir, backend)
}

fn ids(backend: &dyn WorldBackend) -> Vec<String> {
    discover(backend)
        .into_iter()
        .filter(|p| p.source != crate::datapack::PackSource::Feature)
        .map(|p| p.id)
        .collect()
}

fn names(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(|e| e.name.as_str()).collect()
}

fn add_list_remove(backend: &dyn WorldBackend) {
    let zipped = add(backend, "zipped", &zip_pack("a zip"), false).unwrap();
    assert_eq!(zipped.id(), "file/zipped.zip");
    assert_eq!(zipped.files, 2);
    assert!(!zipped.replaced);
    let folder = add(backend, "folder", &folder_pack("a folder"), false).unwrap();
    assert_eq!(folder.id(), "file/folder");
    assert_eq!(folder.meta.description.plain(), "a folder");
    assert_eq!(ids(backend), ["vanilla", "file/folder", "file/zipped.zip"]);

    let entries = list(backend).unwrap();
    assert_eq!(names(&entries), ["folder", "zipped.zip"]);
    assert_eq!(entries[0].kind, Kind::Directory);
    assert_eq!(entries[0].files(), 2);
    assert_eq!(entries[1].kind, Kind::Zip);
    assert_eq!(
        entries[1].meta.as_ref().unwrap().description.plain(),
        "a zip"
    );
    assert!(entries.iter().all(|e| e.selection == Selection::New));

    let removed = remove(backend, "file/folder").unwrap();
    assert_eq!(removed.kind, Kind::Directory);
    let removed = remove(backend, "zipped.zip").unwrap();
    assert_eq!(removed.kind, Kind::Zip);
    assert_eq!(ids(backend), ["vanilla"]);
    assert!(list(backend).unwrap().is_empty());
    assert!(matches!(
        remove(backend, "folder"),
        Err(StoreError::NotFound(_))
    ));
}

#[tuff_core::watchdog]
#[test]
fn packs_are_added_listed_and_removed_in_a_folder_world() {
    let (dir, backend) = vanilla_world();
    add_list_remove(&*backend);
    assert!(!dir.path().join("datapacks").exists());
}

#[tuff_core::watchdog]
#[test]
fn packs_are_added_listed_and_removed_in_a_server_file() {
    let (_dir, backend) = sqlite_world();
    add_list_remove(&*backend);
}

#[tuff_core::watchdog]
#[test]
fn an_existing_pack_is_kept_unless_replaced() {
    for (_dir, backend) in [vanilla_world(), sqlite_world()] {
        let backend = &*backend;
        add(
            backend,
            "p",
            &Upload::Files(vec![
                ("pack.mcmeta".to_owned(), meta_json("old").into_bytes()),
                (
                    "data/a/function/stale.mcfunction".to_owned(),
                    b"say".to_vec(),
                ),
            ]),
            false,
        )
        .unwrap();
        let err = add(backend, "p", &folder_pack("new"), false).unwrap_err();
        assert!(matches!(err, StoreError::Exists(_)), "{err}");
        let added = add(backend, "p", &folder_pack("new"), true).unwrap();
        assert!(added.replaced);
        let files = backend.list_extra_files_under("datapacks/p/").unwrap();
        assert_eq!(
            files,
            [
                "datapacks/p/data/a/function/g.mcfunction",
                "datapacks/p/pack.mcmeta"
            ]
        );
        assert_eq!(
            list(backend).unwrap()[0]
                .meta
                .as_ref()
                .unwrap()
                .description
                .plain(),
            "new"
        );
    }
}

#[tuff_core::watchdog]
#[test]
fn malformed_packs_are_refused_and_nothing_is_written() {
    let (_dir, backend) = vanilla_world();
    let backend = &*backend;
    let refuse = |upload: Upload, expect: &str| {
        let err = add(backend, "bad", &upload, false).unwrap_err();
        assert!(err.to_string().contains(expect), "{err} lacks {expect:?}");
        assert!(
            backend
                .list_extra_files_under("datapacks/")
                .unwrap()
                .is_empty(),
            "{err}"
        );
    };
    refuse(Upload::Zip(b"PK not a zip".to_vec()), "unreadable zip");
    refuse(
        Upload::Zip(zip::write(&[("data/a/x.json", b"{}")])),
        "no pack.mcmeta",
    );
    refuse(
        Upload::Zip(zip::write(&[
            ("inner/pack.mcmeta", meta_json("x").as_bytes()),
            ("inner/data/a/x.json", b"{}"),
        ])),
        "inside inner/",
    );
    refuse(
        Upload::Zip(zip::write(&[("pack.mcmeta", b"{not json")])),
        "pack.mcmeta",
    );
    refuse(
        Upload::Files(vec![("pack.mcmeta".to_owned(), b"[]".to_vec())]),
        "unreadable pack.mcmeta",
    );
    refuse(Upload::Files(Vec::new()), "no pack.mcmeta");
    let mut corrupt = zip::write(&[
        ("pack.mcmeta", meta_json("x").as_bytes()),
        ("data/a/function/f.mcfunction", &[b'a'; 4096]),
    ]);
    let at = corrupt
        .windows(4)
        .rposition(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .unwrap();
    let crc_at = at + 16;
    corrupt[crc_at] ^= 0xff;
    refuse(Upload::Zip(corrupt), "checksum");
}

#[tuff_core::watchdog]
#[test]
fn oversized_packs_are_refused() {
    let (_dir, backend) = vanilla_world();
    let backend = &*backend;
    let big = vec![0u8; MAX_FILE + 1];
    let err = add(
        backend,
        "big",
        &Upload::Zip(zip::write(&[
            ("pack.mcmeta", meta_json("x").as_bytes()),
            ("data/a/big.json", &big),
        ])),
        false,
    )
    .unwrap_err();
    assert!(matches!(err, StoreError::TooLarge(_)), "{err}");
    let err = add(
        backend,
        "big",
        &Upload::Files(vec![
            ("pack.mcmeta".to_owned(), meta_json("x").into_bytes()),
            ("data/a/big.json".to_owned(), big),
        ]),
        false,
    )
    .unwrap_err();
    assert!(matches!(err, StoreError::TooLarge(_)), "{err}");
    assert!(
        backend
            .list_extra_files_under("datapacks/")
            .unwrap()
            .is_empty()
    );

    let dir = TestDir::new();
    let archive = dir.path().join("huge.zip");
    File::create(&archive)
        .unwrap()
        .set_len(MAX_ARCHIVE as u64 + 1)
        .unwrap();
    let err = Upload::read_path(&archive).unwrap_err();
    assert!(matches!(err, StoreError::TooLarge(_)), "{err}");
}

#[tuff_core::watchdog]
#[test]
fn traversal_and_bad_names_are_refused() {
    let (dir, backend) = vanilla_world();
    let backend = &*backend;
    for name in [
        "../escape",
        "a/b",
        "a\\b",
        ".hidden",
        "",
        "CON",
        "trailing.",
        "spaced name",
        &"x".repeat(MAX_NAME + 1),
    ] {
        let err = add(backend, name, &folder_pack("x"), false).unwrap_err();
        assert!(matches!(err, StoreError::Name(_)), "{name:?}: {err}");
    }
    let err = add(backend, "folder.zip", &folder_pack("x"), false).unwrap_err();
    assert!(matches!(err, StoreError::Name(_)), "{err}");
    for path in [
        "../outside.txt",
        "/etc/passwd",
        "data/../../x",
        "data\\..\\x",
        "data//x",
        "./pack.mcmeta",
        "c:/x",
        "data/a\u{0}b",
    ] {
        let files = Upload::Files(vec![
            ("pack.mcmeta".to_owned(), meta_json("x").into_bytes()),
            (path.to_owned(), b"x".to_vec()),
        ]);
        let err = add(backend, "p", &files, false).unwrap_err();
        assert!(matches!(err, StoreError::Name(_)), "{path:?}: {err}");
        let archive = Upload::Zip(zip::write(&[
            ("pack.mcmeta", meta_json("x").as_bytes()),
            (path, b"x"),
        ]));
        let err = add(backend, "p", &archive, false).unwrap_err();
        assert!(matches!(err, StoreError::Name(_)), "{path:?}: {err}");
    }
    let twice = Upload::Files(vec![
        ("pack.mcmeta".to_owned(), meta_json("x").into_bytes()),
        ("pack.mcmeta".to_owned(), meta_json("y").into_bytes()),
    ]);
    assert!(matches!(
        add(backend, "p", &twice, false),
        Err(StoreError::Name(_))
    ));
    for name in ["../escape", "a/b", ""] {
        assert!(matches!(remove(backend, name), Err(StoreError::Name(_))));
    }
    assert!(backend.write_extra_file("../outside", b"x").is_err());
    assert!(backend.remove_extra_file("../outside").is_err());
    assert!(!dir.path().parent().unwrap().join("outside").exists());
    assert!(!dir.path().parent().unwrap().join("outside.txt").exists());
    assert!(
        backend
            .list_extra_files_under("datapacks/")
            .unwrap()
            .is_empty()
    );
}

#[tuff_core::watchdog]
#[test]
fn names_are_normalised() {
    let zip = zip_pack("x");
    let folder = folder_pack("x");
    assert_eq!(stored_name("p", &zip).unwrap(), "p.zip");
    assert_eq!(stored_name("file/p.zip", &zip).unwrap(), "p.zip");
    assert_eq!(stored_name("p", &folder).unwrap(), "p");
    assert_eq!(
        suggested_name(Path::new("/tmp/My Pack (v2).zip")).as_deref(),
        Some("My_Pack__v2_.zip")
    );
    assert_eq!(suggested_name(Path::new("..")), None);
    let long = format!("{}.zip", "a".repeat(100));
    let name = suggested_name(Path::new(&long)).unwrap();
    assert_eq!(name.len(), MAX_NAME);
    assert!(name.ends_with(".zip") && valid_name(&name), "{name}");
}

#[tuff_core::watchdog]
#[test]
fn folders_are_read_from_disk_without_links() {
    let dir = TestDir::new();
    let pack = dir.path().join("pack");
    fs::create_dir_all(pack.join("data/a/function")).unwrap();
    fs::write(pack.join("pack.mcmeta"), meta_json("disk")).unwrap();
    fs::write(pack.join("data/a/function/f.mcfunction"), b"say hi").unwrap();
    let Upload::Files(files) = Upload::read_path(&pack).unwrap() else {
        panic!("a folder reads as files");
    };
    let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(paths, ["data/a/function/f.mcfunction", "pack.mcmeta"]);
    let archive = dir.path().join("p.zip");
    fs::write(&archive, b"bytes").unwrap();
    assert!(matches!(
        Upload::read_path(&archive).unwrap(),
        Upload::Zip(bytes) if bytes == b"bytes"
    ));
    assert!(matches!(
        Upload::read_path(&dir.path().join("missing")),
        Err(StoreError::NotFound(_))
    ));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc", pack.join("data/link")).unwrap();
        let err = Upload::read_path(&pack).unwrap_err();
        assert!(err.to_string().contains("symbolic link"), "{err}");
    }
}

#[tuff_core::watchdog]
#[test]
fn enabling_updates_the_saved_selection() {
    for (_dir, backend) in [vanilla_world(), sqlite_world()] {
        let backend = &*backend;
        add(backend, "on", &zip_pack("x"), false).unwrap();
        add(backend, "off", &folder_pack("x"), false).unwrap();
        add(backend, "fresh", &folder_pack("x"), false).unwrap();
        assert!(!enable(backend, "file/on.zip").unwrap());
        backend
            .documents()
            .write(
                Document::Level,
                level(&["vanilla", "file/on.zip"], &["file/off", "file/gone"]),
            )
            .unwrap();
        let selection = |name: &str| {
            list(backend)
                .unwrap()
                .into_iter()
                .find(|e| e.name == name)
                .unwrap()
                .selection
        };
        assert_eq!(selection("on.zip"), Selection::Enabled);
        assert_eq!(selection("off"), Selection::Disabled);
        assert_eq!(selection("fresh"), Selection::New);
        assert!(enable(backend, "file/off").unwrap());
        assert!(enable(backend, "file/on.zip").unwrap());
        assert_eq!(
            saved_selection(backend).unwrap().unwrap(),
            DataPackConfig {
                enabled: vec!["vanilla".into(), "file/on.zip".into(), "file/off".into()],
                disabled: vec!["file/gone".into()],
            }
        );
        assert_eq!(selection("off"), Selection::Enabled);
    }
}

fn level_dat(dir: &Path) {
    VanillaBackend::new(dir)
        .documents()
        .write(Document::Level, level(&["vanilla"], &[]))
        .unwrap();
}

fn locked(result: Result<OfflineWorld, StoreError>) -> String {
    match result {
        Err(StoreError::Locked(message)) => message,
        Err(other) => panic!("expected a lock error, got {other}"),
        Ok(_) => panic!("opened a locked world"),
    }
}

#[tuff_core::watchdog]
#[test]
fn a_folder_world_in_use_is_refused() {
    let dir = TestDir::new();
    let world = dir.path().join("world");
    let err = OfflineWorld::open(&world).err().unwrap();
    assert!(matches!(err, StoreError::NotFound(_)), "{err}");
    level_dat(&world);
    let err = OfflineWorld::open(dir.path()).err().unwrap();
    assert!(err.to_string().contains("did you mean"), "{err}");

    let server = VanillaBackend::new(&world);
    let held = server.lock().unwrap();
    let message = locked(OfflineWorld::open(&world));
    assert!(message.contains("in use"), "{message}");
    drop(held);

    let offline = OfflineWorld::open(&world).unwrap();
    add(offline.backend(), "p", &zip_pack("x"), false).unwrap();
    assert!(server.lock().is_err());
    drop(offline);
    assert!(server.lock().is_ok());
    assert_eq!(ids(&server), ["vanilla", "file/p.zip"]);
}

fn server_file(dir: &TestDir) -> PathBuf {
    dir.path().join("world.mcserver")
}

#[tuff_core::watchdog]
#[test]
fn a_server_file_in_use_is_refused() {
    let dir = TestDir::new();
    let file = server_file(&dir);
    let err = OfflineWorld::open(&file).err().unwrap();
    assert!(matches!(err, StoreError::NotFound(_)), "{err}");
    fs::write(&file, b"").unwrap();
    let err = OfflineWorld::open(&file).err().unwrap();
    assert!(matches!(err, StoreError::NotFound(_)), "{err}");
    fs::remove_file(&file).unwrap();

    let server = SqliteBackend::open(&file).unwrap();
    let held = server.lock().unwrap();
    locked(OfflineWorld::open(&file));
    drop(held);
    drop(server);

    let server = SqliteBackend::open_with(&file, Locking::Normal).unwrap();
    let held = server.lock().unwrap();
    locked(OfflineWorld::open(&file));
    drop(held);
    drop(server);

    let offline = OfflineWorld::open(&file).unwrap();
    add(offline.backend(), "p", &folder_pack("x"), false).unwrap();
    assert!(SqliteBackend::open(&file).is_err());
    drop(offline);
    let server = SqliteBackend::open(&file).unwrap();
    assert_eq!(ids(&server), ["vanilla", "file/p"]);
}
