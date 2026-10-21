//! The audit log's block rows for changes nobody placed or broke by hand:
//! flowing water and lava, falling blocks, pistons, decaying leaves and
//! explosions, each under its `#cause`, rolled back by cause; and what a
//! player's placement replaced.

use super::*;
use crate::world::game_rules::RuleValue;
use crate::world::messages::{BlockOp, BlockOpResult};
use tuff_feature_builtin_audit_log::filter::Filter;
use tuff_feature_builtin_audit_log::record::Kind;
use tuff_feature_builtin_audit_log::store::{Order, Row};
use tuff_feature_builtin_audit_log::{AuditLog, AuditLogFeature};
use tuff_vanilla_data::{Block, BlockState, Item};

const Y: i32 = 200;

fn start(dir: &Path) -> TestWorld {
    TestWorld::start_with_features(
        dir,
        vec![Box::new(AuditLogFeature::at(dir.join("audit.sqlite")))],
    )
}

async fn rows_by(world: &TestWorld, user: &str) -> Vec<Row> {
    let log = world
        .shared
        .services
        .get::<AuditLog>()
        .expect("the service runs");
    log.sync().await.unwrap();
    let filter = Filter {
        users: vec![user.to_owned()],
        ..Filter::default()
    };
    log.lookup(filter, Order::NewestFirst, 1000, 0)
        .await
        .unwrap()
}

async fn console(world: &TestWorld, line: &str) -> Vec<String> {
    crate::world::commands::console::run(world.shared.clone(), line)
        .await
        .iter()
        .map(|line| line.plain())
        .collect()
}

async fn set_blocks(region: &RegionHandle, changes: Vec<(BlockPos, BlockState)>) {
    let (reply, answer) = oneshot::channel();
    let op = BlockOp::Set {
        changes,
        destroy: false,
        keep: false,
        strict: false,
        quiet: false,
        data: None,
    };
    region.request(RegionRequest::Blocks { op, reply }).await;
    assert!(matches!(answer.await.unwrap(), BlockOpResult::Set(_)));
}

fn platform(center: BlockPos, radius: i32) -> Vec<(BlockPos, BlockState)> {
    let mut blocks = Vec::new();
    for dx in -radius..=radius {
        for dz in -radius..=radius {
            let pos = BlockPos::new(center.x + dx, center.y - 1, center.z + dz);
            blocks.push((pos, Block::Stone.default_state()));
        }
    }
    blocks
}

async fn block(region: &RegionHandle, pos: BlockPos) -> Block {
    block_at(region, pos).await.expect("loaded").block()
}

fn neighbours(pos: BlockPos) -> [BlockPos; 4] {
    [
        BlockPos::new(pos.x + 1, pos.y, pos.z),
        BlockPos::new(pos.x - 1, pos.y, pos.z),
        BlockPos::new(pos.x, pos.y, pos.z + 1),
        BlockPos::new(pos.x, pos.y, pos.z - 1),
    ]
}

