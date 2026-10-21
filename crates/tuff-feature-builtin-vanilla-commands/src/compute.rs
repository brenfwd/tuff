use tuff_core::command::{CommandError, Value};
use tuff_item::text::TextComponent;

use super::data::compute_value;
use super::{Context, Executor, executor_future, text};

pub struct Compute;

fn scaled(original: f32, scale: f32) -> Option<i32> {
    original
        .is_finite()
        .then(|| (original * scale).floor() as i32)
}

impl Executor for Compute {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            if ctx.literals().contains(&"integer") {
                let result = compute_value(ctx, false).await? as i32;
                exact(ctx, result).await;
                return Ok(result);
            }
            let scale = match ctx.arg("scale") {
                Some(Value::Float(scale)) => *scale,
                _ => 1.0,
            };
            let original = compute_value(ctx, true).await? as f32;
            let Some(result) = scaled(original, scale) else {
                return Err(CommandError::dynamic(
                    "command.compute.result.unnamed.invalid",
                    [format!("{original:?}")],
                ));
            };
            if result as f32 == original {
                exact(ctx, result).await;
            } else {
                ctx.success(
                    TextComponent::translate(
                        "command.compute.result.unnamed.rounded",
                        [text(format!("{original:?}")), text(result)],
                    ),
                    false,
                )
                .await;
            }
            Ok(result)
        })
    }
}

async fn exact(ctx: &mut Context<'_>, result: i32) {
    ctx.success(
        TextComponent::translate("command.compute.result.unnamed.exact", [text(result)]),
        false,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::scaled;

    #[tuff_core::watchdog]
    #[test]
    fn scaled_floors_and_rejects_non_finite() {
        assert_eq!(scaled(2.5, 1.0), Some(2));
        assert_eq!(scaled(-0.5, 1.0), Some(-1));
        assert_eq!(scaled(1.5, 2.0), Some(3));
        assert_eq!(scaled(f32::NAN, 1.0), None);
        assert_eq!(scaled(f32::INFINITY, 1.0), None);
    }
}
