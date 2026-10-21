//! `/worldborder`: the source dimension's border, which the coordinator
//! owns, so every change is one atomic request there.

use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, java_fixed, java_fixed_f32};
use tuff_core::world::border::{BorderChange, BorderError, Resized};
use tuff_core::world::messages::CoordinatorMessage;
use tuff_item::text::TextComponent;

use super::{Context, Executor, executor_future, text};

pub struct WorldBorder;

fn seconds(ticks: i64) -> String {
    java_fixed(ticks as f64 / 20.0, 2)
}

fn refused(error: BorderError) -> CommandError {
    let args = match error {
        BorderError::TooBig => vec![text("5.9999968E7")],
        BorderError::TooFar => vec![text("2.9999984E7")],
        _ => Vec::new(),
    };
    CommandError {
        message: Box::new(TextComponent::translate(error.key(), args)),
        cursor: None,
    }
}

fn double(ctx: &Context<'_>, name: &str) -> f64 {
    match ctx.arg(name) {
        Some(Value::Double(value)) => *value,
        _ => 0.0,
    }
}

fn float(ctx: &Context<'_>, name: &str) -> f32 {
    match ctx.arg(name) {
        Some(Value::Float(value)) => *value,
        _ => 0.0,
    }
}

fn ticks(ctx: &Context<'_>) -> Option<i64> {
    match ctx.arg("time") {
        Some(Value::Time(ticks)) => Some(i64::from(*ticks)),
        _ => None,
    }
}

impl Executor for WorldBorder {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            if ctx.has("get") {
                let size = ctx.shared().dimension(ctx.source.dimension).border().size();
                ctx.success(
                    TextComponent::translate(
                        "commands.worldborder.get",
                        [text(java_fixed(size, 0))],
                    ),
                    false,
                )
                .await;
                return Ok((size + 0.5).floor() as i32);
            }
            let change = if ctx.has("set") {
                BorderChange::Set {
                    size: double(ctx, "distance"),
                    ticks: ticks(ctx).unwrap_or(0),
                }
            } else if ctx.has("add") {
                BorderChange::Add {
                    distance: double(ctx, "distance"),
                    ticks: ticks(ctx),
                }
            } else if ctx.has("center") {
                let Some(Value::Coordinates(pos)) = ctx.arg("pos") else {
                    return Err(CommandError::simple("command.unknown.command"));
                };
                let [x, _, z] = pos.position(&ctx.source.pose());
                BorderChange::Center([f64::from(x as f32), f64::from(z as f32)])
            } else if ctx.has("amount") {
                BorderChange::DamageAmount(f64::from(float(ctx, "damagePerBlock")))
            } else if ctx.has("buffer") {
                BorderChange::DamageBuffer(f64::from(float(ctx, "distance")))
            } else if ctx.has("warning") && ctx.has("distance") {
                let Some(Value::Int(blocks)) = ctx.arg("distance") else {
                    return Err(CommandError::simple("command.unknown.command"));
                };
                BorderChange::WarningDistance(*blocks)
            } else if ctx.has("warning") {
                BorderChange::WarningTime(ticks(ctx).unwrap_or(0) as i32)
            } else {
                return Err(CommandError::simple("command.unknown.command"));
            };
            let dimension = ctx.source.dimension;
            let Some(outcome) = ctx
                .ask(|reply| CoordinatorMessage::WorldBorder {
                    dimension,
                    change,
                    reply,
                })
                .await
            else {
                return Ok(0);
            };
            let resized = outcome.map_err(refused)?;
            let (message, result) = feedback(change, resized);
            ctx.success(message, true).await;
            Ok(result)
        })
    }
}

fn feedback(change: BorderChange, resized: Option<Resized>) -> (TextComponent, i32) {
    let translate = |key: &str, args: Vec<TextComponent>| TextComponent::translate(key, args);
    match (change, resized) {
        (BorderChange::Set { .. } | BorderChange::Add { .. }, Some(resized)) => {
            let size = text(java_fixed(resized.to, 1));
            let message = if resized.ticks <= 0 {
                translate("commands.worldborder.set.immediate", vec![size])
            } else if resized.to > resized.from {
                translate(
                    "commands.worldborder.set.grow",
                    vec![size, text(seconds(resized.ticks))],
                )
            } else {
                translate(
                    "commands.worldborder.set.shrink",
                    vec![size, text(seconds(resized.ticks))],
                )
            };
            (message, (resized.to - resized.from) as i32)
        }
        (BorderChange::Center([x, z]), _) => (
            translate(
                "commands.worldborder.center.success",
                vec![
                    text(java_fixed_f32(x as f32, 2)),
                    text(java_fixed_f32(z as f32, 2)),
                ],
            ),
            0,
        ),
        (BorderChange::DamageAmount(amount), _) => (
            translate(
                "commands.worldborder.damage.amount.success",
                vec![text(java_fixed_f32(amount as f32, 2))],
            ),
            amount as f32 as i32,
        ),
        (BorderChange::DamageBuffer(distance), _) => (
            translate(
                "commands.worldborder.damage.buffer.success",
                vec![text(java_fixed_f32(distance as f32, 2))],
            ),
            distance as f32 as i32,
        ),
        (BorderChange::WarningDistance(blocks), _) => (
            translate(
                "commands.worldborder.warning.distance.success",
                vec![text(blocks)],
            ),
            blocks,
        ),
        (BorderChange::WarningTime(ticks), _) => (
            translate(
                "commands.worldborder.warning.time.success",
                vec![text(seconds(i64::from(ticks)))],
            ),
            ticks,
        ),
        (BorderChange::Set { .. } | BorderChange::Add { .. }, None) => (TextComponent::new(""), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(message: &TextComponent) -> String {
        message.to_value()["translate"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    #[tuff_core::watchdog]
    #[test]
    fn feedback_uses_vanilla_keys_and_formats() {
        let (message, result) = feedback(
            BorderChange::Set {
                size: 10.0,
                ticks: 30,
            },
            Some(Resized {
                from: 100.0,
                to: 10.0,
                ticks: 30,
            }),
        );
        assert_eq!(key(&message), "commands.worldborder.set.shrink");
        assert_eq!(message.to_value()["with"][0]["text"], "10.0");
        assert_eq!(message.to_value()["with"][1]["text"], "1.50");
        assert_eq!(result, -90);
        let (message, result) = feedback(BorderChange::DamageAmount(0.5), None);
        assert_eq!(message.to_value()["with"][0]["text"], "0.50");
        assert_eq!(result, 0);
        let mut keys = vec![
            key(&feedback(BorderChange::WarningTime(40), None).0),
            key(&feedback(BorderChange::WarningDistance(4), None).0),
            key(&feedback(BorderChange::DamageBuffer(2.0), None).0),
            key(&feedback(BorderChange::Center([1.0, 2.0]), None).0),
            "commands.worldborder.get".to_owned(),
            "commands.worldborder.set.grow".to_owned(),
            "commands.worldborder.set.immediate".to_owned(),
        ];
        keys.push(key(&refused(BorderError::TooBig).message));
        for key in keys {
            assert!(tuff_vanilla_data::lang::lookup(&key).is_some(), "{key}");
        }
    }
}
