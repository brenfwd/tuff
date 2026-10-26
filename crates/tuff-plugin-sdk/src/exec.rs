// SPDX-License-Identifier: MIT
//! A single-threaded executor for the capability calls. The host answers
//! anything that needs I/O later, as an `io-event` carrying the handle the
//! call returned; here each such call is a future that completes when its
//! event arrives, and [`spawn`]ed tasks run inside the plugin's callbacks
//! (the macro-generated `on-io` and timer hooks). Nothing blocks and no
//! threads are involved.
//!
//! A new kind of handle (a WebSocket, a socket) adds a [`Completion`]
//! variant and a [`Call`] constructor; events no wrapper claims reach
//! [`crate::Plugin::io_event`].

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use crate::bindings::tuff::plugin::capability_types as cap;
#[cfg(target_arch = "wasm32")]
use crate::bindings::tuff::plugin::{background, net};

pub use cap::IoEvent;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Kind {
    Http,
    File,
    Reply,
    Background,
}

/// What an event delivered for a handle.
#[derive(Debug)]
pub(crate) enum Completion {
    Http(Result<cap::HttpResponse, cap::Denied>),
    File(Result<cap::FileResult, cap::Denied>),
    Reply(Result<Vec<u8>, cap::CallError>),
    Background(Result<Vec<u8>, cap::BackgroundError>),
}

/// What a socket handle's event stream delivers.
#[derive(Debug, Clone)]
pub(crate) enum SocketEvent {
    Connected(Result<(), cap::Denied>),
    Text(String),
    Data(Vec<u8>),
    Datagram { peer: String, data: Vec<u8> },
    Closed(Close),
}

/// Why and how a socket ended.
#[derive(Debug, Clone)]
pub struct Close {
    /// None for an orderly close.
    pub error: Option<cap::Denied>,
    /// WebSocket close code, when the peer sent one.
    pub code: Option<u16>,
    pub reason: String,
}

#[derive(Default)]
struct Inbox {
    queue: VecDeque<SocketEvent>,
    ended: bool,
}

type Task = Pin<Box<dyn Future<Output = ()>>>;

use crate::generated::TIMER_BASE;

#[derive(Default)]
struct State {
    waiting: HashMap<(Kind, u32), Option<Completion>>,
    inboxes: HashMap<u32, Inbox>,
    tasks: Vec<Task>,
    spawned: Vec<Task>,
    next_timer: u32,
    fired: Vec<u32>,
    polling: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|state| f(&mut state.borrow_mut()))
}

/// Runs `task` until it first waits, then whenever an event or timer
/// arrives. A task that never finishes lives until the plugin unloads.
pub fn spawn(task: impl Future<Output = ()> + 'static) {
    with(|state| state.spawned.push(Box::pin(task)));
    run();
}

fn run() {
    if with(|state| std::mem::replace(&mut state.polling, true)) {
        return;
    }
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        let mut tasks = with(|state| {
            let mut tasks = std::mem::take(&mut state.tasks);
            tasks.append(&mut state.spawned);
            tasks
        });
        tasks.retain_mut(|task| task.as_mut().poll(&mut cx).is_pending());
        let more = with(|state| {
            state.tasks = tasks;
            !state.spawned.is_empty()
        });
        if !more {
            break;
        }
    }
    with(|state| state.polling = false);
}

/// Feeds one `io-event` to the executor and runs the tasks it unblocks.
/// Returns the event when no pending call claimed it.
#[doc(hidden)]
pub fn dispatch(event: IoEvent) -> Option<IoEvent> {
    let (key, completion) = match event {
        IoEvent::Http(done) => ((Kind::Http, done.request), Completion::Http(done.outcome)),
        IoEvent::File(done) => ((Kind::File, done.operation), Completion::File(done.outcome)),
        IoEvent::Reply(done) => ((Kind::Reply, done.handle), Completion::Reply(done.outcome)),
        IoEvent::Background(done) => (
            (Kind::Background, done.handle),
            Completion::Background(done.outcome),
        ),
        other => return dispatch_socket(other),
    };
    let claimed = with(|state| match state.waiting.get_mut(&key) {
        Some(slot) => {
            *slot = Some(completion);
            true
        }
        None => false,
    });
    if claimed {
        run();
    }
    None
}

fn dispatch_socket(event: IoEvent) -> Option<IoEvent> {
    let (handle, converted) = match &event {
        IoEvent::Connected(c) => (c.socket, SocketEvent::Connected(c.outcome.clone())),
        IoEvent::WsMessage(f) => (
            f.socket,
            match &f.message {
                cap::WsMessage::Text(t) => SocketEvent::Text(t.clone()),
                cap::WsMessage::Binary(b) => SocketEvent::Data(b.clone()),
            },
        ),
        IoEvent::Data(d) => (d.socket, SocketEvent::Data(d.data.clone())),
        IoEvent::Datagram(d) => (
            d.socket,
            SocketEvent::Datagram {
                peer: d.peer.clone(),
                data: d.data.clone(),
            },
        ),
        IoEvent::Closed(c) => (
            c.socket,
            SocketEvent::Closed(Close {
                error: c.error.clone(),
                code: c.code,
                reason: c.reason.clone(),
            }),
        ),
        _ => return Some(event),
    };
    let claimed = with(|state| match state.inboxes.get_mut(&handle) {
        Some(inbox) => {
            inbox.ended |= matches!(converted, SocketEvent::Closed(_));
            inbox.queue.push_back(converted);
            true
        }
        None => false,
    });
    if !claimed {
        return Some(event);
    }
    run();
    None
}

/// The plugin's end of a host socket: an ordered stream of its events.
/// Dropping it closes the socket.
pub(crate) struct Socket {
    handle: u32,
}

