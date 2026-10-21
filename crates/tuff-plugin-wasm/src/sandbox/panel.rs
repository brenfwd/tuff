//! The panel registration imports of a plugin's state.

use std::collections::HashMap;

use tuff_core::metrics::{Kind, Series};

use super::State;
use crate::abi;
use crate::panel as checks;
use crate::wit::bindings::types::{self as wit, HostError, RegisterError};

/// Gauge handles by series. Handles of one name are summed by the metrics
/// actor, so a handle is made once and zeroed when the state is dropped
/// (an unloaded or reloaded plugin leaves no stale value behind).
#[derive(Default)]
pub(super) struct SeriesCache(HashMap<String, Series>);

impl Drop for SeriesCache {
    fn drop(&mut self) {
        for series in self.0.values() {
            series.set(0);
        }
    }
}

impl State {
    fn panel_granted(&self) -> Result<(), RegisterError> {
        if self.group_granted(crate::caps::Kind::Panel) {
            Ok(())
        } else {
            Err(RegisterError::Invalid("cap.panel is not granted".into()))
        }
    }

    pub(super) fn panel_register_page(
        &mut self,
        page: wit::PanelPage,
    ) -> Result<(), RegisterError> {
        self.registering()?;
        self.panel_granted()?;
        checks::page_of(&self.plugin, &page).map_err(RegisterError::Invalid)?;
        let pages = &self.registration.panel_pages;
        if pages.iter().any(|known| known.id == page.id) {
            return Err(RegisterError::Invalid(format!(
                "panel page {} is already registered",
                page.id
            )));
        }
        if pages.len() >= abi::MAX_PANEL_PAGES {
            return Err(RegisterError::LimitReached(abi::MAX_PANEL_PAGES as u32));
        }
        self.registration.panel_pages.push(page);
        Ok(())
    }

    pub(super) fn panel_register_api(&mut self, api: wit::PanelApi) -> Result<(), RegisterError> {
        self.registering()?;
        self.panel_granted()?;
        checks::check_api(&self.plugin, &api).map_err(RegisterError::Invalid)?;
        let apis = &self.registration.panel_apis;
        if apis.iter().any(|known| known.id == api.id) {
            return Err(RegisterError::Invalid(format!(
                "panel api {} is already registered",
                api.id
            )));
        }
        if apis.len() >= abi::MAX_PANEL_APIS {
            return Err(RegisterError::LimitReached(abi::MAX_PANEL_APIS as u32));
        }
        self.registration.panel_apis.push(api);
        Ok(())
    }

    pub(super) fn panel_register_card(
        &mut self,
        card: wit::PanelCard,
    ) -> Result<(), RegisterError> {
        self.registering()?;
        self.panel_granted()?;
        checks::card_of(&self.plugin, &card).map_err(RegisterError::Invalid)?;
        let cards = &self.registration.panel_cards;
        if cards.iter().any(|known| known.series == card.series) {
            return Err(RegisterError::Invalid(format!(
                "panel card {} is already registered",
                card.series
            )));
        }
        if cards.len() >= abi::MAX_PANEL_CARDS {
            return Err(RegisterError::LimitReached(abi::MAX_PANEL_CARDS as u32));
        }
        self.registration.panel_cards.push(card);
        Ok(())
    }

    pub(super) fn panel_set(&mut self, series: &str, value: i64) -> Result<(), HostError> {
        if !self
            .registration
            .panel_cards
            .iter()
            .any(|card| card.series == series)
        {
            return Err(HostError::NotDeclared);
        }
        let Some(env) = &self.env else {
            return Ok(());
        };
        self.panel_series
            .0
            .entry(series.to_owned())
            .or_insert_with(|| env.shared.metrics.series(series, Kind::Gauge))
            .set(value);
        Ok(())
    }
}
