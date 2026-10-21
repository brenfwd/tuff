//! The worldedit example plugin: edits over chunks nobody loaded, across
//! region borders and the origin, applied over ticks, with undo and redo.

use super::*;
use tuff_plugin_wasm::sandbox::{Items, Player, Position};
use tuff_plugin_wasm::wit::bindings::types::{BlockPos as WitPos, Click, Direction, Event};
use tuff_plugin_wasm::{WasmHost, WasmPluginsFeature};
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::BlockState;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

const BUILD: &str = "examples/rust-worldedit/target/wasm32-unknown-unknown/release/worldedit.wasm";

pub(crate) fn build_example(dir: &Path) -> bool {
    std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args([
            "build",
            "--release",
            "--quiet",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .is_ok_and(|status| status.success())
}

fn built() -> Option<Vec<u8>> {
    static BYTES: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    let bytes = BYTES.get_or_init(|| {
        let root = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let path = root.join(BUILD);
        assert!(
            build_example(&root.join("examples/rust-worldedit")),
            "building worldedit.wasm failed (is the wasm32-unknown-unknown target installed?)"
        );
        std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
    });
    Some(bytes.clone())
}

struct Editor {
    world: TestWorld,
    _dir: TestDir,
}

impl Editor {
    async fn start(bytes: &[u8]) -> Editor {
        Self::start_in(bytes, TestDir::new()).await
    }

    async fn start_in(bytes: &[u8], dir: TestDir) -> Editor {
        Self::start_with(bytes, dir, Vec::new()).await
    }

    async fn start_with(
        bytes: &[u8],
        dir: TestDir,
        mut extra: Vec<Box<dyn crate::feature::Feature>>,
    ) -> Editor {
        let plugins = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(plugins.join("worldedit.wasm"), bytes).unwrap();
        let plugin = WasmPluginsFeature::at(plugins);
        let backend = Arc::new(
            tuff_storage::sqlite::SqliteBackend::open(dir.path().join(
                match dir.path().join("world.mcserver").exists() {
                    true => "second.mcserver",
                    false => "world.mcserver",
                },
            ))
            .unwrap(),
        );
        backend
            .config()
            .expect("sqlite keeps the config")
            .write_plugin_grant(
                "worldedit",
                Some("world-write = all\nplayer-write = all\nplayer-read = all"),
            )
            .unwrap();
        extra.push(Box::new(plugin));
        let world = TestWorld::start_with_features_on(backend, extra);
        world
            .shared
            .services
            .get::<WasmHost>()
            .unwrap()
            .sync()
            .await;
        Editor { world, _dir: dir }
    }

    fn player(level: PermissionLevel, at: [f64; 3]) -> Player {
        Self::player_in("minecraft:overworld", level, at)
    }

    fn player_in(dimension: &str, level: PermissionLevel, at: [f64; 3]) -> Player {
        Player {
            uuid: 77,
            name: "Steve".to_owned(),
            level: Some(level),
            position: Some(Position {
                dimension: dimension.to_owned(),
                at,
                look: None,
                eye_height: 1.62,
            }),
            items: None,
        }
    }

    async fn run_as(&self, player: Player, line: &str) -> Vec<String> {
        let host = self.world.shared.services.get::<WasmHost>().unwrap();
        host.command(player, line.to_owned())
            .await
            .expect("the plugin owns /tuff we")
            .replies
    }

    async fn run(&self, line: &str) -> Vec<String> {
        self.run_as(
            Self::player(PermissionLevel::Gamemasters, [0.0, 100.0, 0.0]),
            line,
        )
        .await
    }

    async fn at(&self, x: i32, y: i32, z: i32) -> BlockState {
        let pos = BlockPos::new(x, y, z);
        let region = self.world.region(ChunkPos::of_block(pos).region()).await;
        block_at(&region, pos).await.expect("inside the world")
    }

    async fn is(&self, x: i32, y: i32, z: i32, name: &str) -> bool {
        self.at(x, y, z)
            .await
            .to_string()
            .starts_with(&format!("minecraft:{name}"))
    }

    async fn select(&self, a: [i32; 3], b: [i32; 3]) {
        self.run(&format!("tuff we pos1 {} {} {}", a[0], a[1], a[2]))
            .await;
        self.run(&format!("tuff we pos2 {} {} {}", b[0], b[1], b[2]))
            .await;
    }

    async fn finish(&self) {
        self.world.ticks(12).await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_and_replace_cross_the_origin_and_region_borders_over_unloaded_chunks() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([-3, 100, -3], [3, 100, 3]).await;
    let said = editor.run("tuff we set stone").await;
    assert!(said[0].contains("49 blocks"), "{said:?}");
    editor.finish().await;
    for (x, z) in [(-3, -3), (3, 3), (-3, 3), (3, -3), (0, 0)] {
        assert!(editor.is(x, 100, z, "stone").await, "{x} {z}");
    }
    assert!(!editor.is(4, 100, 0, "stone").await);

    editor.select([509, 120, 509], [514, 120, 514]).await;
    editor.run("tuff we set glass").await;
    editor.finish().await;
    for (x, z) in [(509, 509), (514, 514), (511, 512), (512, 511)] {
        assert!(editor.is(x, 120, z, "glass").await, "{x} {z}");
    }

    editor.select([-3, 100, -3], [3, 100, 3]).await;
    editor.run("tuff we replace stone dirt").await;
    editor.finish().await;
    assert!(editor.is(-3, 100, -3, "dirt").await);
    assert!(editor.is(3, 100, 3, "dirt").await);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setblock_reaches_a_far_unloaded_chunk() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor
        .run("tuff we setblock -5000 100 7000 diamond_block")
        .await;
    editor.finish().await;
    assert!(editor.is(-5000, 100, 7000, "diamond_block").await);
    let said = editor.run("tuff we setblock 1 1 1 not_a_block").await;
    assert!(!said[0].contains("Queued"));
    editor.finish().await;
    assert!(editor.run("tuff we status").await[0].starts_with("0 running"));
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clone_and_paste_copy_blocks_to_unloaded_destinations() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([0, 100, 0], [1, 100, 1]).await;
    editor.run("tuff we set gold_block").await;
    editor.finish().await;
    editor
        .run("tuff we setblock 1 100 1 oak_stairs[facing=east]")
        .await;
    editor.finish().await;

    editor.run("tuff we clone 510 100 -2").await;
    editor.finish().await;
    assert!(editor.is(510, 100, -2, "gold_block").await);
    assert!(editor.is(511, 100, -1, "oak_stairs").await);
    assert!(
        editor
            .at(511, 100, -1)
            .await
            .to_string()
            .contains("facing=east")
    );
    assert!(editor.is(0, 100, 0, "gold_block").await, "the source stays");

    let there = Editor::player(PermissionLevel::Gamemasters, [-600.5, 90.0, 600.5]);
    let said = editor.run_as(there.clone(), "tuff we paste").await;
    assert!(said[0].contains("4 blocks"), "{said:?}");
    editor.finish().await;
    assert!(editor.is(-601, 90, 600, "gold_block").await);
    assert!(editor.is(-600, 90, 601, "oak_stairs").await);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn undo_and_redo_restore_edits_across_regions() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([-2, 110, -2], [2, 110, 2]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    editor.select([509, 130, -2], [514, 130, 2]).await;
    editor.run("tuff we set obsidian").await;
    editor.finish().await;
    assert!(editor.is(514, 130, 2, "obsidian").await);

    editor.run("tuff we undo").await;
    editor.finish().await;
    assert!(editor.is(509, 130, 0, "air").await);
    assert!(!editor.is(514, 130, 2, "obsidian").await);
    assert!(editor.is(0, 110, 0, "stone").await, "the older edit stays");

    editor.run("tuff we redo").await;
    editor.finish().await;
    assert!(editor.is(514, 130, 2, "obsidian").await);

    editor.run("tuff we undo").await;
    editor.run("tuff we undo").await;
    editor.finish().await;
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert!(!editor.is(0, 110, 0, "stone").await);
    let said = editor.run("tuff we undo").await;
    assert_eq!(said, ["Nothing to undo"]);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_survives_a_restart_and_clearhistory_forgets_it() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([-2, 110, -2], [2, 110, 2]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    editor.select([4, 110, 4], [6, 110, 6]).await;
    editor.run("tuff we set obsidian").await;
    editor.finish().await;
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert_eq!(
        editor.run("tuff we status").await,
        ["0 running, 1 to undo, 1 to redo"]
    );
    editor.world.shared.services.flush().await;
    let Editor { world, _dir } = editor;
    world.shutdown().await;

    let editor = Editor::start_in(&bytes, _dir).await;
    assert_eq!(
        editor.run("tuff we status").await,
        ["0 running, 1 to undo, 1 to redo"]
    );
    editor.run("tuff we redo").await;
    editor.finish().await;
    assert!(editor.is(5, 110, 5, "obsidian").await);
    editor.run("tuff we clearhistory").await;
    assert_eq!(
        editor.run("tuff we status").await,
        ["0 running, 0 to undo, 0 to redo"]
    );
    editor.world.shared.services.flush().await;
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn searchitem_finds_items_and_blocks_by_name() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    let items = editor.run("tuff we searchitem diamond_pickaxe").await;
    assert!(items[0].contains("minecraft:diamond_pickaxe"), "{items:?}");
    let planks = editor.run("tuff we searchitem oak_pla").await;
    assert!(planks[0].contains("minecraft:oak_planks"), "{planks:?}");
    let blocks = editor.run("tuff we searchitem -b cobble").await;
    assert!(blocks[0].contains("minecraft:cobblestone"), "{blocks:?}");
    assert_eq!(
        editor.run("tuff we searchitem zzzz_nothing").await,
        ["No matches"]
    );
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn big_edits_are_sliced_over_ticks_and_limits_and_permissions_hold() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([-20, 100, -20], [19, 109, 19]).await;
    editor.run("tuff we set stone").await;
    editor.world.ticks(1).await;
    assert!(
        editor.is(-20, 100, -20, "stone").await != editor.is(19, 109, 19, "stone").await,
        "one tick applies one slice, not the whole edit"
    );
    let busy = editor.run("tuff we set dirt").await;
    assert!(busy[0].contains("already"), "{busy:?}");
    editor.world.ticks(10).await;
    assert!(editor.is(19, 109, 19, "stone").await);
    assert!(editor.is(-20, 100, -20, "stone").await);
    let announced: Vec<String> = editor
        .world
        .announced()
        .await
        .iter()
        .map(|line| line.plain())
        .collect();
    assert!(
        announced
            .iter()
            .any(|line| line.contains("set finished: 16000")),
        "{announced:?}"
    );

    assert!(editor.run("tuff we limit 10").await[0].contains("10"));
    let said = editor.run("tuff we set dirt").await;
    assert!(said[0].contains("over your limit"), "{said:?}");

    let player = Editor::player(PermissionLevel::All, [0.0, 0.0, 0.0]);
    let said = editor.run_as(player, "tuff we set dirt").await;
    assert!(said[0].contains("permission"), "{said:?}");
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn double_slash_aliases_help_and_unknown_subcommands_work() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.run("/pos1 0 100 0").await;
    editor.run("/pos2 1 100 1").await;
    let said = editor.run("/set stone").await;
    assert!(said[0].contains("4 blocks"), "{said:?}");
    editor.finish().await;
    assert!(editor.is(1, 100, 1, "stone").await);
    let said = editor.run("/re stone dirt").await;
    assert!(said[0].contains("Queued"), "{said:?}");
    editor.finish().await;
    assert!(editor.is(0, 100, 0, "dirt").await);

    let said = editor.run("tuff we help set").await;
    assert!(said[0].starts_with("Usage: //set"), "{said:?}");
    let said = editor.run("tuff we frobnicate").await;
    assert!(said[0].contains("Unknown"), "{said:?}");
    let said = editor.run("tuff we set").await;
    assert!(said[0].starts_with("Usage: //set"), "{said:?}");
    let said = editor.run("tuff we help -x").await;
    assert!(said[0].contains("Unknown flag"), "{said:?}");
    assert!(editor.run("tuff we version").await[0].contains("WorldEdit"));
    assert!(editor.run("tuff we toggleeditwand").await[0].contains("disabled"));

    let host = editor.world.shared.services.get::<WasmHost>().unwrap();
    let player = Editor::player(PermissionLevel::Gamemasters, [0.0, 100.0, 0.0]);
    assert!(host.command(player, "we status".to_owned()).await.is_none());
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sphere_selection_sets_only_the_ball_and_expand_vert_spans_the_world() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    let said = editor.run("tuff we sel sphere").await;
    assert!(said[0].contains("sphere"), "{said:?}");
    editor.run("tuff we pos1 0 100 0").await;
    let said = editor.run("tuff we pos2 3 100 0").await;
    assert!(said[0].contains("blocks selected"), "{said:?}");
    editor.run("tuff we set stone").await;
    editor.finish().await;
    assert!(editor.is(0, 100, 0, "stone").await);
    assert!(editor.is(3, 100, 0, "stone").await);
    assert!(editor.is(0, 103, 0, "stone").await);
    assert!(!editor.is(3, 103, 3, "stone").await);
    assert!(!editor.is(4, 100, 0, "stone").await);
    let counted = editor.run("tuff we count stone").await;
    assert!(counted[0].starts_with("Counted: "), "{counted:?}");
    let distr = editor.run("tuff we distr").await;
    assert!(distr[0].contains("minecraft:stone"), "{distr:?}");

    editor.run("tuff we sel cuboid").await;
    editor.select([0, 100, 0], [1, 101, 1]).await;
    let said = editor.run("tuff we expand vert").await;
    assert!(said[0].contains("-64"), "{said:?}");
    let size = editor.run("tuff we size").await;
    assert!(size[0].contains("Size: 2 x 384 x 2"), "{size:?}");
    editor.run("tuff we contract 380 down").await;
    let size = editor.run("tuff we size").await;
    assert!(size[0].contains("Size: 2 x 4 x 2"), "{size:?}");
    editor.run("tuff we shift 10 east").await;
    editor.run("tuff we outset 1 -h").await;
    let size = editor.run("tuff we size").await;
    assert!(size[0].contains("Minimum: 9 "), "{size:?}");
    assert!(size[0].contains("Size: 4 x 4 x 4"), "{size:?}");
    let chunk = editor.run("tuff we chunk").await;
    assert!(chunk[0].contains("0, 0"), "{chunk:?}");
    let size = editor.run("tuff we size").await;
    assert!(size[0].contains("Size: 16 x 384 x 16"), "{size:?}");
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hpos_raycasts_along_the_look_and_directions_follow_the_yaw() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([5, 100, 5], [5, 100, 5]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    let mut player = Editor::player(PermissionLevel::Gamemasters, [5.5, 103.0, 5.5]);
    player.position.as_mut().unwrap().look = Some([0.0, 90.0]);
    let said = editor.run_as(player, "tuff we hpos1").await;
    assert!(said[0].contains("Position 1 set to 5 100 5"), "{said:?}");

    let mut player = Editor::player(PermissionLevel::Gamemasters, [5.5, 103.0, 5.5]);
    player.position.as_mut().unwrap().look = Some([180.0, 0.0]);
    editor.run_as(player.clone(), "tuff we pos2 5 100 5").await;
    editor.run_as(player.clone(), "tuff we expand 3 me").await;
    let size = editor.run("tuff we size").await;
    assert!(size[0].contains("Size: 1 x 1 x 4"), "{size:?}");
    assert!(size[0].contains("Minimum: 5 100 2"), "{size:?}");
    editor.run_as(player, "tuff we shift 2 right").await;
    let size = editor.run("tuff we size").await;
    assert!(size[0].contains("Minimum: 7 100 2"), "{size:?}");
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn set_with_a_weighted_pattern_mixes_blocks_and_bad_patterns_are_rejected() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([0, 100, 0], [19, 100, 9]).await;
    let said = editor.run("tuff we set 50%stone,50%dirt").await;
    assert!(said[0].contains("200 blocks"), "{said:?}");
    editor.finish().await;
    let (mut stone, mut dirt) = (0, 0);
    for x in 0..20 {
        for z in 0..10 {
            if editor.is(x, 100, z, "stone").await {
                stone += 1;
            } else if editor.is(x, 100, z, "dirt").await {
                dirt += 1;
            }
        }
    }
    assert_eq!(stone + dirt, 200);
    assert!(stone > 60 && dirt > 60, "{stone} {dirt}");

    let said = editor.run("tuff we set 50%stone,nope").await;
    assert!(said[0].contains("Unknown block"), "{said:?}");
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replace_with_masks_and_gmask_limit_which_blocks_change() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([0, 100, 0], [3, 100, 0]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    editor.select([2, 100, 0], [3, 100, 0]).await;
    editor.run("tuff we set dirt").await;
    editor.finish().await;

    editor.select([0, 100, 0], [3, 100, 0]).await;
    editor.run("tuff we replace !dirt glass").await;
    editor.finish().await;
    assert!(editor.is(0, 100, 0, "glass").await && editor.is(1, 100, 0, "glass").await);
    assert!(editor.is(2, 100, 0, "dirt").await);

    editor.run("tuff we replace glass,dirt >stone sand").await;
    editor.finish().await;
    assert!(editor.is(0, 100, 0, "glass").await);

    let said = editor.run("tuff we gmask dirt").await;
    assert!(said[0].contains("Global mask set"), "{said:?}");
    editor.run("tuff we set gravel").await;
    editor.finish().await;
    assert!(editor.is(2, 100, 0, "gravel").await && editor.is(3, 100, 0, "gravel").await);
    assert!(editor.is(0, 100, 0, "glass").await);

    editor.run("tuff we gmask").await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    assert!(editor.is(0, 100, 0, "stone").await);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn copy_keeps_the_offset_and_rotate_flip_and_skip_air_work() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([0, 100, 0], [1, 100, 0]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    editor
        .run("tuff we setblock 1 100 0 oak_stairs[facing=north]")
        .await;
    editor.finish().await;
    let from = Editor::player(PermissionLevel::Gamemasters, [1.5, 100.0, 0.5]);
    editor.run_as(from, "tuff we copy").await;

    let there = Editor::player(PermissionLevel::Gamemasters, [50.5, 110.0, 50.5]);
    editor.run_as(there.clone(), "tuff we paste").await;
    editor.finish().await;
    assert!(editor.is(49, 110, 50, "stone").await);
    assert!(editor.is(50, 110, 50, "oak_stairs").await);

    editor.run_as(there.clone(), "tuff we rotate 90").await;
    let at = Editor::player(PermissionLevel::Gamemasters, [80.5, 110.0, 80.5]);
    editor.run_as(at.clone(), "tuff we paste").await;
    editor.finish().await;
    assert!(editor.is(80, 110, 79, "stone").await);
    assert!(editor.is(80, 110, 80, "oak_stairs").await);
    assert!(
        editor
            .at(80, 110, 80)
            .await
            .to_string()
            .contains("facing=east")
    );

    editor.run_as(at.clone(), "tuff we flip e").await;
    editor.run_as(at.clone(), "tuff we paste").await;
    editor.finish().await;
    assert!(editor.is(80, 110, 79, "stone").await);
    assert!(
        editor
            .at(80, 110, 80)
            .await
            .to_string()
            .contains("facing=west")
    );

    editor.run("tuff we setblock 49 110 50 gold_block").await;
    editor.finish().await;
    editor.run("tuff we clearclipboard").await;
    let said = editor.run_as(at.clone(), "tuff we paste").await;
    assert!(said[0].contains("empty"), "{said:?}");
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paste_flags_and_cut_with_undo_behave() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([0, 100, 0], [1, 100, 0]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    editor.run("tuff we setblock 1 100 0 air").await;
    editor.finish().await;
    editor.run("tuff we copy").await;
    editor.run("tuff we setblock 21 100 0 gold_block").await;
    editor.finish().await;
    let there = Editor::player(PermissionLevel::Gamemasters, [20.5, 100.0, 0.5]);
    editor.run_as(there.clone(), "tuff we paste -a").await;
    editor.finish().await;
    assert!(editor.is(20, 100, 0, "stone").await);
    assert!(editor.is(21, 100, 0, "gold_block").await, "-a skips air");
    editor.run_as(there.clone(), "tuff we paste").await;
    editor.finish().await;
    assert!(editor.is(21, 100, 0, "air").await);
    editor.run_as(there.clone(), "tuff we paste -o").await;
    editor.finish().await;
    editor.run("tuff we setblock 0 100 0 dirt").await;
    editor.finish().await;
    editor.run("tuff we paste -o").await;
    editor.finish().await;
    assert!(editor.is(0, 100, 0, "stone").await);

    editor.run("tuff we cut").await;
    editor.finish().await;
    assert!(editor.is(0, 100, 0, "air").await);
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert!(editor.is(0, 100, 0, "stone").await);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shape_generation_builds_around_the_player_and_undoes() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.run("tuff we sphere stone 2").await;
    editor.finish().await;
    assert!(editor.is(0, 100, 0, "stone").await && editor.is(2, 100, 0, "stone").await);
    assert!(!editor.is(2, 102, 2, "stone").await);

    editor.run("tuff we undo").await;
    editor.finish().await;
    assert!(editor.is(0, 100, 0, "air").await);

    editor.run("tuff we hsphere stone 3").await;
    editor.finish().await;
    assert!(editor.is(3, 100, 0, "stone").await && editor.is(0, 100, 0, "air").await);

    editor.run("tuff we hcyl obsidian 2 3").await;
    editor.finish().await;
    assert!(editor.is(2, 101, 0, "obsidian").await && editor.is(0, 101, 0, "air").await);

    editor.run("tuff we pyramid gold_block 3").await;
    editor.finish().await;
    assert!(editor.is(0, 102, 0, "gold_block").await && editor.is(2, 100, 2, "gold_block").await);

    let said = editor.run("tuff we sphere stone 500").await;
    assert!(
        said.iter().any(|l| l.contains("over your limit")),
        "{said:?}"
    );
    editor.world.shutdown().await;
}

impl Editor {
    async fn count(&self, x: [i32; 2], y: [i32; 2], z: [i32; 2], names: &[&str]) -> usize {
        let mut n = 0;
        for yy in y[0]..=y[1] {
            for zz in z[0]..=z[1] {
                for xx in x[0]..=x[1] {
                    for name in names {
                        if self.is(xx, yy, zz, name).await {
                            n += 1;
                        }
                    }
                }
            }
        }
        n
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn walls_faces_and_center_shape_the_selection_and_undo() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([0, 100, 0], [4, 102, 4]).await;
    editor.run("tuff we walls stone").await;
    editor.finish().await;
    assert!(editor.is(0, 101, 2, "stone").await);
    assert!(editor.is(2, 101, 4, "stone").await);
    assert!(editor.is(2, 101, 2, "air").await);
    assert!(
        editor.is(2, 100, 2, "air").await,
        "walls leave floor and ceiling"
    );
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert!(editor.is(0, 101, 2, "air").await);

    editor.run("tuff we faces glass").await;
    editor.finish().await;
    assert!(editor.is(2, 100, 2, "glass").await);
    assert!(editor.is(2, 102, 2, "glass").await);
    assert!(editor.is(2, 101, 2, "air").await);
    editor.run("tuff we center gold_block").await;
    editor.finish().await;
    assert!(editor.is(2, 101, 2, "gold_block").await);
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert!(editor.is(2, 101, 2, "air").await);
    assert!(editor.is(2, 100, 2, "glass").await);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hollow_overlay_and_naturalize_rework_a_solid_block() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([10, 100, 10], [14, 104, 14]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    editor.run("tuff we hollow").await;
    editor.finish().await;
    assert!(editor.is(12, 102, 12, "air").await);
    assert!(editor.is(10, 100, 10, "stone").await);
    assert!(editor.is(10, 102, 12, "stone").await);
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert!(editor.is(12, 102, 12, "stone").await);

    editor.run("tuff we hollow 2 glass").await;
    editor.finish().await;
    assert!(editor.is(12, 102, 12, "glass").await);
    assert!(editor.is(11, 102, 12, "stone").await);
    editor.run("tuff we undo").await;
    editor.finish().await;

    editor.run("tuff we overlay glass").await;
    editor.finish().await;
    assert!(editor.is(12, 105, 12, "glass").await);
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert!(editor.is(12, 105, 12, "air").await);

    editor.run("tuff we naturalize").await;
    editor.finish().await;
    assert!(editor.is(12, 104, 12, "grass_block").await);
    assert!(editor.is(12, 102, 12, "dirt").await);
    assert!(editor.is(12, 100, 12, "stone").await);
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert!(editor.is(12, 104, 12, "stone").await);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn smooth_lowers_a_spike_and_undoes() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([20, 100, 20], [26, 100, 26]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    editor.select([23, 101, 23], [23, 104, 23]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    editor.select([20, 100, 20], [26, 110, 26]).await;
    editor.run("tuff we smooth").await;
    editor.finish().await;
    assert!(editor.is(23, 104, 23, "air").await);
    assert!(editor.is(23, 100, 23, "stone").await);
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert!(editor.is(23, 104, 23, "stone").await);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn move_and_stack_copy_by_direction_with_flags_and_undo() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([30, 100, 30], [31, 100, 30]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    editor.run("tuff we stack 2 east").await;
    editor.finish().await;
    assert_eq!(
        editor
            .count([30, 36], [100, 100], [30, 30], &["stone"])
            .await,
        6
    );
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert_eq!(
        editor
            .count([30, 36], [100, 100], [30, 30], &["stone"])
            .await,
        2
    );

    editor.run("tuff we move 3 up glass").await;
    editor.finish().await;
    assert!(editor.is(30, 103, 30, "stone").await);
    assert!(editor.is(30, 100, 30, "glass").await);
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert!(editor.is(30, 100, 30, "stone").await);
    assert!(editor.is(30, 103, 30, "air").await);

    editor.run("tuff we move 2 south -s").await;
    editor.finish().await;
    assert!(editor.is(30, 100, 32, "stone").await);
    assert!(editor.is(30, 100, 30, "air").await);
    let said = editor.run("tuff we size").await.join(" ");
    assert!(said.contains("32"), "{said}");
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn line_and_curve_draw_between_selection_points() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([40, 100, 40], [46, 100, 40]).await;
    editor.run("tuff we line glass").await;
    editor.finish().await;
    assert_eq!(
        editor
            .count([40, 46], [100, 100], [40, 40], &["glass"])
            .await,
        7
    );
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert_eq!(
        editor
            .count([40, 46], [100, 100], [40, 40], &["glass"])
            .await,
        0
    );

    editor.run("tuff we line stone 1").await;
    editor.finish().await;
    assert!(editor.is(43, 101, 40, "stone").await);

    editor.run("tuff we sel convex").await;
    editor.run("tuff we pos1 60 100 60").await;
    editor.run("tuff we pos2 66 100 60").await;
    editor.run("tuff we curve glass").await;
    editor.finish().await;
    assert!(editor.is(63, 100, 60, "glass").await);
    assert!(editor.is(60, 100, 60, "glass").await);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forest_and_flora_plant_on_grass_and_undo() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([80, 100, 80], [92, 100, 92]).await;
    editor.run("tuff we set grass_block").await;
    editor.finish().await;
    editor.run("tuff we forest birch 100").await;
    editor.finish().await;
    let logs = editor
        .count([80, 92], [101, 106], [80, 92], &["birch_log"])
        .await;
    assert!(logs >= 8, "{logs}");
    assert!(
        editor
            .count([78, 94], [101, 110], [78, 94], &["birch_leaves"])
            .await
            > 20
    );
    let said = editor.run("tuff we forest nonsense").await.join(" ");
    assert!(said.contains("Unknown tree"), "{said}");
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert_eq!(
        editor
            .count([80, 92], [101, 106], [80, 92], &["birch_log"])
            .await,
        0
    );

    editor.run("tuff we flora 100").await;
    editor.finish().await;
    let plants = editor
        .count(
            [80, 92],
            [101, 101],
            [80, 92],
            &["short_grass", "dandelion", "poppy"],
        )
        .await;
    assert!(plants > 100, "{plants}");
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert!(editor.is(85, 101, 85, "air").await);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generate_builds_an_expression_sphere_and_deform_and_expression_masks_work() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([0, 100, 0], [10, 110, 10]).await;
    editor.run("tuff we generate stone x*x+y*y+z*z<1").await;
    editor.finish().await;
    assert!(editor.is(5, 105, 5, "stone").await);
    assert!(editor.is(7, 105, 5, "stone").await);
    assert!(editor.is(0, 100, 0, "air").await);
    assert!(editor.is(5, 100, 5, "air").await);
    assert!(editor.is(9, 109, 9, "air").await);

    editor.run("tuff we undo").await;
    editor.finish().await;
    editor.finish().await;
    assert!(editor.is(5, 105, 5, "air").await);

    editor
        .run("tuff we generate -h obsidian x*x+y*y+z*z<1")
        .await;
    editor.finish().await;
    editor.finish().await;
    assert!(editor.is(5, 105, 5, "air").await);
    assert!(editor.is(8, 105, 5, "air").await);
    let n = editor
        .count([0, 10], [100, 110], [0, 10], &["obsidian"])
        .await;
    assert!(n > 20, "{n}");
    assert!(editor.is(9, 105, 5, "obsidian").await);

    let said = editor.run("tuff we generate stone x+").await;
    assert!(
        said.iter().any(|l| l.contains("Bad expression")),
        "{said:?}"
    );
    let said = editor.run("tuff we generate stone while(1)1").await;
    assert!(said.iter().any(|l| l.contains("failed")), "{said:?}");

    editor.run("tuff we replace =y>104 dirt").await;
    editor.finish().await;
    assert!(editor.is(9, 105, 5, "dirt").await && editor.is(0, 110, 0, "dirt").await);
    assert!(editor.is(0, 100, 0, "air").await && editor.is(9, 104, 5, "obsidian").await);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deform_moves_blocks_through_the_expression_and_runaway_masks_are_contained() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.run("tuff we setblock 5 105 5 stone").await;
    editor.finish().await;
    editor.select([0, 100, 0], [10, 110, 10]).await;
    editor.run("tuff we deform -r x=x+1").await;
    editor.finish().await;
    assert!(editor.is(4, 105, 5, "stone").await && editor.is(5, 105, 5, "air").await);

    editor.run("tuff we replace =while(1)1 dirt").await;
    editor.finish().await;
    assert!(editor.is(4, 105, 5, "stone").await);
    let said = editor.run("tuff we deform nope(").await;
    assert!(
        said.iter().any(|l| l.contains("Bad expression")),
        "{said:?}"
    );
    editor.world.shutdown().await;
}

fn holder(look: [f32; 2], held: &str) -> Player {
    let mut player = Editor::player(PermissionLevel::Gamemasters, [5.5, 103.0, 5.5]);
    player.position.as_mut().unwrap().look = Some(look);
    player.items = Some(Items {
        held: Some((held.to_owned(), 1)),
        slots: Vec::new(),
        ..Default::default()
    });
    player
}

impl Editor {
    async fn click(&self, player: &Player, right: bool, at: [i32; 3]) {
        let click = Click {
            player: player.to_wit(),
            pos: WitPos {
                x: at[0],
                y: at[1],
                z: at[2],
            },
            dimension: "minecraft:overworld".to_owned(),
            face: Direction::Up,
            state: String::new(),
        };
        let event = match right {
            true => Event::RightClick(click),
            false => Event::LeftClick(click),
        };
        let host = self.world.shared.services.get::<WasmHost>().unwrap();
        host.event(event, Some(player.clone()));
        host.sync().await;
        self.finish().await;
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sphere_brush_paints_at_the_looked_at_block_and_respects_mask_and_size() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([5, 100, 5], [5, 100, 5]).await;
    editor.run("tuff we set dirt").await;
    editor.finish().await;
    let player = holder([0.0, 90.0], "minecraft:stick");
    let said = editor
        .run_as(player.clone(), "tuff we brush sphere stone 2")
        .await;
    assert!(said[0].contains("Bound"), "{said:?}");
    editor.click(&player, true, [0, 0, 0]).await;
    assert!(editor.is(5, 102, 5, "stone").await);
    assert!(editor.is(7, 100, 5, "stone").await);
    assert!(!editor.is(8, 100, 5, "stone").await);

    editor.run_as(player.clone(), "tuff we brushsize 1").await;
    editor
        .run_as(player.clone(), "tuff we material glass")
        .await;
    editor.run_as(player.clone(), "tuff we mask stone").await;
    editor.click(&player, true, [0, 0, 0]).await;
    assert!(editor.is(5, 102, 5, "glass").await);
    assert!(editor.is(5, 100, 5, "stone").await);
    assert!(!editor.is(6, 100, 5, "glass").await);
    assert!(editor.is(6, 100, 5, "stone").await);

    editor.run_as(player.clone(), "tuff we none").await;
    editor.click(&player, true, [0, 0, 0]).await;
    assert!(editor.is(5, 102, 5, "glass").await);
    assert!(editor.is(5, 103, 5, "air").await);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn info_tool_reports_the_block_in_sight_and_range_limits_the_ray() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([5, 100, 5], [5, 100, 5]).await;
    editor.run("tuff we set oak_planks").await;
    editor.finish().await;
    let player = holder([0.0, 90.0], "minecraft:stick");
    editor.run_as(player.clone(), "tuff we tool info").await;
    editor.click(&player, true, [0, 0, 0]).await;
    let said = editor.run_as(player.clone(), "tuff we tool last").await;
    assert!(said[0].contains("Position: 5 100 5"), "{said:?}");
    assert!(said[0].contains("minecraft:oak_planks"), "{said:?}");

    editor.run_as(player.clone(), "tuff we range 2").await;
    editor.click(&player, true, [0, 0, 0]).await;
    let said = editor.run_as(player.clone(), "tuff we tool last").await;
    assert!(said[0].contains("No block in sight"), "{said:?}");
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tools_build_select_flood_and_gravity_brush_drops_blocks() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([5, 100, 5], [5, 100, 5]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    let player = holder([0.0, 90.0], "minecraft:stick");
    editor
        .run_as(player.clone(), "tuff we tool lrbuild glass gold_block")
        .await;
    editor.click(&player, false, [0, 0, 0]).await;
    assert!(editor.is(5, 100, 5, "glass").await);
    editor.click(&player, true, [0, 0, 0]).await;
    assert!(editor.is(5, 101, 5, "gold_block").await);

    editor.run_as(player.clone(), "tuff we none").await;
    editor.run_as(player.clone(), "tuff we tool farwand").await;
    editor.click(&player, true, [0, 0, 0]).await;
    let said = editor.run_as(player.clone(), "tuff we pos1 1 1 1").await;
    assert!(said[0].contains("Position 1"), "{said:?}");

    let low = holder([0.0, 90.0], "minecraft:blaze_rod");
    editor.run_as(low.clone(), "tuff we brush gravity 3").await;
    editor.click(&low, true, [0, 0, 0]).await;
    assert!(editor.is(5, 98, 5, "glass").await);
    assert!(editor.is(5, 99, 5, "gold_block").await);
    assert!(editor.is(5, 101, 5, "air").await);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn navigation_commands_move_the_player_through_floors() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    editor.select([-2, 99, -2], [2, 99, 2]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    editor.select([-2, 106, -2], [2, 107, 2]).await;
    editor.run("tuff we set stone").await;
    editor.finish().await;
    let at = [0.5, 100.0, 0.5];
    let said = editor
        .run_as(
            Editor::player(PermissionLevel::Gamemasters, at),
            "tuff we ascend",
        )
        .await;
    assert!(said[0].contains("Whoosh"), "{said:?}");
    let said = editor
        .run_as(
            Editor::player(PermissionLevel::Gamemasters, at),
            "tuff we ceil 1",
        )
        .await;
    assert!(said[0].contains("Whoosh"), "{said:?}");
    editor.finish().await;
    assert!(editor.is(0, 102, 0, "glass").await);
    let said = editor
        .run_as(
            Editor::player(PermissionLevel::Gamemasters, at),
            "tuff we up 20",
        )
        .await;
    assert!(said[0].contains("Whoosh"), "{said:?}");
    editor.finish().await;
    assert!(editor.is(0, 119, 0, "glass").await);
    let said = editor
        .run_as(
            Editor::player(PermissionLevel::Gamemasters, [0.5, 120.0, 0.5]),
            "tuff we descend",
        )
        .await;
    assert!(
        said[0].contains("No free spot") || said[0].contains("Whoosh"),
        "{said:?}"
    );
    let said = editor
        .run_as(Editor::player(PermissionLevel::Gamemasters, at), "up 3")
        .await;
    assert!(said[0].contains("Whoosh"), "{said:?}");
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn undo_takes_a_count_and_another_players_history_needs_permission() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    for (y, block) in [(110, "stone"), (111, "dirt"), (112, "obsidian")] {
        editor.select([0, y, 0], [1, y, 1]).await;
        editor.run(&format!("tuff we set {block}")).await;
        editor.finish().await;
    }
    editor.run("tuff we undo 2").await;
    editor.finish().await;
    editor.finish().await;
    assert!(editor.is(0, 110, 0, "stone").await);
    assert!(editor.is(0, 111, 0, "air").await);
    assert!(editor.is(0, 112, 0, "air").await);
    editor.run("tuff we redo 2").await;
    editor.finish().await;
    editor.finish().await;
    assert!(editor.is(0, 112, 0, "obsidian").await);

    let mut other = Editor::player(PermissionLevel::Gamemasters, [0.0, 100.0, 0.0]);
    other.uuid = 78;
    other.name = "Alex".to_owned();
    editor.run_as(other.clone(), "tuff we pos1 5 120 5").await;
    editor.run_as(other.clone(), "tuff we pos2 5 120 5").await;
    editor.run_as(other.clone(), "tuff we set stone").await;
    editor.finish().await;
    let said = editor.run("tuff we undo 1 Alex").await;
    assert!(said[0].contains("permission"), "{said:?}");
    assert!(editor.is(5, 120, 5, "stone").await);
    let admin = Editor::player(PermissionLevel::Owners, [0.0, 100.0, 0.0]);
    editor.run_as(admin, "tuff we undo 1 Alex").await;
    editor.finish().await;
    assert!(editor.is(5, 120, 5, "air").await);

    editor.run_as(other.clone(), "tuff we clearhistory").await;
    let said = editor.run_as(other, "tuff we undo").await;
    assert_eq!(said, ["Nothing to undo"]);
    assert!(editor.run("tuff we timeout 500").await[0].contains("500"));
    assert!(editor.run("tuff we fast on").await[0].contains("enabled"));
    let perf = editor.run("tuff we perf").await;
    assert!(
        perf[0].contains("fast on") && perf[0].contains("500 ms"),
        "{perf:?}"
    );
    editor.world.shutdown().await;
}

async fn spawn_at(editor: &Editor, kind: &str, at: [f64; 3], snbt: &str) {
    use tuff_nbt::Tag;
    let Ok(Tag::Compound(data)) = Tag::from_snbt(snbt) else {
        panic!("bad snbt")
    };
    let (nbt, fresh) = tuff_plugin_wasm::entities::fresh_nbt(kind, Some(data), rand::random());
    tuff_plugin_wasm::entities::apply(
        &editor.world.shared,
        vec![tuff_plugin_wasm::entities::EntityOp::Spawn {
            dimension: crate::world::dimension::DimensionId::default(),
            nbt,
            position: at,
            fresh,
        }],
    )
    .await;
    editor.world.ticks(3).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn butcher_spares_named_and_pets_and_copy_carries_entities() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    let region = editor
        .world
        .region(tuff_world_model::pos::RegionPos::new(0, 0))
        .await;
    block_at(&region, BlockPos::new(5, 100, 5)).await;
    spawn_at(&editor, "minecraft:zombie", [5.5, 100.0, 5.5], "{}").await;
    spawn_at(
        &editor,
        "minecraft:zombie",
        [6.5, 100.0, 5.5],
        "{CustomName:'\"Bob\"'}",
    )
    .await;
    spawn_at(&editor, "minecraft:armor_stand", [4.5, 100.0, 4.5], "{}").await;
    spawn_at(&editor, "minecraft:cow", [7.5, 100.0, 5.5], "{}").await;

    let butcher = editor.run("tuff we butcher 20").await;
    assert!(
        butcher.iter().any(|r| r.contains("Butchering 1 ")),
        "{butcher:?}"
    );
    editor.world.ticks(5).await;
    let again = editor.run("tuff we butcher 20").await;
    assert!(
        again.iter().any(|r| r.contains("Butchering 0 ")),
        "{again:?}"
    );

    editor.select([4, 0, 4], [6, 130, 6]).await;
    let copied = editor.run("tuff we copy -e").await;
    assert!(
        copied.iter().any(|r| r.contains("2 entities")),
        "{copied:?}"
    );
    let all = editor.run("tuff we butcher 20 -at").await;
    assert!(all.iter().any(|r| r.contains("Butchering 2 ")), "{all:?}");

    let there = Editor::player(PermissionLevel::Gamemasters, [50.5, 100.0, 50.5]);
    editor.run_as(there.clone(), "tuff we paste").await;
    editor.finish().await;
    let stands = editor.run_as(there, "tuff we butcher 120 -r").await;
    assert!(
        stands.iter().any(|r| r.contains("Butchering 2 ")),
        "{stands:?}"
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cui_handshake_is_detected_and_drawsel_toggles() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    let steve = Editor::player(PermissionLevel::Gamemasters, [0.0, 100.0, 0.0]);
    let said = editor.run_as(steve.clone(), "tuff we cui").await;
    assert!(said[0].contains("not detected"), "{said:?}");
    let host = editor.world.shared.services.get::<WasmHost>().unwrap();
    host.event(
        Event::Payload(tuff_plugin_wasm::wit::bindings::types::PayloadMessage {
            player: steve.to_wit(),
            channel: "worldedit:cui".to_owned(),
            data: b"v|4".to_vec(),
        }),
        Some(steve.clone()),
    );
    host.sync().await;
    let said = editor.run_as(steve.clone(), "tuff we cui").await;
    assert!(said[0].contains("resent"), "{said:?}");
    let said = editor.run_as(steve, "tuff we drawsel").await;
    assert!(said[0].contains("disabled"), "{said:?}");
    editor.world.shutdown().await;
}

async fn biome_of(editor: &Editor, x: i32, y: i32, z: i32) -> Option<&'static str> {
    use crate::world::messages::{BlockOp, BlockOpResult};
    let pos = BlockPos::new(x, y, z);
    let region = editor.world.region(ChunkPos::of_block(pos).region()).await;
    let (reply, rx) = oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Biomes {
                positions: vec![pos],
                load: false,
            },
            reply,
        })
        .await;
    match rx.await.unwrap() {
        BlockOpResult::Biomes(found) => found.into_iter().next().flatten(),
        _ => None,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn setbiome_changes_cells_in_unloaded_chunks_and_undoes_and_biomeinfo_reads_them() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    assert_eq!(biome_of(&editor, 204, 66, 204).await, None);
    editor.select([200, 64, 200], [215, 71, 215]).await;
    editor.run("tuff we setbiome mushroom_fields").await;
    editor.finish().await;
    let mushroom = Some("minecraft:mushroom_fields");
    assert_eq!(biome_of(&editor, 204, 66, 204).await, mushroom);
    assert_eq!(biome_of(&editor, 215, 71, 215).await, mushroom);
    let before = biome_of(&editor, 216, 66, 216).await;
    assert_ne!(before, mushroom);
    let info = editor.run("tuff we biomeinfo").await.join("\n");
    assert!(info.contains("minecraft:mushroom_fields"), "{info}");
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert_ne!(biome_of(&editor, 204, 66, 204).await, mushroom);
    editor.run("tuff we redo").await;
    editor.finish().await;
    assert_eq!(biome_of(&editor, 204, 66, 204).await, mushroom);
    editor.select([196, 66, 196], [219, 66, 219]).await;
    editor.run("tuff we replace $mushroom_fields glass").await;
    editor.finish().await;
    let glass = |x: i32, z: i32| {
        let editor = &editor;
        async move {
            let pos = BlockPos::new(x, 66, z);
            let region = editor.world.region(ChunkPos::of_block(pos).region()).await;
            block_at(&region, pos)
                .await
                .expect("inside the world")
                .to_string()
        }
    };
    assert!(glass(204, 204).await.starts_with("minecraft:glass"));
    assert!(!glass(218, 218).await.starts_with("minecraft:glass"));
    assert!(!glass(197, 197).await.starts_with("minecraft:glass"));
    let here = editor.run("tuff we biomeinfo -p").await.join("\n");
    assert!(here.contains("Biome at your position"), "{here}");
    let bad = editor.run("tuff we setbiome not_a_biome").await;
    editor.finish().await;
    let _ = bad;
    assert_eq!(biome_of(&editor, 204, 66, 204).await, mushroom);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generatebiome_follows_an_expression_and_copy_paste_carry_biomes_with_the_b_flag() {
    let Some(bytes) = built() else { return };
    let editor = Editor::start(&bytes).await;
    let mushroom = Some("minecraft:mushroom_fields");
    editor.select([200, 64, 200], [215, 71, 215]).await;
    editor
        .run("tuff we generatebiome -r mushroom_fields x<208")
        .await;
    editor.finish().await;
    assert_eq!(biome_of(&editor, 204, 66, 204).await, mushroom);
    assert_ne!(biome_of(&editor, 212, 66, 204).await, mushroom);
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert_ne!(biome_of(&editor, 204, 66, 204).await, mushroom);

    editor.run("tuff we setbiome mushroom_fields").await;
    editor.finish().await;
    editor.run("tuff we copy -b").await;
    editor.run("tuff we setbiome plains").await;
    editor.finish().await;
    assert_ne!(biome_of(&editor, 204, 66, 204).await, mushroom);
    editor.run("tuff we paste -o").await;
    editor.finish().await;
    assert_ne!(biome_of(&editor, 204, 66, 204).await, mushroom);
    editor.run("tuff we paste -o -b").await;
    editor.finish().await;
    assert_eq!(biome_of(&editor, 204, 66, 204).await, mushroom);
    assert_eq!(biome_of(&editor, 215, 71, 215).await, mushroom);
    editor.run("tuff we undo").await;
    editor.finish().await;
    assert_ne!(biome_of(&editor, 204, 66, 204).await, mushroom);
    editor.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regen_restores_generated_terrain_after_clearing_it_and_undo_reverts() {
    use super::dimension_settings::{FLOOR, LOBBY, Lobby};
    let Some(bytes) = built() else { return };
    let editor = Editor::start_with(&bytes, TestDir::new(), vec![Box::new(Lobby(false))]).await;
    let player = || Editor::player_in(LOBBY, PermissionLevel::Gamemasters, [0.0, 100.0, 0.0]);
    let at = |x: i32, y: i32, z: i32| {
        let editor = &editor;
        async move {
            let pos = BlockPos::new(x, y, z);
            let region = editor
                .world
                .region_in(LOBBY, ChunkPos::of_block(pos).region())
                .await;
            block_at(&region, pos)
                .await
                .expect("inside the world")
                .to_string()
        }
    };
    for line in ["tuff we pos1 -6 36 -6", "tuff we pos2 6 46 6"] {
        editor.run_as(player(), line).await;
    }
    editor.run_as(player(), "tuff we set air").await;
    editor.finish().await;
    assert!(at(0, FLOOR, 0).await.starts_with("minecraft:air"));
    let said = editor.run_as(player(), "tuff we regen").await;
    assert!(said[0].contains("regen"), "{said:?}");
    editor.finish().await;
    assert!(at(0, FLOOR, 0).await.starts_with("minecraft:stone"));
    assert!(at(-6, FLOOR - 2, 6).await.starts_with("minecraft:stone"));
    assert!(at(0, FLOOR + 1, 0).await.starts_with("minecraft:air"));
    editor.run_as(player(), "tuff we undo").await;
    editor.finish().await;
    assert!(at(0, FLOOR, 0).await.starts_with("minecraft:air"));
    editor.run_as(player(), "tuff we regen 42").await;
    editor.finish().await;
    assert!(at(0, FLOOR, 0).await.starts_with("minecraft:dirt"));
    editor.run_as(player(), "tuff we regen desert").await;
    editor.finish().await;
    assert!(at(0, FLOOR, 0).await.starts_with("minecraft:sand"));
    editor.run_as(player(), "tuff we regen jungle 7").await;
    editor.finish().await;
    assert!(at(0, FLOOR, 0).await.starts_with("minecraft:sand"));
    let said = editor.run_as(player(), "tuff we regen 1 2").await;
    assert!(said.join(" ").contains("[biome] [seed]"), "{said:?}");
    editor.world.shutdown().await;
}
