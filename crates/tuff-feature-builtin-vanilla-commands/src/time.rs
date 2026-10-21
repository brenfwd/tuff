//! `/time`: world clocks, timelines and time markers.

use tuff_vanilla_data::data_registry::{TIMELINE, WORLD_CLOCK};
use tuff_vanilla_data::dimension_type::DimensionType;
use tuff_vanilla_data::timeline::Timeline;

use tuff_core::command::args::matches_resource;
use tuff_core::command::{CommandError, Value, java_float};
use tuff_core::world::clock::{self, ClockId};
use tuff_core::world::messages::{ClockOutcome, CoordinatorMessage};
use tuff_item::text::TextComponent;

use super::{Context, Executor, executor_future, text};

pub use tuff_core::world::clock::ClockAction;

fn error(key: &str, args: impl IntoIterator<Item = TextComponent>) -> CommandError {
    CommandError::text(TextComponent::translate(key, args))
}

/// The clock `/time` uses in `dimension` when none is named.
pub fn default_clock(dimension: &str) -> Result<ClockId, CommandError> {
    DimensionType::get(dimension)
        .and_then(|dimension| dimension.default_clock)
        .and_then(clock::clock_id)
        .ok_or_else(|| error("commands.time.no_default_clock", [text(dimension)]))
}

/// The feedback for what a clock subcommand did: the line and whether it
/// changed the clock (and so is broadcast), or the failure.
fn render(outcome: ClockOutcome) -> Result<(TextComponent, bool), TextComponent> {
    let line = |key: &str, args: [TextComponent; 2]| TextComponent::translate(key, args);
    match outcome {
        ClockOutcome::Query { name, total_ticks } => Ok((
            line(
                "commands.time.query.absolute",
                [text(name), text(total_ticks)],
            ),
            false,
        )),
        ClockOutcome::QueryTimeline {
            timeline,
            value,
            repetitions,
        } => {
            let key = if repetitions {
                "commands.time.query.timeline.repetitions"
            } else {
                "commands.time.query.timeline"
            };
            Ok((line(key, [text(timeline), text(value)]), false))
        }
        ClockOutcome::WrongTimeline { timeline, name } => Err(line(
            "commands.time.wrong_timeline_for_clock",
            [text(timeline), text(name)],
        )),
        ClockOutcome::Set { name, total_ticks } => Ok((
            line(
                "commands.time.set.absolute",
                [text(name), text(total_ticks)],
            ),
            true,
        )),
        ClockOutcome::AlreadyAtTime { name, ticks } => Err(line(
            "commands.time.set.already_at_time",
            [text(name), text(ticks)],
        )),
        ClockOutcome::NoTimeMarker { marker, name } => Err(line(
            "commands.time.no_time_marker_found",
            [text(marker), text(name)],
        )),
        ClockOutcome::AlreadyAtMarker { name, marker } => Err(line(
            "commands.time.set.already_at_time_marker",
            [text(name), text(marker)],
        )),
        ClockOutcome::SetMarker { name, marker } => Ok((
            line("commands.time.set.time_marker", [text(name), text(marker)]),
            true,
        )),
        ClockOutcome::Paused { name, paused } => {
            let key = if paused {
                "commands.time.pause"
            } else {
                "commands.time.resume"
            };
            Ok((TextComponent::translate(key, [text(name)]), true))
        }
        ClockOutcome::AlreadyPaused { name, paused } => {
            let key = if paused {
                "commands.time.pause.already_paused"
            } else {
                "commands.time.pause.already_running"
            };
            Err(TextComponent::translate(key, [text(name)]))
        }
        ClockOutcome::Rate { name, rate } => Ok((
            line("commands.time.rate", [text(name), text(java_float(rate))]),
            true,
        )),
        ClockOutcome::AlreadyRate { name, rate } => Err(line(
            "commands.time.rate.already_same",
            [text(name), text(java_float(rate))],
        )),
    }
}

pub fn game_time(ticks: u64) -> TextComponent {
    TextComponent::translate("commands.time.query.gametime", [text(ticks)])
}

