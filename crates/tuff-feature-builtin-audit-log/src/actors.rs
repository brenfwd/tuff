//! The writer and reader threads. The writer owns the only writing
//! connection: records arrive on a bounded queue hooks never wait on, and
//! are written in batches of one transaction each; purges and rollback
//! marks run between batches in bounded steps. The reader answers lookups
//! on its own connection, which WAL lets run beside the writer.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvError, Sender, SyncSender, TryRecvError, TrySendError};

use tokio::sync::oneshot;

use crate::filter::Filter;
use crate::record::Record;
use crate::store::{self, KindScope, Order, Row, Store};

/// Records waiting for the writer; past this a record is dropped rather
/// than making a region wait.
pub const QUEUE: usize = 16_384;
/// Records written per transaction at most.
pub const BATCH: usize = 4_096;
/// Rows a purge deletes per transaction.
pub const PURGE_STEP: usize = 5_000;

enum Msg {
    Record(Box<Record>),
    /// A control message is waiting.
    Wake,
}

pub enum Control {
    Purge {
        before: i64,
        world: Option<String>,
        kinds: KindScope,
        reply: Option<oneshot::Sender<usize>>,
    },
    Mark {
        ids: Vec<i64>,
        rolled_back: bool,
        reply: oneshot::Sender<bool>,
    },
    /// Answers once every record queued before it is written.
    Flush(oneshot::Sender<()>),
    /// Writes what is queued, checkpoints and stops both threads.
    Close(oneshot::Sender<()>),
}

pub enum Read {
    Lookup {
        filter: Box<Filter>,
        order: Order,
        limit: usize,
        offset: usize,
        reply: oneshot::Sender<Result<Vec<Row>, String>>,
    },
    Count {
        filter: Box<Filter>,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    Total {
        reply: oneshot::Sender<Result<u64, String>>,
    },
    Users {
        reply: oneshot::Sender<Result<Vec<String>, String>>,
    },
    /// Closes the reading connection, before the writer's last checkpoint.
    Close(oneshot::Sender<Result<(), String>>),
}

/// The senders the service keeps.
pub struct Handles {
    records: SyncSender<Msg>,
    control: Sender<Control>,
    reads: Sender<Read>,
}

/// Why a record was not queued.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    Full,
    Closed,
}

impl Handles {
    /// Starts both threads on the file at `path`; `worlds` names each
    /// dimension by id index.
    pub fn spawn(path: PathBuf, worlds: Vec<String>) -> std::io::Result<Handles> {
        let (records, records_rx) = mpsc::sync_channel(QUEUE);
        let (control, control_rx) = mpsc::channel();
        let (reads, reads_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("audit-log-writer".to_owned())
            .spawn(move || writer(&path, &worlds, records_rx, control_rx, reads_rx))?;
        Ok(Handles {
            records,
            control,
            reads,
        })
    }

    pub fn record(&self, record: Record) -> Result<(), Refused> {
        self.records
            .try_send(Msg::Record(Box::new(record)))
            .map_err(|err| match err {
                TrySendError::Full(_) => Refused::Full,
                TrySendError::Disconnected(_) => Refused::Closed,
            })
    }

    pub fn control(&self, control: Control) -> bool {
        let sent = self.control.send(control).is_ok();
        let _ = self.records.try_send(Msg::Wake);
        sent
    }

    pub fn read(&self, read: Read) -> bool {
        self.reads.send(read).is_ok()
    }
}

struct Purge {
    before: i64,
    world: Option<String>,
    kinds: KindScope,
    removed: usize,
    reply: Option<oneshot::Sender<usize>>,
}

struct Writer<'a> {
    store: Store,
    worlds: &'a [String],
    batch: Vec<Record>,
}

