use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

pub const LOCK_ENV: &str = "TUFF_CLIENT_LOCK";
pub const CLIENT_SLOTS: usize = 4;
pub const LOCK_WAIT: Duration = Duration::from_secs(15 * 60);
pub const LOCK_POLL: Duration = Duration::from_secs(1);
pub const PREVIOUS_CLIENT_WAIT: Duration = Duration::from_secs(30);

/// Informational record written into the lock file after locking.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LockOwner {
    pub pid: u32,
    pub worktree: String,
    pub test: String,
    pub started_unix: u64,
    pub client_pid: Option<u32>,
}

impl LockOwner {
    pub fn current(test: &str) -> Self {
        Self {
            pid: std::process::id(),
            worktree: std::env::current_dir()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
            test: test.to_owned(),
            started_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            client_pid: None,
        }
    }

    pub fn render(&self) -> String {
        let mut out = format!(
            "pid={}\nworktree={}\ntest={}\nstarted={}\n",
            self.pid,
            self.worktree.replace('\n', " "),
            self.test.replace('\n', " "),
            self.started_unix
        );
        if let Some(pid) = self.client_pid {
            out.push_str(&format!("client_pid={pid}\n"));
        }
        out
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut owner = Self::default();
        let mut seen_pid = false;
        for line in text.lines() {
            let (key, value) = line.split_once('=')?;
            match key {
                "pid" => {
                    owner.pid = value.parse().ok()?;
                    seen_pid = true;
                }
                "worktree" => owner.worktree = value.to_owned(),
                "test" => owner.test = value.to_owned(),
                "started" => owner.started_unix = value.parse().ok()?,
                "client_pid" => owner.client_pid = Some(value.parse().ok()?),
                _ => {}
            }
        }
        seen_pid.then_some(owner)
    }

    pub fn describe(&self) -> String {
        format!(
            "pid {} test {:?} worktree {} client pid {:?}",
            self.pid, self.test, self.worktree, self.client_pid
        )
    }
}

pub fn default_lock_path() -> PathBuf {
    if let Some(path) = std::env::var_os(LOCK_ENV) {
        return PathBuf::from(path);
    }
    if cfg!(windows) {
        std::env::temp_dir().join("tuff-vanilla-client.lock")
    } else {
        PathBuf::from("/tmp/tuff-vanilla-client.lock")
    }
}

/// Machine-wide client slot guard (one of `CLIENT_SLOTS` files): an OS file lock the kernel releases when
/// the holder dies.
#[derive(Debug)]
pub struct ClientLock {
    file: File,
    owner: LockOwner,
    previous: Option<LockOwner>,
}

pub fn read_owner(path: &Path) -> Option<LockOwner> {
    LockOwner::parse(&std::fs::read_to_string(path).ok()?)
}

impl ClientLock {
    pub fn try_acquire(path: &Path, owner: LockOwner) -> Result<Option<Self>> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("open lock file {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(e)) => return Err(e).context("lock client lock file"),
        }
        let mut text = String::new();
        file.read_to_string(&mut text).ok();
        let previous = LockOwner::parse(&text);
        let mut lock = Self {
            file,
            owner,
            previous,
        };
        lock.write_owner()?;
        Ok(Some(lock))
    }

    /// Polls until the lock is free, calling `on_wait` with the current
    /// holder once per poll; fails after `timeout` naming the holder.
    pub fn acquire(
        path: &Path,
        owner: &LockOwner,
        timeout: Duration,
        poll: Duration,
        mut on_wait: impl FnMut(Option<&LockOwner>),
    ) -> Result<Self> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(lock) = Self::try_acquire(path, owner.clone())? {
                return Ok(lock);
            }
            let holder = read_owner(path);
            on_wait(holder.as_ref());
            if Instant::now() >= deadline {
                bail!(
                    "timed out after {timeout:?} waiting for the client lock {}; held by {}",
                    path.display(),
                    holder.map_or_else(|| "unknown".to_owned(), |h| h.describe())
                );
            }
            std::thread::sleep(poll);
        }
    }

    /// Tries each of `slots` lock files `<base>.<i>` once.
    pub fn try_acquire_slot(base: &Path, slots: usize, owner: &LockOwner) -> Result<Option<Self>> {
        for i in 0..slots {
            if let Some(lock) = Self::try_acquire(&slot_path(base, i), owner.clone())? {
                return Ok(Some(lock));
            }
        }
        Ok(None)
    }

    /// Polls until any of `slots` lock files frees; `on_wait` gets the holders.
    pub fn acquire_slot(
        base: &Path,
        slots: usize,
        owner: &LockOwner,
        timeout: Duration,
        poll: Duration,
        mut on_wait: impl FnMut(&[Option<LockOwner>]),
    ) -> Result<Self> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(lock) = Self::try_acquire_slot(base, slots, owner)? {
                return Ok(lock);
            }
            let holders: Vec<_> = (0..slots)
                .map(|i| read_owner(&slot_path(base, i)))
                .collect();
            on_wait(&holders);
            if Instant::now() >= deadline {
                let held: Vec<_> = holders
                    .iter()
                    .map(|h| {
                        h.as_ref()
                            .map_or_else(|| "unknown".to_owned(), |h| h.describe())
                    })
                    .collect();
                bail!(
                    "timed out after {timeout:?} waiting for a client slot {}; held by {}",
                    base.display(),
                    held.join("; ")
                );
            }
            std::thread::sleep(poll);
        }
    }

    pub fn previous_owner(&self) -> Option<&LockOwner> {
        self.previous.as_ref()
    }

    pub fn owner(&self) -> &LockOwner {
        &self.owner
    }

    pub fn set_client_pid(&mut self, pid: Option<u32>) -> Result<()> {
        self.owner.client_pid = pid;
        self.write_owner()
    }

    fn write_owner(&mut self) -> Result<()> {
        self.file.set_len(0)?;
        self.file.rewind()?;
        self.file.write_all(self.owner.render().as_bytes())?;
        self.file.flush()?;
        Ok(())
    }

    /// After acquiring, a previous owner's client may still be alive (parent
    /// died, stdin watchdog not yet fired): wait for it, then fail.
    pub fn wait_previous_client(
        &self,
        timeout: Duration,
        is_alive: impl Fn(u32) -> bool,
    ) -> Result<()> {
        let Some(pid) = self.previous.as_ref().and_then(|p| p.client_pid) else {
            return Ok(());
        };
        wait_pid_gone(pid, timeout, is_alive)
    }
}

pub fn slot_path(base: &Path, i: usize) -> PathBuf {
    let mut name = base.as_os_str().to_owned();
    name.push(format!(".{i}"));
    PathBuf::from(name)
}

pub fn wait_pid_gone(pid: u32, timeout: Duration, is_alive: impl Fn(u32) -> bool) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while is_alive(pid) {
        if Instant::now() >= deadline {
            bail!("previous vanilla client (pid {pid}) is still running after {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(100).min(timeout));
    }
    Ok(())
}

#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub fn pid_alive(_pid: u32) -> bool {
    false
}
