//! Real servers whose data packs shape the dimensions: a pack world preset
//! adds a dimension with a pack dimension type that players spawn in, and
//! a pack dimension type override changes the overworld's bounds and what
//! clients are told.

use crate::common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use common::TempDir;
use tuff::Server;
use tuff::feature::{Feature, LoadContext};
use tuff::storage::Storage;
use tuff::storage::level::{LevelData, OVERWORLD};
use tuff::world::Shared;
use tuff::world::dimension::Dimensions;
use tuff::world::services::{ServiceFactory, WorldService};
use tuff_bot::{Bot, BotConfig};
use tuff_item::load::{LoadError, Phase};
use tuff_vanilla_data::data_registry::DIMENSION_TYPE;
use tuff_vanilla_data::packet::play::Clientbound as Id;
use tuff_wire::PacketReader;
use tuff_world_model::BlockPos;

const SKY: &str = "ex:sky";

type Slot = Arc<Mutex<Option<Arc<Shared>>>>;

#[derive(Clone, Default)]
struct Keep(Slot);

impl ServiceFactory for Keep {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(self.clone())
    }
}

impl WorldService for Keep {
    fn start(&self, shared: &Arc<Shared>) {
        *self.0.lock().unwrap() = Some(shared.clone());
    }
}

impl Feature for Keep {
    fn id(&self) -> &'static str {
        "keep-shared"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries().services.push(Arc::new(self.clone()));
        }
        Ok(())
    }
}

