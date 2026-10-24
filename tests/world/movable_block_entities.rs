use super::*;
use crate::world::messages::{BlockOp, BlockOpResult, BlockUpdate, RegionInternal, Track};
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::{BlockEntityType, DataComponentType, EntityType};

const Y: i32 = 200;

struct Bench {
    world: TestWorld,
    region: RegionHandle,
    _player: PlayerHandle,
    events: mpsc::UnboundedReceiver<PlayerEvent>,
}

async fn bench(dir: &Path, feature: bool, centers: &[ChunkPos]) -> Bench {
    let (player, mut events) = PlayerHandle::new(100);
    let world = if feature {
        TestWorld::start_with_features(
            dir,
            vec![Box::new(
                tuff_feature_builtin_moving_block_entities::MovingBlockEntities,
            )],
        )
    } else {
        TestWorld::start(dir)
    };
    for center in centers {
        view_around(&world, &player, &mut events, *center).await;
    }
    let region = world.region(centers[0].region()).await;
    Bench {
        world,
        region,
        _player: player,
        events,
    }
}

async fn blocks(region: &RegionHandle, op: BlockOp) -> BlockOpResult {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Blocks { op, reply }).await;
    answer.await.unwrap()
}

async fn paste(region: &RegionHandle, pos: BlockPos, state: &str, nbt: Option<Compound>) {
    let state: BlockState = state.parse().unwrap();
    blocks(
        region,
        BlockOp::Paste {
            changes: vec![(pos, state, nbt)],
            strict: false,
        },
    )
    .await;
}

async fn copy(region: &RegionHandle, pos: BlockPos) -> (BlockState, Option<Compound>) {
    match blocks(
        region,
        BlockOp::Copy {
            positions: vec![pos],
        },
    )
    .await
    {
        BlockOpResult::Copied(mut found) => found.remove(0).unwrap(),
        other => panic!("{other:?}"),
    }
}

async fn item_entities(region: &RegionHandle) -> usize {
    let (reply, answer) = oneshot::channel();
    region.request(RegionRequest::Entities { reply }).await;
    answer
        .await
        .unwrap()
        .iter()
        .filter(|e| e.kind == tuff_vanilla_data::registry::EntityType::Item)
        .count()
}

fn holding(id: &str, item: &str, count: i32) -> Compound {
    Compound::new().with("id", id).with(
        "Items",
        Tag::List(vec![Tag::Compound(
            Compound::new()
                .with("Slot", Tag::Byte(0))
                .with("id", item)
                .with("count", Tag::Int(count)),
        )]),
    )
}

fn first_item(nbt: &Compound) -> Option<(String, i32)> {
    let Some(Tag::List(slots)) = nbt.get("Items") else {
        return None;
    };
    let slot = slots.first()?.as_compound()?;
    Some((slot.get_str("id")?.to_owned(), slot.get_int("count")?))
}

fn at(x: i32, z: i32) -> BlockPos {
    BlockPos::new(x, Y, z)
}

