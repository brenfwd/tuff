//! Opt-in smoke tests that launch the real vanilla client. Every test is
//! ignored and also needs `TUFF_CLIENT_TESTS=1` and `TUFF_ACCEPT_MOJANG_EULA=1`.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use std::sync::{Arc, Mutex};

use tuff::feature::{Feature, LoadContext};
use tuff::world::Shared;
use tuff::world::custom_dimension::{BiomeDefinition, DimensionDefinition, DimensionProvider};
use tuff::world::dimension::Dimensions;
use tuff::world::generator::ChunkGenerator;
use tuff::world::messages::CoordinatorMessage;
use tuff::world::services::{ServiceFactory, WorldService};
use tuff::{Server, ServerHandle};
use tuff_client_harness::launch::{LaunchSpec, RunDir, default_run_base, options_txt};
use tuff_client_harness::lock::default_lock_path;
use tuff_client_harness::log::ClientEvent;
use tuff_client_harness::meta::Platform;
use tuff_client_harness::protocol::{ClientState, Command};
use tuff_client_harness::supervisor::ClientHandle;
use tuff_client_harness::supervisor::{JOIN_TIMEOUT, launch};
use tuff_client_harness::{AssetProfile, HttpSource, cache_dir, ensure_client, resolve_java};
use tuff_item::load::{LoadError, Phase};
use tuff_world_model::biomes::Biomes;
use tuff_world_model::chunk::{Chunk, ChunkShape};
use tuff_world_model::pos::ChunkPos;

const EULA_URL: &str = "https://aka.ms/MinecraftEULA";

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        eprintln!("[{}] {}", record.level(), record.args());
    }

    fn flush(&self) {}
}

