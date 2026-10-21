//! The actor owning the scoreboard. Commands and the game change it by
//! sending closures that run against it in order; after each one, the
//! packets the change produced go to every subscribed player. Saves run on
//! the blocking pool, chained so they land in order, and never hold up the
//! inbox. A change that panics is logged and dropped (its caller sees the
//! scoreboard as stopped) and whatever it did before panicking stays.

use std::collections::HashSet;
use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::storage::level::{inner, saved};
use tuff_storage::backend::WorldBackend;
use tuff_storage::document::Document;

use super::{Criterion, Nametag, Scoreboard, Teams};

const SAVED_DATA: &str = "minecraft:scoreboard";
const AUTOSAVE_TICKS: u64 = 6000;
/// Bounded: every sender is a command or session that can wait its turn,
/// and the actor itself never waits on anyone while draining.
const INBOX: usize = 1024;

/// A change, and what to do once its effects are published: answering
/// only then means whoever waits on it sees the new criteria and teams.
type Job = Box<dyn FnOnce(&mut Scoreboard) -> Option<Done> + Send>;
type Done = Box<dyn FnOnce() + Send>;

enum Message {
    Run(Job),
    Subscribe(mpsc::UnboundedSender<Bytes>),
    Flush(oneshot::Sender<()>),
}

/// Where the scoreboard is read and changed.
#[derive(Clone, Debug)]
pub struct ScoreboardHandle {
    inbox: mpsc::Sender<Message>,
    /// The criteria objectives count, so the game reports only what
    /// some objective wants.
    criteria: watch::Receiver<Arc<HashSet<Criterion>>>,
    teams: watch::Receiver<Arc<Teams>>,
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Run(_) => "Run",
            Self::Subscribe(_) => "Subscribe",
            Self::Flush(_) => "Flush",
        })
    }
}

impl ScoreboardHandle {
    /// A handle with no scoreboard behind it: reads answer `None`.
    pub fn detached() -> Self {
        Self {
            inbox: mpsc::channel(1).0,
            criteria: watch::channel(Arc::default()).1,
            teams: watch::channel(Arc::default()).1,
        }
    }

    /// The teams as they stand now.
    pub fn teams(&self) -> Arc<Teams> {
        self.teams.borrow().clone()
    }

    /// Whether some objective counts `criterion`.
    pub fn tracks(&self, criterion: &Criterion) -> bool {
        self.criteria.borrow().contains(criterion)
    }

    /// Whether some objective counts a statistic.
    pub fn tracks_stats(&self) -> bool {
        self.criteria
            .borrow()
            .iter()
            .any(|c| matches!(c, Criterion::Stat(_)))
    }

    /// Whether some objective counts a team color criterion.
    pub fn tracks_team_kills(&self) -> bool {
        self.criteria
            .borrow()
            .iter()
            .any(|c| matches!(c, Criterion::TeamKill(_) | Criterion::KilledByTeam(_)))
    }

