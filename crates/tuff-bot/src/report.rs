//! Measurements a load benchmark reports: tick time percentiles, totals
//! summed over a bot fleet, process CPU and memory, and a side-by-side
//! comparison of two saved reports.

use std::collections::BTreeMap;
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{BotState, Sent};

/// One tick's budget at the default rate.
pub const TICK_BUDGET_MS: f64 = 50.0;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TickSummary {
    pub count: u64,
    pub tps: f64,
    pub mspt_mean: f64,
    pub p50: f64,
    pub p90: f64,
    pub p95: f64,
    pub p99: f64,
    pub p999: f64,
    pub max: f64,
    /// Ticks that took longer than [`TICK_BUDGET_MS`].
    pub over_budget: u64,
    /// Samples the collector missed by falling behind.
    pub lagged: u64,
}

impl TickSummary {
    /// Summarises tick durations in milliseconds seen over `window_s`.
    pub fn new(durations_ms: &[f64], window_s: f64, lagged: u64) -> Self {
        let mut sorted = durations_ms.to_vec();
        sorted.sort_by(f64::total_cmp);
        let count = sorted.len();
        let mean = if count == 0 {
            0.0
        } else {
            sorted.iter().sum::<f64>() / count as f64
        };
        Self {
            count: count as u64,
            tps: if window_s > 0.0 {
                count as f64 / window_s
            } else {
                0.0
            },
            mspt_mean: mean,
            p50: percentile(&sorted, 0.50),
            p90: percentile(&sorted, 0.90),
            p95: percentile(&sorted, 0.95),
            p99: percentile(&sorted, 0.99),
            p999: percentile(&sorted, 0.999),
            max: sorted.last().copied().unwrap_or(0.0),
            over_budget: sorted.iter().filter(|ms| **ms > TICK_BUDGET_MS).count() as u64,
            lagged,
        }
    }
}

/// Nearest-rank percentile of an ascending slice; 0 when empty.
pub fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (fraction * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// Counters summed over every bot, as of one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct BotTotals {
    pub players: u64,
    pub connected: u64,
    pub wire_bytes: u64,
    pub payload_bytes: u64,
    pub chunk_bytes: u64,
    pub chunks: u64,
    pub packets: u64,
    pub teleports: u64,
    /// Moves with a position the bots sent while pursuing goals.
    #[serde(default)]
    pub moves: u64,
    /// Teleports the server sent bots while they pursued goals, mostly
    /// moves it refused.
    #[serde(default)]
    pub setbacks: u64,
    pub deaths: u64,
    pub windows_opened: u64,
    pub digs_started: u64,
    pub digs_finished: u64,
    pub uses_on_block: u64,
    pub attacks: u64,
    pub swings: u64,
}

impl BotTotals {
    pub fn add(&mut self, state: &BotState) {
        let Sent {
            digs_started,
            digs_finished,
            uses_on_block,
            attacks,
            swings,
            ..
        } = state.sent;
        self.players += 1;
        self.connected += u64::from(state.disconnected.is_none());
        self.wire_bytes += state.bytes;
        self.payload_bytes += state.payload_bytes;
        self.chunk_bytes += state.chunk_bytes;
        self.chunks += state.chunks;
        self.packets += state.packets;
        self.teleports += state.teleports;
        self.moves += state.moves;
        self.setbacks += state.setbacks;
        self.deaths += state.deaths;
        self.windows_opened += state.windows_opened;
        self.digs_started += digs_started;
        self.digs_finished += digs_finished;
        self.uses_on_block += uses_on_block;
        self.attacks += attacks;
        self.swings += swings;
    }

    /// What happened since `earlier`; the player counts stay as now.
    pub fn since(&self, earlier: &Self) -> Self {
        let d = |now: u64, then: u64| now.saturating_sub(then);
        Self {
            players: self.players,
            connected: self.connected,
            wire_bytes: d(self.wire_bytes, earlier.wire_bytes),
            payload_bytes: d(self.payload_bytes, earlier.payload_bytes),
            chunk_bytes: d(self.chunk_bytes, earlier.chunk_bytes),
            chunks: d(self.chunks, earlier.chunks),
            packets: d(self.packets, earlier.packets),
            teleports: d(self.teleports, earlier.teleports),
            moves: d(self.moves, earlier.moves),
            setbacks: d(self.setbacks, earlier.setbacks),
            deaths: d(self.deaths, earlier.deaths),
            windows_opened: d(self.windows_opened, earlier.windows_opened),
            digs_started: d(self.digs_started, earlier.digs_started),
            digs_finished: d(self.digs_finished, earlier.digs_finished),
            uses_on_block: d(self.uses_on_block, earlier.uses_on_block),
            attacks: d(self.attacks, earlier.attacks),
            swings: d(self.swings, earlier.swings),
        }
    }
}

