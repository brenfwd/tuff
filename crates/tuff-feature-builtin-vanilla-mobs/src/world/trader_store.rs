//! The wandering trader spawner's timer, one per dimension, kept as the
//! `minecraft:wandering_trader` saved data in the game's format. Every
//! region of a dimension asks the same actor to step the timer when its
//! own countdown ends; the actor steps it once per period and gives every
//! asker the same answer, so the timer is level-wide and survives restarts.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tuff_core::storage::level::{inner, saved};
use tuff_core::world::Shared;
use tuff_core::world::dimension::Dimensions;
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_nbt::Compound;
use tuff_storage::backend::WorldBackend;
use tuff_storage::document::Document;

pub const SAVED_DATA: &str = "minecraft:wandering_trader";
pub const STEP_TICKS: u64 = 1200;
const SPAWN_DELAY: i32 = 24_000;
const MIN_CHANCE: i32 = 25;
const MAX_CHANCE: i32 = 75;
const CHANCE_STEP: i32 = 25;
const INBOX: usize = 256;
const AUTOSAVE_TICKS: u64 = 6000;

enum Message {
    Step {
        dimension: String,
        now: u64,
        reply: oneshot::Sender<Option<i32>>,
    },
    Visited {
        dimension: String,
    },
    Flush(oneshot::Sender<()>),
}

struct Timer {
    spawn_delay: i32,
    spawn_chance: i32,
    last_step: Option<u64>,
    last_result: Option<i32>,
    dirty: bool,
    readable: bool,
}

impl Default for Timer {
    fn default() -> Self {
        Self {
            spawn_delay: SPAWN_DELAY,
            spawn_chance: MIN_CHANCE,
            last_step: None,
            last_result: None,
            dirty: false,
            readable: true,
        }
    }
}

impl Timer {
    fn to_nbt(&self) -> Compound {
        Compound::new()
            .with("spawn_delay", self.spawn_delay)
            .with("spawn_chance", self.spawn_chance)
    }

    fn from_nbt(data: &Compound) -> Self {
        Self {
            spawn_delay: data.get_int("spawn_delay").unwrap_or(SPAWN_DELAY),
            spawn_chance: data.get_int("spawn_chance").unwrap_or(MIN_CHANCE),
            ..Default::default()
        }
    }

    fn step(&mut self, now: u64) -> Option<i32> {
        if self.last_step.is_some_and(|at| now < at + STEP_TICKS) {
            return self.last_result;
        }
        self.last_step = Some(now);
        self.dirty = true;
        self.spawn_delay -= STEP_TICKS as i32;
        self.last_result = if self.spawn_delay > 0 {
            None
        } else {
            self.spawn_delay = SPAWN_DELAY;
            let chance = self.spawn_chance;
            self.spawn_chance = (chance + CHANCE_STEP).clamp(MIN_CHANCE, MAX_CHANCE);
            Some(chance)
        };
        self.last_result
    }

    fn visited(&mut self) {
        if self.spawn_chance != MIN_CHANCE {
            self.spawn_chance = MIN_CHANCE;
            self.dirty = true;
        }
    }
}

struct Actor {
    shared: Arc<Shared>,
    backend: Arc<dyn WorldBackend>,
    timers: HashMap<String, Timer>,
    last_save: Option<JoinHandle<()>>,
}

impl Actor {
    async fn timer(&mut self, key: &str) -> &mut Timer {
        if !self.timers.contains_key(key) {
            let backend = self.backend.clone();
            let dimension = key.to_owned();
            let read = tokio::task::spawn_blocking(move || {
                backend.documents().read(Document::DimensionData {
                    dimension: &dimension,
                    id: SAVED_DATA,
                })
            })
            .await;
            let loaded = match read {
                Ok(Ok(root)) => Timer::from_nbt(&inner(root)),
                Ok(Err(err)) => {
                    tuff_core::log::warn!("failed to read the trader timer of {key}: {err}");
                    Timer {
                        readable: false,
                        ..Default::default()
                    }
                }
                Err(_) => Timer::default(),
            };
            self.timers.insert(key.to_owned(), loaded);
        }
        self.timers.get_mut(key).expect("just inserted")
    }

