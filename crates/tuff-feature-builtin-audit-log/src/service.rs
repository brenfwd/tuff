//! The running audit log: the file, its writer and reader, the switches
//! hooks read on the tick path, and the rollback job in progress.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::Duration;

use tokio::sync::{oneshot, watch};
use tuff_core::config::ConfigStore;
use tuff_core::world::Shared;
use tuff_core::world::conservation::ItemViolation;
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::dimension::Dimensions;
use tuff_core::world::services::{ItemViolations, ServiceFactory, ViolationCulprit, WorldService};

use crate::actors::{Control, Handles, Read, Refused};
use crate::filter::{Filter, parse_duration};
use crate::record::{Group, Kind, Record, now_ms};
use crate::store::{KindScope, Order, Row};

pub const DATABASE_KEY: &str = "tuff.features.audit-log.database";
pub const RETENTION_KEY: &str = "tuff.features.audit-log.retention";
pub const DEFAULT_RETENTION: &str = "90d";
/// How often old rows are purged.
pub const PURGE_EVERY: Duration = Duration::from_secs(600);

/// `plugin:name` with a lowercase namespace and path, as an action must be.
pub fn custom_action(action: &str) -> Result<String, String> {
    let ok = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-./".contains(&b))
    };
    match action.split_once(':') {
        Some((space, path)) if ok(space) && ok(path) && action.len() <= 64 => Ok(action.to_owned()),
        _ => Err(format!("{action:?} is not a plugin:name action")),
    }
}

pub fn group_key(group: Group) -> String {
    format!("tuff.features.audit-log.log.{}", group.name())
}

/// How long rows are kept, `None` for ever.
pub fn retention(config: &ConfigStore) -> Option<i64> {
    let text = config.get_string(RETENTION_KEY)?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    parse_duration(text).ok().filter(|ms| *ms > 0)
}

pub fn group_retention_key(group: Group) -> String {
    format!("{RETENTION_KEY}.{}", group.name())
}

fn parse_retention(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    parse_duration(text).ok().filter(|ms| *ms > 0)
}

/// How long `group`'s rows are kept when it has its own setting: `None`
/// when it follows the general one, `Some(None)` to keep them for ever.
pub fn group_retention(config: &ConfigStore, group: Group) -> Option<Option<i64>> {
    let text = config.get_string(&group_retention_key(group))?;
    if text.trim().is_empty() {
        return None;
    }
    Some(parse_retention(&text))
}

/// A rollback or restore running now.
pub struct Job {
    pub restore: bool,
    pub by: String,
    pub regions: usize,
    pub done: AtomicUsize,
    pub cancelled: AtomicBool,
}

const NOT_RUNNING: &str = "the audit log is not running";

pub struct AuditLog {
    database: Option<PathBuf>,
    handles: OnceLock<Handles>,
    path: OnceLock<PathBuf>,
    mask: AtomicU32,
    dropped: AtomicU64,
    job: watch::Sender<Option<Arc<Job>>>,
    last: Mutex<HashMap<String, Undo>>,
    previews: Mutex<HashMap<String, Preview>>,
    inspectors: Mutex<std::collections::HashSet<i32>>,
    names: RwLock<Vec<String>>,
}

/// A player's last finished rollback or restore, for `undo`.
#[derive(Clone)]
pub struct Undo {
    pub restore: bool,
    pub ids: Vec<i64>,
}

/// A rollback or restore shown to its caller alone, awaiting `confirm`.
pub struct Preview {
    pub restore: bool,
    pub ids: Vec<i64>,
    pub strict: bool,
    pub player: tuff_core::world::messages::PlayerHandle,
    /// The blocks as they really are, sent again on `cancel`.
    pub revert: tuff_core::world::messages::Packets,
}

impl AuditLog {
    pub fn set_preview(&self, by: &str, preview: Preview) {
        if let Ok(mut previews) = self.previews.lock() {
            previews.insert(by.to_owned(), preview);
        }
    }

