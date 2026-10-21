//! Helpers that let several builds (e.g. git worktrees with different dumper
//! sources) share one cache directory: every write goes to a unique temporary
//! path and is renamed into place, and one-time setup is serialized by a lock.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const TEMP_PREFIX: &str = ".tmp-";

fn unique_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "{TEMP_PREFIX}{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// A directory private to this build, removed with its contents on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new_in(parent: &Path) -> io::Result<Self> {
        fs::create_dir_all(parent)?;
        loop {
            let path = parent.join(unique_name());
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Moves `src` to `dest` in one step, so readers see either nothing or the
/// whole file. Concurrent writers of identical content may both succeed.
pub fn publish(src: &Path, dest: &Path) -> io::Result<()> {
    fs::rename(src, dest)
}

/// Writes `data` to a unique sibling of `dest` and renames it into place.
pub fn write_atomic(dest: &Path, data: &[u8]) -> io::Result<()> {
    let parent = dest.parent().filter(|p| !p.as_os_str().is_empty());
    let parent = parent.unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let (temp, mut file) = loop {
        let path = parent.join(unique_name());
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => break (path, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = file
        .write_all(data)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            publish(&temp, dest)
        });
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Collects every `.jar` file under `dir`, recursively; leaves `jars`
/// untouched if `dir` does not exist.
pub fn collect_jars(dir: &Path, jars: &mut Vec<PathBuf>) -> io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_jars(&path, jars)?;
        } else if path.extension().is_some_and(|ext| ext == "jar") {
            jars.push(path);
        }
    }
    Ok(())
}

/// Whether `work/versions` and `work/libraries` (what the vanilla server
/// jar's bundler extracts into its working directory on a run) hold at
/// least one jar between them, so a dumper has a classpath to run against.
/// `work/` is not needed once every dumper has already produced its cached
/// output, but it is needed again the moment a new dumper's output is
/// missing, and nothing outside this build guarantees `work/` survives
/// between runs that share one cache directory.
pub fn work_has_jars(work: &Path) -> bool {
    let mut jars = Vec::new();
    for sub in ["versions", "libraries"] {
        if collect_jars(&work.join(sub), &mut jars).is_err() {
            return false;
        }
    }
    !jars.is_empty()
}

/// An exclusive lock on `<dir>/.lock`, held until dropped. It is advisory and
/// released by the OS if the build dies.
pub struct Lock {
    _file: File,
}

impl Lock {
    pub fn acquire(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(".lock"))?;
        file.lock()?;
        Ok(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread;

    fn scratch() -> TempDir {
        TempDir::new_in(&std::env::temp_dir().join("vanilla-data-cache-tests")).unwrap()
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn temp_dirs_are_unique_and_removed_on_drop() {
        let root = scratch();
        let a = TempDir::new_in(root.path()).unwrap();
        let b = TempDir::new_in(root.path()).unwrap();
        assert_ne!(a.path(), b.path());
        fs::write(a.path().join("Dumper.java"), "class A {}").unwrap();
        let kept = a.path().to_owned();
        drop(a);
        assert!(!kept.exists());
        assert!(b.path().is_dir());
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp_files() {
        let root = scratch();
        let dest = root.path().join("nested").join("out.txt");
        write_atomic(&dest, b"first").unwrap();
        write_atomic(&dest, b"second").unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"second");
        assert_eq!(entries(dest.parent().unwrap()), ["out.txt"]);
    }

    #[test]
    fn concurrent_writers_of_one_file_all_succeed() {
        let root = scratch();
        let dest = root.path().join("light-abc.txt");
        let data = vec![7u8; 1 << 20];
        let barrier = Barrier::new(8);
        thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    write_atomic(&dest, &data).unwrap();
                });
            }
        });
        assert_eq!(fs::read(&dest).unwrap(), data);
        assert_eq!(entries(root.path()), ["light-abc.txt"]);
    }

    #[test]
    fn publish_keeps_other_outputs() {
        let root = scratch();
        let work = TempDir::new_in(root.path()).unwrap();
        for hash in ["aaa", "bbb"] {
            let partial = work.path().join("out.txt");
            fs::write(&partial, hash).unwrap();
            publish(&partial, &root.path().join(format!("light-{hash}.txt"))).unwrap();
        }
        drop(work);
        assert_eq!(entries(root.path()), ["light-aaa.txt", "light-bbb.txt"]);
    }

    #[test]
    fn lock_excludes_a_second_holder_until_dropped() {
        let root = scratch();
        let lock = Lock::acquire(root.path()).unwrap();
        let other = File::options()
            .write(true)
            .open(root.path().join(".lock"))
            .unwrap();
        assert!(other.try_lock().is_err());
        drop(lock);
        other.try_lock().unwrap();
    }

    #[test]
    fn work_has_jars_is_false_when_work_is_missing_or_empty() {
        let root = scratch();
        assert!(!work_has_jars(&root.path().join("work")));

        let work = root.path().join("work");
        fs::create_dir_all(work.join("versions")).unwrap();
        fs::create_dir_all(work.join("libraries")).unwrap();
        assert!(!work_has_jars(&work), "no jars extracted yet");
    }

    #[test]
    fn work_has_jars_finds_a_jar_under_either_subdirectory() {
        let root = scratch();
        let work = root.path().join("work");
        let versions = work.join("versions").join("1.21");
        fs::create_dir_all(&versions).unwrap();
        fs::write(versions.join("server-1.21.jar"), b"jar").unwrap();
        assert!(work_has_jars(&work));

        let work = root.path().join("another-work");
        let libraries = work.join("libraries").join("com").join("mojang");
        fs::create_dir_all(&libraries).unwrap();
        fs::write(libraries.join("datafixerupper.jar"), b"jar").unwrap();
        assert!(work_has_jars(&work));
    }
}
