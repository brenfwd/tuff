//! The host actor's side of plugin HTTP routes: admitting requests against
//! the plugin's live policy, delivering them as `route` events, matching
//! `respond` calls to the requests waiting for them, and keeping the
//! granted top-level aliases registered on the server's HTTP listener.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tuff_http::{Handler, MAIN_LISTENER, RouteOptions, StatusCode, routes_overlap};

use super::{Actor, Request};
use crate::caps::Policy;
use crate::files::validate;
use crate::routes::{self, Admission, Answer, Call, HARD_BODY, PREFIX, Scope, Submit};
use crate::wit::bindings::capability_types::{IoEvent, RouteBody, RouteRequest, RouteResponse};

struct Pending {
    plugin: String,
    scope: Scope,
    path: String,
    answer: oneshot::Sender<Answer>,
}

pub(super) struct RouteState {
    me: mpsc::WeakSender<Request>,
    pending: HashMap<u32, Pending>,
    seq: u32,
    top: HashMap<String, (String, Handler)>,
    base: Option<Handler>,
}

impl RouteState {
    pub(super) fn new(me: mpsc::WeakSender<Request>) -> Self {
        Self {
            me,
            pending: HashMap::new(),
            seq: 0,
            top: HashMap::new(),
            base: None,
        }
    }
}

fn servable(path: &str) -> bool {
    let trimmed = path.trim_matches('/');
    !trimmed.is_empty()
        && trimmed != PREFIX.trim_matches('/')
        && !path.starts_with(&format!("{PREFIX}/"))
}

fn permits(policy: &Policy, scope: &Scope, path: &str) -> bool {
    match scope {
        Scope::Plugin(_) => policy.check_route(path).is_ok(),
        Scope::Top(_) => policy.check_top_route(path).is_ok(),
        Scope::Panel(_) => true,
    }
}

fn status_only(status: u16) -> Answer {
    Answer::Bytes {
        status,
        headers: Vec::new(),
        body: Vec::new(),
    }
}

impl Actor {
    pub(super) fn submitter(&self) -> Submit {
        let me = self.routes.me.clone();
        Arc::new(move |call| {
            let me = me.clone();
            Box::pin(async move {
                match me.upgrade() {
                    Some(tx) => tx.send(Request::Route(call)).await.is_ok(),
                    None => false,
                }
            })
        })
    }

    pub(super) async fn register_base(&mut self) {
        let handler = routes::handler(None, self.submitter());
        let options = RouteOptions {
            max_body: Some(HARD_BODY),
            stream: false,
        };
        self.shared
            .http
            .register_route_with(MAIN_LISTENER, PREFIX, options, handler.clone())
            .await;
        self.routes.base = Some(handler);
    }

    pub(super) async fn unregister_base(&mut self) {
        if let Some(handler) = self.routes.base.take() {
            self.shared
                .http
                .unregister_route_if(MAIN_LISTENER, PREFIX, &handler)
                .await;
        }
    }