fn init_logging() {
    if log::set_logger(&StderrLogger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

fn enabled() -> bool {
    init_logging();
    if std::env::var("TUFF_CLIENT_TESTS").as_deref() != Ok("1") {
        log::warn!("skipped: set TUFF_CLIENT_TESTS=1 to launch a real vanilla client");
        return false;
    }
    let accepted = matches!(
        std::env::var("TUFF_ACCEPT_MOJANG_EULA").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    );
    assert!(
        accepted,
        "running the vanilla client needs the Minecraft EULA accepted: read {EULA_URL} and set TUFF_ACCEPT_MOJANG_EULA=1"
    );
    true
}

struct Running {
    handle: ServerHandle,
    game: SocketAddr,
    _instance: std::path::PathBuf,
}

async fn start_server(bind: &str, name: &str) -> Running {
    let instance = std::env::temp_dir().join(format!("tuff-client-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&instance);
    std::fs::create_dir_all(&instance).unwrap();
    let server = Server::builder()
        .features(tuff::vanilla_features::all())
        .instance_dir(instance.clone())
        .binds(vec![bind.into()])
        .online_mode(false)
        .config_overrides(vec![("tuff.offlineOps".into(), "true".into())])
        .whitelist(false)
        .view_distance(4)
        .seed(1)
        .build();
    let handle = server.handle();
    tokio::spawn(server.run());
    let game = handle.local_addrs().await.unwrap()[0];
    Running {
        handle,
        game,
        _instance: instance,
    }
}

async fn until(
    client: &ClientHandle,
    limit: Duration,
    what: &str,
    pred: impl Fn(&ClientState) -> bool,
) -> ClientState {
    let deadline = Instant::now() + limit;
    loop {
        if let Ok(state) = client.state().await
            && pred(&state)
        {
            return state;
        }
        assert!(
            Instant::now() < deadline && !client.has_exited().await,
            "timed out waiting for {what}\n{}",
            client.log_tail()
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn server_says(handle: &ServerHandle, command: &str) -> String {
    let lines = handle.run_command(command).await.unwrap();
    lines
        .iter()
        .map(|t| t.plain())
        .collect::<Vec<_>>()
        .join(" ")
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_session() {
    if !enabled() {
        return;
    }
    let name = "real_client_session";
    let v4 = start_server("127.0.0.1:0", "session4").await;
    let v6 = start_server("[::1]:0", "session6").await;
    let addr4 = format!("127.0.0.1:{}", v4.game.port());
    let addr6 = format!("[::1]:{}", v6.game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let username = "Smoke";
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username,
            address: &addr4,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    let mut events = client.subscribe();

    let state = until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    server_says(&v4.handle, &format!("op {username}")).await;
    let [x, y, z] = state.position.unwrap();
    let (bx, by, bz) = (x.floor() as i32, y.floor() as i32 + 3, z.floor() as i32);

    server_says(
        &v4.handle,
        &format!("setblock {bx} {by} {bz} minecraft:glowstone"),
    )
    .await;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if client.block(bx, by, bz).await.unwrap() == "minecraft:glowstone" {
            break;
        }
        assert!(Instant::now() < deadline, "client never saw the glowstone");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    while client.light(bx, by, bz).await.unwrap().0 == 0 {
        assert!(Instant::now() < deadline, "client never lit the glowstone");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    server_says(
        &v4.handle,
        &format!("execute at {username} run summon minecraft:cow ~ ~ ~"),
    )
    .await;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let near = client.entities(10.0).await.unwrap();
        if near.iter().any(|e| e.kind == "minecraft:cow") {
            break;
        }
        assert!(Instant::now() < deadline, "client never saw the cow");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    client
        .request(Command::Command("gamemode creative".into()))
        .await
        .unwrap();
    until(&client, Duration::from_secs(20), "creative mode", |s| {
        s.game_mode.as_deref() == Some("creative")
    })
    .await;
    client
        .request(Command::Chat("hello from the hook".into()))
        .await
        .unwrap();
    client
        .request(Command::Look {
            yaw: 90.0,
            pitch: 10.0,
        })
        .await
        .unwrap();
    client.request(Command::Hotbar(3)).await.unwrap();
    client
        .request(Command::Key {
            name: "forward".into(),
            down: true,
        })
        .await
        .unwrap();
    client.request(Command::WaitTicks(10)).await.unwrap();
    client
        .request(Command::Key {
            name: "forward".into(),
            down: false,
        })
        .await
        .unwrap();
    client.request(Command::Attack).await.unwrap();
    client.request(Command::Use).await.unwrap();
    client
        .request(Command::Click {
            slot: 0,
            button: 0,
            mode: "pickup".into(),
        })
        .await
        .unwrap();
    client.request(Command::Close).await.unwrap();
    client.request(Command::Respawn).await.unwrap();
    client.request(Command::ReleaseMouse).await.unwrap();
    client
        .request(Command::Screenshot("session".into()))
        .await
        .unwrap();
    let shot = run.game_dir().join("screenshots/session.png");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !shot.exists() {
        assert!(Instant::now() < deadline, "screenshot was not written");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    client.request(Command::Disconnect).await.unwrap();
    until(&client, Duration::from_secs(20), "disconnect", |s| {
        !s.connected
    })
    .await;
    client
        .request(Command::Connect(addr6.clone()))
        .await
        .unwrap();
    until(&client, JOIN_TIMEOUT, "rejoin over IPv6", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    assert!(server_says(&v6.handle, "list").await.contains(username));

    while let Ok(event) = events.try_recv() {
        if let ClientEvent::ProtocolError(line) = event {
            panic!("client reported a problem: {line}\n{}", client.log_tail());
        }
    }
    let status = client.quit().await.expect("client exit status");
    assert!(
        status.success(),
        "unclean exit {status:?}\n{}",
        client.log_tail()
    );
    assert!(
        client.log_tail().contains("Stopping!"),
        "client did not run its shutdown path\n{}",
        client.log_tail()
    );
    let options = std::fs::read_to_string(run.game_dir().join("options.txt")).unwrap();
    for line in options_txt(tuff_vanilla_data::version::DATA_VERSION).lines() {
        let key = line.split(':').next().unwrap();
        assert!(
            options.contains(&format!("{key}:")),
            "options.txt lost {key}"
        );
    }
    let _ = std::fs::remove_dir_all(&run.root);
}

const SWEEP_ENTITIES: &[&str] = &[
    "cow",
    "pig",
    "sheep",
    "chicken",
    "horse",
    "donkey",
    "llama",
    "wolf",
    "cat",
    "fox",
    "panda",
    "bee",
    "axolotl",
    "frog",
    "turtle",
    "villager",
    "iron_golem",
    "snow_golem",
    "zombie",
    "skeleton",
    "creeper",
    "spider",
    "enderman",
    "witch",
    "slime",
    "blaze",
    "ghast",
    "phantom",
    "armor_stand",
    "item_frame",
    "painting",
    "minecart",
    "oak_boat",
    "tnt",
    "item_display",
    "block_display",
    "text_display",
    "interaction",
    "marker",
    "end_crystal",
    "falling_block",
    "arrow",
    "snowball",
    "ender_pearl",
    "experience_orb",
    "lightning_bolt",
    "area_effect_cloud",
];

const SWEEP_KNOWN_BAD: &[&str] = &[];

const SWEEP_BLOCK_ENTITIES: &[&str] = &[
    "chest",
    "ender_chest",
    "trapped_chest",
    "barrel",
    "furnace",
    "blast_furnace",
    "smoker",
    "hopper",
    "dropper",
    "dispenser",
    "brewing_stand",
    "enchanting_table",
    "beacon",
    "jukebox",
    "lectern",
    "bell",
    "oak_sign",
    "oak_hanging_sign",
    "white_banner",
    "player_head",
    "skeleton_skull",
    "spawner",
    "command_block",
    "structure_block",
    "comparator",
    "daylight_detector",
    "white_bed",
    "conduit",
    "end_portal",
    "end_gateway",
    "sculk_sensor",
    "sculk_shrieker",
    "sculk_catalyst",
    "decorated_pot",
    "crafter",
    "trial_spawner",
    "vault",
    "campfire",
    "chiseled_bookshelf",
    "suspicious_sand",
    "shulker_box",
    "white_bed",
];

const SWEEP_BURSTS: &[(&str, &[&str])] = &[
    (
        "ui",
        &[
            "title @a title {\"text\":\"Sweep\"}",
            "title @a subtitle {\"text\":\"sub\"}",
            "title @a actionbar {\"text\":\"bar\"}",
            "title @a times 5 20 5",
            "title @a clear",
            "bossbar add sweep:a {\"text\":\"Boss\"}",
            "bossbar set sweep:a players @a",
            "bossbar set sweep:a value 40",
            "bossbar set sweep:a color red",
            "bossbar set sweep:a style notched_10",
            "bossbar set sweep:a visible false",
            "bossbar remove sweep:a",
            "tellraw @a [{\"text\":\"a\",\"color\":\"red\"},{\"translate\":\"block.minecraft.stone\"}]",
            "say sweep",
            "playsound minecraft:entity.cow.ambient master @a",
            "stopsound @a",
            "particle minecraft:flame ~ ~1 ~ 0.5 0.5 0.5 0.1 20",
            "particle minecraft:dust{color:[1.0,0.0,0.0],scale:1.0} ~ ~1 ~ 0 0 0 0 5",
            "particle minecraft:block{block_state:\"minecraft:stone\"} ~ ~1 ~ 0 0 0 0 5",
            "particle minecraft:item{item:\"minecraft:apple\"} ~ ~1 ~ 0 0 0 0 5",
            "particle minecraft:entity_effect{color:[1.0,0.0,0.0,1.0]} ~ ~1 ~ 0 0 0 0 5",
            "particle minecraft:vibration{destination:{type:\"block\",pos:[0,64,0]},arrival_in_ticks:10} ~ ~1 ~",
            "particle minecraft:explosion ~ ~1 ~",
            "particle minecraft:sculk_charge{roll:1.0} ~ ~1 ~",
        ],
    ),
    (
        "scoreboard",
        &[
            "scoreboard objectives add sw dummy {\"text\":\"SW\"}",
            "scoreboard objectives add hp health",
            "scoreboard objectives add sw2 dummy",
            "scoreboard objectives setdisplay sidebar sw",
            "scoreboard objectives setdisplay list hp",
            "scoreboard objectives setdisplay below_name hp",
            "scoreboard objectives setdisplay sidebar.team.red sw2",
            "scoreboard players set @a sw 5",
            "scoreboard players add @a sw2 3",
            "scoreboard players reset @a sw2",
            "scoreboard objectives modify sw displayname {\"text\":\"Renamed\"}",
            "scoreboard objectives modify sw numberformat fixed {\"text\":\"fx\"}",
            "team add red",
            "team modify red color red",
            "team modify red prefix {\"text\":\"[R] \"}",
            "team modify red friendlyFire false",
            "team modify red nametagVisibility never",
            "team join red @a",
            "team leave @a",
            "team remove red",
            "scoreboard objectives remove sw",
            "scoreboard objectives remove hp",
            "scoreboard objectives remove sw2",
        ],
    ),
    (
        "world",
        &[
            "worldborder set 100",
            "worldborder center 10 10",
            "worldborder set 50 3",
            "worldborder warning distance 5",
            "worldborder warning time 5",
            "worldborder damage amount 1",
            "worldborder set 29999984",
            "worldborder center 0 0",
            "time set 6000",
            "time add 100",
            "weather rain",
            "weather thunder",
            "weather clear",
            "gamerule advance_time false",
            "gamerule advance_time true",
            "gamerule reduced_debug_info true",
            "gamerule reduced_debug_info false",
            "gamerule immediate_respawn true",
            "gamerule immediate_respawn false",
            "difficulty hard",
            "difficulty peaceful",
            "difficulty normal",
            "setworldspawn ~ ~ ~",
            "spawnpoint @a ~ ~ ~",
            "fill ~2 ~5 ~2 ~6 ~9 ~6 minecraft:stone",
            "fill ~2 ~5 ~2 ~6 ~9 ~6 minecraft:air",
            "setblock ~ ~4 ~ minecraft:note_block",
            "setblock ~1 ~4 ~ minecraft:piston[facing=up]",
            "setblock ~2 ~4 ~ minecraft:water",
            "setblock ~3 ~4 ~ minecraft:lava",
            "setblock ~4 ~4 ~ minecraft:tnt",
            "setblock ~5 ~4 ~ minecraft:redstone_wire",
            "clone ~2 ~4 ~ ~5 ~4 ~ ~2 ~10 ~",
            "kill @e[type=!player]",
        ],
    ),
    (
        "player",
        &[
            "give @a minecraft:diamond_sword[enchantments={sharpness:5,unbreaking:3},custom_name='{\"text\":\"Sw\"}',lore=['{\"text\":\"l\"}'],unbreakable={},damage=5,repair_cost=3,custom_model_data={floats:[1.0]},rarity=epic]",
            "give @a minecraft:potion[potion_contents={potion:\"minecraft:strong_healing\",custom_color:255,custom_effects:[{id:\"minecraft:speed\",duration:200}]}]",
            "give @a minecraft:player_head[profile={name:\"Notch\"}]",
            "give @a minecraft:firework_rocket[fireworks={flight_duration:2,explosions:[{shape:\"star\",colors:[16711680],has_trail:true}]}]",
            "give @a minecraft:written_book[written_book_content={title:\"t\",author:\"a\",pages:['{\"text\":\"p\"}']}]",
            "give @a minecraft:filled_map",
            "give @a minecraft:crossbow[charged_projectiles=[{id:\"minecraft:arrow\",count:1}]]",
            "give @a minecraft:shield[base_color=\"red\",banner_patterns=[{pattern:\"minecraft:stripe_top\",color:\"white\"}]]",
            "give @a minecraft:leather_chestplate[dyed_color=255,trim={material:\"minecraft:gold\",pattern:\"minecraft:eye\"}]",
            "give @a minecraft:bundle[bundle_contents=[{id:\"minecraft:apple\",count:3}]]",
            "give @a minecraft:chest[container=[{slot:0,item:{id:\"minecraft:stone\",count:2}}]]",
            "give @a minecraft:goat_horn[instrument=\"minecraft:ponder_goat_horn\"]",
            "give @a minecraft:enchanted_book[stored_enchantments={mending:1}]",
            "give @a minecraft:suspicious_stew[suspicious_stew_effects=[{id:\"minecraft:speed\",duration:100}]]",
            "give @a minecraft:golden_apple[food={nutrition:4,saturation:2.0,can_always_eat:true}]",
            "give @a minecraft:diamond_pickaxe[tool={rules:[{blocks:\"#minecraft:mineable/pickaxe\",speed:5.0}],default_mining_speed:1.0}]",
            "recipe give @a *",
            "recipe take @a *",
            "recipe give @a minecraft:crafting_table",
            "advancement grant @a everything",
            "advancement revoke @a everything",
            "advancement grant @a only minecraft:story/root",
            "effect give @a minecraft:speed 10 1",
            "effect give @a minecraft:night_vision 10",
            "effect give @a minecraft:glowing 10",
            "effect clear @a",
            "xp add @a 500 points",
            "xp set @a 0 levels",
            "attribute @a minecraft:max_health base set 30",
            "attribute @a minecraft:max_health base reset",
            "gamemode spectator",
            "gamemode survival",
            "gamemode adventure",
            "gamemode creative",
            "tag @a add sw",
            "data merge entity @a {Air:100}",
            "enchant @a minecraft:sharpness 1",
            "clear @a",
            "item replace entity @a weapon.mainhand with minecraft:stick",
            "item replace entity @a armor.head with minecraft:iron_helmet",
            "item replace entity @a weapon.offhand with minecraft:shield",
            "tp @a ~ ~ ~ 45 10",
            "tp @a ~5 ~ ~",
            "tp @a ~-5 ~ ~",
            "spreadplayers 0 0 5 20 false @a",
            "damage @a 1",
        ],
    ),
];

async fn sweep_ok(client: &ClientHandle, burst: &str) {
    let state = client.state().await;
    assert!(
        matches!(&state, Ok(s) if s.connected),
        "client dropped during burst {burst}: {state:?}\n{}",
        client.log_tail()
    );
}

fn drain_errors(events: &mut tokio::sync::broadcast::Receiver<ClientEvent>, burst: &str) {
    while let Ok(event) = events.try_recv() {
        match event {
            ClientEvent::ProtocolError(line) => panic!("protocol error in burst {burst}: {line}"),
            ClientEvent::Disconnected(why) => panic!("disconnected in burst {burst}: {why}"),
            _ => {}
        }
    }
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_protocol_sweep() {
    if !enabled() {
        return;
    }
    let name = "real_client_protocol_sweep";
    let srv = start_server("127.0.0.1:0", "sweep").await;
    let addr = format!("127.0.0.1:{}", srv.game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let username = "Sweep";
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username,
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    let mut events = client.subscribe();
    until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    server_says(&srv.handle, &format!("op {username}")).await;
    server_says(&srv.handle, "gamemode creative Sweep").await;

    let at = |cmd: &str| format!("execute at {username} run {cmd}");
    let mut rejected = Vec::new();
    let mut run_cmd = async |burst: &str, cmd: String| {
        let out = server_says(&srv.handle, &cmd).await;
        if out.to_lowercase().contains("unknown") || out.to_lowercase().contains("incorrect") {
            rejected.push(format!("{burst}: {cmd} -> {out}"));
        }
    };

    for (burst, cmds) in SWEEP_BURSTS {
        for cmd in *cmds {
            run_cmd(burst, at(cmd)).await;
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        sweep_ok(&client, burst).await;
        drain_errors(&mut events, burst);
    }

    for e in SWEEP_ENTITIES
        .iter()
        .filter(|e| !SWEEP_KNOWN_BAD.contains(e))
    {
        run_cmd("entities", at(&format!("summon minecraft:{e} ~ ~ ~"))).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        sweep_ok(&client, &format!("entity {e}")).await;
        drain_errors(&mut events, &format!("entity {e}"));
    }
    run_cmd("entities", at("kill @e[type=!player]")).await;

    for (i, b) in SWEEP_BLOCK_ENTITIES.iter().enumerate() {
        let (dx, dz) = (3 + (i as i32 % 8) * 2, 3 + (i as i32 / 8) * 2);
        run_cmd(
            "block_entities",
            at(&format!("setblock ~{dx} ~ ~{dz} minecraft:{b}")),
        )
        .await;
    }
    run_cmd(
        "block_entities",
        at("setblock ~ ~ ~-3 minecraft:oak_sign{front_text:{messages:['{\"text\":\"hi\"}','','','']}}"),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(2000)).await;
    sweep_ok(&client, "block_entities").await;
    drain_errors(&mut events, "block_entities");

    for dim in ["the_nether", "the_end", "overworld"] {
        run_cmd(
            "dimension",
            format!("execute in minecraft:{dim} run tp {username} 0 100 0"),
        )
        .await;
        until(&client, Duration::from_secs(40), "dimension change", |s| {
            s.connected && s.chunks.unwrap_or(0) > 0
        })
        .await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        sweep_ok(&client, dim).await;
        drain_errors(&mut events, dim);
    }

    run_cmd("death", "gamemode survival Sweep".into()).await;
    run_cmd("death", "kill Sweep".into()).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    client.request(Command::Respawn).await.unwrap();
    until(&client, Duration::from_secs(40), "respawn", |s| {
        s.connected && s.screen == "none"
    })
    .await;
    sweep_ok(&client, "respawn").await;
    drain_errors(&mut events, "respawn");

    eprintln!(
        "sweep: {} commands rejected by the server:\n{}",
        rejected.len(),
        rejected.join("\n")
    );
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_spectator_click() {
    if !enabled() {
        return;
    }
    let name = "real_client_spectator_click";
    let srv = start_server("127.0.0.1:0", "spectator").await;
    let addr = format!("127.0.0.1:{}", srv.game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let username = "Spec";
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username,
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    let mut events = client.subscribe();
    until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    server_says(&srv.handle, &format!("op {username}")).await;
    server_says(&srv.handle, "gamemode spectator Spec").await;

    let at = |cmd: &str| format!("execute at {username} run {cmd}");
    server_says(
        &srv.handle,
        &at("summon minecraft:creeper ~ ~ ~2 {NoAI:1b}"),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    client
        .request(Command::Look {
            yaw: 0.0,
            pitch: 0.0,
        })
        .await
        .unwrap();
    for burst in ["spectator attack", "spectator use"] {
        let cmd = if burst.ends_with("attack") {
            Command::Attack
        } else {
            Command::Use
        };
        client.request(cmd).await.unwrap();
        tokio::time::sleep(Duration::from_millis(1000)).await;
        sweep_ok(&client, burst).await;
        drain_errors(&mut events, burst);
    }
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_hooks() {
    if !enabled() {
        return;
    }
    let name = "real_client_hooks";
    let srv = start_server("127.0.0.1:0", "hooks").await;
    let addr = format!("127.0.0.1:{}", srv.game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let username = "Hooky";
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username,
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    let class = "net.minecraft.client.Minecraft";
    client.hook(class, "tick").await.unwrap();
    client.request(Command::WaitTicks(10)).await.unwrap();
    let (before, after, _) = client.hook_hits(class, "tick").await.unwrap();
    assert!(before >= 1 && after >= 1, "{before} {after}");
    assert_eq!(client.eval("mc.player.getHealth()").await.unwrap(), "20.0");
    client.set("mc.player.lastHurt", "7.5").await.unwrap();
    assert_eq!(client.eval("mc.player.lastHurt").await.unwrap(), "7.5");
    client.set("mc.player.lastHurt", "0").await.unwrap();
    assert!(
        client
            .eval("mc.player.getName().getString()")
            .await
            .unwrap()
            .contains("Hooky")
    );
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_hook_alters() {
    if !enabled() {
        return;
    }
    let name = "real_client_hook_alters";
    let srv = start_server("127.0.0.1:0", "hookalt").await;
    let addr = format!("127.0.0.1:{}", srv.game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let username = "Alterer";
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username,
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    let entity = "net.minecraft.world.entity.Entity";
    let living = "net.minecraft.world.entity.LivingEntity";
    let inventory = "net.minecraft.world.entity.player.Inventory";
    client
        .hook_alter(living, "getHealth", "return=5")
        .await
        .unwrap();
    assert_eq!(client.eval("mc.player.getHealth()").await.unwrap(), "5.0");
    client
        .hook_alter(entity, "setNoGravity", "arg0=true")
        .await
        .unwrap();
    client.eval("mc.player.setNoGravity(false)").await.unwrap();
    assert_eq!(
        client.eval("mc.player.isNoGravity()").await.unwrap(),
        "true"
    );
    client
        .hook_alter(entity, "setSilent", "skip")
        .await
        .unwrap();
    client.eval("mc.player.setSilent(true)").await.unwrap();
    assert_eq!(client.eval("mc.player.isSilent()").await.unwrap(), "false");
    let (before, _, _) = client.hook_hits(entity, "setSilent").await.unwrap();
    assert_eq!(before, 1);
    client.hook(inventory, "setSelectedSlot").await.unwrap();
    assert!(
        client
            .eval("mc.player.getInventory().setSelectedSlot(99)")
            .await
            .is_err()
    );
    let (before, after, thrown) = client
        .hook_hits(inventory, "setSelectedSlot")
        .await
        .unwrap();
    assert_eq!((before, after, thrown), (1, 0, 1));
    client.set("mc.options.renderDistance", "5").await.unwrap();
    assert_eq!(
        client
            .eval("mc.options.renderDistance.get()")
            .await
            .unwrap(),
        "5"
    );
    client.request(Command::Hotbar(3)).await.unwrap();
    assert_eq!(
        client
            .eval("mc.player.getInventory().getSelectedSlot()")
            .await
            .unwrap(),
        "2"
    );
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_recipe_book_each() {
    if !enabled() {
        return;
    }
    let name = "real_client_recipe_book_each";
    let srv = start_server("127.0.0.1:0", "recipes").await;
    let addr = format!("127.0.0.1:{}", srv.game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username: "Book",
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    let mut bad = Vec::new();
    for (recipe, _) in tuff_vanilla_data::recipe::ALL {
        let out = server_says(&srv.handle, &format!("recipe give Book {recipe}")).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        if client.state().await.map(|s| !s.connected).unwrap_or(true) {
            bad.push(format!("{recipe} ({out})"));
            break;
        }
    }
    assert!(bad.is_empty(), "client dropped on recipe {bad:?}");
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
}

struct Lane {
    name: &'static str,
    keys: &'static [&'static str],
    ticks: u32,
    forward_only: bool,
}

async fn drive(client: &ClientHandle, lane: &Lane) {
    for key in lane.keys {
        client
            .request(Command::Key {
                name: (*key).into(),
                down: true,
            })
            .await
            .unwrap();
    }
    let first = client.state().await.unwrap();
    let mut last = first.position.unwrap();
    let mut last_tick = first.tick.unwrap_or(0);
    for _ in 0..lane.ticks {
        client.request(Command::WaitTicks(1)).await.unwrap();
        let st = client.state().await.unwrap();
        let now = st.position.unwrap();
        let elapsed = (st.tick.unwrap_or(last_tick) - last_tick).max(1) as f64;
        last_tick = st.tick.unwrap_or(last_tick);
        let dx = now[0] - last[0];
        let horizontal = (dx * dx + (now[2] - last[2]).powi(2)).sqrt();
        assert!(
            horizontal < 0.5 * elapsed + 0.8,
            "lane {}: moved {horizontal} in one sample (setback?) {last:?} -> {now:?}\n{}",
            lane.name,
            client.log_tail()
        );
        assert!(
            !lane.forward_only || dx > -0.05,
            "lane {}: moved backwards by {dx} (setback?) {last:?} -> {now:?}\n{}",
            lane.name,
            client.log_tail()
        );
        last = now;
    }
    for key in lane.keys {
        client
            .request(Command::Key {
                name: (*key).into(),
                down: false,
            })
            .await
            .unwrap();
    }
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_movement() {
    if !enabled() {
        return;
    }
    let name = "real_client_movement";
    let srv = start_server("127.0.0.1:0", "movement").await;
    let addr = format!("127.0.0.1:{}", srv.game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let username = "Move";
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username,
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    let mut events = client.subscribe();
    let state = until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    server_says(&srv.handle, "gamemode survival Move").await;
    let [px, py, pz] = state.position.unwrap();
    let x0 = px.floor() as i32;
    let fy = py.floor() as i32 + 40;
    let z0 = pz.floor() as i32;

    let s = &srv.handle;
    let lane_z = |i: i32| z0 + 5 * i;
    for i in 0..8 {
        let z = lane_z(i);
        server_says(
            s,
            &format!(
                "fill {} {} {z} {} {} {z} minecraft:air",
                x0 - 3,
                fy + 1,
                x0 + 40,
                fy + 12
            ),
        )
        .await;
        server_says(
            s,
            &format!(
                "fill {} {fy} {z} {} {fy} {z} minecraft:stone",
                x0 - 3,
                x0 + 40
            ),
        )
        .await;
    }
    let z = lane_z(2);
    for k in 0..5 {
        server_says(
            s,
            &format!(
                "fill {} {fy} {z} {} {} {z} minecraft:stone",
                x0 + 5 + k,
                x0 + 5 + k,
                fy + k
            ),
        )
        .await;
        server_says(
            s,
            &format!(
                "setblock {} {} {z} minecraft:stone_stairs[facing=east]",
                x0 + 5 + k,
                fy + 1 + k
            ),
        )
        .await;
    }
    let z = lane_z(3);
    server_says(
        s,
        &format!(
            "fill {} {} {z} {} {} {z} minecraft:stone_slab[type=bottom]",
            x0 + 5,
            fy + 1,
            x0 + 12,
            fy + 1
        ),
    )
    .await;
    let z = lane_z(4);
    server_says(
        s,
        &format!(
            "fill {} {fy} {z} {} {fy} {z} minecraft:packed_ice",
            x0 + 3,
            x0 + 30
        ),
    )
    .await;
    let z = lane_z(5);
    server_says(
        s,
        &format!(
            "fill {} {} {z} {} {} {z} minecraft:water",
            x0 + 3,
            fy + 1,
            x0 + 25,
            fy + 3
        ),
    )
    .await;
    let z = lane_z(6);
    server_says(
        s,
        &format!(
            "fill {} {} {z} {} {} {z} minecraft:stone",
            x0 + 3,
            fy + 1,
            x0 + 3,
            fy + 10
        ),
    )
    .await;
    server_says(
        s,
        &format!(
            "fill {} {} {z} {} {} {z} minecraft:ladder[facing=west]",
            x0 + 2,
            fy + 1,
            x0 + 2,
            fy + 10
        ),
    )
    .await;
    let z = lane_z(7);
    server_says(s, &format!("setblock {x0} {fy} {z} minecraft:hay_block")).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;

    let lanes = [
        Lane {
            name: "walk",
            keys: &["forward"],
            ticks: 80,
            forward_only: true,
        },
        Lane {
            name: "sprint_jump",
            keys: &["forward", "sprint", "jump"],
            ticks: 100,
            forward_only: true,
        },
        Lane {
            name: "stairs",
            keys: &["forward", "sprint"],
            ticks: 60,
            forward_only: true,
        },
        Lane {
            name: "slabs",
            keys: &["forward"],
            ticks: 60,
            forward_only: true,
        },
        Lane {
            name: "ice",
            keys: &["forward", "sprint"],
            ticks: 60,
            forward_only: true,
        },
        Lane {
            name: "swim",
            keys: &["forward", "sprint", "jump"],
            ticks: 80,
            forward_only: true,
        },
        Lane {
            name: "ladder",
            keys: &["forward"],
            ticks: 60,
            forward_only: false,
        },
        Lane {
            name: "hay_fall",
            keys: &[],
            ticks: 60,
            forward_only: false,
        },
    ];
    for (i, lane) in lanes.iter().enumerate() {
        let z = lane_z(i as i32);
        let start_y = if lane.name == "hay_fall" {
            fy + 12
        } else {
            fy + 1
        };
        server_says(s, &format!("tp {username} {}.5 {start_y} {z}.5 -90 0", x0)).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        client
            .request(Command::Look {
                yaw: -90.0,
                pitch: 0.0,
            })
            .await
            .unwrap();
        client.request(Command::WaitTicks(5)).await.unwrap();
        drive(&client, lane).await;
        client.request(Command::WaitTicks(10)).await.unwrap();
        sweep_ok(&client, lane.name).await;
        drain_errors(&mut events, lane.name);
        let report = server_says(s, &format!("anticheat violations {username}")).await;
        assert!(
            report.contains("no anti-cheat violations"),
            "lane {} flagged: {report}",
            lane.name
        );
    }
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
}

async fn client_block_is(client: &ClientHandle, x: i32, y: i32, z: i32, id: &str) -> bool {
    for _ in 0..100 {
        if client_block_is_now(client, x, y, z, id).await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn client_block_is_now(client: &ClientHandle, x: i32, y: i32, z: i32, id: &str) -> bool {
    client.block(x, y, z).await.is_ok_and(|b| b.contains(id))
}

async fn server_block_is(srv: &ServerHandle, x: i32, y: i32, z: i32, id: &str) -> bool {
    server_says(srv, &format!("execute if block {x} {y} {z} {id}"))
        .await
        .contains("passed")
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_world_view() {
    if !enabled() {
        return;
    }
    let name = "real_client_world_view";
    let srv = start_server("127.0.0.1:0", "worldview").await;
    let addr = format!("127.0.0.1:{}", srv.game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username: "View",
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    let state = until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 20
    })
    .await;
    let s = &srv.handle;
    let [px, py, pz] = state.position.unwrap();
    let (x0, z0) = (px.floor() as i32, pz.floor() as i32);
    let py = py.floor() as i32;

    let mut terrain_mismatch = Vec::new();
    for dx in (-24..=24).step_by(6) {
        for dz in (-24..=24).step_by(6) {
            for y in (py - 6)..=(py + 3) {
                let (x, z) = (x0 + dx, z0 + dz);
                let c = client.block(x, y, z).await.unwrap();
                let id = c
                    .split('[')
                    .next()
                    .unwrap()
                    .trim_start_matches("Block{")
                    .trim_end_matches('}')
                    .to_string();
                if !server_block_is(s, x, y, z, &id).await {
                    terrain_mismatch.push((x, y, z, c));
                }
            }
        }
    }
    assert!(
        terrain_mismatch.is_empty(),
        "terrain mismatch: {terrain_mismatch:?}"
    );
    client
        .request(Command::Screenshot("world_view_terrain".into()))
        .await
        .unwrap();

    let fy = py + 40;
    let (bx, bz) = (x0 + 2, z0 + 2);
    server_says(
        s,
        &format!(
            "fill {} {fy} {} {} {fy} {} minecraft:stone",
            bx - 4,
            bz - 4,
            bx + 4,
            bz + 4
        ),
    )
    .await;
    server_says(
        s,
        &format!("setblock {bx} {} {bz} minecraft:glowstone", fy + 1),
    )
    .await;
    server_says(
        s,
        &format!(
            "setblock {} {} {bz} minecraft:redstone_lamp",
            bx + 2,
            fy + 1
        ),
    )
    .await;
    server_says(
        s,
        &format!(
            "setblock {} {} {bz} minecraft:redstone_block",
            bx + 3,
            fy + 1
        ),
    )
    .await;
    server_says(
        s,
        &format!("setblock {} {} {bz} minecraft:water", bx - 3, fy + 3),
    )
    .await;
    for (x, y, z, id) in [
        (bx, fy, bz, "stone"),
        (bx, fy + 1, bz, "glowstone"),
        (bx + 3, fy + 1, bz, "redstone_block"),
        (bx - 3, fy + 3, bz, "water"),
    ] {
        assert!(
            server_block_is(s, x, y, z, &format!("minecraft:{id}")).await,
            "server {id}"
        );
        assert!(
            client_block_is(&client, x, y, z, id).await,
            "client {id} at {x} {y} {z}"
        );
    }
    assert!(server_block_is(s, bx + 2, fy + 1, bz, "minecraft:redstone_lamp[lit=true]").await);
    assert!(
        client_block_is(&client, bx + 2, fy + 1, bz, "lit=true").await,
        "lamp lit on client"
    );

    let mut flowed = false;
    for _ in 0..60 {
        if server_block_is(s, bx - 3, fy + 2, bz, "minecraft:water").await
            && client_block_is_now(&client, bx - 3, fy + 2, bz, "water").await
        {
            flowed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(flowed, "water did not flow identically");

    let mut light_ok = false;
    for _ in 0..60 {
        let (b_above, _) = client.light(bx, fy + 2, bz).await.unwrap();
        let (b_side, _) = client.light(bx + 1, fy + 2, bz).await.unwrap();
        if b_above == 14 && b_side >= 12 {
            light_ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(light_ok, "block light above glowstone not on client");
    let (_, sky) = client.light(bx, fy + 8, bz).await.unwrap();
    assert_eq!(sky, 15, "open sky light");

    server_says(s, &format!("setblock {bx} {} {bz} minecraft:air", fy + 1)).await;
    let mut dark = false;
    for _ in 0..60 {
        if client.light(bx, fy + 2, bz).await.unwrap().0 == 12 {
            dark = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(dark, "block light did not clear after glowstone removal");

    server_says(
        s,
        &format!(
            "fill {} {} {} {} {} {} minecraft:stone",
            bx - 4,
            fy + 10,
            bz - 4,
            bx + 4,
            fy + 14,
            bz + 4
        ),
    )
    .await;
    server_says(s, &format!("summon tnt {bx} {} {bz} {{fuse:1}}", fy + 12)).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    client.request(Command::WaitTicks(10)).await.unwrap();
    let mut explosion_mismatch = Vec::new();
    for dx in -3..=3 {
        for dy in 8..=16 {
            for dz in -3..=3 {
                let (x, y, z) = (bx + dx, fy + dy, bz + dz);
                let on_server = server_block_is(s, x, y, z, "minecraft:stone").await;
                let on_client = client_block_is_now(&client, x, y, z, "stone").await;
                if on_server != on_client {
                    explosion_mismatch.push((x, y, z, on_server));
                }
            }
        }
    }
    assert!(
        explosion_mismatch.is_empty(),
        "explosion mismatch: {explosion_mismatch:?}"
    );

    let ey = fy + 2;
    server_says(s, &format!("summon armor_stand {bx}.5 {ey} {bz}.5")).await;
    server_says(
        s,
        &format!("summon pig {}.5 {ey} {bz}.5 {{NoAI:1b}}", bx + 1),
    )
    .await;
    let mut found = (false, false);
    for _ in 0..60 {
        let list = client.entities(100.0).await.unwrap();
        let near = |kind: &str, x: f64| {
            list.iter().any(|e| {
                e.kind.contains(kind)
                    && (e.position[0] - x).abs() < 0.2
                    && (e.position[1] - ey as f64).abs() < 0.5
                    && (e.position[2] - (bz as f64 + 0.5)).abs() < 0.2
            })
        };
        found = (
            near("armor_stand", bx as f64 + 0.5),
            near("pig", bx as f64 + 1.5),
        );
        if found == (true, true) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(found, (true, true), "client entities differ from summons");
    server_says(s, "kill @e[type=minecraft:pig]").await;
    let mut gone = false;
    for _ in 0..60 {
        let list = client.entities(100.0).await.unwrap();
        if !list.iter().any(|e| e.kind.contains("pig")) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "killed pig still on client");
    client
        .request(Command::Screenshot("world_view_final".into()))
        .await
        .unwrap();
    client.quit().await;
    let artifacts = default_run_base().join("../client-harness-artifacts");
    if std::fs::create_dir_all(&artifacts).is_ok()
        && let Ok(shots) = std::fs::read_dir(run.game_dir().join("screenshots"))
    {
        for shot in shots.flatten() {
            let file = shot.file_name();
            if file.to_string_lossy().starts_with("world_view") {
                let _ = std::fs::copy(shot.path(), artifacts.join(file));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&run.root);
}

async fn chat_seen(
    client: &ClientHandle,
    events: &mut tokio::sync::broadcast::Receiver<ClientEvent>,
    wanted: &str,
) {
    client
        .wait_event(
            events,
            Duration::from_secs(15),
            |e| matches!(e, ClientEvent::Chat(t) if t.contains(wanted)),
        )
        .await
        .unwrap_or_else(|e| panic!("chat line {wanted:?} never arrived: {e}"));
}

async fn held(srv: &ServerHandle, item: &str) -> String {
    server_says(srv, &format!("clear Menu minecraft:{item} 0")).await
}

async fn open_menu(client: &ClientHandle, srv: &ServerHandle, block: &str, screen: &str) {
    let name = "Menu";
    server_says(srv, &format!("execute at {name} run setblock ~ ~ ~2 stone")).await;
    server_says(
        srv,
        &format!("execute at {name} run setblock ~ ~1 ~2 {block} replace"),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    client
        .request(Command::Look {
            yaw: 0.0,
            pitch: 0.0,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    client.request(Command::Use).await.unwrap();
    let state = until(client, Duration::from_secs(10), "a menu screen", |s| {
        s.screen != "none"
    })
    .await;
    assert_eq!(state.screen, screen);
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_chat_and_menus() {
    if !enabled() {
        return;
    }
    let name = "real_client_chat_and_menus";
    let srv = start_server("127.0.0.1:0", "chatmenu").await;
    let addr = format!("127.0.0.1:{}", srv.game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username: "Menu",
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    let mut events = client.subscribe();
    until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    let h = &srv.handle;
    server_says(h, "op Menu").await;
    server_says(h, "gamemode survival Menu").await;

    server_says(
        h,
        r#"tellraw Menu {"translate":"death.attack.fall","with":["Menu"]}"#,
    )
    .await;
    chat_seen(&client, &mut events, "Menu hit the ground too hard").await;
    server_says(
        h,
        r#"tellraw Menu [{"text":"plain ","color":"red"},{"translate":"commands.give.success.single","with":["3","Stone","Menu"]}]"#,
    )
    .await;
    chat_seen(&client, &mut events, "plain Gave 3 Stone to Menu").await;
    client
        .request(Command::Command("time query gametime".into()))
        .await
        .unwrap();
    chat_seen(&client, &mut events, "The game time is").await;
    server_says(h, "kill Menu").await;
    chat_seen(&client, &mut events, "Menu was killed").await;
    client.request(Command::Respawn).await.unwrap();
    until(&client, Duration::from_secs(20), "respawn", |s| {
        s.health.unwrap_or(0.0) > 0.0 && s.screen == "none"
    })
    .await;
    server_says(h, "gamemode survival Menu").await;

    server_says(h, "execute at Menu run fill ~-1 ~2 ~1 ~1 ~4 ~3 air").await;
    open_menu(
        &client,
        h,
        r#"chest{Items:[{Slot:0b,id:"minecraft:stone",count:10},{Slot:1b,id:"minecraft:dirt",count:5},{Slot:2b,id:"minecraft:gravel",count:8},{Slot:3b,id:"minecraft:sand",count:8}]}"#,
        "ContainerScreen",
    )
    .await;
    let chest = "data get block ~ ~1 ~2 Items";
    let at = |c: &str| format!("execute at Menu run {c}");
    let click = |slot: i32, button: i32, mode: &str| {
        let req = client.request(Command::Click {
            slot,
            button,
            mode: mode.into(),
        });
        async move {
            let out = req.await;
            tokio::time::sleep(Duration::from_millis(250)).await;
            out
        }
    };
    click(0, 0, "pickup").await.unwrap();
    click(30, 0, "pickup").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let items = server_says(h, &at(chest)).await;
    assert!(
        !items.contains("minecraft:stone"),
        "stone left the chest: {items}"
    );
    let inv = held(h, "stone").await;
    assert!(inv.contains("Found 10"), "stone in player inventory: {inv}");
    click(1, 0, "quick_move").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let items = server_says(h, &at(chest)).await;
    assert!(!items.contains("minecraft:dirt"), "dirt moved: {items}");
    let inv = held(h, "dirt").await;
    assert!(inv.contains("Found 5"), "dirt in inventory: {inv}");
    click(2, 0, "pickup").await.unwrap();
    click(-999, 0, "quick_craft").await.unwrap();
    click(31, 1, "quick_craft").await.unwrap();
    click(32, 1, "quick_craft").await.unwrap();
    click(-999, 2, "quick_craft").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let inv = held(h, "gravel").await;
    assert!(inv.contains("Found 8"), "gravel dragged: {inv}");
    let items = server_says(h, &at(chest)).await;
    assert!(!items.contains("gravel"), "gravel left the chest: {items}");
    server_says(h, &at("setblock ~ ~1 ~2 air")).await;
    until(
        &client,
        Duration::from_secs(10),
        "chest screen closed",
        |s| s.screen == "none",
    )
    .await;

    for (block, screen) in [
        ("furnace", "FurnaceScreen"),
        ("crafting_table", "CraftingScreen"),
        ("anvil", "AnvilScreen"),
    ] {
        open_menu(&client, h, block, screen).await;
        client.request(Command::Close).await.unwrap();
        until(&client, Duration::from_secs(10), "screen closed", |s| {
            s.screen == "none"
        })
        .await;
        server_says(h, &at("setblock ~ ~1 ~2 air")).await;
    }
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
}

type SharedSlot = Arc<Mutex<Option<Arc<Shared>>>>;

struct CaptureShared(SharedSlot);

impl Feature for CaptureShared {
    fn id(&self) -> &'static str {
        "test-capture-shared"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries()
                .services
                .push(Arc::new(CaptureShared(self.0.clone())));
        }
        Ok(())
    }
}

impl ServiceFactory for CaptureShared {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(CaptureShared(self.0.clone()))
    }
}

impl WorldService for CaptureShared {
    fn start(&self, shared: &Arc<Shared>) {
        *self.0.lock().unwrap() = Some(shared.clone());
    }
}

async fn set_property(shared: &Shared, key: &str, value: &str) {
    let (reply, rx) = tokio::sync::oneshot::channel();
    shared
        .coordinator
        .send(CoordinatorMessage::ServerProperties {
            change: Some((key.into(), value.into())),
            reply,
        })
        .await
        .unwrap();
    rx.await.unwrap();
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_hardcore_toggle() {
    if !enabled() {
        return;
    }
    let name = "real_client_hardcore_toggle";
    let slot = SharedSlot::default();
    let instance = std::env::temp_dir().join(format!("tuff-client-hc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&instance);
    std::fs::create_dir_all(&instance).unwrap();
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(CaptureShared(slot.clone())));
    let server = Server::builder()
        .features(features)
        .instance_dir(instance.clone())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(4)
        .seed(1)
        .build();
    let handle = server.handle();
    tokio::spawn(server.run());
    let game = handle.local_addrs().await.unwrap()[0];
    let shared = slot.lock().unwrap().clone().expect("service started");
    let addr = format!("127.0.0.1:{}", game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username: "Menu",
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    server_says(&handle, "gamemode survival Menu").await;
    server_says(&handle, "give Menu minecraft:cobblestone 5").await;
    let before = until(&client, Duration::from_secs(10), "normal world", |s| {
        s.hardcore == Some(false)
    })
    .await;
    let chunks = before.chunks.unwrap();

    for wanted in [true, false] {
        set_property(&shared, "hardcore", if wanted { "true" } else { "false" }).await;
        let state = until(&client, Duration::from_secs(20), "hardcore flag", |s| {
            s.connected && s.hardcore == Some(wanted)
        })
        .await;
        assert_eq!(state.dimension, before.dimension);
        assert_eq!(state.game_mode.as_deref(), Some("survival"));
        assert!(state.health.unwrap_or(0.0) > 0.0);
        until(&client, Duration::from_secs(20), "chunks kept", |s| {
            s.chunks.unwrap_or(0) + 4 >= chunks
        })
        .await;
        assert!(!client.has_exited().await, "{}", client.log_tail());
        let items = held(&handle, "cobblestone").await;
        assert!(items.contains('5'), "inventory kept: {items}");
    }
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
    let _ = std::fs::remove_dir_all(&instance);
}

struct VoidGenerator;

impl ChunkGenerator for VoidGenerator {
    fn generate(&self, _pos: ChunkPos, shape: ChunkShape) -> Chunk {
        Chunk::empty(shape)
    }
    fn sea_level(&self) -> i32 {
        0
    }
    fn spawn(&self) -> tuff_world_model::BlockPos {
        tuff_world_model::BlockPos::new(0, 64, 0)
    }
}

struct TufferLike;

impl DimensionProvider for TufferLike {
    fn dimensions(&self) -> Vec<DimensionDefinition> {
        vec![DimensionDefinition {
            key: "tuffer:tuffer".into(),
            dimension_type: "tuffer:tuffer_type".into(),
            type_json: include_str!("fixtures/tuffer_type.json").into(),
            biomes: vec![BiomeDefinition {
                key: "tuffer:islands".into(),
                json: r##"{"has_precipitation": false, "temperature": 0.4, "downfall": 0.0,
                    "effects": {"water_color": "#4f7fd8"}, "carvers": [], "features": []}"##
                    .into(),
            }],
            natural_spawning: false,
            claims_spawn: false,
        }]
    }
    fn generator(&self, _: &str, _: i64, _: &Arc<Biomes>) -> Arc<dyn ChunkGenerator> {
        Arc::new(VoidGenerator)
    }
}

struct TufferFeature;

impl Feature for TufferFeature {
    fn id(&self) -> &'static str {
        "test-tuffer-dimension"
    }
    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            cx.registries()
                .dimension_providers
                .push(Arc::new(TufferLike));
        }
        Ok(())
    }
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_custom_dimension_type() {
    if !enabled() {
        return;
    }
    let name = "real_client_custom_dimension_type";
    let instance = std::env::temp_dir().join(format!("tuff-client-dim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&instance);
    std::fs::create_dir_all(&instance).unwrap();
    let mut features = tuff::vanilla_features::all();
    features.push(Box::new(TufferFeature));
    let server = Server::builder()
        .features(features)
        .instance_dir(instance.clone())
        .binds(vec!["127.0.0.1:0".into()])
        .online_mode(false)
        .whitelist(false)
        .view_distance(4)
        .seed(1)
        .build();
    let handle = server.handle();
    tokio::spawn(server.run());
    let game = handle.local_addrs().await.unwrap()[0];
    let addr = format!("127.0.0.1:{}", game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username: "Dimmer",
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    assert!(!client.has_exited().await, "{}", client.log_tail());
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
    let _ = std::fs::remove_dir_all(&instance);
}

struct CorpusLane {
    name: &'static str,
    keys: &'static [&'static str],
    ticks: u32,
    start_dy: i32,
    fills: &'static [([i32; 3], [i32; 3], &'static str)],
}

const CORPUS_LANES: &[CorpusLane] = &[
    CorpusLane {
        name: "walk",
        keys: &["forward"],
        ticks: 80,
        start_dy: 1,
        fills: &[([-3, 0, 0], [40, 0, 0], "minecraft:stone")],
    },
    CorpusLane {
        name: "sprint_jump",
        keys: &["forward", "sprint", "jump"],
        ticks: 100,
        start_dy: 1,
        fills: &[([-3, 0, 0], [40, 0, 0], "minecraft:stone")],
    },
    CorpusLane {
        name: "strafe_back",
        keys: &["back", "left"],
        ticks: 60,
        start_dy: 1,
        fills: &[([-3, 0, 0], [40, 0, 0], "minecraft:stone")],
    },
    CorpusLane {
        name: "sneak",
        keys: &["forward", "sneak"],
        ticks: 60,
        start_dy: 1,
        fills: &[([-3, 0, 0], [40, 0, 0], "minecraft:stone")],
    },
    CorpusLane {
        name: "stairs",
        keys: &["forward", "sprint"],
        ticks: 60,
        start_dy: 1,
        fills: &[
            ([-3, 0, 0], [40, 0, 0], "minecraft:stone"),
            ([5, 0, 0], [5, 0, 0], "minecraft:stone"),
            ([6, 0, 0], [6, 1, 0], "minecraft:stone"),
            ([7, 0, 0], [7, 2, 0], "minecraft:stone"),
            ([8, 0, 0], [8, 3, 0], "minecraft:stone"),
            ([9, 0, 0], [9, 4, 0], "minecraft:stone"),
            ([5, 1, 0], [5, 1, 0], "minecraft:stone_stairs[facing=east]"),
            ([6, 2, 0], [6, 2, 0], "minecraft:stone_stairs[facing=east]"),
            ([7, 3, 0], [7, 3, 0], "minecraft:stone_stairs[facing=east]"),
            ([8, 4, 0], [8, 4, 0], "minecraft:stone_stairs[facing=east]"),
            ([9, 5, 0], [9, 5, 0], "minecraft:stone_stairs[facing=east]"),
        ],
    },
    CorpusLane {
        name: "slabs",
        keys: &["forward"],
        ticks: 60,
        start_dy: 1,
        fills: &[
            ([-3, 0, 0], [40, 0, 0], "minecraft:stone"),
            ([5, 1, 0], [12, 1, 0], "minecraft:stone_slab[type=bottom]"),
        ],
    },
    CorpusLane {
        name: "ice",
        keys: &["forward", "sprint"],
        ticks: 60,
        start_dy: 1,
        fills: &[
            ([-3, 0, 0], [40, 0, 0], "minecraft:stone"),
            ([3, 0, 0], [30, 0, 0], "minecraft:packed_ice"),
        ],
    },
    CorpusLane {
        name: "soul_sand",
        keys: &["forward", "sprint"],
        ticks: 60,
        start_dy: 1,
        fills: &[
            ([-3, 0, 0], [40, 0, 0], "minecraft:stone"),
            ([3, 0, 0], [20, 0, 0], "minecraft:soul_sand"),
        ],
    },
    CorpusLane {
        name: "swim",
        keys: &["forward", "sprint", "jump"],
        ticks: 80,
        start_dy: 1,
        fills: &[
            ([-3, 0, 0], [40, 0, 0], "minecraft:stone"),
            ([3, 1, 0], [25, 3, 0], "minecraft:water"),
        ],
    },
    CorpusLane {
        name: "ladder",
        keys: &["forward"],
        ticks: 60,
        start_dy: 1,
        fills: &[
            ([-3, 0, 0], [40, 0, 0], "minecraft:stone"),
            ([3, 1, 0], [3, 10, 0], "minecraft:stone"),
            ([2, 1, 0], [2, 10, 0], "minecraft:ladder[facing=west]"),
        ],
    },
    CorpusLane {
        name: "slime",
        keys: &["forward", "sprint", "jump"],
        ticks: 80,
        start_dy: 1,
        fills: &[
            ([-3, 0, 0], [40, 0, 0], "minecraft:stone"),
            ([3, 0, 0], [30, 0, 0], "minecraft:slime_block"),
        ],
    },
    CorpusLane {
        name: "honey",
        keys: &["forward", "sprint", "jump"],
        ticks: 80,
        start_dy: 1,
        fills: &[
            ([-3, 0, 0], [40, 0, 0], "minecraft:stone"),
            ([3, 0, 0], [30, 0, 0], "minecraft:honey_block"),
        ],
    },
    CorpusLane {
        name: "cobweb",
        keys: &["forward", "sprint"],
        ticks: 60,
        start_dy: 1,
        fills: &[
            ([-3, 0, 0], [40, 0, 0], "minecraft:stone"),
            ([4, 1, 0], [8, 2, 0], "minecraft:cobweb"),
        ],
    },
    CorpusLane {
        name: "sneak_edge",
        keys: &["forward", "sneak"],
        ticks: 80,
        start_dy: 1,
        fills: &[
            ([-3, 0, 0], [40, 0, 0], "minecraft:stone"),
            ([4, 0, 0], [40, 0, 0], "minecraft:air"),
        ],
    },
    CorpusLane {
        name: "powder_snow",
        keys: &["forward"],
        ticks: 60,
        start_dy: 1,
        fills: &[
            ([-3, 0, 0], [40, 0, 0], "minecraft:stone"),
            ([4, 1, 0], [12, 1, 0], "minecraft:powder_snow"),
        ],
    },
    CorpusLane {
        name: "hay_fall",
        keys: &[],
        ticks: 60,
        start_dy: 12,
        fills: &[([0, 0, 0], [0, 0, 0], "minecraft:hay_block")],
    },
];

#[tuff_core::watchdog]
fn corpus_rtt_ms() -> u64 {
    std::env::var("TUFF_CORPUS_RTT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

fn corpus_file(stem: &str, rtt: u64) -> String {
    if rtt == 0 {
        format!("{stem}.txt")
    } else {
        format!("{stem}_rtt{rtt}.txt")
    }
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_record_movement_corpus() {
    use tuff_core::protocol::play::Serverbound;
    use tuff_replay::corpus::{Fill, Lane as Recorded};
    use tuff_trace::ConnState;
    use tuff_trace::Direction;

    if !enabled() {
        return;
    }
    let name = "real_client_record_corpus";
    let srv = start_server("127.0.0.1:0", "corpus").await;
    let rtt = corpus_rtt_ms();
    let proxy = tuff_client_harness::latency::LatencyProxy::start(
        std::net::SocketAddr::from(([127, 0, 0, 1], srv.game.port())),
        std::time::Duration::from_millis(rtt),
    )
    .await
    .unwrap();
    let addr = proxy.addr().to_string();
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let username = "Corp";
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username,
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    let state = until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    let s = &srv.handle;
    server_says(s, &format!("gamemode survival {username}")).await;
    let [px, py, pz] = state.position.unwrap();
    let (x0, fy, z0) = (px.floor() as i32, py.floor() as i32 + 40, pz.floor() as i32);
    let out_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/world/fixtures");
    let mut text = String::new();
    for (i, spec) in CORPUS_LANES.iter().enumerate() {
        let z = z0 + 5 * i as i32;
        let at = |p: [i32; 3]| (x0 + p[0], fy + p[1], z + p[2]);
        let (ax, ay, az) = at([-3, 1, 0]);
        server_says(
            s,
            &format!(
                "fill {ax} {ay} {az} {} {} {az} minecraft:air",
                x0 + 40,
                fy + 12
            ),
        )
        .await;
        let mut lane = Recorded {
            name: spec.name.to_owned(),
            spawn: Some([
                f64::from(x0) + 0.5,
                f64::from(fy + spec.start_dy),
                f64::from(z) + 0.5,
            ]),
            ..Recorded::default()
        };
        for (from, to, block) in spec.fills {
            let (a, b, c) = at(*from);
            let (d, e, f) = at(*to);
            server_says(s, &format!("fill {a} {b} {c} {d} {e} {f} {block}")).await;
            lane.fills.push(Fill {
                from: *from,
                to: *to,
                state: (*block).to_owned(),
            });
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        server_says(
            s,
            &format!("tp {username} {x0}.5 {} {z}.5 -90 0", fy + spec.start_dy),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        client
            .request(Command::Look {
                yaw: -90.0,
                pitch: 0.0,
            })
            .await
            .unwrap();
        client.request(Command::WaitTicks(5)).await.unwrap();
        let started = server_says(s, &format!("tuff tracing {username} start")).await;
        let path = started.rsplit_once(": ").unwrap().1.trim().to_owned();
        drive(
            &client,
            &Lane {
                name: spec.name,
                keys: spec.keys,
                ticks: spec.ticks,
                forward_only: false,
            },
        )
        .await;
        client.request(Command::WaitTicks(10)).await.unwrap();
        server_says(s, &format!("tuff tracing {username} stop")).await;
        let recording =
            tuff_trace::format::read_all(std::fs::File::open(&path).unwrap(), 1 << 30).unwrap();
        for r in &recording.records {
            if r.tag.is_event()
                || r.tag.state() != ConnState::Play
                || r.tag.direction() != Direction::Serverbound
            {
                continue;
            }
            if let Ok(packet) = Serverbound::decode(r.payload.clone()) {
                lane.packets.push(packet);
            }
        }
        let report = server_says(s, &format!("anticheat violations {username}")).await;
        assert!(
            report.contains("no anti-cheat violations"),
            "{}: {report}",
            spec.name
        );
        text.push_str(&lane.render([f64::from(x0), f64::from(fy), f64::from(z)]));
    }
    std::fs::write(out_dir.join(corpus_file("movement_corpus", rtt)), text).unwrap();
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
}

#[derive(Clone, Copy, PartialEq)]
enum SpecialKind {
    Elytra,
    Boat,
    Horse,
    Bridge {
        sneak: bool,
        jump: bool,
        pulse: bool,
    },
}

struct SpecialLane {
    name: &'static str,
    kind: SpecialKind,
    start_dy: i32,
    fills: &'static [([i32; 3], [i32; 3], &'static str)],
}

const SPECIAL_LANES: &[SpecialLane] = &[
    SpecialLane {
        name: "elytra_rockets",
        kind: SpecialKind::Elytra,
        start_dy: 30,
        fills: &[],
    },
    SpecialLane {
        name: "boat_water",
        kind: SpecialKind::Boat,
        start_dy: 1,
        fills: &[
            ([-3, 0, -2], [40, 0, 2], "minecraft:stone"),
            ([4, 0, 0], [40, 0, 0], "minecraft:water"),
        ],
    },
    SpecialLane {
        name: "horse_gallop",
        kind: SpecialKind::Horse,
        start_dy: 1,
        fills: &[([-3, 0, -2], [40, 0, 2], "minecraft:stone")],
    },
    SpecialLane {
        name: "bridge_sneak",
        kind: SpecialKind::Bridge {
            sneak: true,
            jump: false,
            pulse: false,
        },
        start_dy: 1,
        fills: &[([-3, 0, 0], [0, 0, 0], "minecraft:stone")],
    },
    SpecialLane {
        name: "bridge_jump",
        kind: SpecialKind::Bridge {
            sneak: false,
            jump: true,
            pulse: false,
        },
        start_dy: 1,
        fills: &[([-3, 0, 0], [0, 0, 0], "minecraft:stone")],
    },
    SpecialLane {
        name: "bridge_speed_pulse",
        kind: SpecialKind::Bridge {
            sneak: true,
            jump: false,
            pulse: true,
        },
        start_dy: 1,
        fills: &[([-3, 0, 0], [0, 0, 0], "minecraft:stone")],
    },
];

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_record_special_movement_corpus() {
    use tuff_core::protocol::play::Serverbound;
    use tuff_replay::corpus::{Fill, Lane as Recorded};
    use tuff_trace::ConnState;
    use tuff_trace::Direction;

    if !enabled() {
        return;
    }
    let name = "real_client_record_special_corpus";
    let srv = start_server("127.0.0.1:0", "special_corpus").await;
    let rtt = corpus_rtt_ms();
    let proxy = tuff_client_harness::latency::LatencyProxy::start(
        std::net::SocketAddr::from(([127, 0, 0, 1], srv.game.port())),
        std::time::Duration::from_millis(rtt),
    )
    .await
    .unwrap();
    let addr = proxy.addr().to_string();
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let username = "Spec";
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username,
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    let state = until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    let s = &srv.handle;
    server_says(s, &format!("gamemode survival {username}")).await;
    let [px, py, pz] = state.position.unwrap();
    let (x0, fy, z0) = (px.floor() as i32, py.floor() as i32 + 40, pz.floor() as i32);
    let out_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/world/fixtures");
    let mut text = String::new();
    let mut notes = Vec::new();
    for (i, spec) in SPECIAL_LANES.iter().enumerate() {
        let z = z0 + 5 * i as i32;
        let at = |p: [i32; 3]| (x0 + p[0], fy + p[1], z + p[2]);
        let (ax, ay, az) = at([-3, -1, -2]);
        server_says(
            s,
            &format!(
                "fill {ax} {ay} {az} {} {} {} minecraft:air",
                x0 + 60,
                fy + 12,
                z + 2
            ),
        )
        .await;
        let mut lane = Recorded {
            name: spec.name.to_owned(),
            spawn: Some([
                f64::from(x0) + 0.5,
                f64::from(fy + spec.start_dy),
                f64::from(z) + 0.5,
            ]),
            ..Recorded::default()
        };
        for (from, to, block) in spec.fills {
            let (a, b, c) = at(*from);
            let (d, e, f) = at(*to);
            server_says(s, &format!("fill {a} {b} {c} {d} {e} {f} {block}")).await;
            lane.fills.push(Fill {
                from: *from,
                to: *to,
                state: (*block).to_owned(),
            });
        }
        server_says(s, &format!("clear {username}")).await;
        let yaw = if matches!(spec.kind, SpecialKind::Bridge { .. }) {
            90.0
        } else {
            -90.0
        };
        let pitch = match spec.kind {
            SpecialKind::Bridge { .. } => 80.0,
            SpecialKind::Elytra => -10.0,
            _ => 30.0,
        };
        match spec.kind {
            SpecialKind::Elytra => {
                server_says(
                    s,
                    &format!("item replace entity {username} armor.chest with minecraft:elytra"),
                )
                .await;
                server_says(
                    s,
                    &format!(
                        "give {username} minecraft:firework_rocket[fireworks={{flight_duration:3}}] 16"
                    ),
                )
                .await;
            }
            SpecialKind::Bridge { .. } => {
                server_says(s, &format!("give {username} minecraft:stone 64")).await;
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        server_says(
            s,
            &format!(
                "tp {username} {x0}.5 {} {z}.5 {yaw} {pitch}",
                fy + spec.start_dy
            ),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        client.request(Command::Look { yaw, pitch }).await.unwrap();
        client.request(Command::Hotbar(1)).await.unwrap();
        match spec.kind {
            SpecialKind::Boat => {
                server_says(
                    s,
                    &format!(
                        "summon minecraft:oak_boat {x0}.5 {} {z}.5",
                        fy + spec.start_dy
                    ),
                )
                .await;
            }
            SpecialKind::Horse => {
                server_says(
                    s,
                    &format!(
                        "summon minecraft:horse {x0}.5 {} {z}.5 {{Tame:1b,SaddleItem:{{id:\"minecraft:saddle\",count:1}}}}",
                        fy + spec.start_dy
                    ),
                )
                .await;
            }
            _ => {}
        }
        client.request(Command::WaitTicks(10)).await.unwrap();
        let started = server_says(s, &format!("tuff tracing {username} start")).await;
        let path = started.rsplit_once(": ").unwrap().1.trim().to_owned();
        let key = |name: &'static str, down: bool| {
            client.request(Command::Key {
                name: name.into(),
                down,
            })
        };
        match spec.kind {
            SpecialKind::Elytra => {
                for _ in 0..2 {
                    key("jump", true).await.unwrap();
                    client.request(Command::WaitTicks(2)).await.unwrap();
                    key("jump", false).await.unwrap();
                    client.request(Command::WaitTicks(2)).await.unwrap();
                }
                client.request(Command::WaitTicks(10)).await.unwrap();
                for _ in 0..4 {
                    client.request(Command::Use).await.unwrap();
                    client.request(Command::WaitTicks(25)).await.unwrap();
                }
            }
            SpecialKind::Boat | SpecialKind::Horse => {
                client.request(Command::Use).await.unwrap();
                client.request(Command::WaitTicks(10)).await.unwrap();
                client
                    .request(Command::Look {
                        yaw: -90.0,
                        pitch: 0.0,
                    })
                    .await
                    .unwrap();
                key("forward", true).await.unwrap();
                let horse = spec.kind == SpecialKind::Horse;
                if horse {
                    key("sprint", true).await.unwrap();
                }
                client.request(Command::WaitTicks(80)).await.unwrap();
                key("forward", false).await.unwrap();
                if horse {
                    key("sprint", false).await.unwrap();
                }
            }
            SpecialKind::Bridge { sneak, jump, pulse } => {
                key("back", true).await.unwrap();
                if sneak && !pulse {
                    key("sneak", true).await.unwrap();
                }
                if jump {
                    key("jump", true).await.unwrap();
                }
                for t in 0..120u32 {
                    if pulse {
                        key("sneak", t % 3 != 0).await.unwrap();
                    }
                    client.request(Command::WaitTicks(1)).await.unwrap();
                    if t % 4 == 0 {
                        client.request(Command::Use).await.unwrap();
                    }
                }
                for k in ["back", "sneak", "jump"] {
                    key(k, false).await.unwrap();
                }
            }
        }
        client.request(Command::WaitTicks(10)).await.unwrap();
        server_says(s, &format!("tuff tracing {username} stop")).await;
        let recording =
            tuff_trace::format::read_all(std::fs::File::open(&path).unwrap(), 1 << 30).unwrap();
        for r in &recording.records {
            if r.tag.is_event()
                || r.tag.state() != ConnState::Play
                || r.tag.direction() != Direction::Serverbound
            {
                continue;
            }
            if let Ok(packet) = Serverbound::decode(r.payload.clone()) {
                lane.packets.push(packet);
            }
        }
        let report = server_says(s, &format!("anticheat violations {username}")).await;
        if !report.contains("no anti-cheat violations") {
            notes.push(format!("{}: {report}", spec.name));
        }
        text.push_str(&lane.render([f64::from(x0), f64::from(fy), f64::from(z)]));
        let _ = server_says(s, &format!("ride {username} dismount")).await;
        server_says(s, "kill @e[type=!player]").await;
    }
    std::fs::write(
        out_dir.join(corpus_file("movement_corpus_special", rtt)),
        text,
    )
    .unwrap();
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
    assert!(notes.is_empty(), "lanes flagged: {notes:#?}");
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_ignores_host_input() {
    if !enabled() {
        return;
    }
    let name = "real_client_ignores_host_input";
    let srv = start_server("127.0.0.1:0", "hostinput").await;
    let addr = format!("127.0.0.1:{}", srv.game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username: "Numb",
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    client.request(Command::Hotbar(3)).await.unwrap();
    let handle = client.eval("mc.getWindow().handle()").await.unwrap();
    for _ in 0..3 {
        client
            .eval(&format!("mc.mouseHandler.onScroll({handle}, 0.0, -1.0)"))
            .await
            .unwrap();
    }
    client.request(Command::WaitTicks(2)).await.unwrap();
    assert_eq!(
        client
            .eval("mc.player.getInventory().getSelectedSlot()")
            .await
            .unwrap(),
        "2"
    );
    client.eval("mc.mouseHandler.grabMouse()").await.unwrap();
    assert_eq!(
        client
            .eval("mc.mouseHandler.isMouseGrabbed()")
            .await
            .unwrap(),
        "false"
    );
    client.request(Command::Hotbar(5)).await.unwrap();
    assert_eq!(
        client
            .eval("mc.player.getInventory().getSelectedSlot()")
            .await
            .unwrap(),
        "4"
    );
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
}

type SurfaceLane = (
    &'static str,
    &'static [&'static str],
    &'static [([i32; 3], [i32; 3], &'static str)],
);

struct AcSession {
    srv: Running,
    client: ClientHandle,
    run: RunDir,
    username: &'static str,
    origin: [i32; 3],
}

impl AcSession {
    async fn join(name: &str, username: &'static str) -> Self {
        let srv = start_server("127.0.0.1:0", name).await;
        let addr = format!("127.0.0.1:{}", srv.game.port());
        let platform = Platform::current();
        let cache = cache_dir();
        let files = tokio::task::spawn_blocking({
            let platform = platform.clone();
            let cache = cache.clone();
            move || {
                ensure_client(
                    &HttpSource::new()?,
                    &cache,
                    tuff_vanilla_data::version::NAME,
                    AssetProfile::from_env()?,
                    &platform,
                )
            }
        })
        .await
        .unwrap()
        .unwrap();
        let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
        let run = RunDir::create(&default_run_base(), name).unwrap();
        let client = launch(
            &LaunchSpec {
                java: &java,
                files: &files,
                run: &run,
                username,
                address: &addr,
                platform: &platform,
                hook_args: &[],
                hook_dir: None,
            },
            name,
            default_lock_path(),
        )
        .await
        .unwrap();
        let state = until(&client, JOIN_TIMEOUT, "join", |s| {
            s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
        })
        .await;
        server_says(&srv.handle, &format!("gamemode survival {username}")).await;
        let [px, py, pz] = state.position.unwrap();
        let origin = [px.floor() as i32, py.floor() as i32 + 40, pz.floor() as i32];
        Self {
            srv,
            client,
            run,
            username,
            origin,
        }
    }

    async fn say(&self, command: &str) -> String {
        server_says(&self.srv.handle, command).await
    }

    async fn fill(&self, from: [i32; 3], to: [i32; 3], block: &str) {
        let [x, y, z] = self.origin;
        self.say(&format!(
            "fill {} {} {} {} {} {} {block}",
            x + from[0],
            y + from[1],
            z + from[2],
            x + to[0],
            y + to[1],
            z + to[2]
        ))
        .await;
    }

    async fn clear_lane(&self, lane_z: i32) {
        self.fill([-3, 1, lane_z], [60, 14, lane_z], "minecraft:air")
            .await;
        self.fill([-3, 0, lane_z], [60, 0, lane_z], "minecraft:stone")
            .await;
    }

    async fn place_player(&self, dx: f64, dy: i32, lane_z: i32, yaw: f32, pitch: f32) {
        let [x, y, z] = self.origin;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        self.say(&format!(
            "tp {} {} {} {}.5 {yaw} {pitch}",
            self.username,
            f64::from(x) + 0.5 + dx,
            y + dy,
            z + lane_z
        ))
        .await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        self.client
            .request(Command::Look { yaw, pitch })
            .await
            .unwrap();
        self.client.request(Command::WaitTicks(5)).await.unwrap();
    }

    async fn key(&self, name: &str, down: bool) {
        self.client
            .request(Command::Key {
                name: name.into(),
                down,
            })
            .await
            .unwrap();
    }

    async fn assert_clean(&self, what: &str) {
        self.client.request(Command::WaitTicks(10)).await.unwrap();
        let report = self
            .say(&format!("anticheat violations {}", self.username))
            .await;
        assert!(
            report.contains("no anti-cheat violations"),
            "{what} flagged: {report}\n{}",
            self.client.log_tail()
        );
    }

    async fn finish(self) {
        self.client.quit().await;
        let _ = std::fs::remove_dir_all(&self.run.root);
    }
}

async fn watch_steps(client: &ClientHandle, what: &str, ticks: u32, max_step: f64) {
    let mut last = client.state().await.unwrap().position.unwrap();
    for _ in 0..ticks {
        client.request(Command::WaitTicks(1)).await.unwrap();
        let now = client.state().await.unwrap().position.unwrap();
        let d =
            ((now[0] - last[0]).powi(2) + (now[1] - last[1]).powi(2) + (now[2] - last[2]).powi(2))
                .sqrt();
        assert!(
            d < max_step,
            "{what}: moved {d} in one sample (setback?) {last:?} -> {now:?}\n{}",
            client.log_tail()
        );
        last = now;
    }
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_ac_predict_surfaces() {
    if !enabled() {
        return;
    }
    let s = AcSession::join("ac_surfaces", "Surf").await;
    let lanes: [SurfaceLane; 6] = [
        (
            "slime",
            &["forward", "sprint", "jump"],
            &[([3, 0, 0], [30, 0, 0], "minecraft:slime_block")],
        ),
        (
            "honey",
            &["forward", "sprint", "jump"],
            &[([3, 0, 0], [30, 0, 0], "minecraft:honey_block")],
        ),
        (
            "cobweb",
            &["forward", "sprint"],
            &[([4, 1, 0], [8, 2, 0], "minecraft:cobweb")],
        ),
        (
            "sneak_edge",
            &["forward", "sneak"],
            &[([4, 0, 0], [60, 0, 0], "minecraft:air")],
        ),
        (
            "powder_snow",
            &["forward"],
            &[([4, 1, 0], [12, 1, 0], "minecraft:powder_snow")],
        ),
        (
            "vines",
            &["forward", "jump"],
            &[
                ([6, 1, 0], [6, 20, 0], "minecraft:stone"),
                ([5, 1, 0], [5, 20, 0], "minecraft:vine[east=true]"),
            ],
        ),
    ];
    for (i, (name, keys, fills)) in lanes.iter().enumerate() {
        let lane_z = 5 * i as i32;
        s.clear_lane(lane_z).await;
        for (from, to, block) in *fills {
            s.fill(
                [from[0], from[1], from[2] + lane_z],
                [to[0], to[1], to[2] + lane_z],
                block,
            )
            .await;
        }
        s.place_player(0.0, 1, lane_z, -90.0, 0.0).await;
        for k in *keys {
            s.key(k, true).await;
        }
        watch_steps(&s.client, name, 80, 2.5).await;
        for k in *keys {
            s.key(k, false).await;
        }
        s.assert_clean(name).await;
    }

    s.clear_lane(35).await;
    s.place_player(0.0, 1, 35, -90.0, 0.0).await;
    let [x, y, z] = s.origin;
    s.say(&format!(
        "summon minecraft:tnt {x}.5 {} {}.5 {{fuse:1}}",
        y + 1,
        z + 35
    ))
    .await;
    s.client.request(Command::WaitTicks(40)).await.unwrap();
    s.assert_clean("knockback").await;

    s.finish().await;
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_ac_bridging() {
    if !enabled() {
        return;
    }
    let s = AcSession::join("ac_bridging", "Bridge").await;
    let modes: [(&str, bool, u32); 3] =
        [("sneak", true, 1), ("jump", false, 1), ("speed", false, 3)];
    for (i, (name, sneaking, pulse)) in modes.iter().enumerate() {
        let lane_z = 5 * i as i32;
        s.clear_lane(lane_z).await;
        s.fill([4, 0, lane_z], [60, 0, lane_z], "minecraft:air")
            .await;
        s.say(&format!("clear {}", s.username)).await;
        let g = s
            .say(&format!("give {} minecraft:stone 64", s.username))
            .await;
        drop(g);
        s.place_player(3.0, 1, lane_z, 90.0, 80.0).await;
        s.client.request(Command::Hotbar(1)).await.unwrap();
        s.key("back", true).await;
        if *sneaking {
            s.key("sneak", true).await;
        }
        if *name == "jump" {
            s.key("jump", true).await;
        }
        for t in 0..160u32 {
            if *name == "speed" {
                s.key("sneak", t % (pulse * 3) != 0).await;
            }
            if s.client.request(Command::WaitTicks(1)).await.is_err() {
                let report = s.say(&format!("anticheat violations {}", s.username)).await;
                panic!(
                    "{name}: client lost at tick {t}: {report}\n{}",
                    s.client.log_tail()
                );
            }
            let now = s.client.state().await.unwrap();
            let at = now.position.unwrap();
            assert!(
                !*sneaking || at[1] > f64::from(s.origin[1]) - 2.0,
                "{name}: fell at tick {t}: {at:?} yaw {:?} pitch {:?}",
                now.yaw,
                now.pitch
            );
            if t % 4 == 0 {
                s.client.request(Command::Use).await.unwrap();
            }
        }
        for k in ["back", "sneak", "jump"] {
            s.key(k, false).await;
        }
        let end = s.client.state().await.unwrap().position.unwrap();
        let seen = s.say(&format!("anticheat violations {}", s.username)).await;
        assert!(
            end[0] > f64::from(s.origin[0]) + if *sneaking { 4.5 } else { 3.0 }
                && (!*sneaking || end[1] > f64::from(s.origin[1]) - 1.0),
            "{name}: bridge did not advance, ended at {end:?} origin {:?} {seen}\n{}",
            s.origin,
            s.client.log_tail()
        );
        s.assert_clean(name).await;
    }
    s.finish().await;
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_ac_elytra_boost() {
    if !enabled() {
        return;
    }
    let s = AcSession::join("ac_elytra", "Wing").await;
    s.say(&format!(
        "item replace entity {} armor.chest with minecraft:elytra",
        s.username
    ))
    .await;
    s.place_player(0.0, 120, 0, -90.0, -10.0).await;
    s.key("jump", true).await;
    s.client.request(Command::WaitTicks(2)).await.unwrap();
    s.key("jump", false).await;
    s.client.request(Command::WaitTicks(2)).await.unwrap();
    s.key("jump", true).await;
    s.client.request(Command::WaitTicks(2)).await.unwrap();
    s.key("jump", false).await;
    s.client.request(Command::WaitTicks(10)).await.unwrap();
    let start = s.client.state().await.unwrap().position.unwrap();
    for (round, duration) in [3, 1, 2, 3, 1, 2].into_iter().enumerate() {
        s.say(&format!("clear {} minecraft:firework_rocket", s.username))
            .await;
        s.say(&format!(
            "give {} minecraft:firework_rocket[fireworks={{flight_duration:{duration}}}] 4",
            s.username
        ))
        .await;
        s.client.request(Command::Hotbar(1)).await.unwrap();
        let yaw = -90.0 + 40.0 * (round as f32 - 2.5);
        let pitch = if round % 2 == 0 { -35.0 } else { -5.0 };
        s.client
            .request(Command::Look { yaw, pitch })
            .await
            .unwrap();
        s.client.request(Command::Use).await.unwrap();
        watch_steps(&s.client, "elytra", 25, 8.0).await;
    }
    let end = s.client.state().await.unwrap().position.unwrap();
    assert!(
        (end[0] - start[0]).hypot(end[2] - start[2]) > 20.0,
        "elytra did not glide: {start:?} -> {end:?}\n{}",
        s.client.log_tail()
    );
    s.assert_clean("elytra").await;
    s.finish().await;
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_ac_vehicle_setback_echo() {
    if !enabled() {
        return;
    }
    let s = AcSession::join("ac_vehicles", "Rider").await;
    let [x, y, z] = s.origin;
    s.clear_lane(0).await;
    s.fill([2, 0, -2], [40, 0, 2], "minecraft:stone").await;
    s.fill([4, 0, 0], [40, 0, 0], "minecraft:water").await;
    s.place_player(0.0, 1, 0, -90.0, 30.0).await;
    s.say(&format!("summon minecraft:oak_boat {x}.5 {} {z}.5", y + 1))
        .await;
    s.client.request(Command::WaitTicks(10)).await.unwrap();
    s.client.request(Command::Use).await.unwrap();
    s.client.request(Command::WaitTicks(10)).await.unwrap();
    s.client
        .request(Command::Look {
            yaw: -90.0,
            pitch: 0.0,
        })
        .await
        .unwrap();
    s.key("forward", true).await;
    watch_steps(&s.client, "boat", 60, 2.5).await;
    s.key("forward", false).await;
    s.assert_clean("boat").await;

    s.clear_lane(10).await;
    s.say(&format!(
        "summon minecraft:horse {x}.5 {} {}.5 {{Tame:1b,SaddleItem:{{id:\"minecraft:saddle\",count:1}}}}",
        y + 1,
        z + 10
    ))
    .await;
    s.place_player(0.0, 1, 10, -90.0, 30.0).await;
    s.client.request(Command::WaitTicks(10)).await.unwrap();
    s.client.request(Command::Use).await.unwrap();
    s.client.request(Command::WaitTicks(10)).await.unwrap();
    s.client
        .request(Command::Look {
            yaw: -90.0,
            pitch: 0.0,
        })
        .await
        .unwrap();
    s.key("forward", true).await;
    s.key("sprint", true).await;
    watch_steps(&s.client, "horse", 60, 2.5).await;
    s.key("forward", false).await;
    s.key("sprint", false).await;
    s.assert_clean("horse").await;
    s.finish().await;
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_minecart_track_replay() {
    if !enabled() {
        return;
    }
    let name = "real_client_minecart_track_replay";
    let srv = start_server("127.0.0.1:0", "minecart").await;
    let addr = format!("127.0.0.1:{}", srv.game.port());
    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let username = "Rider";
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username,
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
    })
    .await;
    server_says(&srv.handle, &format!("op {username}")).await;
    server_says(&srv.handle, &format!("gamemode spectator {username}")).await;
    let at = |cmd: &str| format!("execute at {username} run {cmd}");
    server_says(&srv.handle, &at("fill ~2 ~5 ~3 ~30 ~5 ~3 minecraft:stone")).await;
    server_says(
        &srv.handle,
        &at("fill ~2 ~6 ~3 ~30 ~6 ~3 minecraft:rail[shape=east_west]"),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    server_says(
        &srv.handle,
        &at("summon minecraft:minecart ~3 ~6 ~3 {Motion:[0.4d,0d,0d]}"),
    )
    .await;
    let mut xs: Vec<f64> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        let list = client.entities(60.0).await.unwrap();
        if let Some(cart) = list.iter().find(|e| e.kind == "minecraft:minecart") {
            xs.push(cart.position[0]);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(xs.len() > 20, "minecart never seen by the client: {xs:?}");
    let first = xs[0];
    let last = *xs.last().unwrap();
    assert!(
        last - first > 3.0,
        "minecart did not travel: {first} -> {last}"
    );
    for pair in xs.windows(2) {
        assert!(
            pair[1] >= pair[0] - 0.01,
            "minecart jumped backwards: {xs:?}"
        );
        assert!(
            pair[1] - pair[0] < 1.5,
            "minecart teleported instead of gliding: {xs:?}"
        );
    }
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn real_client_accepts_resource_pack() {
    if !enabled() {
        return;
    }
    use tuff::protocol::pack::PackStatus;
    use tuff::resource_pack::{PackSource, ResourcePack};
    let name = "real_client_accepts_resource_pack";
    let srv = start_server("127.0.0.1:0", "pack").await;
    let addr = format!("127.0.0.1:{}", srv.game.port());
    let mcmeta = tuff::custom_pack::pack_mcmeta();
    let zip = tuff::custom_pack::zip::write([("pack.mcmeta", mcmeta.as_slice())], 1 << 20).unwrap();
    let packs = srv.handle.resource_packs().await.unwrap();
    let pack_id = 0x7e57_0000_0000_0000_0000_0000_0000_0606u128;
    packs
        .register(ResourcePack::new(pack_id, "harness", PackSource::Bytes(zip)).required(true))
        .await;
    let mut pack_events = packs.subscribe();

    let platform = Platform::current();
    let cache = cache_dir();
    let files = tokio::task::spawn_blocking({
        let platform = platform.clone();
        let cache = cache.clone();
        move || {
            ensure_client(
                &HttpSource::new()?,
                &cache,
                tuff_vanilla_data::version::NAME,
                AssetProfile::from_env()?,
                &platform,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let java = resolve_java(&cache, &files.meta.java_version.component).unwrap();
    let run = RunDir::create(&default_run_base(), name).unwrap();
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username: "PackUser",
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        name,
        default_lock_path(),
    )
    .await
    .unwrap();
    until(&client, JOIN_TIMEOUT, "join", |s| {
        s.connected && s.screen == "none"
    })
    .await;

    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !seen.contains(&PackStatus::Loaded) {
        assert!(
            Instant::now() < deadline,
            "pack never loaded, statuses {seen:?}\n{}",
            client.log_tail()
        );
        if let Ok(Ok(event)) =
            tokio::time::timeout(Duration::from_millis(200), pack_events.recv()).await
            && event.pack == pack_id
        {
            seen.push(event.status);
        }
    }
    assert!(seen.contains(&PackStatus::Accepted), "{seen:?}");
    assert!(!seen.iter().any(|s| s.is_failure()), "{seen:?}");
    client.quit().await;
    let _ = std::fs::remove_dir_all(&run.root);
}
