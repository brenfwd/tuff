//! The backup jobs actor: one backup at a time on a running world, with
//! progress, cancel, retention after each success and an optional
//! schedule. Commands and the admin panel go through [`Backups`]; progress
//! and results are also published on the panel's `jobs` topic.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{mpsc, oneshot, watch};

use super::*;
use crate::config::ConfigStore;
use crate::world::Shared;
use crate::world::save_gate::{DEFAULT_HOLD_LIMIT, hold_saves};
use std::fs;
use tuff_storage::backup::*;

const QUEUE: usize = 16;
/// The panel topic progress and results are published on.
pub const TOPIC: &str = tuff_web_ui::backups::TOPIC;
const HOLDER: &str = "a backup";
const SCHEDULE_CHECK: Duration = Duration::from_secs(30);
const PUBLISH_EVERY: Duration = Duration::from_millis(500);

pub struct BackupsConfig {
    /// The instance directory: backups go to its `backups/` unless
    /// configured, and a relative configured folder is taken from it.
    /// `None` needs the folder configured.
    pub instance_dir: Option<PathBuf>,
    pub level: String,
    pub settings: ConfigStore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackupError {
    /// A backup is running already.
    Busy(String),
    NotRunning,
    NotFound(String),
    NoDirectory,
    /// The world is not running.
    Stopped,
    Io(String),
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(name) => write!(f, "backup {name} is still running"),
            Self::NotRunning => f.write_str("no backup is running"),
            Self::NotFound(name) => write!(f, "there is no backup called {name}"),
            Self::NoDirectory => write!(f, "no backup folder is set ({DIRECTORY_KEY})"),
            Self::Stopped => f.write_str("the world is not running"),
            Self::Io(err) => f.write_str(err),
        }
    }
}

impl std::error::Error for BackupError {}

