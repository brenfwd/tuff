//! `/tuff chunk generate <x z> <radius> circle|square [--force-regenerate]`,
//! `/tuff chunk cancel` and `/tuff chunk status`: chunk pre-generation
//! jobs (see `tuff_core::world::pregen`) in the source's dimension. The
//! position and radius are in blocks. While a job runs, players holding
//! `tuff.command.chunk` see its progress on a boss bar.

use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, CommandNode, CommandNodeKind};
use tuff_core::protocol::play::{BossAction, BossEvent};
use tuff_core::world::commands::{Context, text};
use tuff_core::world::pregen::{
    Area, JobSpec, MAX_RADIUS, Phase, PregenError, Progress, Shape, Status,
};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{HookFuture, SessionHooks, SessionTick};
use tuff_item::text::TextComponent;

use crate::tuff::{guarded, literal};

pub const CHUNK_NODE: &str = "tuff.command.chunk";
pub const FORCE_FLAG: &str = "--force-regenerate";

const FORCE: CommandNode = literal(FORCE_FLAG, true, &[]);
const CIRCLE: CommandNode = literal("circle", true, &[FORCE]);
const SQUARE: CommandNode = literal("square", true, &[FORCE]);
const RADIUS: CommandNode = CommandNode {
    name: "radius",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:integer",
        properties: Some(r#"{"min":0,"max":100000}"#),
    },
    executable: false,
    redirect: None,
    permissions: None,
    children: &[CIRCLE, SQUARE],
};
const CENTRE: CommandNode = CommandNode {
    name: "center",
    kind: CommandNodeKind::Argument {
        parser: "minecraft:column_pos",
        properties: None,
    },
    executable: false,
    redirect: None,
    permissions: None,
    children: &[RADIUS],
};
const GENERATE: CommandNode = literal("generate", false, &[CENTRE]);
const CANCEL: CommandNode = literal("cancel", true, &[]);
const STATUS: CommandNode = literal("status", true, &[]);
pub const CHUNK: CommandNode = guarded(
    literal("chunk", false, &[CANCEL, GENERATE, STATUS]),
    r#"{"type":"tuff:permission","node":"tuff.command.chunk"}"#,
);

fn fail(err: PregenError) -> CommandError {
    CommandError::text(text(err.to_string()))
}

