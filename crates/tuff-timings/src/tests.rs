use std::sync::Arc;
use std::time::{Duration, Instant};

use super::report::Report;
use super::*;

fn spin(ns: u64) {
    clock::init();
    let start = clock::now();
    while clock::ns(clock::now().saturating_sub(start)) < ns {
        std::hint::spin_loop();
    }
}

fn record<T>(work: impl FnOnce() -> T) -> (T, Profile) {
    start().expect("no recording running");
    let out = work();
    let profile = Profile::from_snapshot(stop().expect("recording"));
    (out, profile)
}

fn nested() {
    let _outer = span!("test.outer");
    spin(20_000);
    for _ in 0..3 {
        let _inner = span!("test.inner");
        spin(10_000);
        timed!("test.leaf", spin(5_000));
    }
}

#[tuff_core::watchdog]
#[test]
fn spans_outside_a_recording_record_nothing() {
    let _serial = serial();
    let idle = span!("test.idle");
    assert!(!idle.is_recording());
    drop(idle);
    let ((), profile) = record(|| {});
    assert!(profile.nodes().iter().all(|n| n.name != "test.idle"));
}

#[tuff_core::watchdog]
#[test]
fn nested_spans_form_a_call_tree_with_self_time() {
    let _serial = serial();
    let ((), profile) = record(nested);
    let outer = profile.find(&["test.outer"]).expect("outer recorded");
    assert_eq!(outer.stats.count, 1);
    let inner = profile.find(&["test.outer", "test.inner"]).unwrap();
    assert_eq!(inner.stats.count, 3);
    let leaf = profile
        .find(&["test.outer", "test.inner", "test.leaf"])
        .unwrap();
    assert_eq!(leaf.stats.count, 3);
    assert!(leaf.stats.min_ns >= 5_000, "{:?}", leaf.stats);
    assert!(inner.stats.total_ns >= leaf.stats.total_ns + 30_000);
    assert!(outer.stats.total_ns >= inner.stats.total_ns + 20_000);
    assert_eq!(outer.self_ns(), outer.stats.total_ns - inner.stats.total_ns);
    assert!(outer.self_ns() >= 20_000);
    assert!(leaf.stats.quantile(0.5) >= leaf.stats.min_ns);
    assert!(profile.find(&["test.inner"]).is_none());
}

#[tuff_core::watchdog]
#[test]
fn tags_split_one_site_and_label_regions() {
    let _serial = serial();
    let ((), profile) = record(|| {
        for kind in ["minecraft:pig", "minecraft:cow", "minecraft:pig"] {
            let _span = span!("test.entity", Tag::Name(kind));
        }
        let _region = span!(
            "test.region",
            Tag::Region {
                dimension: 1,
                x: -2,
                z: 3
            }
        );
    });
    let entities: Vec<&Node> = profile
        .nodes()
        .into_iter()
        .filter(|n| n.name == "test.entity")
        .collect();
    assert_eq!(entities.len(), 2);
    let pig = entities
        .iter()
        .find(|n| n.tag == Tag::Name("minecraft:pig"))
        .unwrap();
    assert_eq!(pig.stats.count, 2);
    let region = profile.find(&["test.region"]).unwrap();
    let labels = Labels {
        dimensions: vec!["minecraft:overworld".into(), "minecraft:the_nether".into()],
        ..Labels::default()
    };
    assert_eq!(
        labels.tag(region.tag).as_deref(),
        Some("minecraft:the_nether -2,3")
    );
    assert_eq!(
        labels.tag(Tag::Name(
            "tuff_core::protocol::play::Foo<alloc::sync::Arc<u8>>"
        )),
        Some("Foo<Arc<u8>>".to_owned())
    );
}

