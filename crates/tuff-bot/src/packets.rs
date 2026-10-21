//! Per-kind accounting of the clientbound play packets a bot receives:
//! how many, and their size framed on the wire and decompressed. Each bot
//! keeps its own [`PacketTally`]; a fleet sums them into [`Kinds`] by name
//! when it reports.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tuff_vanilla_data::packet::play::Clientbound;

/// Packets of one kind, or of every kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    pub count: u64,
    /// Framed as on the wire: length prefix and (compressed) body.
    pub wire_bytes: u64,
    /// Packet id and data after decompression.
    pub bytes: u64,
}

impl Volume {
    pub fn add(&mut self, other: Self) {
        self.count += other.count;
        self.wire_bytes += other.wire_bytes;
        self.bytes += other.bytes;
    }

    pub fn since(self, earlier: Self) -> Self {
        Self {
            count: self.count.saturating_sub(earlier.count),
            wire_bytes: self.wire_bytes.saturating_sub(earlier.wire_bytes),
            bytes: self.bytes.saturating_sub(earlier.bytes),
        }
    }
}

const UNKNOWN: &str = "unknown";

/// One bot's received play packets, indexed by packet id so counting one
/// is a few adds; ids outside the protocol share one slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacketTally {
    by_id: Box<[Volume; Clientbound::COUNT + 1]>,
}

impl Default for PacketTally {
    fn default() -> Self {
        Self {
            by_id: Box::new([Volume::default(); Clientbound::COUNT + 1]),
        }
    }
}

impl PacketTally {
    pub fn record(&mut self, id: i32, wire_bytes: usize, bytes: usize) {
        let slot = usize::try_from(id)
            .ok()
            .filter(|&id| id < Clientbound::COUNT)
            .unwrap_or(Clientbound::COUNT);
        let volume = &mut self.by_id[slot];
        volume.count += 1;
        volume.wire_bytes += wire_bytes as u64;
        volume.bytes += bytes as u64;
    }

    pub fn get(&self, kind: Clientbound) -> Volume {
        self.by_id[kind.id() as usize]
    }

    pub fn total(&self) -> Volume {
        let mut total = Volume::default();
        self.by_id.iter().for_each(|volume| total.add(*volume));
        total
    }

    /// The kinds seen so far, by name without the namespace.
    pub fn kinds(&self) -> impl Iterator<Item = (&'static str, Volume)> + '_ {
        self.by_id
            .iter()
            .enumerate()
            .filter(|(_, volume)| volume.count > 0)
            .map(|(id, volume)| (kind_name(id), *volume))
    }
}

fn kind_name(id: usize) -> &'static str {
    Clientbound::from_id(id as i32).map_or(UNKNOWN, |kind| {
        kind.name()
            .strip_prefix("minecraft:")
            .unwrap_or(kind.name())
    })
}

/// Packet volumes summed over bots, by kind name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Kinds(pub BTreeMap<String, Volume>);

impl Kinds {
    pub fn add(&mut self, tally: &PacketTally) {
        for (name, volume) in tally.kinds() {
            self.0.entry(name.to_owned()).or_default().add(volume);
        }
    }

    pub fn since(&self, earlier: &Self) -> Self {
        Self(
            self.0
                .iter()
                .map(|(name, volume)| {
                    let before = earlier.0.get(name).copied().unwrap_or_default();
                    (name.clone(), volume.since(before))
                })
                .filter(|(_, volume)| volume.count > 0)
                .collect(),
        )
    }

    pub fn total(&self) -> Volume {
        let mut total = Volume::default();
        self.0.values().for_each(|volume| total.add(*volume));
        total
    }

    /// The report's `packets` section for `players` bots over `window_s`:
    /// totals, rates per player per second, and every kind's totals.
    /// `player_seconds` lets a reader turn any kind into a rate.
    pub fn report(&self, players: u64, window_s: f64) -> Value {
        let player_seconds = players.max(1) as f64 * window_s.max(f64::EPSILON);
        let total = self.total();
        let rate = |v: u64| v as f64 / player_seconds;
        json!({
            "player_seconds": player_seconds,
            "total": total,
            "per_player_per_s": {
                "count": rate(total.count),
                "wire_bytes": rate(total.wire_bytes),
                "bytes": rate(total.bytes),
            },
            "kinds": self.0,
        })
    }
}

/// Which figure of a [`Volume`] a top-kinds table ranks by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measure {
    Count,
    WireBytes,
    Bytes,
}

impl Measure {
    fn key(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::WireBytes => "wire_bytes",
            Self::Bytes => "bytes",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Count => "packets",
            Self::WireBytes => "wire bytes",
            Self::Bytes => "uncompressed bytes",
        }
    }
}

/// Per player per second rates of `measure` by kind, from a report's
/// `packets` section; empty when it has none.
pub fn rates(report: &Value, measure: Measure) -> BTreeMap<String, f64> {
    let packets = &report["packets"];
    let Some(player_seconds) = packets["player_seconds"].as_f64().filter(|s| *s > 0.0) else {
        return BTreeMap::new();
    };
    let Some(kinds) = packets["kinds"].as_object() else {
        return BTreeMap::new();
    };
    kinds
        .iter()
        .filter_map(|(name, volume)| {
            let value = volume[measure.key()].as_f64()?;
            Some((name.clone(), value / player_seconds))
        })
        .collect()
}

