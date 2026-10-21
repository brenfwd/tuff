//! Where raids outlive their regions: each dimension's raids are kept as
//! the `minecraft:raids` saved data, in the game's format. The region
//! running a raid sends a snapshot of it every second; a region asks once
//! when it starts for the saved raids centred in it and takes them over.
//! One actor owns the snapshots and writes them when dirty on autosave
//! and on every flush.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tuff_core::storage::level::{inner, saved};
use tuff_core::world::Shared;
use tuff_core::world::dimension::Dimensions;
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_nbt::{Compound, Tag};
use tuff_storage::backend::WorldBackend;
use tuff_storage::document::Document;
use tuff_world_model::pos::{ChunkPos, RegionPos};

use tuff_world_model::BlockPos;

/// The saved data each dimension's raids are kept in.
pub const SAVED_DATA: &str = "minecraft:raids";
/// Bounded: regions only ever `try_send` (a dropped snapshot is resent a
/// second later; a dropped request is retried next tick), so the actor
/// never makes a region wait.
const INBOX: usize = 1024;
const AUTOSAVE_TICKS: u64 = 6000;
/// How long a region's claim on a raid lasts without a fresh snapshot.
const CLAIM_TICKS: u64 = 200;

/// A saved raid's centre, as the game writes block positions.
pub fn center_of(raid: &Compound) -> Option<BlockPos> {
    match raid.get("center")? {
        Tag::IntArray(v) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
        _ => None,
    }
}

enum Message {
    #[cfg(test)]
    Panic,
    Adopt {
        dimension: String,
        region: RegionPos,
        reply: oneshot::Sender<Vec<Compound>>,
    },
    Update {
        dimension: String,
        region: RegionPos,
        raids: Vec<(u32, Compound)>,
        gone: Vec<u32>,
    },
    Flush(oneshot::Sender<()>),
}

#[derive(Default)]
struct Dimension {
    raids: BTreeMap<u32, Compound>,
    claims: HashMap<u32, (RegionPos, u64)>,
    tick: i32,
    dirty: bool,
    readable: bool,
}

impl Dimension {
    fn to_nbt(&self) -> Compound {
        let raids = self
            .raids
            .iter()
            .map(|(id, raid)| Tag::Compound(raid.clone().with("id", *id as i32)))
            .collect();
        let next = self
            .raids
            .keys()
            .map(|id| (*id as i32).wrapping_add(1))
            .max()
            .unwrap_or(1)
            .max(1);
        Compound::new()
            .with("raids", Tag::List(raids))
            .with("next_id", next)
            .with("tick", self.tick)
    }

    fn from_nbt(data: &Compound) -> Self {
        let mut raids = BTreeMap::new();
        for raid in data.get_list("raids").unwrap_or_default() {
            let Some(raid) = raid.as_compound() else {
                continue;
            };
            let (Some(id), Some(_)) = (raid.get_int("id"), center_of(raid)) else {
                continue;
            };
            let mut raid = raid.clone();
            raid.remove("id");
            raids.insert(id as u32, raid);
        }
        Self {
            raids,
            tick: data.get_int("tick").unwrap_or(0),
            readable: true,
            ..Default::default()
        }
    }
}

struct Actor {
    shared: Arc<Shared>,
    backend: Arc<dyn WorldBackend>,
    dimensions: HashMap<String, Dimension>,
    last_save: Option<JoinHandle<()>>,
}

