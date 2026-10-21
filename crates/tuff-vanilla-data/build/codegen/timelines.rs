//! The built-in `minecraft:timeline` entries. The clock, period and time
//! markers are typed; `tracks` is an open-ended map of environment attributes
//! (mods and future versions add new ones), kept as `(key, raw JSON)` pairs.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::json;
use crate::input::Input;
use crate::names::{self, shouty, str_literal};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Timeline {
    clock: String,
    #[serde(default)]
    period_ticks: Option<u32>,
    #[serde(default)]
    tracks: Map<String, Value>,
    #[serde(default)]
    time_markers: BTreeMap<String, TimeMarker>,
}

/// A bare tick count, or an object that can also show it in commands.
#[derive(Deserialize)]
#[serde(untagged)]
enum TimeMarker {
    Ticks(i32),
    Full(FullTimeMarker),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FullTimeMarker {
    ticks: i32,
    #[serde(default)]
    show_in_commands: bool,
}

fn qualified(name: &str) -> String {
    if name.contains(':') {
        name.to_owned()
    } else {
        format!("minecraft:{name}")
    }
}

pub fn generate(input: &Input) -> Result<String> {
    let mut timelines = Vec::new();
    for (path, json) in &input.data_json {
        let Some((namespace, rest)) = path.split_once('/') else {
            continue;
        };
        let Some(name) = rest
            .strip_prefix("timeline/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        let parsed: Timeline =
            serde_json::from_value(json.clone()).with_context(|| format!("data/{path}"))?;
        ensure!(
            parsed.period_ticks != Some(0),
            "data/{path}: period_ticks must be positive"
        );
        timelines.push((format!("{namespace}:{name}"), parsed));
    }
    timelines.sort_by(|(a, _), (b, _)| a.cmp(b));
    ensure!(!timelines.is_empty(), "no timelines found in the data pack");

    let consts = names::unique(timelines.iter().map(|(name, _)| name.as_str()), shouty)?;
    let mut out = String::new();
    for ((name, timeline), ident) in timelines.iter().zip(&consts) {
        let mut markers: Vec<(String, i32, bool)> = timeline
            .time_markers
            .iter()
            .map(|(name, marker)| {
                let (ticks, show) = match marker {
                    TimeMarker::Ticks(ticks) => (*ticks, false),
                    TimeMarker::Full(full) => (full.ticks, full.show_in_commands),
                };
                (qualified(name), ticks, show)
            })
            .collect();
        markers.sort();
        write!(
            out,
            "pub static {ident}: Timeline = Timeline {{
    name: {},
    clock: {},
    period_ticks: {},
    time_markers: &[{}],
    tracks: &[{}],
}};\n",
            str_literal(name),
            str_literal(&qualified(&timeline.clock)),
            match timeline.period_ticks {
                Some(period) => format!("Some({period})"),
                None => "None".to_owned(),
            },
            markers
                .iter()
                .map(|(name, ticks, show)| format!(
                    "TimeMarker {{ name: {}, ticks: {ticks}, show_in_commands: {show} }}",
                    str_literal(name)
                ))
                .collect::<Vec<_>>()
                .join(", "),
            timeline
                .tracks
                .iter()
                .map(|(key, value)| format!("({}, {})", str_literal(key), json::literal(value)))
                .collect::<Vec<_>>()
                .join(", "),
        )?;
    }
    writeln!(out, "pub const ALL: &[(&str, &Timeline)] = &[")?;
    for (name, ident) in timelines.iter().map(|(name, _)| name).zip(&consts) {
        writeln!(out, "    ({}, &{ident}),", str_literal(name))?;
    }
    writeln!(out, "];")?;
    Ok(out)
}
