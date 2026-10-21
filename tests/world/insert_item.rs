//! RegionWorld::insert_item over modeled containers and item ports, and
//! the Dispensers slot fired by redstone activation.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::world::item_port::{ItemPort, PortAccess, Porter};
use crate::world::messages::BlockOp;
use crate::world::region_api::{Dispensers, Insert, RegionTask, RegionWorld};
use tuff_nbt::Tag;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, Item};

const Y: i32 = 200;
const X: i32 = 88;
const Z: i32 = 88;

struct Port;

impl ItemPort for Port {
    fn offer(&self, _: &mut dyn RegionWorld, _: &PortAccess, _: &ItemStack) -> bool {
        true
    }

    fn peek(&self, _: &dyn RegionWorld, _: &PortAccess) -> Option<ItemStack> {
        None
    }

    fn take(&self, _: &mut dyn RegionWorld, _: &PortAccess) -> Option<ItemStack> {
        None
    }
}

struct Fires(Arc<AtomicUsize>, Arc<Mutex<Vec<BlockPos>>>);

impl Dispensers for Fires {
    fn fire(&self, _: &mut dyn RegionWorld, pos: BlockPos) {
        self.0.fetch_add(1, Ordering::SeqCst);
        self.1.lock().unwrap().push(pos);
    }
}

struct TestFeature(Arc<AtomicUsize>, Arc<Mutex<Vec<BlockPos>>>);

impl tuff_core::feature::Feature for TestFeature {
    fn id(&self) -> &'static str {
        "test-insert"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let id = cx.feature_id();
            cx.registries()
                .item_ports
                .register(id, Block::Bookshelf, Arc::new(Port))?;
            cx.registries()
                .dispensers
                .replace(id, Arc::new(Fires(self.0.clone(), self.1.clone())))?;
        }
        Ok(())
    }
}

async fn start() -> (
    TestDir,
    TestWorld,
    RegionHandle,
    Arc<AtomicUsize>,
    Arc<Mutex<Vec<BlockPos>>>,
) {
    let dir = TestDir::new();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let world = TestWorld::start_with_features(
        dir.path(),
        vec![Box::new(TestFeature(count.clone(), seen.clone()))],
    );
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    std::mem::forget((player, events));
    (dir, world, region, count, seen)
}

async fn set(region: &RegionHandle, pos: BlockPos, state: &str, data: Option<Compound>) {
    blocks(
        region,
        BlockOp::Set {
            changes: vec![(pos, state.parse().unwrap())],
            destroy: false,
            keep: false,
            strict: false,
            quiet: false,
            data,
        },
    )
    .await;
}

fn items(id: &str, count: i32, slots: i8) -> Compound {
    Compound::new().with(
        "Items",
        Tag::List(
            (0..slots)
                .map(|slot| {
                    Tag::Compound(
                        Compound::new()
                            .with("Slot", slot)
                            .with("id", id)
                            .with("count", count),
                    )
                })
                .collect(),
        ),
    )
}

async fn insert(
    region: &RegionHandle,
    target: BlockPos,
    face: Option<Facing>,
    stack: ItemStack,
) -> Insert {
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: target,
            max: target,
            run: RegionTask::new(move |world| {
                let _ = reply.send(world.insert_item(target, face, stack, Porter::Mob(0)));
            }),
        })
        .await;
    answer.await.unwrap()
}

async fn slots(region: &RegionHandle, pos: BlockPos) -> usize {
    let crate::world::messages::BlockOpResult::Copied(copied) = blocks(
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
        Some(Some((_, Some(nbt)))) => nbt.get_list("Items").map_or(0, <[Tag]>::len),
        _ => 0,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inserting_into_a_chest_stores_and_a_full_chest_refuses() {
    let (_dir, world, region, ..) = start().await;
    let chest = BlockPos::new(X, Y, Z);
    set(&region, chest, "minecraft:chest", None).await;
    let stone = ItemStack::mint(Item::Stone, 10, MintReason::Test);
    assert_eq!(
        insert(&region, chest, None, stone.dup_unchecked()).await,
        Insert::Stored
    );
    assert_eq!(slots(&region, chest).await, 1);
    set(
        &region,
        chest,
        "minecraft:chest",
        Some(items("minecraft:dirt", 64, 27)),
    )
    .await;
    assert_eq!(
        insert(&region, chest, None, stone.dup_unchecked()).await,
        Insert::Refused(stone)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_double_chest_spills_into_its_other_half() {
    let (_dir, world, region, ..) = start().await;
    let left = BlockPos::new(X, Y, Z);
    let right = BlockPos::new(X + 1, Y, Z);
    set(
        &region,
        left,
        "minecraft:chest[facing=south,type=right]",
        Some(items("minecraft:dirt", 64, 27)),
    )
    .await;
    set(
        &region,
        right,
        "minecraft:chest[facing=south,type=left]",
        None,
    )
    .await;
    assert_eq!(
        insert(
            &region,
            left,
            None,
            ItemStack::mint(Item::Stone, 5, MintReason::Test)
        )
        .await,
        Insert::Stored
    );
    assert_eq!(slots(&region, right).await, 1);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_furnace_refuses_ingredients_from_below_but_takes_them_from_above() {
    let (_dir, world, region, ..) = start().await;
    let furnace = BlockPos::new(X, Y, Z);
    set(&region, furnace, "minecraft:furnace", None).await;
    let ore = ItemStack::mint(Item::IronOre, 1, MintReason::Test);
    assert!(matches!(
        insert(&region, furnace, Some(Facing::Down), ore.dup_unchecked()).await,
        Insert::Refused(_)
    ));
    assert_eq!(
        insert(&region, furnace, Some(Facing::Up), ore).await,
        Insert::Stored
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_port_block_takes_one_item_and_no_target_returns_the_stack() {
    let (_dir, world, region, ..) = start().await;
    let shelf = BlockPos::new(X, Y, Z);
    set(&region, shelf, "minecraft:bookshelf", None).await;
    assert_eq!(
        insert(
            &region,
            shelf,
            Some(Facing::Up),
            ItemStack::mint(Item::Stick, 1, MintReason::Test)
        )
        .await,
        Insert::Stored
    );
    let two = ItemStack::mint(Item::Stick, 2, MintReason::Test);
    assert_eq!(
        insert(&region, shelf, Some(Facing::Up), two).await,
        Insert::Refused(ItemStack::mint(Item::Stick, 1, MintReason::Test))
    );
    let air = BlockPos::new(X + 5, Y, Z);
    let stick = ItemStack::mint(Item::Stick, 1, MintReason::Test);
    assert_eq!(
        insert(&region, air, None, stick.dup_unchecked()).await,
        Insert::Nothing(stick)
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_dispensers_slot_fires_on_each_activation() {
    let (_dir, world, region, count, seen) = start().await;
    let dispenser = BlockPos::new(X, Y, Z);
    set(&region, BlockPos::new(X, Y - 1, Z), "minecraft:stone", None).await;
    set(
        &region,
        dispenser,
        "minecraft:dispenser[facing=east]",
        Some(items("minecraft:stick", 1, 1)),
    )
    .await;
    let power = BlockPos::new(X - 1, Y, Z);
    set(&region, power, "minecraft:redstone_block", None).await;
    world.ticks(6).await;
    assert_eq!(count.load(Ordering::SeqCst), 1);
    set(&region, power, "minecraft:air", None).await;
    world.ticks(6).await;
    set(&region, power, "minecraft:redstone_block", None).await;
    world.ticks(6).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(seen.lock().unwrap()[0], dispenser);
    world.shutdown().await;
}
