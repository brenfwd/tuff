//! `/attribute <target> <attribute> ...`: reads and edits the attributes
//! of one living entity. A player answers through its own session, any
//! other entity through the region holding it.

use tuff_vanilla_data::item::ModifierOperation;
use tuff_vanilla_data::registry::Attribute;

use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, java_double};
use tuff_core::world::messages::{
    AttributeAnswer, AttributeCommand, AttributeOp, Directive, DirectiveSource, PlayerEvent,
};
use tuff_item::text::TextComponent;

use super::ride::regions;
use super::targets::{self, Target};
use super::{Context, Executor, executor_future, text};

pub struct AttributeCmd;

fn description(attribute: Attribute) -> TextComponent {
    let name = attribute.name();
    let path = name.strip_prefix("minecraft:").unwrap_or(name);
    TextComponent::translate(format!("attribute.name.{path}"), [])
}

fn number(value: f64) -> TextComponent {
    text(java_double(value))
}

async fn run(ctx: &mut Context<'_>, target: &Target, command: AttributeCommand) -> AttributeAnswer {
    match target {
        Target::Player(player) => {
            let directive = Directive::Attribute(command);
            let source = DirectiveSource {
                name: ctx.source.display_name(),
                player: ctx.source.player_id(),
            };
            if let Some(own) = ctx
                .actor
                .apply_self(player.id, directive.clone(), source.clone())
                .await
            {
                return own.attribute.unwrap_or(AttributeAnswer::NotLiving);
            }
            let (reply, mut replies) = tokio::sync::mpsc::unbounded_channel();
            if !player.handle.send(PlayerEvent::Directive {
                directive,
                source,
                reply,
            }) {
                return AttributeAnswer::NotLiving;
            }
            replies
                .recv()
                .await
                .and_then(|reply| reply.attribute)
                .unwrap_or(AttributeAnswer::NotLiving)
        }
        Target::Entity(entity) => {
            for region in regions(ctx).await {
                let (id, command) = (entity.id, command.clone());
                if let Some(Some(answer)) = region
                    .ask(move |world| world.attribute_entity(id, &command))
                    .await
                {
                    return answer;
                }
            }
            AttributeAnswer::NotLiving
        }
    }
}

fn operation_of(literals: &[&str], id: &str, value: f64) -> AttributeOp {
    let has = |name: &str| literals.contains(&name);
    if has("modifier") {
        if has("add") {
            let operation = if has("add_multiplied_base") {
                ModifierOperation::AddMultipliedBase
            } else if has("add_multiplied_total") {
                ModifierOperation::AddMultipliedTotal
            } else {
                ModifierOperation::AddValue
            };
            AttributeOp::AddModifier {
                id: id.to_owned(),
                amount: value,
                operation,
            }
        } else if has("remove") {
            AttributeOp::RemoveModifier(id.to_owned())
        } else {
            AttributeOp::ModifierValue(id.to_owned())
        }
    } else if has("base") {
        if has("set") {
            AttributeOp::SetBase(value)
        } else if has("reset") {
            AttributeOp::ResetBase
        } else {
            AttributeOp::Base
        }
    } else {
        AttributeOp::Value
    }
}

impl Executor for AttributeCmd {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Selector(selector)) = ctx.arg("target").cloned() else {
                return Err(CommandError::simple("argument.entity.notfound.entity"));
            };
            let Some(attribute) = (match ctx.arg("attribute") {
                Some(Value::Resource(name)) => Attribute::from_name(name),
                _ => None,
            }) else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let target = targets::select_entity(ctx, &selector).await?;
            let scale = match ctx.arg("scale") {
                Some(Value::Double(scale)) => *scale,
                _ => 1.0,
            };
            let modifier_id = match ctx.arg("id") {
                Some(Value::Identifier(id)) => id.clone(),
                _ => String::new(),
            };
            let value = match ctx.arg("value") {
                Some(Value::Double(value)) => *value,
                _ => 0.0,
            };
            let op = operation_of(&ctx.literals(), &modifier_id, value);
            let command = AttributeCommand {
                attribute,
                op: op.clone(),
            };
            let answer = run(ctx, &target, command).await;
            let name = ctx.name_of(&target);
            let attribute_name = description(attribute);
            let id_arg = || TextComponent::translate(modifier_id.clone(), []);
            let (key, args, result) = match (&op, answer) {
                (_, AttributeAnswer::NotLiving) => {
                    return Err(CommandError::dynamic(
                        "commands.attribute.failed.entity",
                        [name],
                    ));
                }
                (_, AttributeAnswer::NoAttribute) => {
                    return Err(CommandError::dynamic(
                        "commands.attribute.failed.no_attribute",
                        [name, attribute_name],
                    ));
                }
                (_, AttributeAnswer::NoModifier) => {
                    return Err(CommandError::dynamic(
                        "commands.attribute.failed.no_modifier",
                        [attribute_name, name, id_arg()],
                    ));
                }
                (_, AttributeAnswer::DuplicateModifier) => {
                    return Err(CommandError::dynamic(
                        "commands.attribute.failed.modifier_already_present",
                        [id_arg(), attribute_name, name],
                    ));
                }
                (AttributeOp::Value, AttributeAnswer::Number(n)) => (
                    "commands.attribute.value.get.success",
                    vec![attribute_name, name, number(n)],
                    (n * scale) as i32,
                ),
                (AttributeOp::Base, AttributeAnswer::Number(n)) => (
                    "commands.attribute.base_value.get.success",
                    vec![attribute_name, name, number(n)],
                    (n * scale) as i32,
                ),
                (AttributeOp::ModifierValue(_), AttributeAnswer::Number(n)) => (
                    "commands.attribute.modifier.value.get.success",
                    vec![id_arg(), attribute_name, name, number(n)],
                    (n * scale) as i32,
                ),
                (AttributeOp::ResetBase, AttributeAnswer::Number(n)) => (
                    "commands.attribute.base_value.reset.success",
                    vec![attribute_name, name, number(n)],
                    1,
                ),
                (AttributeOp::SetBase(v), _) => (
                    "commands.attribute.base_value.set.success",
                    vec![attribute_name, name, number(*v)],
                    1,
                ),
                (AttributeOp::AddModifier { .. }, _) => (
                    "commands.attribute.modifier.add.success",
                    vec![id_arg(), attribute_name, name],
                    1,
                ),
                (AttributeOp::RemoveModifier(_), _) => (
                    "commands.attribute.modifier.remove.success",
                    vec![id_arg(), attribute_name, name],
                    1,
                ),
                _ => return Err(CommandError::simple("command.unknown.argument")),
            };
            ctx.success(TextComponent::translate(key, args), false)
                .await;
            Ok(result)
        })
    }
}
