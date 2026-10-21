use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::history::History;
use super::*;
use crate::world::tick_rate::TickState;
use tuff_trace::ConnState;

fn region(x: i32, mean: f64, tick: u64) -> Report {
    Report::Region(Box::new(RegionReport {
        dimension: "minecraft:overworld".into(),
        x,
        z: 0,
        tick,
        ticks: 20,
        mspt_mean: mean,
        mspt_max: mean * 2.0,
        chunks: 10,
        entities: 3,
        entities_by_category: {
            let mut counts = vec![0; tuff_vanilla_data::entity::MobCategory::COUNT];
            counts[usize::from(tuff_vanilla_data::entity::MobCategory::Creature.id())] = 3;
            counts
        },
        players: 1,
        ..RegionReport::default()
    }))
}

fn sample(actor: &mut actor::Actor) -> Arc<Snapshot> {
    actor.sample(Instant::now(), SystemTime::now())
}

#[tuff_core::watchdog]
#[test]
fn history_keeps_ten_minutes_of_samples_and_rolls_minutes_up() {
    let mut history = History::default();
    let start = 1_000 * 60_000;
    for second in 0..700u64 {
        history.push("x", start + second * 1000, second as f64);
    }
    let now = start + 699 * 1000;
    let recent = history.range("x", now, Duration::from_secs(10)).unwrap();
    assert_eq!(recent.len(), 11);
    assert_eq!(recent.last().unwrap().value, 699.0);
    let all = history
        .range("x", now, Duration::from_secs(HISTORY_SECONDS as u64))
        .unwrap();
    assert_eq!(all.len(), HISTORY_SECONDS);
    assert_eq!(all[0].value, 100.0);

    let minutes = history.range("x", now, Duration::from_secs(3600)).unwrap();
    assert_eq!(minutes.len(), 12, "11 whole minutes and the open one");
    assert_eq!(minutes[0].unix_ms, start);
    assert_eq!(minutes[0].value, 29.5);
    assert_eq!(minutes[0].max, 59.0);
    assert_eq!(minutes[11].value, (660.0 + 699.0) / 2.0);
    assert!(history.range("y", now, Duration::from_secs(10)).is_none());
}

#[tuff_core::watchdog]
#[test]
fn minute_rollups_keep_one_day() {
    let mut history = History::default();
    for minute in 0..(HISTORY_MINUTES as u64 + 10) {
        history.push("x", minute * 60_000, minute as f64);
    }
    let now = (HISTORY_MINUTES as u64 + 9) * 60_000;
    let day = history
        .range("x", now, Duration::from_secs(48 * 3600))
        .unwrap();
    assert_eq!(day.len(), HISTORY_MINUTES + 1);
    assert_eq!(day[0].value, 9.0);
}

#[tuff_core::watchdog]
#[test]
fn a_window_reports_once_the_pace_allows_with_percentiles() {
    let pace = Pace {
        every: Duration::ZERO,
        ticks: 20,
    };
    let mut window = Window::new(true);
    let now = Instant::now();
    for ms in 1..20u64 {
        assert!(
            window
                .record(pace, Duration::from_millis(ms), now)
                .is_none()
        );
    }
    let stats = window
        .record(pace, Duration::from_millis(20), now)
        .expect("the 20th tick reports");
    assert_eq!(stats.ticks, 20);
    assert_eq!(stats.mean, Duration::from_micros(10_500));
    assert_eq!(stats.max, Duration::from_millis(20));
    assert_eq!(
        (stats.p50, stats.p95, stats.p99),
        (
            Duration::from_millis(11),
            Duration::from_millis(20),
            Duration::from_millis(20)
        )
    );
    assert!(window.record(pace, Duration::from_millis(1), now).is_none());

    let timed = Pace {
        every: Duration::from_secs(3600),
        ticks: 1,
    };
    assert!(
        Window::new(false)
            .record(timed, Duration::ZERO, now)
            .is_none()
    );
}