#[tuff_core::watchdog]
#[test]
fn threads_keep_their_own_trees_and_merge_by_call_path() {
    let _serial = serial();
    let ((), profile) = record(|| {
        let workers: Vec<_> = (0..4)
            .map(|i| {
                std::thread::Builder::new()
                    .name(format!("timings-test-{i}"))
                    .spawn(move || {
                        for _ in 0..=i {
                            nested();
                        }
                    })
                    .unwrap()
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let mine: Vec<&ThreadProfile> = profile
        .threads
        .iter()
        .filter(|t| t.name.starts_with("timings-test-"))
        .collect();
    assert_eq!(mine.len(), 4);
    for thread in &mine {
        let i: u64 = thread.name["timings-test-".len()..].parse().unwrap();
        let outer = thread
            .roots
            .iter()
            .find(|n| n.name == "test.outer")
            .unwrap();
        assert_eq!(outer.stats.count, i + 1);
    }
    assert_eq!(profile.find(&["test.outer"]).unwrap().stats.count, 10);
    assert_eq!(
        profile
            .find(&["test.outer", "test.inner", "test.leaf"])
            .unwrap()
            .stats
            .count,
        30
    );
}

#[tuff_core::watchdog]
#[test]
fn timers_and_measured_durations_record_where_they_finish() {
    let _serial = serial();
    let ((), profile) = record(|| {
        let timer = timer!("test.async", Tag::Name("job"));
        std::thread::spawn(move || {
            spin(10_000);
            drop(timer);
        })
        .join()
        .unwrap();
        let _outer = span!("test.with_record");
        record!("test.measured", Duration::from_micros(7));
    });
    let timed = profile.find(&["test.async"]).unwrap();
    assert!(timed.asynchronous);
    assert_eq!(timed.tag, Tag::Name("job"));
    assert!(timed.stats.total_ns >= 10_000);
    let measured = profile
        .find(&["test.with_record", "test.measured"])
        .unwrap();
    assert_eq!(measured.stats.total_ns, 7_000);
}

#[tuff_core::watchdog]
#[test]
fn the_span_clock_keeps_time_with_instant() {
    let source = clock::init();
    #[cfg(all(target_arch = "aarch64", any(target_os = "macos", target_os = "linux")))]
    assert_eq!(source, clock::Source::Cntvct);
    if let Ok(expected) = std::env::var("TUFF_EXPECT_CLOCK") {
        assert_eq!(source.name(), expected, "clock source");
    }
    let ratio = (0..5)
        .map(|_| clock::drift(Duration::from_millis(20)))
        .min_by(|a, b| (a - 1.0).abs().total_cmp(&(b - 1.0).abs()))
        .unwrap();
    assert!(
        (0.99..1.01).contains(&ratio),
        "{} drifts by {ratio}",
        source.name()
    );
    let _serial = serial();
    let ((), profile) = record(|| {
        let _span = span!("test.slept");
        std::thread::sleep(Duration::from_millis(5));
    });
    let slept = profile.find(&["test.slept"]).unwrap().stats.total_ns;
    assert!((5_000_000..5_000_000_000).contains(&slept), "{slept} ns");
}

#[tuff_core::watchdog]
#[test]
fn spans_deeper_than_the_limit_are_dropped_not_misattributed() {
    let _serial = serial();
    fn deep(level: usize) {
        let _span = span!("test.deep");
        if level > 0 {
            deep(level - 1);
        }
    }
    let ((), profile) = record(|| {
        std::thread::Builder::new()
            .name("timings-deep".into())
            .spawn(|| deep(MAX_DEPTH + 10))
            .unwrap()
            .join()
            .unwrap();
    });
    let thread = profile
        .threads
        .iter()
        .find(|t| t.name == "timings-deep")
        .unwrap();
    assert_eq!(thread.dropped, 11);
    let mut depth = 0;
    let mut node = thread.roots.iter().find(|n| n.name == "test.deep");
    while let Some(n) = node {
        depth += 1;
        assert_eq!(n.stats.count, 1);
        node = n.children.first();
    }
    assert_eq!(depth, MAX_DEPTH);
}

#[tuff_core::watchdog]
#[test]
fn a_span_left_open_across_recordings_is_ignored() {
    let _serial = serial();
    start().unwrap();
    let stale = span!("test.stale");
    assert!(stale.is_recording());
    let _ = stop().unwrap();
    let ((), profile) = record(|| {
        let _fresh = span!("test.fresh");
        drop(stale);
    });
    assert!(profile.find(&["test.fresh"]).is_some());
    assert!(profile.nodes().iter().all(|n| n.name != "test.stale"));
}

#[tuff_core::watchdog]
#[test]
fn only_one_recording_runs_at_a_time() {
    let _serial = serial();
    start().unwrap();
    assert_eq!(start(), Err(RecorderError::AlreadyRecording));
    assert!(is_recording());
    stop().unwrap();
    assert!(!is_recording());
    assert_eq!(stop().err(), Some(RecorderError::NotRecording));
}

fn sample_profile() -> Profile {
    let ((), profile) = record(|| {
        for _ in 0..4 {
            record!(SERVER_TICK, Duration::from_millis(2));
        }
        nested();
        let _hook = span!(FEATURE, Tag::Name("example"));
        spin(3_000);
    });
    profile
}

#[tuff_core::watchdog]
#[test]
fn reports_have_the_versioned_schema() {
    let _serial = serial();
    let profile = sample_profile();
    let labels = Labels {
        server_name: "tuff".to_owned(),
        ..Labels::default()
    };
    let report = Report::new("timings-test", &profile, &["example", "idle"], &labels);
    let json: serde_json::Value = serde_json::from_slice(&report.to_json()).unwrap();
    assert_eq!(json["format"], "tuff-timings");
    assert_eq!(json["version"], 2);
    assert_eq!(json["id"], "timings-test");
    assert!(json["duration_ns"].as_u64().unwrap() > 0);
    assert_eq!(json["server"]["name"], "tuff");
    assert_eq!(json["config"]["max_depth"], MAX_DEPTH as u64);
    assert_eq!(json["config"]["clock"], clock::init().name());
    assert_eq!(json["ticks"]["count"], 4);
    assert_eq!(json["ticks"]["mspt"]["max"], 2.0);
    let features = json["features"].as_array().unwrap();
    assert_eq!(features[0]["id"], "example");
    assert!(features[0]["total_ns"].as_u64().unwrap() >= 3_000);
    assert_eq!(features[1]["id"], "idle");
    assert_eq!(features[1]["calls"], 0);
    let outer = json["merged"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "test.outer")
        .unwrap();
    for key in [
        "count", "total_ns", "self_ns", "min_ns", "max_ns", "p50_ns", "p90_ns", "p99_ns",
    ] {
        assert!(outer[key].is_u64(), "{key}");
    }
    assert!(outer.get("tag").is_none());
    assert!(outer.get("async").is_none());
    assert_eq!(outer["children"][0]["name"], "test.inner");
    let thread = json["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| {
            t["roots"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n["name"] == "test.outer")
        })
        .unwrap();
    assert!(thread["name"].is_string() && thread["index"].is_u64());
    let parsed = Report::from_json(&report.to_json()).unwrap();
    assert_eq!(parsed, report);
    let mut newer = json.clone();
    newer["version"] = 3.into();
    assert!(Report::from_json(&serde_json::to_vec(&newer).unwrap()).is_err());
}

#[tuff_core::watchdog]
#[test]
fn exports_fold_stacks_and_build_speedscope_profiles() {
    let _serial = serial();
    let profile = sample_profile();
    let report = Report::new("timings-test", &profile, &[], &Labels::default());
    let folded = report.folded();
    let leaf = folded
        .lines()
        .find(|l| l.starts_with("test.outer;test.inner;test.leaf "))
        .expect("leaf stack");
    let weight: u64 = leaf.rsplit(' ').next().unwrap().parse().unwrap();
    assert!(weight >= 15_000);
    assert!(folded.lines().any(|l| l.starts_with("feature [example] ")));
    let scope = report.speedscope();
    assert_eq!(
        scope["$schema"],
        "https://www.speedscope.app/file-format-schema.json"
    );
    let frames = scope["shared"]["frames"].as_array().unwrap();
    let profiles = scope["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 1 + report.threads.len());
    let merged = &profiles[0];
    assert_eq!(merged["type"], "sampled");
    assert_eq!(merged["unit"], "nanoseconds");
    let samples = merged["samples"].as_array().unwrap();
    let weights = merged["weights"].as_array().unwrap();
    assert_eq!(samples.len(), weights.len());
    let total: u64 = weights.iter().map(|w| w.as_u64().unwrap()).sum();
    assert_eq!(merged["endValue"], total);
    let leaf_sample = samples
        .iter()
        .find(|s| {
            let names: Vec<&str> = s
                .as_array()
                .unwrap()
                .iter()
                .map(|i| {
                    frames[i.as_u64().unwrap() as usize]["name"]
                        .as_str()
                        .unwrap()
                })
                .collect();
            names == ["test.outer", "test.inner", "test.leaf"]
        })
        .is_some();
    assert!(leaf_sample);
}

#[derive(Default)]
struct MemoryFiles(std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>);

impl ReportFiles for MemoryFiles {
    fn list_files(&self, prefix: &str) -> std::io::Result<Vec<String>> {
        let files = self.0.lock().unwrap();
        Ok(files
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }

    fn read_file(&self, path: &str, _max: usize) -> std::io::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().get(path).cloned())
    }

    fn write_file(&self, path: &str, contents: &[u8]) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(path.to_owned(), contents.to_vec());
        Ok(())
    }
}

#[tuff_core::watchdog]
#[tokio::test]
async fn the_controller_saves_lists_and_serves_reports() {
    let _serial = serial_async().await;
    let memory = Arc::new(MemoryFiles::default());
    let files: Arc<dyn ReportFiles> = memory.clone();
    let timings = Timings::spawn(Some(files.clone()), vec!["example"], Labels::default());
    assert!(matches!(
        timings.stop().await,
        Err(TimingsError::Recorder(RecorderError::NotRecording))
    ));
    let recording = timings
        .start(StartOptions {
            duration: None,
            by: "tester".into(),
        })
        .await
        .unwrap();
    assert_eq!(recording.by, "tester");
    assert!(matches!(
        timings.start(StartOptions::default()).await,
        Err(TimingsError::Recorder(RecorderError::AlreadyRecording))
    ));
    let other = Timings::spawn(None, Vec::new(), Labels::default());
    assert_eq!(
        other.start(StartOptions::default()).await,
        Err(TimingsError::ElsewhereRecording)
    );
    assert!(other.status().await.unwrap().busy_elsewhere);
    std::thread::spawn(nested).join().unwrap();
    record!(SERVER_TICK, Duration::from_millis(3));
    let status = timings.status().await.unwrap();
    assert!(status.recording.is_some());
    let summary = timings.stop().await.unwrap();
    assert_eq!(
        summary.file.as_deref(),
        Ok(report_file(&summary.id).as_str())
    );
    assert!(summary.id.starts_with("timings-"));
    assert_eq!(summary.ticks, 1);
    assert!(summary.top.iter().any(|(label, _)| label == "test.outer"));
    let on_disk = memory
        .0
        .lock()
        .unwrap()
        .get(&report_file(&summary.id))
        .cloned()
        .unwrap();
    assert_eq!(on_disk.len(), summary.bytes);
    Report::from_json(&on_disk).unwrap();
    assert_eq!(timings.status().await.unwrap().last, Some(summary.clone()));

    let again = timings.start(StartOptions::default()).await;
    assert!(again.is_ok());
    let second = timings.stop().await.unwrap();
    assert_ne!(second.id, summary.id);

    let list = timings.list().await.unwrap();
    let ids: Vec<&str> = list.iter().map(|i| i.id.as_str()).collect();
    assert!(ids.contains(&summary.id.as_str()) && ids.contains(&second.id.as_str()));
    let fresh = Timings::spawn(Some(files), Vec::new(), Labels::default());
    assert_eq!(fresh.fetch(&summary.id).await.unwrap(), on_disk);
    assert_eq!(fresh.list().await.unwrap().len(), 2);
    assert!(matches!(
        fresh.fetch("../escape").await,
        Err(TimingsError::InvalidId(_))
    ));
    assert!(matches!(
        fresh.fetch("timings-missing").await,
        Err(TimingsError::NotFound(_))
    ));
    let folded = fresh.export(&summary.id, Export::Folded).await.unwrap();
    assert!(
        String::from_utf8(folded.to_vec())
            .unwrap()
            .contains("test.outer;test.inner")
    );
    let scope = fresh.export(&summary.id, Export::Speedscope).await.unwrap();
    let scope: serde_json::Value = serde_json::from_slice(&scope).unwrap();
    assert_eq!(scope["exporter"], "tuff");
    assert_eq!(
        Timings::detached().status().await,
        Err(TimingsError::Unavailable)
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_panic_restarts_the_controller_keeping_its_recording() {
    let _serial = serial_async().await;
    let timings = Timings::spawn_with(None, Vec::new(), Labels::default(), |runner| {
        tokio::spawn(tuff_supervise::restarting(
            "the timings recorder",
            runner,
            |runner| Box::pin(runner.run()),
        ));
    });
    timings.start(StartOptions::default()).await.unwrap();
    timings.panic_the_controller().await;
    assert!(timings.status().await.unwrap().recording.is_some());
    timings.stop().await.unwrap();
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_timed_recording_stops_and_saves_itself() {
    let _serial = serial_async().await;
    let timings = Timings::spawn(None, Vec::new(), Labels::default());
    timings
        .start(StartOptions {
            duration: Some(Duration::from_millis(50)),
            by: String::new(),
        })
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = timings.status().await.unwrap();
        if status.recording.is_none() {
            let last = status.last.expect("a report");
            assert!(last.file.is_err());
            assert!(timings.fetch(&last.id).await.is_ok());
            break;
        }
        assert!(Instant::now() < deadline, "the recording never stopped");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!is_recording());
}

async fn session_like() {
    let _event = timer!("test.event");
    tokio::task::yield_now().await;
    for feature in ["first", "second"] {
        let _feature = timer!(FEATURE, Tag::Name(feature));
        tokio::task::yield_now().await;
        let _encode = span!("test.encode");
        spin(5_000);
    }
}

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timers_in_a_scoped_task_nest_under_its_open_timers() {
    let _serial = serial_async().await;
    start().unwrap();
    let tasks: Vec<_> = (0..8)
        .map(|_| tokio::spawn(scope(session_like())))
        .collect();
    for task in tasks {
        task.await.unwrap();
    }
    tokio::spawn(session_like()).await.unwrap();
    let profile = Profile::from_snapshot(stop().unwrap());

    let event = profile.find(&["test.event"]).unwrap();
    assert_eq!(event.stats.count, 9);
    let nested: u64 = event
        .children
        .iter()
        .filter(|n| n.name == FEATURE)
        .map(|n| n.stats.count)
        .sum();
    assert_eq!(nested, 16);
    let encode = profile
        .find(&["test.event", FEATURE, "test.encode"])
        .unwrap();
    assert!(encode.stats.min_ns >= 5_000);
    let flat: u64 = profile
        .merged
        .iter()
        .filter(|n| n.name == FEATURE)
        .map(|n| n.stats.count)
        .sum();
    assert_eq!(flat, 2, "the unscoped task's timers stay at the root");
    assert!(event.self_ns() < event.stats.total_ns);
    assert_eq!(profile.features().iter().map(|p| p.2).sum::<u64>(), 18);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn timers_finishing_out_of_order_or_in_another_task_record_flat() {
    let _serial = serial_async().await;
    start().unwrap();
    scope(async {
        let outer = timer!("test.outer_timer");
        let inner = timer!("test.inner_timer");
        drop(outer);
        drop(inner);
        let moved = timer!("test.moved");
        tokio::spawn(scope(async move { drop(moved) }))
            .await
            .unwrap();
    })
    .await;
    let profile = Profile::from_snapshot(stop().unwrap());
    for name in ["test.outer_timer", "test.inner_timer", "test.moved"] {
        assert_eq!(profile.find(&[name]).unwrap().stats.count, 1, "{name}");
    }
}
