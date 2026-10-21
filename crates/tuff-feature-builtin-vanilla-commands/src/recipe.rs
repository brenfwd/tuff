//! `/recipe give|take`: adds recipes to (or removes them from) the target
//! players' recipe books. Taking a recipe leaves its recipe advancement
//! done, so the recipe does not come back on its own.

use tuff_core::command::CommandError;
use tuff_core::command::args::Value;
use tuff_core::world::messages::Directive;
use tuff_item::text::TextComponent;

use super::feedback::Messages;
use super::targets;
use super::{Context, Executor, executor_future, text};

pub struct RecipeCommand;

impl Executor for RecipeCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let give = ctx.literals().contains(&"give");
            let key = if give { "give" } else { "take" };
            let selector = match ctx.arg("targets") {
                Some(Value::Selector(selector)) => selector.clone(),
                _ => return Err(CommandError::simple("command.unknown.argument")),
            };
            let recipes = match ctx.arg("recipe") {
                Some(Value::Resource(name)) => {
                    let recipe =
                        ctx.shared().recipes().resolve(name).ok_or_else(|| {
                            CommandError::dynamic("recipe.notFound", [name.clone()])
                        })?;
                    Some(vec![recipe])
                }
                _ => None,
            };
            let players = targets::select_players(ctx, &selector).await?;
            let messages = Messages::new(
                move |reply| {
                    TextComponent::translate(
                        format!("commands.recipe.{key}.success.single"),
                        [text(reply.value), text(&reply.name)],
                    )
                },
                move |count, total| {
                    TextComponent::translate(
                        format!("commands.recipe.{key}.success.multiple"),
                        [text(total), text(count)],
                    )
                },
            )
            .on_zero(move |_| {
                TextComponent::translate(format!("commands.recipe.{key}.failed"), [])
            });
            targets::dispatch(
                ctx,
                &players,
                |_| Directive::Recipes {
                    give,
                    recipes: recipes.clone(),
                },
                messages,
            )
            .await;
            Ok(1)
        })
    }
}
