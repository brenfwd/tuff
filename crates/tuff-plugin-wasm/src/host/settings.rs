//! What a plugin declares beyond commands and events: settings it owns in
//! the server's configuration and resource packs it offers. Both are added
//! when the plugin is enabled and removed with it.

use std::collections::HashMap;

use tuff_core::config::Value;
use tuff_core::resource_pack::{PackSource, ResourcePack};
use tuff_core::world::commands::text;

use super::Actor;
use crate::abi;
use crate::sandbox::{self, Registration};
use crate::wit::bindings::types::{ConfigChange, Event, EventKinds};

impl Actor {
    /// Registers plugin `id`'s settings and packs; returns each setting's
    /// value as of now.
    pub(super) async fn add_extras(
        &self,
        id: &str,
        registration: &Registration,
    ) -> HashMap<String, Value> {
        let config = &self.shared.config;
        let mut values = HashMap::new();
        for setting in &registration.settings {
            if let Err(err) = config.register(setting.to_key(id)) {
                log::warn!("wasm plugin {id}: setting {}: {err}", setting.key);
            }
            if let Some(value) = config.get(&abi::setting_name(id, &setting.key)) {
                values.insert(setting.key.clone(), value);
            }
        }
        let packs = &self.shared.resource_packs;
        for pack in &registration.packs {
            let pack_id = pack.id(id);
            let mut hosted = ResourcePack::new(
                pack_id,
                &format!("{id}:{}", pack.name),
                PackSource::Bytes(pack.data.clone()),
            )
            .required(pack.required)
            .offer_on_join(true);
            if let Some(prompt) = &pack.prompt {
                hosted = hosted.prompt(text(prompt.clone()));
            }
            packs.register(hosted).await;
            packs.push(None, pack_id).await;
        }
        values
    }

    /// Adds plugin `id`'s panel pages, APIs and cards, failing the load
    /// when one is invalid or its key is taken.
    pub(super) async fn add_panel(
        &self,
        id: &str,
        registration: &Registration,
    ) -> Result<(), String> {
        if registration.panel_pages.is_empty()
            && registration.panel_apis.is_empty()
            && registration.panel_cards.is_empty()
        {
            return Ok(());
        }
        let entries = crate::panel::entries_of(id, registration, &self.submitter())
            .map_err(|err| format!("{id}: {err}"))?;
        self.shared
            .web
            .live
            .install(id, entries)
            .await
            .map_err(|err| format!("{id}: {err}"))
    }

    /// Takes plugin `id`'s settings out of the configuration (a saved
    /// value stays saved) and its packs off every client.
    pub(super) async fn drop_extras(&self, id: &str, registration: &Registration) {
        for setting in &registration.settings {
            self.shared
                .config
                .unregister(&abi::setting_name(id, &setting.key));
        }
        for pack in &registration.packs {
            self.shared.resource_packs.remove(pack.id(id)).await;
        }
        if !registration.panel_pages.is_empty()
            || !registration.panel_apis.is_empty()
            || !registration.panel_cards.is_empty()
        {
            self.shared.web.live.remove(id).await;
        }
    }

    /// Tells each subscribed plugin about every one of its settings whose
    /// value changed since it was last told.
    pub(super) async fn settings_changed(&mut self) {
        let mut changes = Vec::new();
        for (id, plugin) in &mut self.plugins {
            if plugin.registration.settings.is_empty() {
                continue;
            }
            let listening = plugin
                .registration
                .events
                .intersects(EventKinds::CONFIG_CHANGE);
            for setting in &plugin.registration.settings {
                let Some(value) = self.shared.config.get(&abi::setting_name(id, &setting.key))
                else {
                    continue;
                };
                if plugin.settings.get(&setting.key) == Some(&value) {
                    continue;
                }
                plugin.settings.insert(setting.key.clone(), value.clone());
                if listening {
                    changes.push((id.clone(), setting.key.clone(), value));
                }
            }
        }
        for (id, key, value) in changes {
            let event = Event::ConfigChange(ConfigChange {
                key,
                value: sandbox::config_value(value),
            });
            self.deliver_event(vec![id], event, None).await;
        }
    }
}
