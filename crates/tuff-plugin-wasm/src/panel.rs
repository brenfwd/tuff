//! What a plugin adds to the web admin panel: the checks and the
//! conversion from the declarations a guest makes to core's panel types.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use bytes::Bytes;
use tuff_web_ui::Scope;
use tuff_web_ui::live::Entries;
use tuff_web_ui::panel::{Asset, MetricCard, PanelApi, PanelPage, Unit};

use crate::abi;
use crate::routes::{self, Submit};
use crate::wit::bindings::types::{self as wit, PanelUnit};

/// The longest title, section or icon name.
const MAX_LABEL: usize = 64;

fn intern(name: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES.get_or_init(Mutex::default).lock().unwrap();
    if let Some(known) = names.get(name) {
        return known;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(leaked);
    leaked
}

/// The panel scope for permission node `node`, which must be in the
/// plugin's own namespace.
pub fn scope_of(plugin: &str, node: &str) -> Result<Scope, String> {
    let prefix = format!("{plugin}.");
    if node.len() <= prefix.len() || !node.starts_with(&prefix) {
        return Err(format!(
            "scope {node:?} must be a node of {plugin} ({prefix}...)"
        ));
    }
    let scope = Scope::Plugin(intern(node));
    tuff_web_ui::panel::valid_scope(scope)?;
    Ok(scope)
}

fn label(what: &str, text: &str) -> Result<(), String> {
    if text.len() > MAX_LABEL || text.chars().any(char::is_control) {
        return Err(format!(
            "panel {what} must be plain text of at most {MAX_LABEL} bytes"
        ));
    }
    Ok(())
}

/// Core's page for a declaration, validated.
pub fn page_of(plugin: &str, page: &wit::PanelPage) -> Result<PanelPage, String> {
    label("title", &page.title)?;
    label("section", &page.section)?;
    label("icon", &page.icon)?;
    if page.assets.len() > abi::MAX_PANEL_ASSETS {
        return Err(format!(
            "a panel page may ship {} files",
            abi::MAX_PANEL_ASSETS
        ));
    }
    let total: usize = page.assets.iter().map(|a| a.data.len()).sum();
    if total > abi::MAX_PANEL_BYTES {
        return Err(format!(
            "panel page files are over {} bytes",
            abi::MAX_PANEL_BYTES
        ));
    }
    let scope = scope_of(plugin, &page.scope)?;
    let asset = |a: &wit::PanelAsset| {
        Asset::new(
            a.path.clone(),
            a.content_type.clone(),
            Bytes::from(a.data.clone()),
        )
    };
    let module = page
        .assets
        .iter()
        .find(|a| a.path == page.module)
        .map(asset)
        .ok_or_else(|| format!("module {:?} is not one of the page's files", page.module))?;
    let mut built = PanelPage::new(page.id.clone(), page.title.clone(), scope, module);
    if !page.section.is_empty() {
        built = built.section(page.section.clone());
    }
    built = built.icon(page.icon.clone());
    for other in page.assets.iter().filter(|a| a.path != page.module) {
        built = built.asset(asset(other));
    }
    built.styles = page.styles.clone();
    built.validate()?;
    Ok(built)
}

pub fn card_of(plugin: &str, card: &wit::PanelCard) -> Result<MetricCard, String> {
    let prefix = format!("{plugin}.");
    if card.series.len() <= prefix.len() || !card.series.starts_with(&prefix) {
        return Err(format!("series {:?} must start with {prefix}", card.series));
    }
    label("card title", &card.title)?;
    let mut built =
        MetricCard::new(card.series.clone(), card.title.clone()).unit(match card.unit {
            PanelUnit::Count => Unit::Count,
            PanelUnit::Bytes => Unit::Bytes,
            PanelUnit::Millis => Unit::Millis,
        });
    if let Some(scope) = &card.scope {
        built = built.scope(scope_of(plugin, scope)?);
    }
    built.validate()?;
    Ok(built)
}

pub fn check_api(plugin: &str, api: &wit::PanelApi) -> Result<(), String> {
    tuff_web_ui::panel::valid_id(&api.id)?;
    scope_of(plugin, &api.scope).map(drop)
}

/// Everything plugin `plugin` registered, ready to install. Calls for the
/// APIs go to the host actor through `submit`.
pub fn entries_of(
    plugin: &str,
    registration: &crate::sandbox::Registration,
    submit: &Submit,
) -> Result<Entries, String> {
    let mut entries = Entries::default();
    for page in &registration.panel_pages {
        entries.pages.push(page_of(plugin, page)?);
    }
    for api in &registration.panel_apis {
        let scope = scope_of(plugin, &api.scope)?;
        let handler = routes::panel_api(plugin.to_owned(), api.id.clone(), submit.clone());
        entries
            .apis
            .push(PanelApi::new(api.id.clone(), scope, handler));
    }
    for card in &registration.panel_cards {
        entries.cards.push(card_of(plugin, card)?);
    }
    Ok(entries)
}
