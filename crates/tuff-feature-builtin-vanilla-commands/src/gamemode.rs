//! `/gamemode`.

use tuff_core::command::{CommandError, Value};
use tuff_core::world::messages::{CoordinatorMessage, Directive};
use tuff_item::text::TextComponent;

use super::feedback::Messages;
use super::targets;
use super::{Context, Executor, executor_future, text};

pub use tuff_core::world::commands::gamemode::{changed, mode_name, set_other, set_self};

pub struct DefaultGamemode;

impl Executor for DefaultGamemode {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::GameMode(mode)) = ctx.arg("gamemode").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            ctx.ask(|reply| CoordinatorMessage::DefaultGameMode { mode, reply })
                .await;
            let mut changed = 0;
            if ctx
                .shared()
                .force_game_mode
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                let everyone = targets::players(ctx).await;
                let replies =
                    targets::dispatch_wait(ctx, &everyone, |_| Directive::GameMode(mode)).await;
                changed = replies.iter().filter(|reply| reply.affected).count() as i32;
            }
            ctx.success(
                TextComponent::translate("commands.defaultgamemode.success", [mode_name(mode)]),
                true,
            )
            .await;
            Ok(changed)
        })
    }
}

pub struct Gamemode;

impl Executor for Gamemode {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::GameMode(mode)) = ctx.arg("gamemode").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let players = match ctx.arg("target").cloned() {
                Some(Value::Selector(selector)) => targets::select_players(ctx, &selector).await?,
                _ => vec![targets::source_player(ctx).await?],
            };
            let source_id = ctx.source.player_id();
            let messages = Messages::new(
                move |reply| {
                    if Some(reply.player) == source_id {
                        set_self(mode)
                    } else {
                        set_other(&reply.name, mode)
                    }
                },
                move |_, _| TextComponent::new(""),
            )
            .silent();
            // The game mode change itself tells each player; the source
            // only hears about others through the reply below.
            let own = players.iter().find(|p| Some(p.id) == source_id).is_some();
            let others: Vec<_> = players
                .iter()
                .filter(|p| Some(p.id) != source_id)
                .cloned()
                .collect();
            let other_messages = Messages::new(
                move |reply| set_other(&reply.name, mode),
                move |count, _| {
                    TextComponent::translate(
                        "commands.gamemode.success.other",
                        [text(count), mode_name(mode)],
                    )
                },
            )
            .quiet();
            if own {
                let own_target: Vec<_> = players
                    .iter()
                    .filter(|p| Some(p.id) == source_id)
                    .cloned()
                    .collect();
                targets::dispatch(ctx, &own_target, |_| Directive::GameMode(mode), messages).await;
            }
            if !others.is_empty() {
                targets::dispatch(ctx, &others, |_| Directive::GameMode(mode), other_messages)
                    .await;
            }
            Ok(players.len() as i32)
        })
    }
}