    /// Runs `change` against the scoreboard and returns its answer; `None`
    /// once the scoreboard has stopped.
    pub async fn update<T: Send + 'static>(
        &self,
        change: impl FnOnce(&mut Scoreboard) -> T + Send + 'static,
    ) -> Option<T> {
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |board| {
            let answer = change(board);
            Some(Box::new(move || {
                let _ = reply.send(answer);
            }))
        });
        self.inbox.send(Message::Run(job)).await.ok()?;
        answer.await.ok()
    }

    /// Reads from the scoreboard.
    pub async fn read<T: Send + 'static>(
        &self,
        read: impl FnOnce(&Scoreboard) -> T + Send + 'static,
    ) -> Option<T> {
        self.update(move |board| read(board)).await
    }

    /// Changes the scoreboard without waiting for it.
    pub async fn apply(&self, change: impl FnOnce(&mut Scoreboard) + Send + 'static) {
        let job: Job = Box::new(move |board| {
            change(board);
            None
        });
        let _ = self.inbox.send(Message::Run(job)).await;
    }

    /// Drops the scores and team membership of destroyed entities.
    pub async fn forget_entities(&self, uuids: Vec<u128>) {
        self.apply(move |board| {
            for uuid in uuids {
                let holder = tuff_storage::admin::format_uuid(uuid);
                board.reset_all(&holder);
                board.leave_team(&holder);
            }
        })
        .await;
    }

    /// Puts an entity on the named team without waiting; a no-op outside a
    /// runtime.
    pub fn join_entity_to_team(&self, uuid: u128, team: String) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let this = self.clone();
        runtime.spawn(async move {
            this.apply(move |board| {
                board.join_team(&tuff_storage::admin::format_uuid(uuid), &team);
            })
            .await;
        });
    }

    /// Sets the role nametags of players by name without waiting; `None`
    /// clears one.
    pub fn set_nametags(&self, tags: Vec<(String, Option<Nametag>)>) {
        if tags.is_empty() {
            return;
        }
        let change = move |board: &mut Scoreboard| {
            for (holder, tag) in tags {
                board.set_nametag(&holder, tag);
            }
        };
        let job: Job = Box::new(move |board| {
            change(board);
            None
        });
        if let Err(mpsc::error::TrySendError::Full(message)) =
            self.inbox.try_send(Message::Run(job))
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let inbox = self.inbox.clone();
            runtime.spawn(async move {
                let _ = inbox.send(message).await;
            });
        }
    }

    /// Sends a joining player the whole scoreboard, then every change.
    pub async fn subscribe(&self, outbound: mpsc::UnboundedSender<Bytes>) {
        let _ = self.inbox.send(Message::Subscribe(outbound)).await;
    }

    /// Resolves once every change so far is saved.
    pub async fn flush(&self) {
        let (done, saved) = oneshot::channel();
        if self.inbox.send(Message::Flush(done)).await.is_ok() {
            let _ = saved.await;
        }
    }
}

struct Actor {
    board: Scoreboard,
    backend: Arc<dyn WorldBackend>,
    /// False when the saved board could not be read, so it is never
    /// overwritten.
    persist: bool,
    subscribers: Vec<mpsc::UnboundedSender<Bytes>>,
    last_save: Option<JoinHandle<()>>,
    criteria: watch::Sender<Arc<HashSet<Criterion>>>,
    teams: watch::Sender<Arc<Teams>>,
}

impl Actor {
    fn publish_teams(&mut self) {
        if let Some(teams) = self.board.take_teams() {
            self.teams.send_replace(Arc::new(teams));
        }
    }

    fn publish_criteria(&mut self) {
        if !self.board.take_objectives_changed() {
            return;
        }
        let criteria = self.board.criteria();
        self.criteria.send_if_modified(|current| {
            if **current == criteria {
                return false;
            }
            *current = Arc::new(criteria);
            true
        });
    }

    fn deliver(&mut self) {
        let packets = self.board.take_outbox();
        if packets.is_empty() {
            return;
        }
        self.subscribers.retain(|outbound| {
            packets
                .iter()
                .all(|packet| outbound.send(packet.clone()).is_ok())
        });
    }

    fn save(&mut self) {
        if !self.persist || !self.board.is_dirty() {
            return;
        }
        let data = saved(self.board.save());
        let backend = self.backend.clone();
        let previous = self.last_save.take();
        self.last_save = Some(tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let written = tokio::task::spawn_blocking(move || {
                backend
                    .documents()
                    .write(Document::SavedData(SAVED_DATA), data)
            })
            .await;
            if let Ok(Err(err)) = written {
                log::error!("failed to save the scoreboard: {err}");
            }
        }));
    }

    fn flush(&mut self, done: oneshot::Sender<()>) {
        self.save();
        match self.last_save.take() {
            Some(pending) => {
                self.last_save = Some(tokio::spawn(async move {
                    let _ = pending.await;
                    let _ = done.send(());
                }));
            }
            None => {
                let _ = done.send(());
            }
        }
    }
}

