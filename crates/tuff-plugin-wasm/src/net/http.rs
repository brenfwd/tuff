//! The HTTP client executor. Each hop gets its own client pinned to the
//! addresses the policy admitted, so a connection can only reach what the
//! owner granted; redirects are followed by hand and every hop is checked
//! like the first.

use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, StatusCode, Url};

use super::{Shared, denied};
use crate::caps::pattern::TargetHost;
use crate::caps::resolve::resolve;
use crate::caps::{Approval, Kind};
use crate::wit::bindings::capability_types::{
    Denied, Handle, Header, HttpDone, HttpRequest, HttpResponse, IoEvent,
};

const MAX_REDIRECTS: u8 = 10;
const FORBIDDEN: [&str; 11] = [
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
    "trailer",
    "keep-alive",
    "expect",
    "proxy-authorization",
    "proxy-connection",
];

struct Prepared {
    method: Method,
    headers: HeaderMap,
    body: Vec<u8>,
    decompress: bool,
    max_redirects: u8,
    timeout: Duration,
}

pub(super) fn tls() -> rustls::ClientConfig {
    static CONFIG: OnceLock<rustls::ClientConfig> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            #[allow(unused_mut)]
            let mut roots =
                rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            #[cfg(test)]
            roots
                .add(rustls::pki_types::CertificateDer::from(
                    &include_bytes!("testdata/ca.der")[..],
                ))
                .expect("the test CA parses");
            rustls::ClientConfig::builder_with_provider(Arc::new(rustls_rustcrypto::provider()))
                .with_safe_default_protocol_versions()
                .expect("the default protocol versions are supported")
                .with_root_certificates(roots)
                .with_no_client_auth()
        })
        .clone()
}

fn prepare(request: &HttpRequest, shared: &Shared) -> Result<Prepared, Denied> {
    let limits = shared.limits();
    if request.body.len() as u64 > limits.request_bytes {
        return Err(Denied::Quota(format!(
            "the request body is over {} bytes",
            limits.request_bytes
        )));
    }
    let method = Method::from_bytes(request.method.to_ascii_uppercase().as_bytes())
        .ok()
        .filter(|method| method != Method::CONNECT)
        .ok_or_else(|| Denied::Malformed(format!("{:?} is not an HTTP method", request.method)))?;
    let mut headers = HeaderMap::new();
    for header in &request.headers {
        let bad = |what: &str| Denied::Malformed(format!("header {:?}: {what}", header.name));
        let name = HeaderName::from_bytes(header.name.as_bytes()).map_err(|_| bad("bad name"))?;
        if FORBIDDEN.contains(&name.as_str()) {
            return Err(bad("the host sets this header"));
        }
        let value = HeaderValue::from_bytes(&header.value).map_err(|_| bad("bad value"))?;
        headers.append(name, value);
    }
    let encoding = if request.decompress {
        HeaderValue::from_static("gzip")
    } else if headers.contains_key("accept-encoding") {
        headers["accept-encoding"].clone()
    } else {
        HeaderValue::from_static("identity")
    };
    headers.insert("accept-encoding", encoding);
    let timeout = match request.timeout_ms {
        Some(ms) => limits.request_timeout.min(Duration::from_millis(ms.into())),
        None => limits.request_timeout,
    };
    Ok(Prepared {
        method,
        headers,
        body: request.body.clone(),
        decompress: request.decompress,
        max_redirects: request.max_redirects.min(MAX_REDIRECTS),
        timeout,
    })
}

pub(super) fn start(
    shared: &Arc<Shared>,
    runtime: &tokio::runtime::Handle,
    request: HttpRequest,
) -> Result<Handle, Denied> {
    let approval = shared
        .policy()
        .check_url(Kind::Http, &request.url)
        .map_err(denied)?;
    let prepared = prepare(&request, shared)?;
    let slot = shared.connect_slot()?;
    let task = shared.clone();
    shared
        .handles
        .open(|handle| {
            runtime
                .spawn(async move {
                    let _slot = slot;
                    let current = Mutex::new(approval.target.url_text());
                    let work = run(&task, approval, prepared, &current);
                    let revoked = task.revoked(|policy| {
                        let url = current.lock().unwrap().clone();
                        policy.check_url(Kind::Http, &url).map(drop)
                    });
                    let outcome = tokio::select! {
                        outcome = work => outcome,
                        why = revoked => Err(why),
                    };
                    task.handles.finish(handle);
                    task.events
                        .send(IoEvent::Http(HttpDone {
                            request: handle,
                            outcome,
                        }))
                        .await;
                })
                .abort_handle()
        })
        .ok_or_else(|| Denied::Quota("the plugin used all its handles".into()))
}