impl Actor {
    async fn dimension(&mut self, key: &str) -> &mut Dimension {
        if !self.dimensions.contains_key(key) {
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
                Ok(Ok(root)) => Dimension::from_nbt(&inner(root)),
                Ok(Err(err)) => {
                    tuff_core::log::warn!("failed to read the raids of {key}: {err}");
                    Dimension::default()
                }
                Err(_) => Dimension::default(),
            };
            self.dimensions.insert(key.to_owned(), loaded);
        }
        self.dimensions.get_mut(key).expect("just inserted")
    }

    fn now(&self) -> u64 {
        *self.shared.clock.borrow()
    }

    async fn handle(&mut self, message: Message) {
        let now = self.now();
        match message {
            #[cfg(test)]
            Message::Panic => panic!("test panic"),
            Message::Adopt {
                dimension,
                region,
                reply,
            } => {
                let state = self.dimension(&dimension).await;
                let mut taken = Vec::new();
                for (id, raid) in &state.raids {
                    let home = center_of(raid).map(|c| ChunkPos::of_block(c).region());
                    let claimed_elsewhere = state
                        .claims
                        .get(id)
                        .is_some_and(|(by, at)| *by != region && now < at + CLAIM_TICKS);
                    if home == Some(region) && !claimed_elsewhere {
                        taken.push(raid.clone().with("id", *id as i32));
                    }
                }
                for raid in &taken {
                    if let Some(id) = raid.get_int("id") {
                        state.claims.insert(id as u32, (region, now));
                    }
                }
                let _ = reply.send(taken);
            }
            Message::Update {
                dimension,
                region,
                raids,
                gone,
            } => {
                let state = self.dimension(&dimension).await;
                for id in gone {
                    if state.claims.get(&id).is_none_or(|(by, _)| *by == region) {
                        state.raids.remove(&id);
                        state.claims.remove(&id);
                    }
                }
                for (id, raid) in raids {
                    state.raids.insert(id, raid);
                    state.claims.insert(id, (region, now));
                }
                state.tick = now as i32;
                state.dirty = true;
            }
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
        for (key, state) in &mut self.dimensions {
            if !state.dirty || !state.readable {
                continue;
            }
            state.dirty = false;
            writes.push((key.clone(), saved(state.to_nbt())));
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
                        tuff_core::log::error!("failed to save the raids of {dimension}: {err}");
                    }
                }
            })
            .await;
        }));
    }

    async fn run(&mut self, messages: &mut mpsc::Receiver<Message>) {
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

/// The world's raid snapshots, reached by regions through `try_send`.
pub struct RaidStore {
    inbox: mpsc::Sender<Message>,
    start: Mutex<Option<mpsc::Receiver<Message>>>,
}

impl RaidStore {
    /// Asks for the saved raids centred in `region`; `None` when the
    /// store is busy (ask again later).
    pub fn adopt(
        &self,
        dimension: &str,
        region: RegionPos,
    ) -> Option<oneshot::Receiver<Vec<Compound>>> {
        let (reply, answer) = oneshot::channel();
        self.inbox
            .try_send(Message::Adopt {
                dimension: dimension.to_owned(),
                region,
                reply,
            })
            .ok()?;
        Some(answer)
    }

    /// Hands over `region`'s current raids and the ids it no longer runs;
    /// false when the store is busy.
    pub fn update(
        &self,
        dimension: &str,
        region: RegionPos,
        raids: Vec<(u32, Compound)>,
        gone: Vec<u32>,
    ) -> bool {
        self.inbox
            .try_send(Message::Update {
                dimension: dimension.to_owned(),
                region,
                raids,
                gone,
            })
            .is_ok()
    }
}

impl WorldService for RaidStore {
    fn start(&self, shared: &Arc<Shared>) {
        let Some(messages) = self.start.lock().expect("raid store lock").take() else {
            return;
        };
        let actor = Actor {
            shared: shared.clone(),
            backend: shared.backend.clone(),
            dimensions: HashMap::new(),
            last_save: None,
        };
        tuff_supervise::spawn(
            "the raid store",
            tuff_supervise::restarting("the raid store", (actor, messages), |(actor, messages)| {
                Box::pin(actor.run(messages))
            }),
        );
    }

    fn flush(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if self.start.lock().expect("raid store lock").is_some() {
                return;
            }
            let (done, saved) = oneshot::channel();
            if self.inbox.send(Message::Flush(done)).await.is_ok() {
                let _ = saved.await;
            }
        })
    }
}

pub struct RaidStoreFactory;

impl ServiceFactory for RaidStoreFactory {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        let (inbox, messages) = mpsc::channel(INBOX);
        Arc::new(RaidStore {
            inbox,
            start: Mutex::new(Some(messages)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panic_restarts_the_store_keeping_its_state() {
        let (_clock, clock_rx) = tokio::sync::watch::channel(0u64);
        let mut shared = tuff_test_support::bare_shared();
        shared.clock = clock_rx;
        let shared = Arc::new(shared);
        let (inbox, messages) = mpsc::channel(8);
        let actor = Actor {
            backend: shared.backend.clone(),
            shared,
            dimensions: HashMap::new(),
            last_save: None,
        };
        tokio::spawn(tuff_supervise::restarting(
            "the raid store",
            (actor, messages),
            |(actor, messages)| Box::pin(actor.run(messages)),
        ));
        inbox.send(Message::Panic).await.unwrap();
        let (reply, answer) = oneshot::channel();
        inbox
            .send(Message::Adopt {
                dimension: "minecraft:overworld".to_owned(),
                region: RegionPos::new(0, 0),
                reply,
            })
            .await
            .unwrap();
        assert!(answer.await.unwrap().is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn raids_save_in_the_game_layout() {
        let mut dimension = Dimension::default();
        let raid = Compound::new()
            .with("started", true)
            .with("center", Tag::IntArray(vec![1, 64, -3]));
        dimension.raids.insert(7, raid);
        dimension.raids.insert(
            u32::MAX,
            Compound::new().with("center", Tag::IntArray(vec![0, 0, 0])),
        );
        let nbt = dimension.to_nbt();
        let raids = nbt.get_list("raids").unwrap();
        assert_eq!(raids.len(), 2);
        assert_eq!(raids[0].as_compound().unwrap().get_int("id"), Some(7));
        assert_eq!(raids[1].as_compound().unwrap().get_int("id"), Some(-1));
        assert_eq!(nbt.get_int("next_id"), Some(8));
        let read = Dimension::from_nbt(&nbt);
        assert_eq!(read.raids.len(), 2);
        assert_eq!(center_of(&read.raids[&7]), Some(BlockPos::new(1, 64, -3)));
        assert!(read.raids[&u32::MAX].get("id").is_none());
        let broken = Compound::new().with(
            "raids",
            Tag::List(vec![Tag::Compound(Compound::new().with("id", 3))]),
        );
        assert!(Dimension::from_nbt(&broken).raids.is_empty());
    }
}
