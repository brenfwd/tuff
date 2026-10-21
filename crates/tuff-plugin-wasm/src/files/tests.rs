use std::sync::atomic::AtomicUsize;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use super::*;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    files: Files,
    done: Receiver<Done>,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn fixture(quota: u64, entries: u32) -> Fixture {
    let dir = std::env::temp_dir().join(format!(
        "tuff-files-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(dir.join("files")).unwrap();
    let (tx, done) = channel();
    let tx = Mutex::new(tx);
    let files = Files::new(
        dir.join("files"),
        Bounds {
            entries,
            read: 4,
            pending: 8,
        },
        move |d| {
            let _ = tx.lock().unwrap().send(d);
        },
    );
    files.set_access(Access::Quota(quota));
    Fixture { files, done, dir }
}

impl Fixture {
    fn run(&self, op: Op) -> Result<FileResult, Denied> {
        let handle = self.files.submit(op)?;
        let (got, outcome) = self.done.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got, handle);
        outcome
    }

    fn write(&self, path: &str, data: &[u8]) -> Result<FileResult, Denied> {
        self.run(Op::Write {
            path: validate(path, false).unwrap(),
            mode: WriteMode::Replace,
            data: data.to_vec(),
        })
    }
}

fn path(p: &str) -> Vec<String> {
    validate(p, false).unwrap()
}

#[tuff_core::watchdog]
#[test]
fn malformed_paths_are_refused() {
    let long = "a/".repeat(17) + "a";
    for bad in [
        "",
        "/abs",
        "a//b",
        "../x",
        "a/../b",
        "./a",
        ".hidden",
        "a\\b",
        "%2e%2e/x",
        "a b",
        "a/",
        "caf\u{e9}",
        "a\0b",
        &long,
        &"x".repeat(1025),
    ] {
        assert!(
            matches!(validate(bad, false), Err(Denied::Malformed(_))),
            "{bad:?}"
        );
    }
    assert_eq!(validate("", true).unwrap(), Vec::<String>::new());
    assert_eq!(validate("a/b.txt", false).unwrap(), ["a", "b.txt"]);
}

#[tuff_core::watchdog]
#[test]
fn write_read_stat_list_remove_roundtrip() {
    let f = fixture(1024, 16);
    f.run(Op::CreateDir(path("dir"))).unwrap();
    f.write("dir/a.txt", b"hello world").unwrap();
    let FileResult::Data(data) = f
        .run(Op::Read {
            path: path("dir/a.txt"),
            offset: 0,
            len: 100,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(data, b"hell");
    let FileResult::Data(data) = f
        .run(Op::Read {
            path: path("dir/a.txt"),
            offset: 4,
            len: 3,
        })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(data, b"o w");
    let FileResult::Entry(stat) = f.run(Op::Stat(path("dir/a.txt"))).unwrap() else {
        panic!()
    };
    assert_eq!(
        (stat.name.as_str(), stat.size, stat.is_dir),
        ("a.txt", 11, false)
    );
    let FileResult::Entries(list) = f.run(Op::List(Vec::new())).unwrap() else {
        panic!()
    };
    assert_eq!(list.len(), 1);
    assert!(list[0].is_dir);
    assert_eq!(f.files.usage(), (11, 1024));
    f.run(Op::Rename(path("dir/a.txt"), path("b.txt"))).unwrap();
    assert!(matches!(
        f.run(Op::Remove(path("dir/a.txt"))),
        Err(Denied::NotFound)
    ));
    f.run(Op::Remove(path("b.txt"))).unwrap();
    f.run(Op::Remove(path("dir"))).unwrap();
    assert_eq!(f.files.usage().0, 0);
    assert_eq!(f.files.entries(), 0);
}

#[tuff_core::watchdog]
#[test]
fn append_and_write_at() {
    let f = fixture(1024, 16);
    f.write("a", b"abc").unwrap();
    f.run(Op::Write {
        path: path("a"),
        mode: WriteMode::Append,
        data: b"def".to_vec(),
    })
    .unwrap();
    f.run(Op::Write {
        path: path("a"),
        mode: WriteMode::At(1),
        data: b"XY".to_vec(),
    })
    .unwrap();
    assert_eq!(fs::read(f.dir.join("files/a")).unwrap(), b"aXYdef");
    assert!(matches!(
        f.run(Op::Write {
            path: path("none"),
            mode: WriteMode::At(0),
            data: vec![1]
        }),
        Err(Denied::NotFound)
    ));
    assert_eq!(f.files.usage().0, 6);
}

#[tuff_core::watchdog]
#[test]
fn byte_and_entry_quota_are_enforced() {
    let f = fixture(10, 2);
    f.write("a", b"123456").unwrap();
    assert!(matches!(f.write("b", b"12345"), Err(Denied::Quota(_))));
    f.write("a", b"1234567890").unwrap();
    assert!(matches!(
        f.run(Op::Write {
            path: path("a"),
            mode: WriteMode::Append,
            data: vec![1]
        }),
        Err(Denied::Quota(_))
    ));
    f.write("b", b"").unwrap();
    assert!(matches!(f.write("c", b""), Err(Denied::Quota(_))));
    assert!(matches!(
        f.run(Op::CreateDir(path("d"))),
        Err(Denied::Quota(_))
    ));
    f.run(Op::Remove(path("a"))).unwrap();
    f.write("c", b"1234567890").unwrap();
}

#[tuff_core::watchdog]
#[test]
fn symlinks_planted_by_the_owner_are_not_followed() {
    let f = fixture(1024, 16);
    let outside = f.dir.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret"), b"secret").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, f.dir.join("files/link")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), f.dir.join("files/file")).unwrap();
        for op in [
            Op::Read {
                path: path("link/secret"),
                offset: 0,
                len: 10,
            },
            Op::Read {
                path: path("file"),
                offset: 0,
                len: 10,
            },
            Op::Write {
                path: path("link/new"),
                mode: WriteMode::Replace,
                data: vec![1],
            },
            Op::Write {
                path: path("file"),
                mode: WriteMode::Replace,
                data: vec![1],
            },
            Op::List(path("link")),
            Op::Remove(path("file")),
        ] {
            assert!(matches!(f.run(op), Err(Denied::Malformed(_))));
        }
        assert!(!outside.join("new").exists());
        assert_eq!(fs::read(outside.join("secret")).unwrap(), b"secret");
        let FileResult::Entries(list) = f.run(Op::List(Vec::new())).unwrap() else {
            panic!()
        };
        assert!(list.is_empty());
    }
}

#[tuff_core::watchdog]
#[test]
fn usage_is_scanned_from_preseeded_files() {
    let dir = std::env::temp_dir().join(format!("tuff-files-seed-{}", std::process::id()));
    let root = dir.join("files");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub/x"), [0u8; 7]).unwrap();
    let (tx, done) = channel();
    let tx = Mutex::new(tx);
    let files = Files::new(
        root,
        Bounds {
            entries: 8,
            read: 16,
            pending: 4,
        },
        move |d| {
            let _ = tx.lock().unwrap().send(d);
        },
    );
    files.set_access(Access::Quota(8));
    files.submit(Op::Stat(path("sub"))).unwrap();
    let _ = done.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(files.usage(), (7, 8));
    assert_eq!(files.entries(), 2);
    files
        .submit(Op::Write {
            path: path("y"),
            mode: WriteMode::Replace,
            data: vec![0; 2],
        })
        .unwrap();
    assert!(matches!(
        done.recv_timeout(Duration::from_secs(5)).unwrap().1,
        Err(Denied::Quota(_))
    ));
    let _ = fs::remove_dir_all(dir);
}

#[tuff_core::watchdog]
#[test]
fn revoking_access_stops_operations() {
    let f = fixture(1024, 16);
    f.write("a", b"x").unwrap();
    f.files.set_access(Access::NotGranted);
    assert!(matches!(
        f.files.submit(Op::Stat(path("a"))),
        Err(Denied::NotGranted(_))
    ));
    f.files.set_access(Access::NotRequested);
    assert!(matches!(
        f.files.submit(Op::Stat(path("a"))),
        Err(Denied::NotRequested(_))
    ));
    f.files.set_access(Access::Quota(1024));
    let Fixture { files, done, dir } = &f;
    files.submit(Op::Stat(path("a"))).unwrap();
    let _ = done.recv_timeout(Duration::from_secs(5)).unwrap();
    let _ = dir;
}

#[tuff_core::watchdog]
#[test]
fn too_many_operations_in_flight_are_refused() {
    let f = fixture(1 << 20, 4096);
    let mut refused = false;
    let mut accepted = 0;
    for _ in 0..200 {
        match f.files.submit(Op::Write {
            path: path("big"),
            mode: WriteMode::Replace,
            data: vec![0; 1 << 16],
        }) {
            Ok(_) => accepted += 1,
            Err(Denied::Quota(_)) => refused = true,
            Err(other) => panic!("{other:?}"),
        }
    }
    assert!(refused);
    for _ in 0..accepted {
        let _ = f.done.recv_timeout(Duration::from_secs(10)).unwrap();
    }
}

#[tuff_core::watchdog]
#[test]
fn purge_removes_the_store() {
    let f = fixture(1024, 16);
    f.write("a", b"x").unwrap();
    purge(f.files.root()).unwrap();
    purge(f.files.root()).unwrap();
    f.files.rescan();
    assert!(matches!(f.run(Op::Stat(path("a"))), Err(Denied::NotFound)));
    f.write("a", b"y").unwrap();
    assert_eq!(f.files.usage().0, 1);
}
