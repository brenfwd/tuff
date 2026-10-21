//! A backend wrapper that can stop every document and extra-file write
//! from reaching storage, so a copy of the world's files taken meanwhile is
//! consistent. The chunk storage actor decides when to pause and resume;
//! chunk blobs are held back by that actor itself.
//!
//! A small actor owns the gate: whether writes are paused, how many are
//! under way, and the documents written while paused. `Documents` is
//! synchronous and only ever called on the blocking pool, so each write
//! asks the actor for admission and blocks on the answer
//! (`blocking_recv` panics on a runtime thread, which keeps it there).
//! Reads skip the actor unless documents are being held back.

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::sync::{mpsc, oneshot, watch};

use crate::backend::{BlobCodec, Documents, RegionStore, WorldBackend, WorldLock};
use crate::document::{Document, DocumentId};

/// Decides what a panic of the gate actor means for the server.
pub trait Supervisor: Send + 'static {
    /// Reports `panic`; true if the actor should resume serving.
    fn panicked(&mut self, panic: &(dyn Any + Send)) -> bool;
    /// Reports a panic during the actor's final cleanup.
    fn panicked_closing(&mut self, panic: &(dyn Any + Send));
}

/// Logs a panic and resumes, giving up after three.
struct LogOnly(u32);

impl Supervisor for LogOnly {
    fn panicked(&mut self, _: &(dyn Any + Send)) -> bool {
        self.0 += 1;
        log::error!("the document save gate panicked");
        self.0 < 3
    }

    fn panicked_closing(&mut self, _: &(dyn Any + Send)) {
        log::error!("the document save gate panicked while closing");
    }
}

struct CatchUnwind<F>(F);

impl<F: Future + Unpin> Future for CatchUnwind<F> {
    type Output = Result<F::Output, Box<dyn Any + Send>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = Pin::new(&mut self.0);
        match catch_unwind(AssertUnwindSafe(|| inner.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
            Err(panic) => Poll::Ready(Err(panic)),
        }
    }
}
use crate::config::ConfigStore;
use tuff_nbt::Compound;
use tuff_world_model::pos::FileRegionPos;

enum Request {
    /// Answers the root back if it may be written now, `None` if it was
    /// kept until the resume.
    Write {
        document: DocumentId,
        root: Compound,
        reply: oneshot::Sender<Option<Compound>>,
    },
    /// Admits an extra-file write; false while paused.
    Extra {
        reply: oneshot::Sender<bool>,
    },
    /// An admitted write finished.
    Done,
    Read {
        document: DocumentId,
        reply: oneshot::Sender<Option<Compound>>,
    },
    List {
        reply: oneshot::Sender<Vec<DocumentId>>,
    },
    Pause {
        reply: oneshot::Sender<()>,
    },
    Resume {
        reply: oneshot::Sender<usize>,
    },
    #[cfg(test)]
    Panic,
}

/// Documents held back, each with the version it was written at so a
/// flush only drops the entries it wrote.
type Deferred = HashMap<DocumentId, (u64, Compound)>;

type Flushed = Vec<(DocumentId, u64)>;

struct Actor {
    inner: Arc<dyn WorldBackend>,
    paused: bool,
    /// A resume was asked for and no pause came after it.
    resuming: bool,
    /// Admitted writes under way, a running flush included.
    writing: usize,
    deferred: Deferred,
    version: u64,
    flushing: bool,
    written: usize,
    holding: watch::Sender<bool>,
    pause_waiters: Vec<oneshot::Sender<()>>,
    resume_waiters: Vec<oneshot::Sender<usize>>,
    // Unbounded: at most one flush runs at a time.
    flushed_tx: mpsc::UnboundedSender<Flushed>,
}

impl Actor {
    /// Serves until every handle is gone, then writes what is held back.
    /// The gate is critical: without it no document is saved, so a panic
    /// asks the server to stop and the loop resumes on the same state.
    async fn run(
        mut self,
        mut requests: mpsc::UnboundedReceiver<Request>,
        mut flushed: mpsc::UnboundedReceiver<Flushed>,
        mut critical: Box<dyn Supervisor>,
    ) {
        loop {
            match CatchUnwind(Box::pin(self.serve(&mut requests, &mut flushed))).await {
                Ok(()) => break,
                Err(panic) if critical.panicked(&*panic) => {}
                Err(_) => break,
            }
        }
        if let Err(panic) = CatchUnwind(Box::pin(self.close(&mut flushed))).await {
            critical.panicked_closing(&*panic);
        }
    }

