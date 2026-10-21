//! `/random` and `/stopwatch`.

use tuff_core::command::CommandError;
use tuff_core::command::args::Value;
use tuff_core::world::messages::CoordinatorMessage;
use tuff_item::text::TextComponent;

use super::{Context, Executor, executor_future, text};

fn identifier(ctx: &Context<'_>, name: &str) -> Result<String, CommandError> {
    match ctx.arg(name) {
        Some(Value::Identifier(id)) => Ok(id.clone()),
        _ => Err(CommandError::simple("command.unknown.argument")),
    }
}

fn flag(ctx: &Context<'_>, name: &str) -> Option<bool> {
    match ctx.arg(name) {
        Some(Value::Bool(value)) => Some(*value),
        _ => None,
    }
}

pub struct Random;

impl Random {
    async fn sample(ctx: &mut Context<'_>, announce: bool) -> Result<i32, CommandError> {
        let Some(Value::IntRange(range)) = ctx.arg("range").cloned() else {
            return Err(CommandError::simple("command.unknown.argument"));
        };
        let min = range.min.unwrap_or(i32::MIN);
        let max = range.max.unwrap_or(i32::MAX);
        let span = max as i64 - min as i64;
        if span == 0 {
            return Err(CommandError::simple(
                "commands.random.error.range_too_small",
            ));
        }
        if span < 0 || span >= i32::MAX as i64 {
            return Err(CommandError::simple(
                "commands.random.error.range_too_large",
            ));
        }
        let seed = ctx.shared().seed;
        let value = match ctx.arg("sequence") {
            Some(Value::Identifier(id)) => {
                let id = id.clone();
                ctx.shared().random_sequences.sample(seed, &id, min, max)
            }
            _ => {
                use rand::Rng;
                ctx.rng().gen_range(min..=max)
            }
        };
        if announce {
            let message = TextComponent::translate(
                "commands.random.roll",
                [ctx.source_display_name(), text(value), text(min), text(max)],
            );
            ctx.coordinator(CoordinatorMessage::Announce(message)).await;
        } else {
            ctx.success(
                TextComponent::translate("commands.random.sample.success", [text(value)]),
                false,
            )
            .await;
        }
        Ok(value)
    }
}

impl Executor for Random {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            match ctx.literals().get(1).copied() {
                Some("value") => Self::sample(ctx, false).await,
                Some("roll") => Self::sample(ctx, true).await,
                Some("reset") => {
                    let seed = ctx.shared().seed;
                    let salt = match ctx.arg("seed") {
                        Some(Value::Int(salt)) => Some(*salt),
                        _ => None,
                    };
                    let world = flag(ctx, "includeWorldSeed").unwrap_or(true);
                    let with_id = flag(ctx, "includeSequenceId").unwrap_or(true);
                    if ctx.has("*") {
                        let sequences = &ctx.shared().random_sequences;
                        if let Some(salt) = salt {
                            sequences.set_defaults(salt, world, with_id);
                        }
                        let count = sequences.clear();
                        ctx.success(
                            TextComponent::translate(
                                "commands.random.reset.all.success",
                                [text(count)],
                            ),
                            false,
                        )
                        .await;
                        return Ok(count as i32);
                    }
                    let id = identifier(ctx, "sequence")?;
                    ctx.shared().random_sequences.reset(
                        seed,
                        &id,
                        salt.map(|salt| (salt, world, with_id)),
                    );
                    ctx.success(
                        TextComponent::translate("commands.random.reset.success", [text(&id)]),
                        false,
                    )
                    .await;
                    Ok(1)
                }
                _ => Err(CommandError::simple("command.unknown.command")),
            }
        })
    }
}

pub struct Stopwatch;

fn missing(id: &str) -> CommandError {
    CommandError::dynamic("commands.stopwatch.does_not_exist", [text(id)])
}

impl Executor for Stopwatch {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let action = ctx.literals().get(1).copied();
            let id = identifier(ctx, "id")?;
            let stopwatches = &ctx.shared().stopwatches;
            let (key, result) = match action {
                Some("create") => {
                    if !stopwatches.create(&id) {
                        return Err(CommandError::dynamic(
                            "commands.stopwatch.already_exists",
                            [text(&id)],
                        ));
                    }
                    ("commands.stopwatch.create.success", 1)
                }
                Some("restart") => {
                    if !stopwatches.restart(&id) {
                        return Err(missing(&id));
                    }
                    ("commands.stopwatch.restart.success", 1)
                }
                Some("remove") => {
                    if !stopwatches.remove(&id) {
                        return Err(missing(&id));
                    }
                    ("commands.stopwatch.remove.success", 1)
                }
                Some("query") => {
                    let scale = match ctx.arg("scale") {
                        Some(Value::Double(scale)) => *scale,
                        _ => 1.0,
                    };
                    let elapsed = stopwatches
                        .elapsed_seconds(&id)
                        .ok_or_else(|| missing(&id))?;
                    ctx.success(
                        TextComponent::translate(
                            "commands.stopwatch.query",
                            [text(&id), text(elapsed)],
                        ),
                        true,
                    )
                    .await;
                    return Ok((elapsed * scale) as i32);
                }
                _ => return Err(CommandError::simple("command.unknown.command")),
            };
            ctx.success(TextComponent::translate(key, [text(&id)]), true)
                .await;
            Ok(result)
        })
    }
}