/// `seconds` as `1h 2m 3s`, leaving out leading zero units.
pub fn duration(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// One line about a running job, as the boss bar and status show it.
pub fn describe(progress: &Progress) -> String {
    let verb = match progress.phase {
        Phase::Removing => "Removing chunks",
        Phase::Generating => "Generating chunks",
    };
    let percent = f64::from(progress.fraction()) * 100.0;
    let mut line = format!(
        "{verb}: {} / {} ({percent:.1}%)",
        progress.done, progress.total
    );
    if progress.rate > 0.0 {
        line.push_str(&format!(" - {:.1} chunks/s", progress.rate));
    }
    if let Some(eta) = progress.eta_secs {
        line.push_str(&format!(" - ETA {}", duration(eta)));
    }
    if progress.cancelling {
        line.push_str(" - cancelling");
    }
    line
}

pub async fn run(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let pregen = ctx.shared().pregen.clone();
    match ctx.literals().get(2).copied() {
        Some("generate") => {
            let Some(Value::Coordinates(centre)) = ctx.arg("center") else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let [x, _, z] = centre.block(&ctx.source.pose());
            let Some(Value::Int(radius)) = ctx.arg("radius").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let shape = ctx
                .literals()
                .iter()
                .find_map(|word| Shape::parse(word))
                .ok_or_else(|| CommandError::simple("command.unknown.argument"))?;
            let regenerate = ctx.literals().contains(&FORCE_FLAG);
            let spec = JobSpec {
                dimension: ctx.source.dimension,
                area: Area::new(x, z, radius.min(MAX_RADIUS), shape),
                regenerate,
                by: ctx.source.name.clone(),
            };
            let started = pregen.start(ctx.shared(), spec).await.map_err(fail)?;
            let verb = if regenerate {
                "Regenerating"
            } else {
                "Generating"
            };
            ctx.success(
                text(format!(
                    "{verb} {} chunks in a {} of radius {} around {x} {z} in {} (job {}); /tuff chunk status shows its progress",
                    started.total, started.shape, started.radius, started.dimension, started.id
                )),
                true,
            )
            .await;
            Ok(i32::try_from(started.total).unwrap_or(i32::MAX))
        }
        Some("cancel") => {
            let progress = pregen.cancel().await.map_err(fail)?;
            ctx.success(
                text(format!(
                    "Cancelling chunk generation job {} after the chunks in flight",
                    progress.id
                )),
                true,
            )
            .await;
            Ok(1)
        }
        Some("status") => {
            let status = pregen.status();
            report(ctx, &status);
            Ok(i32::from(status.running.is_some()))
        }
        _ => Err(CommandError::simple("command.unknown.command")),
    }
}

/// The phase of a running job with the counts of each phase.
pub fn phases(progress: &Progress) -> String {
    match (progress.regenerate, progress.phase) {
        (false, _) => format!(
            "Phase: generating, {} / {} done, {} failed",
            progress.done, progress.total, progress.failed
        ),
        (true, Phase::Removing) => format!(
            "Phase 1 of 2: removing, {} / {} removed, {} failed; then generating",
            progress.done, progress.total, progress.failed
        ),
        (true, Phase::Generating) => format!(
            "Phase 2 of 2: generating, {} / {} done, {} failed; removed {} / {} first, {} failed",
            progress.done,
            progress.total,
            progress.failed,
            progress.removed,
            progress.total,
            progress.remove_failed
        ),
    }
}

/// What the chunks in flight wait on, oldest first.
pub fn waiting(progress: &Progress) -> String {
    if progress.in_flight == 0 {
        return "Waiting on no chunks".to_owned();
    }
    let oldest: Vec<String> = progress
        .waiting
        .iter()
        .map(|w| {
            format!(
                "{} {} on {} for {}",
                w.x,
                w.z,
                w.on.name(),
                duration(w.secs)
            )
        })
        .collect();
    format!(
        "Waiting on {} chunks in flight; oldest: {}",
        progress.in_flight,
        oldest.join(", ")
    )
}

fn report(ctx: &Context<'_>, status: &Status) {
    match &status.running {
        Some(progress) => {
            ctx.reply(text(format!(
                "Job {} by {} in {}: {}, {} failed",
                progress.id,
                progress.by,
                progress.dimension,
                describe(progress),
                progress.failed
            )));
            ctx.reply(text(phases(progress)));
            ctx.reply(text(waiting(progress)));
        }
        None => ctx.reply(text("No chunk generation job is running")),
    }
    if let Some(last) = &status.last {
        let how = match (&last.error, last.cancelled) {
            (Some(error), _) => format!("stopped: {error}"),
            (None, true) => "cancelled".to_owned(),
            (None, false) => "finished".to_owned(),
        };
        let removal = if last.remove_failed > 0 {
            format!(", {} could not be removed", last.remove_failed)
        } else {
            String::new()
        };
        ctx.reply(text(format!(
            "Last job {} {how}: {} of {} chunks generated, {} failed{removal}, in {}",
            last.id,
            last.generated,
            last.total,
            last.failed,
            duration(last.duration_ms / 1000)
        )));
    }
}

/// The boss bar's UUID, apart from any other bar.
pub const BAR_UUID: u128 = 0x5C3B_9E41_7A2D_4F08_B1C6_2E9D_83F4_0A17;
const BAR_COLOR_BLUE: i32 = 1;
const BAR_OVERLAY_PROGRESS: i32 = 0;
const RECHECK_TICKS: u64 = 20;

/// Shows the running job's progress to players holding the node.
pub struct ProgressBar;

#[derive(Default)]
struct Shown {
    status: Option<tokio::sync::watch::Receiver<Status>>,
    shown: Option<(String, f32)>,
}

impl SessionHooks for ProgressBar {
    fn tick<'a>(&'a self, session: &'a mut PlayerSession, phase: SessionTick) -> HookFuture<'a> {
        if phase == SessionTick::Late {
            update_bar(session);
        }
        Box::pin(async {})
    }
}

fn update_bar(session: &mut PlayerSession) {
    let mut bar = session.ext.remove::<Shown>().unwrap_or_default();
    let first = bar.status.is_none();
    let status = bar
        .status
        .get_or_insert_with(|| session.shared().pregen.watch());
    let changed = status.has_changed().unwrap_or(false);
    if changed || first || session.tick.is_multiple_of(RECHECK_TICKS) {
        let running = status.borrow_and_update().running.clone();
        let shown = running.filter(|_| {
            session
                .shared()
                .permissions
                .allows(session.permission_subject(), CHUNK_NODE)
        });
        let want = shown.map(|p| (describe(&p), p.fraction()));
        match (&bar.shown, &want) {
            (None, Some((title, progress))) => {
                let title = TextComponent::new(title.clone());
                session.send(&BossEvent {
                    uuid: BAR_UUID,
                    action: BossAction::Add {
                        title: &title,
                        progress: *progress,
                        color: BAR_COLOR_BLUE,
                        overlay: BAR_OVERLAY_PROGRESS,
                        flags: 0,
                    },
                });
            }
            (Some(_), None) => session.send(&BossEvent {
                uuid: BAR_UUID,
                action: BossAction::Remove,
            }),
            (Some((old_title, old_progress)), Some((title, progress))) => {
                if old_title != title {
                    let title = TextComponent::new(title.clone());
                    session.send(&BossEvent {
                        uuid: BAR_UUID,
                        action: BossAction::Title(&title),
                    });
                }
                if old_progress != progress {
                    session.send(&BossEvent {
                        uuid: BAR_UUID,
                        action: BossAction::Progress(*progress),
                    });
                }
            }
            (None, None) => {}
        }
        bar.shown = want;
    }
    *session.ext.entry::<Shown>() = bar;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(regenerate: bool, phase: Phase) -> Progress {
        Progress {
            id: 1,
            dimension: "minecraft:overworld".to_owned(),
            x: 0,
            z: 0,
            radius: 100,
            shape: "square",
            regenerate,
            by: "Server".to_owned(),
            phase,
            done: 40,
            total: 169,
            failed: 1,
            removed: 169,
            remove_failed: 2,
            in_flight: 9,
            waiting: vec![tuff_core::world::pregen::InFlight {
                x: 20,
                z: 8,
                on: tuff_core::world::pregen::WaitingOn::Generation,
                secs: 75,
            }],
            rate: 0.0,
            eta_secs: None,
            started_unix: 0,
            cancelling: false,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn status_names_the_phase_with_its_counts() {
        assert_eq!(
            phases(&progress(false, Phase::Generating)),
            "Phase: generating, 40 / 169 done, 1 failed"
        );
        assert_eq!(
            phases(&progress(true, Phase::Removing)),
            "Phase 1 of 2: removing, 40 / 169 removed, 1 failed; then generating"
        );
        assert_eq!(
            phases(&progress(true, Phase::Generating)),
            "Phase 2 of 2: generating, 40 / 169 done, 1 failed; removed 169 / 169 first, 2 failed"
        );
        assert_eq!(
            waiting(&progress(true, Phase::Generating)),
            "Waiting on 9 chunks in flight; oldest: 20 8 on generation for 1m 15s"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn durations_leave_out_leading_zero_units() {
        assert_eq!(duration(0), "0s");
        assert_eq!(duration(59), "59s");
        assert_eq!(duration(61), "1m 1s");
        assert_eq!(duration(3600), "1h 0m 0s");
        assert_eq!(duration(3725), "1h 2m 5s");
    }
}
