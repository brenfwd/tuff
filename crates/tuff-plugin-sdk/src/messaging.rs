// SPDX-License-Identifier: MIT
//! Messages between loaded plugins: providers declare an interface and
//! route its methods to handlers, callers [`notify`] or [`request`] and
//! await the answer.
//!
//! A missing provider is a cheap `Err`, never a panic. A request future
//! sends on its first poll, so await it (or [`crate::spawn`] it).

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::rc::Rc;

use crate::bindings::tuff::plugin::capability_types as cap;
use crate::exec::{Call, Completion};

pub use cap::{CallError, Incoming, PluginChange};

/// Which provider a message goes to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Target(Option<String>);

impl Target {
    /// The sole provider of the interface (every provider, for a notify).
    pub fn any() -> Self {
        Self(None)
    }

    /// The provider with this plugin id.
    pub fn plugin(id: impl Into<String>) -> Self {
        Self(Some(id.into()))
    }
}

impl From<&str> for Target {
    fn from(id: &str) -> Self {
        Self::plugin(id)
    }
}

#[cfg(not(test))]
use real as host_calls;
#[cfg(test)]
use stub as host_calls;

#[cfg(not(test))]
mod real {
    use super::*;
    use crate::host;

    pub fn provide(interface: &str, methods: &[u64]) -> Result<(), crate::types::RegisterError> {
        host::provide(interface, methods)
    }
    pub fn notify(t: &Target, i: &str, m: u64, p: &[u8]) -> Result<u32, CallError> {
        host::notify(t.0.as_deref(), i, m, p)
    }
    pub fn request(t: &Target, i: &str, m: u64, p: &[u8], ticks: u32) -> Result<u32, CallError> {
        host::request(t.0.as_deref(), i, m, p, ticks)
    }
    pub fn respond(request: u64, outcome: Result<&[u8], &str>) {
        let _ = host::respond(request, outcome);
    }
}

/// Declares the interface with its method keys (register phase only);
/// requests and notifies for those keys run the handlers.
pub fn provide(
    interface: &str,
    handlers: Vec<(u64, Handler)>,
) -> Result<(), crate::types::RegisterError> {
    let keys: Vec<u64> = handlers.iter().map(|(key, _)| *key).collect();
    host_calls::provide(interface, &keys)?;
    ROUTES.with(|routes| {
        let mut routes = routes.borrow_mut();
        for (key, handler) in handlers {
            routes.insert((interface.to_owned(), key), Rc::from(handler));
        }
    });
    Ok(())
}

/// A method implementation: gets the message and the token to answer it.
pub type Handler = Box<dyn Fn(Incoming, Reply)>;

/// Boxes a closure as a [`Handler`].
pub fn handler(f: impl Fn(Incoming, Reply) + 'static) -> Handler {
    Box::new(f)
}

type Routes = HashMap<(String, u64), Rc<dyn Fn(Incoming, Reply)>>;

thread_local! {
    static ROUTES: RefCell<Routes> = RefCell::new(HashMap::new());
}

/// The right to answer one request, once. Move it into [`crate::spawn`] to
/// answer after awaiting something. Dropping it unanswered refuses the
/// request at once so the requester does not wait for its timeout.
#[must_use = "an unused Reply refuses the request"]
pub struct Reply {
    request: Option<u64>,
}

impl Reply {
    pub fn ok(mut self, payload: impl AsRef<[u8]>) {
        self.finish(Ok(payload.as_ref()));
    }

    pub fn err(mut self, text: impl AsRef<str>) {
        self.finish(Err(text.as_ref()));
    }

    fn finish(&mut self, outcome: Result<&[u8], &str>) {
        if let Some(request) = self.request.take() {
            host_calls::respond(request, outcome);
        }
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        self.finish(Err("dropped"));
    }
}

/// Routes an incoming message to its handler. Returns it when it was a
/// notify nobody routed, for [`crate::Plugin::message`].
#[doc(hidden)]
pub fn dispatch(msg: Incoming) -> Option<Incoming> {
    let route = ROUTES.with(|routes| {
        routes
            .borrow()
            .get(&(msg.interface.clone(), msg.method))
            .cloned()
    });
    let token = Reply {
        request: msg.request,
    };
    match route {
        Some(route) => {
            route(msg, token);
            None
        }
        None if msg.request.is_some() => {
            token.err("no such method");
            None
        }
        None => Some(msg),
    }
}

/// One-way message; returns how many deliveries were queued.
pub fn notify(
    target: impl Into<Target>,
    interface: &str,
    method: u64,
    payload: &[u8],
) -> Result<u32, CallError> {
    host_calls::notify(&target.into(), interface, method, payload)
}

/// A request under construction; await it for the provider's answer.
pub struct Call_ {
    target: Target,
    interface: String,
    method: u64,
    payload: Vec<u8>,
    ticks: u32,
}

/// Starts a request. Await the result: `Ok(bytes)` is the provider's
/// answer, `Err` says why there is none.
pub fn request(
    target: impl Into<Target>,
    interface: &str,
    method: u64,
    payload: impl Into<Vec<u8>>,
) -> Call_ {
    Call_ {
        target: target.into(),
        interface: interface.to_owned(),
        method,
        payload: payload.into(),
        ticks: 0,
    }
}

impl Call_ {
    /// Gives up with [`CallError::TimedOut`] after this many ticks.
    pub fn timeout(mut self, ticks: u32) -> Self {
        self.ticks = ticks;
        self
    }
}

type Answer = Pin<Box<dyn Future<Output = Result<Vec<u8>, CallError>>>>;

impl IntoFuture for Call_ {
    type Output = Result<Vec<u8>, CallError>;
    type IntoFuture = Answer;

