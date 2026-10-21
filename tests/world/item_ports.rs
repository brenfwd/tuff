//! Item ports: hoppers, droppers and hopper minecarts reaching blocks that
//! are not modeled containers through the per-block port table.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::world::item_port::{ItemPort, PortAccess};
use crate::world::messages::BlockOp;
use crate::world::region_api::RegionWorld;
use tuff_nbt::Tag;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, Item};

const Y: i32 = 200;
const X: i32 = 88;
const Z: i32 = 88;

#[derive(Default)]
struct Probe {
    faces: Mutex<Vec<Option<tuff_vanilla_data::block::Facing>>>,
    takes: AtomicUsize,
}

struct TestPort(Arc<Probe>);

impl ItemPort for TestPort {
    fn offer(
        &self,
        _: &mut dyn RegionWorld,
        at: &PortAccess,
        stack: ItemStack,
    ) -> Result<(), ItemStack> {
        self.0.faces.lock().unwrap().push(at.face);
        if at.face != Some(tuff_vanilla_data::block::Facing::Up) {
            return Err(stack);
        }
        stack.burn(tuff_item::item::BurnReason::Test);
        Ok(())
    }

    fn peek(&self, _: &dyn RegionWorld, _: &PortAccess) -> Option<tuff_item::item::ItemSnapshot> {
        Some(tuff_item::item::ItemSnapshot::describe(
            Item::Stick,
            1,
            Default::default(),
        ))
    }

    fn take(&self, _: &mut dyn RegionWorld, _: &PortAccess) -> Option<ItemStack> {
        self.0.takes.fetch_add(1, Ordering::SeqCst);
        Some(ItemStack::mint(Item::Stick, 1, MintReason::Test))
    }
}

struct PortFeature(Arc<Probe>);

impl tuff_core::feature::Feature for PortFeature {
    fn id(&self) -> &'static str {
        "test-ports"
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            let id = cx.feature_id();
            cx.registries().item_ports.register(
                id,
                Block::Bookshelf,
                Arc::new(TestPort(self.0.clone())),
            )?;
        }
        Ok(())
    }
}

async fn start(
    features: Vec<Box<dyn tuff_core::feature::Feature>>,
) -> (TestDir, TestWorld, RegionHandle) {
    let dir = TestDir::new();
    let world = TestWorld::start_with_features(dir.path(), features);
    let (player, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &player, &mut events, center).await;
    let region = world.region(center.region()).await;
    std::mem::forget((player, events));
    (dir, world, region)
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

async fn hopper_slots(region: &RegionHandle, pos: BlockPos) -> usize {
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
async fn a_hopper_minecart_pulls_a_disc_from_a_jukebox_above() {
    let (_dir, world, region) = start(vec![]).await;
    let jukebox = BlockPos::new(X, Y, Z);
    let track = BlockPos::new(X, Y - 1, Z);
    set(&region, BlockPos::new(X, Y - 2, Z), "minecraft:stone", None).await;
    set(&region, track, "minecraft:rail", None).await;
    let record = Compound::new()
        .with("id", "minecraft:music_disc_13")
        .with("count", 1i32);
    set(
        &region,
        jukebox,
        "minecraft:jukebox[has_record=true]",
        Some(Compound::new().with("RecordItem", record)),
    )
    .await;
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
    let cart = mobs(&region, EntityType::HopperMinecart).await[0].id;
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::EntityData { id: cart, reply })
        .await;
    let data = answer.await.unwrap().expect("the cart exists");
    let held = data.get_list("Items").unwrap_or(&[]);
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(
        held[0].as_compound().and_then(|i| i.get_str("id")),
        Some("minecraft:music_disc_13")
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_side_hopper_does_not_reach_a_port_that_refuses_that_face() {
    let probe = Arc::new(Probe::default());
    let (_dir, world, region) = start(vec![Box::new(PortFeature(probe.clone()))]).await;
    set(&region, BlockPos::new(X, Y, Z), "minecraft:bookshelf", None).await;
    let side = BlockPos::new(X - 1, Y, Z);
    set(
        &region,
        side,
        "minecraft:hopper[facing=east]",
        Some(items("minecraft:stick", 1, 1)),
    )
    .await;
    world.ticks(20).await;
    assert_eq!(hopper_slots(&region, side).await, 1, "the stick stays");
    assert_eq!(
        probe.faces.lock().unwrap().first().copied(),
        Some(Some(tuff_vanilla_data::block::Facing::West))
    );

    let top = BlockPos::new(X, Y + 1, Z);
    set(
        &region,
        top,
        "minecraft:hopper[facing=down]",
        Some(items("minecraft:stick", 1, 1)),
    )
    .await;
    world.ticks(20).await;
    assert_eq!(hopper_slots(&region, top).await, 0, "the top face accepts");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_hopper_does_not_trigger_a_take() {
    let probe = Arc::new(Probe::default());
    let (_dir, world, region) = start(vec![Box::new(PortFeature(probe.clone()))]).await;
    set(&region, BlockPos::new(X, Y, Z), "minecraft:bookshelf", None).await;
    set(&region, BlockPos::new(X, Y - 2, Z), "minecraft:stone", None).await;
    let hopper = BlockPos::new(X, Y - 1, Z);
    set(
        &region,
        hopper,
        "minecraft:hopper[facing=down]",
        Some(items("minecraft:stone", 64, 5)),
    )
    .await;
    world.ticks(30).await;
    assert_eq!(probe.takes.load(Ordering::SeqCst), 0);
    set(
        &region,
        hopper,
        "minecraft:hopper[facing=down]",
        Some(items("minecraft:stone", 64, 4)),
    )
    .await;
    world.ticks(30).await;
    assert!(probe.takes.load(Ordering::SeqCst) >= 1);
    world.shutdown().await;
}
