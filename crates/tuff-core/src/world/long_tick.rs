//! The long tick recorder: with `TUFF_DEBUG_MONITOR_TICK_TIMES` set, a region
//! tick that runs 300 ms or longer writes its phase timings to
//! `debug/tick-results-<time>-<region>.txt` and logs where.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const ENV_FLAG: &str = "TUFF_DEBUG_MONITOR_TICK_TIMES";
pub const THRESHOLD: Duration = Duration::from_millis(300);
const DIRECTORY: &str = "debug";

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os(ENV_FLAG).is_some_and(|v| !v.is_empty() && v != "0" && v != "false")
    })
}

/// Phase timings of one tick, collected only while the monitor is enabled.
pub struct TickProfile {
    start: Instant,
    last: Instant,
    phases: Vec<(&'static str, Duration)>,
}

impl TickProfile {
    pub fn start() -> Option<Self> {
        enabled().then(Self::begin)
    }

    fn begin() -> Self {
        let now = Instant::now();
        Self {
            start: now,
            last: now,
            phases: Vec::new(),
        }
    }

    pub fn mark(profile: &mut Option<Self>, phase: &'static str) {
        if let Some(profile) = profile {
            let now = Instant::now();
            profile.phases.push((phase, now - profile.last));
            profile.last = now;
        }
    }

    pub fn finish(self, label: &str, tick: u64) {
        let total = self.start.elapsed();
        if let Some(file) = self.write_if_long(Path::new(DIRECTORY), THRESHOLD, label, tick, total)
        {
            log::info!("Recorded long tick -- wrote info to: {}", file.display());
        }
    }

    fn write_if_long(
        &self,
        dir: &Path,
        threshold: Duration,
        label: &str,
        tick: u64,
        total: Duration,
    ) -> Option<PathBuf> {
        if total < threshold {
            return None;
        }
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let safe: String = label
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let file = dir.join(format!(
            "tick-results-{}-{safe}.txt",
            tuff_timings::file_stamp(seconds)
        ));
        std::fs::create_dir_all(dir).ok()?;
        std::fs::write(&file, self.render(label, tick, total)).ok()?;
        Some(file)
    }

    fn render(&self, label: &str, tick: u64, total: Duration) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "---- Tick profiler results ----");
        let _ = writeln!(out, "Region: {label}");
        let _ = writeln!(out, "Tick: {tick}");
        let _ = writeln!(out, "Duration: {:.3} ms", total.as_secs_f64() * 1000.0);
        let _ = writeln!(out);
        for (phase, took) in &self.phases {
            let percent = took.as_secs_f64() / total.as_secs_f64().max(f64::EPSILON) * 100.0;
            let _ = writeln!(
                out,
                "[00] {phase} {percent:.2}% {:.3} ms",
                took.as_secs_f64() * 1000.0
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_tick_over_the_threshold_is_written_and_a_short_one_is_not() {
        let dir = tuff_storage::test_dir::TestDir::new();
        let mut profile = Some(TickProfile::begin());
        std::thread::sleep(Duration::from_millis(2));
        TickProfile::mark(&mut profile, "simulation");
        let profile = profile.unwrap();
        let long = Duration::from_millis(350);
        assert!(
            profile
                .write_if_long(
                    dir.path(),
                    THRESHOLD,
                    "minecraft:overworld 0,0",
                    7,
                    Duration::from_millis(10)
                )
                .is_none()
        );
        let file = profile
            .write_if_long(dir.path(), THRESHOLD, "minecraft:overworld 0,0", 7, long)
            .unwrap();
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("tick-results-") && name.ends_with("-minecraft_overworld_0_0.txt")
        );
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("Tick: 7") && text.contains("Duration: 350.000 ms"));
        assert!(text.contains("simulation"));
    }
}
