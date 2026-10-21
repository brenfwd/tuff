//! Crowd benchmark: tick time with many mobs packed together while a
//! player walks up holding wheat, with crowd mode off and on.
//!
//! ```text
//! cargo bench -p tuff-feature-builtin-vanilla-mobs --bench crowd
//! cargo bench -p tuff-feature-builtin-vanilla-mobs --bench crowd -- --scenario cow_pen --count 2000 --ticks 400 --crowd on
//! ```
//!
//! Flags: `--scenario ladder_pit|cow_pen|both` (both), `--count <n>` (800
//! pigs, 5000 cows), `--ticks <n>` (300), `--crowd off|on|both` (both).
//! Prints one JSON object per run with `tick_mean_ms`, `tick_p99_ms` and
//! `tps`. The server runs in process on the test world (no network, no
//! vanilla comparison).

#[path = "crowd_scenario.rs"]
mod crowd_scenario;

use crowd_scenario::Scenario;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if !args.iter().any(|arg| arg == "--bench") {
        println!("[crowd] runs only via `cargo bench --bench crowd`");
        return;
    }
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let scenarios = match flag("--scenario").as_deref() {
        Some("ladder_pit") => vec![Scenario::LadderPit],
        Some("cow_pen") => vec![Scenario::CowPen],
        _ => vec![Scenario::LadderPit, Scenario::CowPen],
    };
    let crowd = match flag("--crowd").as_deref() {
        Some("off") => vec![false],
        Some("on") => vec![true],
        _ => vec![false, true],
    };
    let ticks = flag("--ticks").and_then(|t| t.parse().ok()).unwrap_or(300);
    let count = flag("--count").and_then(|c| c.parse().ok());
    let runtime = tuff_core::threads::name_tokio(&mut tokio::runtime::Builder::new_multi_thread())
        .enable_all()
        .build()
        .expect("a runtime");
    for scenario in scenarios {
        for &on in &crowd {
            let count = count.unwrap_or(scenario.default_count());
            let report = runtime.block_on(crowd_scenario::run(scenario, count, on, ticks));
            println!(
                "{}",
                serde_json::json!({
                    "scenario": scenario.name(),
                    "count": count,
                    "crowd": on,
                    "ticks": ticks,
                    "tick_mean_ms": report.mean_ms,
                    "tick_p99_ms": report.p99_ms,
                    "tps": report.tps,
                    "wall_s": report.wall_s,
                })
            );
        }
    }
}