    async fn serve(
        &mut self,
        requests: &mut mpsc::UnboundedReceiver<Request>,
        flushed: &mut mpsc::UnboundedReceiver<Flushed>,
    ) {
        loop {
            tokio::select! {
                request = requests.recv() => match request {
                    Some(request) => self.handle(request),
                    None => return,
                },
                Some(written) = flushed.recv() => self.flushed(written),
            }
        }
    }

    /// Waits out a running flush, then writes what is still held back.
    async fn close(&mut self, flushed: &mut mpsc::UnboundedReceiver<Flushed>) {
        if self.flushing
            && let Some(written) = flushed.recv().await
        {
            self.flushed(written);
        }
        if !self.deferred.is_empty() {
            self.flush();
            if let Some(written) = flushed.recv().await {
                self.flushed(written);
            }
        }
    }

    fn handle(&mut self, request: Request) {
        match request {
            Request::Write {
                document,
                root,
                reply,
            } => {
                if self.paused {
                    self.version += 1;
                    self.deferred.insert(document, (self.version, root));
                    let _ = reply.send(None);
                } else {
                    self.writing += 1;
                    if reply.send(Some(root)).is_err() {
                        self.finished();
                    }
                }
            }
            Request::Extra { reply } => {
                if self.paused {
                    let _ = reply.send(false);
                } else {
                    self.writing += 1;
                    if reply.send(true).is_err() {
                        self.finished();
                    }
                }
            }
            Request::Done => self.finished(),
            Request::Read { document, reply } => {
                let _ = reply.send(self.deferred.get(&document).map(|(_, root)| root.clone()));
            }
            Request::List { reply } => {
                let _ = reply.send(self.deferred.keys().cloned().collect());
            }
            Request::Pause { reply } => {
                self.paused = true;
                self.resuming = false;
                self.holding.send_replace(true);
                if self.writing == 0 {
                    let _ = reply.send(());
                } else {
                    self.pause_waiters.push(reply);
                }
            }
            #[cfg(test)]
            Request::Panic => panic!("document gate test panic"),
            Request::Resume { reply } => {
                if !self.paused && !self.flushing {
                    let _ = reply.send(0);
                    return;
                }
                self.resume_waiters.push(reply);
                if self.paused {
                    self.resuming = true;
                    if !self.flushing {
                        self.flush();
                    }
                }
            }
        }
    }

    fn finished(&mut self) {
        self.writing -= 1;
        if self.writing == 0 {
            for waiter in self.pause_waiters.drain(..) {
                let _ = waiter.send(());
            }
        }
    }

    /// Writes a snapshot of the deferred documents on the blocking pool.
    fn flush(&mut self) {
        self.flushing = true;
        self.writing += 1;
        let snapshot: Vec<(DocumentId, u64, Compound)> = self
            .deferred
            .iter()
            .map(|(id, (version, root))| (id.clone(), *version, root.clone()))
            .collect();
        let inner = self.inner.clone();
        let flushed = self.flushed_tx.clone();
        tokio::task::spawn_blocking(move || {
            let mut written = Vec::with_capacity(snapshot.len());
            for (document, version, root) in snapshot {
                if let Err(err) = inner.documents().write(document.borrow(), root) {
                    log::error!("failed to write held-back {document:?}: {err}");
                }
                written.push((document, version));
            }
            let _ = flushed.send(written);
        });
    }

    fn flushed(&mut self, written: Flushed) {
        self.flushing = false;
        self.written += written.len();
        for (document, version) in written {
            if self
                .deferred
                .get(&document)
                .is_some_and(|(held, _)| *held == version)
            {
                self.deferred.remove(&document);
            }
        }
        self.finished();
        if self.resuming && !self.deferred.is_empty() {
            self.flush();
            return;
        }
        if self.resuming {
            self.paused = false;
            self.resuming = false;
            self.holding.send_replace(false);
        }
        let written = std::mem::take(&mut self.written);
        for waiter in self.resume_waiters.drain(..) {
            let _ = waiter.send(written);
        }
    }
}