impl Socket {
    /// Starts listening for `handle`'s events (a connect just returned it).
    pub(crate) fn new(handle: u32) -> Self {
        with(|state| state.inboxes.insert(handle, Inbox::default()));
        Self { handle }
    }

    pub(crate) fn handle(&self) -> u32 {
        self.handle
    }

    /// The next event, or None once the closing event was taken.
    pub(crate) fn recv(&self) -> Recv {
        Recv {
            handle: self.handle,
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let ended = STATE
            .try_with(|state| state.borrow_mut().inboxes.remove(&self.handle))
            .map_or(true, |inbox| inbox.is_none_or(|i| i.ended));
        #[cfg(target_arch = "wasm32")]
        if !ended {
            net::close(self.handle);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = ended;
    }
}

pub(crate) struct Recv {
    handle: u32,
}

impl Future for Recv {
    type Output = Option<SocketEvent>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<SocketEvent>> {
        with(|state| match state.inboxes.get_mut(&self.handle) {
            Some(inbox) => match inbox.queue.pop_front() {
                Some(event) => Poll::Ready(Some(event)),
                None if inbox.ended => Poll::Ready(None),
                None => Poll::Pending,
            },
            None => Poll::Ready(None),
        })
    }
}

/// Feeds a timer token to the executor; true when it was the executor's.
#[doc(hidden)]
pub fn timer(token: u32) -> bool {
    if token < TIMER_BASE {
        return false;
    }
    with(|state| state.fired.push(token));
    run();
    true
}

/// A pending host call; completes with the call's result. Dropping it
/// cancels the call.
pub(crate) struct Call {
    kind: Kind,
    handle: u32,
    done: bool,
    #[cfg_attr(
        not(target_arch = "wasm32"),
        expect(dead_code, reason = "used only by the wasm guest build")
    )]
    cancel: Cancel,
}

/// How dropping an unfinished [`Call`] tells the host to stop.
#[derive(Clone, Copy)]
enum Cancel {
    Nothing,
    Close,
    Background,
}

impl Call {
    fn make(kind: Kind, handle: u32, cancel: Cancel) -> Self {
        with(|state| state.waiting.insert((kind, handle), None));
        Self {
            kind,
            handle,
            done: false,
            cancel,
        }
    }

    pub(crate) fn http(handle: u32) -> Self {
        Self::make(Kind::Http, handle, Cancel::Close)
    }

    pub(crate) fn file(handle: u32) -> Self {
        Self::make(Kind::File, handle, Cancel::Close)
    }

    /// A background task: dropping it cancels the task.
    pub(crate) fn background(handle: u32) -> Self {
        Self::make(Kind::Background, handle, Cancel::Background)
    }

    /// A plugin-message request: a late reply is simply ignored.
    pub(crate) fn reply(handle: u32) -> Self {
        Self::make(Kind::Reply, handle, Cancel::Nothing)
    }
}

impl Future for Call {
    type Output = Completion;

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Completion> {
        let key = (self.kind, self.handle);
        let ready = with(|state| match state.waiting.get_mut(&key) {
            Some(slot) if slot.is_some() => state.waiting.remove(&key).flatten(),
            _ => None,
        });
        match ready {
            Some(completion) => {
                self.done = true;
                Poll::Ready(completion)
            }
            None => Poll::Pending,
        }
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if !self.done {
            let _ = STATE
                .try_with(|state| state.borrow_mut().waiting.remove(&(self.kind, self.handle)));
            #[cfg(target_arch = "wasm32")]
            match self.cancel {
                Cancel::Nothing => {}
                Cancel::Close => net::close(self.handle),
                Cancel::Background => background::cancel(self.handle),
            }
        }
    }
}

/// Completes after a number of server ticks.
pub struct Sleep(Wait);

/// Waits `ticks` server ticks (20 per second).
pub fn sleep(ticks: u64) -> Sleep {
    Sleep(wait(ticks))
}

impl Future for Sleep {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.0).poll(cx).map(|_| ())
    }
}

/// Like [`Sleep`], but completes with false at once when the host
/// refused the timer (while registering, or past the timer limit).
pub(crate) struct Wait {
    token: Option<u32>,
    ticks: u64,
}

pub(crate) fn wait(ticks: u64) -> Wait {
    Wait { token: None, ticks }
}

impl Future for Wait {
    type Output = bool;

    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<bool> {
        match self.token {
            None => {
                let token = with(|state| {
                    state.next_timer = (state.next_timer + 1) % TIMER_BASE;
                    TIMER_BASE + state.next_timer
                });
                if crate::schedule_timer(self.ticks, token as i32) {
                    self.token = Some(token);
                    Poll::Pending
                } else {
                    Poll::Ready(false)
                }
            }
            Some(token) => {
                let fired = with(|state| match state.fired.iter().position(|t| *t == token) {
                    Some(at) => {
                        state.fired.swap_remove(at);
                        true
                    }
                    None => false,
                });
                if fired {
                    Poll::Ready(true)
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub fn complete(handle: u32, completion: Completion) -> Option<IoEvent> {
        let event = match completion {
            Completion::Http(outcome) => IoEvent::Http(cap::HttpDone {
                request: handle,
                outcome,
            }),
            Completion::File(outcome) => IoEvent::File(cap::FileDone {
                operation: handle,
                outcome,
            }),
            Completion::Reply(outcome) => IoEvent::Reply(cap::ReplyDone { handle, outcome }),
            Completion::Background(outcome) => {
                IoEvent::Background(cap::BackgroundDone { handle, outcome })
            }
        };
        dispatch(event)
    }
}