/// Completions for `/time set` markers and `/time query` timelines.
pub fn suggest(
    path: &[tuff_core::command::ParsedNode],
    partial: &str,
    dimension: &str,
) -> Vec<String> {
    let literals: Vec<&str> = path.iter().map(|n| n.node.name).collect();
    let clock = match literals.as_slice() {
        ["time", "of", "clock", ..] => match tuff_core::command::Parsed::value(path, "clock") {
            Some(Value::Resource(name)) => clock::clock_id(name),
            _ => None,
        },
        _ => default_clock(dimension).ok(),
    };
    let Some(clock) = clock else {
        return Vec::new();
    };
    let candidates: Vec<&str> = match literals.last() {
        Some(&"set") => clock::command_markers(clock).collect(),
        Some(&"query") => clock::timelines_of(clock::clock_name(clock))
            .map(|timeline| timeline.name)
            .collect(),
        _ => Vec::new(),
    };
    candidates
        .into_iter()
        .filter(|id| matches_resource(&partial.to_lowercase(), id))
        .map(str::to_owned)
        .collect()
}

/// `/time`, run by the coordinator, which owns the clocks.
pub struct Time;

impl Executor for Time {
    fn suggest(
        &self,
        path: &[tuff_core::command::ParsedNode],
        partial: &str,
        input: &super::SuggestInput,
    ) -> Option<Vec<String>> {
        Some(suggest(path, partial, input.dimension))
    }

    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let literals: Vec<&str> = ctx.literals();
            let clock = match ctx.arg("clock") {
                Some(Value::Resource(name)) => clock::clock_id(name).ok_or_else(|| {
                    error(
                        "argument.resource.not_found",
                        [text(name), text(WORLD_CLOCK.key)],
                    )
                })?,
                _ => default_clock(ctx.dimension_key())?,
            };
            let ticks = |ctx: &Context<'_>| match ctx.arg("time") {
                Some(Value::Time(ticks)) => *ticks,
                _ => 0,
            };
            let action = match literals.as_slice() {
                [.., "query", "gametime"] => {
                    let time = *ctx.shared().clock.borrow();
                    ctx.success(game_time(time), false).await;
                    return Ok(time as i32);
                }
                [.., "query", "time"] => ClockAction::Query,
                [.., "query", "timeline"] | [.., "query", "timeline", "repetition"] => {
                    let name = match ctx.arg("timeline") {
                        Some(Value::Resource(name)) => name.clone(),
                        _ => String::new(),
                    };
                    let timeline = Timeline::get(&name).ok_or_else(|| {
                        error(
                            "argument.resource.not_found",
                            [text(&name), text(TIMELINE.key)],
                        )
                    })?;
                    ClockAction::QueryTimeline {
                        timeline,
                        repetitions: literals.last() == Some(&"repetition"),
                    }
                }
                [.., "add", "time"] => ClockAction::Add(ticks(ctx)),
                [.., "set", "time"] => ClockAction::Set(ticks(ctx)),
                [.., "set", "timemarker"] => match ctx.arg("timemarker") {
                    Some(Value::Identifier(marker)) => ClockAction::SetMarker(marker.clone()),
                    _ => return Err(CommandError::simple("command.unknown.argument")),
                },
                [.., "pause"] => ClockAction::Pause,
                [.., "resume"] => ClockAction::Resume,
                [.., "rate", "rate"] => match ctx.arg("rate") {
                    Some(Value::Float(rate)) => ClockAction::Rate(*rate),
                    _ => return Err(CommandError::simple("command.unknown.argument")),
                },
                _ => return Err(CommandError::simple("command.unknown.command")),
            };
            if let Some(outcome) = ctx
                .ask(|reply| CoordinatorMessage::Clock {
                    clock,
                    action,
                    reply,
                })
                .await
            {
                match render(outcome) {
                    Ok((message, broadcast)) => ctx.success(message, broadcast).await,
                    Err(message) => ctx.fail(message),
                }
            }
            Ok(1)
        })
    }
}
