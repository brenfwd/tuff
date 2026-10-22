//! Data packs in a running world: functions run through `/function`, the
//! load and tick tags, macros, `return`, `execute if function`, and
//! `/datapack` and `/reload` changing what is loaded.

use super::*;

fn write(root: &Path, path: &str, contents: &[u8]) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn mcmeta() -> String {
    let current = tuff_vanilla_data::version::DATA_PACK_VERSION.major;
    format!(
        r#"{{"pack":{{"description":"test pack","min_format":{current},"max_format":{current}}}}}"#
    )
}

/// A folder pack `name` holding `files` (paths below `data/`).
fn pack(root: &Path, name: &str, files: &[(&str, &str)]) {
    write(
        root,
        &format!("datapacks/{name}/pack.mcmeta"),
        mcmeta().as_bytes(),
    );
    for (path, contents) in files {
        write(
            root,
            &format!("datapacks/{name}/data/{path}"),
            contents.as_bytes(),
        );
    }
}

/// An enchantment definition for swords with `effects`.
fn enchantment_json(effects: &str) -> String {
    format!(
        r#"{{"description":"x","supported_items":"minecraft:diamond_sword","weight":1,"max_level":5,"min_cost":{{"base":1,"per_level_above_first":1}},"max_cost":{{"base":5,"per_level_above_first":1}},"anvil_cost":1,"slots":["mainhand"],"effects":{effects}}}"#
    )
}

async fn console(world: &TestWorld, line: &str) -> Vec<String> {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(TextComponent::plain)
        .collect()
}

