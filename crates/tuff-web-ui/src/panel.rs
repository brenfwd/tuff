//! Panel pages and APIs plugins add to the web admin panel.
//!
//! A [`PanelPage`] is an ES module plus the files it loads (stylesheets
//! listed in `styles` are linked by the shell while the page is open),
//! served at `/admin/pages/<id>/<path>` with ETags. The shell lists the
//! pages a session may see from `GET /admin/api/v1/pages`, imports the
//! page's module and calls its exported `mount(element, ctx)`, which may
//! return a cleanup function. `ctx` (typed in
//! `crates/tuff-web-ui/app/src/sdk/context.ts`) offers `api(path, init)` (JSON
//! under `/admin/api/v1` with the CSRF header), `events(topics, names,
//! handler)` (one shared event stream per mounted page; plugin topics are
//! `plugin.<page id>.<name>`, published with `PanelEvents::publish` and
//! readable with the page's scope), `principal`, `scopes` and `toast`.
//!
//! Pages are React apps built with Vite and bun from the plugin crate's
//! build script, using `panelPage` from `crates/tuff-web-ui/app/sdk/vite.ts`:
//! it aliases React to the shell's own copy, so a page ships only its own
//! code. `crates/tuff-feature-builtin-example` is the reference.
//!
//! A [`PanelApi`] is mounted at `/admin/api/v1/plugins/<id>/` behind the
//! panel guard and runs only for sessions holding its scope; finer checks
//! use [`AuthContext::require`]. Plugin actions belong in the audit log
//! (`shared.web.audit.record`).
//!
//! A [`MetricCard`] adds a card to the dashboard for a series the plugin
//! feeds through `Metrics::series`, shown to sessions holding its scope.
//!
//! All three are keyed (pages and APIs by id, cards by series): a second
//! plugin registering the same key fails loading with a conflict naming
//! both plugins.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use bytes::Bytes;
pub use hyper::{Method, StatusCode};

use super::auth::{AuthContext, Scope};
use tuff_http::{Request, Response, RouteOptions};

/// Where page assets are served.
pub const PAGES_PREFIX: &str = "/admin/pages";
/// Where plugin APIs are mounted.
pub const APIS_PREFIX: &str = "/admin/api/v1/plugins";
const MAX_ID: usize = 64;

/// One file of a page, served with a strong ETag.
#[derive(Clone, Debug)]
pub struct Asset {
    pub path: String,
    pub content_type: String,
    pub bytes: Bytes,
}

impl Asset {
    /// A stylesheet compiled into the plugin.
    pub fn stylesheet(path: impl Into<String>, source: &'static str) -> Self {
        Self::new(
            path,
            "text/css; charset=utf-8",
            Bytes::from_static(source.as_bytes()),
        )
    }

    pub fn new(path: impl Into<String>, content_type: impl Into<String>, bytes: Bytes) -> Self {
        Self {
            path: path.into(),
            content_type: content_type.into(),
            bytes,
        }
    }

    /// A JavaScript module compiled into the plugin.
    pub fn module(path: impl Into<String>, source: &'static str) -> Self {
        Self::new(
            path,
            "text/javascript; charset=utf-8",
            Bytes::from_static(source.as_bytes()),
        )
    }
}

/// A page in the panel's navigation.
#[derive(Clone, Debug)]
pub struct PanelPage {
    pub id: String,
    pub title: String,
    /// The navigation group it is listed under.
    pub section: String,
    pub icon: String,
    /// Needed to see the page and fetch its assets.
    pub scope: Scope,
    /// The asset path of the ES module exporting `mount`.
    pub module: String,
    /// Asset paths of stylesheets the shell links while the page is open.
    pub styles: Vec<String>,
    pub assets: Vec<Asset>,
}

impl PanelPage {
    /// A page whose only asset is `module`, listed under "Plugins".
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        scope: Scope,
        module: Asset,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            section: "Plugins".to_owned(),
            icon: String::new(),
            scope,
            module: module.path.clone(),
            styles: Vec::new(),
            assets: vec![module],
        }
    }

    pub fn section(mut self, section: impl Into<String>) -> Self {
        self.section = section.into();
        self
    }

    pub fn icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = icon.into();
        self
    }

    pub fn asset(mut self, asset: Asset) -> Self {
        self.assets.push(asset);
        self
    }

    /// Adds a stylesheet asset the shell links while the page is open.
    pub fn style(mut self, asset: Asset) -> Self {
        self.styles.push(asset.path.clone());
        self.assets.push(asset);
        self
    }

    pub fn asset_at(&self, path: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.path == path)
    }

    /// The URL of one of its assets.
    pub fn asset_url(&self, path: &str) -> String {
        format!("{PAGES_PREFIX}/{}/{path}", self.id)
    }

    /// The URL the shell imports.
    pub fn module_url(&self) -> String {
        self.asset_url(&self.module)
    }

    pub fn validate(&self) -> Result<(), String> {
        valid_id(&self.id)?;
        valid_scope(self.scope)?;
        if self.title.trim().is_empty() {
            return Err(format!("panel page {:?} has no title", self.id));
        }
        for (i, asset) in self.assets.iter().enumerate() {
            if !valid_asset_path(&asset.path) {
                return Err(format!(
                    "panel page {:?} asset path {:?} is not a plain relative path",
                    self.id, asset.path
                ));
            }
            if self.assets[..i].iter().any(|a| a.path == asset.path) {
                return Err(format!(
                    "panel page {:?} lists asset {:?} twice",
                    self.id, asset.path
                ));
            }
        }
        for style in &self.styles {
            if !self
                .asset_at(style)
                .is_some_and(|a| essence(&a.content_type) == "text/css")
            {
                return Err(format!(
                    "panel page {:?} style {style:?} is not a text/css asset",
                    self.id
                ));
            }
        }
        match self.asset_at(&self.module) {
            Some(m) if is_javascript(&m.content_type) => Ok(()),
            Some(_) => Err(format!(
                "panel page {:?} module {:?} is not served as JavaScript",
                self.id, self.module
            )),
            None => Err(format!(
                "panel page {:?} module {:?} is not one of its assets",
                self.id, self.module
            )),
        }
    }
}

