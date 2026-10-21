//! Composters filling from players and hoppers, ripening, and giving up
//! bone meal by hand or to a hopper below.

use super::*;
use crate::world::messages::{BlockOp, BlockOpResult, HandEffect};
use tuff_nbt::Tag;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::EntityType;

const Y: i32 = 200;
const X: i32 = 88;
const Z: i32 = 88;

type Events = mpsc::UnboundedReceiver<PlayerEvent>;

async fn start() -> (TestDir, TestWorld, RegionHandle, PlayerHandle, Events) {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    (dir, world, region, player, events)
}

async fn set(region: &RegionHandle, pos: BlockPos, state: BlockState, data: Option<Compound>) {
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

async fn level(region: &RegionHandle, pos: BlockPos) -> Option<u8> {
    tuff_vanilla_common::composter::level(block_at(region, pos).await?)
}

async fn wait_level(world: &TestWorld, region: &RegionHandle, pos: BlockPos, want: u8) {
    let mut last = None;
    for _ in 0..100 {
        last = level(region, pos).await;
        if last == Some(want) {
            return;
        }
        world.ticks(2).await;
    }
    panic!("the composter is at {last:?}, expected {want}");
}

async fn click(region: &RegionHandle, events: &mut Events, mut place: Place) -> ActionOutcome {
    let sequence = place.sequence;
    place.item = None;
    region.request(RegionRequest::Place(place)).await;
    expect(events, |event| match event {
        PlayerEvent::ActionResult {
            sequence: seq,
            outcome,
        } if seq == sequence => Some(outcome),
        _ => None,
    })
    .await
}

fn empty_hand(player: &PlayerHandle, target: BlockPos, sequence: i32) -> Place {
    let mut place = place(player, target, Item::Stick, sequence);
    place.held = None;
    place
}

async fn bone_meal_items(region: &RegionHandle) -> usize {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer
        .await
        .unwrap()
        .into_iter()
        .filter(|entity| entity.kind == EntityType::Item)
        .count()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_composter_fills_from_clicks_ripens_and_gives_bone_meal() {
    let (_dir, world, region, player, mut events) = start().await;
    let floor = BlockPos::new(X, Y - 1, Z);
    let pos = BlockPos::new(X, Y, Z);
    set(&region, floor, Block::Stone.default_state(), None).await;
    set(&region, pos, Block::Composter.default_state(), None).await;

    let outcome = click(&region, &mut events, place(&player, pos, Item::Stick, 1)).await;
    assert!(
        !matches!(outcome, ActionOutcome::Used { .. }),
        "sticks do not compost: {outcome:?}"
    );
    assert_eq!(level(&region, pos).await, Some(0));

    for sequence in 2..9 {
        let outcome = click(
            &region,
            &mut events,
            place(&player, pos, Item::Cake, sequence),
        )
        .await;
        assert!(
            matches!(
                outcome,
                ActionOutcome::Used {
                    item: Item::Cake,
                    effect: HandEffect::Consume(1),
                    ..
                }
            ),
            "{outcome:?}"
        );
    }
    assert_eq!(level(&region, pos).await, Some(7));
    let refused = click(&region, &mut events, place(&player, pos, Item::Cake, 9)).await;
    assert!(
        matches!(refused, ActionOutcome::Interacted { .. }),
        "a full composter takes nothing more: {refused:?}"
    );

    wait_level(&world, &region, pos, 8).await;
    assert_eq!(bone_meal_items(&region).await, 0);
    let outcome = click(&region, &mut events, empty_hand(&player, pos, 10)).await;
    assert!(
        matches!(outcome, ActionOutcome::Interacted { .. }),
        "{outcome:?}"
    );
    assert_eq!(level(&region, pos).await, Some(0));
    assert_eq!(bone_meal_items(&region).await, 1);
    world.shutdown().await;
}

fn hopper_items(item: &str, count: i8) -> Compound {
    Compound::new().with(
        "Items",
        Tag::List(vec![Tag::Compound(
            Compound::new()
                .with("Slot", 0i8)
                .with("id", item)
                .with("count", i32::from(count)),
        )]),
    )
}

async fn hopper_contents(region: &RegionHandle, pos: BlockPos) -> Vec<(String, i32)> {
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
    let Some(Some((_, Some(nbt)))) = copied.into_iter().next() else {
        return Vec::new();
    };
    let Some(Tag::List(items)) = nbt.get("Items") else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|tag| match tag {
            Tag::Compound(item) => Some((
                item.get_str("id")?.to_owned(),
                item.get_int("count").unwrap_or(1),
            )),
            _ => None,
        })
        .collect()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hoppers_feed_a_composter_from_above_and_empty_it_from_below() {
    let (_dir, world, region, _player, _events) = start().await;
    let below = BlockPos::new(X, Y - 1, Z);
    let pos = BlockPos::new(X, Y, Z);
    let above = BlockPos::new(X, Y + 1, Z);
    let down = Block::Hopper.default_state();
    set(
        &region,
        BlockPos::new(X, Y - 2, Z),
        Block::Stone.default_state(),
        None,
    )
    .await;
    set(&region, below, down, None).await;
    set(&region, pos, Block::Composter.default_state(), None).await;
    set(
        &region,
        above,
        down,
        Some(hopper_items("minecraft:cake", 10)),
    )
    .await;

    let mut last = Vec::new();
    for _ in 0..100 {
        last = hopper_contents(&region, below).await;
        if last == [("minecraft:bone_meal".to_owned(), 1)] {
            break;
        }
        world.ticks(2).await;
    }
    assert_eq!(last, [("minecraft:bone_meal".to_owned(), 1)]);
    wait_level(&world, &region, pos, 3).await;
    assert!(hopper_contents(&region, above).await.is_empty());
    assert_eq!(
        hopper_contents(&region, below).await,
        [("minecraft:bone_meal".to_owned(), 1)]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropper_feeds_a_composter_only_from_above() {
    let (_dir, world, region, _player, _events) = start().await;
    let pos = BlockPos::new(X, Y, Z);
    let above = BlockPos::new(X, Y + 1, Z);
    let side = BlockPos::new(X - 1, Y, Z);
    set(
        &region,
        BlockPos::new(X, Y - 1, Z),
        Block::Stone.default_state(),
        None,
    )
    .await;
    set(&region, pos, Block::Composter.default_state(), None).await;
    let dropper = |facing: &str| -> BlockState {
        format!("minecraft:dropper[facing={facing}]")
            .parse()
            .unwrap()
    };
    set(
        &region,
        above,
        dropper("down"),
        Some(hopper_items("minecraft:cake", 10)),
    )
    .await;
    set(
        &region,
        side,
        dropper("east"),
        Some(hopper_items("minecraft:cake", 10)),
    )
    .await;
    world.ticks(2).await;
    for power in [BlockPos::new(X, Y + 2, Z), BlockPos::new(X - 2, Y, Z)] {
        set(&region, power, Block::RedstoneBlock.default_state(), None).await;
    }
    world.ticks(10).await;
    assert_eq!(level(&region, pos).await, Some(1), "one cake from above");
    assert_eq!(
        hopper_contents(&region, above).await,
        [("minecraft:cake".to_owned(), 9)]
    );
    assert_eq!(
        hopper_contents(&region, side).await,
        [("minecraft:cake".to_owned(), 10)],
        "a composter takes nothing through its side"
    );
    assert_eq!(bone_meal_items(&region).await, 0);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hopper_minecart_takes_bone_meal_from_a_ripe_composter_above() {
    let (_dir, world, region, _player, _events) = start().await;
    let pos = BlockPos::new(X, Y, Z);
    let track = BlockPos::new(X, Y - 1, Z);
    set(
        &region,
        BlockPos::new(X, Y - 2, Z),
        Block::Stone.default_state(),
        None,
    )
    .await;
    set(&region, track, Block::Rail.default_state(), None).await;
    let ripe = tuff_vanilla_common::composter::with_level(Block::Composter.default_state(), 8);
    set(&region, pos, ripe, None).await;
    summon(
        &region,
        EntityType::HopperMinecart,
        Compound::new(),
        [
            f64::from(X) + 0.5,
            f64::from(Y - 1) + 0.0625,
            f64::from(Z) + 0.5,
        ],
    )
    .await;
    world.ticks(6).await;
    assert_eq!(level(&region, pos).await, Some(0));
    let cart = mobs(&region, EntityType::HopperMinecart).await[0].id;
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id: cart, reply })
        .await;
    let data = answer.await.unwrap().expect("the cart exists");
    let items = data.get_list("Items").unwrap_or(&[]);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(
        items[0].as_compound().and_then(|i| i.get_str("id")),
        Some("minecraft:bone_meal")
    );
    world.shutdown().await;
}

async fn pot_item(region: &RegionHandle, pos: BlockPos) -> Option<(String, i32)> {
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
    let Some(Some((_, Some(nbt)))) = copied.into_iter().next() else {
        return None;
    };
    let item = nbt.get_compound("item")?;
    Some((
        item.get_str("id")?.to_owned(),
        item.get_int("count").unwrap_or(1),
    ))
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hoppers_and_droppers_fill_a_decorated_pot() {
    let (_dir, world, region, _player, _events) = start().await;
    let pot = BlockPos::new(X, Y, Z);
    let above = BlockPos::new(X, Y + 1, Z);
    let side = BlockPos::new(X - 1, Y, Z);
    set(
        &region,
        BlockPos::new(X, Y - 1, Z),
        Block::Stone.default_state(),
        None,
    )
    .await;
    set(&region, pot, Block::DecoratedPot.default_state(), None).await;
    set(
        &region,
        above,
        Block::Hopper.default_state(),
        Some(hopper_items("minecraft:stick", 3)),
    )
    .await;
    let dropper: BlockState = "minecraft:dropper[facing=east]".parse().unwrap();
    set(
        &region,
        side,
        dropper,
        Some(hopper_items("minecraft:stick", 2)),
    )
    .await;
    let mut last = None;
    for _ in 0..100 {
        last = pot_item(&region, pot).await;
        if last == Some(("minecraft:stick".to_owned(), 3)) {
            break;
        }
        world.ticks(2).await;
    }
    assert_eq!(last, Some(("minecraft:stick".to_owned(), 3)));
    set(
        &region,
        BlockPos::new(X - 2, Y, Z),
        Block::RedstoneBlock.default_state(),
        None,
    )
    .await;
    world.ticks(10).await;
    assert_eq!(
        pot_item(&region, pot).await,
        Some(("minecraft:stick".to_owned(), 4))
    );
    assert_eq!(
        hopper_contents(&region, side).await,
        [("minecraft:stick".to_owned(), 1)]
    );
    world.shutdown().await;
}