    pub fn take_preview(&self, by: &str) -> Option<Preview> {
        self.previews.lock().ok()?.remove(by)
    }

    /// Flips the inspector for player `id`; true when now on.
    pub fn toggle_inspector(&self, id: i32) -> bool {
        let Ok(mut set) = self.inspectors.lock() else {
            return false;
        };
        !set.remove(&id) && set.insert(id)
    }

    pub fn inspecting(&self, id: i32) -> bool {
        self.inspectors.lock().is_ok_and(|set| set.contains(&id))
    }

    pub fn stop_inspecting(&self, id: i32) {
        if let Ok(mut set) = self.inspectors.lock() {
            set.remove(&id);
        }
    }

    pub fn new(database: Option<PathBuf>) -> AuditLog {
        AuditLog {
            database,
            handles: OnceLock::new(),
            path: OnceLock::new(),
            mask: AtomicU32::new(0),
            dropped: AtomicU64::new(0),
            job: watch::Sender::new(None),
            last: Mutex::new(HashMap::new()),
            previews: Mutex::new(HashMap::new()),
            inspectors: Mutex::new(std::collections::HashSet::new()),
            names: RwLock::new(Vec::new()),
        }
    }

    /// Whether `kind` is logged; hooks ask before building a record.
    pub fn enabled(&self, kind: Kind) -> bool {
        self.mask.load(Ordering::Relaxed) & kind.bit() != 0 && self.handles.get().is_some()
    }

    /// Queues `record` if its kind is logged. Never waits: a full queue
    /// drops it and counts the loss.
    pub fn log(&self, record: Record) {
        if self.mask.load(Ordering::Relaxed) & record.kind.bit() == 0 {
            return;
        }
        let Some(handles) = self.handles.get() else {
            return;
        };
        if handles.record(record) == Err(Refused::Full) {
            let lost = self.dropped.fetch_add(1, Ordering::Relaxed);
            if lost.is_power_of_two() {
                log::warn!(
                    "audit log: the write queue is full; {} records lost",
                    lost + 1
                );
            }
        }
    }