async fn power(b: &Bench, pos: BlockPos, on: bool) {
    let block = if on {
        "minecraft:redstone_block"
    } else {
        "minecraft:air"
    };
    paste(&b.region, pos, block, None).await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_the_feature_a_chest_stops_a_piston() {
    let dir = TestDir::new();
    let b = bench(dir.path(), false, &[ChunkPos::new(5, 5)]).await;
    paste(&b.region, at(84, 86), "minecraft:piston[facing=east]", None).await;
    paste(
        &b.region,
        at(85, 86),
        "minecraft:chest[facing=north]",
        Some(holding("minecraft:chest", "minecraft:diamond", 5)),
    )
    .await;
    power(&b, at(84, 87), true).await;
    b.world.ticks(4).await;
    assert_eq!(copy(&b.region, at(84, 86)).await.0.extended(), Some(false));
    assert_eq!(copy(&b.region, at(85, 86)).await.0.block(), Block::Chest);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pushed_chest_keeps_its_items_and_lock() {
    let dir = TestDir::new();
    let b = bench(dir.path(), true, &[ChunkPos::new(5, 5)]).await;
    paste(&b.region, at(84, 86), "minecraft:piston[facing=east]", None).await;
    let chest = holding("minecraft:chest", "minecraft:diamond", 5)
        .with("LootTable", "minecraft:chests/simple_dungeon");
    paste(
        &b.region,
        at(85, 86),
        "minecraft:chest[facing=north]",
        Some(chest),
    )
    .await;
    paste(&b.region, at(86, 86), "minecraft:stone", None).await;
    power(&b, at(84, 87), true).await;
    b.world.ticks(1).await;
    let (moving, data) = copy(&b.region, at(86, 86)).await;
    assert_eq!(moving.block(), Block::MovingPiston);
    let carried = data
        .and_then(|nbt| nbt.get_compound("tuff:carried").cloned())
        .expect("the moving piston carries the chest");
    assert_eq!(first_item(&carried), Some(("minecraft:diamond".into(), 5)));
    b.world.ticks(3).await;
    assert_eq!(copy(&b.region, at(87, 86)).await.0.block(), Block::Stone);
    let (state, data) = copy(&b.region, at(86, 86)).await;
    assert_eq!(state.block(), Block::Chest);
    let data = data.expect("the chest landed with its block entity");
    assert_eq!(data.get_int("x"), Some(86));
    assert_eq!(first_item(&data), Some(("minecraft:diamond".into(), 5)));
    assert_eq!(
        data.get_str("LootTable"),
        Some("minecraft:chests/simple_dungeon")
    );
    assert_eq!(item_entities(&b.region).await, 0, "nothing spilled");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_sticky_piston_pulls_a_furnace_back_with_its_fuel() {
    let dir = TestDir::new();
    let b = bench(dir.path(), true, &[ChunkPos::new(5, 5)]).await;
    paste(
        &b.region,
        at(84, 86),
        "minecraft:sticky_piston[facing=east]",
        None,
    )
    .await;
    paste(
        &b.region,
        at(85, 86),
        "minecraft:furnace[facing=north]",
        Some(holding("minecraft:furnace", "minecraft:coal", 7)),
    )
    .await;
    power(&b, at(84, 87), true).await;
    b.world.ticks(4).await;
    assert_eq!(copy(&b.region, at(86, 86)).await.0.block(), Block::Furnace);
    power(&b, at(84, 87), false).await;
    b.world.ticks(4).await;
    assert_eq!(copy(&b.region, at(86, 86)).await.0.block(), Block::Air);
    let (state, data) = copy(&b.region, at(85, 86)).await;
    assert_eq!(state.block(), Block::Furnace);
    let data = data.expect("the furnace kept its block entity");
    assert_eq!(first_item(&data), Some(("minecraft:coal".into(), 7)));
    assert_eq!(item_entities(&b.region).await, 0);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pushed_bed_still_breaks() {
    let dir = TestDir::new();
    let b = bench(dir.path(), true, &[ChunkPos::new(5, 5)]).await;
    paste(&b.region, at(84, 86), "minecraft:piston[facing=east]", None).await;
    paste(
        &b.region,
        at(85, 86),
        "minecraft:red_bed[facing=north,part=foot]",
        None,
    )
    .await;
    paste(
        &b.region,
        at(85, 85),
        "minecraft:red_bed[facing=north,part=head]",
        None,
    )
    .await;
    power(&b, at(84, 87), true).await;
    b.world.ticks(4).await;
    assert_eq!(
        copy(&b.region, at(85, 86)).await.0.block(),
        Block::PistonHead
    );
    assert_ne!(copy(&b.region, at(86, 86)).await.0.block(), Block::RedBed);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_double_chest_moves_whole_along_its_axis_and_splits_sideways() {
    let dir = TestDir::new();
    let b = bench(dir.path(), true, &[ChunkPos::new(5, 5)]).await;
    paste(&b.region, at(84, 86), "minecraft:piston[facing=east]", None).await;
    paste(
        &b.region,
        at(85, 86),
        "minecraft:chest[facing=north,type=left]",
        Some(holding("minecraft:chest", "minecraft:diamond", 1)),
    )
    .await;
    paste(
        &b.region,
        at(86, 86),
        "minecraft:chest[facing=north,type=right]",
        Some(holding("minecraft:chest", "minecraft:emerald", 2)),
    )
    .await;
    power(&b, at(84, 87), true).await;
    b.world.ticks(4).await;
    let (left, left_data) = copy(&b.region, at(86, 86)).await;
    let (right, right_data) = copy(&b.region, at(87, 86)).await;
    assert_eq!(left.get("type"), Some("left"));
    assert_eq!(right.get("type"), Some("right"));
    assert_eq!(
        first_item(&left_data.unwrap()),
        Some(("minecraft:diamond".into(), 1))
    );
    assert_eq!(
        first_item(&right_data.unwrap()),
        Some(("minecraft:emerald".into(), 2))
    );

    paste(
        &b.region,
        at(86, 87),
        "minecraft:piston[facing=north]",
        None,
    )
    .await;
    power(&b, at(86, 88), true).await;
    b.world.ticks(4).await;
    let (pushed, pushed_data) = copy(&b.region, at(86, 85)).await;
    let (stayed, _) = copy(&b.region, at(87, 86)).await;
    assert_eq!(pushed.block(), Block::Chest);
    assert_eq!(pushed.get("type"), Some("single"));
    assert_eq!(stayed.get("type"), Some("single"));
    assert_eq!(
        first_item(&pushed_data.unwrap()),
        Some(("minecraft:diamond".into(), 1))
    );
    assert_eq!(item_entities(&b.region).await, 0);
    b.world.shutdown().await;
}

async fn border_bench(dir: &Path) -> (Bench, RegionHandle) {
    let b = bench(dir, true, &[ChunkPos::new(31, 5), ChunkPos::new(32, 5)]).await;
    // Freshly loaded border chunks reach the neighbour's halo only when their
    // region ends a tick; until then redstone at the border defers a tick.
    b.world.ticks(1).await;
    let east = b.world.region(ChunkPos::new(32, 5).region()).await;
    (b, east)
}

async fn items_around(b: &Bench, east: &RegionHandle) -> usize {
    item_entities(&b.region).await + item_entities(east).await
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chest_crosses_a_region_border_and_comes_back() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    let power = at(510, 87);
    paste(
        &b.region,
        at(510, 86),
        "minecraft:sticky_piston[facing=east]",
        None,
    )
    .await;
    paste(
        &b.region,
        at(511, 86),
        "minecraft:chest[facing=north]",
        Some(holding("minecraft:chest", "minecraft:diamond", 5)),
    )
    .await;
    paste(&b.region, power, "minecraft:redstone_block", None).await;
    b.world.ticks(5).await;
    assert_eq!(copy(&b.region, at(510, 86)).await.0.extended(), Some(true));
    let (state, data) = copy(&east, at(512, 86)).await;
    assert_eq!(state.block(), Block::Chest, "the chest moved east");
    let data = data.expect("the chest landed with its block entity");
    assert_eq!(data.get_int("x"), Some(512));
    assert_eq!(first_item(&data), Some(("minecraft:diamond".into(), 5)));

    paste(&b.region, power, "minecraft:air", None).await;
    b.world.ticks(5).await;
    assert_eq!(copy(&east, at(512, 86)).await.0.block(), Block::Air);
    let (state, data) = copy(&b.region, at(511, 86)).await;
    assert_eq!(state.block(), Block::Chest, "the chest was pulled back");
    let data = data.expect("the pulled chest kept its block entity");
    assert_eq!(data.get_int("x"), Some(511));
    assert_eq!(first_item(&data), Some(("minecraft:diamond".into(), 5)));
    assert_eq!(items_around(&b, &east).await, 0, "nothing spilled");
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_piston_moves_block_entities_into_and_inside_the_neighbour() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    paste(
        &b.region,
        at(510, 86),
        "minecraft:piston[facing=east]",
        None,
    )
    .await;
    paste(
        &b.region,
        at(511, 86),
        "minecraft:furnace[facing=north]",
        Some(holding("minecraft:furnace", "minecraft:coal", 7)),
    )
    .await;
    paste(
        &east,
        at(512, 86),
        "minecraft:barrel[facing=up]",
        Some(holding("minecraft:barrel", "minecraft:emerald", 3)),
    )
    .await;
    paste(&b.region, at(510, 87), "minecraft:redstone_block", None).await;
    b.world.ticks(5).await;
    assert_eq!(
        copy(&b.region, at(511, 86)).await.0.block(),
        Block::PistonHead
    );
    let (furnace, furnace_data) = copy(&east, at(512, 86)).await;
    let (barrel, barrel_data) = copy(&east, at(513, 86)).await;
    assert_eq!(furnace.block(), Block::Furnace);
    assert_eq!(barrel.block(), Block::Barrel);
    assert_eq!(
        first_item(&furnace_data.expect("furnace data")),
        Some(("minecraft:coal".into(), 7))
    );
    assert_eq!(
        first_item(&barrel_data.expect("barrel data")),
        Some(("minecraft:emerald".into(), 3))
    );
    assert_eq!(items_around(&b, &east).await, 0, "nothing spilled");
    b.world.shutdown().await;
}

/// The chest a piston west of the border starts moving into `(512, 86)`,
/// as the neighbour would send it.
fn chest_arrives(tick: u64) -> BlockUpdate {
    let state = Block::MovingPiston.default_state();
    let state = state
        .with_facing(tuff_vanilla_data::block::Facing::East)
        .unwrap_or(state);
    let chest: BlockState = "minecraft:chest[facing=north]".parse().unwrap();
    BlockUpdate::Piston {
        pos: at(512, 86),
        state,
        flags: crate::world::redstone::UPDATE_MOVED_BY_PISTON
            | crate::world::redstone::UPDATE_INVISIBLE,
        drops: false,
        moving: Some(crate::world::redstone::MovingBlock::new(
            chest,
            tuff_vanilla_data::block::Facing::East,
            true,
            false,
        )),
        lifted: Some(crate::world::block_ledger::LiftedBlock::test(chest)),
        tick,
        expected: Block::Air.default_state(),
        carry: None,
    }
}

fn chest_entity(nbt: Option<Compound>) -> BlockUpdate {
    BlockUpdate::CarriedEntity {
        pos: at(512, 86),
        nbt,
    }
}

fn diamonds() -> Option<Compound> {
    Some(
        holding("minecraft:chest", "minecraft:diamond", 5)
            .with("x", 511)
            .with("y", Y)
            .with("z", 86),
    )
}

fn send(region: &RegionHandle, update: BlockUpdate) {
    assert!(region.internal(RegionInternal::BlockUpdates {
        tick: None,
        updates: vec![update]
    }));
}

fn now(b: &Bench) -> u64 {
    *b.world.clock.borrow()
}

async fn chest_items(region: &RegionHandle) -> Option<(String, i32)> {
    let (state, data) = copy(region, at(512, 86)).await;
    assert_eq!(state.block(), Block::Chest);
    first_item(&data.expect("the chest has a block entity"))
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_block_entity_arriving_before_its_move_is_kept_for_it() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    send(&east, chest_entity(diamonds()));
    b.world.ticks(2).await;
    assert_eq!(item_entities(&east).await, 0, "kept, not spilled");
    send(&east, chest_arrives(now(&b)));
    b.world.ticks(4).await;
    assert_eq!(
        chest_items(&east).await,
        Some(("minecraft:diamond".into(), 5))
    );
    assert_eq!(item_entities(&east).await, 0);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_moved_block_waits_to_land_until_its_block_entity_arrives() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    send(&east, chest_arrives(now(&b)));
    b.world.ticks(8).await;
    assert_eq!(
        copy(&east, at(512, 86)).await.0.block(),
        Block::MovingPiston,
        "held while its block entity is on its way"
    );
    send(&east, chest_entity(diamonds()));
    b.world.ticks(2).await;
    let (state, data) = copy(&east, at(512, 86)).await;
    assert_eq!(state.block(), Block::Chest);
    let data = data.unwrap();
    assert_eq!(data.get_int("x"), Some(512));
    assert_eq!(first_item(&data), Some(("minecraft:diamond".into(), 5)));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_answer_lands_the_block_at_once() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    send(&east, chest_arrives(now(&b)));
    send(&east, chest_entity(None));
    b.world.ticks(4).await;
    assert_eq!(chest_items(&east).await, None);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_block_entity_arriving_after_a_bare_landing_moves_in() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    send(&east, chest_arrives(now(&b)));
    b.world.ticks(26).await;
    assert_eq!(
        chest_items(&east).await,
        None,
        "it gave up waiting and landed bare"
    );
    send(&east, chest_entity(diamonds()));
    b.world.ticks(1).await;
    assert_eq!(
        chest_items(&east).await,
        Some(("minecraft:diamond".into(), 5))
    );
    assert_eq!(item_entities(&east).await, 0);
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_block_entity_no_move_claims_spills_its_contents() {
    let dir = TestDir::new();
    let (b, east) = border_bench(dir.path()).await;
    send(&east, chest_entity(diamonds()));
    b.world.ticks(26).await;
    assert_eq!(copy(&east, at(512, 86)).await.0.block(), Block::Air);
    assert!(
        item_entities(&east).await > 0,
        "the diamonds dropped instead of vanishing"
    );
    b.world.shutdown().await;
}

#[derive(Debug)]
enum Seen {
    Event(BlockPos),
    Block(BlockPos, u32),
    Data(BlockPos, i32, Compound),
    Display(Box<Display>),
    Gone(i32),
}

#[derive(Debug)]
struct Display {
    id: i32,
    kind: i32,
    start: [f64; 3],
    end: [f64; 3],
    data: Vec<(u8, Value)>,
}

#[derive(Debug, PartialEq)]
enum Value {
    Int(i32),
    Floats(Vec<f32>),
    /// Count, item id and each added component's id with its patterns
    /// (pattern network id, colour id) when it is banner patterns.
    Item(i32, i32, Vec<(i32, Vec<(i32, i32)>)>),
}

impl Display {
    fn value(&self, index: u8) -> Option<&Value> {
        self.data.iter().find(|(i, _)| *i == index).map(|(_, v)| v)
    }
}

/// Reads the spawn and movement packets a display entity's report holds.
fn display_of(track: &Track) -> Display {
    use tuff_vanilla_data::entity::data_serializer as ty;
    let mut add = framed_payload(&track.spawn[0]);
    add.read_var_int().unwrap();
    let id = add.read_var_int().unwrap();
    add.read_uuid().unwrap();
    let kind = add.read_var_int().unwrap();
    let start = [0; 3].map(|_| add.read_double().unwrap());
    let mut meta = framed_payload(&track.spawn[1]);
    meta.read_var_int().unwrap();
    assert_eq!(meta.read_var_int().unwrap(), id);
    let mut data = Vec::new();
    loop {
        let index = meta.read_unsigned_byte().unwrap();
        if index == 0xFF {
            break;
        }
        let value = match meta.read_var_int().unwrap() as u8 {
            ty::INT | ty::BLOCK_STATE => Value::Int(meta.read_var_int().unwrap()),
            ty::VECTOR3 => Value::Floats((0..3).map(|_| meta.read_float().unwrap()).collect()),
            ty::QUATERNION => Value::Floats((0..4).map(|_| meta.read_float().unwrap()).collect()),
            ty::ITEM_STACK => {
                let count = meta.read_var_int().unwrap();
                let item = meta.read_var_int().unwrap();
                let added = meta.read_var_int().unwrap();
                assert_eq!(meta.read_var_int().unwrap(), 0, "nothing removed");
                let components = (0..added)
                    .map(|_| {
                        let kind = meta.read_var_int().unwrap();
                        assert_eq!(kind, DataComponentType::BannerPatterns.id() as i32);
                        let layers = (0..meta.read_var_int().unwrap())
                            .map(|_| (meta.read_var_int().unwrap(), meta.read_var_int().unwrap()))
                            .collect();
                        (kind, layers)
                    })
                    .collect();
                Value::Item(count, item, components)
            }
            other => panic!("unexpected display data type {other}"),
        };
        data.push((index, value));
    }
    let mut sync = framed_payload(&track.update[0]);
    sync.read_var_int().unwrap();
    assert_eq!(sync.read_var_int().unwrap(), id);
    sync.read_var_int().unwrap();
    let end = [0; 3].map(|_| sync.read_double().unwrap());
    Display {
        id,
        kind,
        start,
        end,
        data,
    }
}

/// The block events, block changes and block entity data the viewer got
/// since the last call, in order.
fn seen(b: &mut Bench) -> Vec<Seen> {
    seen_in(&mut b.events)
}

fn seen_in(events: &mut mpsc::UnboundedReceiver<PlayerEvent>) -> Vec<Seen> {
    use tuff_vanilla_data::packet::play::Clientbound;
    let mut out = Vec::new();
    while let Ok(event) = events.try_recv() {
        let packets = match event {
            PlayerEvent::ChunkPackets { packets, .. } => packets,
            PlayerEvent::Track(track) => {
                out.push(Seen::Display(Box::new(display_of(&track))));
                continue;
            }
            PlayerEvent::Untrack { entity, .. } => {
                out.push(Seen::Gone(entity));
                continue;
            }
            _ => continue,
        };
        for frame in packets.iter() {
            let mut r = framed_payload(frame);
            let id = r.read_var_int().unwrap();
            if id == Clientbound::BlockEvent.id() {
                out.push(Seen::Event(r.read_position().unwrap()));
            } else if id == Clientbound::BlockUpdate.id() {
                let pos = r.read_position().unwrap();
                out.push(Seen::Block(pos, r.read_var_int().unwrap() as u32));
            } else if id == Clientbound::SectionBlocksUpdate.id() {
                let section = r.read_long().unwrap();
                let (x, y, z) = (section >> 42, section << 44 >> 44, section << 22 >> 42);
                for _ in 0..r.read_var_int().unwrap() {
                    let entry = r.read_var_long().unwrap();
                    let index = (entry & 0xFFF) as i32;
                    let pos = BlockPos::new(
                        x as i32 * 16 + (index >> 8),
                        y as i32 * 16 + (index & 15),
                        z as i32 * 16 + ((index >> 4) & 15),
                    );
                    out.push(Seen::Block(pos, (entry >> 12) as u32));
                }
            } else if id == Clientbound::BlockEntityData.id() {
                let pos = r.read_position().unwrap();
                let kind = r.read_var_int().unwrap();
                let nbt = r
                    .read_nbt()
                    .unwrap()
                    .and_then(|tag| tag.as_compound().cloned())
                    .unwrap_or_default();
                out.push(Seen::Data(pos, kind, nbt));
            }
        }
    }
    out
}

fn state_id(state: &str) -> u32 {
    state.parse::<BlockState>().unwrap().id()
}

fn block_of(id: u32) -> Option<Block> {
    BlockState::from_id(id).map(BlockState::block)
}

fn told_at(seen: &[Seen], at: BlockPos) -> usize {
    seen.iter()
        .position(|s| matches!(s, Seen::Event(pos) if *pos == at))
        .unwrap_or_else(|| panic!("no Block Action animates the move: {seen:?}"))
}

fn shown_at(seen: &[Seen], at: BlockPos, state: u32) -> Option<usize> {
    seen.iter()
        .position(|s| matches!(s, Seen::Block(pos, id) if *pos == at && *id == state))
}

fn data_at(seen: &[Seen], at: BlockPos, kind: BlockEntityType) -> Option<(usize, &Compound)> {
    seen.iter().enumerate().find_map(|(i, s)| match s {
        Seen::Data(pos, id, nbt) if *pos == at && *id == kind.id() as i32 => Some((i, nbt)),
        _ => None,
    })
}

fn moved_state(nbt: &Compound) -> BlockState {
    crate::storage::chunk_nbt::decode_block_state(nbt.get("blockState").unwrap())
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewers_see_a_pushed_lit_furnace_move() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), true, &[ChunkPos::new(5, 5)]).await;
    let lit = "minecraft:furnace[facing=north,lit=true]";
    paste(&b.region, at(84, 86), "minecraft:piston[facing=east]", None).await;
    paste(
        &b.region,
        at(85, 86),
        lit,
        Some(holding("minecraft:furnace", "minecraft:coal", 7)),
    )
    .await;
    seen(&mut b);
    power(&b, at(84, 87), true).await;
    b.world.ticks(1).await;
    let told = seen(&mut b);
    let event = told_at(&told, at(84, 86));
    let stand_in = shown_at(&told, at(85, 86), state_id("minecraft:stone"))
        .expect("the furnace first shows as a block the client moves");
    assert!(stand_in < event, "{told:?}");
    let (data_index, data) = data_at(&told, at(86, 86), BlockEntityType::Piston)
        .expect("the moving piston is told what it moves");
    assert!(data_index > event, "{told:?}");
    assert_eq!(moved_state(data), lit.parse().unwrap());
    assert_eq!(data.get_int("facing"), Some(5));
    assert_eq!(data.get_bool("extending"), Some(true));
    assert_eq!(data.get_bool("source"), Some(false));
    assert_eq!(data.get_float("progress"), Some(0.0));
    assert!(!data.contains_key("tuff:carried"), "{data:?}");
    for (i, s) in told.iter().enumerate() {
        if let Seen::Block(pos, state) = s
            && *pos == at(86, 86)
        {
            assert!(
                i > event,
                "the move is told before its blocks change: {told:?}"
            );
            assert_eq!(block_of(*state), Some(Block::MovingPiston));
        }
    }

    b.world.ticks(3).await;
    let landed = seen(&mut b);
    assert!(
        shown_at(&landed, at(86, 86), state_id(lit)).is_some(),
        "{landed:?}"
    );
    let (state, data) = copy(&b.region, at(86, 86)).await;
    assert_eq!(state, lit.parse().unwrap());
    let data = data.expect("the furnace landed with its block entity");
    assert_eq!(first_item(&data), Some(("minecraft:coal".into(), 7)));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewers_see_a_furnace_pulled_back() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), true, &[ChunkPos::new(5, 5)]).await;
    paste(
        &b.region,
        at(84, 86),
        "minecraft:sticky_piston[facing=east]",
        None,
    )
    .await;
    paste(
        &b.region,
        at(85, 86),
        "minecraft:furnace[facing=north]",
        None,
    )
    .await;
    power(&b, at(84, 87), true).await;
    b.world.ticks(4).await;
    seen(&mut b);
    power(&b, at(84, 87), false).await;
    b.world.ticks(1).await;
    let told = seen(&mut b);
    let event = told_at(&told, at(84, 86));
    let stand_in = shown_at(&told, at(86, 86), state_id("minecraft:stone"))
        .expect("the furnace first shows as a block the client pulls");
    assert!(stand_in < event, "{told:?}");
    let (_, data) = data_at(&told, at(85, 86), BlockEntityType::Piston)
        .expect("the moving piston is told what it moves");
    assert_eq!(moved_state(data).block(), Block::Furnace);
    assert_eq!(data.get_bool("extending"), Some(false));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pushed_campfire_shows_its_food_where_it_lands() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), true, &[ChunkPos::new(5, 5)]).await;
    paste(&b.region, at(84, 86), "minecraft:piston[facing=east]", None).await;
    paste(
        &b.region,
        at(85, 86),
        "minecraft:campfire[facing=north,lit=false]",
        Some(holding("minecraft:campfire", "minecraft:porkchop", 1)),
    )
    .await;
    seen(&mut b);
    power(&b, at(84, 87), true).await;
    b.world.ticks(4).await;
    let told = seen(&mut b);
    let (_, food) = data_at(&told, at(86, 86), BlockEntityType::Campfire)
        .expect("the landed campfire's data reaches viewers");
    assert_eq!(first_item(food), Some(("minecraft:porkchop".into(), 1)));
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewers_see_pushed_stone_move_without_a_stand_in() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), false, &[ChunkPos::new(5, 5)]).await;
    paste(&b.region, at(84, 86), "minecraft:piston[facing=east]", None).await;
    paste(&b.region, at(85, 86), "minecraft:stone", None).await;
    seen(&mut b);
    power(&b, at(84, 87), true).await;
    b.world.ticks(1).await;
    let told = seen(&mut b);
    let event = told_at(&told, at(84, 86));
    for (i, s) in told.iter().enumerate() {
        match s {
            Seen::Block(pos, _) if pos.z == 86 && (84..=86).contains(&pos.x) => {
                assert!(
                    i > event,
                    "blocks change only after the move is told: {told:?}"
                );
            }
            Seen::Data(..) => panic!("the client moves stone by itself: {told:?}"),
            _ => {}
        }
    }
    assert!(
        told.iter().any(|s| matches!(s, Seen::Block(pos, state)
            if *pos == at(86, 86) && block_of(*state) == Some(Block::MovingPiston))),
        "{told:?}"
    );
    b.world.ticks(3).await;
    let landed = seen(&mut b);
    assert!(
        shown_at(&landed, at(86, 86), state_id("minecraft:stone")).is_some(),
        "{landed:?}"
    );
    b.world.shutdown().await;
}