/// The share of the bots' moves a server may set back before the run no
/// longer counts as the same workload.
pub const SETBACK_LIMIT: f64 = 0.01;

/// Setbacks over moves, and why the run is invalid when they pass
/// [`SETBACK_LIMIT`].
pub fn setback_check(setbacks: u64, moves: u64) -> (f64, Option<String>) {
    let ratio = setbacks as f64 / moves.max(1) as f64;
    let invalid = (ratio > SETBACK_LIMIT).then(|| {
        format!(
            "{setbacks} setbacks in {moves} moves ({:.2}%, over the {:.0}% limit)",
            ratio * 100.0,
            SETBACK_LIMIT * 100.0
        )
    });
    (ratio, invalid)
}

/// One line on whether a report's figures can be trusted: `FAILED` with
/// the cause when its server died, `INVALID` when it set back too many
/// moves, else `valid`.
pub fn verdict(report: &Value) -> String {
    let failed = &report["failed"];
    if !failed.is_null() {
        let cause = failed["cause"]
            .as_str()
            .or(failed["error"].as_str())
            .unwrap_or("unknown cause");
        return format!("FAILED: {cause}");
    }
    let count = |key: &str| report["bots"][key].as_u64();
    match (count("setbacks"), count("moves")) {
        (Some(setbacks), Some(moves)) => match setback_check(setbacks, moves) {
            (_, Some(reason)) => format!("INVALID: {reason}"),
            (ratio, None) => format!(
                "valid: {setbacks} setbacks in {moves} moves ({:.2}%)",
                ratio * 100.0
            ),
        },
        _ => "valid (no setback counts)".into(),
    }
}

/// Below this fraction of the other server's bot moves, two runs no
/// longer count as the same workload.
pub const MOVE_SHORTFALL: f64 = 0.9;

/// Why two reports of the same bench are not comparable: one server's
/// bots made fewer than [`MOVE_SHORTFALL`] of the other's moves.
pub fn workload_check(left: &Value, right: &Value, labels: [&str; 2]) -> Option<String> {
    let moves = |report: &Value| report["bots"]["moves"].as_u64();
    let (l, r) = (moves(left)?, moves(right)?);
    let (low, high, short, full) = if l < r {
        (l, r, labels[0], labels[1])
    } else {
        (r, l, labels[1], labels[0])
    };
    let ratio = low as f64 / high.max(1) as f64;
    (ratio < MOVE_SHORTFALL).then(|| {
        format!(
            "{short} made {low} moves, {ratio:.2}x {full}'s {high} (under {MOVE_SHORTFALL:.2}x)"
        )
    })
}

/// The report with a `per_move` section: server CPU, wire bytes and
/// packets divided by bot moves, so runs with different move counts
/// compare by cost per unit of work.
pub fn with_per_move(report: &Value) -> Value {
    let mut out = report.clone();
    let Some(moves) = report["bots"]["moves"].as_u64().filter(|m| *m > 0) else {
        return out;
    };
    let per = |value: &Value| value.as_f64().map(|v| v / moves as f64);
    if let Some(object) = out.as_object_mut() {
        object.insert(
            "per_move".into(),
            serde_json::json!({
                "server_cpu_ms": per(&report["cpu"]["server_s"]).map(|s| s * 1000.0),
                "wire_bytes": per(&report["network"]["wire_bytes"]),
                "packets": per(&report["network"]["packets"]),
            }),
        );
    }
    out
}

/// CPU time used and resident memory of one process.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessSample {
    pub cpu_s: f64,
    pub rss_bytes: u64,
}

/// Linux's fixed `USER_HZ`, the unit of `/proc/<pid>/stat` CPU times.
const CLOCK_TICKS_PER_SECOND: f64 = 100.0;

