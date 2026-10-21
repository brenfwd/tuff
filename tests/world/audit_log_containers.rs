//! The audit log's container rows: what players, hoppers and hopper
//! minecarts put in and took out, item frames, lookups by item, and
//! rolling container contents back and forward.

use super::*;
use crate::world::messages::BlockOp;
use tuff_feature_builtin_audit_log::AuditLogFeature;
use tuff_feature_builtin_audit_log::filter::Filter;
use tuff_feature_builtin_audit_log::record::Kind;
use tuff_feature_builtin_audit_log::store::Order;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, BlockState, Item};

fn start(dir: &Path) -> TestWorld {
    TestWorld::start_with_features(
        dir,
        vec![Box::new(AuditLogFeature::at(dir.join("audit.sqlite")))],
    )
}

async fn console(world: &TestWorld, line: &str) -> Vec<String> {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(|line| line.plain())
        .collect()
}

async fn paste(region: &RegionHandle, pos: BlockPos, state: &str, nbt: Option<Compound>) {
    let state: BlockState = state.parse().unwrap();
    let (reply, answer) = oneshot::channel();
    let op = BlockOp::Paste {
        changes: vec![(pos, state, nbt)],
        strict: false,
    };
    region.request(RegionRequest::Blocks { op, reply }).await;
    answer.await.unwrap();
}

fn chest_with(id: &str, items: &[(i8, Item, i32)]) -> Compound {
    let items = items
        .iter()
        .map(|(slot, item, count)| {
            Tag::Compound(
                Compound::new()
                    .with("Slot", Tag::Byte(*slot))
                    .with("id", item.name())
                    .with("count", Tag::Int(*count)),
            )
        })
        .collect();
    Compound::new()
        .with("id", id)
        .with("Items", Tag::List(items))
}

/// The items saved in the block entity or entity NBT `nbt`, by slot.
fn items(nbt: Option<&Compound>) -> Vec<(i8, String, i32)> {
    let Some(Tag::List(slots)) = nbt.and_then(|nbt| nbt.get("Items")) else {
        return Vec::new();
    };
    slots
        .iter()
        .filter_map(|slot| {
            let slot = slot.as_compound()?;
            Some((
                slot.get_byte("Slot")?,
                slot.get_str("id")?.to_owned(),
                slot.get_int("count")?,
            ))
        })
        .collect()
}

async fn contents(region: &RegionHandle, pos: BlockPos) -> Vec<(i8, String, i32)> {
    let (reply, answer) = oneshot::channel();
    let op = BlockOp::Copy {
        positions: vec![pos],
    };
    region.request(RegionRequest::Blocks { op, reply }).await;
    match answer.await.unwrap() {
        crate::world::messages::BlockOpResult::Copied(mut found) => {
            items(found.remove(0).and_then(|(_, nbt)| nbt).as_ref())
        }
        other => panic!("{other:?}"),
    }
}

async fn entity_items(region: &RegionHandle, id: i32) -> Vec<(i8, String, i32)> {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    items(answer.await.unwrap().as_ref())
}

async fn rows(
    world: &TestWorld,
    kinds: Vec<Kind>,
) -> Vec<tuff_feature_builtin_audit_log::store::Row> {
    let log = world
        .shared
        .services
        .get::<tuff_feature_builtin_audit_log::AuditLog>()
        .expect("the service runs");
    log.sync().await.unwrap();
    log.lookup(
        Filter {
            kinds,
            ..Filter::default()
        },
        Order::OldestFirst,
        100,
        0,
    )
    .await
    .unwrap()
}

