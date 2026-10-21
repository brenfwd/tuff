//! Jukeboxes: hoppers and droppers load records, a playing record powers
//! the blocks around it until its song ends, and a hopper below unloads
//! it once the song is over.

use super::*;
use crate::world::messages::{BlockOp, BlockOpResult};
use crate::world::region_api::RegionTask;
use tuff_nbt::Tag;
use tuff_vanilla_data::BlockState;

const Y: i32 = 200;
const X: i32 = 88;
const Z: i32 = 88;
const SONG_13_TICKS: i64 = 178 * 20;

pub(super) async fn start() -> (
    TestDir,
    TestWorld,
    RegionHandle,
    PlayerHandle,
    mpsc::UnboundedReceiver<PlayerEvent>,
) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    (dir, world, region, player, events)
}

pub(super) async fn set(
    region: &RegionHandle,
    pos: BlockPos,
    state: BlockState,
    data: Option<Compound>,
) {
    blocks(
        region,
        BlockOp::Set {
            changes: vec![(pos, state)],
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data,
        },
    )
    .await;
}

fn disc_items() -> Compound {
    Compound::new().with(
        "Items",
        Tag::List(vec![Tag::Compound(
            Compound::new()
                .with("Slot", 0i8)
                .with("id", "minecraft:music_disc_13")
                .with("count", 1i32),
        )]),
    )
}

pub(super) async fn saved(region: &RegionHandle, pos: BlockPos) -> Compound {
    let BlockOpResult::Copied(copied) = blocks(
        region,
        BlockOp::Copy {
            positions: vec![pos],
        },
    )
    .await
    else {
        panic!("not a copy");
    };
    match copied.into_iter().next() {
        Some(Some((_, Some(nbt)))) => nbt,
        _ => Compound::new(),
    }
}

async fn edit(
    region: &RegionHandle,
    pos: BlockPos,
    edit: impl FnOnce(&mut Compound) + Send + 'static,
) {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: pos,
            max: pos,
            run: RegionTask::new(move |world| {
                if let Some(nbt) = world.raw_block_entity_mut(pos) {
                    edit(nbt);
                }
                let _ = reply.send(());
            }),
        })
        .await;
    answer.await.unwrap();
}

pub(super) async fn prop(region: &RegionHandle, pos: BlockPos, name: &str) -> Option<String> {
    block_at(region, pos)
        .await
        .and_then(|s| s.get(name).map(str::to_owned))
}

