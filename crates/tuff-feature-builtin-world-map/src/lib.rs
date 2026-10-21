//! A live, pannable and zoomable map of the world in the web panel.
//! Tiles are rendered from
//! chunk data on the map's own thread pool, cached by one actor and kept
//! current while the world changes; the panel page draws them on a canvas
//! with player markers.

pub mod api;
pub mod disk;
pub mod entities;
pub mod gather;
pub mod png;
pub mod service;
pub mod surface;
pub mod tile;

use std::sync::Arc;

use tuff_core::config::{Key, Value};
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::permission::Node;
use tuff_item::load::{LoadError, Phase};
use tuff_storage::admin::PermissionLevel;
use tuff_web_ui::Scope;
use tuff_web_ui::panel::{Asset, MetricCard, PanelApi, PanelPage, api_handler};

pub const ID: &str = "world-map";
/// The page, API and topic prefix id.
pub const PAGE: &str = "world-map";
pub const VIEW_NODE: &str = "map.view";
/// Seeing the map, its tiles and its markers.
pub const VIEW: Scope = Scope::Plugin(VIEW_NODE);
pub const GENERATE_NODE: &str = "map.generate";
/// Starting and cancelling chunk pre-generation from the map.
pub const GENERATE: Scope = Scope::Plugin(GENERATE_NODE);

pub struct WorldMapFeature;

impl Feature for WorldMapFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() != Phase::Content {
            return Ok(());
        }
        let registries = cx.registries();
        registries
            .services
            .push(Arc::new(service::MapServiceFactory));
        registries.config_keys.extend([
            Key::new(
                service::CACHE_KEY,
                Value::Int(128),
                "Memory the world map's tile cache may use, in MiB",
            ),
            Key::new(
                service::THREADS_KEY,
                Value::Int(0),
                "Threads rendering world map tiles; 0 uses half the CPUs (read at startup)",
            ),
            Key::new(
                service::DISK_KEY,
                Value::Int(128),
                "Storage the world map's persistent tile cache may use inside the world, in MiB; 0 turns it off and deletes it",
            ),
            Key::new(
                service::RATE_KEY,
                Value::Int(32),
                "World map tiles one panel session may fetch per second (bursts of four times that)",
            ),
        ]);
        registries.permissions.register(
            ID,
            Node::new(VIEW_NODE, "See the world map in the web panel")
                .op_level(PermissionLevel::Gamemasters),
        )?;
        registries.permissions.register(
            ID,
            Node::new(
                GENERATE_NODE,
                "Start and cancel chunk pre-generation from the world map",
            ),
        )?;
        let module = Asset::module(
            "map.js",
            include_str!(concat!(env!("OUT_DIR"), "/panel/map.js")),
        );
        let style = Asset::stylesheet(
            "map.css",
            include_str!(concat!(env!("OUT_DIR"), "/panel/map.css")),
        );
        registries.panel.register_page(
            ID,
            PanelPage::new(PAGE, "World map", VIEW, module)
                .section("World")
                .style(style)
                .icon("map"),
        )?;
        registries
            .panel
            .register_api(ID, PanelApi::new(PAGE, VIEW, api_handler(api::handle)))?;
        registries.panel.register_card(
            ID,
            MetricCard::new("map.tiles_rendered", "Map tiles rendered").scope(VIEW),
        )
    }
}