async fn click(
    region: &RegionHandle,
    player: &PlayerHandle,
    events: &mut mpsc::UnboundedReceiver<PlayerEvent>,
    changes: Vec<ContainerChange>,
) {
    region
        .request(RegionRequest::ContainerTransaction {
            player: player.clone(),
            window_id: 1,
            changes,
            took_result: false,
        })
        .await;
    assert!(expect(events, transaction_answer).await.0);
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn player_clicks_log_net_item_rows_that_roll_back_and_restore() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (alex, mut events) = PlayerHandle::new(100);
    world.publish_players("minecraft:overworld", &[(&alex, [2.5, 201.0, 3.5])]);
    subscribe(&region, &alex, &mut events, &[chunk]).await;
    let chest = BlockPos::new(2, 200, 3);
    let saved = chest_with("minecraft:chest", &[(0, Item::Diamond, 5)]);
    paste(&region, chest, "minecraft:chest[facing=north]", Some(saved)).await;
    region
        .request(RegionRequest::Place(place(&alex, chest, Item::Torch, 1)))
        .await;
    expect(&mut events, container_menu).await;

    let diamonds = |count| Some(ItemStack::mint(Item::Diamond, count, MintReason::Test));
    let stone = |count| Some(ItemStack::mint(Item::Stone, count, MintReason::Test));
    click(
        &region,
        &alex,
        &mut events,
        set_slot(chest, 0, diamonds(5), diamonds(2)),
    )
    .await;
    let mut spread = set_slot(chest, 1, None, stone(2));
    spread.extend(set_slot(chest, 2, None, stone(2)));
    click(&region, &alex, &mut events, spread).await;
    world.ticks(2).await;

    let logged = rows(&world, vec![Kind::ContainerAdd, Kind::ContainerRemove]).await;
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert_eq!(logged[0].kind, Some(Kind::ContainerRemove));
    assert_eq!(logged[0].user, "player100");
    assert_eq!(logged[0].material.as_deref(), Some("minecraft:diamond"));
    assert_eq!(logged[0].amount, 3);
    assert_eq!(logged[0].pos, [2, 200, 3]);
    assert_eq!(logged[1].kind, Some(Kind::ContainerAdd));
    assert_eq!(logged[1].amount, 4, "one click over two slots is one row");

    let lookup = console(&world, "tuff audit lookup a:container").await;
    assert!(
        lookup
            .iter()
            .any(|l| l.contains("player100 removed 3 diamond (2 200 3)")),
        "{lookup:?}"
    );
    assert!(
        lookup.iter().any(|l| l.contains("player100 added 4 stone")),
        "{lookup:?}"
    );
    assert_eq!(
        console(&world, "tuff audit lookup i:diamond a:container #count").await,
        ["1 rows"]
    );
    assert_eq!(
        console(&world, "tuff audit lookup a:+container e:stone #count").await,
        ["0 rows"]
    );

    let rolled = console(&world, "tuff audit rollback t:1h r:#global a:container").await;
    assert_eq!(
        rolled,
        ["Rolled back 0 blocks and 7 items from 2 rows in 1 regions"]
    );
    assert_eq!(
        contents(&region, chest).await,
        [(0, "minecraft:diamond".to_owned(), 5)]
    );
    let lookup = console(&world, "tuff audit lookup a:container").await;
    assert!(lookup[1].starts_with("[undone]"), "{lookup:?}");
    world.ticks(2).await;
    assert_eq!(
        rows(&world, vec![Kind::ContainerAdd, Kind::ContainerRemove])
            .await
            .len(),
        2,
        "a rollback is not logged as new container rows"
    );

    let restored = console(&world, "tuff audit restore t:1h r:#global").await;
    assert_eq!(
        restored,
        ["Restored 0 blocks and 7 items from 2 rows in 1 regions"]
    );
    let mut now = contents(&region, chest).await;
    now.sort();
    assert_eq!(
        now,
        [
            (0, "minecraft:diamond".to_owned(), 2),
            (2, "minecraft:stone".to_owned(), 4),
        ]
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hoppers_log_both_ends_of_a_move_under_their_cause() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let chest = BlockPos::new(2, 201, 3);
    let hopper = BlockPos::new(2, 200, 3);
    paste(&region, hopper, "minecraft:hopper[facing=down]", None).await;
    let saved = chest_with("minecraft:chest", &[(0, Item::Diamond, 2)]);
    paste(&region, chest, "minecraft:chest[facing=north]", Some(saved)).await;
    world.ticks(30).await;
    assert_eq!(
        contents(&region, hopper).await,
        [(0, "minecraft:diamond".to_owned(), 2)]
    );

    let logged = rows(&world, vec![Kind::ContainerAdd, Kind::ContainerRemove]).await;
    assert!(logged.iter().all(|row| row.user == "#hopper"), "{logged:?}");
    let moved = |kind, pos: BlockPos| -> i64 {
        logged
            .iter()
            .filter(|row| row.kind == Some(kind) && row.pos == [pos.x, pos.y, pos.z])
            .map(|row| row.amount)
            .sum()
    };
    assert_eq!(moved(Kind::ContainerRemove, chest), 2);
    assert_eq!(moved(Kind::ContainerAdd, hopper), 2);

    let rolled = console(&world, "tuff audit rollback t:1h r:#global u:#hopper").await;
    assert!(rolled[0].contains("and 4 items"), "{rolled:?}");
    assert_eq!(
        contents(&region, chest).await,
        [(0, "minecraft:diamond".to_owned(), 2)]
    );
    assert_eq!(contents(&region, hopper).await, []);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hopper_minecarts_take_rolls_back_into_the_chest_and_out_of_the_cart() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (player, mut events) = PlayerHandle::new(100);
    subscribe(&region, &player, &mut events, &[chunk]).await;
    place_blocks(&world, &region, pen(2, 13)).await;
    let track = BlockPos::new(6, SKY + 1, 6);
    let top = BlockPos::new(6, SKY + 2, 6);
    paste(&region, track, "minecraft:rail[shape=north_south]", None).await;
    let saved = chest_with("minecraft:chest", &[(3, Item::Stone, 3)]);
    paste(&region, top, "minecraft:chest[facing=north]", Some(saved)).await;
    summon(
        &region,
        EntityType::HopperMinecart,
        Compound::new(),
        [6.5, SKY as f64 + 1.0625, 6.5],
    )
    .await;
    let cart = mobs(&region, EntityType::HopperMinecart).await[0].id;
    world.ticks(20).await;
    assert_eq!(contents(&region, top).await, []);
    assert_eq!(
        entity_items(&region, cart).await,
        [(0, "minecraft:stone".to_owned(), 3)]
    );

    let logged = rows(&world, vec![Kind::ContainerAdd, Kind::ContainerRemove]).await;
    assert!(
        logged.iter().all(|row| row.user == "#hopper_minecart"),
        "{logged:?}"
    );
    assert!(
        logged
            .iter()
            .any(|row| row.kind == Some(Kind::ContainerAdd) && row.pos == [6, SKY + 1, 6]),
        "the cart's rows are where it stands: {logged:?}"
    );

    let rolled = console(&world, "tuff audit rollback t:1h r:#global").await;
    assert!(rolled[0].contains("and 6 items"), "{rolled:?}");
    assert_eq!(
        contents(&region, top).await,
        [(3, "minecraft:stone".to_owned(), 3)]
    );
    assert_eq!(entity_items(&region, cart).await, []);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn putting_an_item_in_a_frame_is_a_container_row() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (alex, mut events) = PlayerHandle::new(100);
    world.publish_players("minecraft:overworld", &[(&alex, [6.5, 201.0, 7.5])]);
    subscribe(&region, &alex, &mut events, &[chunk]).await;
    paste(&region, BlockPos::new(6, 200, 5), "minecraft:stone", None).await;
    summon(
        &region,
        EntityType::ItemFrame,
        Compound::new()
            .with("Facing", Tag::Byte(3))
            .with("TileX", Tag::Int(6))
            .with("TileY", Tag::Int(200))
            .with("TileZ", Tag::Int(6)),
        [6.5, 200.5, 6.03125],
    )
    .await;
    let frame = mobs(&region, EntityType::ItemFrame).await[0].id;
    region
        .request(RegionRequest::InteractEntity {
            player: alex.clone(),
            target: frame,
            interaction: interaction(&alex, Some(Item::Compass)),
        })
        .await;
    world.ticks(2).await;
    let logged = rows(&world, vec![Kind::ContainerAdd, Kind::ContainerRemove]).await;
    assert_eq!(logged.len(), 1, "{logged:?}");
    assert_eq!(logged[0].kind, Some(Kind::ContainerAdd));
    assert_eq!(logged[0].user, "player100");
    assert_eq!(logged[0].material.as_deref(), Some("minecraft:compass"));
    assert!(
        logged[0]
            .text
            .as_deref()
            .is_some_and(|text| text.contains("entity=minecraft:item_frame:")),
        "{logged:?}"
    );
    let rolled = console(&world, "tuff audit rollback t:1h r:#global a:container").await;
    assert!(rolled[0].contains("1 items"), "{rolled:?}");
    assert!(!frame_data(&region, frame).await.contains_key("Item"));
    let restored = console(&world, "tuff audit restore t:1h r:#global a:container").await;
    assert!(restored[0].contains("1 items"), "{restored:?}");
    assert!(frame_data(&region, frame).await.contains_key("Item"));
    world.shutdown().await;
}

async fn frame_data(region: &RegionHandle, id: i32) -> Compound {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id, reply })
        .await;
    answer.await.unwrap().unwrap()
}

