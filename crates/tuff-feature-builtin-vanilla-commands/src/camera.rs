//! `/spectate`.

use tuff_core::command::CommandError;
use tuff_core::command::args::Value;
use tuff_core::world::messages::{DialogRef, Directive, PostEffectOp};
use tuff_item::text::TextComponent;
use tuff_vanilla_data::registry::EntityType;

use super::targets::{self, Target};
use super::{Context, Executor, executor_future, text};

pub struct Spectate;

impl Executor for Spectate {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let target = match ctx.arg("target").cloned() {
                Some(Value::Selector(selector)) => {
                    Some(targets::select_entity(ctx, &selector).await?)
                }
                _ => None,
            };
            let player = match ctx.arg("player").cloned() {
                Some(Value::Selector(selector)) => targets::select_player(ctx, &selector).await?,
                _ => targets::source_player(ctx).await?,
            };
            let id = target.as_ref().map(Target::id);
            let name = target
                .as_ref()
                .map_or_else(|| TextComponent::new(""), |target| ctx.name_of(target));
            let untrackable = matches!(&target, Some(Target::Entity(entity)) if entity.kind == EntityType::Marker);
            let replies = targets::dispatch_wait(ctx, std::slice::from_ref(&player), |_| {
                Directive::Spectate {
                    target: id,
                    target_name: name.clone(),
                    untrackable,
                }
            })
            .await;
            if let Some(error) = replies.iter().find_map(|reply| reply.error.clone()) {
                return Err(CommandError::text(error));
            }
            let message = match target {
                Some(_) => TextComponent::translate("commands.spectate.success.started", [name]),
                None => TextComponent::translate("commands.spectate.success.stopped", []),
            };
            ctx.success(message, false).await;
            Ok(1)
        })
    }
}

pub struct PostEffect;

impl Executor for PostEffect {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let action = ctx.literals().get(1).copied().unwrap_or_default();
            let op = match action {
                "add" => PostEffectOp::Add,
                "remove" => PostEffectOp::Remove,
                "clear" => PostEffectOp::Clear,
                _ => PostEffectOp::List,
            };
            let value = match ctx.arg("posteffect") {
                Some(Value::Identifier(id)) => Some(id.clone()),
                _ => None,
            };
            let selector = match ctx.arg(if op == PostEffectOp::List {
                "target"
            } else {
                "targets"
            }) {
                Some(Value::Selector(selector)) => selector.clone(),
                _ => return Err(CommandError::simple("command.unknown.argument")),
            };
            let players = targets::select_players(ctx, &selector).await?;
            let replies = targets::dispatch_wait(ctx, &players, |_| Directive::PostEffect {
                op,
                value: value.clone(),
            })
            .await;
            if op == PostEffectOp::List {
                let reply = replies
                    .first()
                    .ok_or_else(|| CommandError::simple("argument.entity.notfound.player"))?;
                let name = text(&reply.name);
                let message = if reply.post_effects.is_empty() {
                    TextComponent::translate("commands.posteffect.list.empty", [name])
                } else {
                    TextComponent::translate(
                        "commands.posteffect.list.success",
                        [
                            name,
                            text(reply.post_effects.len()),
                            text(reply.post_effects.join(", ")),
                        ],
                    )
                };
                ctx.success(message, false).await;
                return Ok(reply.post_effects.len() as i32);
            }
            let changed: Vec<&_> = replies.iter().filter(|reply| reply.affected).collect();
            let id = value.as_deref().unwrap_or_default();
            let (single, multiple, failure) = match op {
                PostEffectOp::Add => (
                    "commands.posteffect.add.success.single",
                    "commands.posteffect.add.success.multiple",
                    "commands.posteffect.add.failed",
                ),
                PostEffectOp::Remove => (
                    "commands.posteffect.remove.success.single",
                    "commands.posteffect.remove.success.multiple",
                    "commands.posteffect.remove.failed",
                ),
                _ => (
                    "commands.posteffect.clear.success.single",
                    "commands.posteffect.clear.success.multiple",
                    "commands.posteffect.clear.failed",
                ),
            };
            let with_id = op != PostEffectOp::Clear;
            let message = match changed.as_slice() {
                [] => return Err(CommandError::simple(failure)),
                [only] => {
                    let mut args = Vec::new();
                    if with_id {
                        args.push(text(id));
                    }
                    args.push(text(&only.name));
                    TextComponent::translate(single, args)
                }
                many => {
                    let mut args = Vec::new();
                    if with_id {
                        args.push(text(id));
                    }
                    args.push(text(many.len()));
                    TextComponent::translate(multiple, args)
                }
            };
            ctx.success(message, true).await;
            Ok(changed.len() as i32)
        })
    }
}

pub struct Dialog;

fn dialog_ref(ctx: &Context<'_>) -> Result<DialogRef, CommandError> {
    match ctx.arg("dialog") {
        Some(Value::Identifier(id)) => {
            let entries =
                tuff_core::command::args::registry_entries("minecraft:dialog").unwrap_or(&[]);
            entries
                .binary_search(&id.as_str())
                .map(|index| DialogRef::Registry(index as i32))
                .map_err(|_| {
                    CommandError::dynamic(
                        "argument.resource_or_id.no_such_element",
                        [id.clone(), "minecraft:dialog".to_owned()],
                    )
                })
        }
        Some(Value::Nbt(tuff_nbt::Tag::Compound(inline))) => Ok(DialogRef::Inline(inline.clone())),
        _ => Err(CommandError::simple("command.unknown.argument")),
    }
}

impl Executor for Dialog {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let show = ctx.literals().get(1).copied() == Some("show");
            let selector = match ctx.arg("targets") {
                Some(Value::Selector(selector)) => selector.clone(),
                _ => return Err(CommandError::simple("command.unknown.argument")),
            };
            let dialog = if show { Some(dialog_ref(ctx)?) } else { None };
            let players = targets::select_players(ctx, &selector).await?;
            let replies = targets::dispatch_wait(ctx, &players, |_| match &dialog {
                Some(dialog) => Directive::ShowDialog {
                    dialog: dialog.clone(),
                },
                None => Directive::ClearDialog,
            })
            .await;
            let kind = if show { "show" } else { "clear" };
            let message = match replies.as_slice() {
                [reply] => TextComponent::translate(
                    format!("commands.dialog.{kind}.single"),
                    [text(&reply.name)],
                ),
                _ => TextComponent::translate(
                    format!("commands.dialog.{kind}.multiple"),
                    [text(replies.len())],
                ),
            };
            ctx.success(message, true).await;
            Ok(replies.len() as i32)
        })
    }
}
