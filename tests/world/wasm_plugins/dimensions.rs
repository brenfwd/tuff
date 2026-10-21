//! Dimensions a plugin makes and discards while the server runs: the
//! `cap.dimension` grant, copying a base's saved chunks, chunks kept out
//! of the world's storage, and the discard freeing the key.

use super::*;
use crate::world::messages::{BlockOp, ManagerMessage, RegionHandle, RegionRequest};
use tuff_plugin_wasm::wit::bindings::types::ChunkArea;
use tuff_vanilla_data::BlockState;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::RegionPos;

const ARENA: &str = "game:arena_1";
const NOT_GRANTED: i32 = 0;
const INVALID_KEY: i32 = 1;
const EXISTS: i32 = 2;
const UNKNOWN_DIMENSION: i32 = 3;
const NOT_OWNED: i32 = 4;
const OVER_BUDGET: i32 = 5;
const OK: i32 = -1;

fn area(min: i32, max: i32) -> WitValue<'static> {
    Some(ChunkArea {
        min_x: min,
        min_z: min,
        max_x: max,
        max_z: max,
    })
    .to_value()
    .into_owned()
}

/// Commands `make`, `bad`, `lobby`, `nobase`, `huge`, `drop` return the
/// dimension error case of their call (-1 for ok); `count` returns how
/// many dimensions the plugin has made.
fn arenas() -> Vec<u8> {
    let mut data = Data::new();
    let names = ["make", "bad", "lobby", "nobase", "huge", "drop", "count"];
    let register = register(&mut data, &names, "");
    let none = None::<ChunkArea>.to_value().into_owned();
    let calls = [
        data.call("create-dimension", &[s(ARENA), s("overworld"), area(0, 0)]),
        data.call(
            "create-dimension",
            &[s("game:Arena"), s("overworld"), none.clone()],
        ),
        data.call(
            "create-dimension",
            &[s("game:lobby"), s("overworld"), none.clone()],
        ),
        data.call("create-dimension", &[s("game:arena_2"), s("nowhere"), none]),
        data.call(
            "create-dimension",
            &[s("game:arena_3"), s("overworld"), area(0, 64)],
        ),
        data.call("discard-dimension", &[s(ARENA)]),
    ];
    let count = data.call("runtime-dimensions", &[]);
    let code = ok_code(&world_error());
    let branches: String = calls
        .iter()
        .enumerate()
        .map(|(handler, call)| {
            format!(
                "(if (i32.eq (local.get $handler) (i32.const {handler})) (then {call} (return {code})))"
            )
        })
        .collect();
    let body = format!(
        "{register}
         (func {run} (param $handler i32) (param i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)
           {branches}
           {count}
           {listed})
         {}",
        data.wat(),
        run = export("run-command").replace(
            " (param i32 i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)",
            ""
        ),
        listed = ok_code(&format!("(i32.load (i32.const {}))", RET + 4)),
    );
    module(
        "id = arenas\nversion = 1.0.0\napi = 0\n\
         cap.dimension = game:arena_* -- One arena per match",
        &body,
    )
}

async fn put(region: &RegionHandle, pos: BlockPos, state: BlockState) {
    let (reply, done) = tokio::sync::oneshot::channel();
    region
        .request(RegionRequest::Blocks {
            op: BlockOp::Set {
                changes: vec![(pos, state)],
                destroy: false,
                keep: false,
                strict: false,
                quiet: false,
                data: None,
            },
            reply,
        })
        .await;
    done.await.unwrap();
}

/// The state at `pos` in `dimension`, loading its chunk.
async fn read(world: &TestWorld, dimension: &str, pos: BlockPos) -> Option<BlockState> {
    let dimension = world.dimension(dimension);
    tuff_plugin_wasm::world::read(world.shared.clone(), dimension, vec![pos]).await[0]
}

