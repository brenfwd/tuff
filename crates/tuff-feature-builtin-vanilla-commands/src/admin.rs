//! World settings and server chat: gamerule, weather, difficulty, seed,
//! say, me and help. Most run in the coordinator, which owns the
//! settings. The administration commands (op, whitelist, bans, stop,
//! save-all) are in `core-commands`.

use tuff_vanilla_data::command::{CommandNode, CommandNodeKind};

use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, ParseContext, allowed};
use tuff_core::world::game_rules::RuleValue;
use tuff_core::world::messages::{CoordinatorMessage, DifficultyOutcome, GameRuleOutcome};
use tuff_core::world::player::Difficulty;
use tuff_core::world::weather::WeatherKind;
use tuff_item::text::TextComponent;

use super::targets::resolve_message;
use super::{Context, Executor, executor_future, text};

fn rule_command_result(value: &str) -> i32 {
    match value {
        "true" => 1,
        "false" => 0,
        other => other.parse().unwrap_or(0),
    }
}

pub struct GameRule;

impl Executor for GameRule {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(name) = ctx.literals().get(1).copied() else {
                return Err(CommandError::simple("command.unknown.command"));
            };
            let value = match ctx.arg("value") {
                Some(Value::Bool(value)) => Some(RuleValue::Bool(*value)),
                Some(Value::Int(value)) => Some(RuleValue::Int(*value)),
                _ => None,
            };
            let name = name.to_owned();
            let Some(outcome) = ctx
                .ask(|reply| CoordinatorMessage::GameRule { name, value, reply })
                .await
            else {
                return Ok(1);
            };
            let translate = |key: &str, rule: &str, value: &str| {
                TextComponent::translate(key, [text(rule), text(value)])
            };
            match outcome {
                GameRuleOutcome::Unknown | GameRuleOutcome::Disabled => {
                    ctx.fail(TextComponent::translate("command.unknown.argument", []))
                }
                GameRuleOutcome::Query { rule, value } => {
                    ctx.success(translate("commands.gamerule.query", rule, &value), false)
                        .await;
                    return Ok(rule_command_result(&value));
                }
                GameRuleOutcome::AlreadySet { rule, value } => {
                    ctx.fail(translate("commands.gamerule.not_set", rule, &value))
                }
                GameRuleOutcome::Set { rule, value } => {
                    ctx.success(translate("commands.gamerule.set", rule, &value), true)
                        .await;
                    return Ok(rule_command_result(&value));
                }
            }
            Ok(1)
        })
    }
}

pub struct Weather;

impl Executor for Weather {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let kind = match ctx.literals().get(1).copied() {
                Some("clear") => WeatherKind::Clear,
                Some("rain") => WeatherKind::Rain,
                Some("thunder") => WeatherKind::Thunder,
                _ => return Err(CommandError::simple("command.unknown.command")),
            };
            let duration = match ctx.arg("duration") {
                Some(Value::Time(ticks)) => Some(*ticks),
                _ => None,
            };
            ctx.ask(|done| CoordinatorMessage::Weather {
                kind,
                duration,
                done,
            })
            .await;
            let key = match kind {
                WeatherKind::Clear => "commands.weather.set.clear",
                WeatherKind::Rain => "commands.weather.set.rain",
                WeatherKind::Thunder => "commands.weather.set.thunder",
            };
            ctx.success(TextComponent::translate(key, []), true).await;
            Ok(1)
        })
    }
}

pub struct DifficultyCommand;

impl Executor for DifficultyCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let difficulty = ctx
                .literals()
                .get(1)
                .and_then(|name| Difficulty::from_name(name));
            let Some(outcome) = ctx
                .ask(|reply| CoordinatorMessage::Difficulty { difficulty, reply })
                .await
            else {
                return Ok(1);
            };
            let display = |difficulty: Difficulty| {
                TextComponent::translate(format!("options.difficulty.{}", difficulty.name()), [])
            };
            match outcome {
                DifficultyOutcome::Query(current) => {
                    let message =
                        TextComponent::translate("commands.difficulty.query", [display(current)]);
                    ctx.success(message, false).await;
                    Ok(i32::from(current.id()))
                }
                DifficultyOutcome::Unchanged(difficulty) => Err(CommandError::text(
                    TextComponent::translate("commands.difficulty.failure", [display(difficulty)]),
                )),
                DifficultyOutcome::Set(difficulty) => {
                    let message = TextComponent::translate(
                        "commands.difficulty.success",
                        [display(difficulty)],
                    );
                    ctx.success(message, true).await;
                    Ok(0)
                }
            }
        })
    }
}

pub struct Seed;

impl Executor for Seed {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let seed = ctx.shared().seed;
            ctx.success(
                TextComponent::translate(
                    "commands.seed.success",
                    [TextComponent::copy_on_click(seed.to_string())],
                ),
                false,
            )
            .await;
            Ok(seed as i32)
        })
    }
}

pub struct Version;

