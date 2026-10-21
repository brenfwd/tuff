//! The timings report: a versioned JSON document, plus exports to folded
//! stacks and speedscope for external tools.
//!
//! Schema version 2 ([`VERSION`]), top level ([`Report`]):
//!
//! - `format`: always `"tuff-timings"`; `version`: the schema version.
//! - `id`: the report name (`timings-YYYYMMDD-HHMMSS`, UTC start time).
//! - `started_unix_ms`, `duration_ns`: when the recording started (wall
//!   clock) and how long it ran (monotonic clock).
//! - `server`: `{ name, version, game_version }`.
//! - `config`: how it was recorded: `clock` (`cntvct`, `tsc` or
//!   `monotonic`: the counter spans read), `histogram` (percentile
//!   bucketing), `max_nodes_per_thread`, `max_depth`.
//! - `ticks`: `null` when no server tick finished, else `{ count, tps,
//!   mspt: { mean, min, max, p50, p90, p99 } }` with mspt in milliseconds
//!   from the `server.tick` samples.
//! - `features`: every loaded feature with the time and calls of its hook
//!   spans (`feature` nodes tagged with the feature id): `[{ id, total_ns,
//!   calls }]`, most expensive first.
//! - `dropped`: spans and call paths lost to the depth or node limits.
//! - `threads`: `[{ index, name, dropped, total_ns, roots: [Node] }]`, one
//!   per thread that recorded, with that thread's own call tree.
//! - `merged`: `[Node]`, every thread's tree merged by call path.
//!
//! A `Node` is `{ name, tag?, async?, count, total_ns, self_ns, min_ns,
//! max_ns, p50_ns, p90_ns, p99_ns, children? }`. `name` is the span site's
//! name (`region.tick`, `entity`, `net.encode`, `feature`...); `tag`, when
//! present, what that call was about (a region as `<dimension> <x>,<z>`,
//! an entity type, a packet type, a feature id). `async` nodes are wall
//! time across awaits and are not subtracted from their parent's
//! `self_ns`. Percentiles come from log-linear buckets, at most 12.5% wide.
//! Siblings are sorted by `total_ns`, largest first.

use serde::{Deserialize, Serialize};

use super::profile::{Labels, Node, Profile};
use super::{MAX_DEPTH, MAX_NODES};