async fn run(
    shared: &Shared,
    first: Approval,
    mut prepared: Prepared,
    current: &Mutex<String>,
) -> Result<HttpResponse, Denied> {
    let limits = shared.limits();
    let timeout = prepared.timeout;
    let work = async {
        let mut approval = first;
        let mut hop = 0;
        loop {
            let url = approval.target.url_text();
            let addrs = resolve(&approval, &*shared.resolver, limits.connect_timeout)
                .await
                .map_err(denied)?;
            let mut client = reqwest::Client::builder()
                .tls_backend_preconfigured(tls())
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .http1_only()
                .pool_max_idle_per_host(0)
                .connect_timeout(limits.connect_timeout);
            if let TargetHost::Name(name) = &approval.target.host {
                client = client.resolve_to_addrs(name, &addrs);
            }
            let client = client.build().map_err(failed)?;
            pause(
                shared
                    .usage
                    .sent(Instant::now(), prepared.body.len() as u64),
            )
            .await;
            let mut response = client
                .request(prepared.method.clone(), &url)
                .headers(prepared.headers.clone())
                .body(prepared.body.clone())
                .send()
                .await
                .map_err(failed)?;
            let status = response.status();
            let next = redirect_target(&response, &url, prepared.max_redirects > 0);
            if let Some(next) = next {
                hop += 1;
                if hop > prepared.max_redirects {
                    return Err(Denied::Failed("too many redirects".into()));
                }
                let next = next.map_err(Denied::Malformed)?;
                let policy = shared.policy();
                let allowed = policy
                    .check_url(Kind::Http, next.as_str())
                    .map_err(denied)?;
                if allowed.target.host != approval.target.host
                    || allowed.target.port != approval.target.port
                {
                    prepared.headers.remove("authorization");
                    prepared.headers.remove("cookie");
                }
                if switches_to_get(status, &prepared.method) {
                    prepared.method = Method::GET;
                    prepared.body.clear();
                }
                *current.lock().unwrap() = allowed.target.url_text();
                approval = allowed;
                continue;
            }
            return read(shared, &mut response, &prepared, url).await;
        }
    };
    tokio::time::timeout(timeout, work)
        .await
        .map_err(|_| Denied::TimedOut)?
}

fn redirect_target(
    response: &reqwest::Response,
    from: &str,
    follow: bool,
) -> Option<Result<Url, String>> {
    if !follow || !response.status().is_redirection() {
        return None;
    }
    if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let location = response.headers().get("location")?;
    let resolved = location
        .to_str()
        .map_err(|_| "the Location header is not text".to_owned())
        .and_then(|location| {
            Url::parse(from)
                .and_then(|base| base.join(location))
                .map_err(|err| format!("bad redirect: {err}"))
        })
        .map(|mut url| {
            url.set_fragment(None);
            url
        });
    Some(resolved)
}

fn switches_to_get(status: StatusCode, method: &Method) -> bool {
    match status.as_u16() {
        303 => method != Method::HEAD,
        301 | 302 => method == Method::POST,
        _ => false,
    }
}

async fn read(
    shared: &Shared,
    response: &mut reqwest::Response,
    prepared: &Prepared,
    url: String,
) -> Result<HttpResponse, Denied> {
    let limit = shared.limits().response_bytes;
    let over = || Denied::Quota(format!("the response body is over {limit} bytes"));
    if response.content_length().is_some_and(|len| len > limit) {
        return Err(over());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        if body.len() as u64 + chunk.len() as u64 > limit {
            return Err(over());
        }
        body.extend_from_slice(&chunk);
        pause(shared.usage.received(Instant::now(), chunk.len() as u64)).await;
    }
    let mut headers: Vec<Header> = response
        .headers()
        .iter()
        .map(|(name, value)| Header {
            name: name.to_string(),
            value: value.as_bytes().to_vec(),
        })
        .collect();
    let gzipped = response
        .headers()
        .get("content-encoding")
        .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"gzip"));
    if prepared.decompress && gzipped {
        let mut inflated = Vec::new();
        flate2::read::GzDecoder::new(body.as_slice())
            .take(limit + 1)
            .read_to_end(&mut inflated)
            .map_err(|err| Denied::Failed(format!("bad gzip body: {err}")))?;
        if inflated.len() as u64 > limit {
            return Err(over());
        }
        body = inflated;
        headers.retain(|h| h.name != "content-encoding" && h.name != "content-length");
    }
    Ok(HttpResponse {
        status: response.status().as_u16(),
        headers,
        body,
        url,
    })
}

async fn pause(wait: Duration) {
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
}

fn failed(err: reqwest::Error) -> Denied {
    if err.is_timeout() {
        return Denied::TimedOut;
    }
    let mut text = err.to_string();
    let mut source = std::error::Error::source(&err);
    while let Some(next) = source {
        text = format!("{text}: {next}");
        source = next.source();
    }
    Denied::Failed(text)
}
