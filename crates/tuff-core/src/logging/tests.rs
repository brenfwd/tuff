use std::io::Read;

use super::*;

fn quiet(ring: usize, queue: usize) -> HubOptions {
    HubOptions {
        terminal: false,
        ring,
        queue,
        ..HubOptions::default()
    }
}

fn line(text: impl Into<String>) -> LogLine {
    LogLine::new(Level::Info, "test", text)
}

fn texts(lines: &[Arc<LogLine>]) -> Vec<String> {
    lines.iter().map(|line| line.text.clone()).collect()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tuff-logging-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn gunzip(path: &Path) -> String {
    let mut text = String::new();
    flate2::read::GzDecoder::new(File::open(path).unwrap())
        .take(1 << 20)
        .read_to_string(&mut text)
        .unwrap();
    text
}

#[tuff_core::watchdog]
#[tokio::test]
async fn lines_keep_their_order_and_get_consecutive_seqs() {
    let hub = LogHub::spawn(quiet(RING_LINES, QUEUE_LINES));
    for i in 0..100 {
        hub.offer(line(format!("line {i}"))).unwrap();
    }
    let sub = hub.subscribe().await.unwrap();
    let expected: Vec<String> = (0..100).map(|i| format!("line {i}")).collect();
    assert_eq!(texts(&sub.backfill), expected);
    let seqs: Vec<u64> = sub.backfill.iter().map(|line| line.seq).collect();
    assert_eq!(seqs, (0..100).collect::<Vec<_>>());
    assert_eq!(
        hub.shutdown().await,
        Some(HubStats {
            written: 100,
            dropped: 0
        })
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn backfill_is_the_ring_and_live_continues_after_it() {
    let hub = LogHub::spawn(quiet(5, QUEUE_LINES));
    for i in 0..8 {
        hub.offer(line(format!("old {i}"))).unwrap();
    }
    let mut sub = hub.subscribe().await.unwrap();
    assert_eq!(
        texts(&sub.backfill),
        ["old 3", "old 4", "old 5", "old 6", "old 7"]
    );
    hub.offer(line("new")).unwrap();
    let next = sub.live.recv().await.unwrap();
    assert_eq!((next.seq, next.text.as_str()), (8, "new"));
    hub.shutdown().await;
    assert!(sub.live.recv().await.is_err());
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_full_queue_drops_and_reports_the_count() {
    let (hub, actor) = LogHub::new(quiet(RING_LINES, 2));
    for i in 0..5 {
        hub.offer(line(format!("line {i}"))).unwrap();
    }
    std::thread::spawn(move || actor.run_blocking());
    let stats = hub.flush().await.unwrap();
    assert_eq!(stats.dropped, 3);
    let sub = hub.subscribe().await.unwrap();
    let backfill = texts(&sub.backfill);
    assert_eq!(&backfill[..2], ["line 0", "line 1"]);
    assert_eq!(backfill[2], "3 log line(s) dropped: the log queue was full");
    assert_eq!(sub.backfill[2].level, Level::Warn);
    hub.shutdown().await;
}

#[tuff_core::watchdog]
#[tokio::test]
async fn records_fall_back_to_stderr_without_a_running_hub() {
    assert_eq!(route(None, line("no hub")), Route::Stderr);
    let hub = LogHub::spawn(quiet(RING_LINES, QUEUE_LINES));
    assert_eq!(route(Some(&hub), line("to hub")), Route::Hub);
    hub.shutdown().await;
    assert_eq!(route(Some(&hub), line("stopped")), Route::Stderr);
    assert!(hub.subscribe().await.is_none());
}

#[tuff_core::watchdog]
#[test]
fn dependency_chatter_needs_warn() {
    assert!(enabled(Level::Info, "tuff_core::world::coordinator"));
    assert!(enabled(
        Level::Info,
        "tuff_feature_builtin_vanilla_maps::service"
    ));
    assert!(!enabled(Level::Debug, "tuff_core::server"));
    assert!(!enabled(Level::Info, "reqwest::connect"));
    assert!(enabled(Level::Warn, "reqwest::connect"));
    assert!(!enabled(Level::Info, "rustls"));
}

#[tuff_core::watchdog]
#[test]
fn file_lines_carry_time_target_and_level() {
    let line = LogLine {
        seq: 0,
        unix_millis: (3 * 86_400 + 13 * 3600 + 4 * 60 + 5) * 1000 + 999,
        level: Level::Warn,
        target: "tuff_core::server".into(),
        text: "hello".into(),
    };
    assert_eq!(
        line.file_line(),
        "[13:04:05] [tuff_core::server/WARN]: hello"
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn latest_log_is_archived_on_start_and_on_a_new_day() {
    let dir = scratch("rotation");
    let latest = dir.join(LATEST);
    fs::write(&latest, "previous run\n").unwrap();
    let previous_day = 20_455u64;
    File::options()
        .write(true)
        .open(&latest)
        .unwrap()
        .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(previous_day * 86_400 + 60))
        .unwrap();
    fs::write(dir.join("2026-01-02-1.log.gz"), "taken").unwrap();

    let hub = LogHub::spawn(HubOptions {
        terminal: false,
        ..HubOptions::new(&dir)
    });
    let base = now_millis().div_euclid(DAY_MILLIS) + 10;
    let at = |day: i64, text: &str| LogLine {
        unix_millis: day * DAY_MILLIS + 1000,
        ..line(text)
    };
    hub.offer(at(base, "first")).unwrap();
    hub.offer(at(base - 1, "late")).unwrap();
    hub.offer(at(base + 1, "second")).unwrap();
    hub.offer(at(base + 2, "third")).unwrap();
    hub.shutdown().await;

    assert_eq!(civil_from_days(previous_day as i64), (2026, 1, 2));
    assert_eq!(gunzip(&dir.join("2026-01-02-2.log.gz")), "previous run\n");
    let name = |day: i64| {
        let (y, m, d) = civil_from_days(day);
        dir.join(format!("{y:04}-{m:02}-{d:02}-1.log.gz"))
    };
    let first = gunzip(&name(base));
    assert_eq!(first.lines().count(), 2, "{first}");
    assert!(first.ends_with("]: late\n"), "{first}");
    assert!(gunzip(&name(base + 1)).ends_with("]: second\n"));
    let current = fs::read_to_string(&latest).unwrap();
    assert_eq!(current.lines().count(), 1, "{current}");
    assert!(current.ends_with("[test/INFO]: third\n"), "{current}");
    assert_eq!(
        fs::read_dir(&dir).unwrap().count(),
        5,
        "the empty file of the start day is not archived"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[tuff_core::watchdog]
#[test]
fn a_panic_is_described_with_thread_message_and_location() {
    let text: &(dyn std::any::Any + Send) = &"boom";
    assert_eq!(
        describe_panic("worker-3", Some(("a.rs", 7)), text),
        "Uncaught panic in thread worker-3: boom at a.rs:7"
    );
    let owned: &(dyn std::any::Any + Send) = &String::from("bang");
    assert_eq!(
        describe_panic("main", None, owned),
        "Uncaught panic in thread main: bang"
    );
}
