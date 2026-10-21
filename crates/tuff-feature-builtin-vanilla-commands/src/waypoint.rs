//! `/waypoint`: list the locator bar's waypoints and restyle a player's.

use tuff_core::command::CommandError;
use tuff_core::command::args::Value;
use tuff_core::world::messages::{Directive, WaypointEdit};
use tuff_item::text::TextComponent;

use super::targets::{self, Target};
use super::{Context, Executor, executor_future};

pub struct WaypointCommand;

fn invalid() -> CommandError {
    CommandError::simple("command.unknown.argument")
}

async fn list(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let dimension = ctx.source.dimension;
    let mut names: Vec<String> = targets::players(ctx)
        .await
        .into_iter()
        .filter(|p| p.state.dimension == dimension && p.state.waypoint_transmit_range > 0.0)
        .map(|p| p.name)
        .collect();
    names.sort();
    let count = names.len() as i32;
    let message = if names.is_empty() {
        TextComponent::translate(
            "commands.waypoint.list.empty",
            [TextComponent::new(ctx.dimension_key())],
        )
    } else {
        TextComponent::translate(
            "commands.waypoint.list.success",
            [
                TextComponent::new(count.to_string()),
                TextComponent::new(ctx.dimension_key()),
                TextComponent::new(names.join(", ")),
            ],
        )
    };
    ctx.success(message, false).await;
    Ok(count)
}

fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn change(ctx: &Context<'_>) -> Result<(WaypointEdit, TextComponent), CommandError> {
    let names = ctx.literals();
    let color_arg = ctx.arg("color");
    match (names.get(3).copied(), names.get(4).copied()) {
        (Some("color"), Some("reset")) => Ok((
            WaypointEdit::Color(None),
            TextComponent::translate("commands.waypoint.modify.color.reset", []),
        )),
        (Some("color"), Some("hex")) => match color_arg {
            Some(Value::HexColor(rgb)) => {
                let [_, r, g, b] = rgb.to_be_bytes();
                let rgb = [r, g, b];
                Ok((
                    WaypointEdit::Color(Some(rgb)),
                    TextComponent::translate(
                        "commands.waypoint.modify.color",
                        [TextComponent::new(hex(rgb).trim_start_matches('#')).color(hex(rgb))],
                    ),
                ))
            }
            _ => Err(invalid()),
        },
        (Some("color"), Some("color")) => match color_arg {
            Some(Value::TeamColor(color)) => Ok((
                WaypointEdit::Color(Some(color.rgb())),
                TextComponent::translate(
                    "commands.waypoint.modify.color",
                    [TextComponent::new(color.name()).color(color.name())],
                ),
            )),
            _ => Err(invalid()),
        },
        (Some("style"), Some("reset")) => Ok((
            WaypointEdit::Style(None),
            TextComponent::translate("commands.waypoint.modify.style", []),
        )),
        (Some("style"), Some("set")) => match ctx.arg("style") {
            Some(Value::Identifier(style) | Value::Resource(style)) => Ok((
                WaypointEdit::Style(Some(style.clone())),
                TextComponent::translate("commands.waypoint.modify.style", []),
            )),
            _ => Err(invalid()),
        },
        _ => Err(invalid()),
    }
}

async fn modify(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let Some(Value::Selector(selector)) = ctx.arg("waypoint").cloned() else {
        return Err(invalid());
    };
    let target = targets::select_entity(ctx, &selector).await?;
    let Target::Player(player) = target else {
        return Err(CommandError::simple("argument.entity.notfound.player"));
    };
    let (edit, message) = change(ctx)?;
    targets::dispatch_wait(ctx, std::slice::from_ref(&player), |_| {
        Directive::Waypoint(edit.clone())
    })
    .await;
    ctx.success(message, true).await;
    Ok(1)
}

impl Executor for WaypointCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            match ctx.literals().get(1).copied() {
                Some("list") => list(ctx).await,
                Some("modify") => modify(ctx).await,
                _ => Err(invalid()),
            }
        })
    }
}