    /// Records lost to a full queue since start.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.get().map(PathBuf::as_path)
    }

    pub fn running(&self) -> bool {
        self.handles.get().is_some()
    }

    pub fn remember(&self, by: &str, undo: Undo) {
        if let Ok(mut last) = self.last.lock() {
            last.insert(by.to_owned(), undo);
        }
    }

    pub fn take_undo(&self, by: &str) -> Option<Undo> {
        self.last.lock().ok()?.remove(by)
    }

    /// Known player names, as of the last refresh.
    pub fn names(&self) -> Vec<String> {
        self.names.read().map(|n| n.clone()).unwrap_or_default()
    }

    pub async fn refresh_names(&self) {
        if let Ok(names) = self.read(|reply| Read::Users { reply }).await
            && let Ok(mut slot) = self.names.write()
        {
            *slot = names;
        }
    }

    /// Re-reads the settings the hooks follow.
    pub fn reload(&self, config: &ConfigStore) {
        self.refresh(config);
    }

    fn refresh(&self, config: &ConfigStore) {
        let mask = Group::ALL
            .into_iter()
            .filter(|group| config.get_bool(&group_key(*group)).unwrap_or(true))
            .fold(0, |mask, group| mask | group.mask());
        self.mask.store(mask, Ordering::Relaxed);
    }

    async fn control<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<T>) -> Control,
    ) -> Result<T, String> {
        let handles = self.handles.get().ok_or(NOT_RUNNING)?;
        let (reply, answer) = oneshot::channel();
        if !handles.control(make(reply)) {
            return Err(NOT_RUNNING.to_owned());
        }
        answer.await.map_err(|_| NOT_RUNNING.to_owned())
    }

    async fn read<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<Result<T, String>>) -> Read,
    ) -> Result<T, String> {
        let handles = self.handles.get().ok_or(NOT_RUNNING)?;
        let (reply, answer) = oneshot::channel();
        if !handles.read(make(reply)) {
            return Err(NOT_RUNNING.to_owned());
        }
        answer.await.map_err(|_| NOT_RUNNING.to_owned())?
    }

    /// Resolves once every record queued so far is in the file.
    pub async fn sync(&self) -> Result<(), String> {
        self.control(Control::Flush).await
    }

    pub async fn lookup(
        &self,
        filter: Filter,
        order: Order,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Row>, String> {
        self.read(|reply| Read::Lookup {
            filter: Box::new(filter),
            order,
            limit,
            offset,
            reply,
        })
        .await
    }

    pub async fn count(&self, filter: Filter) -> Result<u64, String> {
        self.read(|reply| Read::Count {
            filter: Box::new(filter),
            reply,
        })
        .await
    }

    pub async fn total(&self) -> Result<u64, String> {
        self.read(|reply| Read::Total { reply }).await
    }

    /// Deletes rows older than `before` (in `world`), in bounded steps
    /// between record batches; resolves with how many went.
    pub async fn purge(&self, before: i64, world: Option<String>) -> Result<usize, String> {
        self.control(|reply| Control::Purge {
            before,
            world,
            kinds: KindScope::Any,
            reply: Some(reply),
        })
        .await
    }

    /// Logs a plugin's action. `action` is `plugin:name`; the row's
    /// material holds it and `text` any detail. Never waits.
    pub fn log_custom(
        &self,
        who: crate::record::Who,
        action: &str,
        world: tuff_core::world::dimension::DimensionId,
        pos: [i32; 3],
        text: Option<String>,
    ) -> Result<(), String> {
        let action = custom_action(action)?;
        let mut record = Record::new(now_ms(), who, Kind::Custom, world, pos);
        record.material = Some(action);
        record.text = text;
        self.log(record);
        Ok(())
    }

    /// Queues the purges the retention settings call for: each group with
    /// its own setting by it, everything else by the general one.
    pub fn purge_by_retention(&self, config: &ConfigStore) {
        let Some(handles) = self.handles.get() else {
            return;
        };
        let now = now_ms();
        let mut own = Vec::new();
        for group in Group::ALL {
            let Some(keep) = group_retention(config, group) else {
                continue;
            };
            own.extend(group.kinds());
            if let Some(keep) = keep {
                handles.control(Control::Purge {
                    before: now - keep,
                    world: None,
                    kinds: KindScope::Only(group.kinds().collect()),
                    reply: None,
                });
            }
        }
        if let Some(keep) = retention(config) {
            handles.control(Control::Purge {
                before: now - keep,
                world: None,
                kinds: KindScope::Except(own),
                reply: None,
            });
        }
    }

    pub async fn mark(&self, ids: Vec<i64>, rolled_back: bool) -> Result<bool, String> {
        self.control(|reply| Control::Mark {
            ids,
            rolled_back,
            reply,
        })
        .await
    }

    /// Writes what is queued and closes the file; later records are lost.
    pub async fn close(&self) {
        let _ = self.read(Read::Close).await;
        let _ = self.control(Control::Close).await;
    }

    pub fn job(&self) -> Option<Arc<Job>> {
        self.job.borrow().clone()
    }

    /// Takes the job slot, unless a job is running.
    pub fn begin(&self, job: Arc<Job>) -> bool {
        self.job.send_if_modified(|slot| {
            if slot.is_some() {
                return false;
            }
            *slot = Some(job.clone());
            true
        })
    }

    pub fn end(&self) {
        self.job.send_replace(None);
    }

    /// Where the file goes: the feature's override, then the setting, then
    /// beside the world.
    fn location(&self, shared: &Shared) -> Option<PathBuf> {
        if let Some(path) = &self.database {
            return Some(path.clone());
        }
        if let Some(path) = shared
            .config
            .get_string(DATABASE_KEY)
            .filter(|path| !path.trim().is_empty())
        {
            return Some(PathBuf::from(path.trim()));
        }
        let world = shared.world_path.as_ref()?;
        Some(beside(world))
    }
}