    async fn handle(&mut self, message: Message) {
        match message {
            Message::Step {
                dimension,
                now,
                reply,
            } => {
                let result = self.timer(&dimension).await.step(now);
                let _ = reply.send(result);
            }
            Message::Visited { dimension } => self.timer(&dimension).await.visited(),
            Message::Flush(done) => {
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
    }

    fn save(&mut self) {
        let mut writes = Vec::new();
        for (key, timer) in &mut self.timers {
            if !timer.dirty || !timer.readable {
                continue;
            }
            timer.dirty = false;
            writes.push((key.clone(), saved(timer.to_nbt())));
        }
        if writes.is_empty() {
            return;
        }
        let backend = self.backend.clone();
        let previous = self.last_save.take();
        self.last_save = Some(tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let _ = tokio::task::spawn_blocking(move || {
                for (dimension, root) in writes {
                    let document = Document::DimensionData {
                        dimension: &dimension,
                        id: SAVED_DATA,
                    };
                    if let Err(err) = backend.documents().write(document, root) {
                        tuff_core::log::error!(
                            "failed to save the trader timer of {dimension}: {err}"
                        );
                    }
                }
            })
            .await;
        }));
    }

    async fn run(mut self, mut messages: mpsc::Receiver<Message>) {
        let mut clock = self.shared.clock.clone();
        let mut last_autosave = *clock.borrow();
        loop {
            tokio::select! {
                message = messages.recv() => {
                    let Some(message) = message else { break };
                    self.handle(message).await;
                }
                changed = clock.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let now = *clock.borrow_and_update();
                    if now >= last_autosave + AUTOSAVE_TICKS && self.shared.saves().autosaves() {
                        last_autosave = now;
                        self.save();
                    }
                }
            }
        }
        while let Ok(message) = messages.try_recv() {
            self.handle(message).await;
        }
        self.save();
        if let Some(last) = self.last_save.take() {
            let _ = last.await;
        }
    }
}

/// The world's trader timers, reached by regions through `try_send`.
pub struct TraderStore {
    inbox: mpsc::Sender<Message>,
    start: Mutex<Option<mpsc::Receiver<Message>>>,
}

impl TraderStore {
    /// Steps the dimension's timer unless it was stepped within the last
    /// period; the answer is the chance to try for a trader with, when due.
    /// `None` when the store is busy (ask again next tick).
    pub fn step(&self, dimension: &str, now: u64) -> Option<oneshot::Receiver<Option<i32>>> {
        let (reply, answer) = oneshot::channel();
        self.inbox
            .try_send(Message::Step {
                dimension: dimension.to_owned(),
                now,
                reply,
            })
            .ok()?;
        Some(answer)
    }

    /// Reports that a trader came, which resets the chance.
    pub fn visited(&self, dimension: &str) {
        let _ = self.inbox.try_send(Message::Visited {
            dimension: dimension.to_owned(),
        });
    }
}

impl WorldService for TraderStore {
    fn start(&self, shared: &Arc<Shared>) {
        let Some(messages) = self.start.lock().expect("trader store lock").take() else {
            return;
        };
        let actor = Actor {
            shared: shared.clone(),
            backend: shared.backend.clone(),
            timers: HashMap::new(),
            last_save: None,
        };
        tuff_supervise::spawn("the trader timer store", actor.run(messages));
    }

    fn flush(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if self.start.lock().expect("trader store lock").is_some() {
                return;
            }
            let (done, saved) = oneshot::channel();
            if self.inbox.send(Message::Flush(done)).await.is_ok() {
                let _ = saved.await;
            }
        })
    }
}

pub struct TraderStoreFactory;

impl ServiceFactory for TraderStoreFactory {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        let (inbox, messages) = mpsc::channel(INBOX);
        Arc::new(TraderStore {
            inbox,
            start: Mutex::new(Some(messages)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_timer_saves_in_the_game_layout_and_steps_once_per_period() {
        let mut timer = Timer::default();
        assert_eq!(timer.step(1200), None);
        assert_eq!(timer.spawn_delay, 22_800);
        assert_eq!(timer.step(1500), None, "same period, same answer");
        assert_eq!(timer.spawn_delay, 22_800);
        timer.spawn_delay = 1200;
        assert_eq!(timer.step(2400), Some(25));
        assert_eq!((timer.spawn_delay, timer.spawn_chance), (24_000, 50));
        timer.visited();
        assert_eq!(timer.spawn_chance, 25);
        timer.spawn_chance = 75;
        timer.spawn_delay = 1200;
        assert_eq!(timer.step(3600), Some(75));
        assert_eq!(timer.spawn_chance, 75);
        let nbt = timer.to_nbt();
        assert_eq!(nbt.get_int("spawn_delay"), Some(24_000));
        assert_eq!(nbt.get_int("spawn_chance"), Some(75));
        let read = Timer::from_nbt(&nbt);
        assert_eq!((read.spawn_delay, read.spawn_chance), (24_000, 75));
        let empty = Timer::from_nbt(&Compound::new());
        assert_eq!((empty.spawn_delay, empty.spawn_chance), (24_000, 25));
    }
}
