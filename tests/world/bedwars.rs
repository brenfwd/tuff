//! The bedwars example plugin loaded as WebAssembly: its lobby and arena
//! dimensions, a map built over ticks, queue and start, forges, block and
//! bed rules, a final kill deciding the game and the arena's reset.

use super::*;
use crate::protocol::play::GameMode;
use crate::world::messages::{PlayerHandle, RegionRequest};
use tuff_plugin_wasm::sandbox::{Items, uuid};
use tuff_plugin_wasm::sandbox::{Player, Position};
use tuff_plugin_wasm::wit::bindings::types::{
    BlockChange, BlockPos as WitPos, Death, EntityInteract, Event, MenuClick, MenuClickKind,
    Vec3 as WitVec3,
};
use tuff_plugin_wasm::{WasmHost, WasmPluginsFeature};
use tuff_storage::admin::PermissionLevel;
use tuff_storage::sqlite::SqliteBackend;
use tuff_vanilla_data::registry::EntityType;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const ARENA: &str = "bedwars:arena_1";
const LOBBY: &str = "bedwars:bedwars";
const BUILD: &str = "examples/rust-bedwars/target/wasm32-unknown-unknown/release/bedwars.wasm";
const RED_BED: &str = "minecraft:red_bed[facing=south,part=foot,occupied=false]";

fn built() -> Vec<u8> {
    static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    BYTES
        .get_or_init(|| {
            let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
            assert!(
                super::worldedit_wasm::build_example(&root.join("examples/rust-bedwars")),
                "building bedwars.wasm failed (is the wasm32-unknown-unknown target installed?)"
            );
            let path = root.join(BUILD);
            std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
        })
        .clone()
}

/// `text` without its section-sign formatting codes.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{a7}' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

struct Bedwars {
    world: TestWorld,
    _dir: TestDir,
}

impl Bedwars {
    async fn start(grant: bool) -> Bedwars {
        let dir = TestDir::new();
        let plugins = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(plugins.join("bedwars.wasm"), built()).unwrap();
        let backend = Arc::new(SqliteBackend::open(dir.path().join("world.mcserver")).unwrap());
        if grant {
            backend
                .config()
                .expect("sqlite keeps the config")
                .write_plugin_grant("bedwars", Some("dimension = bedwars:*\nworld-write = all\nplayer-write = all\nplayer-read = all"))
                .unwrap();
        }
        let plugin: Box<dyn crate::feature::Feature> = Box::new(WasmPluginsFeature::at(plugins));
        let world = TestWorld::start_with_features_on(backend, vec![plugin]);
        let spawn = if grant { "bedwars:bedwars" } else { OVERWORLD };
        assert_eq!(world.shared.spawn().dimension, spawn, "the lobby claim");
        let game = Bedwars { world, _dir: dir };
        game.host().sync().await;
        game
    }

    fn host(&self) -> &WasmHost {
        self.world.shared.services.get::<WasmHost>().unwrap()
    }

    fn player(uuid: u128, name: &str, level: PermissionLevel) -> Player {
        Player {
            level: Some(level),
            position: Some(Position {
                dimension: "minecraft:overworld".into(),
                at: [3.5, 80.0, -2.5],
                look: Some([90.0, 0.0]),
                eye_height: 1.62,
            }),
            ..Player::new(uuid, name)
        }
    }

    fn alex() -> Player {
        Self::player(1, "Alex", PermissionLevel::All)
    }

    fn bo() -> Player {
        Self::player(2, "Bo", PermissionLevel::All)
    }

    fn op() -> Player {
        Self::player(9, "Op", PermissionLevel::Gamemasters)
    }

    async fn run(&self, player: &Player, line: &str) -> Vec<String> {
        let outcome = self
            .host()
            .command(player.clone(), line.to_owned())
            .await
            .expect("the plugin owns the command");
        let mut said: Vec<String> = outcome.replies.iter().map(|r| plain(r)).collect();
        if let Err(err) = outcome.result {
            said.push(format!("error: {err}"));
        }
        said
    }

