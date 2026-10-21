//! `/tick`: target tick rate, freeze/unfreeze, step and sprint. The
//! coordinator owns the tick loop, so every subcommand just tells it what
//! to do; this module holds the parsing and the vanilla feedback text.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;
use tuff_core::command::{CommandError, Value};
use tuff_core::world::Shared;
use tuff_core::world::commands::console::CONSOLE_NAME;
use tuff_core::world::commands::feedback;
use tuff_core::world::messages::{CoordinatorMessage, FeedbackTarget, TickOutcome};
use tuff_core::world::tick_rate::{Samples, SprintReport};
use tuff_item::text::TextComponent;

use super::{Context, Executor, executor_future, text};

pub use tuff_core::world::tick_rate::TickAction;

pub struct Tick;

impl Executor for Tick {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let literals: Vec<&str> = ctx.literals();
            let action = match literals.as_slice() {
                [.., "query"] => TickAction::Query,
                [.., "rate", "rate"] => match ctx.arg("rate") {
                    Some(Value::Float(rate)) => TickAction::Rate(*rate),
                    _ => return Err(CommandError::simple("command.unknown.argument")),
                },
                [.., "freeze"] => TickAction::Freeze,
                [.., "unfreeze"] => TickAction::Unfreeze,
                [.., "step", "stop"] => TickAction::StepStop,
                [.., "step", "time"] => match ctx.arg("time") {
                    Some(Value::Time(ticks)) => TickAction::Step(*ticks as u32),
                    _ => return Err(CommandError::simple("command.unknown.argument")),
                },
                [.., "step"] => TickAction::Step(1),
                [.., "sprint", "stop"] => TickAction::SprintStop,
                [.., "sprint", "time"] => match ctx.arg("time") {
                    Some(Value::Time(ticks)) => TickAction::Sprint(*ticks as u32),
                    _ => return Err(CommandError::simple("command.unknown.argument")),
                },
                _ => return Err(CommandError::simple("command.unknown.command")),
            };
            let sprint_report = if let TickAction::Sprint(_) = action {
                let (report, finished) = oneshot::channel();
                tokio::spawn(deliver_sprint_report(ctx.shared().clone(), finished));
                Some(report)
            } else {
                None
            };
            let Some(outcome) = ctx
                .ask(|reply| CoordinatorMessage::Tick {
                    action,
                    sprint_report,
                    reply,
                })
                .await
            else {
                return Ok(1);
            };
            match outcome {
                TickOutcome::Query {
                    state,
                    sprinting,
                    average,
                    samples,
                } => {
                    let lines = query_lines(
                        state.rate,
                        state.frozen,
                        sprinting,
                        average,
                        state.period(),
                        &samples,
                    );
                    for line in lines {
                        ctx.success(line, false).await;
                    }
                }
                TickOutcome::Rate(rate) => ctx.success(rate_success(rate), true).await,
                TickOutcome::Frozen(frozen) => ctx.success(frozen_status(frozen), true).await,
                TickOutcome::Stepping(ticks) => ctx.success(step_success(ticks), true).await,
                TickOutcome::NotFrozen => ctx.fail(step_fail()),
                TickOutcome::StepStopped => ctx.success(step_stop_success(), true).await,
                TickOutcome::NotStepping => ctx.fail(step_stop_fail()),
                TickOutcome::Sprinting { interrupted } => {
                    if interrupted {
                        ctx.success(sprint_stop_success(), true).await;
                    }
                    ctx.success(sprinting_status(), true).await;
                }
                TickOutcome::SprintStopped => ctx.success(sprint_stop_success(), true).await,
                TickOutcome::NotSprinting => ctx.fail(sprint_stop_fail()),
            }
            Ok(1)
        })
    }
}

/// Waits for a sprint to end and reports it to the console and operators,
/// whoever started it. Nothing is said if another sprint replaced it.
async fn deliver_sprint_report(shared: Arc<Shared>, finished: oneshot::Receiver<SprintReport>) {
    let Ok(report) = finished.await else {
        return;
    };
    feedback::success(
        &shared,
        &FeedbackTarget::Console,
        TextComponent::new(CONSOLE_NAME),
        sprint_report(report.ticks_per_second, report.ms_per_tick),
        true,
    )
    .await;
}

