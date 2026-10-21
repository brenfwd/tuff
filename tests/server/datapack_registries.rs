//! Real servers whose data packs add worldgen and synchronized registry
//! entries: a pack structure (in the biomes of a pack tag) placed by a
//! pack structure set is found by `/locate`, while a client joins with a
//! pack chat type synced.

use crate::common;

use std::path::Path;

use common::TempDir;
use tuff::Server;
use tuff_bot::{Bot, BotConfig};

fn write(pack: &Path, path: &str, text: &str) {
    let file = pack.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

fn pack(instance: &TempDir, files: &[(&str, &str)]) {
    let pack = instance.path().join("world/datapacks/registries");
    let v = tuff_vanilla_data::version::DATA_PACK_VERSION;
    write(
        &pack,
        "pack.mcmeta",
        &format!(
            r#"{{"pack":{{"description":"registries","min_format":[{0},{1}],"max_format":[{0},{1}]}}}}"#,
            v.major, v.minor
        ),
    );
    for (path, text) in files {
        write(&pack, path, text);
    }
}

fn server(instance: &TempDir) -> Server {
    Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.path().to_path_buf())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(2)
        .seed(1)
        .config_overrides(
            [
                ("tuff.world.dimensions.nether.enabled", "false"),
                ("tuff.world.dimensions.end.enabled", "false"),
            ]
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        )
        .build()
}

const FLAT_OVERWORLD: &str = r#"{"type":"minecraft:overworld","generator":{"type":"minecraft:flat","settings":{"biome":"minecraft:swamp","layers":[{"block":"minecraft:bedrock","height":1},{"block":"minecraft:stone","height":3},{"block":"minecraft:grass_block","height":1}],"lakes":false,"features":false,"structure_overrides":["ex:huts"]}}}"#;

const HUT: &str = r##"{"type":"minecraft:swamp_hut","biomes":"#ex:hut_biomes","step":"surface_structures","spawn_overrides":{}}"##;

const HUTS: &str = r#"{"structures":[{"structure":"ex:hut","weight":1}],"placement":{"type":"minecraft:random_spread","spacing":3,"separation":1,"salt":1234}}"#;

const HUT_BIOMES: &str = r#"{"values":["minecraft:swamp"]}"#;

const SHOUT: &str = r#"{"chat":{"translation_key":"%s!","parameters":["content"]},"narration":{"translation_key":"%s","parameters":["content"]}}"#;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pack_structure_set_places_a_pack_structure_that_locate_finds() {
    let instance = TempDir::new();
    pack(
        &instance,
        &[
            ("data/minecraft/dimension/overworld.json", FLAT_OVERWORLD),
            ("data/ex/worldgen/structure/hut.json", HUT),
            ("data/ex/worldgen/structure_set/huts.json", HUTS),
            ("data/ex/tags/worldgen/biome/hut_biomes.json", HUT_BIOMES),
            ("data/ex/chat_type/shout.json", SHOUT),
        ],
    );
    let server = server(&instance);
    let handle = server.handle();
    let task = tokio::spawn(server.run());
    let addr = handle.local_addrs().await.unwrap()[0];
    let bot = Bot::connect(
        addr,
        BotConfig {
            view_distance: 2,
            ..BotConfig::new("Explorer")
        },
    )
    .await
    .unwrap();
    bot.wait_for(|state| state.chunks > 0).await.unwrap();

    let feedback = handle
        .run_command("execute positioned 0 0 0 run locate structure ex:hut")
        .await
        .unwrap();
    let text: String = feedback.iter().map(|line| line.plain()).collect();
    assert!(text.contains("The nearest ex:hut is at"), "{text}");
    assert!(bot.state().disconnected.is_none());

    drop(bot);
    handle.shutdown().await.unwrap();
    task.await.unwrap().unwrap();
}
