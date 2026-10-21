//! `/tuff timings record [seconds]|stop|status|list|export <id> <format>`:
//! records span timings (see `tuff_timings`) and writes the JSON
//! report to the server's `timings/` directory.

use std::time::Duration;

use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, CommandNode, CommandNodeKind};
use tuff_core::world::commands::{Context, text};
use tuff_timings::{Export, StartOptions, Summary, TimingsError};

use crate::tuff::literal;

const SECONDS: CommandNode = CommandNode {
    name: "seconds",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:integer",
        properties: Some(r#"{"min":1,"max":86400}"#),
    },
    executable: true,
    redirect: None,
    permissions: None,
    children: &[],
};
const FORMAT: CommandNode = CommandNode {
    name: "format",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:string",
        properties: Some(r#"{"type":"word"}"#),
    },
    executable: true,
    redirect: None,
    permissions: None,
    children: &[],
};
const ID: CommandNode = CommandNode {
    name: "id",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:string",
        properties: Some(r#"{"type":"word"}"#),
    },
    executable: false,
    redirect: None,
    permissions: None,
    children: &[FORMAT],
};
const RECORD: CommandNode = literal("record", true, &[SECONDS]);
const STOP: CommandNode = literal("stop", true, &[]);
const STATUS: CommandNode = literal("status", true, &[]);
const LIST: CommandNode = literal("list", true, &[]);
const EXPORT: CommandNode = literal("export", false, &[ID]);
pub const TIMINGS: CommandNode = crate::tuff::guarded(
    literal("timings", false, &[EXPORT, LIST, RECORD, STATUS, STOP]),
    r#"{"type":"tuff:permission","node":"tuff.command.timings"}"#,
);

fn fail(err: TimingsError) -> CommandError {
    CommandError::text(text(err.to_string()))
}

fn ms(ns: u64) -> String {
    format!("{:.2} ms", ns as f64 / 1_000_000.0)
}

/// The lines `/tuff timings stop` prints for a finished recording.
pub fn summary_lines(summary: &Summary) -> Vec<String> {
    let mut lines = Vec::new();
    let mut head = format!(
        "Timings {} stopped after {:.1} s",
        summary.id,
        summary.duration.as_secs_f64()
    );
    if let (Some(tps), Some(mean), Some(p99), Some(max)) = (
        summary.tps,
        summary.mspt_mean,
        summary.mspt_p99,
        summary.mspt_max,
    ) {
        head.push_str(&format!(
            ": {} ticks, {tps:.2} TPS, mspt mean {mean:.2} p99 {p99:.2} max {max:.2}",
            summary.ticks
        ));
    }
    lines.push(head);
    lines.push(match &summary.file {
        Ok(file) => format!("Saved {file} ({} KiB)", summary.bytes.div_ceil(1024)),
        Err(err) => format!("Not saved ({err}); kept in memory as {}", summary.id),
    });
    if !summary.top.is_empty() {
        let top: Vec<String> = summary
            .top
            .iter()
            .map(|(label, ns)| format!("{label} {}", ms(*ns)))
            .collect();
        lines.push(format!("Most self time: {}", top.join(", ")));
    }
    if !summary.features.is_empty() {
        let features: Vec<String> = summary
            .features
            .iter()
            .take(5)
            .map(|(id, ns)| format!("{id} {}", ms(*ns)))
            .collect();
        lines.push(format!("Feature hooks: {}", features.join(", ")));
    }
    if summary.dropped > 0 {
        lines.push(format!(
            "{} spans were dropped (too deep or too many call paths)",
            summary.dropped
        ));
    }
    lines
}

pub async fn run(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let timings = ctx.shared().timings.clone();
    match ctx.literals().get(2).copied() {
        Some("record") => {
            let duration = match ctx.arg("seconds") {
                Some(Value::Int(seconds)) => Some(Duration::from_secs(*seconds as u64)),
                _ => None,
            };
            timings
                .start(StartOptions {
                    duration,
                    by: ctx.source.name.clone(),
                })
                .await
                .map_err(fail)?;
            ctx.reply(text(match duration {
                Some(after) => format!(
                    "Recording timings for {} s; /tuff timings stop ends it sooner",
                    after.as_secs()
                ),
                None => {
                    "Recording timings; /tuff timings stop ends it and writes the report".to_owned()
                }
            }));
            Ok(1)
        }
        Some("stop") => {
            let summary = timings.stop().await.map_err(fail)?;
            for line in summary_lines(&summary) {
                ctx.reply(text(line));
            }
            Ok(1)
        }
        Some("status") => {
            let status = timings.status().await.map_err(fail)?;
            if let Some(recording) = &status.recording {
                let mut line = format!(
                    "Recording timings for {:.1} s on {} threads",
                    recording.elapsed.as_secs_f64(),
                    recording.threads
                );
                if !recording.by.is_empty() {
                    line.push_str(&format!(", started by {}", recording.by));
                }
                if let Some(after) = recording.stops_after {
                    line.push_str(&format!(", stops after {} s", after.as_secs()));
                }
                ctx.reply(text(line));
            } else if status.busy_elsewhere {
                ctx.reply(text("Another server in this process is recording timings"));
            } else {
                ctx.reply(text("Not recording timings"));
            }
            if let Some(last) = &status.last {
                ctx.reply(text(format!(
                    "Last report: {}",
                    last.file.as_deref().unwrap_or(&last.id)
                )));
            }
            Ok(i32::from(status.recording.is_some()))
        }
        Some("list") => {
            let list = timings.list().await.map_err(fail)?;
            if list.is_empty() {
                ctx.reply(text("No timings reports"));
            } else {
                let ids: Vec<&str> = list.iter().map(|info| info.id.as_str()).collect();
                ctx.reply(text(format!(
                    "{} timings reports: {}",
                    ids.len(),
                    ids.join(", ")
                )));
            }
            Ok(list.len() as i32)
        }
        Some("export") => {
            let (Some(Value::Str(id)), Some(Value::Str(format))) =
                (ctx.arg("id").cloned(), ctx.arg("format").cloned())
            else {
                return Err(CommandError::simple("command.unknown.command"));
            };
            let Some(format) = Export::parse(&format) else {
                return Err(CommandError::text(text(format!(
                    "Unknown export format '{format}'; use json, folded or speedscope"
                ))));
            };
            let file = timings.save_export(&id, format).await.map_err(fail)?;
            ctx.reply(text(format!("Wrote {file}")));
            Ok(1)
        }
        _ => Err(CommandError::simple("command.unknown.command")),
    }
}