/// A handle to the actor gating a [`GatedBackend`]'s writes.
#[derive(Clone)]
pub struct DocumentGate {
    // Unbounded: every sender but the controller blocks on its answer, so
    // the queue is bounded by the blocking pool's threads.
    tx: mpsc::UnboundedSender<Request>,
    holding: watch::Receiver<bool>,
}

impl DocumentGate {
    /// Starts the actor, which writes held-back documents to `inner`.
    pub fn start(inner: Arc<dyn WorldBackend>) -> Self {
        Self::start_supervised(inner, LogOnly(0))
    }

    /// Like [`Self::start`]; a panic of the actor is reported to
    /// `supervisor`.
    pub fn start_supervised(inner: Arc<dyn WorldBackend>, supervisor: impl Supervisor) -> Self {
        let (tx, requests) = mpsc::unbounded_channel();
        let (flushed_tx, flushed) = mpsc::unbounded_channel();
        let (holding_tx, holding) = watch::channel(false);
        let actor = Actor {
            inner,
            paused: false,
            resuming: false,
            writing: 0,
            deferred: HashMap::new(),
            version: 0,
            flushing: false,
            written: 0,
            holding: holding_tx,
            pause_waiters: Vec::new(),
            resume_waiters: Vec::new(),
            flushed_tx,
        };
        tokio::spawn(actor.run(requests, flushed, Box::new(supervisor)));
        Self { tx, holding }
    }

    /// Holds back every later write; answers once the writes already
    /// admitted have finished.
    pub fn pause(&self) -> oneshot::Receiver<()> {
        let (reply, answer) = oneshot::channel();
        let _ = self.tx.send(Request::Pause { reply });
        answer
    }

    /// Writes the held-back documents, then lets writes through again;
    /// answers with how many were written.
    pub fn resume(&self) -> oneshot::Receiver<usize> {
        let (reply, answer) = oneshot::channel();
        let _ = self.tx.send(Request::Resume { reply });
        answer
    }

    /// Whether documents are being held back (paused, or not yet written
    /// out after a resume).
    pub fn holding(&self) -> bool {
        *self.holding.borrow()
    }

    /// Asks the actor and blocks this (blocking-pool) thread for the answer.
    fn ask<T>(&self, request: impl FnOnce(oneshot::Sender<T>) -> Request) -> Option<T> {
        let (reply, answer) = oneshot::channel();
        self.tx.send(request(reply)).ok()?;
        answer.blocking_recv().ok()
    }
}

/// An admitted write; tells the actor when it is done.
struct Admitted<'a>(&'a DocumentGate);

impl Drop for Admitted<'_> {
    fn drop(&mut self) {
        let _ = self.0.tx.send(Request::Done);
    }
}

fn stopped() -> io::Error {
    io::Error::other("the save gate has stopped")
}

fn paused_error(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::ResourceBusy,
        format!("cannot write {what}: world saving is paused"),
    )
}

/// `inner` with its document and extra-file writes behind a
/// [`DocumentGate`].
pub struct GatedBackend {
    inner: Arc<dyn WorldBackend>,
    documents: GatedDocuments,
}

struct GatedDocuments {
    inner: Arc<dyn WorldBackend>,
    gate: DocumentGate,
}

impl GatedBackend {
    pub fn new(inner: Arc<dyn WorldBackend>, gate: DocumentGate) -> Self {
        Self {
            documents: GatedDocuments {
                inner: inner.clone(),
                gate,
            },
            inner,
        }
    }

    fn admit_extra(&self, path: &str) -> io::Result<Admitted<'_>> {
        let gate = &self.documents.gate;
        match gate.ask(|reply| Request::Extra { reply }) {
            Some(true) => Ok(Admitted(gate)),
            Some(false) => Err(paused_error(path)),
            None => Err(stopped()),
        }
    }
}

impl GatedDocuments {
    fn held(&self, document: Document<'_>) -> Option<Compound> {
        if !self.gate.holding() {
            return None;
        }
        let document = DocumentId::from(document);
        self.gate
            .ask(|reply| Request::Read { document, reply })
            .flatten()
    }
}