async fn rollback(world: &TestWorld, user: &str) -> String {
    let line = format!("tuff audit rollback u:{user} t:1h r:#global");
    console(world, &line).await.join("\n")
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_placement_into_water_rolls_back_to_the_water() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (alex, mut events) = PlayerHandle::new(100);
    world.publish_players(
        "minecraft:overworld",
        &[(&alex, [3.5, Y as f64 + 1.0, 3.5])],
    );
    subscribe(&region, &alex, &mut events, &[chunk]).await;
    let floor = BlockPos::new(3, Y - 1, 3);
    let pool = BlockPos::new(3, Y, 3);
    set_blocks(
        &region,
        vec![
            (floor, Block::Stone.default_state()),
            (pool, Block::Water.default_state()),
        ],
    )
    .await;

    region
        .request(RegionRequest::Place(place(&alex, floor, Item::Stone, 1)))
        .await;
    expect(&mut events, |event| match event {
        PlayerEvent::ActionResult { sequence: 1, .. } => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(block(&region, pool).await, Block::Stone);

    let rows = rows_by(&world, "player100").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].kind, Some(Kind::BlockPlace));
    assert_eq!(rows[0].old.as_deref(), Some("minecraft:water[level=0]"));

    let rolled = rollback(&world, "player100").await;
    assert_eq!(rolled, "Rolled back 1 blocks from 1 rows in 1 regions");
    assert_eq!(block(&region, pool).await, Block::Water);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn water_and_lava_flows_are_logged_and_rolled_back() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let (watcher, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(1, 1);
    view_around(&world, &watcher, &mut events, center).await;
    let region = world.region(center.region()).await;
    let water = BlockPos::new(20, Y, 20);
    let lava = BlockPos::new(28, Y, 28);
    let mut blocks = platform(water, 3);
    blocks.extend(platform(lava, 3));
    set_blocks(&region, blocks).await;
    set_blocks(
        &region,
        vec![
            (water, Block::Water.default_state()),
            (lava, Block::Lava.default_state()),
        ],
    )
    .await;
    world.ticks(40).await;
    for pos in neighbours(water) {
        assert_eq!(block(&region, pos).await, Block::Water);
    }
    for pos in neighbours(lava) {
        assert_eq!(block(&region, pos).await, Block::Lava);
    }

    let flowed = rows_by(&world, "#water").await;
    assert!(
        neighbours(water)
            .iter()
            .all(|pos| flowed.iter().any(|row| row.pos == [pos.x, pos.y, pos.z])),
        "{flowed:?}"
    );
    assert!(flowed.iter().all(|row| row.kind == Some(Kind::BlockPlace)));
    let lava_rows = rows_by(&world, "#lava").await;
    assert!(!lava_rows.is_empty());

    let rolled = rollback(&world, "#water").await;
    assert!(rolled.starts_with("Rolled back"), "{rolled}");
    for pos in neighbours(water) {
        assert_eq!(block(&region, pos).await, Block::Air);
    }
    assert_eq!(
        block(&region, water).await,
        Block::Water,
        "the source stays"
    );
    for pos in neighbours(lava) {
        assert_eq!(
            block(&region, pos).await,
            Block::Lava,
            "only #water rolls back"
        );
    }
    rollback(&world, "#lava").await;
    for pos in neighbours(lava) {
        assert_eq!(block(&region, pos).await, Block::Air);
    }
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn falling_blocks_are_logged_where_they_start_and_land() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (watcher, mut events) = PlayerHandle::new(100);
    view_around(&world, &watcher, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let (floor, support, gravel) = (
        BlockPos::new(8, Y, 8),
        BlockPos::new(8, Y + 1, 8),
        BlockPos::new(8, Y + 2, 8),
    );
    place_blocks(
        &world,
        &region,
        [
            (floor, Block::Stone.default_state()),
            (support, Block::Stone.default_state()),
            (gravel, Block::Gravel.default_state()),
        ],
    )
    .await;
    place_blocks(&world, &region, [(support, Block::Air.default_state())]).await;
    for _ in 0..40 {
        if block(&region, support).await == Block::Gravel {
            break;
        }
        world.ticks(1).await;
    }
    assert_eq!(block(&region, support).await, Block::Gravel);

    let rows = rows_by(&world, "#falling_block").await;
    let at = |pos: BlockPos| rows.iter().find(|row| row.pos == [pos.x, pos.y, pos.z]);
    assert_eq!(at(gravel).and_then(|row| row.kind), Some(Kind::BlockBreak));
    assert_eq!(at(support).and_then(|row| row.kind), Some(Kind::BlockPlace));

    let rolled = rollback(&world, "#falling_block").await;
    assert_eq!(rolled, "Rolled back 2 blocks from 2 rows in 1 regions");
    assert_eq!(block(&region, gravel).await, Block::Gravel);
    assert_eq!(block(&region, support).await, Block::Air);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn piston_pushes_are_logged_and_rolled_back() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (watcher, mut events) = PlayerHandle::new(100);
    view_around(&world, &watcher, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let piston = BlockPos::new(8, Y, 8);
    let pushed = BlockPos::new(9, Y, 8);
    let landed = BlockPos::new(10, Y, 8);
    let power = BlockPos::new(7, Y, 8);
    let facing_east: BlockState = "minecraft:piston[facing=east]".parse().unwrap();
    let mut blocks = platform(BlockPos::new(8, Y, 8), 3);
    blocks.push((piston, facing_east));
    blocks.push((pushed, Block::Stone.default_state()));
    place_blocks(&world, &region, blocks).await;
    place_blocks(
        &world,
        &region,
        [(power, Block::RedstoneBlock.default_state())],
    )
    .await;
    world.ticks(4).await;
    assert_eq!(block(&region, landed).await, Block::Stone);
    assert_eq!(block(&region, pushed).await, Block::PistonHead);

    let rows = rows_by(&world, "#piston").await;
    assert!(
        rows.iter()
            .any(|row| row.pos == [landed.x, landed.y, landed.z]),
        "{rows:?}"
    );

    let rolled = rollback(&world, "#piston").await;
    assert!(rolled.starts_with("Rolled back 3 blocks"), "{rolled}");
    assert_eq!(block(&region, pushed).await, Block::Stone);
    assert_eq!(block(&region, landed).await, Block::Air);
    assert_eq!(block_at(&region, piston).await, Some(facing_east));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decaying_leaves_are_logged_as_decay() {
    let dir = TestDir::new();
    let world = start(dir.path());
    world.game_rules.send_modify(|rules| {
        rules
            .set("random_tick_speed", RuleValue::Int(200))
            .expect("a known rule");
    });
    let (watcher, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &watcher, &mut events, center).await;
    let region = world.region(center.region()).await;
    let log = BlockPos::new(88, Y, 88);
    let leaf = BlockPos::new(89, Y, 88);
    let near_log = Block::OakLeaves.default_state().with_distance(1).unwrap();
    set_blocks(
        &region,
        vec![(log, Block::OakLog.default_state()), (leaf, near_log)],
    )
    .await;
    set_blocks(&region, vec![(log, Block::Air.default_state())]).await;
    for _ in 0..400 {
        if block(&region, leaf).await == Block::Air {
            break;
        }
        world.ticks(5).await;
    }
    assert_eq!(block(&region, leaf).await, Block::Air, "the leaves decay");

    let rows = rows_by(&world, "#decay").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].pos, [leaf.x, leaf.y, leaf.z]);
    assert_eq!(rows[0].kind, Some(Kind::BlockBreak));
    assert_eq!(rows[0].material.as_deref(), Some("minecraft:oak_leaves"));

    rollback(&world, "#decay").await;
    assert_eq!(block(&region, leaf).await, Block::OakLeaves);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explosions_are_logged_and_rolled_back() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (watcher, mut events) = PlayerHandle::new(100);
    view_around(&world, &watcher, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let center = BlockPos::new(8, Y, 8);
    let mut blocks = platform(BlockPos::new(8, Y + 1, 8), 2);
    blocks.extend(platform(BlockPos::new(8, Y + 2, 8), 2));
    set_blocks(&region, blocks).await;
    region
        .request(RegionRequest::Run(
            crate::world::region_api::RegionTask::new(|world| {
                world.explode(crate::world::explosion::Explosion {
                    dangerous_wither_skull: false,
                    spares_nether_portals: false,
                    center: [8.5, Y as f64 + 0.5, 8.5],
                    radius: 3.0,
                    fire: false,
                    blocks: crate::world::explosion::BlockInteraction::Destroy,
                    damage_type: &tuff_vanilla_data::damage_type::EXPLOSION,
                    igniter: None,
                    culprit: None,
                });
            }),
        ))
        .await;
    world.ticks(2).await;
    assert_eq!(block(&region, center).await, Block::Air);

    let rows = rows_by(&world, "#explosion").await;
    assert!(
        rows.iter()
            .any(|row| row.pos == [center.x, center.y, center.z]),
        "{rows:?}"
    );
    assert!(rows.iter().all(|row| row.kind == Some(Kind::BlockBreak)));
    rollback(&world, "#explosion").await;
    assert_eq!(block(&region, center).await, Block::Stone);
    world.shutdown().await;
}

fn square(from: (i32, i32), to: (i32, i32), y: i32) -> Vec<BlockPos> {
    let mut positions = Vec::new();
    for x in from.0..=to.0 {
        for z in from.1..=to.1 {
            positions.push(BlockPos::new(x, y, z));
        }
    }
    positions
}

async fn count(region: &RegionHandle, positions: &[BlockPos], kind: Block) -> usize {
    let mut found = 0;
    for pos in positions {
        if block(region, *pos).await == kind {
            found += 1;
        }
    }
    found
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn burnt_blocks_are_logged_as_fire_and_rolled_back() {
    let dir = TestDir::new();
    let world = start(dir.path());
    world.game_rules.send_modify(|rules| {
        rules
            .set("fire_spread_radius_around_player", RuleValue::Int(-1))
            .expect("a known rule");
    });
    let (watcher, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &watcher, &mut events, center).await;
    let region = world.region(center.region()).await;
    let floor = square((84, 84), (90, 90), Y);
    let mut changes: Vec<_> = floor
        .iter()
        .map(|pos| (*pos, Block::OakPlanks.default_state()))
        .collect();
    changes.push((BlockPos::new(87, Y + 1, 87), Block::Fire.default_state()));
    set_blocks(&region, changes).await;
    let above: Vec<BlockPos> = floor
        .iter()
        .map(|p| BlockPos::new(p.x, Y + 1, p.z))
        .collect();
    for _ in 0..400 {
        if count(&region, &floor, Block::OakPlanks).await <= 40 {
            break;
        }
        world.ticks(5).await;
        if count(&region, &above, Block::Fire).await == 0 {
            for (pos, top) in floor.iter().zip(&above) {
                if block(&region, *pos).await == Block::OakPlanks {
                    set_blocks(&region, vec![(*top, Block::Fire.default_state())]).await;
                    break;
                }
            }
        }
    }
    assert!(
        count(&region, &floor, Block::OakPlanks).await <= 40,
        "the floor burns"
    );

    let rows = rows_by(&world, "#fire").await;
    assert!(
        rows.iter().any(|row| row.kind == Some(Kind::BlockBreak)
            && row.material.as_deref() == Some("minecraft:oak_planks")),
        "{rows:?}"
    );
    rollback(&world, "#fire").await;
    assert_eq!(count(&region, &floor, Block::OakPlanks).await, floor.len());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_grown_tree_is_logged_as_grow_and_rolled_back_to_its_sapling() {
    let dir = TestDir::new();
    let world = start(dir.path());
    world.game_rules.send_modify(|rules| {
        rules
            .set("random_tick_speed", RuleValue::Int(500))
            .expect("a known rule");
    });
    let (watcher, mut events) = PlayerHandle::new(100);
    let center = ChunkPos::new(5, 5);
    view_around(&world, &watcher, &mut events, center).await;
    let region = world.region(center.region()).await;
    let mut changes: Vec<_> = square((90, 90), (94, 94), Y)
        .into_iter()
        .map(|pos| (pos, Block::GrassBlock.default_state()))
        .collect();
    let sapling = BlockPos::new(92, Y + 1, 92);
    changes.push((sapling, Block::OakSapling.default_state()));
    set_blocks(&region, changes).await;
    for _ in 0..600 {
        if block(&region, sapling).await == Block::OakLog {
            break;
        }
        world.ticks(5).await;
    }
    assert_eq!(
        block(&region, sapling).await,
        Block::OakLog,
        "the sapling grows"
    );

    let rows = rows_by(&world, "#grow").await;
    assert!(
        rows.iter()
            .any(|row| row.pos == [sapling.x, sapling.y, sapling.z]),
        "{rows:?}"
    );
    rollback(&world, "#grow").await;
    assert_eq!(block(&region, sapling).await, Block::OakSapling);
    assert_eq!(
        block(&region, BlockPos::new(92, Y, 92)).await,
        Block::GrassBlock
    );
    assert_eq!(
        block(&region, BlockPos::new(92, Y + 3, 92)).await,
        Block::Air,
        "the trunk goes"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_a_dispenser_places_is_logged_as_dispenser() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (watcher, mut events) = PlayerHandle::new(100);
    view_around(&world, &watcher, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let dispenser = BlockPos::new(8, Y, 8);
    let front = BlockPos::new(9, Y, 8);
    set_blocks(&region, platform(dispenser, 3)).await;
    let bucket = tuff_nbt::Compound::new()
        .with("id", "minecraft:dispenser")
        .with(
            "Items",
            tuff_nbt::Tag::List(vec![tuff_nbt::Tag::Compound(
                tuff_nbt::Compound::new()
                    .with("Slot", tuff_nbt::Tag::Byte(0))
                    .with("id", "minecraft:lava_bucket")
                    .with("count", tuff_nbt::Tag::Int(1)),
            )]),
        );
    let state: BlockState = "minecraft:dispenser[facing=east]".parse().unwrap();
    let (reply, answer) = oneshot::channel();
    let op = BlockOp::Paste {
        changes: vec![(dispenser, state, Some(bucket))],
        strict: false,
    };
    region.request(RegionRequest::Blocks { op, reply }).await;
    answer.await.unwrap();
    place_blocks(
        &world,
        &region,
        [(BlockPos::new(7, Y, 8), Block::RedstoneBlock.default_state())],
    )
    .await;
    world.ticks(6).await;
    assert_eq!(block(&region, front).await, Block::Lava);

    let rows = rows_by(&world, "#dispenser").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].pos, [front.x, front.y, front.z]);
    rollback(&world, "#dispenser").await;
    assert_eq!(block(&region, front).await, Block::Air);
    world.shutdown().await;
}

#[derive(Default)]
struct Recorder(std::sync::Mutex<Vec<crate::world::region_api::BlockChange>>);

impl crate::world::region_api::RegionHooks for Recorder {
    fn observes_block_changes(&self) -> bool {
        true
    }

    fn block_changed(
        &self,
        world: &mut dyn crate::world::region_api::RegionWorld,
        change: &crate::world::region_api::BlockChange,
    ) {
        assert_eq!(world.block(change.pos), Some(change.old), "still in place");
        self.0.lock().unwrap().push(*change);
    }
}

struct RecorderFeature(Arc<Recorder>);

impl crate::feature::Feature for RecorderFeature {
    fn id(&self) -> &'static str {
        "block-change-recorder"
    }

    fn load(&self, cx: &mut crate::feature::LoadContext) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            cx.registries().region_hooks.push(self.0.clone());
        }
        Ok(())
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn plugins_see_each_block_change_with_its_cause_before_it_lands() {
    use crate::world::region_api::{BlockCause, BlockChange};
    let dir = TestDir::new();
    let recorder = Arc::new(Recorder::default());
    let world = TestWorld::start_with_features(
        dir.path(),
        vec![Box::new(RecorderFeature(recorder.clone()))],
    );
    let region = world.region(ChunkPos::new(0, 0).region()).await;
    let (stone, gold) = (BlockPos::new(1, Y, 1), BlockPos::new(2, Y, 1));
    set_blocks(&region, vec![(stone, Block::Stone.default_state())]).await;
    region
        .request(RegionRequest::Run(
            crate::world::region_api::RegionTask::new(move |world| {
                world.caused(BlockCause::Weather, |world| {
                    world.set_block_flags(gold, Block::GoldBlock.default_state(), 3);
                    assert_eq!(world.block_cause(), BlockCause::Weather);
                });
                assert_eq!(world.block_cause(), BlockCause::World, "restored");
            }),
        ))
        .await;
    block_at(&region, gold).await;
    let changes = recorder.0.lock().unwrap().clone();
    let air = Block::Air.default_state();
    assert!(changes.contains(&BlockChange {
        pos: stone,
        old: air,
        new: Block::Stone.default_state(),
        cause: BlockCause::World,
    }));
    assert!(changes.contains(&BlockChange {
        pos: gold,
        old: air,
        new: Block::GoldBlock.default_state(),
        cause: BlockCause::Weather,
    }));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tree_growing_across_a_region_border_keeps_the_grow_cause() {
    let dir = TestDir::new();
    let world = start(dir.path());
    world.game_rules.send_modify(|rules| {
        rules
            .set("random_tick_speed", RuleValue::Int(500))
            .expect("a known rule");
    });
    let (watcher, mut events) = PlayerHandle::new(100);
    view_around(&world, &watcher, &mut events, ChunkPos::new(31, 6)).await;
    view_around(&world, &watcher, &mut events, ChunkPos::new(32, 6)).await;
    let west = world.region(ChunkPos::new(31, 0).region()).await;
    let east = world.region(ChunkPos::new(32, 0).region()).await;
    world.ticks(3).await;
    let grass = |from: (i32, i32), to: (i32, i32)| -> Vec<_> {
        square(from, to, Y)
            .into_iter()
            .map(|pos| (pos, Block::GrassBlock.default_state()))
            .collect()
    };
    set_blocks(&east, grass((512, 100), (516, 108))).await;
    let mut changes = grass((506, 100), (511, 108));
    let sapling = BlockPos::new(511, Y + 1, 104);
    changes.push((sapling, Block::OakSapling.default_state()));
    set_blocks(&west, changes).await;
    for _ in 0..600 {
        if block(&west, sapling).await == Block::OakLog {
            break;
        }
        world.ticks(5).await;
    }
    assert_eq!(block(&west, sapling).await, Block::OakLog);
    world.ticks(10).await;

    let rows = rows_by(&world, "#grow").await;
    assert!(
        rows.iter().any(|row| row.pos[0] >= 512),
        "leaves across the border are logged: {rows:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bone_meal_growth_is_logged_under_the_player() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(5, 5);
    let region = world.region(chunk.region()).await;
    let (alex, mut events) = PlayerHandle::new(100);
    world.publish_players(
        "minecraft:overworld",
        &[(&alex, [92.5, Y as f64 + 1.0, 89.5])],
    );
    view_around(&world, &alex, &mut events, chunk).await;
    let mut changes: Vec<_> = square((88, 88), (96, 96), Y)
        .into_iter()
        .map(|pos| (pos, Block::GrassBlock.default_state()))
        .collect();
    let sapling = BlockPos::new(92, Y + 1, 92);
    changes.push((sapling, Block::OakSapling.default_state()));
    set_blocks(&region, changes).await;

    let mut sequence = 0;
    for _ in 0..80 {
        sequence += 1;
        let mut request = place(&alex, sapling, Item::BoneMeal, sequence);
        request.sequence = sequence;
        region.request(RegionRequest::Place(request)).await;
        world.ticks(2).await;
        if block(&region, sapling).await == Block::OakLog {
            break;
        }
    }
    assert_eq!(block(&region, sapling).await, Block::OakLog);

    let rows = rows_by(&world, "player100").await;
    assert!(
        rows.iter()
            .any(|row| row.pos == [sapling.x, sapling.y, sapling.z]),
        "{rows:?}"
    );
    assert!(rows_by(&world, "#grow").await.is_empty());
    world.shutdown().await;
}

fn snowy_flat(_: &storage::level::DimensionSettings, _: i64) -> Arc<dyn generator::ChunkGenerator> {
    Arc::new(tuff_test_support::placeholder::Layered {
        layers: vec![(62, Block::Stone.default_state())],
        biome: "minecraft:snowy_plains",
        sea_level: 63,
    })
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn snow_and_ice_from_the_weather_are_logged_as_weather() {
    let dir = TestDir::new();
    let world = TestWorld::start_generated_with_features(
        Storage::default().open(dir.path()).unwrap(),
        snowy_flat,
        vec![Box::new(AuditLogFeature::at(
            dir.path().join("audit.sqlite"),
        ))],
    );
    world.game_rules.send_modify(|rules| {
        rules
            .set("random_tick_speed", RuleValue::Int(300))
            .expect("a known rule");
    });
    world.weather.send_modify(|weather| {
        weather.raining = true;
        weather.rain_level = 1.0;
    });
    let (watcher, mut events) = PlayerHandle::new(100);
    let chunk = ChunkPos::new(5, 5);
    view_around(&world, &watcher, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let pond = BlockPos::new(88, 62, 88);
    set_blocks(&region, vec![(pond, Block::Water.default_state())]).await;
    let ground = BlockPos::new(92, 63, 92);
    for _ in 0..400 {
        let snowed = block(&region, ground).await == Block::Snow;
        if snowed && block(&region, pond).await == Block::Ice {
            break;
        }
        world.ticks(5).await;
    }
    assert_eq!(block(&region, pond).await, Block::Ice);
    assert_eq!(block(&region, ground).await, Block::Snow);

    let rows = rows_by(&world, "#weather").await;
    let at = |pos: BlockPos| rows.iter().any(|row| row.pos == [pos.x, pos.y, pos.z]);
    assert!(at(pond) && at(ground), "{rows:?}");

    rollback(&world, "#weather").await;
    assert_eq!(block(&region, pond).await, Block::Water);
    assert_eq!(block(&region, ground).await, Block::Air);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_enderman_picking_up_a_block_is_logged_under_the_enderman() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (watcher, mut events) = PlayerHandle::new(100);
    world.publish_players(
        "minecraft:overworld",
        &[(&watcher, [14.5, Y as f64 + 1.0, 14.5])],
    );
    view_around(&world, &watcher, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let dirt: Vec<BlockPos> = square((6, 6), (10, 10), Y + 1)
        .into_iter()
        .filter(|pos| (pos.x, pos.z) != (8, 8))
        .collect();
    let mut changes = platform(BlockPos::new(8, Y + 1, 8), 6);
    changes.extend(dirt.iter().map(|pos| (*pos, Block::Dirt.default_state())));
    set_blocks(&region, changes).await;
    summon(
        &region,
        tuff_vanilla_data::registry::EntityType::Enderman,
        Compound::new(),
        [8.5, Y as f64 + 1.0, 8.5],
    )
    .await;
    for _ in 0..200 {
        if count(&region, &dirt, Block::Dirt).await < dirt.len() {
            break;
        }
        world.ticks(5).await;
    }
    assert!(count(&region, &dirt, Block::Dirt).await < dirt.len());

    let rows = rows_by(&world, "#enderman").await;
    assert!(
        rows.iter().any(|row| row.kind == Some(Kind::BlockBreak)
            && row.material.as_deref() == Some("minecraft:dirt")),
        "{rows:?}"
    );
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_deferred_fall_keeps_its_own_cause_not_the_breakers() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (alex, mut events) = PlayerHandle::new(100);
    world.publish_players(
        "minecraft:overworld",
        &[(&alex, [9.5, Y as f64 + 1.0, 8.5])],
    );
    view_around(&world, &alex, &mut events, chunk).await;
    let (floor, support, sand) = (
        BlockPos::new(8, Y, 8),
        BlockPos::new(8, Y + 1, 8),
        BlockPos::new(8, Y + 2, 8),
    );
    set_blocks(
        &region,
        vec![
            (floor, Block::Stone.default_state()),
            (support, Block::Stone.default_state()),
            (sand, Block::Sand.default_state()),
        ],
    )
    .await;
    region
        .request(RegionRequest::Break {
            player: alex.clone(),
            pos: support,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 1,
        })
        .await;
    for _ in 0..40 {
        if block(&region, support).await == Block::Sand {
            break;
        }
        world.ticks(1).await;
    }
    assert_eq!(block(&region, support).await, Block::Sand);

    let player_rows = rows_by(&world, "player100").await;
    assert_eq!(player_rows.len(), 1, "{player_rows:?}");
    assert_eq!(player_rows[0].pos, [support.x, support.y, support.z]);
    let fall_rows = rows_by(&world, "#falling_block").await;
    assert_eq!(fall_rows.len(), 2, "{fall_rows:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_player_break_is_logged_and_rolled_back() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (alex, mut events) = PlayerHandle::new(100);
    world.publish_players(
        "minecraft:overworld",
        &[(&alex, [9.5, Y as f64 + 1.0, 8.5])],
    );
    view_around(&world, &alex, &mut events, chunk).await;
    let target = BlockPos::new(8, Y, 8);
    set_blocks(&region, vec![(target, Block::Dirt.default_state())]).await;
    region
        .request(RegionRequest::Break {
            player: alex.clone(),
            pos: target,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 1,
        })
        .await;
    world.ticks(2).await;
    assert_eq!(block(&region, target).await, Block::Air);

    let rows = rows_by(&world, "player100").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].kind, Some(Kind::BlockBreak));
    assert_eq!(rows[0].material.as_deref(), Some("minecraft:dirt"));

    rollback(&world, "player100").await;
    assert_eq!(block(&region, target).await, Block::Dirt);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaking_an_infested_block_is_logged_under_the_player_and_rolls_back() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (alex, mut events) = PlayerHandle::new(100);
    world.publish_players(
        "minecraft:overworld",
        &[(&alex, [9.5, Y as f64 + 1.0, 8.5])],
    );
    view_around(&world, &alex, &mut events, chunk).await;
    let target = BlockPos::new(8, Y, 8);
    set_blocks(
        &region,
        vec![(target, Block::InfestedStone.default_state())],
    )
    .await;
    region
        .request(RegionRequest::Break {
            player: alex.clone(),
            pos: target,
            tool: None,
            mode: BreakMode::Finish,
            sequence: 1,
        })
        .await;
    world.ticks(2).await;
    assert_eq!(block(&region, target).await, Block::Air);

    let rows = rows_by(&world, "player100").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0].material.as_deref(),
        Some("minecraft:infested_stone")
    );
    assert!(rows_by(&world, "#silverfish").await.is_empty());

    rollback(&world, "player100").await;
    assert_eq!(block(&region, target).await, Block::InfestedStone);
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hurt_withers_griefing_is_logged_as_wither_and_rolled_back() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (watcher, mut events) = PlayerHandle::new(100);
    world.publish_players(
        "minecraft:overworld",
        &[(&watcher, [14.5, Y as f64 + 1.0, 14.5])],
    );
    view_around(&world, &watcher, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let mut cube = Vec::new();
    for y in Y..Y + 12 {
        for pos in square((2, 2), (14, 14), y) {
            cube.push(pos);
        }
    }
    let changes = cube
        .iter()
        .map(|pos| (*pos, Block::Dirt.default_state()))
        .collect();
    set_blocks(&region, changes).await;
    summon(
        &region,
        tuff_vanilla_data::registry::EntityType::Wither,
        Compound::new().with("Invul", 0i32),
        [8.5, Y as f64 + 5.0, 8.5],
    )
    .await;
    let (id, snapshot) = oneshot::channel();
    region.request(RegionRequest::Entities { reply: id }).await;
    let target = snapshot.await.unwrap()[0].id;
    let attack = crate::world::entity::EntityAttack {
        attacker: watcher.id,
        attacker_uuid: 77,
        attacker_position: [8.5, Y as f64 + 5.0, 8.5],
        attacker_yaw: 0.0,
        attacker_pitch: 0.0,
        damage: 1.0,
        knockback: 0.0,
        weapon: None,
        critical: false,
        magic: false,
        sweep: false,
        part: None,
    };
    region
        .request(RegionRequest::AttackEntity {
            attacker: watcher.clone(),
            target,
            attack,
            sweep: None,
        })
        .await;
    world.ticks(40).await;

    let rows = rows_by(&world, "#wither").await;
    assert!(
        rows.iter().any(|row| row.kind == Some(Kind::BlockBreak)
            && row.material.as_deref() == Some("minecraft:dirt")),
        "{rows:?}"
    );
    let broken = cube.len() - count(&region, &cube, Block::Dirt).await;
    assert!(broken > 0);

    rollback(&world, "#wither").await;
    assert_eq!(count(&region, &cube, Block::Dirt).await, cube.len());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_silverfish_hiding_in_stone_is_logged_as_silverfish_and_rolled_back() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (watcher, mut events) = PlayerHandle::new(100);
    world.publish_players(
        "minecraft:overworld",
        &[(&watcher, [14.5, Y as f64 + 1.0, 14.5])],
    );
    view_around(&world, &watcher, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let mut shell = Vec::new();
    for y in Y..Y + 3 {
        for pos in square((2, 2), (4, 4), y) {
            if (pos.x, pos.y, pos.z) != (3, Y + 1, 3) {
                shell.push(pos);
            }
        }
    }
    let changes = shell
        .iter()
        .map(|pos| (*pos, Block::Stone.default_state()))
        .collect();
    set_blocks(&region, changes).await;
    summon(
        &region,
        tuff_vanilla_data::registry::EntityType::Silverfish,
        Compound::new(),
        [3.5, Y as f64 + 1.0, 3.5],
    )
    .await;
    for _ in 0..100 {
        if count(&region, &shell, Block::InfestedStone).await > 0 {
            break;
        }
        world.ticks(5).await;
    }
    assert_eq!(count(&region, &shell, Block::InfestedStone).await, 1);

    let rows = rows_by(&world, "#silverfish").await;
    assert!(!rows.is_empty(), "{rows:?}");

    rollback(&world, "#silverfish").await;
    assert_eq!(count(&region, &shell, Block::Stone).await, shell.len());
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ravager_trampling_leaves_is_logged_as_ravager_and_rolled_back() {
    let dir = TestDir::new();
    let world = start(dir.path());
    let chunk = ChunkPos::new(0, 0);
    let (watcher, mut events) = PlayerHandle::new(100);
    world.publish_players(
        "minecraft:overworld",
        &[(&watcher, [14.5, Y as f64 + 1.0, 14.5])],
    );
    view_around(&world, &watcher, &mut events, chunk).await;
    let region = world.region(chunk.region()).await;
    let mut leaves = Vec::new();
    for y in Y + 1..Y + 4 {
        for z in 4..=12 {
            leaves.push(BlockPos::new(10, y, z));
        }
    }
    let mut changes = platform(BlockPos::new(8, Y + 1, 8), 6);
    changes.extend(
        leaves
            .iter()
            .map(|pos| (*pos, Block::OakLeaves.default_state())),
    );
    set_blocks(&region, changes).await;
    summon(
        &region,
        tuff_vanilla_data::registry::EntityType::Ravager,
        Compound::new().with(
            "Motion",
            Tag::List([0.8, 0.0, 0.0].map(Tag::Double).to_vec()),
        ),
        [7.5, Y as f64 + 1.0, 8.5],
    )
    .await;
    for _ in 0..100 {
        if count(&region, &leaves, Block::OakLeaves).await < leaves.len() {
            break;
        }
        world.ticks(2).await;
    }
    assert!(count(&region, &leaves, Block::OakLeaves).await < leaves.len());

    let rows = rows_by(&world, "#ravager").await;
    assert!(
        rows.iter().any(|row| row.kind == Some(Kind::BlockBreak)),
        "{rows:?}"
    );
    rollback(&world, "#ravager").await;
    assert_eq!(
        count(&region, &leaves, Block::OakLeaves).await,
        leaves.len()
    );
    world.shutdown().await;
}