async fn pot_has_item(region: &RegionHandle, pos: BlockPos) -> bool {
    let (reply, answer) = oneshot::channel();
    let op = BlockOp::Copy {
        positions: vec![pos],
    };
    region.request(RegionRequest::Blocks { op, reply }).await;
    match answer.await.unwrap() {
        crate::world::messages::BlockOpResult::Copied(mut found) => found
            .remove(0)
            .and_then(|(_, nbt)| nbt)
            .is_some_and(|nbt| nbt.contains_key("item")),
        other => panic!("{other:?}"),
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_decorated_pots_item_rolls_back_and_restores() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (alex, mut events) = PlayerHandle::new(100);
    world.publish_players("minecraft:overworld", &[(&alex, [6.5, 201.0, 7.5])]);
    subscribe(&region, &alex, &mut events, &[chunk]).await;
    let pot = BlockPos::new(6, 200, 5);
    paste(&region, pot, "minecraft:decorated_pot", None).await;
    let mut click = place(&alex, pot, Item::Brick, 1);
    click.item = None;
    click.held = Some(Item::Brick);
    region.request(RegionRequest::Place(click)).await;
    world.ticks(2).await;
    assert!(pot_has_item(&region, pot).await);
    let rolled = console(&world, "tuff audit rollback t:1h r:#global a:container").await;
    assert!(rolled[0].contains("1 items"), "{rolled:?}");
    assert!(!pot_has_item(&region, pot).await);
    let restored = console(&world, "tuff audit restore t:1h r:#global a:container").await;
    assert!(restored[0].contains("1 items"), "{restored:?}");
    assert!(pot_has_item(&region, pot).await);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rollback_while_the_chest_is_open_rejects_the_viewers_stale_clicks() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (alex, mut events) = PlayerHandle::new(100);
    world.publish_players("minecraft:overworld", &[(&alex, [2.5, 201.0, 3.5])]);
    subscribe(&region, &alex, &mut events, &[chunk]).await;
    let chest = BlockPos::new(2, 200, 3);
    let saved = chest_with("minecraft:chest", &[(0, Item::Diamond, 5)]);
    paste(&region, chest, "minecraft:chest[facing=north]", Some(saved)).await;
    region
        .request(RegionRequest::Place(place(&alex, chest, Item::Torch, 1)))
        .await;
    expect(&mut events, container_menu).await;
    let diamonds = |count| Some(ItemStack::mint(Item::Diamond, count, MintReason::Test));
    click(
        &region,
        &alex,
        &mut events,
        set_slot(chest, 0, diamonds(5), diamonds(2)),
    )
    .await;
    world.ticks(2).await;

    let rolled = console(&world, "tuff audit rollback t:1h r:#global a:container").await;
    assert!(rolled[0].contains("3 items"), "{rolled:?}");
    assert_eq!(
        contents(&region, chest).await,
        [(0, "minecraft:diamond".to_owned(), 5)]
    );

    region
        .request(RegionRequest::ContainerTransaction {
            player: alex.clone(),
            window_id: 1,
            changes: set_slot(chest, 0, diamonds(2), None),
            took_result: false,
        })
        .await;
    assert!(
        !expect(&mut events, transaction_answer).await.0,
        "a click made against the contents before the rollback is refused"
    );
    assert_eq!(
        contents(&region, chest).await,
        [(0, "minecraft:diamond".to_owned(), 5)]
    );
    click(
        &region,
        &alex,
        &mut events,
        set_slot(chest, 0, diamonds(5), diamonds(4)),
    )
    .await;
    assert_eq!(
        contents(&region, chest).await,
        [(0, "minecraft:diamond".to_owned(), 4)]
    );
    world.shutdown().await;
}
