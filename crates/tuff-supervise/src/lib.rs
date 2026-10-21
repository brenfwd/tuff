//! Panic isolation for long-lived actors: a loop that panics is caught at
//! its task boundary so its owner can log it and recover instead of
//! leaving a dead handle behind.
//!
//! Policies: region actors restart from storage and sessions leave (see
//! their own modules). Actors whose loss would corrupt the world's
//! lifecycle or storage (region manager, coordinator, chunk storage,
//! document gate) are [`Critical`]: they keep serving on their state and
//! ask the server to stop, so the shutdown still saves through them.
//! Actors that only answer requests from their owned state restart in
//! place (see [`restarting`] for those that keep their state in a struct). Every other service runs under [`spawn`], which logs and counts
//! its panic; its handles then see a closed channel, as they would after
//! a shutdown.

use std::any::Any;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::task::{Context, Poll};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Panics after which a critical actor stops resuming and runs its final
/// cleanup directly.
pub const CRITICAL_PANICS: u32 = 3;

/// Polls `inner`, turning a panic during any poll into `Err` with the
/// panic payload. The future is dropped right after a panic, so state it
/// borrowed may be half updated: callers only read what they can trust.
/// Box the future first when it is not `Unpin`.
pub struct CatchUnwind<F>(F);

impl<F: Future + Unpin> CatchUnwind<F> {
    pub fn new(inner: F) -> Self {
        Self(inner)
    }
}

impl<F: Future + Unpin> Future for CatchUnwind<F> {
    type Output = Result<F::Output, Box<dyn Any + Send>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = Pin::new(&mut self.0);
        match std::panic::catch_unwind(AssertUnwindSafe(|| inner.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
            Err(panic) => Poll::Ready(Err(panic)),
        }
    }
}

/// The message a panic was raised with, or a placeholder for payloads that
/// are not strings.
pub fn panic_message(panic: &(dyn Any + Send)) -> &str {
    if let Some(message) = panic.downcast_ref::<&'static str>() {
        message
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message
    } else {
        "non-string panic payload"
    }
}

/// Counts a panic of an actor other than a region or session.
static ACTOR_PANICS: std::sync::LazyLock<Arc<AtomicI64>> = std::sync::LazyLock::new(Arc::default);

/// The process-wide count of panics passed to [`count_panic`].
pub fn actor_panics() -> &'static Arc<AtomicI64> {
    &ACTOR_PANICS
}

pub fn count_panic() {
    ACTOR_PANICS.fetch_add(1, Ordering::Relaxed);
}

/// Spawns a long-lived actor task whose panic is logged under `name` and
/// counted instead of ending the task silently.
pub fn spawn<F>(name: &'static str, future: F) -> JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(logged(name, future))
}

/// Runs `future`, logging and counting a panic under `name`.
pub async fn logged<F: Future<Output = ()>>(name: &'static str, future: F) {
    if let Err(panic) = CatchUnwind::new(Box::pin(future)).await {
        count_panic();
        log::error!(
            "{name} panicked: {}; it has stopped and requests to it now fail",
            panic_message(&*panic)
        );
    }
}

/// Runs `run` on `actor` again after every panic, so an actor that keeps
/// its state in itself resumes with it. Returns when `run` finishes
/// normally. The request being handled at the panic is lost.
pub async fn restarting<A, F>(name: &'static str, mut actor: A, mut run: F)
where
    F: for<'a> FnMut(&'a mut A) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
{
    while let Err(panic) = CatchUnwind::new(run(&mut actor)).await {
        count_panic();
        log::error!(
            "{name} panicked: {}; restarting it with its state",
            panic_message(&*panic)
        );
    }
}

/// How a critical actor asks the server to stop in order. The default
/// only logs: nothing outside the actor can stop.
#[derive(Clone, Default)]
pub struct Fatal(Option<Arc<dyn Fn() + Send + Sync>>);

impl Fatal {
    pub fn new(stop: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(stop)))
    }

    /// Stops through the server's stop channel.
    pub fn channel(stop: mpsc::UnboundedSender<()>) -> Self {
        Self::new(move || {
            let _ = stop.send(());
        })
    }

    fn raise(&self) {
        if let Some(stop) = &self.0 {
            stop();
        }
    }
}

impl std::fmt::Debug for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Fatal"
        } else {
            "Fatal(log only)"
        })
    }
}

/// The panics of an actor the server cannot run without. Each is logged
/// and counted; the first asks the server to stop; the actor resumes
/// serving on its (possibly half updated) state so the shutdown can still
/// save through it, until [`CRITICAL_PANICS`].
#[derive(Debug)]
pub struct Critical {
    name: &'static str,
    fatal: Fatal,
    panics: u32,
}

impl Critical {
    pub fn new(name: &'static str, fatal: Fatal) -> Self {
        Self {
            name,
            fatal,
            panics: 0,
        }
    }

    /// Reports `panic`; true if the actor should resume serving.
    pub fn panicked(&mut self, panic: &(dyn Any + Send)) -> bool {
        self.panics += 1;
        count_panic();
        let message = panic_message(panic);
        let resume = self.panics < CRITICAL_PANICS;
        if resume {
            log::error!(
                "{} panicked: {message}; stopping the server, it keeps serving until then",
                self.name
            );
        } else {
            log::error!(
                "{} panicked {} times: {message}; stopping the server, it no longer serves",
                self.name,
                self.panics
            );
        }
        if self.panics == 1 {
            self.fatal.raise();
        }
        resume
    }

    /// Reports a panic during the actor's final cleanup.
    pub fn panicked_closing(&mut self, panic: &(dyn Any + Send)) {
        self.panics += 1;
        count_panic();
        log::error!(
            "{} panicked while closing: {}; what it had not saved yet is lost",
            self.name,
            panic_message(panic)
        );
        if self.panics == 1 {
            self.fatal.raise();
        }
    }
}

impl tuff_storage::gate::Supervisor for Critical {
    fn panicked(&mut self, panic: &(dyn Any + Send)) -> bool {
        Critical::panicked(self, panic)
    }

    fn panicked_closing(&mut self, panic: &(dyn Any + Send)) {
        Critical::panicked_closing(self, panic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panicking_future_reports_its_message() {
        let caught = CatchUnwind::new(Box::pin(async {
            tokio::task::yield_now().await;
            panic!("boom {}", 7);
        }))
        .await;
        let panic = caught.expect_err("the panic is caught");
        assert_eq!(panic_message(&*panic), "boom 7");
        assert_eq!(CatchUnwind::new(Box::pin(async { 3 })).await.ok(), Some(3));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_critical_actor_stops_the_server_once_and_resumes_a_few_times() {
        let (stop, mut stops) = mpsc::unbounded_channel();
        let mut critical = Critical::new("test actor", Fatal::channel(stop));
        let before = actor_panics().load(Ordering::Relaxed);
        let resumed: Vec<bool> = (0..CRITICAL_PANICS)
            .map(|n| critical.panicked(&format!("boom {n}")))
            .collect();
        assert_eq!(resumed, [true, true, false]);
        assert!(actor_panics().load(Ordering::Relaxed) >= before + 3);
        assert_eq!(stops.try_recv(), Ok(()));
        assert!(stops.try_recv().is_err());
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_spawned_service_panic_is_counted() {
        let before = actor_panics().load(Ordering::Relaxed);
        let task = spawn("test service", async {
            tokio::task::yield_now().await;
            panic!("service bug");
        });
        task.await.expect("the panic is caught inside the task");
        assert!(actor_panics().load(Ordering::Relaxed) > before);
    }
}
