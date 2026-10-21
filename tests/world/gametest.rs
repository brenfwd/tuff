//! Game tests driven through `/test`: data pack test instances placed on
//! the grid, block-based and function tests passing and failing, reruns
//! of failures, locating and clearing.

use super::pack_predicates::{console, pack};
use super::*;
use tuff_nbt::{Compound, NamedTag, Tag};

/// A structure template `size` large holding test blocks at `blocks`
/// (`(pos, mode, message)`), as gzip NBT.
fn template(size: [i32; 3], blocks: &[([i32; 3], &str, &str)]) -> Vec<u8> {
    let ints = |v: [i32; 3]| Tag::List(v.iter().map(|i| Tag::Int(*i)).collect());
    let mut palette = Vec::new();
    let mut list = Vec::new();
    for (index, (pos, mode, message)) in blocks.iter().enumerate() {
        let mut properties = Compound::new();
        properties.insert("mode", (*mode).to_owned());
        let mut entry = Compound::new();
        entry.insert("Name", "minecraft:test_block".to_owned());
        entry.insert("Properties", properties);
        palette.push(Tag::Compound(entry));
        let mut nbt = Compound::new();
        nbt.insert("id", "minecraft:test_block".to_owned());
        nbt.insert("mode", (*mode).to_owned());
        nbt.insert("message", (*message).to_owned());
        nbt.insert("powered", false);
        let mut block = Compound::new();
        block.insert("pos", ints(*pos));
        block.insert("state", index as i32);
        block.insert("nbt", nbt);
        list.push(Tag::Compound(block));
    }
    let mut root = Compound::new();
    root.insert("DataVersion", tuff_vanilla_data::version::DATA_VERSION);
    root.insert("size", ints(size));
    root.insert("palette", Tag::List(palette));
    root.insert("blocks", Tag::List(list));
    root.insert("entities", Tag::List(Vec::new()));
    NamedTag::new("", root)
        .to_bytes(tuff_nbt::Compression::Gzip)
        .unwrap()
}

fn instance(kind: &str, structure: &str, extra: &str) -> String {
    format!(
        r#"{{"type":"minecraft:{kind}","environment":"minecraft:default","structure":"{structure}","max_ticks":20{extra}}}"#
    )
}

fn test_pack(root: &Path) {
    pack(
        root,
        "gametests",
        &[
            (
                "gt/test_instance/passes.json",
                &instance("block_based", "gt:pass", ""),
            ),
            (
                "gt/test_instance/fails.json",
                &instance("block_based", "gt:fail", ""),
            ),
            (
                "gt/test_instance/func.json",
                &instance(
                    "function",
                    "gt:pass",
                    r#","function":"minecraft:always_pass""#,
                ),
            ),
            (
                "gt/test_instance/optional.json",
                &instance("block_based", "gt:fail", r#","required":false"#),
            ),
            (
                "gt/test_instance/manual.json",
                &instance("block_based", "gt:pass", r#","manual_only":true"#),
            ),
        ],
    );
    let structures = root.join("datapacks/gametests/data/gt/structure");
    std::fs::create_dir_all(&structures).unwrap();
    std::fs::write(
        structures.join("pass.nbt"),
        template(
            [2, 1, 1],
            &[([0, 0, 0], "start", ""), ([1, 0, 0], "accept", "")],
        ),
    )
    .unwrap();
    std::fs::write(
        structures.join("fail.nbt"),
        template(
            [4, 1, 1],
            &[
                ([0, 0, 0], "start", ""),
                ([1, 0, 0], "fail", "boom"),
                ([3, 0, 0], "accept", ""),
            ],
        ),
    )
    .unwrap();
}

async fn summary(
    world: &TestWorld,
) -> tuff_feature_builtin_vanilla_commands::gametest::runner::Summary {
    for _ in 0..40 {
        if let Some(summary) =
            tuff_feature_builtin_vanilla_commands::gametest::last_summary(&world.shared).await
        {
            return summary;
        }
        world.ticks(1).await;
    }
    panic!("the test run did not finish");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_places_runs_and_reports_block_and_function_tests() {
    let dir = TestDir::new();
    test_pack(dir.path());
    let world = TestWorld::start(dir.path());

    let started = console(&world, "test run gt:*").await.join("\n");
    assert!(started.contains('5'), "{started}");
    let summary = summary(&world).await;
    assert_eq!(summary.total, 5, "{summary:?}");
    assert_eq!(summary.passed, 3, "{summary:?}");
    assert_eq!(summary.failed_required.len(), 1, "{summary:?}");
    assert!(
        summary.failed_required[0].starts_with("gt:fails: "),
        "{summary:?}"
    );
    assert!(summary.failed_required[0].contains("boom"), "{summary:?}");
    assert_eq!(summary.failed_optional.len(), 1, "{summary:?}");

    let mut failed =
        tuff_feature_builtin_vanilla_commands::gametest::failed_tests(&world.shared).await;
    failed.sort();
    assert_eq!(failed, ["gt:fails", "gt:optional"]);

    let rerun = console(&world, "test runfailed true").await.join("\n");
    assert!(rerun.contains('1'), "{rerun}");
    let again = super::gametest::summary(&world).await;
    assert_eq!(again.total, 1, "{again:?}");
    assert_eq!(again.failed_required.len(), 1, "{again:?}");

    let located = console(&world, "test locate gt:passes").await.join("\n");
    assert!(!located.contains("No test instances"), "{located}");

    let cleared = console(&world, "test clearall").await.join("\n");
    assert!(cleared.contains("Cleared"), "{cleared}");
    assert!(located.contains("found 1"), "{located}");
    let none = console(&world, "test clearall").await.join("\n");
    assert!(none.contains("Could not find any tests"), "{none}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_run_retries_until_failed_and_skips_unknown_selections() {
    let dir = TestDir::new();
    test_pack(dir.path());
    let world = TestWorld::start(dir.path());

    let nothing = console(&world, "test run gt:nothing_*").await.join("\n");
    assert!(nothing.contains("No tests"), "{nothing}");

    console(&world, "test run gt:passes 3").await;
    let summary = summary(&world).await;
    assert_eq!((summary.total, summary.passed), (3, 3), "{summary:?}");

    console(&world, "test run gt:fails 5 true").await;
    let halted = super::gametest::summary(&world).await;
    assert_eq!(halted.total, 1, "{halted:?}");
    assert_eq!(halted.failed_required.len(), 1, "{halted:?}");
    world.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_create_builds_an_empty_test_and_rejects_large_ones() {
    let dir = TestDir::new();
    let world = TestWorld::start(dir.path());
    let created = console(&world, "test create gt:new 3").await.join("\n");
    assert!(created.contains("gt:new"), "{created}");
    let large = console(&world, "test create gt:big 49").await.join("\n");
    assert!(large.contains("48"), "{large}");
    let reset = console(&world, "test resetthese").await.join("\n");
    assert!(reset.contains("gt:new"), "{reset}");
    world.shutdown().await;
}