fn write(pack: &Path, path: &str, text: &str) {
    let file = pack.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

fn pack(instance: &TempDir, files: &[(&str, String)]) {
    let pack = instance.path().join("world/datapacks/dims");
    let v = tuff_vanilla_data::version::DATA_PACK_VERSION;
    write(
        &pack,
        "pack.mcmeta",
        &format!(
            r#"{{"pack":{{"description":"dims","min_format":[{0},{1}],"max_format":[{0},{1}]}}}}"#,
            v.major, v.minor
        ),
    );
    for (path, text) in files {
        write(&pack, path, text);
    }
}

fn dimension_type(min_y: i32, height: i32, ambient_light: f32) -> String {
    format!(
        r##"{{"ambient_light":{ambient_light},"coordinate_scale":1.0,"has_ceiling":false,"has_ender_dragon_fight":false,"has_skylight":true,"height":{height},"infiniburn":"#minecraft:infiniburn_overworld","logical_height":{height},"min_y":{min_y},"monster_spawn_block_light_limit":0,"monster_spawn_light_level":7}}"##
    )
}

fn flat_stem(kind: &str) -> String {
    format!(
        r#"{{"type":"{kind}","generator":{{"type":"minecraft:flat","settings":{{"biome":"minecraft:plains","layers":[{{"block":"minecraft:bedrock","height":1}},{{"block":"minecraft:stone","height":3}}],"lakes":false,"features":false,"structure_overrides":[]}}}}}}"#
    )
}

fn server(instance: &TempDir, keep: &Keep, settings: &[(&str, &str)]) -> Server {
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(keep.clone()));
    Server::builder()
        .features(features)
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .config_overrides(
            settings
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
        .build()
}

/// The dimension type id and dimension the Login packet names.
fn login_dimension(body: &bytes::Bytes) -> (i32, String) {
    let mut r = PacketReader::new(body.clone());
    r.read_int().unwrap();
    r.read_bool().unwrap();
    for _ in 0..r.read_var_int().unwrap() {
        r.read_string().unwrap();
    }
    for _ in 0..3 {
        r.read_var_int().unwrap();
    }
    for _ in 0..3 {
        r.read_bool().unwrap();
    }
    (r.read_var_int().unwrap(), r.read_string().unwrap())
}

async fn join(addr: std::net::SocketAddr, name: &str) -> Bot {
    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            keep: vec![Id::Login.id()],
            ..BotConfig::new(name)
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();
    bot
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pack_world_preset_adds_a_dimension_of_a_pack_type_players_spawn_in() {
    let instance = TempDir::new();
    pack(
        &instance,
        &[
            (
                "data/ex/dimension_type/tall.json",
                dimension_type(0, 128, 0.5),
            ),
            (
                "data/minecraft/worldgen/world_preset/normal.json",
                format!(r#"{{"dimensions":{{"{SKY}":{}}}}}"#, flat_stem("ex:tall")),
            ),
        ],
    );
    let keep = Keep::default();
    let server = server(&instance, &keep, &[]);
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let shared = keep.0.lock().unwrap().clone().expect("started");
    assert_eq!(shared.dimensions.keys(), [SKY]);
    let sky = shared.dimensions.by_key(SKY).unwrap();
    assert_eq!(sky.kind.name, "ex:tall");
    assert_eq!((sky.kind.min_y, sky.kind.height), (0, 128));
    assert_eq!(sky.kind.ambient_light, 0.5);
    assert_eq!((sky.shape.min_y, sky.shape.max_y()), (0, 128));
    assert_eq!(sky.type_id(), DIMENSION_TYPE.entries.len() as i32);
    assert_eq!(shared.spawn().dimension, SKY);
    assert_eq!(shared.spawn().pos, BlockPos::new(0, 4, 0));

    let bot = join(addr, "Skyborn").await;
    let login = bot.state().last.get(&Id::Login.id()).cloned().unwrap();
    assert_eq!(login_dimension(&login), (sky.type_id(), SKY.to_owned()));
    assert_eq!(bot.state().position[1], 4.0);
    let sky_id = sky.id;
    let mut joined = shared.players.clone();
    joined
        .wait_for(|index| index.in_dimension(sky_id) == 1)
        .await
        .unwrap();
    assert!(bot.state().disconnected.is_none());

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    let backend = Storage::default()
        .open(&instance.path().join("world"))
        .unwrap();
    let level = LevelData::load(backend.documents(), || 0, |_| BlockPos::new(0, 0, 0)).unwrap();
    let keys: Vec<&str> = level.dimensions.iter().map(|d| d.key.as_str()).collect();
    assert_eq!(keys, [SKY], "the preset's dimensions are saved");
    assert_eq!(level.spawn.dimension, SKY);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pack_dimension_type_override_changes_the_overworld_bounds() {
    let instance = TempDir::new();
    pack(
        &instance,
        &[
            (
                "data/minecraft/dimension_type/overworld.json",
                dimension_type(0, 256, 0.0),
            ),
            (
                "data/minecraft/dimension/overworld.json",
                flat_stem(OVERWORLD),
            ),
        ],
    );
    let keep = Keep::default();
    let server = server(
        &instance,
        &keep,
        &[
            ("tuff.world.dimensions.nether.enabled", "false"),
            ("tuff.world.dimensions.end.enabled", "false"),
        ],
    );
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let shared = keep.0.lock().unwrap().clone().expect("started");
    let overworld = shared.dimensions.by_key(OVERWORLD).unwrap();
    assert_eq!((overworld.shape.min_y, overworld.shape.max_y()), (0, 256));
    assert!(overworld.is_flat(), "the pack stem replaces the generator");
    let frozen = shared.datapacks.contents().registries.clone();
    let types = frozen.get(DIMENSION_TYPE.key).unwrap();
    assert_eq!(
        overworld.type_id(),
        types.index_of(OVERWORLD).unwrap() as i32,
    );
    let nbt = types.get(OVERWORLD).unwrap().network.get().expect("synced");
    assert!(nbt.windows(6).any(|w| w == b"height"));
    assert_eq!(shared.spawn().pos, BlockPos::new(0, 4, 0));

    let bot = join(addr, "Lowlander").await;
    let login = bot.state().last.get(&Id::Login.id()).cloned().unwrap();
    assert_eq!(
        login_dimension(&login),
        (overworld.type_id(), OVERWORLD.to_owned())
    );
    assert_eq!(bot.state().position[1], 4.0);

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
    let backend = Storage::default()
        .open(&instance.path().join("world"))
        .unwrap();
    let level = LevelData::load(backend.documents(), || 0, |_| BlockPos::new(0, 0, 0)).unwrap();
    let overworld = level.dimension(OVERWORLD).unwrap();
    assert_eq!(
        overworld.generator, "minecraft:noise",
        "pack stems are not saved"
    );
}