pub(super) async fn wait_prop(
    world: &TestWorld,
    region: &RegionHandle,
    pos: BlockPos,
    name: &str,
    want: &str,
) {
    for _ in 0..60 {
        if prop(region, pos, name).await.as_deref() == Some(want) {
            return;
        }
        world.ticks(1).await;
    }
    panic!(
        "{name} at {pos:?} is {:?}, expected {want}",
        prop(region, pos, name).await
    );
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hopper_loads_a_record_that_powers_a_note_block_until_the_song_ends() {
    let (_dir, world, region, _player, _events) = start().await;
    let jukebox = BlockPos::new(X, Y, Z);
    let note = BlockPos::new(X + 1, Y, Z);
    set(
        &region,
        BlockPos::new(X, Y - 1, Z),
        Block::Stone.default_state(),
        None,
    )
    .await;
    set(
        &region,
        BlockPos::new(X + 1, Y - 1, Z),
        Block::Stone.default_state(),
        None,
    )
    .await;
    set(&region, jukebox, Block::Jukebox.default_state(), None).await;
    set(&region, note, Block::NoteBlock.default_state(), None).await;
    set(
        &region,
        BlockPos::new(X, Y + 1, Z),
        Block::Hopper.default_state(),
        Some(disc_items()),
    )
    .await;

    wait_prop(&world, &region, jukebox, "has_record", "true").await;
    let nbt = saved(&region, jukebox).await;
    assert_eq!(
        nbt.get_compound("RecordItem").and_then(|r| r.get_str("id")),
        Some("minecraft:music_disc_13")
    );
    assert!(nbt.get_long("ticks_since_song_started").is_some());
    wait_prop(&world, &region, note, "powered", "true").await;

    world.ticks(5).await;
    let played = saved(&region, jukebox)
        .await
        .get_long("ticks_since_song_started");
    assert!(
        played.is_some_and(|t| t >= 5),
        "the song advances: {played:?}"
    );

    edit(&region, jukebox, |nbt| {
        nbt.insert("ticks_since_song_started", Tag::Long(SONG_13_TICKS + 18));
    })
    .await;
    world.ticks(1).await;
    assert_eq!(
        prop(&region, note, "powered").await.as_deref(),
        Some("true")
    );
    wait_prop(&world, &region, note, "powered", "false").await;
    let nbt = saved(&region, jukebox).await;
    assert!(
        !nbt.contains_key("ticks_since_song_started"),
        "the song ended"
    );
    assert!(nbt.contains_key("RecordItem"), "the record stays in");
    assert_eq!(
        prop(&region, jukebox, "has_record").await.as_deref(),
        Some("true")
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hopper_below_is_locked_while_the_song_plays_then_takes_the_record() {
    let (_dir, world, region, _player, _events) = start().await;
    let jukebox = BlockPos::new(X, Y, Z);
    let hopper = BlockPos::new(X, Y - 1, Z);
    set(
        &region,
        BlockPos::new(X, Y - 2, Z),
        Block::Stone.default_state(),
        None,
    )
    .await;
    set(&region, hopper, Block::Hopper.default_state(), None).await;
    let record = Compound::new()
        .with("id", "minecraft:music_disc_13")
        .with("count", 1i32);
    set(
        &region,
        jukebox,
        "minecraft:jukebox[has_record=true]".parse().unwrap(),
        Some(
            Compound::new()
                .with("RecordItem", record)
                .with("ticks_since_song_started", SONG_13_TICKS),
        ),
    )
    .await;

    wait_prop(&world, &region, hopper, "enabled", "false").await;
    world.ticks(10).await;
    assert_eq!(
        prop(&region, jukebox, "has_record").await.as_deref(),
        Some("true"),
        "the playing jukebox powers the hopper off"
    );
    wait_prop(&world, &region, jukebox, "has_record", "false").await;
    let nbt = saved(&region, jukebox).await;
    assert!(!nbt.contains_key("RecordItem"));
    assert!(!nbt.contains_key("ticks_since_song_started"));
    let items = saved(&region, hopper).await;
    let Some(Tag::List(items)) = items.get("Items") else {
        panic!("the hopper holds the record");
    };
    assert_eq!(
        items
            .first()
            .and_then(Tag::as_compound)
            .and_then(|i| i.get_str("id")),
        Some("minecraft:music_disc_13")
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropper_loads_a_record_but_not_other_items() {
    let (_dir, world, region, _player, _events) = start().await;
    let jukebox = BlockPos::new(X, Y, Z);
    let dropper = BlockPos::new(X - 1, Y, Z);
    set(
        &region,
        BlockPos::new(X, Y - 1, Z),
        Block::Stone.default_state(),
        None,
    )
    .await;
    set(
        &region,
        BlockPos::new(X - 1, Y - 1, Z),
        Block::Stone.default_state(),
        None,
    )
    .await;
    set(&region, jukebox, Block::Jukebox.default_state(), None).await;
    let items = Compound::new().with(
        "Items",
        Tag::List(vec![Tag::Compound(
            Compound::new()
                .with("Slot", 0i8)
                .with("id", "minecraft:music_disc_13")
                .with("count", 1i32),
        )]),
    );
    set(
        &region,
        dropper,
        "minecraft:dropper[facing=east]".parse().unwrap(),
        Some(items),
    )
    .await;
    set(
        &region,
        BlockPos::new(X - 2, Y, Z),
        Block::RedstoneBlock.default_state(),
        None,
    )
    .await;
    wait_prop(&world, &region, jukebox, "has_record", "true").await;
    let left = saved(&region, dropper).await;
    assert!(
        !matches!(left.get("Items"), Some(Tag::List(items)) if !items.is_empty()),
        "the record left the dropper"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_placed_redstone_block_powers_adjacent_bell_and_note_block() {
    let (_dir, world, region, _player, _events) = start().await;
    let bell = BlockPos::new(X, Y, Z);
    let note = BlockPos::new(X, Y, Z + 4);
    for p in [bell, note] {
        set(
            &region,
            BlockPos::new(p.x, p.y - 1, p.z),
            Block::Stone.default_state(),
            None,
        )
        .await;
    }
    set(&region, bell, Block::Bell.default_state(), None).await;
    set(&region, note, Block::NoteBlock.default_state(), None).await;
    world.ticks(2).await;
    set(
        &region,
        BlockPos::new(X + 1, Y, Z),
        Block::RedstoneBlock.default_state(),
        None,
    )
    .await;
    set(
        &region,
        BlockPos::new(X + 1, Y, Z + 4),
        Block::RedstoneBlock.default_state(),
        None,
    )
    .await;
    wait_prop(&world, &region, bell, "powered", "true").await;
    wait_prop(&world, &region, note, "powered", "true").await;
}
