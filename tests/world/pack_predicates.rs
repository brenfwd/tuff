//! Data pack content commands read by name: stored predicates in
//! `predicate=` selector options and `execute if|unless predicate`, and
//! pack recipes in `/recipe`.

use super::*;
use crate::auth::GameProfile;
use crate::protocol::configuration::ClientInformation;
use crate::world::messages::{PlayerSnapshot, PlayerState};
use crate::world::session::PlayerSession;
use tuff_storage::admin::PermissionLevel;

fn write(root: &Path, path: &str, contents: &[u8]) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// A folder pack `name` holding `files` (paths below `data/`).
pub(super) fn pack(root: &Path, name: &str, files: &[(&str, &str)]) {
    let current = tuff_vanilla_data::version::DATA_PACK_VERSION.major;
    let meta = format!(
        r#"{{"pack":{{"description":"test pack","min_format":{current},"max_format":{current}}}}}"#
    );
    write(
        root,
        &format!("datapacks/{name}/pack.mcmeta"),
        meta.as_bytes(),
    );
    for (path, contents) in files {
        write(
            root,
            &format!("datapacks/{name}/data/{path}"),
            contents.as_bytes(),
        );
    }
}

pub(super) async fn console(world: &TestWorld, line: &str) -> Vec<String> {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(TextComponent::plain)
        .collect()
}

const PREDICATES: &[(&str, &str)] = &[
    (
        "a/predicate/tagged.json",
        r#"{"type":"minecraft:entity_properties","entity":"this",
            "predicate":{"minecraft:entity_tags":{"any_of":["probe"]}}}"#,
    ),
    (
        "a/predicate/on_glass.json",
        r#"{"type":"minecraft:location_check","offsetY":-1,
            "predicate":{"block":{"blocks":"minecraft:glass"}}}"#,
    ),
    (
        "a/predicate/tagged_pig.json",
        r#"["a:tagged",{"type":"entity_properties","entity":"this",
            "predicate":{"minecraft:entity_type":"minecraft:pig"}}]"#,
    ),
    (
        "a/predicate/rich.json",
        r#"{"type":"minecraft:entity_scores","entity":"this","scores":{"coins":{"min":5}}}"#,
    ),
    (
        "a/predicate/still.json",
        r#"{"type":"minecraft:entity_properties","entity":"this",
            "predicate":{"minecraft:nbt":"{NoAI:1b}"}}"#,
    ),
    ("a/predicate/broken.json", r#"{"type":"minecraft:nope"}"#),
];