    async fn ticks(&self, n: u64) {
        for _ in 0..n {
            self.world.ticks(1).await;
            self.host().sync().await;
        }
    }

    async fn heard(&self) -> Vec<String> {
        self.host().sync().await;
        self.world
            .announced()
            .await
            .iter()
            .map(|line| plain(&line.plain()))
            .collect()
    }

    async fn heard_any(&self, part: &str) -> bool {
        self.heard().await.iter().any(|line| line.contains(part))
    }

    async fn block(&self, dimension: &str, [x, y, z]: [i32; 3]) -> String {
        let pos = BlockPos::new(x, y, z);
        let region = self
            .world
            .region_in(dimension, ChunkPos::of_block(pos).region())
            .await;
        if let Some(state) = block_at(&region, pos).await {
            return state.to_string();
        }
        let (viewer, mut events) = PlayerHandle::new(100);
        subscribe(&region, &viewer, &mut events, &[ChunkPos::of_block(pos)]).await;
        self.world.ticks(3).await;
        block_at(&region, pos)
            .await
            .expect("inside the world")
            .to_string()
    }

    async fn set(
        &self,
        dimension: &str,
        [x, y, z]: [i32; 3],
        state: tuff_vanilla_data::BlockState,
    ) {
        let pos = BlockPos::new(x, y, z);
        let region = self
            .world
            .region_in(dimension, ChunkPos::of_block(pos).region())
            .await;
        place_blocks(&self.world, &region, [(pos, state)]).await;
    }

