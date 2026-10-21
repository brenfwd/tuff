//! The structure block editor on a running world: an edit lands in the
//! block, Save writes the template to the world's generated folder and the
//! template is then found by name.

use tokio::sync::oneshot;
use tuff_core::protocol::play::StructureBlockEdit;
use tuff_core::world::messages::{PlayerHandle, RegionHandle, RegionRequest};
use tuff_core::world::region_api::RegionTask;
use tuff_core::world::session::structure_block::{StructureOutcome, apply_edit};
use tuff_test_support::{TestDir, TestWorld, place_blocks, subscribe};
use tuff_vanilla_data::Block;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

/// The template sources are process-wide, so tests that point them at
/// their own folder take turns.
static SOURCES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn run<T: Send + 'static>(
    region: &RegionHandle,
    task: impl FnOnce(&mut dyn tuff_core::world::region_api::RegionWorld) -> T + Send + 'static,
) -> T {
    let (reply, answer) = oneshot::channel();
    let task = RegionTask::new(move |world| {
        let _ = reply.send(task(world));
    });
    assert!(region.request(RegionRequest::Run(task)).await);
    answer.await.unwrap()
}

fn edit(position: BlockPos, update: i32, mode: i32) -> StructureBlockEdit {
    StructureBlockEdit {
        position,
        update,
        mode,
        name: "tuff:test_area".into(),
        offset: [0, 1, 0],
        size: [2, 1, 2],
        mirror: 0,
        rotation: 0,
        data: String::new(),
        integrity: 1.0,
        seed: 0,
        ignore_entities: true,
        show_air: false,
        show_bounding_box: true,
        strict: false,
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saving_an_area_writes_a_template_the_world_can_load() {
    let _sources = SOURCES.lock().await;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    tuff_feature_builtin_vanilla_worldgen::structure::template::set_generated_dir(Some(
        dir.path().join("generated"),
    ));
    let chunk = ChunkPos::new(0, 0);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    subscribe(&region, &viewer, &mut events, &[chunk]).await;
    let block = BlockPos::new(4, 70, 4);
    place_blocks(
        &world,
        &region,
        [
            (block, Block::StructureBlock.default_state()),
            (BlockPos::new(4, 71, 4), Block::Stone.default_state()),
            (BlockPos::new(5, 71, 5), Block::Dirt.default_state()),
        ],
    )
    .await;
    let outcome = run(&region, move |world| {
        match apply_edit(world, &edit(block, 1, 0)) {
            StructureOutcome::Say(message) => message.plain(),
            _ => "other".to_owned(),
        }
    })
    .await;
    assert!(outcome.starts_with("Structure saved"), "{outcome}");
    let saved = dir.path().join("generated/tuff/structure/test_area.nbt");
    assert!(saved.exists(), "{saved:?}");
    let template =
        tuff_feature_builtin_vanilla_worldgen::structure::template::get("tuff:test_area");
    assert_eq!(template.size, [2, 1, 2]);
    let loaded = run(&region, move |world| {
        matches!(
            apply_edit(world, &edit(block, 2, 1)),
            StructureOutcome::Place { .. }
        )
    })
    .await;
    assert!(loaded);
    let data = run(&region, move |world| world.saved_block_entity(block)).await;
    assert_eq!(data.unwrap().get_str("mode"), Some("LOAD"));
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn redstone_saves_in_memory_loads_and_forgets() {
    use tuff_core::world::growth::GrowthLevel;
    use tuff_core::world::session::structure_block::{place, redstone_power};
    use tuff_feature_builtin_vanilla_worldgen::structure::template;

    let _sources = SOURCES.lock().await;
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    template::set_generated_dir(Some(dir.path().join("generated")));
    let chunk = ChunkPos::new(2, 2);
    let region = world.region(chunk.region()).await;
    let (viewer, mut events) = PlayerHandle::new(100);
    let window: Vec<ChunkPos> = (1..=3)
        .flat_map(|x| (1..=3).map(move |z| ChunkPos::new(x, z)))
        .collect();
    subscribe(&region, &viewer, &mut events, &window).await;
    let saver = BlockPos::new(34, 70, 34);
    let loader = BlockPos::new(42, 70, 34);
    place_blocks(
        &world,
        &region,
        [
            (saver, Block::StructureBlock.default_state()),
            (loader, Block::StructureBlock.default_state()),
            (BlockPos::new(34, 71, 34), Block::GoldBlock.default_state()),
            (
                BlockPos::new(35, 71, 35),
                Block::DiamondBlock.default_state(),
            ),
        ],
    )
    .await;
    run(&region, move |world| {
        let mut save = edit(saver, 0, 0);
        save.name = "tuff:redstone_area".into();
        apply_edit(world, &save);
        assert!(redstone_power(world, saver, true).is_none());
        assert!(redstone_power(world, saver, true).is_none());
        assert!(redstone_power(world, saver, false).is_none());
    })
    .await;
    let kept = template::get("tuff:redstone_area");
    assert_eq!(kept.size, [2, 1, 2]);
    assert!(
        !dir.path()
            .join("generated/tuff/structure/redstone_area.nbt")
            .exists()
    );

    let triggered = run(&region, move |world| {
        let mut load = edit(loader, 0, 1);
        load.name = "tuff:redstone_area".into();
        load.size = [0, 0, 0];
        apply_edit(world, &load);
        let triggered = redstone_power(world, loader, true);
        let size: Vec<_> = ["sizeX", "sizeY", "sizeZ"]
            .iter()
            .map(|key| world.saved_block_entity(loader).unwrap().get_int(key))
            .collect();
        assert_eq!(size, [Some(2), Some(1), Some(2)]);
        assert!(redstone_power(world, loader, true).is_none());
        triggered.map(|t| (t, world.shared().clone(), world.dimension_id()))
    })
    .await;
    let ((request, extent), shared, dimension) = triggered.expect("load triggers a placement");
    place(shared, dimension, request, extent).await;
    let placed = run(&region, |world| {
        [
            GrowthLevel::block(world, BlockPos::new(42, 71, 34)).map(|s| s.block()),
            GrowthLevel::block(world, BlockPos::new(43, 71, 35)).map(|s| s.block()),
        ]
    })
    .await;
    assert_eq!(placed, [Some(Block::GoldBlock), Some(Block::DiamondBlock)]);

    run(&region, move |world| {
        let mut corner = edit(saver, 0, 2);
        corner.name = "tuff:redstone_area".into();
        apply_edit(world, &corner);
        redstone_power(world, saver, true);
    })
    .await;
    assert_eq!(template::get("tuff:redstone_area").size, [0, 0, 0]);
    world.shutdown().await;
}
