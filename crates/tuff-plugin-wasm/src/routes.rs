//! The HTTP side of plugin routes: a handler that turns a request on the
//! server's HTTP listener into a [`Call`] for the host actor and the
//! actor's [`Answer`] back into a response. Whether a path is served at
//! all is decided by the actor against the plugin's live policy.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use tuff_http::{Handler, HeaderName, HeaderValue, Request, Response, StatusCode};
use tuff_web_ui::panel::{ApiHandler, api_handler};

use crate::wit::bindings::capability_types::Header;

/// The one prefix every plugin's own routes live under.
pub const PREFIX: &str = "/plugins";

/// The largest request body the HTTP layer collects for a plugin route;
/// each plugin's own `route-body-bytes` limit applies below it.
pub const HARD_BODY: usize = 16 << 20;

const SANDBOX_POLICY: &str = "sandbox allow-scripts allow-forms allow-popups allow-downloads";

const FORBIDDEN_HEADERS: [&str; 7] = [
    "content-length",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "upgrade",
    "set-cookie",
    "content-security-policy",
];

const HIDDEN_FROM_PLUGINS: [&str; 2] = ["cookie", "authorization"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `/plugins/<id>/...`, the plugin named by the first path segment.
    Plugin(String),
    /// A granted top-level alias, by its registered prefix.
    Top(String),
    /// A panel API of the plugin, already behind the panel's own auth.
    Panel(String),
}

pub struct Call {
    pub scope: Scope,
    pub method: String,
    /// The path below the plugin's prefix (the whole path for an alias),
    /// with the query.
    pub path: String,
    pub headers: Vec<Header>,
    pub body: Vec<u8>,
    pub peer: String,
    pub reply: oneshot::Sender<Admission>,
}

pub enum Admission {
    Refused(StatusCode),
    Accepted {
        timeout: Duration,
        answer: oneshot::Receiver<Answer>,
    },
}

pub enum Answer {
    Bytes {
        status: u16,
        headers: Vec<Header>,
        body: Vec<u8>,
    },
    File {
        headers: Vec<Header>,
        root: PathBuf,
        path: PathBuf,
        plugin: String,
    },
}

pub type Submit = Arc<dyn Fn(Call) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

pub fn handler(scope: Option<String>, submit: Submit) -> Handler {
    tuff_http::handler(move |request: Request| {
        let submit = submit.clone();
        let top = scope.clone();
        async move { serve(request, top, submit).await }
    })
}

/// The panel handler of plugin `id`'s API: whatever the panel admitted
/// reaches the guest as a route request as `/<api id>/<rest>`.
pub fn panel_api(id: String, api: String, submit: Submit) -> ApiHandler {
    api_handler(move |call| {
        let id = id.clone();
        let api = api.clone();
        let submit = submit.clone();
        async move {
            let path = format!("/{api}{}", call.request.rest);
            dispatch(call.request, Scope::Panel(id), path, submit).await
        }
    })
}

fn split(rest: &str) -> Option<(String, String)> {
    let rest = rest.strip_prefix('/')?;
    let (id, tail) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    (!id.is_empty()).then(|| (id.to_owned(), tail.to_owned()))
}

async fn serve(request: Request, top: Option<String>, submit: Submit) -> Response {
    let (scope, path) = match top {
        Some(prefix) => (Scope::Top(prefix), request.path.clone()),
        None => match split(&request.rest) {
            Some((id, tail)) => (Scope::Plugin(id), tail),
            None => return finish(Response::status(StatusCode::NOT_FOUND)),
        },
    };
    dispatch(request, scope, path, submit).await
}

/// Hands `request` to the host actor as `scope` and renders its answer.
pub async fn dispatch(
    request: Request,
    scope: Scope,
    mut path: String,
    submit: Submit,
) -> Response {
    if let Some(query) = &request.query {
        path.push('?');
        path.push_str(query);
    }
    let headers = request
        .headers
        .iter()
        .filter(|(name, _)| !HIDDEN_FROM_PLUGINS.contains(&name.as_str()))
        .map(|(name, value)| Header {
            name: name.as_str().to_owned(),
            value: value.as_bytes().to_vec(),
        })
        .collect();
    let (reply, admission) = oneshot::channel();
    let call = Call {
        scope,
        method: request.method.as_str().to_owned(),
        path,
        headers,
        body: request.body.to_vec(),
        peer: request.remote.to_string(),
        reply,
    };
    if !submit(call).await {
        return finish(Response::status(StatusCode::SERVICE_UNAVAILABLE));
    }
    let (timeout, answer) = match admission.await {
        Ok(Admission::Accepted { timeout, answer }) => (timeout, answer),
        Ok(Admission::Refused(status)) => return finish(Response::status(status)),
        Err(_) => return finish(Response::status(StatusCode::SERVICE_UNAVAILABLE)),
    };
    let answer = match tokio::time::timeout(timeout, answer).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(_)) => return finish(Response::status(StatusCode::SERVICE_UNAVAILABLE)),
        Err(_) => return finish(Response::status(StatusCode::GATEWAY_TIMEOUT)),
    };
    finish(render(&request, answer).await)
}

fn finish(mut response: Response) -> Response {
    response.set_header("x-content-type-options", "nosniff");
    response.set_header("content-security-policy", SANDBOX_POLICY);
    response
}

fn apply_headers(response: &mut Response, headers: &[Header]) {
    for header in headers {
        let name = header.name.to_ascii_lowercase();
        if FORBIDDEN_HEADERS.contains(&name.as_str()) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_bytes(&header.value),
        ) {
            response.headers.retain(|(have, _)| *have != name);
            response.headers.push((name, value));
        }
    }
}

fn header_value<'a>(headers: &'a [Header], name: &str) -> Option<&'a [u8]> {
    headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case(name))
        .map(|header| header.value.as_slice())
}

async fn render(request: &Request, answer: Answer) -> Response {
    match answer {
        Answer::Bytes {
            status,
            headers,
            body,
        } => {
            let Ok(status) = StatusCode::from_u16(status) else {
                return Response::status(StatusCode::BAD_GATEWAY);
            };
            let mut response = Response::new(status, body);
            apply_headers(&mut response, &headers);
            response
        }
        Answer::File {
            headers,
            root,
            path,
            plugin,
        } => {
            let inside = match (
                tokio::fs::canonicalize(&root).await,
                tokio::fs::canonicalize(&path).await,
            ) {
                (Ok(root), Ok(path)) => path.starts_with(root).then_some(path),
                _ => None,
            };
            let Some(path) = inside else {
                return Response::status(StatusCode::NOT_FOUND);
            };
            let kind = header_value(&headers, "content-type")
                .and_then(|value| std::str::from_utf8(value).ok())
                .map_or_else(|| guess_type(&path), str::to_owned);
            let mut response = Response::file(request, &kind, path, &plugin).await;
            apply_headers(&mut response, &headers);
            response
        }
    }
}

fn guess_type(path: &std::path::Path) -> String {
    let kind = match path.extension().and_then(|e| e.to_str()) {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    };
    kind.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn paths_split_into_plugin_and_tail() {
        assert_eq!(split("/map/a/b"), Some(("map".into(), "/a/b".into())));
        assert_eq!(split("/map"), Some(("map".into(), "/".into())));
        assert_eq!(split("/"), None);
        assert_eq!(split(""), None);
    }
}
