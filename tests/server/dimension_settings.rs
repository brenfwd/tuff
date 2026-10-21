//! Real servers with vanilla dimensions switched off: a player joins a
//! world whose only dimension is a plugin's, borders are saved under their
//! own dimension, and a world left with no dimension refuses to start.

use crate::common;

use std::sync::{Arc, Mutex};

use common::TempDir;
use tuff::Server;
use tuff::feature::{Feature, LoadContext};
use tuff::storage::Storage;
use tuff::storage::level::{LevelData, OVERWORLD, THE_END, THE_NETHER};
use tuff::world::Shared;
use tuff::world::custom_dimension::{DimensionDefinition, DimensionProvider};
use tuff::world::dimension::Dimensions;
use tuff::world::generator::ChunkGenerator;
use tuff::world::services::{ServiceFactory, WorldService};
use tuff_bot::{Bot, BotConfig};
use tuff_item::load::{LoadError, Phase};
use tuff_world_model::BlockPos;
use tuff_world_model::biomes::Biomes;
use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::pos::ChunkPos;

const LOBBY: &str = "test:lobby";
const FLOOR: i32 = 40;

type Slot = Arc<Mutex<Option<Arc<Shared>>>>;

struct Floor;

impl ChunkGenerator for Floor {
    fn generate(&self, _pos: ChunkPos, shape: ChunkShape) -> Chunk {
        let mut chunk = Chunk::empty(shape);
        let stone = tuff_vanilla_data::Block::Stone.default_state();
        for x in 0..16 {
            for z in 0..16 {
                chunk.set(x, FLOOR, z, stone);
            }
        }
        chunk
    }

    fn sea_level(&self) -> i32 {
        0
    }

    fn spawn(&self) -> BlockPos {
        BlockPos::new(0, FLOOR + 1, 0)
    }
}

/// Adds [`LOBBY`] (claiming the spawn when `claims` says so) and keeps the
/// running world's [`Shared`].
#[derive(Clone)]
struct Lobby {
    claims: bool,
    shared: Slot,
}

impl DimensionProvider for Lobby {
    fn dimensions(&self) -> Vec<DimensionDefinition> {
        vec![DimensionDefinition {
            key: LOBBY.into(),
            dimension_type: OVERWORLD.into(),
            type_json: String::new(),
            biomes: Vec::new(),
            natural_spawning: false,
            claims_spawn: self.claims,
        }]
    }

    fn generator(&self, _: &str, _: i64, _: &Arc<Biomes>) -> Arc<dyn ChunkGenerator> {
        Arc::new(Floor)
    }
}

impl ServiceFactory for Lobby {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(self.clone())
    }
}

impl WorldService for Lobby {
    fn start(&self, shared: &Arc<Shared>) {
        *self.shared.lock().unwrap() = Some(shared.clone());
    }
}

impl Feature for Lobby {
    fn id(&self) -> &'static str {
        "test-lobby"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries()
                .dimension_providers
                .push(Arc::new(self.clone()));
            cx.registries().services.push(Arc::new(self.clone()));
        }
        Ok(())
    }
}