/// `./world` -> `./world.audit.sqlite`; `world.sqlite` -> `world.audit.sqlite`.
pub fn beside(world: &Path) -> PathBuf {
    match world.file_stem() {
        Some(stem) => world.with_file_name(format!("{}.audit.sqlite", stem.to_string_lossy())),
        None => world.join("audit.sqlite"),
    }
}

impl ItemViolations for AuditLog {
    fn violation(
        &self,
        culprit: ViolationCulprit<'_>,
        dimension: DimensionId,
        violation: &ItemViolation,
    ) {
        let who = match culprit {
            ViolationCulprit::Player { uuid, name } => crate::record::Who::Player {
                uuid,
                name: name.into(),
            },
            ViolationCulprit::Plugin(plugin) => {
                crate::record::Who::Cause(format!("#plugin:{plugin}"))
            }
        };
        self.log(crate::hooks::violation_record(who, dimension, violation));
    }
}

impl WorldService for AuditLog {
    fn item_violations(&self) -> Option<&dyn ItemViolations> {
        Some(self)
    }

    fn start(&self, shared: &Arc<Shared>) {
        self.refresh(&shared.config);
        let Some(path) = self.location(shared) else {
            log::warn!("audit log: the world has no location on disk; set {DATABASE_KEY}");
            return;
        };
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            && let Err(err) = std::fs::create_dir_all(parent)
        {
            log::error!("audit log: cannot create {}: {err}", parent.display());
            return;
        }
        let mut worlds = vec![String::new(); shared.dimensions.iter().len()];
        for dimension in shared.dimensions.iter() {
            if let Some(slot) = worlds.get_mut(dimension.id.index()) {
                *slot = dimension.key.clone();
            }
        }
        match Handles::spawn(path.clone(), worlds) {
            Ok(handles) => {
                let _ = self.handles.set(handles);
                let _ = self.path.set(path);
            }
            Err(err) => {
                log::error!("audit log: cannot start: {err}");
                return;
            }
        }
        tokio::spawn(follow_config(Arc::downgrade(shared)));
        tokio::spawn(purge_old_rows(Arc::downgrade(shared)));
        tokio::spawn(follow_names(Arc::downgrade(shared)));
    }

    fn flush(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(self.close())
    }
}

async fn follow_config(shared: Weak<Shared>) {
    let mut changes = match shared.upgrade() {
        Some(shared) => shared.config.subscribe(),
        None => return,
    };
    while changes.changed().await.is_ok() {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        if let Some(log) = shared.services.get::<AuditLog>() {
            log.refresh(&shared.config);
        }
    }
}

async fn follow_names(shared: Weak<Shared>) {
    let mut every = tokio::time::interval(Duration::from_secs(60));
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let Some(log) = shared.services.get::<AuditLog>() else {
            return;
        };
        if !log.running() {
            return;
        }
        let _ = log.sync().await;
        log.refresh_names().await;
    }
}

async fn purge_old_rows(shared: Weak<Shared>) {
    let mut every = tokio::time::interval(PURGE_EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let Some(log) = shared.services.get::<AuditLog>() else {
            return;
        };
        log.purge_by_retention(&shared.config);
    }
}

pub struct AuditLogFactory {
    pub database: Option<PathBuf>,
}

impl ServiceFactory for AuditLogFactory {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(AuditLog::new(self.database.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_file_sits_beside_the_world() {
        assert_eq!(
            beside(Path::new("./world")),
            PathBuf::from("./world.audit.sqlite")
        );
        assert_eq!(
            beside(Path::new("/srv/mc/world.sqlite")),
            PathBuf::from("/srv/mc/world.audit.sqlite")
        );
        assert_eq!(beside(Path::new("/")), PathBuf::from("/audit.sqlite"));
    }
}
