//! Data pack tags in effect where the game reads them: region checks,
//! stored predicates and command tag arguments follow each reload.

use super::pack_predicates::{console, pack};
use super::*;
use crate::world::region_api::RegionTask;

/// Whether the region owning (5, 309, 5) counts glass among
/// `#minecraft:logs`.
async fn region_sees_glass_log(world: &TestWorld) -> bool {
    let region = world.region(RegionPos::new(0, 0)).await;
    let at = BlockPos::new(5, 309, 5);
    let (reply, answer) = oneshot::channel();
    region
        .request(RegionRequest::WhenLoaded {
            min: at,
            max: at,
            run: RegionTask::new(move |_| {
                let _ = reply.send(tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::block::key::LOGS,
                    Block::Glass,
                ));
            }),
        })
        .await;
    answer.await.unwrap()
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn regions_and_predicates_follow_pack_tags_across_reloads() {
    let dir = TestDir::new();
    pack(
        dir.path(),
        "tags",
        &[
            (
                "minecraft/tags/block/logs.json",
                r#"{"values":["minecraft:glass"]}"#,
            ),
            (
                "a/tags/worldgen/biome/flat.json",
                r#"{"values":["minecraft:plains"]}"#,
            ),
        ],
    );
    pack(
        dir.path(),
        "preds",
        &[(
            "a/predicate/on_log.json",
            r##"{"type":"minecraft:location_check","offsetY":-1,
                "predicate":{"block":{"blocks":"#minecraft:logs"}}}"##,
        )],
    );
    let world = TestWorld::start(dir.path());
    console(&world, "forceload add 0 0 20 20").await;
    console(&world, "setblock 5 309 5 glass").await;
    let on_log = "execute positioned 5.5 310 5.5 if predicate a:on_log";

    assert!(region_sees_glass_log(&world).await);
    assert_eq!(console(&world, on_log).await, ["Test passed"]);
    let unknown = console(&world, "locate biome #a:nope").await.join("\n");
    assert!(unknown.contains("a:nope"), "{unknown}");

    console(&world, r#"datapack disable "file/tags""#).await;
    assert!(!region_sees_glass_log(&world).await);
    assert_eq!(console(&world, on_log).await, ["Test failed"]);
    let gone = console(&world, "locate biome #a:flat").await.join("\n");
    assert!(gone.contains("a:flat"), "{gone}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pack_tag_members_reach_built_in_recipes() {
    use tuff_item::item::ItemStack;
    use tuff_vanilla_data::Item;
    let dir = TestDir::new();
    pack(
        dir.path(),
        "planks",
        &[(
            "minecraft/tags/item/planks.json",
            r#"{"values":["minecraft:stone"]}"#,
        )],
    );
    let world = TestWorld::start(dir.path());
    let sticks = |world: &TestWorld, item| {
        world.shared.recipes().grid_result(
            &[Some(ItemStack::new(item, 1)), Some(ItemStack::new(item, 1))],
            1,
            2,
        )
    };
    let four_sticks = Some(ItemStack::new(Item::Stick, 4));
    assert_eq!(sticks(&world, Item::Stone), four_sticks);
    assert_eq!(sticks(&world, Item::BirchPlanks), four_sticks);

    console(&world, r#"datapack disable "file/planks""#).await;
    assert_eq!(sticks(&world, Item::Stone), None);
    assert_eq!(sticks(&world, Item::BirchPlanks), four_sticks);
    world.shutdown().await;
}