fn overrides(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn server(instance: &TempDir, lobby: Option<Lobby>, settings: &[(&str, &str)]) -> Server {
    let mut features = tuff::vanilla_features::all();
    if let Some(lobby) = lobby {
        features.push(Box::new(lobby));
    }
    Server::builder()
        .features(features)
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .config_overrides(overrides(settings))
        .build()
}

fn saved_level(instance: &TempDir) -> LevelData {
    let backend = Storage::default()
        .open(&instance.path().join("world"))
        .unwrap();
    LevelData::load(backend.documents(), || 0, |_| BlockPos::new(0, 0, 0)).unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_joins_a_world_whose_only_dimension_is_a_plugins() {
    let instance = TempDir::new();
    let lobby = Lobby {
        claims: true,
        shared: Slot::default(),
    };
    let server = server(
        &instance,
        Some(lobby.clone()),
        &[
            ("tuff.world.dimensions.overworld.enabled", "false"),
            ("tuff.world.dimensions.nether.enabled", "false"),
            ("tuff.world.dimensions.end.enabled", "false"),
        ],
    );
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let shared = lobby.shared.lock().unwrap().clone().expect("started");
    assert_eq!(shared.dimensions.keys(), [LOBBY]);
    assert_eq!(shared.spawn().dimension, LOBBY);

    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new("Lobbyist")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    assert_eq!(bot.state().position[1], f64::from(FLOOR + 1));
    let lobby_id = shared.dimensions.by_key(LOBBY).unwrap().id;
    let mut joined = shared.players.clone();
    joined
        .wait_for(|index| index.in_dimension(lobby_id) == 1)
        .await
        .unwrap();
    assert!(bot.state().disconnected.is_none());

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    let level = saved_level(&instance);
    let keys: Vec<&str> = level.dimensions.iter().map(|d| d.key.as_str()).collect();
    assert_eq!(keys, [OVERWORLD, THE_END, THE_NETHER], "their data is kept");
    assert_eq!(level.spawn.dimension, LOBBY);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn borders_are_saved_under_their_own_dimension() {
    let instance = TempDir::new();
    let lobby = Lobby {
        claims: false,
        shared: Slot::default(),
    };
    let server = server(
        &instance,
        Some(lobby.clone()),
        &[
            ("tuff.world.dimensions.overworld.enabled", "false"),
            ("tuff.world.spawnDimension", LOBBY),
        ],
    );
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    handle.local_addrs().await.unwrap();
    let shared = lobby.shared.lock().unwrap().clone().expect("started");
    assert_eq!(shared.dimensions.keys(), [THE_END, THE_NETHER, LOBBY]);
    assert_eq!(shared.spawn().dimension, LOBBY);
    handle
        .run_command("execute in minecraft:the_nether run worldborder set 100")
        .await
        .unwrap();
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();

    let level = saved_level(&instance);
    let size = |key: &str| level.dimension(key).unwrap().border.size;
    assert_eq!(size(THE_NETHER), 100.0);
    assert_ne!(size(THE_END), 100.0);
    assert_ne!(size(OVERWORLD), 100.0);
    assert_eq!(level.spawn.dimension, LOBBY);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_world_with_no_dimension_refuses_to_start() {
    let instance = TempDir::new();
    let server = server(
        &instance,
        None,
        &[
            ("tuff.world.dimensions.overworld.enabled", "false"),
            ("tuff.world.dimensions.nether.enabled", "false"),
            ("tuff.world.dimensions.end.enabled", "false"),
        ],
    );
    let err = server.run().await.expect_err("no dimension, no server");
    let text = format!("{err:#}");
    assert!(text.contains("the world has no dimensions"), "{text}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugin_dimension_borders_survive_a_restart() {
    let instance = TempDir::new();
    let run = |lobby: Lobby| {
        let server = server(&instance, Some(lobby), &[]);
        let handle = server.handle();
        (handle, tokio::spawn(server.run()))
    };
    let first = Lobby {
        claims: false,
        shared: Slot::default(),
    };
    let (handle, task) = run(first.clone());
    handle.local_addrs().await.unwrap();
    handle
        .run_command(format!("execute in {LOBBY} run worldborder set 80"))
        .await
        .unwrap();
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    assert!(
        saved_level(&instance).dimension(LOBBY).is_none(),
        "the plugin dimension stays out of the level data"
    );

    let second = Lobby {
        claims: false,
        shared: Slot::default(),
    };
    let (handle, task) = run(second.clone());
    handle.local_addrs().await.unwrap();
    let shared = second.shared.lock().unwrap().clone().expect("started");
    let lobby = shared.dimensions.by_key(LOBBY).unwrap();
    assert_eq!(lobby.border().size(), 80.0);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}