async fn count(world: &TestWorld, selector: &str) -> String {
    let near = "x=5,y=310,z=5,distance=..10";
    let lines = console(world, &format!("execute if entity @e[{near},{selector}]")).await;
    lines.join("\n")
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn selectors_and_execute_test_stored_and_inline_predicates() {
    let dir = TestDir::new();
    pack(dir.path(), "preds", PREDICATES);
    let world = TestWorld::start(dir.path());
    console(&world, "forceload add 0 0 20 20").await;
    console(&world, "setblock 5 309 5 glass").await;
    console(&world, "setblock 7 309 7 stone").await;
    for kind in ["pig 5.5 310 5.5", "cow 7.5 310 7.5"] {
        console(
            &world,
            &format!(
                r#"summon {kind} {{NoAI:1b,NoGravity:1b,PersistenceRequired:1b,Tags:["probe"]}}"#
            ),
        )
        .await;
    }
    console(&world, "scoreboard objectives add coins dummy").await;
    console(
        &world,
        "scoreboard players set @e[x=5,y=310,z=5,distance=..10,type=cow] coins 9",
    )
    .await;

    let passed = |count: usize| format!("Test passed. Count: {count}");
    assert_eq!(count(&world, "predicate=a:tagged").await, passed(2));
    assert_eq!(
        count(&world, "predicate=a:on_glass,type=pig").await,
        passed(1)
    );
    assert_eq!(count(&world, "predicate=!a:on_glass").await, passed(1));
    assert_eq!(count(&world, "predicate=a:tagged_pig").await, passed(1));
    assert_eq!(count(&world, "predicate=a:rich,type=cow").await, passed(1));
    assert_eq!(count(&world, "predicate=a:still").await, passed(2));
    for missing in ["a:broken", "!a:broken", "a:missing", "!a:missing"] {
        assert_eq!(
            count(&world, &format!("predicate={missing}")).await,
            "Test failed",
            "{missing} is not a predicate"
        );
    }

    let test = |line: &'static str| console(&world, line);
    assert_eq!(
        test("execute positioned 5.5 310 5.5 if predicate a:on_glass").await,
        ["Test passed"]
    );
    assert_eq!(
        test("execute positioned 7.5 310 7.5 if predicate a:on_glass").await,
        ["Test failed"]
    );
    assert_eq!(
        test("execute positioned 7.5 310 7.5 unless predicate a:on_glass").await,
        ["Test passed"]
    );
    assert_eq!(
        test("execute as @e[x=5,y=310,z=5,distance=..10,type=cow] if predicate a:rich").await,
        ["Test passed"],
        "the entity is found wherever the source stands"
    );
    assert_eq!(test("execute if predicate a:rich").await, ["Test failed"]);
    assert_eq!(
        test(r#"execute if predicate {type:"minecraft:random_chance",chance:1.0f}"#).await,
        ["Test passed"]
    );
    assert_eq!(
        test(r#"execute if predicate [{type:"random_chance",chance:0.0f}]"#).await,
        ["Test failed"]
    );
    assert_eq!(
        test(r#"execute positioned 5.5 310 5.5 if predicate {type:"inverted",term:"a:on_glass"}"#)
            .await,
        ["Test failed"]
    );
    assert_eq!(
        test(r#"execute if predicate {type:"minecraft:environment_attribute_check",attribute:"gameplay/can_start_raid",value:true}"#).await,
        ["Test passed"]
    );
    assert_eq!(
        test(r#"execute if predicate {type:"minecraft:environment_attribute_check",attribute:"gameplay/can_start_raid",value:false}"#).await,
        ["Test failed"]
    );
    let sky = tuff_vanilla_data::biome::Biome::get("minecraft:plains")
        .and_then(|biome| biome.attribute("minecraft:visual/sky_color"))
        .and_then(|value| value.as_str().map(str::to_owned))
        .expect("plains defines a sky color");
    let command = format!(
        r#"execute positioned 5.5 310 5.5 if predicate {{type:"minecraft:environment_attribute_check",attribute:"visual/sky_color",value:"{sky}"}}"#
    );
    assert_eq!(
        console(&world, &command).await,
        ["Test passed"],
        "a biome-defined attribute is read at the position"
    );
    let unknown = test("execute if predicate a:missing").await.join("\n");
    assert!(unknown.contains("a:missing"), "{unknown}");
    let invalid = test(r#"execute if predicate {type:"minecraft:nope"}"#)
        .await
        .join("\n");
    assert!(invalid.contains("minecraft:nope"), "{invalid}");

    let loaded = Arc::downgrade(&world.shared.datapacks.contents().predicates);
    console(&world, r#"datapack disable "file/preds""#).await;
    assert!(
        loaded.upgrade().is_none(),
        "a reload frees the old predicates"
    );
    let contents = world.shared.datapacks.contents();
    assert!(!contents.predicates.contains("a:tagged"));
    assert!(contents.predicates.contains("minecraft:tool/can_shear"));
    drop(contents);
    assert_eq!(count(&world, "predicate=a:tagged").await, "Test failed");
    world.shutdown().await;
}

struct Solo {
    session: PlayerSession,
    states: mpsc::UnboundedReceiver<()>,
    _frames: mpsc::UnboundedReceiver<bytes::Bytes>,
}

impl Solo {
    async fn run(&mut self, line: &str) {
        self.session.run_command(line).await;
        self.session.report_state().await;
        while self.states.try_recv().is_ok() {}
    }

    async fn score(&self, objective: &str) -> Option<i32> {
        let objective = objective.to_owned();
        self.session
            .shared()
            .scoreboard
            .read(move |board| board.score("Steve", &objective).map(|s| s.value))
            .await
            .flatten()
    }
}

/// One player, Steve, on a world with the packs under `dir` and a
/// coordinator that only knows him.
async fn solo(dir: &Path) -> Solo {
    let (coordinator, mut requests) = mpsc::channel(64);
    let mut shared = bare_shared();
    shared.coordinator = coordinator;
    shared.backend = Storage::default().open(dir).unwrap();
    shared.scoreboard =
        crate::world::scoreboard::start(shared.backend.clone(), shared.clock.clone()).0;
    shared.datapacks = tuff_core::datapack::DataPacks::open_now(
        shared.backend.clone(),
        shared.commands.clone(),
        tuff_core::datapack::Base {
            recipes: shared.recipe_set.clone(),
            data_recipes: vanilla_registries().data_recipes.get().cloned(),
            ..Default::default()
        },
        tuff_core::datapack::DataPackConfig::default(),
        None,
    );
    let shared = Arc::new(shared);
    let (outbound, frames) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(1000);
    let (_packets, packets) = mpsc::channel(8);
    let session = PlayerSession::new(
        shared.clone(),
        1000,
        GameProfile {
            id: 1000,
            name: "Steve".to_owned(),
            properties: Vec::new(),
        },
        ClientInformation::default(),
        outbound,
        handle.clone(),
        events,
        packets,
        PermissionLevel::Owners,
    );
    let (states, received) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut known: Option<PlayerState> = None;
        while let Some(message) = requests.recv().await {
            match message {
                CoordinatorMessage::PlayerState { state, .. } => {
                    known = Some(state);
                    let _ = states.send(());
                }
                CoordinatorMessage::Players { reply } => {
                    let snapshots = known.iter().map(|state| PlayerSnapshot {
                        id: 1000,
                        uuid: 1000,
                        name: "Steve".to_owned(),
                        handle: handle.clone(),
                        address: std::net::Ipv4Addr::LOCALHOST.into(),
                        permission: PermissionLevel::Owners,
                        state: state.clone(),
                        fake: false,
                    });
                    let _ = reply.send(snapshots.collect());
                }
                _ => {}
            }
        }
    });
    let mut solo = Solo {
        session,
        states: received,
        _frames: frames,
    };
    solo.session.report_state().await;
    solo.states.recv().await;
    solo
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn player_predicates_read_mode_scores_and_flags() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "preds",
        &[
            (
                "a/predicate/creative.json",
                r#"{"type":"minecraft:entity_properties","entity":"this",
                    "predicate":{"minecraft:type_specific/player":{"gamemode":["creative"]}}}"#,
            ),
            (
                "a/predicate/rich.json",
                r#"{"type":"minecraft:entity_scores","entity":"this","scores":{"coins":{"min":5}}}"#,
            ),
            (
                "a/predicate/sneaking.json",
                r#"{"type":"minecraft:entity_properties","entity":"this",
                    "predicate":{"minecraft:flags":{"is_sneaking":true}}}"#,
            ),
            (
                "a/predicate/odd.json",
                r#"{"type":"minecraft:int_value_check",
                    "value":{"type":"minecraft:score","target":"this","score":"coins"},
                    "test":{"min":7,"max":7}}"#,
            ),
        ],
    );
    let mut steve = solo(dir.path()).await;
    steve.run("scoreboard objectives add hits dummy").await;
    steve.run("scoreboard objectives add coins dummy").await;
    steve
        .run("execute if predicate a:creative run scoreboard players set Steve hits 1")
        .await;
    assert_eq!(steve.score("hits").await, None, "Steve starts in survival");
    steve.run("gamemode creative").await;
    steve
        .run("execute if predicate a:creative run scoreboard players set Steve hits 1")
        .await;
    assert_eq!(steve.score("hits").await, Some(1));

    steve.run("scoreboard players set Steve coins 7").await;
    steve
        .run("execute if entity @s[predicate=a:rich] run scoreboard players set Steve hits 2")
        .await;
    assert_eq!(steve.score("hits").await, Some(2));
    steve
        .run("execute if predicate a:odd run scoreboard players set Steve hits 3")
        .await;
    assert_eq!(steve.score("hits").await, Some(3));
    steve
        .run("execute if entity @a[predicate=a:sneaking] run scoreboard players set Steve hits 4")
        .await;
    assert_eq!(steve.score("hits").await, Some(3), "Steve is not sneaking");
    steve
        .run("execute if entity @a[predicate=!a:sneaking] run scoreboard players set Steve hits 5")
        .await;
    assert_eq!(steve.score("hits").await, Some(5));
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recipe_give_and_take_reach_pack_recipes() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "content",
        &[(
            "a/recipe/x.json",
            r#"{"type":"minecraft:crafting_shapeless","ingredients":["minecraft:emerald"],
                "result":{"id":"minecraft:dirt","count":4}}"#,
        )],
    );
    let mut steve = solo(dir.path()).await;
    assert!(steve.session.shared().recipes().get("a:x").is_some());
    steve.run("recipe give @s a:x").await;
    assert!(steve.session.knows_recipe("a:x"));
    steve.run("recipe take @s a:x").await;
    assert!(!steve.session.knows_recipe("a:x"));
    steve.run("recipe give @s a:missing").await;
    assert!(!steve.session.knows_recipe("a:missing"));
    steve.run("recipe give @s minecraft:piston").await;
    assert!(steve.session.knows_recipe("minecraft:piston"));
}