impl Writer<'_> {
    fn write(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        let worlds = self.worlds;
        let rows = self.batch.iter().map(|record| {
            let world = worlds
                .get(record.world.index())
                .map_or("minecraft:overworld", String::as_str);
            (record, world)
        });
        if let Err(err) = self.store.insert(rows) {
            log::error!("audit log: lost {} records: {err}", self.batch.len());
        }
        self.batch.clear();
    }

    /// Takes queued records until `limit` or the queue is empty; false once
    /// every sender is gone.
    fn drain(&mut self, records: &Receiver<Msg>, limit: usize) -> bool {
        while self.batch.len() < limit {
            match records.try_recv() {
                Ok(Msg::Record(record)) => self.batch.push(*record),
                Ok(Msg::Wake) => {}
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
        true
    }

    /// Writes everything queued so far.
    fn drain_all(&mut self, records: &Receiver<Msg>) {
        loop {
            let open = self.drain(records, BATCH);
            let full = self.batch.len() >= BATCH;
            self.write();
            if !open || !full {
                return;
            }
        }
    }
}

fn writer(
    path: &Path,
    worlds: &[String],
    records: Receiver<Msg>,
    control: Receiver<Control>,
    reads: Receiver<Read>,
) {
    let store = match Store::open(path) {
        Ok(store) => store,
        Err(err) => {
            log::error!("audit log: cannot open {}: {err}", path.display());
            return;
        }
    };
    let reader_path = path.to_owned();
    if let Err(err) = std::thread::Builder::new()
        .name("audit-log-reader".to_owned())
        .spawn(move || reader(&reader_path, reads))
    {
        log::error!("audit log: cannot start the reader: {err}");
    }
    let mut writer = Writer {
        store,
        worlds,
        batch: Vec::with_capacity(BATCH),
    };
    let mut purges: VecDeque<Purge> = VecDeque::new();
    loop {
        let open = if purges.is_empty() {
            match records.recv() {
                Ok(Msg::Record(record)) => {
                    writer.batch.push(*record);
                    true
                }
                Ok(Msg::Wake) => true,
                Err(RecvError) => false,
            }
        } else {
            true
        };
        let open = writer.drain(&records, BATCH) && open;
        writer.write();
        loop {
            match control.try_recv() {
                Ok(Control::Purge {
                    before,
                    world,
                    kinds,
                    reply,
                }) => purges.push_back(Purge {
                    before,
                    world,
                    kinds,
                    removed: 0,
                    reply,
                }),
                Ok(Control::Mark {
                    ids,
                    rolled_back,
                    reply,
                }) => {
                    writer.drain_all(&records);
                    let marked = writer.store.mark(&ids, rolled_back);
                    if let Err(err) = &marked {
                        log::error!("audit log: cannot mark rows: {err}");
                    }
                    let _ = reply.send(marked.is_ok());
                }
                Ok(Control::Flush(reply)) => {
                    writer.drain_all(&records);
                    let _ = reply.send(());
                }
                Ok(Control::Close(reply)) => {
                    writer.drain_all(&records);
                    writer.store.close();
                    let _ = reply.send(());
                    return;
                }
                Err(_) => break,
            }
        }
        if let Some(purge) = purges.front_mut() {
            let step = writer.store.purge(
                purge.before,
                purge.world.as_deref(),
                &purge.kinds,
                PURGE_STEP,
            );
            let done = match step {
                Ok(removed) => {
                    purge.removed += removed;
                    removed < PURGE_STEP
                }
                Err(err) => {
                    log::error!("audit log: purge failed: {err}");
                    true
                }
            };
            if done && let Some(purge) = purges.pop_front() {
                if purge.removed > 0 {
                    log::info!("audit log: purged {} rows", purge.removed);
                }
                if let Some(reply) = purge.reply {
                    let _ = reply.send(purge.removed);
                }
            }
        }
        if !open && purges.is_empty() {
            writer.store.close();
            return;
        }
    }
}

fn reader(path: &Path, reads: Receiver<Read>) {
    let conn = match store::open_reader(path) {
        Ok(conn) => conn,
        Err(err) => {
            log::error!("audit log: cannot open {} to read: {err}", path.display());
            return;
        }
    };
    let text = |err: rusqlite::Error| err.to_string();
    while let Ok(read) = reads.recv() {
        match read {
            Read::Lookup {
                filter,
                order,
                limit,
                offset,
                reply,
            } => {
                let _ =
                    reply.send(store::lookup(&conn, &filter, order, limit, offset).map_err(text));
            }
            Read::Count { filter, reply } => {
                let _ = reply.send(store::count(&conn, &filter).map_err(text));
            }
            Read::Total { reply } => {
                let _ = reply.send(store::total(&conn).map_err(text));
            }
            Read::Users { reply } => {
                let _ = reply.send(store::user_names(&conn).map_err(text));
            }
            Read::Close(reply) => {
                drop(conn);
                let _ = reply.send(Ok(()));
                return;
            }
        }
    }
}