async fn score(world: &TestWorld, holder: &str, objective: &str) -> Option<i32> {
    let (holder, objective) = (holder.to_owned(), objective.to_owned());
    world
        .shared
        .scoreboard
        .read(move |board| board.score(&holder, &objective).map(|s| s.value))
        .await
        .flatten()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_function_runs_its_commands_in_order() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "counter",
        &[(
            "a/function/count.mcfunction",
            "# counts to three\nscoreboard players add x obj 1\nscoreboard players add x obj 1\n\nscoreboard players \\\n  add x obj 1\n",
        )],
    );
    let world = TestWorld::start(dir.path());
    console(&world, "scoreboard objectives add obj dummy").await;
    let lines = console(&world, "function a:count").await;
    assert_eq!(lines, ["Running function a:count"]);
    assert_eq!(score(&world, "x", "obj").await, Some(3));
    assert_eq!(
        console(&world, "function a:missing").await,
        ["Unknown function a:missing"]
    );
    assert_eq!(
        console(&world, "function #a:missing").await,
        ["Unknown function tag 'a:missing'"]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn return_ends_a_call_with_its_value() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "ret",
        &[
            (
                "a/function/seven.mcfunction",
                "scoreboard players set x obj 1\nreturn 7\nscoreboard players set x obj 2",
            ),
            (
                "a/function/fails.mcfunction",
                "return fail\nscoreboard players set x obj 3",
            ),
            (
                "a/function/passes.mcfunction",
                "return run function a:seven",
            ),
            (
                "a/function/fork.mcfunction",
                "execute if score x obj matches 1 run return 4\nscoreboard players set x obj 5",
            ),
            (
                "a/function/silent.mcfunction",
                "scoreboard players set z obj 9",
            ),
            (
                "a/function/stores_silent.mcfunction",
                "scoreboard players set q obj 11\nexecute store result score q obj run function a:silent",
            ),
        ],
    );
    let world = TestWorld::start(dir.path());
    console(&world, "scoreboard objectives add obj dummy").await;
    let lines = console(&world, "function a:seven").await;
    assert_eq!(
        lines,
        ["Running function a:seven", "Function a:seven returned 7"]
    );
    assert_eq!(score(&world, "x", "obj").await, Some(1));
    console(
        &world,
        "execute store result score y obj run function a:seven",
    )
    .await;
    assert_eq!(score(&world, "y", "obj").await, Some(7));
    console(
        &world,
        "execute store success score s obj run function a:fails",
    )
    .await;
    assert_eq!(score(&world, "s", "obj").await, Some(0));
    assert_eq!(score(&world, "x", "obj").await, Some(1));
    console(
        &world,
        "execute store result score p obj run function a:passes",
    )
    .await;
    assert_eq!(score(&world, "p", "obj").await, Some(7));
    console(
        &world,
        "execute store result score f obj run function a:fork",
    )
    .await;
    assert_eq!(score(&world, "f", "obj").await, Some(4));
    assert_eq!(score(&world, "x", "obj").await, Some(1));
    console(&world, "scoreboard players set n obj 3").await;
    console(
        &world,
        "execute store result score n obj run function a:silent",
    )
    .await;
    assert_eq!(score(&world, "z", "obj").await, Some(9));
    assert_eq!(score(&world, "n", "obj").await, Some(3), "nothing returned");
    console(&world, "function a:stores_silent").await;
    assert_eq!(score(&world, "q", "obj").await, Some(11));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn macros_fill_in_arguments_from_nbt_and_storage() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "macros",
        &[(
            "a/function/set.mcfunction",
            "$scoreboard players set $(who) obj $(n)",
        )],
    );
    let world = TestWorld::start(dir.path());
    console(&world, "scoreboard objectives add obj dummy").await;
    console(&world, r#"function a:set {who:"z",n:5}"#).await;
    assert_eq!(score(&world, "z", "obj").await, Some(5));
    console(
        &world,
        r#"data modify storage t:s args set value {who:"w",n:9b}"#,
    )
    .await;
    console(&world, "function a:set with storage t:s args").await;
    assert_eq!(score(&world, "w", "obj").await, Some(9));
    console(&world, r#"data modify storage t:w who set value "v""#).await;
    console(&world, "data modify storage t:w n set value 2").await;
    console(&world, "function a:set with storage t:w").await;
    assert_eq!(score(&world, "v", "obj").await, Some(2));
    assert_eq!(
        console(&world, r#"function a:set {who:"q"}"#).await,
        ["Failed to instantiate function a:set: Missing argument n to function a:set"]
    );
    assert_eq!(
        console(&world, "function a:set").await,
        ["Failed to instantiate function a:set: Missing arguments to function a:set"]
    );
    assert_eq!(
        console(&world, "function a:set with storage t:s args.who").await,
        ["Invalid argument type: STRING. Expected Compound"]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tags_run_their_functions_and_conditions_test_return_values() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "cond",
        &[
            (
                "a/function/one.mcfunction",
                "scoreboard players add ran obj 1\nreturn 1",
            ),
            ("a/function/zero.mcfunction", "return 0"),
            (
                "a/function/silent.mcfunction",
                "scoreboard players add ran obj 10",
            ),
            (
                "a/tags/function/all.json",
                r#"{"values":["a:silent","a:one","a:zero"]}"#,
            ),
        ],
    );
    let world = TestWorld::start(dir.path());
    console(&world, "scoreboard objectives add obj dummy").await;
    let lines = console(&world, "function #a:all").await;
    assert_eq!(lines[0], "Running functions a:silent, a:one, a:zero",);
    assert_eq!(score(&world, "ran", "obj").await, Some(11));
    for (condition, expected) in [
        ("if function a:one", true),
        ("unless function a:one", false),
        ("if function a:zero", false),
        ("unless function a:zero", true),
        ("if function a:silent", false),
        ("unless function a:silent", false),
        ("if function #a:all", true),
    ] {
        console(&world, "scoreboard players set hit obj 0").await;
        console(
            &world,
            &format!("execute {condition} run scoreboard players set hit obj 1"),
        )
        .await;
        assert_eq!(
            score(&world, "hit", "obj").await,
            Some(i32::from(expected)),
            "{condition}"
        );
    }
    world.shutdown().await;
}

const LOOPS: &[(&str, &str)] = &[
    (
        "a/function/forever.mcfunction",
        "scoreboard players add n obj 1\nfunction a:forever",
    ),
    (
        "a/function/deep.mcfunction",
        "scoreboard players add d obj 1\nexecute if score d obj matches ..100000 run function a:deep",
    ),
    (
        "a/function/tail.mcfunction",
        "scoreboard players add t obj 1\nexecute if score t obj matches ..5000 run return run function a:tail\nreturn 99",
    ),
    (
        "a/function/stored.mcfunction",
        "scoreboard players add s obj 1\nexecute if score s obj matches ..100000 store result score tmp obj run function a:stored",
    ),
    (
        "a/function/stored_to.mcfunction",
        "scoreboard players add s obj 1\n\
         execute if score s obj < max obj run scoreboard players add tried obj 1\n\
         scoreboard players set tmp obj 0\n\
         execute if score s obj < max obj store result score tmp obj run function a:stored_to\n\
         execute if score tmp obj matches 7 run scoreboard players add back obj 1\n\
         scoreboard players set tmp obj 0\n\
         return 7",
    ),
    (
        "a/function/cond_to.mcfunction",
        "scoreboard players add s obj 1\n\
         execute if score s obj < max obj run scoreboard players add tried obj 1\n\
         execute if score s obj < max obj if function a:cond_to run scoreboard players add back obj 1\n\
         return 7",
    ),
];

async fn loops_world(dir: &TestDir, budget: i32) -> TestWorld {
    pack(dir.path(), "loops", LOOPS);
    let world = TestWorld::start(dir.path());
    console(&world, "scoreboard objectives add obj dummy").await;
    world.game_rules.send_modify(|rules| {
        rules
            .set(
                "max_command_sequence_length",
                tuff_core::world::game_rules::RuleValue::Int(budget),
            )
            .unwrap();
    });
    world
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn recursion_stops_when_the_command_budget_is_spent() {
    let dir = TestDir::new();
    let world = loops_world(&dir, 100).await;
    console(&world, "function a:forever").await;
    assert_eq!(score(&world, "n", "obj").await, Some(33));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_recursion_runs_on_the_call_stack() {
    let dir = TestDir::new();
    let world = loops_world(&dir, 65536).await;
    console(&world, "function a:deep").await;
    assert_eq!(score(&world, "d", "obj").await, Some(21845));
    let lines = console(&world, "function a:tail").await;
    assert_eq!(score(&world, "t", "obj").await, Some(5001));
    assert_eq!(
        lines,
        ["Running function a:tail", "Function a:tail returned 99"]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nested_calls_stop_at_the_nesting_cap() {
    let dir = TestDir::new();
    let world = loops_world(&dir, 65536).await;
    console(&world, "function a:stored").await;
    assert_eq!(
        score(&world, "s", "obj").await,
        Some(tuff_feature_builtin_vanilla_commands::function::MAX_NESTING as i32 + 1)
    );
    world.shutdown().await;
}

/// Runs `line`, whose function recurses through a nested call until it has
/// run `cap + 2` times, and checks that every call down to the cap ran and
/// returned while the one call past it was refused.
async fn recurse_past_the_cap(line: &str) {
    let cap = tuff_feature_builtin_vanilla_commands::function::MAX_NESTING as i32;
    let dir = TestDir::new();
    let world = loops_world(&dir, 65536).await;
    let max = cap + 2;
    console(&world, &format!("scoreboard players set max obj {max}")).await;
    console(&world, line).await;
    assert_eq!(score(&world, "s", "obj").await, Some(cap + 1));
    assert_eq!(score(&world, "tried", "obj").await, Some(cap + 1));
    assert_eq!(score(&world, "back", "obj").await, Some(cap));
    assert_eq!(score(&world, "r", "obj").await, Some(7));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stored_calls_reach_the_nesting_cap_and_refuse_one_more() {
    recurse_past_the_cap("execute store result score r obj run function a:stored_to").await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn function_conditions_reach_the_nesting_cap_and_refuse_one_more() {
    recurse_past_the_cap("execute if function a:cond_to run scoreboard players set r obj 7").await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn load_runs_after_each_reload_and_tick_every_tick() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "ticking",
        &[
            (
                "a/function/init.mcfunction",
                "scoreboard objectives add t dummy\nscoreboard players add loads t 1",
            ),
            (
                "a/function/tick.mcfunction",
                "scoreboard players add ticks t 1",
            ),
            (
                "minecraft/tags/function/load.json",
                r#"{"values":["a:init"]}"#,
            ),
            (
                "minecraft/tags/function/tick.json",
                r#"{"values":["a:tick"]}"#,
            ),
        ],
    );
    let world = TestWorld::start(dir.path());
    world.ticks(3).await;
    assert_eq!(score(&world, "loads", "t").await, Some(1));
    assert_eq!(score(&world, "ticks", "t").await, Some(3));
    assert_eq!(console(&world, "reload").await, ["Reloading!"]);
    world.ticks(2).await;
    assert_eq!(score(&world, "loads", "t").await, Some(2));
    assert_eq!(score(&world, "ticks", "t").await, Some(5));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn datapack_commands_list_enable_and_disable_packs() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "one",
        &[("a/function/one.mcfunction", "say one")],
    );
    pack(
        dir.path(),
        "two",
        &[("a/function/two.mcfunction", "say two")],
    );
    let world = TestWorld::start(dir.path());
    assert_eq!(
        console(&world, "datapack list enabled").await,
        [
            "There are 3 data pack(s) enabled: [vanilla (built-in)], [file/one (world)], [file/two (world)]"
        ]
    );
    assert_eq!(
        console(&world, "datapack list available").await,
        ["There are no more data packs available"]
    );
    assert_eq!(
        console(&world, r#"datapack disable "file/one""#).await,
        ["Disabling data pack [file/one (world)]"]
    );
    assert!(
        world
            .shared
            .datapacks
            .contents()
            .function("a:one")
            .is_none()
    );
    assert_eq!(
        world.shared.datapacks.config().disabled,
        [
            "file/one",
            "minecart_improvements",
            "redstone_experiments",
            "trade_rebalance"
        ]
    );
    assert_eq!(
        console(&world, r#"datapack disable "file/one""#).await,
        ["Pack 'file/one' is not enabled!"]
    );
    assert_eq!(
        console(&world, "datapack list available").await,
        ["There are 1 data pack(s) available: [file/one (world)]"]
    );
    pack(
        dir.path(),
        "three",
        &[("a/function/three.mcfunction", "say three")],
    );
    console(&world, "reload").await;
    let contents = world.shared.datapacks.contents();
    assert!(
        contents.function("a:three").is_some(),
        "reload finds new packs"
    );
    assert!(
        contents.function("a:one").is_none(),
        "but keeps disabled ones off"
    );
    console(&world, r#"datapack enable "file/one" before "file/two""#).await;
    assert_eq!(
        world.shared.datapacks.config().enabled,
        ["vanilla", "file/one", "file/two", "file/three"]
    );
    assert_eq!(
        console(&world, r#"datapack enable "file/one""#).await,
        ["Pack 'file/one' is already enabled!"]
    );
    assert_eq!(
        console(&world, r#"datapack enable "file/none""#).await,
        ["Unknown data pack 'file/none'"]
    );
    let refused = console(&world, "datapack enable trade_rebalance")
        .await
        .join("\n");
    assert!(
        refused.contains("trade_rebalance") && refused.contains("minecraft:trade_rebalance"),
        "{refused}"
    );
    assert!(
        !world
            .shared
            .datapacks
            .config()
            .enabled
            .contains(&"trade_rebalance".to_owned())
    );
    console(&world, r#"datapack disable vanilla"#).await;
    assert_eq!(world.shared.datapacks.config().enabled[0], "vanilla");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn datapack_create_writes_an_empty_pack() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    assert_eq!(
        console(&world, r#"datapack create mine "Mine""#).await,
        ["Created new empty pack with name 'mine'"]
    );
    let written = std::fs::read(dir.path().join("datapacks/mine/pack.mcmeta")).unwrap();
    let meta = tuff_core::datapack::PackMeta::parse(&written).unwrap();
    assert_eq!(meta.description.plain(), "Mine");
    assert_eq!(
        meta.compatibility(),
        tuff_core::datapack::Compatibility::Compatible
    );
    assert_eq!(
        console(&world, r#"datapack create mine "Again""#).await,
        ["Pack with name 'mine' already exists"]
    );
    assert_eq!(
        console(&world, r#"datapack create "Bad Name" "x""#).await,
        ["Invalid characters in new pack name 'Bad Name'"]
    );
    assert_eq!(
        console(&world, r#"datapack create con "x""#).await,
        ["Invalid new pack name 'con'"]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pack_that_cannot_be_read_is_left_out_at_startup() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "good",
        &[("a/function/good.mcfunction", "say good")],
    );
    let huge = "#".repeat(tuff_core::datapack::MAX_FILE + 1);
    let zip = tuff_datapack::zip::write(&[
        ("pack.mcmeta", mcmeta().as_bytes()),
        ("data/a/function/huge.mcfunction", huge.as_bytes()),
    ]);
    write(dir.path(), "datapacks/huge.zip", &zip);
    let world = TestWorld::start(dir.path());
    let contents = world.shared.datapacks.contents();
    assert!(contents.function("a:good").is_some());
    assert!(contents.function("a:huge").is_none());
    assert_eq!(
        world.shared.datapacks.config().enabled,
        ["vanilla", "file/good"]
    );
    assert_eq!(world.shared.datapacks.config().disabled[0], "file/huge.zip");
    assert_eq!(
        console(&world, r#"datapack enable "file/huge.zip""#).await,
        [
            "Enabling data pack [file/huge.zip (world)]",
            "Reload failed; keeping old data"
        ]
    );
    assert!(
        world
            .shared
            .datapacks
            .contents()
            .function("a:good")
            .is_some()
    );
    world.shutdown().await;
}

fn player(
    dir: &Path,
    level: tuff_storage::admin::PermissionLevel,
    scoreboard: Option<crate::world::scoreboard::ScoreboardHandle>,
) -> crate::world::session::PlayerSession {
    let (coordinator, mut requests) = mpsc::channel(16);
    tokio::spawn(async move { while requests.recv().await.is_some() {} });
    let mut shared = bare_shared();
    shared.coordinator = coordinator;
    shared.backend = Storage::default().open(dir).unwrap();
    shared.scoreboard = scoreboard.unwrap_or_else(|| {
        crate::world::scoreboard::start(shared.backend.clone(), shared.clock.clone()).0
    });
    shared.datapacks = tuff_core::datapack::DataPacks::open_now(
        shared.backend.clone(),
        shared.commands.clone(),
        tuff_core::datapack::Base {
            recipes: shared.recipe_set.clone(),
            ..Default::default()
        },
        tuff_core::datapack::DataPackConfig::default(),
        None,
    );
    let (outbound, _outbound) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(1000);
    let (_packets, packets) = mpsc::channel(8);
    crate::world::session::PlayerSession::new(
        Arc::new(shared),
        1000,
        crate::auth::GameProfile {
            id: 1000,
            name: "Steve".to_owned(),
            properties: Vec::new(),
        },
        crate::protocol::configuration::ClientInformation::default(),
        outbound,
        handle,
        events,
        packets,
        level,
    )
}

async fn player_score(
    session: &crate::world::session::PlayerSession,
    objective: &str,
) -> Option<i32> {
    let objective = objective.to_owned();
    session
        .shared()
        .scoreboard
        .read(move |board| board.score("Steve", &objective).map(|s| s.value))
        .await
        .flatten()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn players_run_functions_and_rewards_run_them_at_gamemaster_level() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "mine",
        &[
            (
                "a/function/me.mcfunction",
                "scoreboard players add Steve obj 1",
            ),
            ("a/function/op.mcfunction", "op Steve"),
        ],
    );
    let mut gamemaster = player(
        dir.path(),
        tuff_storage::admin::PermissionLevel::Gamemasters,
        None,
    );
    gamemaster
        .run_command("scoreboard objectives add obj dummy")
        .await;
    gamemaster.run_command("function a:me").await;
    assert_eq!(player_score(&gamemaster, "obj").await, Some(1));
    gamemaster.run_reward_function("a:me").await;
    assert_eq!(player_score(&gamemaster, "obj").await, Some(2));
    gamemaster.run_reward_function("a:missing").await;

    let mut visitor = player(
        dir.path(),
        tuff_storage::admin::PermissionLevel::All,
        Some(gamemaster.shared().scoreboard.clone()),
    );
    gamemaster
        .run_command("scoreboard players reset Steve obj")
        .await;
    visitor.run_command("function a:me").await;
    assert_eq!(
        player_score(&visitor, "obj").await,
        None,
        "function needs level 2"
    );
    visitor.run_reward_function("a:me").await;
    assert_eq!(
        player_score(&visitor, "obj").await,
        Some(1),
        "rewards run at level 2"
    );
    assert!(
        visitor
            .shared()
            .datapacks
            .contents()
            .function("a:op")
            .is_none(),
        "a function using a command above level 2 does not load"
    );
}

fn content_session(
    world: &TestWorld,
) -> (
    crate::world::session::PlayerSession,
    mpsc::UnboundedReceiver<bytes::Bytes>,
) {
    let (outbound, outbound_rx) = mpsc::unbounded_channel();
    let (handle, events) = PlayerHandle::new(1001);
    let (_packets, packets) = mpsc::channel(8);
    let session = crate::world::session::PlayerSession::new(
        world.shared.clone(),
        1001,
        crate::auth::GameProfile {
            id: 1001,
            name: "Alex".to_owned(),
            properties: Vec::new(),
        },
        crate::protocol::configuration::ClientInformation::default(),
        outbound,
        handle,
        events,
        packets,
        tuff_storage::admin::PermissionLevel::Owners,
    );
    (session, outbound_rx)
}

fn sent(outbound: &mut mpsc::UnboundedReceiver<bytes::Bytes>, id: i32) -> usize {
    let mut found = 0;
    while let Ok(frame) = outbound.try_recv() {
        if framed_packet_id(&frame) == id {
            found += 1;
        }
    }
    found
}

fn dirt_drops(world: &TestWorld) -> Vec<tuff_item::item::ItemStack> {
    use rand::SeedableRng;
    let mut rng = rand::rngs::StdRng::seed_from_u64(1);
    world.shared.block_loot().drops(
        tuff_vanilla_data::Block::Dirt.default_state(),
        None,
        None,
        &mut rng,
    )
}

const DIRT_LOOT: &str = "minecraft/loot_table/blocks/dirt.json";

fn dirt_table(item: &str) -> String {
    format!(
        r#"{{"type":"block","pools":[{{"rolls":1,"condition":"a:always",
            "entries":[{{"type":"item","name":"{item}"}}]}}]}}"#
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn packs_change_recipes_loot_and_tags_and_reload_resends_them() {
    use tuff_item::item::{ItemStack, MintReason};
    use tuff_vanilla_data::Item;
    let dir = TestDir::new();
    let table = dirt_table("minecraft:diamond");
    pack(
        dir.path(),
        "content",
        &[
            (
                "a/tags/item/gems.json",
                r#"{"values":["minecraft:diamond","minecraft:emerald"]}"#,
            ),
            (
                "minecraft/tags/item/planks.json",
                r#"{"values":["minecraft:stone"]}"#,
            ),
            (
                "a/recipe/gem_to_dirt.json",
                r##"{"type":"minecraft:crafting_shapeless","ingredients":["#a:gems"],
                    "result":{"id":"minecraft:dirt","count":4}}"##,
            ),
            ("minecraft/recipe/stick.json", "{not json"),
            (
                "a/predicate/always.json",
                r#"{"type":"minecraft:inverted","term":{"type":"minecraft:random_chance","chance":0}}"#,
            ),
            (DIRT_LOOT, &table),
        ],
    );
    let world = TestWorld::start(dir.path());

    let recipes = world.shared.recipes();
    assert_eq!(
        recipes.grid_result(
            &[Some(ItemStack::mint(Item::Emerald, 1, MintReason::Test))],
            1,
            1
        ),
        Some(ItemStack::mint(Item::Dirt, 4, MintReason::Test))
    );
    assert!(recipes.get("a:gem_to_dirt").is_some());
    assert!(recipes.get("minecraft:stick").is_none());
    assert!(recipes.get("minecraft:oak_planks").is_some());

    let contents = world.shared.datapacks.contents();
    let planks = contents.tags.typed::<Item>("minecraft:planks").unwrap();
    assert!(planks.contains(&Item::Stone) && planks.contains(&Item::OakPlanks));
    assert_ne!(
        contents.tags_packet,
        tuff_datapack::tags::TagSet::vanilla().network_body()
    );
    assert_eq!(
        dirt_drops(&world),
        [ItemStack::mint(Item::Diamond, 1, MintReason::Test)]
    );

    let (mut player, mut outbound) = content_session(&world);
    let tags = tuff_vanilla_data::packet::play::Clientbound::UpdateTags.id();
    let update_recipes = tuff_vanilla_data::packet::play::Clientbound::UpdateRecipes.id();
    player.resend_reloaded_data();
    assert_eq!(sent(&mut outbound, tags), 0, "nothing changed yet");

    write(
        dir.path(),
        &format!("datapacks/content/data/{DIRT_LOOT}"),
        dirt_table("minecraft:emerald").as_bytes(),
    );
    assert_eq!(console(&world, "reload").await, ["Reloading!"]);
    assert_eq!(
        dirt_drops(&world),
        [ItemStack::mint(Item::Emerald, 1, MintReason::Test)]
    );
    player.resend_reloaded_data();
    let frames: Vec<i32> = std::iter::from_fn(|| outbound.try_recv().ok())
        .map(|frame| framed_packet_id(&frame))
        .collect();
    assert_eq!(frames.iter().filter(|id| **id == tags).count(), 1);
    assert_eq!(frames.iter().filter(|id| **id == update_recipes).count(), 1);
    player.resend_reloaded_data();
    assert_eq!(sent(&mut outbound, tags), 0, "sent once per reload");

    console(&world, r#"datapack disable "file/content""#).await;
    assert_eq!(
        dirt_drops(&world),
        [ItemStack::mint(Item::Dirt, 1, MintReason::Test)]
    );
    assert!(world.shared.recipes().get("minecraft:stick").is_some());
    world.shutdown().await;
}

fn renamed_recipe(generation: usize) -> String {
    let items: Vec<String> = tuff_vanilla_data::Item::ALL[1..=generation]
        .iter()
        .map(|item| format!("\"{}\"", item.name()))
        .collect();
    format!(
        r#"{{"type":"minecraft:crafting_shapeless","group":"g{generation}",
            "ingredients":[[{}]],"result":"minecraft:dirt"}}"#,
        items.join(",")
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reloading_changed_packs_frees_the_previous_recipes_and_loot_tables() {
    use std::sync::{Arc, Weak};
    use tuff_vanilla_data::Frozen;
    use tuff_vanilla_data::recipe::Ingredient;
    let dir = TestDir::new();
    pack(dir.path(), "content", &[]);
    let world = TestWorld::start(dir.path());
    let (mut player, mut outbound) = content_session(&world);
    let mut previous: Option<Vec<Box<dyn Fn() -> usize>>> = None;
    for generation in 1..=12 {
        let data = dir.path().join("datapacks/content/data");
        let _ = std::fs::remove_file(data.join(format!("a/recipe/r{}.json", generation - 1)));
        let item = tuff_vanilla_data::Item::ALL[generation].name();
        pack(
            dir.path(),
            "content",
            &[
                (
                    &format!("a/recipe/r{generation}.json"),
                    &renamed_recipe(generation),
                ),
                (DIRT_LOOT, &dirt_table(item)),
                (
                    "a/predicate/always.json",
                    r#"{"type":"minecraft:random_chance","chance":1}"#,
                ),
            ],
        );
        assert_eq!(console(&world, "reload").await, ["Reloading!"]);
        player.resend_reloaded_data();
        let name = format!("a:r{generation}");
        player.award_recipes([name.as_str()]);
        assert!(player.knows_recipe(&name));
        sent(&mut outbound, -1);

        let recipes = world.shared.recipes();
        assert!(recipes.get(&name).is_some());
        assert!(recipes.get(&format!("a:r{}", generation - 1)).is_none());
        let tables = world.shared.loot_tables();
        assert!(tables.get("minecraft:blocks/dirt").is_some());
        let placement = recipes.get(&name).unwrap().placement().unwrap();
        let Ingredient::AnyOf(Frozen::Shared(items)) = &placement.ingredients[0] else {
            panic!("a pack ingredient is runtime data");
        };
        let Frozen::Shared(pools) = &tables.get("minecraft:blocks/dirt").unwrap().pools else {
            panic!("a pack loot table is runtime data");
        };
        fn alive<T: ?Sized + 'static>(weak: Weak<T>) -> Box<dyn Fn() -> usize> {
            Box::new(move || weak.strong_count())
        }
        let current = vec![
            alive(Arc::downgrade(&recipes)),
            alive(Arc::downgrade(&tables)),
            alive(Arc::downgrade(items)),
            alive(Arc::downgrade(pools)),
        ];
        drop((placement, recipes, tables));
        if let Some(old) = previous.replace(current) {
            assert!(
                old.iter().all(|strong| strong() == 0),
                "generation {} is still alive",
                generation - 1
            );
        }
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn command_tag_arguments_read_the_tags_packs_define() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "tags",
        &[
            (
                "a/tags/block/shiny.json",
                r#"{"values":["minecraft:diamond_block"]}"#,
            ),
            (
                "a/tags/item/gems.json",
                r#"{"values":["minecraft:emerald"]}"#,
            ),
            (
                "a/tags/entity_type/pets.json",
                r#"{"values":["minecraft:pig"]}"#,
            ),
        ],
    );
    let world = TestWorld::start(dir.path());
    console(&world, "forceload add 0 0").await;
    console(&world, "setblock 0 64 0 minecraft:diamond_block").await;
    assert_eq!(
        console(&world, "execute if block 0 64 0 #a:shiny").await,
        ["Test passed"]
    );
    assert_eq!(
        console(&world, "execute if block 0 64 0 #minecraft:logs").await,
        ["Test failed"]
    );
    console(
        &world,
        "summon minecraft:pig 0 310 0 {NoAI:1b,PersistenceRequired:1b}",
    )
    .await;
    assert_eq!(
        console(
            &world,
            "execute if entity @e[type=#a:pets,x=0,y=310,z=0,distance=..2]"
        )
        .await,
        ["Test passed. Count: 1"]
    );

    let tags = world.shared.datapacks.contents().tags.clone();
    let mut reader = crate::command::reader::Reader::new("#a:gems");
    let gems = crate::command::item::parse_item_predicate(&mut reader)
        .unwrap()
        .resolved(&tags)
        .unwrap();
    assert!(gems.test(&tuff_item::item::ItemStack::mint(
        tuff_vanilla_data::Item::Emerald,
        1,
        tuff_item::item::MintReason::Test
    )));
    assert!(!gems.test(&tuff_item::item::ItemStack::mint(
        tuff_vanilla_data::Item::Diamond,
        1,
        tuff_item::item::MintReason::Test
    )));
    world.shutdown().await;
}

fn schedule_pack(dir: &TestDir) {
    pack(
        dir.path(),
        "later",
        &[
            (
                "a/function/count.mcfunction",
                "scoreboard objectives add s dummy\nscoreboard players add runs s 1",
            ),
            (
                "a/function/again.mcfunction",
                "scoreboard objectives add s dummy\nscoreboard players add again s 1\nschedule function a:again 2t",
            ),
            ("a/function/macro.mcfunction", "$say $(x)"),
            ("a/tags/function/both.json", r#"{"values":["a:count"]}"#),
        ],
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schedule_runs_functions_and_tags_when_due() {
    let dir = TestDir::new();
    schedule_pack(&dir);
    let world = TestWorld::start(dir.path());
    let now = *world.shared.clock.borrow();
    assert_eq!(
        console(&world, "schedule function a:count 3t").await,
        [format!(
            "Scheduled function 'a:count' in 3 tick(s) at gametime {}",
            now + 3
        )]
    );
    assert_eq!(
        console(&world, "schedule function a:count 0t").await,
        ["Can't schedule for current tick"]
    );
    assert_eq!(
        console(&world, "schedule function a:macro 1t").await,
        ["Can't schedule a macro"]
    );
    let unknown = console(&world, "schedule function a:nope 1t")
        .await
        .join("\n");
    assert!(unknown.contains("a:nope"), "{unknown}");
    world.ticks(2).await;
    assert_eq!(score(&world, "runs", "s").await, None);
    world.ticks(1).await;
    assert_eq!(score(&world, "runs", "s").await, Some(1));

    console(&world, "schedule function #a:both 2t").await;
    console(&world, "schedule function a:count 5t").await;
    console(&world, "schedule function a:count 1t append").await;
    world.ticks(2).await;
    assert_eq!(score(&world, "runs", "s").await, Some(3));
    world.ticks(3).await;
    assert_eq!(score(&world, "runs", "s").await, Some(4));

    console(&world, "schedule function a:count 4t").await;
    console(&world, "schedule function a:count 2t").await;
    world.ticks(4).await;
    assert_eq!(
        score(&world, "runs", "s").await,
        Some(5),
        "replace is the default"
    );

    console(&world, "function a:again").await;
    world.ticks(4).await;
    assert_eq!(score(&world, "again", "s").await, Some(3));
    world
        .shared
        .services
        .get::<tuff_feature_builtin_vanilla_commands::function::FunctionTicker>()
        .and_then(tuff_feature_builtin_vanilla_commands::function::FunctionTicker::scheduler)
        .expect("the schedule runs")
        .ids()
        .await;
    let queued = crate::world::commands::console::suggest(
        &world.shared,
        "schedule clear a:ag",
        crate::permission::Subject::Server,
    )
    .await;
    assert_eq!(
        queued.matches,
        ["a:again"],
        "clear completes the queued ids"
    );
    assert_eq!(
        console(&world, "schedule clear a:again").await,
        ["Removed 1 schedule(s) with ID a:again"]
    );
    assert_eq!(
        console(&world, "schedule clear a:again").await,
        ["No schedules with ID a:again"]
    );
    world.ticks(4).await;
    assert_eq!(score(&world, "again", "s").await, Some(3));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scheduled_functions_survive_a_restart() {
    let dir = TestDir::new();
    schedule_pack(&dir);
    let world = TestWorld::start(dir.path());
    console(&world, "schedule function a:count 6t").await;
    world.ticks(2).await;
    world.shared.services.flush().await;
    world.shutdown().await;
    assert!(
        dir.path()
            .join("data/minecraft/scheduled_events.dat")
            .exists()
    );

    let world = TestWorld::start(dir.path());
    world.ticks(5).await;
    assert_eq!(score(&world, "runs", "s").await, None);
    world.ticks(1).await;
    assert_eq!(score(&world, "runs", "s").await, Some(1));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_file_keeps_the_scheduled_events() {
    let dir = TestDir::new();
    let backend: Arc<dyn WorldBackend> = Arc::new(
        tuff_storage::sqlite::SqliteBackend::open(dir.path().join("world.mcserver")).unwrap(),
    );
    backend
        .write_extra_file("datapacks/later/pack.mcmeta", mcmeta().as_bytes())
        .unwrap();
    backend
        .write_extra_file(
            "datapacks/later/data/a/function/count.mcfunction",
            b"say counted",
        )
        .unwrap();
    let world = TestWorld::start_with(backend);
    console(&world, "schedule function a:count 30t").await;
    world.shared.services.flush().await;
    let backend = world.shared.backend.clone();
    let saved = tokio::task::spawn_blocking(move || {
        backend
            .documents()
            .read(tuff_storage::document::Document::SavedData(
                "minecraft:scheduled_events",
            ))
    })
    .await
    .unwrap()
    .unwrap()
    .expect("saved in the server file");
    let events = tuff_storage::document::inner(Some(saved));
    let event = events.get_list("events").unwrap()[0]
        .as_compound()
        .unwrap()
        .clone();
    assert_eq!(event.get_str("id"), Some("a:count"));
    assert_eq!(
        event.get_long("trigger_time"),
        Some(*world.shared.clock.borrow() as i64 + 30)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pack_painting_joins_the_registry_and_its_tags_at_startup() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "zap",
        &[
            (
                "zap/painting_variant/shock.json",
                r#"{"width":1,"height":1,"asset_id":"zap:shock"}"#,
            ),
            (
                "minecraft/tags/painting_variant/placeable.json",
                r#"{"values":["zap:shock"]}"#,
            ),
        ],
    );
    let world = TestWorld::start(dir.path());
    let contents = world.shared.datapacks.contents();
    let paintings = contents
        .registries
        .get("minecraft:painting_variant")
        .unwrap();
    assert!(paintings.modified());
    assert!(!paintings.entry("zap:shock").unwrap().is_builtin());
    assert!(paintings.entry("minecraft:kebab").unwrap().is_builtin());
    let tag = contents
        .tags
        .get("minecraft:painting_variant", "minecraft:placeable")
        .unwrap();
    assert!(tag.iter().any(|id| id == "zap:shock"));
    assert!(tag.iter().any(|id| id == "minecraft:kebab"));

    pack(
        dir.path(),
        "zap",
        &[(
            "zap/painting_variant/late.json",
            r#"{"width":1,"height":1,"asset_id":"zap:late"}"#,
        )],
    );
    console(&world, "reload").await;
    let reloaded = world.shared.datapacks.contents();
    assert!(reloaded.generation > contents.generation);
    assert!(
        !reloaded
            .registries
            .get("minecraft:painting_variant")
            .unwrap()
            .contains("zap:late")
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pack_enchantments_change_melee_damage_and_post_attack_effects() {
    use tuff_item::enchantment;
    use tuff_vanilla_common::combat::{self, DamageContext};
    let dir = TestDir::new();
    pack(
        dir.path(),
        "zap",
        &[
            (
                "zap/enchantment/shock.json",
                &enchantment_json(
                    r#"{"minecraft:damage":[{"effect":{"type":"minecraft:add","value":5.0}}],
                        "minecraft:post_attack":[{"enchanted":"attacker","affected":"victim",
                        "effect":{"type":"minecraft:ignite","duration":4.0}}]}"#,
                ),
            ),
            (
                "minecraft/enchantment/sharpness.json",
                &enchantment_json(
                    r#"{"minecraft:damage":[{"effect":{"type":"minecraft:add","value":10.0}}]}"#,
                ),
            ),
        ],
    );
    let world = TestWorld::start(dir.path());
    let contents = world.shared.datapacks.contents();
    let mut sword = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    sword.components.enchantments = Some(tuff_item::item::Enchantments::new(vec![
        ("zap:shock".into(), 1),
        ("minecraft:sharpness".into(), 1),
    ]));
    let context = DamageContext::melee(tuff_vanilla_data::registry::EntityType::Cow);
    let mut rng = rand::thread_rng();
    let with_pack = enchantment::sync_scope(contents.enchantments.clone(), || {
        combat::modify_damage(&sword, &context, 1.0, &mut rng)
    });
    assert_eq!(with_pack, 16.0);
    let vanilla = enchantment::sync_scope(enchantment::vanilla(), || {
        combat::modify_damage(&sword, &context, 1.0, &mut rng)
    });
    assert_eq!(
        vanilla, 2.0,
        "the built-in sharpness stays for other worlds"
    );

    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(1000);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    let floor: Vec<_> = (0..16)
        .flat_map(|x| {
            (0..16).map(move |z| (BlockPos::new(x, SKY - 2, z), Block::Stone.default_state()))
        })
        .collect();
    place_blocks(&world, &region, floor).await;
    let position = [8.5, SKY as f64 - 1.0, 8.5];
    summon(
        &region,
        tuff_vanilla_data::registry::EntityType::Cow,
        Compound::new().with("NoAI", 1i8),
        position,
    )
    .await;
    let cow = mobs(&region, tuff_vanilla_data::registry::EntityType::Cow).await[0].id;
    let mut zap = ItemStack::mint(Item::DiamondSword, 1, MintReason::Test);
    zap.components.enchantments = Some(tuff_item::item::Enchantments::new(vec![(
        "zap:shock".into(),
        1,
    )]));
    region
        .request(RegionRequest::AttackEntity {
            attacker: player.clone(),
            target: cow,
            attack: crate::world::entity::EntityAttack {
                attacker: player.id,
                attacker_uuid: 77,
                attacker_position: position,
                attacker_yaw: 0.0,
                attacker_pitch: 0.0,
                damage: 1.0,
                knockback: 0.0,
                weapon: Some(tuff_item::item::ItemSnapshot::of(&zap)),
                critical: false,
                magic: true,
                sweep: false,
                part: None,
            },
            sweep: None,
        })
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::AttackLanded { .. } => Some(()),
        _ => None,
    })
    .await;
    let fire = entity_nbt(&region, cow)
        .await
        .and_then(|nbt| nbt.get_short("Fire"))
        .unwrap_or_default();
    assert!(
        fire > 60,
        "the pack's post_attack ignite sets the cow on fire: {fire}"
    );
    world.shutdown().await;
}