fn io_error(err: io::Error) -> BackupError {
    BackupError::Io(err.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Running {
    pub id: u64,
    pub name: String,
    pub by: String,
    pub started_unix: u64,
    pub progress: Progress,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finished {
    pub id: u64,
    pub name: String,
    pub by: String,
    /// Why it failed; `None` on success.
    pub error: Option<String>,
    pub size: u64,
    /// How long saving was held.
    pub hold_ms: u64,
    pub duration_ms: u64,
    /// Older backups retention deleted afterwards.
    pub pruned: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Status {
    pub running: Option<Running>,
    pub last: Option<Finished>,
}

/// Runs on the blocking pool while saving is held, after the snapshot.
pub type WhileHeld = Box<dyn FnOnce() + Send>;

pub struct CreateOptions {
    /// Who asked, for status lines and logs.
    pub by: String,
    pub while_held: Option<WhileHeld>,
    /// Keep the rebuildable caches (such as the world map's tiles).
    pub include_caches: bool,
}

impl CreateOptions {
    pub fn by(by: impl Into<String>) -> Self {
        Self {
            by: by.into(),
            while_held: None,
            include_caches: false,
        }
    }

    pub fn with_caches(mut self, include: bool) -> Self {
        self.include_caches = include;
        self
    }
}

enum Request {
    #[cfg(test)]
    Panic,
    Attach(Weak<Shared>),
    Create {
        options: CreateOptions,
        reply: oneshot::Sender<Result<Running, BackupError>>,
    },
    Cancel {
        reply: oneshot::Sender<Result<String, BackupError>>,
    },
    Status {
        reply: oneshot::Sender<Status>,
    },
    List {
        reply: oneshot::Sender<Result<Vec<BackupInfo>, BackupError>>,
    },
    Delete {
        name: String,
        reply: oneshot::Sender<Result<(), BackupError>>,
    },
    Path {
        name: String,
        reply: oneshot::Sender<Result<PathBuf, BackupError>>,
    },
    Directory {
        reply: oneshot::Sender<Result<PathBuf, BackupError>>,
    },
    CheckSchedule {
        reply: oneshot::Sender<Option<Running>>,
    },
    Done(Finished),
}

/// The handle to a world's backup jobs.
#[derive(Clone)]
pub struct Backups {
    tx: mpsc::Sender<Request>,
    finished: watch::Receiver<Option<Finished>>,
    level: Arc<str>,
}

impl std::fmt::Debug for Backups {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backups").finish()
    }
}

impl Backups {
    pub fn spawn(config: BackupsConfig) -> Backups {
        let (tx, rx) = mpsc::channel(QUEUE);
        let (finished_tx, finished) = watch::channel(None);
        let level: Arc<str> = file_level(&config.level).into();
        let actor = Actor {
            level: level.to_string(),
            config,
            shared: Weak::new(),
            running: None,
            last: None,
            last_started: None,
            next_id: 1,
            tx: tx.downgrade(),
            finished: finished_tx,
        };
        tuff_supervise::spawn(
            "the backup runner",
            tuff_supervise::restarting("the backup runner", (actor, rx), |(actor, rx)| {
                Box::pin(actor.run(rx))
            }),
        );
        Backups {
            tx,
            finished,
            level,
        }
    }

    /// A handle whose every request fails, for worlds built without one.
    pub fn detached() -> Backups {
        Backups {
            tx: mpsc::channel(1).0,
            finished: watch::channel(None).1,
            level: "world".into(),
        }
    }

    /// The level name backups are named after, made safe for file names.
    pub fn level(&self) -> &str {
        &self.level
    }

    /// The folder backups are kept in.
    pub async fn directory(&self) -> Result<PathBuf, BackupError> {
        self.ask(|reply| Request::Directory { reply }).await?
    }

    /// Runs the schedule check now (the actor runs it every 30 s),
    /// returning the backup it started, if one was due.
    pub async fn check_schedule(&self) -> Option<Running> {
        self.ask(|reply| Request::CheckSchedule { reply })
            .await
            .ok()
            .flatten()
    }

    /// Gives the actor the world it backs up; scheduling starts here.
    pub fn attach(&self, shared: &Arc<Shared>) {
        let _ = self.tx.try_send(Request::Attach(Arc::downgrade(shared)));
    }

    async fn ask<T>(
        &self,
        request: impl FnOnce(oneshot::Sender<T>) -> Request,
    ) -> Result<T, BackupError> {
        let (reply, answer) = oneshot::channel();
        self.tx
            .send(request(reply))
            .await
            .map_err(|_| BackupError::Stopped)?;
        answer.await.map_err(|_| BackupError::Stopped)
    }

    /// Starts a backup, returning once it is under way.
    pub async fn create(&self, options: CreateOptions) -> Result<Running, BackupError> {
        self.ask(|reply| Request::Create { options, reply }).await?
    }

    /// Cancels the running backup, returning its name.
    pub async fn cancel(&self) -> Result<String, BackupError> {
        self.ask(|reply| Request::Cancel { reply }).await?
    }

    #[cfg(test)]
    pub(super) async fn panic_for_test(&self) {
        let _ = self.tx.send(Request::Panic).await;
    }

    pub async fn status(&self) -> Status {
        self.ask(|reply| Request::Status { reply })
            .await
            .unwrap_or_default()
    }

    /// Finished backups, newest first.
    pub async fn list(&self) -> Result<Vec<BackupInfo>, BackupError> {
        self.ask(|reply| Request::List { reply }).await?
    }

    pub async fn delete(&self, name: &str) -> Result<(), BackupError> {
        let name = name.to_owned();
        self.ask(|reply| Request::Delete { name, reply }).await?
    }

    /// The file of the finished backup `name` (a name from [`Self::list`]).
    pub async fn path(&self, name: &str) -> Result<PathBuf, BackupError> {
        let name = name.to_owned();
        self.ask(|reply| Request::Path { name, reply }).await?
    }

    /// The most recent finished job, updated as each one ends.
    pub fn finished(&self) -> watch::Receiver<Option<Finished>> {
        self.finished.clone()
    }

    /// Waits for the job `id` to finish.
    pub async fn wait(&self, id: u64) -> Result<Finished, BackupError> {
        let mut finished = self.finished.clone();
        let done = finished
            .wait_for(|f| f.as_ref().is_some_and(|f| f.id >= id))
            .await
            .map_err(|_| BackupError::Stopped)?;
        Ok(done.clone().expect("checked above"))
    }
}

struct Job {
    running: Running,
    tracker: Tracker,
}

struct Actor {
    config: BackupsConfig,
    level: String,
    shared: Weak<Shared>,
    running: Option<Job>,
    last: Option<Finished>,
    last_started: Option<u64>,
    next_id: u64,
    tx: mpsc::WeakSender<Request>,
    finished: watch::Sender<Option<Finished>>,
}

impl Actor {
    async fn run(&mut self, rx: &mut mpsc::Receiver<Request>) {
        let start = tokio::time::Instant::now() + SCHEDULE_CHECK;
        let mut schedule = tokio::time::interval_at(start, SCHEDULE_CHECK);
        schedule.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                request = rx.recv() => match request {
                    Some(request) => self.handle(request).await,
                    None => break,
                },
                _ = schedule.tick() => {
                    self.scheduled().await;
                }
            }
        }
    }

    fn directory(&self) -> Result<PathBuf, BackupError> {
        let configured = self
            .config
            .settings
            .get_string(DIRECTORY_KEY)
            .unwrap_or_default();
        let instance = self.config.instance_dir.as_deref();
        match (configured.trim(), instance) {
            ("", Some(instance)) => Ok(instance.join("backups")),
            ("", None) => Err(BackupError::NoDirectory),
            (dir, Some(instance)) => Ok(instance.join(dir)),
            (dir, None) => Ok(PathBuf::from(dir)),
        }
    }

    fn int(&self, key: &str) -> u64 {
        self.config.settings.get_int(key).unwrap_or(0).max(0) as u64
    }

    async fn handle(&mut self, request: Request) {
        match request {
            #[cfg(test)]
            Request::Panic => panic!("test panic"),
            Request::Attach(shared) => {
                self.shared = shared;
                if let Ok(dir) = self.directory() {
                    let level = self.level.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        clean_leftovers(&dir, &level)?;
                        restore::clean_uploads(&dir)
                    })
                    .await;
                }
            }
            Request::Create { options, reply } => {
                let _ = reply.send(self.start(options).await);
            }
            Request::Cancel { reply } => {
                let _ = reply.send(match &self.running {
                    Some(job) => {
                        job.tracker.cancel();
                        Ok(job.running.name.clone())
                    }
                    None => Err(BackupError::NotRunning),
                });
            }
            Request::Status { reply } => {
                let _ = reply.send(self.status());
            }
            Request::List { reply } => {
                let _ = reply.send(self.list().await);
            }
            Request::Delete { name, reply } => {
                let result = match self.directory() {
                    Ok(dir) => match resolve(&dir, &self.level, &name) {
                        Some(path) => tokio::task::spawn_blocking(move || fs::remove_file(path))
                            .await
                            .map_err(|err| BackupError::Io(err.to_string()))
                            .and_then(|r| r.map_err(io_error)),
                        None => Err(BackupError::NotFound(name)),
                    },
                    Err(err) => Err(err),
                };
                let _ = reply.send(result);
            }
            Request::Path { name, reply } => {
                let _ = reply.send(self.directory().and_then(|dir| {
                    resolve(&dir, &self.level, &name).ok_or(BackupError::NotFound(name))
                }));
            }
            Request::Directory { reply } => {
                let _ = reply.send(self.directory());
            }
            Request::CheckSchedule { reply } => {
                let _ = reply.send(self.scheduled().await);
            }
            Request::Done(finished) => {
                if self
                    .running
                    .as_ref()
                    .is_some_and(|job| job.running.id == finished.id)
                {
                    self.running = None;
                }
                if let Some(shared) = self.shared.upgrade() {
                    let data = serde_json::json!({ "kind": "backup", "finished": finished });
                    shared
                        .web
                        .events
                        .publish(TOPIC, "finished", data.to_string());
                }
                self.last = Some(finished.clone());
                self.finished.send_replace(Some(finished));
            }
        }
    }

    fn status(&self) -> Status {
        Status {
            running: self.running.as_ref().map(|job| Running {
                progress: job.tracker.progress(),
                ..job.running.clone()
            }),
            last: self.last.clone(),
        }
    }

    async fn list(&self) -> Result<Vec<BackupInfo>, BackupError> {
        let dir = self.directory()?;
        let level = self.level.clone();
        tokio::task::spawn_blocking(move || tuff_storage::backup::list(&dir, &level))
            .await
            .map_err(|err| BackupError::Io(err.to_string()))?
            .map_err(io_error)
    }

    async fn scheduled(&mut self) -> Option<Running> {
        let minutes = self.int(INTERVAL_KEY);
        if minutes == 0 || self.running.is_some() || self.shared.strong_count() == 0 {
            return None;
        }
        if self.last_started.is_none() {
            self.last_started = Some(
                self.list()
                    .await
                    .ok()
                    .and_then(|list| list.first().map(|b| b.created_unix))
                    .unwrap_or(0),
            );
        }
        if !due(self.last_started, minutes, now_unix()) {
            return None;
        }
        match self.start(CreateOptions::by("schedule")).await {
            Ok(running) => Some(running),
            Err(err) => {
                log::warn!("scheduled backup did not start: {err}");
                self.last_started = Some(now_unix());
                None
            }
        }
    }

    async fn start(&mut self, options: CreateOptions) -> Result<Running, BackupError> {
        if let Some(job) = &self.running {
            return Err(BackupError::Busy(job.running.name.clone()));
        }
        let shared = self.shared.upgrade().ok_or(BackupError::Stopped)?;
        let source = shared.backend.backup_source();
        if matches!(source, BackupSource::Unsupported) {
            return Err(BackupError::Io(
                "this world's storage cannot be backed up".to_owned(),
            ));
        }
        let plan = Plan {
            source,
            dir: self.directory()?,
            level: self.level.clone(),
            compression: self.int(COMPRESSION_KEY).min(9) as u32,
            staged: self.config.settings.get_bool(STAGED_KEY).unwrap_or(true),
            include_caches: options.include_caches,
        };
        let target = {
            let plan = plan.clone();
            tokio::task::spawn_blocking(move || {
                clean_leftovers(&plan.dir, &plan.level)?;
                plan.target()
            })
            .await
            .map_err(|err| BackupError::Io(err.to_string()))?
            .map_err(io_error)?
        };
        let (tracker, progress) = Tracker::new();
        let started_unix = now_unix();
        let running = Running {
            id: self.next_id,
            name: target.name.clone(),
            by: options.by.clone(),
            started_unix,
            progress: Progress::default(),
        };
        self.next_id += 1;
        self.last_started = Some(started_unix);
        self.running = Some(Job {
            running: running.clone(),
            tracker: tracker.clone(),
        });
        log::info!("backup {} started by {}", target.name, options.by);
        tokio::spawn(publish_progress(shared.clone(), running.clone(), progress));
        let retention = Retention {
            keep: self.int(KEEP_KEY) as usize,
            max_age: Some(self.int(MAX_AGE_KEY))
                .filter(|days| *days > 0)
                .map(|days| Duration::from_secs(days * 86_400)),
        };
        let done = self.tx.clone();
        let job = running.clone();
        tokio::spawn(async move {
            let began = Instant::now();
            let result = run(&shared, &plan, &target, &tracker, options.while_held).await;
            drop(shared);
            let mut finished = Finished {
                id: job.id,
                name: job.name.clone(),
                by: job.by.clone(),
                error: None,
                size: 0,
                hold_ms: 0,
                duration_ms: 0,
                pruned: Vec::new(),
            };
            match result {
                Ok((size, hold)) => {
                    finished.size = size;
                    finished.hold_ms = hold.as_millis() as u64;
                    let (dir, level, name) = (plan.dir.clone(), plan.level.clone(), job.name);
                    finished.pruned = tokio::task::spawn_blocking(move || {
                        prune(&dir, &level, retention, now_unix(), &name)
                    })
                    .await
                    .ok()
                    .and_then(|r| {
                        r.map_err(|err| log::warn!("backup retention failed: {err}"))
                            .ok()
                    })
                    .unwrap_or_default();
                    log::info!(
                        "backup {} done: {} bytes, saving held for {} ms",
                        finished.name,
                        size,
                        finished.hold_ms
                    );
                }
                Err((err, hold)) => {
                    finished.hold_ms = hold.as_millis() as u64;
                    let target = target.clone();
                    let _ = tokio::task::spawn_blocking(move || discard(&target)).await;
                    log::warn!("backup {} failed: {err}", finished.name);
                    finished.error = Some(err);
                }
            }
            finished.duration_ms = began.elapsed().as_millis() as u64;
            if let Some(done) = done.upgrade() {
                let _ = done.send(Request::Done(finished)).await;
            }
        });
        Ok(running)
    }
}

