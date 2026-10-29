//! Opt-in real-client parity suite: scripted inputs from the vanilla client are
//! checked against the server's own anticheat verdict, and per-tick client
//! traces are written under cargo's test tmp dir (`parity/`) for calibration. Ignored, and
//! also needs `TUFF_CLIENT_TESTS=1` and `TUFF_ACCEPT_MOJANG_EULA=1`.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use tuff::{Server, ServerHandle};
use tuff_client_harness::launch::{LaunchSpec, RunDir, default_run_base};
use tuff_client_harness::lock::default_lock_path;
use tuff_client_harness::log::ClientEvent;
use tuff_client_harness::meta::Platform;
use tuff_client_harness::protocol::{ClientState, Command};
use tuff_client_harness::supervisor::{ClientHandle, JOIN_TIMEOUT, launch};
use tuff_client_harness::{AssetProfile, HttpSource, cache_dir, ensure_client, resolve_java};

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

fn enabled() -> bool {
    if log::set_logger(&StderrLogger).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
    if std::env::var("TUFF_CLIENT_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set TUFF_CLIENT_TESTS=1 to launch a real vanilla client");
        return false;
    }
    assert!(
        matches!(
            std::env::var("TUFF_ACCEPT_MOJANG_EULA").as_deref(),
            Ok("1") | Ok("true") | Ok("yes")
        ),
        "set TUFF_ACCEPT_MOJANG_EULA=1 after reading https://aka.ms/MinecraftEULA"
    );
    true
}

async fn says(handle: &ServerHandle, command: &str) -> String {
    let lines = handle.run_command(command).await.unwrap();
    lines
        .iter()
        .map(|t| t.plain())
        .collect::<Vec<_>>()
        .join(" ")
}