    async fn items_near(&self, [x, y, z]: [f64; 3]) -> usize {
        let pos = BlockPos::new(x as i32, y as i32, z as i32);
        let region = self
            .world
            .region_in(ARENA, ChunkPos::of_block(pos).region())
            .await;
        let (reply, answer) = oneshot::channel();
        region.request(RegionRequest::Entities { reply }).await;
        answer
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind == EntityType::Item)
            .filter(|e| (e.position[0] - x).abs() < 2.0 && (e.position[2] - z).abs() < 2.0)
            .count()
    }

    async fn count(&self, kind: EntityType, [x, y, z]: [i32; 3]) -> usize {
        let pos = BlockPos::new(x, y, z);
        let region = self
            .world
            .region_in(ARENA, ChunkPos::of_block(pos).region())
            .await;
        let (reply, answer) = oneshot::channel();
        region.request(RegionRequest::Entities { reply }).await;
        answer
            .await
            .unwrap()
            .iter()
            .filter(|e| e.kind == kind)
            .count()
    }

    fn event(&self, event: Event, player: &Player) {
        self.host().event(event, Some(player.clone()));
    }

    fn change(player: &Player, [x, y, z]: [i32; 3], state: &str) -> BlockChange {
        BlockChange {
            player: Some(player.to_wit()),
            pos: WitPos { x, y, z },
            dimension: ARENA.into(),
            state: state.into(),
        }
    }

    async fn saved(&self, player: &Player) -> bool {
        let stored = self.host().stored("bedwars").await.expect("a store");
        stored.get(&format!("saved/{:032x}", player.uuid)).is_some()
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_game_is_queued_built_played_decided_and_reset() {
    let game = Bedwars::start(true).await;
    let dimensions = &game.world.shared.dimensions;
    for key in [LOBBY, ARENA, "bedwars:arena_4"] {
        assert!(dimensions.by_key(key).is_some(), "{key} is registered");
    }
    assert!(
        game.block(LOBBY, [0, 64, 0])
            .await
            .starts_with("minecraft:smooth_stone"),
        "the lobby platform"
    );

    game.ticks(3).await;
    assert!(
        game.block(ARENA, [0, 65, -37])
            .await
            .starts_with("minecraft:red_bed")
    );
    assert!(
        game.block(ARENA, [0, 64, -40])
            .await
            .starts_with("minecraft:red_concrete")
    );
    assert!(
        game.block(ARENA, [0, 109, 0])
            .await
            .starts_with("minecraft:glass")
    );
    assert_eq!(
        game.run(&Bedwars::alex(), "bedwars list").await,
        ["Arena 1 classic: waiting 0/4"]
    );

    let (alex, bo) = (Bedwars::alex(), Bedwars::bo());
    assert_eq!(
        game.run(&alex, "bedwars join").await,
        ["You joined arena 1 (classic)"]
    );
    assert!(game.saved(&alex).await, "where Alex was is kept");
    assert_eq!(
        game.run(&bo, "bw join 1").await,
        ["You joined arena 1 (classic)"]
    );
    assert!(game.heard_any("Bo has joined (2/4)!").await);
    let refused = game.run(&alex, "bedwars start").await;
    assert!(
        refused.iter().all(|line| line.starts_with("error:")),
        "{refused:?}"
    );
    assert_eq!(
        game.run(&Bedwars::op(), "bedwars start 1").await,
        ["Started arena 1 (classic)"]
    );
    assert!(game.heard_any("You are on the Red team.").await);
    assert!(game.heard_any("You are on the Blue team.").await);
    game.ticks(2).await;
    assert!(
        game.block(ARENA, [0, 109, 0])
            .await
            .starts_with("minecraft:air")
    );
    assert!(
        game.block(ARENA, [0, 65, 37])
            .await
            .starts_with("minecraft:air"),
        "green is empty"
    );

    game.ticks(3).await;
    assert!(
        game.items_near([0.5, 65.0, -44.5]).await >= 1,
        "the red forge drops"
    );

    let carrying = Player {
        items: Some(Items {
            stacks: vec![(
                36,
                tuff_item::item::ItemSnapshot::of(&tuff_item::item::ItemStack::mint(
                    tuff_vanilla_data::Item::IronIngot,
                    10,
                    tuff_item::item::MintReason::Test,
                )),
            )],
            ..Items::default()
        }),
        ..alex.clone()
    };
    let menu = |game: &Bedwars| game.host().menus().open_menu(1);
    game.event(
        Event::EntityInteract(EntityInteract {
            player: carrying.to_wit(),
            entity: uuid(500),
            kind: "minecraft:villager".into(),
            cancelled: true,
        }),
        &carrying,
    );
    game.ticks(1).await;
    assert_eq!(menu(&game), Some(("shop:blocks".into(), 6)));
    let click = |menu: &str, slot: u8| {
        Event::MenuClick(MenuClick {
            player: carrying.to_wit(),
            menu: menu.into(),
            slot,
            click: MenuClickKind::Left,
        })
    };
    game.event(click("shop:blocks", 9), &carrying);
    game.event(click("shop:blocks", 8), &carrying);
    game.ticks(1).await;
    assert_eq!(menu(&game), Some(("upgrades".into(), 3)));
    game.event(click("upgrades", 18), &carrying);
    game.ticks(1).await;
    assert_eq!(menu(&game), Some(("shop:blocks".into(), 6)));

    game.set(
        ARENA,
        [2, 64, -40],
        tuff_vanilla_data::Block::Air.default_state(),
    )
    .await;
    game.event(
        Event::BlockBreak(Bedwars::change(
            &alex,
            [2, 64, -40],
            "minecraft:red_concrete",
        )),
        &alex,
    );
    game.ticks(2).await;
    assert!(
        game.block(ARENA, [2, 64, -40])
            .await
            .starts_with("minecraft:red_concrete"),
        "map blocks come back"
    );
    assert!(
        game.heard_any("You can only break blocks placed by a player!")
            .await
    );

    game.event(
        Event::BlockBreak(Bedwars::change(&bo, [0, 65, -37], RED_BED)),
        &bo,
    );
    game.ticks(2).await;
    assert!(
        game.heard_any("BED DESTRUCTION > Red Bed was destroyed by Bo")
            .await
    );
    assert!(
        game.block(ARENA, [0, 65, -36])
            .await
            .starts_with("minecraft:air")
    );
    let status = game.run(&bo, "bedwars status").await.join("\n");
    assert!(status.contains("Red (no bed): Alex"), "{status}");
    assert!(status.contains("Blue (bed): Bo"), "{status}");

    let death = Death {
        player: alex.to_wit(),
        damage_type: "minecraft:out_of_world".into(),
        pos: WitVec3 {
            x: 0.5,
            y: -70.0,
            z: -40.5,
        },
        dimension: ARENA.into(),
    };
    game.event(Event::PlayerDeath(death), &alex);
    game.ticks(1).await;
    assert!(game.heard_any("Alex died. FINAL KILL!").await);
    assert!(
        game.heard_any("TEAM ELIMINATED > Red has been eliminated!")
            .await
    );
    assert!(game.heard_any("VICTORY! Blue wins: Bo").await);

    game.ticks(205).await;
    assert!(!game.saved(&alex).await, "Alex was put back");
    assert!(!game.saved(&bo).await, "Bo was put back");
    game.ticks(3).await;
    assert_eq!(
        game.run(&alex, "bedwars list").await,
        ["Arena 1 classic: waiting 0/4"]
    );
    assert!(
        game.block(ARENA, [0, 65, -37])
            .await
            .starts_with("minecraft:red_bed")
    );
    assert!(
        game.block(ARENA, [0, 65, 37])
            .await
            .starts_with("minecraft:lime_bed")
    );
    game.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_the_grant_there_are_no_arenas_but_the_shop_lists() {
    let game = Bedwars::start(false).await;
    assert!(game.world.shared.dimensions.by_key(ARENA).is_none());
    let alex = Bedwars::alex();
    assert_eq!(
        game.run(&alex, "bedwars list").await,
        ["No arenas; an admin adds one with /bedwars create <map>"]
    );
    assert_eq!(
        game.run(&alex, "bedwars join").await,
        ["error: No arena has room; try again after the next game"]
    );
    let made = game.run(&Bedwars::op(), "bedwars create classic").await;
    assert_eq!(
        made,
        ["error: bedwars:arena_1 does not exist; see the server log"]
    );
    let maps = game.run(&alex, "bedwars maps").await;
    assert_eq!(
        maps,
        [
            "classic 4x1: Red, Blue, Green, Yellow",
            "doubles 4x2: Red, Blue, Green, Yellow"
        ]
    );
    let shop = game.run(&alex, "bw shop blocks").await;
    assert_eq!(shop[0], "Wool x16 4 Iron");
    assert_eq!(shop.len(), 7);
    game.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn placed_tnt_lights_itself_and_burns_down() {
    let game = Bedwars::start(true).await;
    game.ticks(3).await;
    let (alex, bo) = (Bedwars::alex(), Bedwars::bo());
    game.run(&alex, "bedwars join").await;
    game.run(&bo, "bw join 1").await;
    game.run(&Bedwars::op(), "bedwars start 1").await;
    game.ticks(3).await;

    let at = [4, 65, -40];
    let floor = [4, 64, -40];
    assert!(!game.block(ARENA, floor).await.starts_with("minecraft:air"));
    game.set(ARENA, at, tuff_vanilla_data::Block::Tnt.default_state())
        .await;
    game.event(
        Event::BlockPlace(Bedwars::change(&alex, at, "minecraft:tnt")),
        &alex,
    );
    game.ticks(2).await;
    assert!(game.block(ARENA, at).await.starts_with("minecraft:air"));
    assert_eq!(game.count(EntityType::Tnt, at).await, 1, "primed");
    game.ticks(60).await;
    assert_eq!(game.count(EntityType::Tnt, at).await, 0, "exploded");
    assert!(
        !game.block(ARENA, floor).await.starts_with("minecraft:air"),
        "the blast spares map blocks"
    );
    game.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_the_bots_feature_anyone_asking_for_bots_is_told_so() {
    let game = Bedwars::start(true).await;
    game.ticks(3).await;
    for line in ["bedwars bot add 1", "bedwars bot fill 1 hard"] {
        let said = game.run(&Bedwars::alex(), line).await;
        assert!(
            said.iter().any(|l| l.contains("Bots need the bots plugin")),
            "{line}: {said:?}"
        );
    }
    game.ticks(3).await;
    let said = game.run(&Bedwars::alex(), "bedwars list").await;
    assert!(said.iter().all(|l| !l.contains("BOT_")), "{said:?}");
    game.world.shutdown().await;
}

/// A real server running Bedwars next to the bots plugin.
struct WithBots {
    handle: tuff::ServerHandle,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
    shared: Arc<Shared>,
    modes: GameModes,
    _dir: TestDir,
}

/// Every game mode a player was given, in order: a spectator spell that
/// ends before a probe gets to it still shows here.
type GameModes = Arc<std::sync::Mutex<Vec<(String, GameMode)>>>;

struct RecordGameModes(GameModes);

impl crate::world::session::hooks::SessionHooks for RecordGameModes {
    fn game_mode_changed(
        &self,
        session: &crate::world::session::PlayerSession,
        _from: GameMode,
        to: GameMode,
        cancelled: bool,
    ) {
        if !cancelled {
            let name = session.profile.name.clone();
            self.0.lock().unwrap().push((name, to));
        }
    }
}

impl crate::feature::Feature for RecordGameModes {
    fn id(&self) -> &'static str {
        "record-game-modes"
    }

    fn load(&self, cx: &mut crate::feature::LoadContext) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            cx.registries()
                .session_hooks
                .push(Arc::new(RecordGameModes(self.0.clone())));
        }
        Ok(())
    }
}

impl WithBots {
    async fn start() -> Option<WithBots> {
        let bots = super::bots::built("examples/rust-bots", "bots.wasm")?;
        let dir = TestDir::new();
        let plugins = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(plugins.join("bedwars.wasm"), built()).unwrap();
        std::fs::write(plugins.join("bots.wasm"), &bots).unwrap();
        let backend = Arc::new(SqliteBackend::open(dir.path().join("world.mcserver")).unwrap());
        let config = backend.config().expect("sqlite keeps the config");
        config
            .write_plugin_grant("bots", Some("cross-plugin = all"))
            .unwrap();
        config
            .write_plugin_grant(
                "bedwars",
                Some("dimension = bedwars:*\nworld-write = all\nplayer-write = all\nplayer-read = all\ncross-plugin = all"),
            )
            .unwrap();
        let cell = super::bots::Cell::default();
        let mut all = tuff::vanilla_features::all();
        let modes = GameModes::default();
        all.push(Box::new(super::bots::CaptureShared(cell.clone())));
        all.push(Box::new(RecordGameModes(modes.clone())));
        all.push(Box::new(WasmPluginsFeature::at(plugins)));
        let server = tuff::Server::builder()
            .features(all)
            .storage(backend)
            .instance_dir(dir.path().to_path_buf())
            .binds(vec!["127.0.0.1:0".into()])
            .online_mode(false)
            .whitelist(false)
            .view_distance(2)
            .seed(1)
            .build();
        let handle = server.handle();
        let task = tokio::spawn(server.run());
        handle.local_addrs().await.unwrap();
        let shared = cell.get().expect("the world started").clone();
        let run = WithBots {
            handle,
            task,
            shared,
            modes,
            _dir: dir,
        };
        run.host().sync().await;
        Some(run)
    }

    async fn stop(self) {
        self.handle.shutdown().await.unwrap();
        drop(self.shared);
        tokio::time::timeout(Duration::from_secs(20), self.task)
            .await
            .expect("the server task exits after shutdown")
            .unwrap()
            .unwrap();
    }

    fn host(&self) -> impl std::ops::Deref<Target = WasmHost> {
        self.shared.services.get::<WasmHost>().unwrap()
    }

    async fn bedwars(&self, line: &str) -> String {
        let outcome = self
            .host()
            .command(Bedwars::op(), format!("bedwars {line}"))
            .await
            .expect("bedwars owns its command");
        let mut said: Vec<String> = outcome.replies.iter().map(|r| plain(r)).collect();
        if let Err(err) = outcome.result {
            said.push(format!("error: {err}"));
        }
        said.join("\n")
    }

    async fn say(&self, line: &str) -> String {
        self.handle
            .run_command(line)
            .await
            .unwrap()
            .iter()
            .map(|c| plain(&c.plain()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Polls `probe` until it answers, for up to 60 seconds.
    async fn until<T, F: std::future::Future<Output = Option<T>>>(
        &self,
        what: &str,
        mut probe: impl FnMut() -> F,
    ) -> T {
        for _ in 0..1200 {
            if let Some(found) = probe().await {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let modes = format!("{:?}", self.modes.lock().unwrap());
        let online = self.say("list").await;
        let games = self.bedwars("list").await;
        panic!(
            "{what} did not happen within a minute; modes {modes}; online {online}; games {games}"
        );
    }

    async fn numbers(&self, line: &str) -> Vec<f64> {
        let text = self.say(line).await;
        let start = text.find("data: ").map_or(0, |i| i + 6);
        text[start..]
            .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
            .filter_map(|part| part.parse().ok())
            .collect()
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bots_are_fake_players_that_play_die_respawn_and_shop_by_the_rules() {
    let Some(run) = WithBots::start().await else {
        return;
    };
    let bot = "BOT_Ashwin";
    let filled = run.bedwars("bot fill 1").await;
    assert!(filled.contains("Adding"), "{filled}");
    run.until("the bots coming online", || async {
        run.say("list").await.contains(bot).then_some(())
    })
    .await;
    run.until("the start", || async {
        let said = run.bedwars("start 1").await;
        let started = !said.contains("error:") || run.bedwars("list").await.contains("running");
        started.then_some(())
    })
    .await;
    run.until("the bot on its team island", || async {
        run.say(&format!("data get entity {bot} Dimension"))
            .await
            .contains(ARENA)
            .then_some(())
    })
    .await;
    let list = run.bedwars("list").await;
    assert!(list.contains("running"), "{list}");

    run.say(&format!("effect give {bot} minecraft:instant_damage 1 5"))
        .await;
    run.until("the death and the wait as a spectator", || async {
        run.modes
            .lock()
            .unwrap()
            .iter()
            .any(|(name, mode)| name == bot && *mode == GameMode::Spectator)
            .then_some(())
    })
    .await;
    run.until("the respawn", || async {
        let health = run.numbers(&format!("data get entity {bot} Health")).await;
        let home = run
            .say(&format!("data get entity {bot} Dimension"))
            .await
            .contains(ARENA);
        let survival = run
            .numbers(&format!("data get entity {bot} playerGameType"))
            .await
            .first()
            == Some(&0.0);
        (home && survival && health.first().is_some_and(|h| *h >= 20.0)).then_some(())
    })
    .await;
    run.say(&format!("give {bot} minecraft:iron_ingot 20"))
        .await;
    let bought = run
        .until("a purchase", || async {
            let pos = run.numbers(&format!("data get entity {bot} Pos")).await;
            if pos.len() >= 3 {
                run.say(&format!(
                    "execute in {ARENA} run summon minecraft:item {} {} {} {{Item:{{id:\"minecraft:iron_ingot\",count:1}}}}",
                    pos[0], pos[1], pos[2]
                ))
                .await;
            }
            let inventory = run.say(&format!("data get entity {bot} Inventory")).await;
            inventory.contains("stone_sword").then_some(inventory)
        })
        .await;
    assert!(bought.contains("stone_sword"), "{bought}");

    let removed = run.bedwars("bot remove 1").await;
    assert!(removed.contains("Removed"), "{removed}");
    run.until("the bots leaving", || async {
        (!run.say("list").await.contains(bot)).then_some(())
    })
    .await;
    run.stop().await;
}
