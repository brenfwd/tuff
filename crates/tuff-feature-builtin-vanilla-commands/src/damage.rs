//! `/damage <target> <amount> [<type> [at <pos> | by <entity> [from <cause>]]]`.

use tuff_vanilla_data::damage_type::{self, DamageType};

use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, java_float};
use tuff_core::world::entity::Culprit;
use tuff_core::world::messages::{Directive, ManagerMessage};
use tuff_item::text::TextComponent;

use super::feedback::Messages;
use super::targets::{self, Target};
use super::{Context, Executor, executor_future};

pub struct Damage;

fn kind(ctx: &Context<'_>) -> Result<&'static DamageType, CommandError> {
    match ctx.arg("damageType") {
        Some(Value::Resource(name)) => damage_type::ALL
            .iter()
            .find(|kind| kind.name == name.strip_prefix("minecraft:").unwrap_or(name))
            .ok_or_else(|| CommandError::simple("command.unknown.argument")),
        _ => Ok(&damage_type::GENERIC),
    }
}

fn culprit(target: &Target) -> Culprit {
    match target {
        Target::Player(player) => Culprit::player(player.id, player.uuid, player.name.clone()),
        Target::Entity(entity) => Culprit {
            id: entity.id,
            uuid: entity.uuid,
            kind: entity.kind,
            name: TextComponent::new(entity.name.clone()),
            charged: None,
            owner_name: entity.owner_name.clone(),
            held_name: None,
        },
    }
}

impl Executor for Damage {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Selector(selector)) = ctx.arg("target").cloned() else {
                return Err(CommandError::simple("argument.entity.notfound.entity"));
            };
            let target = targets::select_entity(ctx, &selector).await?;
            let amount = match ctx.arg("amount") {
                Some(Value::Float(amount)) => *amount,
                _ => return Err(CommandError::simple("command.unknown.argument")),
            };
            let kind = kind(ctx)?;
            let position = match ctx.arg("location") {
                Some(Value::Coordinates(c)) => Some(c.position(&ctx.source.pose())),
                _ => None,
            };
            let mut attacker = None;
            let mut direct = false;
            let position = match position {
                Some(position) => Some(position),
                None => match ctx.arg("entity").cloned() {
                    Some(Value::Selector(by)) => {
                        let by = targets::select_entity(ctx, &by).await?;
                        let cause = match ctx.arg("cause").cloned() {
                            Some(Value::Selector(cause)) => {
                                Some(targets::select_entity(ctx, &cause).await?)
                            }
                            _ => None,
                        };
                        direct = cause.is_none();
                        attacker = Some(culprit(cause.as_ref().unwrap_or(&by)));
                        Some(by.position())
                    }
                    _ => None,
                },
            };
            let name = target.display_name();
            let message = TextComponent::translate(
                "commands.damage.success",
                [TextComponent::new(java_float(amount)), name],
            );
            match target {
                Target::Player(player) => {
                    let success = message.clone();
                    let messages =
                        Messages::new(move |_| success.clone(), move |_, _| message.clone())
                            .on_zero(|_| {
                                TextComponent::translate("commands.damage.invulnerable", [])
                            });
                    targets::dispatch_result(
                        ctx,
                        &[player],
                        |_| Directive::Hurt {
                            kind,
                            amount,
                            position,
                            attacker: attacker.clone(),
                            direct,
                        },
                        messages,
                    )
                    .await?;
                    Ok(1)
                }
                Target::Entity(entity) => {
                    let (reply, regions) = tokio::sync::oneshot::channel();
                    let _ = ctx
                        .shared()
                        .manager
                        .send(ManagerMessage::Regions { reply })
                        .await;
                    let mut hurt = false;
                    for region in regions.await.unwrap_or_default() {
                        let id = entity.id;
                        let culprit = attacker.clone();
                        hurt |= region
                            .ask(move |world| world.hurt_entity(id, kind, amount, culprit))
                            .await
                            .unwrap_or(false);
                    }
                    if !hurt {
                        return Err(CommandError::simple("commands.damage.invulnerable"));
                    }
                    ctx.success(message, true).await;
                    Ok(1)
                }
            }
        })
    }
}