#[tuff_core::watchdog]
#[test]
fn tick_reports_count_rate_and_lag_behind_the_target() {
    let stats = WindowStats {
        elapsed: Duration::from_secs(2),
        ticks: 20,
        mean: Duration::from_millis(80),
        ..WindowStats::default()
    };
    let report = tick_report(500, stats, TickState::default(), false, Vec::new());
    assert_eq!(report.tps, 10.0);
    assert_eq!(report.behind, 20.0);
    assert_eq!(report.mspt_mean, 80.0);
    assert_eq!(report.target_rate, 20.0);
    let sprinting = tick_report(500, stats, TickState::default(), true, Vec::new());
    assert_eq!(sprinting.behind, 0.0);
    let on_time = WindowStats {
        elapsed: Duration::from_millis(1020),
        ..stats
    };
    assert_eq!(
        tick_report(1, on_time, TickState::default(), false, Vec::new()).behind,
        0.0,
        "under a tick of slack is not lag"
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_full_queue_drops_reports_and_counts_them() {
    let (metrics, mut actor) = Metrics::build(MetricsConfig::default());
    for _ in 0..REPORT_QUEUE {
        assert!(metrics.report(Report::Manager(ManagerReport::default())));
    }
    assert!(!metrics.report(Report::Manager(ManagerReport::default())));
    assert!(!metrics.report(Report::Manager(ManagerReport::default())));
    assert_eq!(metrics.dropped(), 2);
    assert_eq!(sample(&mut actor).dropped_reports, 2);
    assert_eq!(
        sample(&mut actor).series["metrics.dropped"].kind,
        Kind::Counter
    );
}

#[tuff_core::watchdog]
#[test]
fn the_slowest_regions_come_first_and_stale_ones_age_out() {
    let (_metrics, mut actor) = Metrics::build(MetricsConfig::default());
    for x in 0..15 {
        actor.apply(region(x, f64::from(x % 5), 100));
    }
    let snapshot = sample(&mut actor);
    assert_eq!(snapshot.world.regions, 15);
    assert_eq!(snapshot.world.chunks, 150);
    assert_eq!(snapshot.world.entities, 45);
    assert_eq!(snapshot.world.players, 15);
    let creature = tuff_vanilla_data::entity::MobCategory::Creature.name();
    assert_eq!(snapshot.world.entities_by_category[creature], 45);
    let overworld = &snapshot.dimensions["minecraft:overworld"];
    assert_eq!((overworld.regions, overworld.chunks), (15, 150));
    assert_eq!(snapshot.dimensions.len(), 1);
    let slowest: Vec<(f64, i32)> = snapshot
        .slowest_regions
        .iter()
        .map(|r| (r.mspt_mean, r.x))
        .collect();
    assert_eq!(
        slowest,
        vec![
            (4.0, 4),
            (4.0, 9),
            (4.0, 14),
            (3.0, 3),
            (3.0, 8),
            (3.0, 13),
            (2.0, 2),
            (2.0, 7),
            (2.0, 12),
            (1.0, 1),
        ]
    );

    actor.apply(region(0, 0.5, 100 + STALE_REGION_TICKS));
    let snapshot = sample(&mut actor);
    assert_eq!(snapshot.world.regions, 15, "200 ticks old is still live");
    actor.apply(region(0, 0.5, 101 + STALE_REGION_TICKS));
    let snapshot = sample(&mut actor);
    assert_eq!(snapshot.world.regions, 1, "the others stopped reporting");
}

#[tuff_core::watchdog]
#[tokio::test]
async fn a_panic_restarts_the_actor_keeping_series_and_history() {
    let metrics = Metrics::spawn(MetricsConfig::default());
    let queue = metrics.series("plugin.queue", Kind::Gauge);
    queue.set(3);
    assert_eq!(metrics.collect().await.series["plugin.queue"].value, 3);
    metrics.0.queries.send(Query::Panic).await.unwrap();
    queue.set(4);
    assert_eq!(metrics.collect().await.series["plugin.queue"].value, 4);
}

#[tuff_core::watchdog]
#[tokio::test]
async fn series_are_summed_by_name_and_counters_report_rates() {
    let metrics = Metrics::spawn(MetricsConfig::default());
    let first = metrics.series("plugin.things", Kind::Counter);
    let second = metrics.series("plugin.things", Kind::Counter);
    let queue = metrics.series("plugin.queue", Kind::Gauge);
    let clash = metrics.series("plugin.queue", Kind::Counter);
    metrics.collect().await;
    first.add(5);
    second.add(7);
    queue.set(3);
    clash.add(100);
    let snapshot = metrics.collect().await;
    let things = &snapshot.series["plugin.things"];
    assert_eq!((things.kind, things.value), (Kind::Counter, 12));
    assert!(things.rate > 0.0);
    let queue_value = &snapshot.series["plugin.queue"];
    assert_eq!((queue_value.kind, queue_value.value), (Kind::Gauge, 3));
    assert_eq!(metrics.snapshot().series["plugin.queue"].value, 3);

    let history = metrics
        .history("plugin.queue", Duration::from_secs(60))
        .await
        .unwrap();
    assert!(history.len() >= 2);
    assert_eq!(history.last().unwrap().value, 3.0);
    assert!(
        metrics
            .history("plugin.unknown", Duration::from_secs(60))
            .await
            .is_none()
    );
}

#[tuff_core::watchdog]
#[tokio::test]
async fn reports_land_before_a_collect_and_the_tick_goes_stale() {
    let metrics = Metrics::spawn(MetricsConfig::default());
    metrics.report(Report::Tick(TickReport {
        tick: 40,
        tps: 20.0,
        mspt_mean: 3.0,
        players: vec![PlayerPing {
            name: "Alex".into(),
            uuid: "0".into(),
            ping_ms: 42,
        }],
        ..TickReport::default()
    }));
    metrics.report(Report::ChunkIo(ChunkIoReport {
        queued: 4,
        ..ChunkIoReport::default()
    }));
    let snapshot = metrics.collect().await;
    let tick = snapshot.tick.as_ref().unwrap();
    assert_eq!((tick.tick, tick.players[0].ping_ms), (40, 42));
    assert!(!snapshot.tick_stale);
    assert_eq!(snapshot.chunk_io.as_ref().unwrap().queued, 4);
    assert!(snapshot.runtime.as_ref().unwrap().workers >= 1);
    let json = snapshot.to_json();
    assert_eq!(json["tick"]["players"][0]["name"], "Alex");
    let tps = metrics
        .history(TICK_TPS, Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(tps.last().unwrap().value, 20.0);
}

#[cfg(unix)]
#[tuff_core::watchdog]
#[tokio::test]
async fn process_cpu_is_sampled_into_the_snapshot_and_history() {
    let metrics = Metrics::spawn(MetricsConfig::default());
    tokio::time::sleep(MIN_CPU_WINDOW + Duration::from_millis(20)).await;
    let snapshot = metrics.collect().await;
    let cpu = snapshot.cpu.expect("a cpu figure after one window");
    assert!(cpu.cores >= 0.0 && cpu.cores.is_finite(), "{cpu:?}");
    assert!(cpu.host_cores.is_some_and(|n| n >= 1));
    assert!(snapshot.to_json()["cpu"]["cores"].is_number());
    let again = metrics.collect().await.cpu.expect("the figure is kept");
    let history = metrics
        .history(PROCESS_CPU, Duration::from_secs(60))
        .await
        .expect("process.cpu history");
    assert_eq!(history.last().unwrap().value, again.cores);
}

#[tuff_core::watchdog]
#[test]
fn a_detached_handle_is_inert() {
    let metrics = Metrics::detached();
    assert!(!metrics.report(Report::Manager(ManagerReport::default())));
    assert_eq!(metrics.dropped(), 0);
    metrics.series("x", Kind::Counter).inc();
    assert!(metrics.snapshot().series.is_empty());
}

#[tuff_core::watchdog]
#[test]
fn proc_status_gives_resident_and_peak_memory() {
    let text = "Name:\ttuff\nVmPeak:\t  900 kB\nVmHWM:\t  2048 kB\nVmRSS:\t  1024 kB\n";
    assert_eq!(
        parse_proc_status(text),
        Some(MemoryStats {
            rss_bytes: 1024 * 1024,
            peak_rss_bytes: Some(2048 * 1024),
        })
    );
    assert_eq!(parse_proc_status("Name:\ttuff\n"), None);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert!(MemoryStats::sample().is_some_and(|m| m.rss_bytes > 0));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    assert!(MemoryStats::sample().is_none());
}

#[tuff_core::watchdog]
#[test]
fn connection_meters_flush_on_drop_and_gauges_follow_the_state() {
    let (metrics, _actor) = Metrics::build(MetricsConfig::default());
    {
        let mut meter = NetMeter::inbound(&metrics);
        meter.bytes(100);
        meter.packets(3);
        let mut gauge = StateGauge::new(&metrics, ConnState::Handshake);
        assert_eq!(metrics.core().connection(ConnState::Handshake).get(), 1);
        gauge.set(ConnState::Login);
        gauge.set(ConnState::Login);
        assert_eq!(metrics.core().connection(ConnState::Handshake).get(), 0);
        assert_eq!(metrics.core().connection(ConnState::Login).get(), 1);
    }
    assert_eq!(metrics.core().bytes_in.get(), 100);
    assert_eq!(metrics.core().packets_in.get(), 3);
    assert_eq!(metrics.core().connection(ConnState::Login).get(), 0);
}

#[tuff_core::watchdog]
#[test]
fn timestamps_are_wall_clock_millis() {
    let (_metrics, mut actor) = Metrics::build(MetricsConfig::default());
    let wall = UNIX_EPOCH + Duration::from_millis(1_234_567);
    let snapshot = actor.sample(Instant::now(), wall);
    assert_eq!(snapshot.unix_ms, 1_234_567);
}