fn fmt1(value: f64) -> String {
    format!("{value:.1}")
}

fn fmt2(value: f64) -> String {
    format!("{value:.2}")
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn status(key: &str) -> TextComponent {
    TextComponent::translate(key, [])
}

/// `/tick query`'s lines, in the order the game sends them.
pub fn query_lines(
    rate: f32,
    frozen: bool,
    sprinting: bool,
    average: Duration,
    target: Duration,
    samples: &Samples,
) -> Vec<TextComponent> {
    let mut lines = Vec::with_capacity(3);
    if sprinting {
        lines.push(status("commands.tick.status.sprinting"));
        lines.push(TextComponent::translate(
            "commands.tick.query.rate.sprinting",
            [text(fmt1(rate.into())), text(fmt1(ms(average)))],
        ));
    } else {
        let status_key = if frozen {
            "commands.tick.status.frozen"
        } else if target < average {
            "commands.tick.status.lagging"
        } else {
            "commands.tick.status.running"
        };
        lines.push(status(status_key));
        lines.push(TextComponent::translate(
            "commands.tick.query.rate.running",
            [
                text(fmt1(rate.into())),
                text(fmt1(ms(average))),
                text(fmt1(ms(target))),
            ],
        ));
    }
    let (p50, p95, p99, count) = samples.percentiles();
    lines.push(TextComponent::translate(
        "commands.tick.query.percentiles",
        [
            text(fmt1(ms(p50))),
            text(fmt1(ms(p95))),
            text(fmt1(ms(p99))),
            text(count),
        ],
    ));
    lines
}

pub fn rate_success(rate: f32) -> TextComponent {
    TextComponent::translate("commands.tick.rate.success", [text(fmt1(rate.into()))])
}

pub fn frozen_status(frozen: bool) -> TextComponent {
    status(if frozen {
        "commands.tick.status.frozen"
    } else {
        "commands.tick.status.running"
    })
}

pub fn sprinting_status() -> TextComponent {
    status("commands.tick.status.sprinting")
}

pub fn sprint_report(ticks_per_second: i32, ms_per_tick: f64) -> TextComponent {
    TextComponent::translate(
        "commands.tick.sprint.report",
        [text(ticks_per_second), text(fmt2(ms_per_tick))],
    )
}

pub fn sprint_stop_success() -> TextComponent {
    status("commands.tick.sprint.stop.success")
}

pub fn sprint_stop_fail() -> TextComponent {
    status("commands.tick.sprint.stop.fail")
}

pub fn step_success(ticks: u32) -> TextComponent {
    TextComponent::translate("commands.tick.step.success", [text(ticks)])
}

pub fn step_fail() -> TextComponent {
    status("commands.tick.step.fail")
}

pub fn step_stop_success() -> TextComponent {
    status("commands.tick.step.stop.success")
}

pub fn step_stop_fail() -> TextComponent {
    status("commands.tick.step.stop.fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn query_lines_pick_the_right_status() {
        let samples = Samples::default();
        let running = query_lines(
            20.0,
            false,
            false,
            Duration::from_millis(10),
            Duration::from_millis(50),
            &samples,
        );
        assert_eq!(running[0].text, "");
        assert_eq!(
            running[0].translate.as_deref(),
            Some("commands.tick.status.running")
        );

        let lagging = query_lines(
            20.0,
            false,
            false,
            Duration::from_millis(80),
            Duration::from_millis(50),
            &samples,
        );
        assert_eq!(
            lagging[0].translate.as_deref(),
            Some("commands.tick.status.lagging")
        );

        let frozen = query_lines(
            20.0,
            true,
            false,
            Duration::from_millis(80),
            Duration::from_millis(50),
            &samples,
        );
        assert_eq!(
            frozen[0].translate.as_deref(),
            Some("commands.tick.status.frozen")
        );

        let sprinting = query_lines(
            20.0,
            true,
            true,
            Duration::from_millis(5),
            Duration::from_millis(50),
            &samples,
        );
        assert_eq!(
            sprinting[0].translate.as_deref(),
            Some("commands.tick.status.sprinting")
        );
        assert_eq!(sprinting.len(), 3);
    }
}
