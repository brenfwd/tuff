//! Quick release-mode timing of the crowd benchmark's scenarios:
//! `cargo test --release -p tuff-feature-builtin-vanilla-mobs --test crowd_timing -- --ignored --nocapture`,
//! narrowed with `CROWD_SCENARIO=ladder_pit|cow_pen` and `CROWD_MODE=on|off`.

#[path = "../benches/crowd_scenario.rs"]
mod crowd_scenario;

use crowd_scenario::Scenario;

#[tuff_core::watchdog]
#[tokio::test(flavor = "multi_thread")]
#[ignore = "timing run, not a test"]
async fn crowd_scenarios_tick_times() {
    let only = std::env::var("CROWD_SCENARIO").ok();
    let modes = match std::env::var("CROWD_MODE").as_deref() {
        Ok("on") => vec![true],
        Ok("off") => vec![false],
        _ => vec![false, true],
    };
    for scenario in [Scenario::LadderPit, Scenario::CowPen] {
        if only.as_deref().is_some_and(|name| name != scenario.name()) {
            continue;
        }
        for &crowd in &modes {
            let report = crowd_scenario::run(scenario, scenario.default_count(), crowd, 200).await;
            println!(
                "{} crowd={crowd}: mean {:.2} ms, p99 {:.2} ms, tps {:.1}, wall {:.1} s",
                scenario.name(),
                report.mean_ms,
                report.p99_ms,
                report.tps,
                report.wall_s
            );
        }
    }
}