impl Executor for Version {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            use tuff_vanilla_data::version as v;
            let t = |key: &str, args: &[TextComponent]| {
                TextComponent::translate(key, args.iter().cloned())
            };
            let pack = |p: v::PackVersion| format!("{}.{}", p.major, p.minor);
            let lines = [
                t("commands.version.header", &[]),
                t("commands.version.id", &[TextComponent::new(v::ID)]),
                t("commands.version.name", &[TextComponent::new(v::NAME)]),
                t(
                    "commands.version.data",
                    &[TextComponent::new(v::DATA_VERSION.to_string())],
                ),
                t("commands.version.series", &[TextComponent::new(v::SERIES)]),
                t(
                    "commands.version.protocol",
                    &[
                        TextComponent::new(v::PROTOCOL_VERSION.to_string()),
                        TextComponent::new(format!("0x{:x}", v::PROTOCOL_VERSION)),
                    ],
                ),
                t(
                    "commands.version.build_time",
                    &[TextComponent::new(v::BUILD_TIME)],
                ),
                t(
                    "commands.version.pack.resource",
                    &[TextComponent::new(pack(v::RESOURCE_PACK_VERSION))],
                ),
                t(
                    "commands.version.pack.data",
                    &[TextComponent::new(pack(v::DATA_PACK_VERSION))],
                ),
                t(
                    if v::STABLE {
                        "commands.version.stable.yes"
                    } else {
                        "commands.version.stable.no"
                    },
                    &[],
                ),
            ];
            for line in lines {
                ctx.success(line, false).await;
            }
            Ok(1)
        })
    }
}

/// `/say` and `/me`: chat lines formatted client-side with a chat type.
pub struct Chat {
    pub chat_type: &'static str,
    pub argument: &'static str,
}

impl Executor for Chat {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Message(message)) = ctx.arg(self.argument).cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let message = resolve_message(ctx, &message).await?;
            ctx.coordinator(CoordinatorMessage::ChatBroadcast {
                chat_type: self.chat_type,
                sender: ctx.source_display_name(),
                message,
            })
            .await;
            Ok(1)
        })
    }
}

fn usage_text(node: &CommandNode) -> String {
    match node.kind {
        CommandNodeKind::Argument { .. } => format!("<{}>", node.name),
        _ => node.name.to_owned(),
    }
}

/// The game's compact usage line for a node.
fn smart_usage(
    node: &CommandNode,
    ctx: &Context<'_>,
    optional: bool,
    deep: bool,
) -> Option<String> {
    if !allowed(node, &ctx.shared().permissions.access(ctx.source.subject())) {
        return None;
    }
    let text = if optional {
        format!("[{}]", usage_text(node))
    } else {
        usage_text(node)
    };
    let child_optional = node.executable;
    let (open, close) = if child_optional {
        ("[", "]")
    } else {
        ("<", ">")
    };
    if !deep {
        if let Some(path) = node.redirect {
            let target = if path.is_empty() {
                "...".to_owned()
            } else {
                format!("-> {}", path.last().copied().unwrap_or(""))
            };
            return Some(format!("{text} {target}"));
        }
        let children: Vec<&CommandNode> = node
            .children
            .iter()
            .filter(|c| allowed(c, &ctx.shared().permissions.access(ctx.source.subject())))
            .collect();
        if children.len() == 1 {
            if let Some(usage) = smart_usage(children[0], ctx, child_optional, child_optional) {
                return Some(format!("{text} {usage}"));
            }
        } else if children.len() > 1 {
            let mut usages: Vec<String> = children
                .iter()
                .filter_map(|c| smart_usage(c, ctx, child_optional, true))
                .collect();
            usages.dedup();
            if usages.len() == 1 {
                let usage = if child_optional {
                    format!("[{}]", usages[0])
                } else {
                    usages[0].clone()
                };
                return Some(format!("{text} {usage}"));
            } else if usages.len() > 1 {
                let names: Vec<String> = children.iter().map(|c| usage_text(c)).collect();
                return Some(format!("{text} {open}{}{close}", names.join("|")));
            }
        }
    }
    Some(text)
}

pub struct Help;

impl Executor for Help {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let registry = ctx.shared().commands.clone();
            let lines: Vec<String> = match ctx.arg("command").cloned() {
                None => registry
                    .roots()
                    .commands()
                    .iter()
                    .filter_map(|command| smart_usage(command.node, ctx, false, false))
                    .map(|usage| format!("/{usage}"))
                    .collect(),
                Some(Value::Str(command)) => {
                    let parsed = registry.roots().parse(
                        &command,
                        ctx.source.permission,
                        ParseContext {
                            allow_selectors: ctx.source.permission.allows_selectors(),
                        },
                    );
                    let Some(last) = parsed.nodes.last() else {
                        return Err(CommandError::simple("commands.help.failed"));
                    };
                    let children: Vec<&CommandNode> = match last.node.redirect {
                        Some(path) => registry
                            .roots()
                            .resolve(path)
                            .map_or(Vec::new(), |(c, _)| c.iter().collect()),
                        None => last.node.children.iter().collect(),
                    };
                    children
                        .iter()
                        .filter_map(|child| smart_usage(child, ctx, last.node.executable, false))
                        .map(|usage| format!("/{command} {usage}"))
                        .collect()
                }
                Some(_) => return Err(CommandError::simple("command.unknown.argument")),
            };
            let count = lines.len();
            for line in lines {
                ctx.success(TextComponent::new(line), false).await;
            }
            Ok(count as i32)
        })
    }
}

pub struct SetIdleTimeout;

impl Executor for SetIdleTimeout {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Int(minutes)) = ctx.arg("minutes") else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let minutes = *minutes;
            ctx.ask(|reply| CoordinatorMessage::ServerProperties {
                change: Some(("player-idle-timeout".to_owned(), minutes.to_string())),
                reply,
            })
            .await;
            let message = if minutes > 0 {
                TextComponent::translate("commands.setidletimeout.success", [text(minutes)])
            } else {
                TextComponent::translate("commands.setidletimeout.success.disabled", [])
            };
            ctx.success(message, true).await;
            Ok(minutes)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::rule_command_result;

    #[tuff_core::watchdog]
    #[test]
    fn gamerule_results_follow_the_rule_value() {
        assert_eq!(rule_command_result("true"), 1);
        assert_eq!(rule_command_result("false"), 0);
        assert_eq!(rule_command_result("300"), 300);
    }
}