/// Loads the saved scoreboard and starts its actor.
pub fn start(
    backend: Arc<dyn WorldBackend>,
    clock: watch::Receiver<u64>,
) -> (ScoreboardHandle, JoinHandle<()>) {
    let (inbox, messages) = mpsc::channel(INBOX);
    let (criteria_tx, criteria) = watch::channel(Arc::default());
    let (teams_tx, teams) = watch::channel(Arc::default());
    let task = tuff_supervise::spawn(
        "the scoreboard",
        run(backend, clock, messages, criteria_tx, teams_tx),
    );
    (
        ScoreboardHandle {
            inbox,
            criteria,
            teams,
        },
        task,
    )
}

async fn run(
    backend: Arc<dyn WorldBackend>,
    mut clock: watch::Receiver<u64>,
    mut messages: mpsc::Receiver<Message>,
    criteria: watch::Sender<Arc<HashSet<Criterion>>>,
    teams: watch::Sender<Arc<Teams>>,
) {
    let loaded = {
        let backend = backend.clone();
        tokio::task::spawn_blocking(move || {
            backend.documents().read(Document::SavedData(SAVED_DATA))
        })
        .await
    };
    let (board, persist) = match loaded {
        Ok(Ok(root)) => (Scoreboard::load(&inner(root)), true),
        Ok(Err(err)) => {
            log::error!("failed to read the scoreboard, changes will not be saved: {err}");
            (Scoreboard::default(), false)
        }
        Err(_) => (Scoreboard::default(), false),
    };
    let mut actor = Actor {
        board,
        backend,
        persist,
        subscribers: Vec::new(),
        last_save: None,
        criteria,
        teams,
    };
    actor.board.objectives_changed = true;
    actor.publish_criteria();
    actor.board.teams_changed = true;
    actor.publish_teams();
    let mut next_save = *clock.borrow() + AUTOSAVE_TICKS;
    let mut ticking = true;
    loop {
        tokio::select! {
            message = messages.recv() => match message {
                Some(Message::Run(job)) => {
                    let done = isolated(|| job(&mut actor.board)).flatten();
                    actor.deliver();
                    actor.publish_criteria();
                    actor.publish_teams();
                    if let Some(done) = done {
                        isolated(done);
                    }
                }
                Some(Message::Subscribe(outbound)) => {
                    actor.subscribers.retain(|s| !s.is_closed());
                    if actor
                        .board
                        .join_packets()
                        .into_iter()
                        .all(|packet| outbound.send(packet).is_ok())
                    {
                        actor.subscribers.push(outbound);
                    }
                }
                Some(Message::Flush(done)) => actor.flush(done),
                None => break,
            },
            changed = clock.changed(), if ticking => {
                if changed.is_err() {
                    ticking = false;
                    continue;
                }
                let tick = *clock.borrow_and_update();
                if tick >= next_save {
                    next_save = tick + AUTOSAVE_TICKS;
                    actor.save();
                }
            }
        }
    }
    actor.save();
    if let Some(last) = actor.last_save.take() {
        let _ = last.await;
    }
}

/// Runs a caller's closure, logging and counting a panic instead of
/// passing it on to the actor.
fn isolated<T>(work: impl FnOnce() -> T) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
        Ok(value) => Some(value),
        Err(panic) => {
            tuff_supervise::count_panic();
            log::error!(
                "a scoreboard change panicked: {}; the scoreboard keeps what it did",
                tuff_supervise::panic_message(&*panic)
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_storage::memory::MemoryBackend;

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panicking_change_is_dropped_and_the_scoreboard_keeps_running() {
        let (_clock_tx, clock) = watch::channel(0);
        let (board, _task) = start(Arc::new(MemoryBackend::default()), clock);
        let before = crate::metrics::actor_panics().get();
        let failed = board
            .update(|_| -> u8 { panic!("scoreboard change bug") })
            .await;
        assert_eq!(failed, None);
        assert_eq!(board.read(|_| 7).await, Some(7));
        assert!(crate::metrics::actor_panics().get() > before);
    }
}
