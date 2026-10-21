//! `/data ... storage`: named compounds that data packs and commands keep
//! beside the world, saved per namespace as
//! `minecraft:command_storage_<namespace>`. One actor owns them; saves run
//! on the blocking pool, chained so they land in order.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::storage::level::{inner, saved};
use tuff_nbt::{Compound, Tag};
use tuff_storage::backend::WorldBackend;
use tuff_storage::document::Document;

/// Bounded: every sender is a command that can wait its turn, and the actor
/// never waits on anyone while draining.
const INBOX: usize = 256;

enum Message {
    Get {
        id: String,
        reply: oneshot::Sender<Compound>,
    },
    Set {
        id: String,
        data: Compound,
        reply: oneshot::Sender<()>,
    },
    Flush(oneshot::Sender<()>),
}

/// Where command storage is read and changed.
#[derive(Clone, Debug)]
pub struct StorageHandle {
    inbox: mpsc::Sender<Message>,
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Message")
    }
}

fn split(id: &str) -> (&str, &str) {
    id.split_once(':').unwrap_or(("minecraft", id))
}

impl StorageHandle {
    /// A handle with nothing behind it: reads are empty and writes vanish.
    pub fn detached() -> Self {
        Self {
            inbox: mpsc::channel(1).0,
        }
    }

    /// The compound stored under `id` (`namespace:path`); empty when none.
    pub async fn get(&self, id: &str) -> Compound {
        let (reply, answer) = oneshot::channel();
        let message = Message::Get {
            id: id.to_owned(),
            reply,
        };
        if self.inbox.send(message).await.is_err() {
            return Compound::new();
        }
        answer.await.unwrap_or_default()
    }

    /// Replaces what is stored under `id`; an empty compound removes it.
    pub async fn set(&self, id: &str, data: Compound) {
        let (reply, answer) = oneshot::channel();
        let message = Message::Set {
            id: id.to_owned(),
            data,
            reply,
        };
        if self.inbox.send(message).await.is_ok() {
            let _ = answer.await;
        }
    }

    /// Resolves once every change so far is saved.
    pub async fn flush(&self) {
        let (done, saved) = oneshot::channel();
        if self.inbox.send(Message::Flush(done)).await.is_ok() {
            let _ = saved.await;
        }
    }
}

struct Namespace {
    contents: BTreeMap<String, Compound>,
    persist: bool,
}

struct Actor {
    backend: Arc<dyn WorldBackend>,
    namespaces: HashMap<String, Namespace>,
    last_save: Option<JoinHandle<()>>,
}

fn document(namespace: &str) -> String {
    format!("minecraft:command_storage_{namespace}")
}

impl Actor {
    async fn namespace(&mut self, namespace: &str) -> &mut Namespace {
        if !self.namespaces.contains_key(namespace) {
            let backend = self.backend.clone();
            let name = document(namespace);
            let loaded = tokio::task::spawn_blocking(move || {
                backend.documents().read(Document::SavedData(&name))
            })
            .await;
            let loaded = match loaded {
                Ok(Ok(root)) => {
                    let data = inner(root);
                    let contents = match data.get("contents") {
                        Some(Tag::Compound(c)) => c
                            .iter()
                            .filter_map(|(k, v)| Some((k.to_owned(), v.as_compound()?.clone())))
                            .collect(),
                        _ => BTreeMap::new(),
                    };
                    Namespace {
                        contents,
                        persist: true,
                    }
                }
                Ok(Err(err)) => {
                    log::warn!("failed to read {namespace} command storage: {err}");
                    Namespace {
                        contents: BTreeMap::new(),
                        persist: false,
                    }
                }
                Err(_) => Namespace {
                    contents: BTreeMap::new(),
                    persist: false,
                },
            };
            self.namespaces.insert(namespace.to_owned(), loaded);
        }
        self.namespaces.get_mut(namespace).expect("just inserted")
    }

    fn save(&mut self, namespace: &str) {
        let Some(state) = self.namespaces.get(namespace).filter(|s| s.persist) else {
            return;
        };
        let contents: Compound = state
            .contents
            .iter()
            .map(|(k, v)| (k.clone(), Tag::Compound(v.clone())))
            .collect();
        let data = saved(Compound::new().with("contents", contents));
        let backend = self.backend.clone();
        let name = document(namespace);
        let previous = self.last_save.take();
        self.last_save = Some(tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let written = tokio::task::spawn_blocking(move || {
                backend.documents().write(Document::SavedData(&name), data)
            })
            .await;
            if let Ok(Err(err)) = written {
                log::error!("failed to save command storage: {err}");
            }
        }));
    }

    async fn handle(&mut self, message: Message) {
        match message {
            Message::Get { id, reply } => {
                let (namespace, path) = split(&id);
                let state = self.namespace(namespace).await;
                let _ = reply.send(state.contents.get(path).cloned().unwrap_or_default());
            }
            Message::Set { id, data, reply } => {
                let (namespace, path) = split(&id);
                let state = self.namespace(namespace).await;
                if data.is_empty() {
                    state.contents.remove(path);
                } else {
                    state.contents.insert(path.to_owned(), data);
                }
                self.save(namespace);
                let _ = reply.send(());
            }
            Message::Flush(done) => match self.last_save.take() {
                Some(pending) => {
                    self.last_save = Some(tokio::spawn(async move {
                        let _ = pending.await;
                        let _ = done.send(());
                    }));
                }
                None => {
                    let _ = done.send(());
                }
            },
        }
    }
}

/// Starts the storage actor.
pub fn start(backend: Arc<dyn WorldBackend>) -> (StorageHandle, JoinHandle<()>) {
    let (inbox, mut messages) = mpsc::channel(INBOX);
    let task = tokio::spawn(async move {
        let mut actor = Actor {
            backend,
            namespaces: HashMap::new(),
            last_save: None,
        };
        while let Some(message) = messages.recv().await {
            actor.handle(message).await;
        }
        if let Some(last) = actor.last_save.take() {
            let _ = last.await;
        }
    });
    (StorageHandle { inbox }, task)
}
