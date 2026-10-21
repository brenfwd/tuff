//! `/advancement grant|revoke`: meets or un-meets every criterion of the
//! chosen advancements (or one criterion) for each target player.

use tuff_core::command::CommandError;
use tuff_core::command::args::Value;
use tuff_core::world::advancement::Related;
use tuff_core::world::messages::{Directive, DirectiveReply};
use tuff_item::text::TextComponent;

use super::feedback::Messages;
use super::targets;
use super::{Context, Executor, executor_future, text};

pub struct AdvancementCommand;

fn player_name(reply: &DirectiveReply) -> TextComponent {
    text(&reply.name)
}

impl Executor for AdvancementCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let literals = ctx.literals();
            let grant = literals.contains(&"grant");
            let key = if grant { "grant" } else { "revoke" };
            let selector = match ctx.arg("targets") {
                Some(Value::Selector(selector)) => selector.clone(),
                _ => return Err(CommandError::simple("command.unknown.argument")),
            };
            let tree = ctx.shared().advancements();
            let everything = literals.contains(&"everything");
            let (ids, target) = if everything {
                (tree.ids(), None)
            } else {
                let name = match ctx.arg("advancement") {
                    Some(Value::Resource(name)) => name.clone(),
                    _ => return Err(CommandError::simple("command.unknown.argument")),
                };
                let Some(id) = tree.id(&name) else {
                    return Err(CommandError::dynamic(
                        "advancement.advancementNotFound",
                        [name],
                    ));
                };
                let mode: Related = if literals.contains(&"from") {
                    Related::From
                } else if literals.contains(&"until") {
                    Related::Until
                } else if literals.contains(&"through") {
                    Related::Through
                } else {
                    Related::Only
                };
                (tree.related(id, mode), Some(id))
            };
            let criterion = match ctx.arg("criterion") {
                Some(Value::Str(criterion)) => Some(criterion.clone()),
                _ => None,
            };
            let players = targets::select_players(ctx, &selector).await?;

            if let (Some(criterion), Some(id)) = (criterion.clone(), target) {
                if !tree.has_criterion(id, &criterion) {
                    return Err(CommandError::dynamic(
                        "commands.advancement.criterionNotFound",
                        [tree.chat_name(id), text(&criterion)],
                    ));
                }
                let (name, many_name, error_name) =
                    (tree.chat_name(id), tree.chat_name(id), tree.chat_name(id));
                let (c1, c2, c3) = (criterion.clone(), criterion.clone(), criterion.clone());
                let messages = Messages::new(
                    move |reply| {
                        TextComponent::translate(
                            format!("commands.advancement.{key}.criterion.to.one.success"),
                            [text(&c1), name.clone(), player_name(reply)],
                        )
                    },
                    move |count, _| {
                        TextComponent::translate(
                            format!("commands.advancement.{key}.criterion.to.many.success"),
                            [text(&c2), many_name.clone(), text(count)],
                        )
                    },
                )
                .on_zero(move |replies| match replies {
                    [only] => TextComponent::translate(
                        format!("commands.advancement.{key}.criterion.to.one.failure"),
                        [text(&c3), error_name.clone(), player_name(only)],
                    ),
                    _ => TextComponent::translate(
                        format!("commands.advancement.{key}.criterion.to.many.failure"),
                        [text(&c3), error_name.clone(), text(replies.len())],
                    ),
                });
                targets::dispatch(
                    ctx,
                    &players,
                    |_| Directive::Advancement {
                        grant,
                        tree: tree.clone(),
                        ids: vec![id],
                        criterion: Some(criterion.clone()),
                        show: true,
                    },
                    messages,
                )
                .await;
                return Ok(1);
            }

            let count = ids.len();
            let messages = if let [only] = ids.as_slice() {
                let (name, many_name, error_name) = (
                    tree.chat_name(*only),
                    tree.chat_name(*only),
                    tree.chat_name(*only),
                );
                Messages::new(
                    move |reply| {
                        TextComponent::translate(
                            format!("commands.advancement.{key}.one.to.one.success"),
                            [name.clone(), player_name(reply)],
                        )
                    },
                    move |players, _| {
                        TextComponent::translate(
                            format!("commands.advancement.{key}.one.to.many.success"),
                            [many_name.clone(), text(players)],
                        )
                    },
                )
                .on_zero(move |replies| match replies {
                    [only] => TextComponent::translate(
                        format!("commands.advancement.{key}.one.to.one.failure"),
                        [error_name.clone(), player_name(only)],
                    ),
                    _ => TextComponent::translate(
                        format!("commands.advancement.{key}.one.to.many.failure"),
                        [error_name.clone(), text(replies.len())],
                    ),
                })
            } else {
                Messages::new(
                    move |reply| {
                        TextComponent::translate(
                            format!("commands.advancement.{key}.many.to.one.success"),
                            [text(count), player_name(reply)],
                        )
                    },
                    move |players, _| {
                        TextComponent::translate(
                            format!("commands.advancement.{key}.many.to.many.success"),
                            [text(count), text(players)],
                        )
                    },
                )
                .on_zero(move |replies| match replies {
                    [only] => TextComponent::translate(
                        format!("commands.advancement.{key}.many.to.one.failure"),
                        [text(count), player_name(only)],
                    ),
                    _ => TextComponent::translate(
                        format!("commands.advancement.{key}.many.to.many.failure"),
                        [text(count), text(replies.len())],
                    ),
                })
            };
            targets::dispatch(
                ctx,
                &players,
                |_| Directive::Advancement {
                    grant,
                    tree: tree.clone(),
                    ids: ids.clone(),
                    criterion: None,
                    show: !everything,
                },
                messages,
            )
            .await;
            Ok(1)
        })
    }
}

#[cfg(test)]
mod tests {

    #[tuff_core::watchdog]
    #[test]
    fn advancement_and_recipe_commands_parse() {
        use tuff_core::command::ParseContext;
        use tuff_storage::admin::PermissionLevel;
        let mut registry = tuff_core::world::commands::Registry::new();
        crate::registry::register(&mut registry);
        let parse = |input: &str| {
            let parsed = registry.roots().parse(
                input,
                PermissionLevel::Gamemasters,
                ParseContext {
                    allow_selectors: true,
                },
            );
            parsed
                .error(input)
                .map(|e| e.key().unwrap_or_default().to_owned())
        };
        assert_eq!(parse("advancement grant @s everything"), None);
        assert_eq!(parse("advancement revoke @a from story/mine_stone"), None);
        assert_eq!(
            parse(
                "advancement grant @s only minecraft:adventure/adventuring_time minecraft:badlands"
            ),
            None
        );
        assert_eq!(
            parse("advancement grant @s only minecraft:no/such"),
            None,
            "advancement ids resolve when the command runs"
        );
        assert_eq!(parse("recipe take @s *"), None);
        assert_eq!(parse("recipe give @s minecraft:piston"), None);
    }
}