async fn until(
    client: &ClientHandle,
    what: &str,
    pred: impl Fn(&ClientState) -> bool,
) -> ClientState {
    let deadline = Instant::now() + JOIN_TIMEOUT;
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

struct Rig {
    srv: ServerHandle,
    client: ClientHandle,
    events: tokio::sync::broadcast::Receiver<ClientEvent>,
    run: RunDir,
    user: &'static str,
    origin: [i32; 3],
    trace: String,
}

impl Rig {
    async fn start(name: &'static str, user: &'static str) -> Rig {
        let instance =
            std::env::temp_dir().join(format!("tuff-parity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&instance);
        std::fs::create_dir_all(&instance).unwrap();
        let server = Server::builder()
            .features(tuff::vanilla_features::all())
            .instance_dir(instance)
            .binds(vec!["127.0.0.1:0".into()])
            .online_mode(false)
            .whitelist(false)
            .view_distance(tuff_client_harness::launch::parity_view_distance().unwrap_or(4))
            .seed(1)
            .build();
        let srv = server.handle();
        tokio::spawn(server.run());
        let game = srv.local_addrs().await.unwrap()[0];
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
                username: user,
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
        let events = client.subscribe();
        let st = until(&client, "join", |s| {
            s.connected && s.screen == "none" && s.chunks.unwrap_or(0) > 4
        })
        .await;
        says(&srv, &format!("gamemode survival {user}")).await;
        let [x, y, z] = st.position.unwrap();
        Rig {
            srv,
            client,
            events,
            run,
            user,
            origin: [x.floor() as i32, y.floor() as i32 + 40, z.floor() as i32],
            trace: String::from("scenario,tick,x,y,z,yaw,pitch,health\n"),
        }
    }

    async fn cmd(&self, c: &str) -> String {
        says(&self.srv, c).await
    }

    async fn req(&self, c: Command) {
        self.client.request(c).await.unwrap();
    }

    async fn keys(&self, names: &[&str], down: bool) {
        for n in names {
            self.req(Command::Key {
                name: (*n).into(),
                down,
            })
            .await;
        }
    }

    async fn platform(&mut self, z_off: i32) -> [i32; 3] {
        let [x, y, z] = self.origin;
        let z = z + z_off;
        self.cmd(&format!(
            "fill {} {} {z} {} {} {z} minecraft:air",
            x - 6,
            y + 1,
            x + 30,
            y + 12
        ))
        .await;
        self.cmd(&format!(
            "fill {} {y} {z} {} {y} {z} minecraft:stone",
            x - 6,
            x + 30
        ))
        .await;
        [x, y, z]
    }

    async fn place_player(&self, at: [i32; 3], yaw: f32, pitch: f32) {
        self.cmd(&format!(
            "tp {} {}.5 {} {}.5 {yaw} {pitch}",
            self.user,
            at[0],
            at[1] + 1,
            at[2]
        ))
        .await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        self.req(Command::Look { yaw, pitch }).await;
        self.req(Command::WaitTicks(5)).await;
    }

    async fn record(&mut self, scenario: &str, ticks: u32) {
        for _ in 0..ticks {
            self.req(Command::WaitTicks(1)).await;
            self.sample(scenario).await;
        }
    }

    async fn sample(&mut self, scenario: &str) {
        let st = self.client.state().await.unwrap();
        assert!(
            st.connected,
            "{scenario}: client dropped\n{}",
            self.client.log_tail()
        );
        let p = st.position.unwrap_or_default();
        let _ = writeln!(
            self.trace,
            "{scenario},{},{:.4},{:.4},{:.4},{:.2},{:.2},{}",
            st.tick.unwrap_or(0),
            p[0],
            p[1],
            p[2],
            st.yaw.unwrap_or(0.0),
            st.pitch.unwrap_or(0.0),
            st.health.unwrap_or(0.0)
        );
    }

    async fn verdict(&mut self, scenario: &str) {
        self.req(Command::WaitTicks(10)).await;
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                ClientEvent::ProtocolError(l) => panic!("{scenario}: protocol error {l}"),
                ClientEvent::Disconnected(w) => panic!("{scenario}: disconnected {w}"),
                _ => {}
            }
        }
        let report = self
            .cmd(&format!("anticheat violations {}", self.user))
            .await;
        assert!(
            report.contains("no anti-cheat violations"),
            "{scenario} flagged: {report}\n{}",
            self.client.log_tail()
        );
    }

    async fn finish(self, file: &str) {
        let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("parity");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(file), &self.trace);
        self.client.quit().await;
        let _ = std::fs::remove_dir_all(&self.run.root);
    }
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn parity_movement_surfaces() {
    if !enabled() {
        return;
    }
    let mut rig = Rig::start("parity_movement", "ParityMove").await;
    let surfaces: [(&str, &str, &[&str], u32); 4] = [
        ("sneak", "minecraft:stone", &["forward", "sneak"], 60),
        (
            "soul_sand",
            "minecraft:soul_sand",
            &["forward", "sprint"],
            60,
        ),
        (
            "honey",
            "minecraft:honey_block",
            &["forward", "sprint", "jump"],
            60,
        ),
        ("slime", "minecraft:slime_block", &["forward", "sprint"], 60),
    ];
    for (i, (name, block, keys, ticks)) in surfaces.iter().enumerate() {
        let [x, y, z] = rig.platform(5 * i as i32).await;
        rig.cmd(&format!(
            "fill {} {y} {z} {} {y} {z} {block}",
            x + 2,
            x + 28
        ))
        .await;
        tokio::time::sleep(Duration::from_millis(1200)).await;
        rig.place_player([x, y, z], -90.0, 0.0).await;
        rig.keys(keys, true).await;
        rig.record(name, *ticks).await;
        rig.keys(keys, false).await;
        rig.verdict(name).await;
    }
    rig.finish("movement_surfaces.csv").await;
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn parity_blocks_and_combat() {
    if !enabled() {
        return;
    }
    let mut rig = Rig::start("parity_blocks", "ParityBlock").await;
    let [x, y, z] = rig.platform(0).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let user = rig.user;
    rig.cmd(&format!("give {user} minecraft:cobblestone 64"))
        .await;
    rig.cmd(&format!("give {user} minecraft:diamond_pickaxe"))
        .await;
    rig.cmd(&format!("give {user} minecraft:iron_sword")).await;

    rig.place_player([x, y, z], 0.0, 80.0).await;
    rig.req(Command::Hotbar(1)).await;
    rig.keys(&["sneak", "back"], true).await;
    for _ in 0..40 {
        rig.req(Command::Use).await;
        rig.req(Command::WaitTicks(2)).await;
        rig.sample("bridge").await;
    }
    rig.keys(&["sneak", "back"], false).await;
    rig.verdict("bridge").await;

    rig.place_player([x, y, z], 0.0, 40.0).await;
    rig.cmd(&format!(
        "fill {x} {} {} {} {} {} minecraft:stone",
        y + 1,
        z + 1,
        x + 3,
        y + 1,
        z + 1
    ))
    .await;
    rig.req(Command::Hotbar(2)).await;
    rig.keys(&["attack"], true).await;
    rig.record("break", 80).await;
    rig.keys(&["attack"], false).await;
    rig.verdict("break").await;

    rig.cmd(&format!(
        "summon minecraft:zombie {x}.5 {} {}.5 {{NoAI:1b}}",
        y + 1,
        z + 2
    ))
    .await;
    rig.place_player([x, y, z], 0.0, 0.0).await;
    rig.req(Command::Hotbar(3)).await;
    for _ in 0..30 {
        rig.req(Command::Attack).await;
        rig.req(Command::WaitTicks(12)).await;
        rig.sample("combat").await;
    }
    rig.verdict("combat").await;
    rig.finish("blocks_combat.csv").await;
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn parity_inventory_clicks() {
    if !enabled() {
        return;
    }
    let mut rig = Rig::start("parity_inventory", "ParityInv").await;
    let user = rig.user;
    rig.cmd(&format!("give {user} minecraft:cobblestone 64"))
        .await;
    rig.cmd(&format!("give {user} minecraft:stick 64")).await;
    for round in 0..20 {
        for (slot, mode) in [
            (9, "pickup"),
            (10, "pickup"),
            (9, "quick_move"),
            (36, "swap"),
        ] {
            rig.req(Command::Click {
                slot,
                button: if mode == "swap" { 1 } else { 0 },
                mode: mode.into(),
            })
            .await;
        }
        if round % 5 == 0 {
            rig.sample("clicks").await;
        }
    }
    rig.verdict("clicks").await;
    rig.finish("inventory_clicks.csv").await;
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn parity_ice_ladder_liquid_knockback() {
    if !enabled() {
        return;
    }
    let mut rig = Rig::start("parity_surfaces2", "ParitySurf").await;
    let mut row = 0;
    for (name, block, keys) in [
        ("ice", "minecraft:ice", &["forward", "sprint"][..]),
        (
            "packed_ice_jump",
            "minecraft:packed_ice",
            &["forward", "sprint", "jump"][..],
        ),
        ("water", "minecraft:water", &["forward", "sprint"][..]),
        (
            "water_jump",
            "minecraft:water",
            &["forward", "sprint", "jump"][..],
        ),
    ] {
        let [x, y, z] = rig.platform(5 * row).await;
        row += 1;
        if block == "minecraft:water" {
            rig.cmd(&format!(
                "fill {} {} {z} {} {} {z} minecraft:water",
                x + 2,
                y + 1,
                x + 28,
                y + 3
            ))
            .await;
        } else {
            rig.cmd(&format!(
                "fill {} {y} {z} {} {y} {z} {block}",
                x + 2,
                x + 28
            ))
            .await;
        }
        tokio::time::sleep(Duration::from_millis(1200)).await;
        rig.place_player([x, y, z], -90.0, 0.0).await;
        rig.keys(keys, true).await;
        rig.record(name, 60).await;
        rig.keys(keys, false).await;
        rig.record(name, 30).await;
        rig.verdict(name).await;
    }

    let [x, y, z] = rig.platform(5 * row).await;
    row += 1;
    rig.cmd(&format!(
        "fill {x} {} {} {x} {} {} minecraft:stone",
        y + 1,
        z + 1,
        y + 12,
        z + 1
    ))
    .await;
    rig.cmd(&format!(
        "fill {x} {} {z} {x} {} {z} minecraft:ladder[facing=north]",
        y + 1,
        y + 12
    ))
    .await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.place_player([x, y, z], 180.0, 0.0).await;
    rig.keys(&["forward", "jump"], true).await;
    rig.record("ladder_up", 60).await;
    rig.keys(&["jump"], false).await;
    rig.keys(&["sneak"], true).await;
    rig.record("ladder_hold", 30).await;
    rig.keys(&["forward", "sneak"], false).await;
    rig.verdict("ladder").await;

    let [x, y, z] = rig.platform(5 * row).await;
    rig.place_player([x + 20, y, z], -90.0, 0.0).await;
    rig.keys(&["forward", "sneak"], true).await;
    rig.record("sneak_edge", 80).await;
    rig.keys(&["forward", "sneak"], false).await;
    rig.verdict("sneak_edge").await;

    let [x, y, z] = rig.platform(5 * (row + 1)).await;
    rig.place_player([x, y, z], -90.0, 0.0).await;
    for _ in 0..4 {
        rig.cmd(&format!(
            "summon minecraft:tnt {x}.5 {} {}.5 {{fuse:1}}",
            y + 1,
            z + 2
        ))
        .await;
        rig.record("knockback", 40).await;
        rig.place_player([x, y, z], -90.0, 0.0).await;
    }
    rig.verdict("knockback").await;
    rig.finish("surfaces2.csv").await;
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn parity_bridging_styles() {
    if !enabled() {
        return;
    }
    let mut rig = Rig::start("parity_bridging", "ParityBridge").await;
    let [x, y, z] = rig.platform(0).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let user = rig.user;
    rig.cmd(&format!("give {user} minecraft:cobblestone 64"))
        .await;
    rig.req(Command::Hotbar(1)).await;

    rig.place_player([x, y, z], 0.0, 80.0).await;
    rig.keys(&["back"], true).await;
    for i in 0..60 {
        rig.keys(&["sneak"], i % 6 < 3).await;
        rig.req(Command::Use).await;
        rig.req(Command::WaitTicks(1)).await;
        rig.sample("speed_bridge").await;
    }
    rig.keys(&["back", "sneak"], false).await;
    rig.verdict("speed_bridge").await;

    rig.place_player([x + 10, y, z], -90.0, 70.0).await;
    rig.keys(&["forward", "sprint", "jump"], true).await;
    for _ in 0..40 {
        rig.req(Command::Use).await;
        rig.req(Command::WaitTicks(2)).await;
        rig.sample("telly").await;
    }
    rig.keys(&["forward", "sprint", "jump"], false).await;
    rig.verdict("telly").await;

    rig.place_player([x, y, z + 8], 45.0, 78.0).await;
    rig.keys(&["back", "sneak"], true).await;
    for _ in 0..50 {
        rig.req(Command::Use).await;
        rig.req(Command::WaitTicks(2)).await;
        rig.sample("god_bridge").await;
    }
    rig.keys(&["back", "sneak"], false).await;
    rig.verdict("god_bridge").await;
    rig.finish("bridging_styles.csv").await;
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn parity_elytra_firework() {
    if !enabled() {
        return;
    }
    let mut rig = Rig::start("parity_elytra", "ParityElytra").await;
    let [x, y, z] = rig.platform(0).await;
    let user = rig.user;
    rig.cmd(&format!(
        "item replace entity {user} armor.chest with minecraft:elytra"
    ))
    .await;
    rig.cmd(&format!(
        "give {user} minecraft:firework_rocket[fireworks={{flight_duration:3}}] 32"
    ))
    .await;
    rig.req(Command::Hotbar(1)).await;
    rig.cmd(&format!("tp {user} {x}.5 {} {z}.5 -90 -25", y + 120))
        .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    rig.req(Command::Look {
        yaw: -90.0,
        pitch: -25.0,
    })
    .await;
    rig.keys(&["jump"], true).await;
    rig.req(Command::WaitTicks(2)).await;
    rig.keys(&["jump"], false).await;
    rig.req(Command::WaitTicks(2)).await;
    rig.keys(&["jump"], true).await;
    rig.req(Command::WaitTicks(2)).await;
    rig.keys(&["jump"], false).await;
    for i in 0..60 {
        if i % 15 == 5 {
            rig.req(Command::Use).await;
        }
        rig.req(Command::WaitTicks(2)).await;
        rig.sample("elytra").await;
    }
    rig.verdict("elytra").await;
    rig.finish("elytra_firework.csv").await;
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn parity_vehicle_moves() {
    if !enabled() {
        return;
    }
    let mut rig = Rig::start("parity_vehicles", "ParityRide").await;
    let user = rig.user;
    let [x, y, z] = rig.platform(0).await;
    rig.cmd(&format!(
        "fill {} {} {} {} {} {} minecraft:water",
        x - 4,
        y,
        z - 6,
        x + 24,
        y,
        z + 6
    ))
    .await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.cmd(&format!(
        "summon minecraft:oak_boat {x}.5 {} {}.5 {{Rotation:[-90f,0f]}}",
        y + 1,
        z
    ))
    .await;
    rig.cmd(&format!(
        "ride {user} mount @e[type=minecraft:oak_boat,limit=1,sort=nearest]"
    ))
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    rig.keys(&["forward"], true).await;
    for i in 0..80 {
        rig.keys(&["left"], (20..30).contains(&i)).await;
        rig.req(Command::WaitTicks(1)).await;
        rig.sample("boat").await;
    }
    rig.keys(&["forward", "left"], false).await;
    rig.verdict("boat").await;
    rig.cmd(&format!("ride {user} dismount")).await;

    let [x, y, z] = rig.platform(20).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.cmd(&format!(
        "summon minecraft:horse {x}.5 {} {}.5 {{Tame:1b}}",
        y + 1,
        z
    ))
    .await;
    rig.cmd("item replace entity @e[type=minecraft:horse,limit=1,sort=nearest] saddle with minecraft:saddle")
        .await;
    rig.cmd(&format!(
        "ride {user} mount @e[type=minecraft:horse,limit=1,sort=nearest]"
    ))
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    rig.req(Command::Look {
        yaw: -90.0,
        pitch: 0.0,
    })
    .await;
    rig.keys(&["forward"], true).await;
    for i in 0..80 {
        rig.keys(&["jump"], i % 30 < 5).await;
        rig.req(Command::WaitTicks(1)).await;
        rig.sample("horse").await;
    }
    rig.keys(&["forward", "jump"], false).await;
    rig.verdict("horse").await;
    rig.finish("vehicle_moves.csv").await;
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client"]
#[tokio::test(flavor = "multi_thread")]
async fn parity_tnt_knockback_repeated() {
    if !enabled() {
        return;
    }
    let mut rig = Rig::start("parity_tnt", "ParityTnt").await;
    let [x, y, z] = rig.platform(0).await;
    rig.cmd(&format!(
        "fill {} {y} {} {} {y} {} minecraft:obsidian",
        x - 6,
        z - 6,
        x + 6,
        z + 6
    ))
    .await;
    rig.cmd(&format!(
        "fill {} {} {} {} {} {} minecraft:air",
        x - 6,
        y + 1,
        z - 6,
        x + 6,
        y + 6,
        z + 6
    ))
    .await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    rig.place_player([x, y, z], -90.0, 0.0).await;
    for _ in 0..8 {
        rig.cmd(&format!(
            "effect give {} minecraft:instant_health 1 10",
            rig.user
        ))
        .await;
        rig.cmd(&format!(
            "summon minecraft:tnt {x}.5 {} {}.5 {{fuse:1}}",
            y + 1,
            z + 2
        ))
        .await;
        rig.record("tnt_knockback", 40).await;
        rig.place_player([x, y, z], -90.0, 0.0).await;
    }
    rig.verdict("tnt_knockback").await;
    rig.finish("tnt_knockback.csv").await;
}

#[tuff_core::watchdog]
#[ignore = "launches a real vanilla client against a vanilla server (scripts/vanilla-parity-run.sh)"]
#[tokio::test(flavor = "multi_thread")]
async fn vanilla_side() {
    if !enabled() {
        return;
    }
    let Ok(addr) = std::env::var("PARITY_SERVER_ADDR") else {
        eprintln!("skipped: set PARITY_SERVER_ADDR (host:port of the vanilla server)");
        return;
    };
    let user = std::env::var("PARITY_USERNAME").unwrap_or_else(|_| "Parity".into());
    let sentinel = std::path::PathBuf::from(
        std::env::var("PARITY_SENTINEL")
            .expect("PARITY_SENTINEL: file whose existence ends the run"),
    );
    let ready = std::env::var("PARITY_READY")
        .ok()
        .map(std::path::PathBuf::from);
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
    let run = RunDir::create(&default_run_base(), "vanilla-side").unwrap();
    let client = launch(
        &LaunchSpec {
            java: &java,
            files: &files,
            run: &run,
            username: &user,
            address: &addr,
            platform: &platform,
            hook_args: &[],
            hook_dir: None,
        },
        "vanilla-side",
        default_lock_path(),
    )
    .await
    .unwrap();
    until(&client, "join", |s| s.connected && s.screen == "none").await;
    if let Some(ready) = ready {
        std::fs::write(ready, b"joined").unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(900);
    while !sentinel.exists() && Instant::now() < deadline && !client.has_exited().await {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    client.shutdown().await;
}