/// What a plugin API handler gets: the server, the caller and the request,
/// whose `rest` is the path after `/admin/api/v1/plugins/<id>`.
pub struct ApiCall {
    /// The server; [`ApiCall::server`] gives it as its own type.
    pub server: Arc<dyn std::any::Any + Send + Sync>,
    pub auth: Arc<AuthContext>,
    pub request: Request,
}

impl ApiCall {
    /// The server as `T`, `None` if it is something else.
    pub fn server<T: std::any::Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.server.clone().downcast().ok()
    }
}

pub type ApiFuture = Pin<Box<dyn Future<Output = Response> + Send>>;
pub type ApiHandler = Arc<dyn Fn(ApiCall) -> ApiFuture + Send + Sync>;

pub fn api_handler<F, Fut>(f: F) -> ApiHandler
where
    F: Fn(ApiCall) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    Arc::new(move |call| Box::pin(f(call)))
}

/// A plugin's JSON (or streaming) API under the panel.
#[derive(Clone)]
pub struct PanelApi {
    pub id: String,
    /// Needed to reach any endpoint of the API.
    pub scope: Scope,
    pub options: RouteOptions,
    pub handler: ApiHandler,
}

impl PanelApi {
    /// An API taking JSON bodies of at most 64 KiB.
    pub fn new(id: impl Into<String>, scope: Scope, handler: ApiHandler) -> Self {
        Self {
            id: id.into(),
            scope,
            options: RouteOptions {
                max_body: Some(64 * 1024),
                stream: false,
            },
            handler,
        }
    }

    pub fn options(mut self, options: RouteOptions) -> Self {
        self.options = options;
        self
    }

    /// Where it is mounted.
    pub fn route(&self) -> String {
        format!("{APIS_PREFIX}/{}", self.id)
    }
}

impl std::fmt::Debug for PanelApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PanelApi")
            .field("id", &self.id)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

/// How a dashboard card formats its value. A counter series is shown as
/// its rate per second in any unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    Count,
    Bytes,
    Millis,
}

/// A dashboard card with a sparkline for one metrics series.
#[derive(Clone, Debug)]
pub struct MetricCard {
    /// The name given to `Metrics::series`.
    pub series: String,
    pub title: String,
    pub unit: Unit,
    /// Needed to see the card, on top of `metrics.read`.
    pub scope: Scope,
}

impl MetricCard {
    pub fn new(series: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            series: series.into(),
            title: title.into(),
            unit: Unit::Count,
            scope: Scope::MetricsRead,
        }
    }

    pub fn unit(mut self, unit: Unit) -> Self {
        self.unit = unit;
        self
    }

    pub fn scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        valid_scope(self.scope)?;
        if !super::dashboard::valid_series_name(&self.series) {
            return Err(format!(
                "metric card series {:?} must be 1 to 128 of a-z, 0-9, ., - and _",
                self.series
            ));
        }
        if self.title.trim().is_empty() {
            return Err(format!("metric card {:?} has no title", self.series));
        }
        Ok(())
    }
}

/// The pages, APIs and cards the panel serves, by key.
#[derive(Clone, Debug, Default)]
pub struct Installed {
    pub pages: BTreeMap<String, Arc<PanelPage>>,
    pub apis: BTreeMap<String, Arc<PanelApi>>,
    pub cards: BTreeMap<String, Arc<MetricCard>>,
}

impl Installed {
    /// The dashboard cards `ctx` may see, by series.
    pub fn cards_for(&self, ctx: &AuthContext) -> Vec<&Arc<MetricCard>> {
        self.cards
            .values()
            .filter(|c| ctx.allows(Scope::MetricsRead) && ctx.allows(c.scope))
            .collect()
    }

    /// The pages `ctx` may see, in navigation order.
    pub fn visible(&self, ctx: &AuthContext) -> Vec<&Arc<PanelPage>> {
        let mut pages: Vec<&Arc<PanelPage>> = self
            .pages
            .values()
            .filter(|p| ctx.allows(p.scope))
            .collect();
        pages.sort_by(|a, b| (&a.section, &a.title, &a.id).cmp(&(&b.section, &b.title, &b.id)));
        pages
    }

    /// Whether `ctx` may read event `topic` of a plugin page
    /// (`plugin.<page id>.<name>`).
    pub fn topic_allowed(&self, ctx: &AuthContext, topic: &str) -> bool {
        let Some(rest) = topic.strip_prefix("plugin.") else {
            return false;
        };
        let Some((id, name)) = rest.split_once('.') else {
            return false;
        };
        !name.is_empty()
            && self
                .pages
                .get(id)
                .is_some_and(|page| ctx.allows(page.scope))
    }
}

pub fn valid_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "panel id {id:?} must be 1 to {MAX_ID} of a-z, 0-9, - and _"
        ))
    }
}

pub fn valid_scope(scope: Scope) -> Result<(), String> {
    match scope {
        Scope::Plugin(name)
            if name.is_empty()
                || !name.contains('.')
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) =>
        {
            Err(format!(
                "plugin scope {name:?} must be dotted and namespaced, like myplugin.read"
            ))
        }
        _ => Ok(()),
    }
}

fn valid_asset_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 256
        && path.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

fn essence(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn is_javascript(content_type: &str) -> bool {
    let essence = essence(content_type);
    essence == "text/javascript" || essence == "application/javascript"
}