/// The `top` kinds by `measure` in either report, highest rate on either
/// side first, per player per second with the left/right ratio.
pub fn top_kinds(
    left: &Value,
    right: &Value,
    labels: [&str; 2],
    measure: Measure,
    top: usize,
) -> String {
    let (a, b) = (rates(left, measure), rates(right, measure));
    let mut names: Vec<&String> = a.keys().chain(b.keys()).collect();
    names.sort();
    names.dedup();
    let rate = |map: &BTreeMap<String, f64>, name: &str| map.get(name).copied().unwrap_or(0.0);
    let peak = |name: &str| rate(&a, name).max(rate(&b, name));
    names.sort_by(|x, y| peak(y).total_cmp(&peak(x)).then(x.cmp(y)));
    names.truncate(top);
    let width = names.iter().map(|n| n.len()).max().unwrap_or(4).max(4);
    let ratio_label = format!("{}/{}", labels[0], labels[1]);
    let mut out = format!(
        "top {top} kinds by {}, per player per second\n{:width$}  {:>14}  {:>14}  {:>14}\n",
        measure.title(),
        "kind",
        labels[0],
        labels[1],
        ratio_label
    );
    for name in names {
        let (l, r) = (rate(&a, name), rate(&b, name));
        let ratio = if r > 0.0 {
            format!("{:.2}x", l / r)
        } else if l > 0.0 {
            format!("only {}", labels[0])
        } else {
            "=".into()
        };
        out.push_str(&format!(
            "{name:width$}  {:>14.3}  {:>14.3}  {ratio:>14}\n",
            l, r
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn tallies_count_by_kind_and_sum_over_bots() {
        let mut one = PacketTally::default();
        one.record(Clientbound::KeepAlive.id(), 12, 9);
        one.record(Clientbound::KeepAlive.id(), 12, 9);
        one.record(Clientbound::LevelChunkWithLight.id(), 4000, 20000);
        one.record(-1, 3, 2);
        one.record(9999, 5, 4);
        assert_eq!(
            one.get(Clientbound::KeepAlive),
            Volume {
                count: 2,
                wire_bytes: 24,
                bytes: 18
            }
        );
        assert_eq!(one.total().count, 5);
        let names: Vec<_> = one.kinds().map(|(name, _)| name).collect();
        assert_eq!(names, ["keep_alive", "level_chunk_with_light", UNKNOWN]);

        let mut two = PacketTally::default();
        two.record(Clientbound::KeepAlive.id(), 10, 9);
        let mut early = Kinds::default();
        early.add(&one);
        let mut late = early.clone();
        late.add(&two);
        assert_eq!(late.0["keep_alive"].count, 3);
        assert_eq!(late.0["unknown"].wire_bytes, 8);
        let window = late.since(&early);
        assert_eq!(
            window.0.keys().collect::<Vec<_>>(),
            [&"keep_alive".to_string()]
        );
        assert_eq!(window.0["keep_alive"].wire_bytes, 10);

        let report = json!({ "packets": late.report(2, 0.5) });
        assert_eq!(report["packets"]["total"]["count"], 6);
        assert_eq!(report["packets"]["per_player_per_s"]["count"], 6.0);
        assert_eq!(
            rates(&report, Measure::Bytes)["level_chunk_with_light"],
            20000.0
        );
        let back: Kinds = serde_json::from_value(report["packets"]["kinds"].clone()).unwrap();
        assert_eq!(back, late);
    }

    #[tuff_core::watchdog]
    #[test]
    fn top_kinds_rank_by_either_side_with_ratios() {
        let side = |kinds: &[(&str, u64)]| {
            let kinds = Kinds(
                kinds
                    .iter()
                    .map(|(name, count)| {
                        let volume = Volume {
                            count: *count,
                            wire_bytes: count * 10,
                            bytes: count * 20,
                        };
                        (name.to_string(), volume)
                    })
                    .collect(),
            );
            json!({ "packets": kinds.report(1, 1.0) })
        };
        let tuff = side(&[
            ("move_entity_pos", 400),
            ("keep_alive", 1),
            ("set_time", 20),
        ]);
        let vanilla = side(&[
            ("move_entity_pos", 100),
            ("debug_sample", 200),
            ("set_time", 1),
        ]);
        let table = top_kinds(&tuff, &vanilla, ["tuff", "vanilla"], Measure::Count, 3);
        let rows: Vec<Vec<&str>> = table
            .lines()
            .skip(2)
            .map(|l| l.split_whitespace().collect())
            .collect();
        assert_eq!(
            rows,
            [
                vec!["move_entity_pos", "400.000", "100.000", "4.00x"],
                vec!["debug_sample", "0.000", "200.000", "0.00x"],
                vec!["set_time", "20.000", "1.000", "20.00x"],
            ]
        );
        assert!(table.lines().nth(1).unwrap().ends_with("tuff/vanilla"));
        let bytes = top_kinds(&tuff, &vanilla, ["tuff", "vanilla"], Measure::WireBytes, 9);
        assert!(bytes.contains("keep_alive"));
        assert!(
            bytes
                .lines()
                .any(|l| l.starts_with("keep_alive") && l.ends_with("only tuff"))
        );
        assert!(
            top_kinds(&json!({}), &json!({}), ["a", "b"], Measure::Count, 5)
                .lines()
                .count()
                == 2
        );
    }
}
