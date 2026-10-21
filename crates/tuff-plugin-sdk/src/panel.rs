// SPDX-License-Identifier: MIT
//! Web admin panel pages, APIs and dashboard cards.

use crate::{host, types};

pub use types::{PanelUnit, RegisterError};

/// A navigation page: an ES module plus the files it loads. Register with
/// [`Page::register`] while the plugin registers.
#[derive(Clone, Debug)]
pub struct Page {
    id: String,
    title: String,
    section: String,
    icon: String,
    scope: String,
    module: String,
    styles: Vec<String>,
    assets: Vec<types::PanelAsset>,
}

impl Page {
    /// `scope` is a permission node `<plugin id>.<rest>`; `module` is the
    /// path of the module asset.
    pub fn new(id: &str, title: &str, scope: &str, module: &str) -> Self {
        Self {
            id: id.to_owned(),
            title: title.to_owned(),
            section: String::new(),
            icon: String::new(),
            scope: scope.to_owned(),
            module: module.to_owned(),
            styles: Vec::new(),
            assets: Vec::new(),
        }
    }

    pub fn section(mut self, section: &str) -> Self {
        self.section = section.to_owned();
        self
    }

    pub fn icon(mut self, icon: &str) -> Self {
        self.icon = icon.to_owned();
        self
    }

    /// Adds a file served below the page.
    pub fn asset(mut self, path: &str, content_type: &str, data: impl Into<Vec<u8>>) -> Self {
        self.assets.push(types::PanelAsset {
            path: path.to_owned(),
            content_type: content_type.to_owned(),
            data: data.into(),
        });
        self
    }

    /// Adds a stylesheet (an already added asset's path) linked while open.
    pub fn style(mut self, path: &str) -> Self {
        self.styles.push(path.to_owned());
        self
    }

    pub fn register(self) -> Result<(), RegisterError> {
        host::register_panel_page(&types::PanelPage {
            id: self.id,
            title: self.title,
            section: self.section,
            icon: self.icon,
            scope: self.scope,
            module: self.module,
            styles: self.styles,
            assets: self.assets,
        })
    }
}

/// Mounts an API under `/admin/api/v1/plugins/<id>/`; requests arrive as
/// `route` io events, answer with `net.respond`.
pub fn api(id: &str, scope: &str) -> Result<(), RegisterError> {
    host::register_panel_api(&types::PanelApi {
        id: id.to_owned(),
        scope: scope.to_owned(),
    })
}

/// Adds a dashboard card for the series `<plugin id>.<rest>`; `scope` is
/// needed on top of `metrics.read`.
pub fn card(
    series: &str,
    title: &str,
    unit: PanelUnit,
    scope: Option<&str>,
) -> Result<(), RegisterError> {
    host::register_panel_card(&types::PanelCard {
        series: series.to_owned(),
        title: title.to_owned(),
        unit,
        scope: scope.map(str::to_owned),
    })
}

/// Feeds a card's sparkline.
pub fn card_set(series: &str, value: i64) -> Result<(), crate::HostError> {
    host::panel_card_set(series, value).map_err(Into::into)
}