/// Whether a scheduled backup every `minutes` (0: never) is due at `now`
/// when the last one started at `last_started` (unix seconds).
pub fn due(last_started: Option<u64>, minutes: u64, now: u64) -> bool {
    minutes > 0 && now >= last_started.unwrap_or(0).saturating_add(minutes * 60)
}

async fn publish_progress(
    shared: Arc<Shared>,
    running: Running,
    mut progress: watch::Receiver<Progress>,
) {
    let events = shared.web.events.clone();
    drop(shared);
    loop {
        let snapshot = progress.borrow_and_update().clone();
        let data = serde_json::json!({
            "kind": "backup",
            "running": Running { progress: snapshot, ..running.clone() },
        });
        events.publish(TOPIC, "progress", data.to_string());
        tokio::time::sleep(PUBLISH_EVERY).await;
        if progress.changed().await.is_err() {
            break;
        }
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> Result<T, String> {
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result.map_err(|err| err.to_string()),
        Err(err) => Err(err.to_string()),
    }
}

/// One backup on a running world: hold saving for the snapshot only.
async fn run(
    shared: &Arc<Shared>,
    plan: &Plan,
    target: &Target,
    tracker: &Tracker,
    while_held: Option<WhileHeld>,
) -> Result<(u64, Duration), (String, Duration)> {
    let hold = hold_saves(shared, HOLDER, DEFAULT_HOLD_LIMIT)
        .await
        .map_err(|err| (err.to_string(), Duration::ZERO))?;
    let held = Instant::now();
    let snapshot = {
        let (plan, target, tracker) = (plan.clone(), target.clone(), tracker.clone());
        blocking(move || {
            let snapshot = tuff_storage::backup::snapshot(&plan, &target, &tracker)?;
            if let Some(hook) = while_held {
                hook();
            }
            Ok(snapshot)
        })
        .await
    };
    let consistent = hold.holding();
    hold.release().await;
    let hold_time = held.elapsed();
    let fail = |err: String| (err, hold_time);
    let snapshot = snapshot.map_err(fail)?;
    if !consistent {
        let _ = tokio::task::spawn_blocking(move || drop(snapshot)).await;
        return Err(fail(format!(
            "saving resumed before the copy finished (the hold lasts {} minutes)",
            DEFAULT_HOLD_LIMIT.as_secs() / 60
        )));
    }
    let (plan, target, tracker) = (plan.clone(), target.clone(), tracker.clone());
    let size = blocking(move || tuff_storage::backup::finish(&plan, &target, snapshot, &tracker))
        .await
        .map_err(fail)?;
    Ok((size, hold_time))
}

/// The backup directory a server with `instance_dir` uses under `settings`.
pub fn directory_for(settings: &ConfigStore, instance_dir: &Path) -> PathBuf {
    let configured = settings.get_string(DIRECTORY_KEY).unwrap_or_default();
    match configured.trim() {
        "" => instance_dir.join("backups"),
        dir => instance_dir.join(dir),
    }
}