impl Documents for GatedDocuments {
    fn read(&self, document: Document<'_>) -> io::Result<Option<Compound>> {
        if let Some(root) = self.held(document) {
            return Ok(Some(root));
        }
        self.inner.documents().read(document)
    }

    fn write(&self, document: Document<'_>, root: Compound) -> io::Result<()> {
        let id = DocumentId::from(document);
        let admitted = self.gate.ask(|reply| Request::Write {
            document: id,
            root,
            reply,
        });
        match admitted {
            None => Err(stopped()),
            Some(None) => Ok(()),
            Some(Some(root)) => {
                let _admitted = Admitted(&self.gate);
                self.inner.documents().write(document, root)
            }
        }
    }

    fn exists(&self, document: Document<'_>) -> bool {
        self.held(document).is_some() || self.inner.documents().exists(document)
    }

    fn list(&self) -> io::Result<Vec<DocumentId>> {
        let mut ids = self.inner.documents().list()?;
        if self.gate.holding() {
            for id in self
                .gate
                .ask(|reply| Request::List { reply })
                .unwrap_or_default()
            {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }
}

impl WorldBackend for GatedBackend {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn lock(&self) -> io::Result<Box<dyn WorldLock>> {
        self.inner.lock()
    }

    fn upgrade(&self) -> io::Result<()> {
        self.inner.upgrade()
    }

    fn backup_source(&self) -> crate::backup::BackupSource {
        self.inner.backup_source()
    }

    fn set_entity_index(&self, enabled: bool) {
        self.inner.set_entity_index(enabled);
    }

    fn sql_console(&self) -> Option<crate::sqlite::console::SqlConsole> {
        self.inner.sql_console()
    }

    fn documents(&self) -> &dyn Documents {
        &self.documents
    }

    fn open_region(
        &self,
        dimension: &str,
        region: FileRegionPos,
    ) -> io::Result<Box<dyn RegionStore>> {
        self.inner.open_region(dimension, region)
    }

    fn blob_codec(&self) -> Arc<dyn BlobCodec> {
        self.inner.blob_codec()
    }

    fn config(&self) -> Option<Arc<dyn ConfigStore>> {
        self.inner.config()
    }

    fn list_dimensions(&self) -> io::Result<Vec<String>> {
        self.inner.list_dimensions()
    }

    fn list_regions(&self, dimension: &str) -> io::Result<Vec<FileRegionPos>> {
        self.inner.list_regions(dimension)
    }

    fn list_extra_files(&self) -> io::Result<Vec<String>> {
        self.inner.list_extra_files()
    }

    fn read_extra_file(&self, path: &str) -> io::Result<Vec<u8>> {
        self.inner.read_extra_file(path)
    }

    fn list_extra_files_under(&self, prefix: &str) -> io::Result<Vec<String>> {
        self.inner.list_extra_files_under(prefix)
    }

    fn read_extra_file_capped(&self, path: &str, max: usize) -> io::Result<Vec<u8>> {
        self.inner.read_extra_file_capped(path, max)
    }

    fn write_extra_file(&self, path: &str, data: &[u8]) -> io::Result<()> {
        let _admitted = self.admit_extra(path)?;
        self.inner.write_extra_file(path, data)
    }

    fn remove_extra_file(&self, path: &str) -> io::Result<()> {
        let _admitted = self.admit_extra(path)?;
        self.inner.remove_extra_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryBackend;

    fn gated() -> (Arc<MemoryBackend>, Arc<GatedBackend>, DocumentGate) {
        let inner = Arc::new(MemoryBackend::default());
        let gate = DocumentGate::start(inner.clone());
        let backend = Arc::new(GatedBackend::new(inner.clone(), gate.clone()));
        (inner, backend, gate)
    }

    fn doc(value: i32) -> Compound {
        Compound::new().with("v", value)
    }

    fn value(documents: &dyn Documents, document: Document<'_>) -> Option<i32> {
        documents.read(document).unwrap()?.get_int("v")
    }

    async fn blocking<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> T {
        tokio::task::spawn_blocking(job).await.unwrap()
    }

    async fn write(backend: &Arc<GatedBackend>, document: Document<'static>, root: Compound) {
        let backend = backend.clone();
        blocking(move || backend.documents().write(document, root).unwrap()).await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panic_reaches_the_supervisor_and_keeps_held_documents() {
        struct Stop(mpsc::UnboundedSender<()>);

        impl Supervisor for Stop {
            fn panicked(&mut self, _: &(dyn Any + Send)) -> bool {
                let _ = self.0.send(());
                true
            }

            fn panicked_closing(&mut self, _: &(dyn Any + Send)) {}
        }

        let inner = Arc::new(MemoryBackend::default());
        let (stop, mut stops) = mpsc::unbounded_channel();
        let gate = DocumentGate::start_supervised(inner.clone(), Stop(stop));
        let backend = Arc::new(GatedBackend::new(inner.clone(), gate.clone()));
        gate.pause().await.unwrap();
        write(&backend, Document::Level, doc(1)).await;
        assert!(gate.tx.send(Request::Panic).is_ok());
        assert_eq!(stops.recv().await, Some(()));
        write(&backend, Document::Player(7), doc(2)).await;
        assert_eq!(gate.resume().await.unwrap(), 2);
        assert_eq!(value(inner.as_ref(), Document::Level), Some(1));
        assert_eq!(value(inner.as_ref(), Document::Player(7)), Some(2));
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn paused_writes_stay_in_memory_until_resumed() {
        let (inner, backend, gate) = gated();
        write(&backend, Document::Level, doc(1)).await;
        gate.pause().await.unwrap();
        assert!(gate.holding());
        write(&backend, Document::Level, doc(2)).await;
        write(&backend, Document::Player(7), doc(3)).await;
        let b = backend.clone();
        blocking(move || {
            let documents = b.documents();
            assert_eq!(value(documents, Document::Level), Some(2));
            assert!(documents.exists(Document::Player(7)));
            assert!(documents.list().unwrap().contains(&DocumentId::Player(7)));
        })
        .await;
        assert_eq!(value(inner.as_ref(), Document::Level), Some(1));
        assert!(!inner.exists(Document::Player(7)));

        assert_eq!(gate.resume().await.unwrap(), 2);
        assert!(!gate.holding());
        assert_eq!(value(inner.as_ref(), Document::Level), Some(2));
        assert_eq!(value(inner.as_ref(), Document::Player(7)), Some(3));
        write(&backend, Document::Level, doc(4)).await;
        assert_eq!(value(inner.as_ref(), Document::Level), Some(4));
        assert_eq!(gate.resume().await.unwrap(), 0, "nothing held");
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn extra_file_writes_are_refused_while_paused() {
        let (_, backend, gate) = gated();
        gate.pause().await.unwrap();
        blocking(move || {
            let err = backend
                .write_extra_file("datapacks/x.zip", b"x")
                .unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::ResourceBusy);
            assert!(backend.remove_extra_file("datapacks/x.zip").is_err());
        })
        .await;
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pause_waits_for_an_admitted_write() {
        let (_, backend, gate) = gated();
        let admitted = {
            let gate = gate.clone();
            blocking(move || gate.ask(|reply| Request::Extra { reply })).await
        };
        assert_eq!(admitted, Some(true));
        let mut paused = gate.pause();
        write(&backend, Document::Level, doc(1)).await;
        assert!(paused.try_recv().is_err(), "a write is still under way");
        drop(Admitted(&gate));
        paused.await.unwrap();
        assert_eq!(gate.resume().await.unwrap(), 1);
    }

    #[tuff_core::watchdog]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pause_during_a_resume_keeps_holding() {
        let (inner, backend, gate) = gated();
        gate.pause().await.unwrap();
        write(&backend, Document::Level, doc(1)).await;
        let resumed = gate.resume();
        gate.pause().await.unwrap();
        assert_eq!(resumed.await.unwrap(), 1);
        assert!(gate.holding());
        assert_eq!(value(inner.as_ref(), Document::Level), Some(1));
        write(&backend, Document::Level, doc(2)).await;
        assert_eq!(value(inner.as_ref(), Document::Level), Some(1));
        assert_eq!(gate.resume().await.unwrap(), 1);
        assert_eq!(value(inner.as_ref(), Document::Level), Some(2));
    }
}