/// Samples process `pid`: from `/proc` where there is one, else `ps`.
pub fn sample_process(pid: u32) -> Option<ProcessSample> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"));
    if let (Ok(stat), Ok(status)) = (stat, status) {
        return Some(ProcessSample {
            cpu_s: parse_proc_stat(&stat)?,
            rss_bytes: parse_proc_rss(&status)?,
        });
    }
    let output = Command::new("ps")
        .args(["-o", "rss=,time=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    parse_ps(std::str::from_utf8(&output.stdout).ok()?)
}

/// User plus system CPU seconds from a `/proc/<pid>/stat` line.
pub fn parse_proc_stat(text: &str) -> Option<f64> {
    let rest = &text[text.rfind(')')? + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let user: f64 = fields.get(11)?.parse().ok()?;
    let system: f64 = fields.get(12)?.parse().ok()?;
    Some((user + system) / CLOCK_TICKS_PER_SECOND)
}

/// `VmRSS` from `/proc/<pid>/status`, in bytes.
pub fn parse_proc_rss(text: &str) -> Option<u64> {
    let line = text.lines().find(|line| line.starts_with("VmRSS:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// `ps -o rss=,time=` output: KiB, then `[[dd-]hh:]mm:ss[.cc]`.
pub fn parse_ps(text: &str) -> Option<ProcessSample> {
    let mut fields = text.split_whitespace();
    let kib: u64 = fields.next()?.parse().ok()?;
    let time = fields.next()?;
    let (days, clock) = match time.split_once('-') {
        Some((days, clock)) => (days.parse::<f64>().ok()?, clock),
        None => (0.0, time),
    };
    let mut seconds = 0.0;
    for part in clock.split(':') {
        seconds = seconds * 60.0 + part.parse::<f64>().ok()?;
    }
    Some(ProcessSample {
        cpu_s: days * 86_400.0 + seconds,
        rss_bytes: kib * 1024,
    })
}

/// Every numeric leaf of a JSON document, keyed by its dotted path.
pub fn flatten(value: &Value) -> BTreeMap<String, f64> {
    flatten_with_gaps(value)
        .into_iter()
        .filter_map(|(key, value)| Some((key, value?)))
        .collect()
}

/// Every numeric or `null` leaf, keyed by its dotted path; `null` marks a
/// figure a report could not measure.
pub fn flatten_with_gaps(value: &Value) -> BTreeMap<String, Option<f64>> {
    fn walk(value: &Value, path: &str, out: &mut BTreeMap<String, Option<f64>>) {
        match value {
            Value::Number(n) => {
                if let Some(v) = n.as_f64() {
                    out.insert(path.to_string(), Some(v));
                }
            }
            Value::Null => {
                out.insert(path.to_string(), None);
            }
            Value::Object(map) => {
                for (key, child) in map {
                    let path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    walk(child, &path, out);
                }
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    walk(value, "", &mut out);
    out
}

/// A plain-text table of every numeric field two reports share, with
/// the relative change from `before` to `after`.
pub fn compare(before: &Value, after: &Value) -> String {
    let (a, b) = (flatten(before), flatten(after));
    let width = a.keys().map(String::len).max().unwrap_or(5).max(5);
    let mut out = format!(
        "{:width$}  {:>14}  {:>14}  {:>9}\n",
        "field", "before", "after", "change"
    );
    for (key, old) in &a {
        let Some(new) = b.get(key) else {
            continue;
        };
        let change = if *old == 0.0 {
            if *new == 0.0 {
                "=".to_string()
            } else {
                "new".to_string()
            }
        } else {
            format!("{:+.1}%", (new - old) / old.abs() * 100.0)
        };
        out.push_str(&format!(
            "{key:width$}  {:>14}  {:>14}  {change:>9}\n",
            number(*old),
            number(*new)
        ));
    }
    out
}

/// Kinds shown in each of [`side_by_side`]'s packet tables.
pub const TOP_PACKET_KINDS: usize = 15;

const PACKET_KINDS: &str = "packets.kinds.";

/// A plain-text table of every numeric field either report has, with
/// `n/a` where one side has no figure and the ratio right / left; then,
/// when the reports count packets by kind, the top kinds by count and by
/// bytes with the left / right ratio instead of a row per kind.
pub fn side_by_side(left: &Value, right: &Value, labels: [&str; 2]) -> String {
    let (a, b) = (flatten_with_gaps(left), flatten_with_gaps(right));
    let keys: std::collections::BTreeSet<&String> = a
        .keys()
        .chain(b.keys())
        .filter(|key| !key.starts_with(PACKET_KINDS))
        .collect();
    let width = keys.iter().map(|k| k.len()).max().unwrap_or(5).max(5);
    let ratio_label = format!("{}/{}", labels[1], labels[0]);
    let mut out = format!(
        "{:width$}  {:>14}  {:>14}  {:>14}\n",
        "field", labels[0], labels[1], ratio_label
    );
    let shown = |v: Option<&Option<f64>>| v.copied().flatten().map_or("n/a".into(), number);
    for key in keys {
        let (l, r) = (a.get(key).copied().flatten(), b.get(key).copied().flatten());
        let ratio = match (l, r) {
            (Some(l), Some(r)) if l != 0.0 => format!("{:.2}x", r / l),
            (Some(l), Some(r)) if l == r => "=".into(),
            (Some(_), Some(_)) => "new".into(),
            _ => "n/a".into(),
        };
        out.push_str(&format!(
            "{key:width$}  {:>14}  {:>14}  {ratio:>14}\n",
            shown(a.get(key)),
            shown(b.get(key)),
        ));
    }
    if left["packets"]["kinds"].is_object() || right["packets"]["kinds"].is_object() {
        use crate::packets::{Measure, top_kinds};
        for measure in [Measure::Count, Measure::WireBytes, Measure::Bytes] {
            out.push('\n');
            out.push_str(&top_kinds(left, right, labels, measure, TOP_PACKET_KINDS));
        }
    }
    out
}

fn number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.0}")
    } else {
        format!("{v:.3}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_move_shortfall_marks_the_workload_mismatched() {
        let run = |moves: u64| {
            serde_json::json!({
                "bots": { "moves": moves },
                "cpu": { "server_s": 2.0 },
                "network": { "wire_bytes": 4000, "packets": 40 },
            })
        };
        let labels = ["tuff", "vanilla"];
        assert_eq!(workload_check(&run(1000), &run(950), labels), None);
        let reason = workload_check(&run(1000), &run(760), labels).unwrap();
        assert!(
            reason.starts_with("vanilla made 760 moves, 0.76x tuff's 1000"),
            "{reason}"
        );
        let reason = workload_check(&run(500), &run(1000), labels).unwrap();
        assert!(reason.starts_with("tuff made 500"), "{reason}");
        assert_eq!(
            workload_check(&run(10), &serde_json::json!({}), labels),
            None
        );

        let per = with_per_move(&run(1000))["per_move"].clone();
        assert_eq!(per["server_cpu_ms"], 2.0);
        assert_eq!(per["wire_bytes"], 4.0);
        assert_eq!(per["packets"], 0.04);
        assert!(with_per_move(&run(0)).get("per_move").is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn tick_summary_uses_nearest_rank() {
        let ms: Vec<f64> = (1..=100).rev().map(f64::from).collect();
        let summary = TickSummary::new(&ms, 5.0, 2);
        assert_eq!(summary.count, 100);
        assert_eq!(summary.tps, 20.0);
        assert_eq!(summary.p50, 50.0);
        assert_eq!(summary.p99, 99.0);
        assert_eq!(summary.p999, 100.0);
        assert_eq!(summary.max, 100.0);
        assert_eq!(summary.mspt_mean, 50.5);
        assert_eq!(summary.over_budget, 50);
        assert_eq!(summary.lagged, 2);
        assert_eq!(TickSummary::new(&[], 0.0, 0), TickSummary::default());
    }

    #[tuff_core::watchdog]
    #[test]
    fn proc_and_ps_output_parse() {
        let stat = "4242 (tuff (main)) S 1 2 3 4 5 6 7 8 9 10 250 150 0 0 20 0 30";
        assert_eq!(parse_proc_stat(stat), Some(4.0));
        let status = "Name:\ttuff\nVmPeak:\t 9000 kB\nVmRSS:\t  2048 kB\n";
        assert_eq!(parse_proc_rss(status), Some(2048 * 1024));
        let mac = parse_ps(" 51200   1:02.50\n").unwrap();
        assert_eq!(mac.rss_bytes, 51200 * 1024);
        assert!((mac.cpu_s - 62.5).abs() < 1e-9);
        let linux = parse_ps("100 1-02:03:04").unwrap();
        assert_eq!(linux.cpu_s, 86_400.0 + 7384.0);
        assert!(parse_ps("").is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn totals_subtract_counters_but_keep_player_counts() {
        let mut state = BotState {
            bytes: 100,
            chunks: 7,
            ..BotState::default()
        };
        let mut early = BotTotals::default();
        early.add(&state);
        state.bytes = 250;
        state.chunks = 10;
        state.disconnected = Some("kicked".into());
        let mut late = BotTotals::default();
        late.add(&state);
        let delta = late.since(&early);
        assert_eq!((delta.wire_bytes, delta.chunks), (150, 3));
        assert_eq!((delta.players, delta.connected), (1, 0));
    }

    #[tuff_core::watchdog]
    #[test]
    fn setbacks_over_the_limit_make_a_run_invalid() {
        let state = BotState {
            moves: 1000,
            setbacks: 10,
            ..BotState::default()
        };
        let mut totals = BotTotals::default();
        totals.add(&state);
        assert_eq!((totals.moves, totals.setbacks), (1000, 10));
        let report =
            |setbacks: u64| serde_json::json!({"bots": {"moves": 1000, "setbacks": setbacks}});
        assert_eq!(
            verdict(&report(10)),
            "valid: 10 setbacks in 1000 moves (1.00%)"
        );
        assert_eq!(
            verdict(&report(11)),
            "INVALID: 11 setbacks in 1000 moves (1.10%, over the 1% limit)"
        );
        assert_eq!(
            setback_check(5, 0).1.as_deref(),
            Some("5 setbacks in 0 moves (500.00%, over the 1% limit)")
        );
        let failed =
            serde_json::json!({"failed": {"error": "rcon closed", "cause": "watchdog: stuck"}});
        assert_eq!(verdict(&failed), "FAILED: watchdog: stuck");
        assert_eq!(verdict(&serde_json::json!({})), "valid (no setback counts)");
        let old: BotTotals = serde_json::from_value(serde_json::json!({
            "players": 1, "connected": 1, "wire_bytes": 0, "payload_bytes": 0,
            "chunk_bytes": 0, "chunks": 0, "packets": 0, "teleports": 0, "deaths": 0,
            "windows_opened": 0, "digs_started": 0, "digs_finished": 0,
            "uses_on_block": 0, "attacks": 0, "swings": 0
        }))
        .unwrap();
        assert_eq!(old.setbacks, 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn compare_lists_shared_numeric_fields() {
        let before =
            serde_json::json!({"ticks": {"p50": 2.0, "count": 10}, "label": "off", "x": 0});
        let after = serde_json::json!({"ticks": {"p50": 3.0, "count": 10}, "label": "on", "x": 5});
        let table = compare(&before, &after);
        assert!(table.contains("ticks.p50"));
        assert!(table.contains("+50.0%"));
        assert!(
            table
                .lines()
                .any(|l| l.starts_with("ticks.count") && l.ends_with("+0.0%"))
        );
        assert!(
            table
                .lines()
                .any(|l| l.starts_with('x') && l.trim_end().ends_with("new"))
        );
        assert!(!table.contains("label"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn side_by_side_marks_missing_figures() {
        let tuff = serde_json::json!({"ticks": {"p50": 2.0}, "world": {"loaded_chunks": null}});
        let vanilla =
            serde_json::json!({"ticks": {"p50": 5.0}, "world": {"loaded_chunks": 900}, "x": 1});
        let table = side_by_side(&tuff, &vanilla, ["tuff", "vanilla"]);
        assert!(table.lines().next().unwrap().contains("vanilla/tuff"));
        let line = |key: &str| {
            table
                .lines()
                .find(|l| l.starts_with(key))
                .unwrap()
                .split_whitespace()
                .skip(1)
                .collect::<Vec<_>>()
        };
        assert_eq!(line("ticks.p50"), ["2", "5", "2.50x"]);
        assert_eq!(line("world.loaded_chunks"), ["n/a", "900", "n/a"]);
        assert_eq!(line("x"), ["n/a", "1", "n/a"]);
        assert_eq!(flatten(&tuff).len(), 1);

        let mut kinds = crate::packets::Kinds::default();
        let mut tally = crate::packets::PacketTally::default();
        tally.record(1, 50, 80);
        kinds.add(&tally);
        let with_kinds = serde_json::json!({ "packets": kinds.report(1, 1.0) });
        let table = side_by_side(&with_kinds, &with_kinds, ["tuff", "vanilla"]);
        assert!(table.contains("packets.per_player_per_s.count"));
        assert!(!table.contains("packets.kinds."));
        assert!(table.contains("top 15 kinds by wire bytes"));
        assert!(
            table
                .lines()
                .any(|l| l.starts_with("add_entity") && l.ends_with("1.00x"))
        );
    }
}