async fn save_all(world: &TestWorld) {
    let (done, saved) = tokio::sync::oneshot::channel();
    world
        .shared
        .manager
        .send(ManagerMessage::SaveAll {
            done: Some(done),
            snapshotted: None,
        })
        .await
        .unwrap();
    saved.await.unwrap();
}

/// Waits until no runtime dimension holds `key` any more.
async fn freed(world: &TestWorld, key: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while world.shared.dimensions.runtime_taken(key) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the discard finishes");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugins_make_copy_and_discard_dimensions_at_runtime() {
    let dir = TestDir::new();
    install(dir.path(), "arenas.wasm", &arenas());
    let backend = open_sqlite(dir.path());
    let world = start_sqlite(dir.path(), backend.clone()).await;
    let steve = || Player::new(77, "Steve");
    let run = |line: &'static str| {
        let world = &world;
        async move {
            host(world)
                .command(steve(), line.into())
                .await
                .expect("the plugin owns the command")
                .result
        }
    };
    let stone: BlockState = "minecraft:stone".parse().unwrap();
    let glass: BlockState = "minecraft:glass".parse().unwrap();
    let (marked, built) = (BlockPos::new(1, 100, 1), BlockPos::new(2, 100, 2));
    let outside = BlockPos::new(17, 100, 1);

    assert_eq!(run("make").await, Ok(NOT_GRANTED));
    let granted = console(&world, "tuff plugin grant arenas dimension game:arena_*").await;
    assert_eq!(granted, ["Granted dimension game:arena_*"]);

    let overworld = world.region(RegionPos::new(0, 0)).await;
    put(&overworld, marked, stone).await;
    put(&overworld, outside, stone).await;
    save_all(&world).await;

    assert_eq!(run("make").await, Ok(OK));
    assert_eq!(run("make").await, Ok(EXISTS));
    assert_eq!(run("bad").await, Ok(INVALID_KEY));
    assert_eq!(run("lobby").await, Ok(NOT_GRANTED));
    assert_eq!(run("nobase").await, Ok(UNKNOWN_DIMENSION));
    assert_eq!(run("huge").await, Ok(OVER_BUDGET));
    assert_eq!(run("count").await, Ok(1));

    let id = world.dimension(ARENA);
    assert!(world.shared.dimensions.is_runtime(id));
    assert_eq!(
        world.shared.dimension(id).type_id(),
        world
            .shared
            .dimension(world.dimension("minecraft:overworld"))
            .type_id()
    );
    assert_eq!(read(&world, ARENA, marked).await, Some(stone), "copied");
    assert_ne!(
        read(&world, ARENA, outside).await,
        Some(stone),
        "outside the copied area"
    );
    let arena = world.region_in(ARENA, RegionPos::new(0, 0)).await;
    put(&arena, built, glass).await;
    assert_eq!(read(&world, ARENA, built).await, Some(glass));
    save_all(&world).await;
    assert_ne!(
        read(&world, "minecraft:overworld", built).await,
        Some(glass)
    );
    let stored = backend.list_dimensions().unwrap();
    assert!(!stored.iter().any(|key| key == ARENA), "{stored:?}");

    assert_eq!(run("drop").await, Ok(OK));
    assert_eq!(run("drop").await, Ok(NOT_OWNED));
    assert_eq!(run("count").await, Ok(0));
    assert!(world.shared.dimensions.by_key(ARENA).is_none());
    freed(&world, ARENA).await;

    assert_eq!(run("make").await, Ok(OK), "the key is free again");
    assert_eq!(read(&world, ARENA, marked).await, Some(stone));
    assert_ne!(
        read(&world, ARENA, built).await,
        Some(glass),
        "a new arena starts from the base"
    );

    let unloaded = console(&world, "tuff plugin unload arenas").await;
    assert!(!unloaded.is_empty(), "{unloaded:?}");
    assert!(world.shared.dimensions.by_key(ARENA).is_none());
    freed(&world, ARENA).await;
    world.shutdown().await;
}
