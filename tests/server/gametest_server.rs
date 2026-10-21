//! The headless game test server (`tuff gametest`): a fresh world with
//! the given packs, the selected tests run on startup, a JUnit-like
//! report and the failed required count as the exit status.

use crate::common;

use std::path::Path;

use common::TempDir;
use tuff::gametest_cli::{ERROR_STATUS, Options, run};
use tuff_nbt::{Compound, NamedTag, Tag};

fn write(path: &Path, contents: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

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
    root.insert("DataVersion", 5023);
    root.insert("size", ints(size));
    root.insert("palette", Tag::List(palette));
    root.insert("blocks", Tag::List(list));
    root.insert("entities", Tag::List(Vec::new()));
    NamedTag::new("", root)
        .to_bytes(tuff_nbt::Compression::Gzip)
        .unwrap()
}

/// A packs folder holding one pack with a passing and a failing test.
fn packs(root: &Path) {
    let pack = root.join("headless");
    let current = tuff_vanilla_data::version::DATA_PACK_VERSION.major;
    write(
        &pack.join("pack.mcmeta"),
        format!(
            r#"{{"pack":{{"description":"t","min_format":{current},"max_format":{current}}}}}"#
        )
        .as_bytes(),
    );
    let data = pack.join("data/hl");
    let instance = |structure: &str| {
        format!(
            r#"{{"type":"minecraft:block_based","environment":"minecraft:default","structure":"{structure}","max_ticks":20}}"#
        )
    };
    write(
        &data.join("test_instance/passes.json"),
        instance("hl:pass").as_bytes(),
    );
    write(
        &data.join("test_instance/fails.json"),
        instance("hl:fail").as_bytes(),
    );
    write(
        &data.join("structure/pass.nbt"),
        &template(
            [2, 1, 1],
            &[([0, 0, 0], "start", ""), ([1, 0, 0], "accept", "")],
        ),
    );
    write(
        &data.join("structure/fail.nbt"),
        &template(
            [4, 1, 1],
            &[
                ([0, 0, 0], "start", ""),
                ([1, 0, 0], "fail", "boom"),
                ([3, 0, 0], "accept", ""),
            ],
        ),
    );
}

fn options(dir: &TempDir, tests: Option<&str>) -> Options {
    let source = dir.path().join("packs");
    packs(&source);
    Options {
        universe: dir.path().join("universe"),
        report: Some(dir.path().join("report.xml")),
        tests: tests.map(str::to_owned),
        repeat_count: 1,
        packs: Some(source),
        origin: Some([0, 4, 0]),
        ..Options::default()
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_run_reports_failures_and_exits_with_their_count() {
    let dir = TempDir::new();
    let status = run(options(&dir, Some("hl:*"))).await.unwrap();
    assert_eq!(status, 1);
    let report = std::fs::read_to_string(dir.path().join("report.xml")).unwrap();
    assert!(report.contains("name=\"hl:passes\""), "{report}");
    assert!(report.contains("<failure message="), "{report}");
    assert!(report.contains("boom"), "{report}");
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn headless_run_passes_and_rejects_empty_selections() {
    let dir = TempDir::new();
    assert_eq!(run(options(&dir, Some("hl:passes"))).await.unwrap(), 0);
    let dir = TempDir::new();
    assert_eq!(
        run(options(&dir, Some("hl:missing"))).await.unwrap(),
        ERROR_STATUS
    );
}