    fn into_future(self) -> Answer {
        Box::pin(async move {
            let handle = host_calls::request(
                &self.target,
                &self.interface,
                self.method,
                &self.payload,
                self.ticks,
            )?;
            match Call::reply(handle).await {
                Completion::Reply(outcome) => outcome,
                _ => Err(CallError::Refused("mismatched completion".into())),
            }
        })
    }
}

/// The version of a loaded plugin.
pub fn plugin_loaded(id: &str) -> Option<String> {
    crate::host::plugin_loaded(id)
}

#[cfg(test)]
pub(crate) mod stub {
    use super::*;

    #[derive(Default)]
    pub struct Stub {
        pub next_request: Option<Result<u32, CallError>>,
        pub responses: Vec<(u64, Result<Vec<u8>, String>)>,
        pub provided: Vec<(String, Vec<u64>)>,
    }

    thread_local! {
        pub static STUB: RefCell<Stub> = RefCell::new(Stub::default());
    }

    pub fn provide(interface: &str, methods: &[u64]) -> Result<(), crate::types::RegisterError> {
        STUB.with(|s| {
            s.borrow_mut()
                .provided
                .push((interface.into(), methods.into()))
        });
        Ok(())
    }
    pub fn notify(_: &Target, _: &str, _: u64, _: &[u8]) -> Result<u32, CallError> {
        Ok(1)
    }
    pub fn request(_: &Target, _: &str, _: u64, _: &[u8], _: u32) -> Result<u32, CallError> {
        STUB.with(|s| s.borrow_mut().next_request.take().unwrap_or(Ok(1)))
    }
    pub fn respond(request: u64, outcome: Result<&[u8], &str>) {
        let outcome = outcome.map(<[u8]>::to_vec).map_err(str::to_owned);
        let _ = STUB.try_with(|s| s.borrow_mut().responses.push((request, outcome)));
    }
}

#[cfg(test)]
mod tests {
    use super::stub::STUB;
    use super::*;
    use crate::exec::{dispatch as io, sleep, spawn};

    fn incoming(method: u64, request: Option<u64>) -> Incoming {
        Incoming {
            sender: "other".into(),
            interface: "demo".into(),
            method,
            payload: b"in".to_vec(),
            request,
        }
    }

    fn responses() -> Vec<(u64, Result<Vec<u8>, String>)> {
        STUB.with(|s| std::mem::take(&mut s.borrow_mut().responses))
    }

    fn answer(handle: u32, outcome: Result<Vec<u8>, CallError>) {
        let event = cap::IoEvent::Reply(cap::ReplyDone { handle, outcome });
        assert!(io(event).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_request_completes_on_its_reply_event() {
        let seen = Rc::new(RefCell::new(None));
        let out = seen.clone();
        STUB.with(|s| s.borrow_mut().next_request = Some(Ok(7001)));
        spawn(async move {
            *out.borrow_mut() = Some(request("p", "demo", 1, b"x".to_vec()).timeout(5).await);
        });
        assert!(seen.borrow().is_none());
        answer(7001, Ok(b"yes".to_vec()));
        assert_eq!(seen.borrow().clone().unwrap().unwrap(), b"yes".to_vec());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_synchronous_error_resolves_immediately() {
        let seen = Rc::new(RefCell::new(None));
        let out = seen.clone();
        STUB.with(|s| s.borrow_mut().next_request = Some(Err(CallError::NoProvider)));
        spawn(async move {
            *out.borrow_mut() = Some(request(Target::any(), "demo", 1, vec![]).await);
        });
        assert!(matches!(
            seen.borrow().as_ref(),
            Some(Err(CallError::NoProvider))
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_late_reply_after_drop_is_ignored() {
        STUB.with(|s| s.borrow_mut().next_request = Some(Ok(7002)));
        let mut fut = request("p", "demo", 1, vec![]).into_future();
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(fut.as_mut().poll(&mut cx).is_pending());
        drop(fut);
        let event = cap::IoEvent::Reply(cap::ReplyDone {
            handle: 7002,
            outcome: Ok(vec![]),
        });
        assert!(io(event).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn routed_requests_respond_once_and_deferred_replies_wait() {
        responses();
        provide(
            "demo",
            vec![
                (
                    1,
                    handler(|msg, reply| reply.ok([msg.payload.as_slice(), b"!"].concat())),
                ),
                (
                    2,
                    handler(|_, reply| {
                        spawn(async move {
                            sleep(1).await;
                            reply.ok(b"late");
                        });
                    }),
                ),
                (3, handler(|_, _reply| {})),
            ],
        )
        .unwrap();
        assert!(super::dispatch(incoming(1, Some(10))).is_none());
        assert_eq!(responses(), vec![(10, Ok(b"in!".to_vec()))]);

        STUB.with(|s| s.borrow_mut().responses.clear());
        assert!(super::dispatch(incoming(2, Some(11))).is_none());
        assert!(responses().is_empty());
        assert!(crate::exec::timer(crate::generated::TIMER_BASE + 1));
        assert_eq!(responses(), vec![(11, Ok(b"late".to_vec()))]);

        assert!(super::dispatch(incoming(3, Some(12))).is_none());
        assert_eq!(responses(), vec![(12, Err("dropped".into()))]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn unrouted_requests_are_refused_and_notifies_fall_through() {
        responses();
        assert!(super::dispatch(incoming(99, Some(20))).is_none());
        assert_eq!(responses(), vec![(20, Err("no such method".into()))]);
        let raw = super::dispatch(incoming(99, None));
        assert!(raw.is_some());
        assert!(responses().is_empty());
    }
}
