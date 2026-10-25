//! Maps: the empty map that becomes a filled one, the saved map data and
//! its id counter, terrain drawn onto a map held in hand, player and
//! banner markers, and the Map Data packets that keep clients current.

#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "test code is exempt from the door gate"
    )
)]

pub mod crafted;
pub mod data;
pub mod explore;
pub mod markers;
pub mod render;
pub mod service;
mod session;

use std::sync::Arc;

use tuff_core::feature::{Feature, LoadContext};
use tuff_item::load::{LoadError, Phase};

pub use data::MapFrame;
pub use data::{MapBanner, MapData};
pub use service::{Derivation, Framed, Hold, MapId, MapService, StaticMarker};

const ID: &str = "vanilla-maps";

pub struct MapsFeature;

impl Feature for MapsFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            let registries = cx.registries();
            registries
                .services
                .push(Arc::new(service::MapServiceFactory));
            registries.services.push(Arc::new(ExplorationFactory));
            registries
                .session_hooks
                .push(Arc::new(session::MapSessionHooks));
            registries
                .region_hooks
                .push(Arc::new(session::MapRegionHooks));
        }
        Ok(())
    }
}

pub struct ExplorationFactory;

impl tuff_core::world::services::ServiceFactory for ExplorationFactory {
    fn create(
        &self,
        _dimensions: &tuff_core::world::dimension::Dimensions,
    ) -> Arc<dyn tuff_core::world::services::WorldService> {
        Arc::new(
            tuff_core::world::exploration::ExplorationMaps::new(explore::explore)
                .with_locator(explore::locate),
        )
    }
}