    pub(super) fn route(&mut self, call: Call) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(self.routing(call))
    }

    async fn routing(&mut self, call: Call) {
        let Call {
            scope,
            method,
            path,
            headers,
            body,
            peer,
            reply,
        } = call;
        self.routes.pending.retain(|_, p| !p.answer.is_closed());
        let refuse = |reply: oneshot::Sender<Admission>, status| {
            let _ = reply.send(Admission::Refused(status));
        };
        let owner = match &scope {
            Scope::Plugin(id) => Some(id.clone()),
            Scope::Top(prefix) => self.routes.top.get(prefix).map(|(id, _)| id.clone()),
            Scope::Panel(id) => Some(id.clone()),
        };
        let Some(id) = owner.filter(|id| self.plugins.contains_key(id)) else {
            return refuse(reply, StatusCode::NOT_FOUND);
        };
        let policy = self.plugins[&id].policy.borrow().clone();
        if !permits(&policy, &scope, &path) {
            return refuse(reply, StatusCode::NOT_FOUND);
        }
        let limits = policy.limits();
        if body.len() as u64 > limits.route_body_bytes {
            return refuse(reply, StatusCode::PAYLOAD_TOO_LARGE);
        }
        let open = self
            .routes
            .pending
            .values()
            .filter(|p| p.plugin == id)
            .count();
        if open >= limits.route_concurrency as usize {
            return refuse(reply, StatusCode::SERVICE_UNAVAILABLE);
        }
        let handle = loop {
            self.routes.seq = self.routes.seq.wrapping_add(1);
            let handle = self.routes.seq | 0x8000_0000;
            if !self.routes.pending.contains_key(&handle) {
                break handle;
            }
        };
        let (answer, waiting) = oneshot::channel();
        self.routes.pending.insert(
            handle,
            Pending {
                plugin: id.clone(),
                scope,
                path: path.clone(),
                answer,
            },
        );
        let _ = reply.send(Admission::Accepted {
            timeout: limits.route_timeout,
            answer: waiting,
        });
        let event = IoEvent::Route(RouteRequest {
            request: handle,
            method,
            path,
            headers,
            body,
            peer,
        });
        match self.call(&id, move |sandbox| sandbox.io(&event)).await {
            Some(Ok(effects)) => {
                self.apply(&id, effects).await;
            }
            Some(Err(fault)) => self.fail(&id, "a route request", &fault).await,
            None => {}
        }
    }

    pub(super) fn finish_routes(&mut self, id: &str, responses: Vec<(u32, RouteResponse)>) {
        for (handle, response) in responses {
            let owned = self
                .routes
                .pending
                .get(&handle)
                .is_some_and(|p| p.plugin == id);
            if !owned {
                continue;
            }
            let Some(pending) = self.routes.pending.remove(&handle) else {
                continue;
            };
            let answer = self.answer_for(id, response);
            let _ = pending.answer.send(answer);
        }
    }

    fn answer_for(&self, id: &str, response: RouteResponse) -> Answer {
        let Some(loaded) = self.plugins.get(id) else {
            return status_only(503);
        };
        let RouteResponse {
            status,
            headers,
            body,
        } = response;
        match body {
            RouteBody::Bytes(body) => {
                let cap = loaded.policy.borrow().limits().message_bytes;
                if body.len() as u64 > cap {
                    return status_only(502);
                }
                Answer::Bytes {
                    status,
                    headers,
                    body,
                }
            }
            RouteBody::File(path) => {
                let Ok(segments) = validate(&path, false) else {
                    return status_only(404);
                };
                if loaded.files.access().denied().is_some() {
                    return status_only(404);
                }
                let root = loaded.files.root().to_path_buf();
                let file = segments
                    .iter()
                    .fold(root.clone(), |dir, part| dir.join(part));
                Answer::File {
                    headers,
                    root,
                    path: file,
                    plugin: id.to_owned(),
                }
            }
        }
    }

    /// Drops requests the plugin's policy no longer allows.
    pub(super) fn recheck_routes(&mut self, id: &str) {
        let Some(loaded) = self.plugins.get(id) else {
            return;
        };
        let policy = loaded.policy.borrow().clone();
        self.routes
            .pending
            .retain(|_, p| p.plugin != id || permits(&policy, &p.scope, &p.path));
    }

    pub(super) fn drop_routes<'a>(
        &'a mut self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        self.routes.pending.retain(|_, p| p.plugin != id);
        Box::pin(self.sync_top_routes(id))
    }

    /// Registers the top-level prefixes plugin `id` is granted and
    /// unregisters the ones it no longer is (all of them once unloaded).
    pub(super) async fn sync_top_routes(&mut self, id: &str) {
        let want: Vec<String> = self.plugins.get(id).map_or_else(Vec::new, |loaded| {
            loaded
                .policy
                .borrow()
                .top_routes()
                .into_iter()
                .filter(|path| servable(path))
                .collect()
        });
        let stale: Vec<String> = self
            .routes
            .top
            .iter()
            .filter(|(path, (owner, _))| owner == id && !want.contains(path))
            .map(|(path, _)| path.clone())
            .collect();
        for path in stale {
            if let Some((_, handler)) = self.routes.top.remove(&path) {
                self.shared
                    .http
                    .unregister_route_if(MAIN_LISTENER, &path, &handler)
                    .await;
            }
        }
        for path in want {
            if self.routes.top.contains_key(&path) {
                continue;
            }
            let handler = routes::handler(Some(path.clone()), self.submitter());
            let options = RouteOptions {
                max_body: Some(HARD_BODY),
                stream: false,
            };
            let registered = self
                .shared
                .http
                .register_route_free(MAIN_LISTENER, &path, options, handler.clone())
                .await;
            match registered {
                true => {
                    self.routes.top.insert(path, (id.to_owned(), handler));
                }
                false => log::warn!(
                    "wasm plugin {id}'s route {path} is not served: it overlaps an existing route"
                ),
            }
        }
    }

    /// The top-level aliases of `id` that another route has replaced.
    pub(super) async fn shadowed_routes(&self, id: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (path, (owner, handler)) in &self.routes.top {
            if owner == id
                && !self
                    .shared
                    .http
                    .route_is(MAIN_LISTENER, path, handler)
                    .await
            {
                out.push(path.clone());
            }
        }
        out.sort();
        out
    }

    /// The top-level routes in `grant` that `id` does not serve yet and
    /// that cannot be, naming the first.
    pub(super) async fn route_conflict(&self, id: &str, policy: &Policy) -> Option<String> {
        let registered = self.shared.http.routes(MAIN_LISTENER).await;
        for path in policy.top_routes() {
            if !servable(&path) {
                return Some(format!("{path} is reserved"));
            }
            let mine = self
                .routes
                .top
                .get(&path)
                .is_some_and(|(owner, _)| owner == id);
            let key = format!("/{}", path.trim_matches('/'));
            if !mine && registered.iter().any(|taken| routes_overlap(taken, &key)) {
                return Some(format!("{path} overlaps a route that is already served"));
            }
        }
        None
    }
}