fn displayed(seen: &[Seen]) -> Option<(usize, &Display)> {
    seen.iter().enumerate().find_map(|(i, s)| match s {
        Seen::Display(display) => Some((i, &**display)),
        _ => None,
    })
}

/// Ticks one at a time until the display `id` is removed: the tick count
/// at which the block landed at `dest` as `block`, and at which it went.
async fn land_and_remove(b: &mut Bench, id: i32, dest: BlockPos, block: Block) -> (u32, u32) {
    let (mut landed, mut gone) = (None, None);
    for tick in 1..=8 {
        b.world.ticks(1).await;
        for s in seen(b) {
            match s {
                Seen::Block(pos, state) if pos == dest && block_of(state) == Some(block) => {
                    landed.get_or_insert(tick);
                }
                Seen::Gone(entity) if entity == id => {
                    gone.get_or_insert(tick);
                }
                Seen::Display(display) if display.id == id => {
                    panic!("the display is reported again: {display:?}")
                }
                _ => {}
            }
        }
    }
    (
        landed.expect("the block lands"),
        gone.expect("the display is removed"),
    )
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn viewers_see_a_pushed_chest_slide_as_a_block_display() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), true, &[ChunkPos::new(5, 5)]).await;
    paste(&b.region, at(84, 86), "minecraft:piston[facing=east]", None).await;
    paste(
        &b.region,
        at(85, 86),
        "minecraft:chest[facing=north]",
        Some(holding("minecraft:chest", "minecraft:diamond", 5)),
    )
    .await;
    let chest = copy(&b.region, at(85, 86)).await.0;
    seen(&mut b);
    power(&b, at(84, 87), true).await;
    b.world.ticks(1).await;
    let told = seen(&mut b);
    let event = told_at(&told, at(84, 86));
    let stand_in = shown_at(&told, at(85, 86), state_id("minecraft:stone"))
        .expect("the chest still shows as a block the client moves");
    assert!(stand_in < event, "{told:?}");
    let (index, display) = displayed(&told).expect("a display stands in for the chest");
    assert!(index > event, "{told:?}");
    assert_eq!(display.kind, EntityType::BlockDisplay.id() as i32);
    let y = f64::from(Y);
    assert_eq!(
        display.start,
        [85.0, y, 86.0],
        "it starts where the chest stood"
    );
    assert_eq!(display.end, [86.0, y, 86.0], "it slides to where it lands");
    assert_eq!(display.value(23), Some(&Value::Int(chest.id() as i32)));
    assert_eq!(
        display.value(10),
        Some(&Value::Int(2)),
        "it slides over the two ticks the client moves the block"
    );

    let id = display.id;
    let (landed, gone) = land_and_remove(&mut b, id, at(86, 86), Block::Chest).await;
    assert_eq!(
        gone,
        landed + 1,
        "the display goes the tick after the chest lands"
    );
    let (_, data) = copy(&b.region, at(86, 86)).await;
    assert_eq!(
        first_item(&data.expect("the chest landed with its block entity")),
        Some(("minecraft:diamond".into(), 5))
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_furnace_keeps_its_model_and_a_mid_move_viewer_sees_a_display() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), true, &[ChunkPos::new(5, 5)]).await;
    let lit = "minecraft:furnace[facing=north,lit=true]";
    paste(&b.region, at(84, 86), "minecraft:piston[facing=east]", None).await;
    paste(
        &b.region,
        at(85, 86),
        lit,
        Some(holding("minecraft:furnace", "minecraft:coal", 7)),
    )
    .await;
    seen(&mut b);
    power(&b, at(84, 87), true).await;
    b.world.ticks(1).await;
    let told = seen(&mut b);
    told_at(&told, at(84, 86));
    assert!(
        displayed(&told).is_none(),
        "the moving piston draws a furnace itself: {told:?}"
    );

    let (late, mut late_events) = PlayerHandle::new(101);
    subscribe(
        &b.region,
        &late,
        &mut late_events,
        &[ChunkPos::of_block(at(84, 86))],
    )
    .await;
    let first = expect(&mut late_events, |event| match event {
        PlayerEvent::Track(track) => Some(Seen::Display(Box::new(display_of(&track)))),
        _ => None,
    })
    .await;
    let mut joined = vec![first];
    joined.extend(seen_in(&mut late_events));
    let (_, display) =
        displayed(&joined).expect("a viewer arriving mid-move sees the furnace move");
    assert_eq!(display.kind, EntityType::BlockDisplay.id() as i32);
    let y = f64::from(Y);
    assert_eq!(
        display.start,
        [85.5, y, 86.0],
        "half way, as far as the move is"
    );
    assert_eq!(display.end, [86.0, y, 86.0]);
    assert_eq!(display.value(10), Some(&Value::Int(1)));
    assert_eq!(display.value(23), Some(&Value::Int(state_id(lit) as i32)));

    let id = display.id;
    let mut gone = false;
    for _ in 0..6 {
        b.world.ticks(1).await;
        gone |= seen_in(&mut late_events)
            .iter()
            .any(|s| matches!(s, Seen::Gone(entity) if *entity == id));
    }
    assert!(gone, "the late viewer's display is removed once it lands");
    assert_eq!(copy(&b.region, at(86, 86)).await.0, lit.parse().unwrap());
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_patterned_banner_slides_as_its_item() {
    let dir = TestDir::new();
    let mut b = bench(dir.path(), true, &[ChunkPos::new(5, 5)]).await;
    paste(&b.region, at(84, 86), "minecraft:piston[facing=east]", None).await;
    for x in 85..=86 {
        paste(
            &b.region,
            BlockPos::new(x, Y - 1, 86),
            "minecraft:stone",
            None,
        )
        .await;
    }
    let patterns = Compound::new().with("id", "minecraft:banner").with(
        "patterns",
        Tag::List(vec![Tag::Compound(
            Compound::new()
                .with("pattern", "minecraft:stripe_top")
                .with("color", "red"),
        )]),
    );
    paste(
        &b.region,
        at(85, 86),
        "minecraft:white_banner[rotation=4]",
        Some(patterns),
    )
    .await;
    seen(&mut b);
    power(&b, at(84, 87), true).await;
    b.world.ticks(1).await;
    let told = seen(&mut b);
    told_at(&told, at(84, 86));
    let (_, display) = displayed(&told).expect("a display stands in for the banner");
    assert_eq!(display.kind, EntityType::ItemDisplay.id() as i32);
    assert_eq!(display.start, [85.0, f64::from(Y), 86.0]);
    assert_eq!(display.value(10), Some(&Value::Int(2)));
    assert_eq!(display.value(11), Some(&Value::Floats(vec![0.5; 3])));
    let Some(Value::Floats(rotation)) = display.value(13) else {
        panic!("{display:?}");
    };
    let half = std::f32::consts::FRAC_1_SQRT_2;
    for (got, want) in rotation.iter().zip([0.0, -half, 0.0, -half]) {
        assert!((got - want).abs() < 1e-5, "{rotation:?}");
    }
    let stripe = tuff_item::item::decor::BannerLayer {
        pattern: "minecraft:stripe_top".into(),
        inline: None,
        color: tuff_vanilla_data::item::DyeColor::Red,
    };
    assert_eq!(
        display.value(23),
        Some(&Value::Item(
            1,
            tuff_vanilla_data::Item::WhiteBanner.id() as i32,
            vec![(
                DataComponentType::BannerPatterns.id() as i32,
                vec![(
                    stripe.network_id().unwrap() as i32 + 1,
                    stripe.color.id() as i32
                )]
            )]
        )),
        "the banner's item, with its patterns"
    );
    b.world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chest_pushed_across_a_region_border_slides_as_a_display() {
    let dir = TestDir::new();
    let (mut b, east) = border_bench(dir.path()).await;
    paste(
        &b.region,
        at(510, 86),
        "minecraft:piston[facing=east]",
        None,
    )
    .await;
    paste(
        &b.region,
        at(511, 86),
        "minecraft:chest[facing=north]",
        Some(holding("minecraft:chest", "minecraft:diamond", 5)),
    )
    .await;
    seen(&mut b);
    paste(&b.region, at(510, 87), "minecraft:redstone_block", None).await;
    b.world.ticks(1).await;
    let told = seen(&mut b);
    told_at(&told, at(510, 86));
    let (_, display) = displayed(&told).expect("the piston's region shows the chest moving");
    let y = f64::from(Y);
    assert_eq!(display.start, [511.0, y, 86.0]);
    assert_eq!(
        display.end,
        [512.0, y, 86.0],
        "it slides into the neighbour"
    );
    let id = display.id;
    let (landed, gone) = land_and_remove(&mut b, id, at(512, 86), Block::Chest).await;
    assert!(
        gone >= landed,
        "the display outlasts the move: {landed} {gone}"
    );
    let (state, data) = copy(&east, at(512, 86)).await;
    assert_eq!(state.block(), Block::Chest);
    assert_eq!(
        first_item(&data.expect("the chest landed with its block entity")),
        Some(("minecraft:diamond".into(), 5))
    );
    b.world.shutdown().await;
}
