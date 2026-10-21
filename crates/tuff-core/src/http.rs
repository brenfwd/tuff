use hyper::Method;
use tuff_http::{HttpConfig, HttpHandle, Meter, Response};

/// Builds the main listener's config from `tuff.http.*` settings, falling
/// back to the key defaults if they are somehow missing (an embedder's
/// registries did not include core's keys).
pub fn config_from_store(store: &crate::config::ConfigStore) -> HttpConfig {
    let host = store
        .get_ip(crate::config::HTTP_HOST)
        .unwrap_or(std::net::IpAddr::from([0, 0, 0, 0]));
    let port = store.get_port(crate::config::HTTP_PORT).unwrap_or(0);
    HttpConfig {
        hosts: vec![host.to_string()],
        port,
        ..HttpConfig::default()
    }
}

/// Whether a changed config key affects the main HTTP listener's bind
/// address.
pub fn is_bind_key(name: &str) -> bool {
    name == crate::config::HTTP_HOST || name == crate::config::HTTP_PORT
}

struct CoreMeter {
    requests: crate::metrics::Series,
    bytes_out: crate::metrics::Series,
    pack_downloads: crate::metrics::Series,
}

impl Meter for CoreMeter {
    fn served(&self, method: &Method, path: &str, resp: &Response) {
        self.requests.inc();
        if let Some(bytes) = resp.body.bytes() {
            self.bytes_out.add(bytes.len() as i64);
        }
        if *method == Method::GET
            && resp.status.is_success()
            && path.starts_with(crate::resource_pack::ROUTE_PREFIX)
        {
            self.pack_downloads.inc();
        }
    }
}

/// Spawns the HTTP actor under supervision.
pub fn spawn() -> HttpHandle {
    let (handle, server) = tuff_http::start(None);
    spawn_restarting(server);
    handle
}

/// Like [`spawn`], counting requests, bytes and pack downloads in
/// `metrics`.
pub fn spawn_metered(metrics: &crate::metrics::Metrics) -> HttpHandle {
    let core = metrics.core();
    let (handle, server) = tuff_http::start(Some(std::sync::Arc::new(CoreMeter {
        requests: core.http_requests.clone(),
        bytes_out: core.http_bytes_out.clone(),
        pack_downloads: core.pack_downloads.clone(),
    })));
    spawn_restarting(server);
    handle
}

fn spawn_restarting(server: tuff_http::Server) {
    tokio::spawn(tuff_supervise::restarting(
        "the http server",
        server,
        |server| Box::pin(server.run()),
    ));
}