const SUB_PREDICATES: &[(&str, &str)] = &[
    (
        "b/predicate/sheared.json",
        r#"{"type":"entity_properties","entity":"this",
            "predicate":{"type_specific/sheep":{"sheared":true}}}"#,
    ),
    (
        "b/predicate/big_slime.json",
        r#"{"type":"entity_properties","entity":"this",
            "predicate":{"type_specific/cube_mob":{"size":{"min":3}}}}"#,
    ),
    (
        "b/predicate/carries_chicken.json",
        r#"{"type":"entity_properties","entity":"this",
            "predicate":{"passenger":{"type":"chicken"}}}"#,
    ),
    (
        "b/predicate/rides_pig.json",
        r#"{"type":"entity_properties","entity":"this",
            "predicate":{"vehicle":{"type":"pig"},"periodic_tick":1}}"#,
    ),
    (
        "b/predicate/stored_four.json",
        r#"{"type":"int_value_check","value":{"type":"add","inputs":[
            {"type":"storage","storage":"b:s","path":"n"},1]},"test":5}"#,
    ),
];

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn entity_sub_predicates_read_riders_type_data_and_storage() {
    let dir = TestDir::new();
    pack(dir.path(), "subs", SUB_PREDICATES);
    let world = TestWorld::start(dir.path());
    console(&world, "forceload add 0 0 20 20").await;
    console(&world, "setblock 1 300 1 stone").await;
    let base = "NoAI:1b,NoGravity:1b,PersistenceRequired:1b";
    for (kind, extra) in [
        ("sheep 5.5 310 5.5", "Sheared:1b"),
        ("sheep 6.5 310 5.5", "Sheared:0b"),
        ("slime 7.5 310 5.5", "Size:2"),
        ("slime 8.5 310 5.5", "Size:0"),
        (
            "pig 9.5 310 5.5",
            "Passengers:[{id:\"minecraft:chicken\",NoAI:1b,PersistenceRequired:1b}]",
        ),
    ] {
        console(&world, &format!("summon {kind} {{{base},{extra}}}")).await;
    }
    let passed = |count: usize| format!("Test passed. Count: {count}");
    assert_eq!(count(&world, "predicate=b:sheared").await, passed(1));
    assert_eq!(count(&world, "predicate=b:big_slime").await, passed(1));
    assert_eq!(
        count(&world, "predicate=b:carries_chicken").await,
        passed(1)
    );
    assert_eq!(
        count(&world, "predicate=b:rides_pig,type=chicken").await,
        passed(1)
    );
    assert_eq!(
        console(&world, "execute if predicate b:stored_four")
            .await
            .join(""),
        "Test failed"
    );
    console(&world, "data modify storage b:s n set value 4").await;
    assert_eq!(
        console(&world, "execute if predicate b:stored_four")
            .await
            .join(""),
        "Test passed"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn villager_variant_predicates_read_villager_data() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "variants",
        &[(
            "c/predicate/desert.json",
            r#"{"type":"entity_properties","entity":"this",
                "predicate":{"predicates":{"villager/variant":"desert"}}}"#,
        )],
    );
    let world = TestWorld::start(dir.path());
    console(&world, "forceload add 0 0 20 20").await;
    console(&world, "setblock 1 300 1 stone").await;
    for (x, kind) in [(5.5, "desert"), (6.5, "plains")] {
        console(
            &world,
            &format!(
                "summon villager {x} 310 5.5 {{NoAI:1b,NoGravity:1b,PersistenceRequired:1b,\
                 VillagerData:{{type:\"minecraft:{kind}\",profession:\"minecraft:none\",level:1}}}}"
            ),
        )
        .await;
    }
    assert_eq!(
        count(&world, "predicate=c:desert").await,
        "Test passed. Count: 1"
    );
}