pub const FORMAT: &str = "tuff-timings";
pub const VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub format: String,
    pub version: u32,
    pub id: String,
    pub started_unix_ms: u64,
    pub duration_ns: u64,
    pub server: ServerInfo,
    pub config: Config,
    pub ticks: Option<Ticks>,
    pub features: Vec<FeatureTime>,
    pub dropped: u64,
    pub threads: Vec<Thread>,
    pub merged: Vec<ReportNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub game_version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Config {
    pub clock: String,
    pub histogram: String,
    pub max_nodes_per_thread: u64,
    pub max_depth: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Ticks {
    pub count: u64,
    pub tps: f64,
    pub mspt: Mspt,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Mspt {
    pub mean: f64,
    pub min: f64,
    pub max: f64,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FeatureTime {
    pub id: String,
    pub total_ns: u64,
    pub calls: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Thread {
    pub index: u64,
    pub name: String,
    pub dropped: u64,
    pub total_ns: u64,
    pub roots: Vec<ReportNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ReportNode {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, rename = "async", skip_serializing_if = "is_false")]
    pub asynchronous: bool,
    pub count: u64,
    pub total_ns: u64,
    pub self_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub p50_ns: u64,
    pub p90_ns: u64,
    pub p99_ns: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<ReportNode>,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl ReportNode {
    fn from_node(node: &Node, labels: &Labels) -> Self {
        let stats = &node.stats;
        Self {
            name: node.name.to_owned(),
            tag: labels.tag(node.tag),
            asynchronous: node.asynchronous,
            count: stats.count,
            total_ns: stats.total_ns,
            self_ns: node.self_ns(),
            min_ns: if stats.count == 0 { 0 } else { stats.min_ns },
            max_ns: stats.max_ns,
            p50_ns: stats.quantile(0.5),
            p90_ns: stats.quantile(0.9),
            p99_ns: stats.quantile(0.99),
            children: node
                .children
                .iter()
                .map(|child| Self::from_node(child, labels))
                .collect(),
        }
    }

    /// The frame text exports use: the name, then the tag in brackets.
    pub fn frame(&self) -> String {
        match &self.tag {
            Some(tag) => format!("{} [{tag}]", self.name),
            None => self.name.clone(),
        }
    }

    fn visit<'a>(&'a self, stack: &mut Vec<&'a ReportNode>, out: &mut impl FnMut(&[&ReportNode])) {
        stack.push(self);
        out(stack);
        for child in &self.children {
            child.visit(stack, out);
        }
        stack.pop();
    }
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1_000_000.0
}

impl Report {
    pub fn new(id: &str, profile: &Profile, features: &[&'static str], labels: &Labels) -> Self {
        let ticks = profile.ticks().filter(|t| t.count > 0).map(|t| Ticks {
            count: t.count,
            tps: t.count as f64 / profile.duration.as_secs_f64().max(f64::EPSILON),
            mspt: Mspt {
                mean: ms(t.total_ns) / t.count as f64,
                min: ms(t.min_ns),
                max: ms(t.max_ns),
                p50: ms(t.quantile(0.5)),
                p90: ms(t.quantile(0.9)),
                p99: ms(t.quantile(0.99)),
            },
        });
        let mut feature_times: Vec<FeatureTime> = profile
            .features()
            .into_iter()
            .map(|(id, total_ns, calls)| FeatureTime {
                id: id.to_owned(),
                total_ns,
                calls,
            })
            .collect();
        for id in features {
            if !feature_times.iter().any(|p| p.id == *id) {
                feature_times.push(FeatureTime {
                    id: (*id).to_owned(),
                    total_ns: 0,
                    calls: 0,
                });
            }
        }
        let started_unix_ms = profile
            .started
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        Self {
            format: FORMAT.to_owned(),
            version: VERSION,
            id: id.to_owned(),
            started_unix_ms,
            duration_ns: profile.duration.as_nanos() as u64,
            server: ServerInfo {
                name: labels.server_name.clone(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                game_version: tuff_vanilla_data::version::NAME.to_owned(),
            },
            config: Config {
                clock: super::clock::init().name().to_owned(),
                histogram: "log-linear, exact below 16 ns, 8 buckets per power of two".to_owned(),
                max_nodes_per_thread: MAX_NODES as u64,
                max_depth: MAX_DEPTH as u64,
            },
            ticks,
            features: feature_times,
            dropped: profile.dropped(),
            threads: profile
                .threads
                .iter()
                .map(|thread| Thread {
                    index: thread.index as u64,
                    name: thread.name.clone(),
                    dropped: thread.dropped,
                    total_ns: thread
                        .roots
                        .iter()
                        .filter(|r| !r.asynchronous)
                        .map(|r| r.stats.total_ns)
                        .sum(),
                    roots: thread
                        .roots
                        .iter()
                        .map(|node| ReportNode::from_node(node, labels))
                        .collect(),
                })
                .collect(),
            merged: profile
                .merged
                .iter()
                .map(|node| ReportNode::from_node(node, labels))
                .collect(),
        }
    }

    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a report always serializes")
    }

    /// Parses a report, refusing other formats and newer schema versions.
    pub fn from_json(bytes: &[u8]) -> Result<Self, String> {
        let report: Report = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
        if report.format != FORMAT {
            return Err(format!("not a timings report: format {:?}", report.format));
        }
        if report.version > VERSION {
            return Err(format!(
                "timings report version {} is newer than {VERSION}",
                report.version
            ));
        }
        Ok(report)
    }

    /// The merged tree as folded stacks (`frame;frame;frame self_ns` per
    /// line), the input of flamegraph.pl, inferno and speedscope.
    pub fn folded(&self) -> String {
        let mut out = String::new();
        let mut stack = Vec::new();
        for root in &self.merged {
            root.visit(&mut stack, &mut |path| {
                let leaf = path[path.len() - 1];
                if leaf.self_ns == 0 {
                    return;
                }
                let frames: Vec<String> =
                    path.iter().map(|n| n.frame().replace(';', ":")).collect();
                out.push_str(&frames.join(";"));
                out.push(' ');
                out.push_str(&leaf.self_ns.to_string());
                out.push('\n');
            });
        }
        out
    }

    /// A speedscope file: one sampled profile for the merged tree, then one
    /// per thread, each call path weighted by its self time.
    pub fn speedscope(&self) -> serde_json::Value {
        let mut frames: Vec<String> = Vec::new();
        let mut index: std::collections::HashMap<String, usize> = Default::default();
        let mut frame_of = |text: String| -> usize {
            *index.entry(text).or_insert_with_key(|text| {
                frames.push(text.clone());
                frames.len() - 1
            })
        };
        let mut profiles = Vec::new();
        let mut trees: Vec<(String, &Vec<ReportNode>)> = vec![("merged".to_owned(), &self.merged)];
        for thread in &self.threads {
            trees.push((format!("{} #{}", thread.name, thread.index), &thread.roots));
        }
        for (name, roots) in trees {
            let mut samples: Vec<Vec<usize>> = Vec::new();
            let mut weights: Vec<u64> = Vec::new();
            let mut stack = Vec::new();
            for root in roots {
                root.visit(&mut stack, &mut |path| {
                    let leaf = path[path.len() - 1];
                    if leaf.self_ns == 0 {
                        return;
                    }
                    samples.push(path.iter().map(|n| frame_of(n.frame())).collect());
                    weights.push(leaf.self_ns);
                });
            }
            let end: u64 = weights.iter().sum();
            profiles.push(serde_json::json!({
                "type": "sampled",
                "name": name,
                "unit": "nanoseconds",
                "startValue": 0,
                "endValue": end,
                "samples": samples,
                "weights": weights,
            }));
        }
        serde_json::json!({
            "$schema": "https://www.speedscope.app/file-format-schema.json",
            "exporter": "tuff",
            "name": self.id,
            "activeProfileIndex": 0,
            "shared": {
                "frames": frames
                    .iter()
                    .map(|name| serde_json::json!({ "name": name }))
                    .collect::<Vec<_>>(),
            },
            "profiles": profiles,
        })
    }
}
