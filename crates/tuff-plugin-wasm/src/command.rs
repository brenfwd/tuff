//! `/tuff plugin list|load|unload|reload|purge`: managing runtime plugins.

use tuff_core::command::args::{Value, suggest_resources};
use tuff_core::command::parse::ParsedNode;
use tuff_core::command::{CommandError, CommandNode, CommandNodeKind};
use tuff_core::world::commands::{
    Context, Executor, ExecutorFuture, SuggestInput, executor_future, text,
};

use crate::host::{GrantChange, WasmHost};

pub const NODE: &str = "tuff.command.plugin";
const REQUIREMENT: &str = r#"{"type":"tuff:permission","node":"tuff.command.plugin"}"#;

const TARGET: CommandNode = CommandNode {
    name: "target",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:string",
        properties: Some(r#"{"type":"word"}"#),
    },
    executable: true,
    redirect: None,
    permissions: None,
    children: &[],
};

const ID: CommandNode = CommandNode {
    name: "id",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:string",
        properties: Some(r#"{"type":"word"}"#),
    },
    executable: false,
    redirect: None,
    permissions: None,
    children: &[CAP],
};

const GRANTS_ID: CommandNode = CommandNode {
    executable: true,
    children: &[],
    ..ID
};

const CAP: CommandNode = CommandNode {
    name: "cap",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:string",
        properties: Some(r#"{"type":"word"}"#),
    },
    executable: false,
    redirect: None,
    permissions: None,
    children: &[ENTRY],
};

const ENTRY: CommandNode = CommandNode {
    name: "entry",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:string",
        properties: Some(r#"{"type":"greedy"}"#),
    },
    executable: true,
    redirect: None,
    permissions: None,
    children: &[],
};

const fn action(name: &'static str, children: &'static [CommandNode]) -> CommandNode {
    CommandNode {
        name,
        kind: CommandNodeKind::Literal,
        executable: children.is_empty(),
        redirect: None,
        permissions: None,
        children,
    }
}

pub static PLUGIN: CommandNode = CommandNode {
    name: "plugin",
    kind: CommandNodeKind::Literal,
    executable: false,
    redirect: None,
    permissions: Some(REQUIREMENT),
    children: &[
        action("grants", &[GRANTS_ID]),
        action("grant", &[ID]),
        action("revoke", &[ID]),
        action("list", &[]),
        action("catalog", &[]),
        action("load", &[TARGET]),
        action("purge", &[TARGET]),
        action("reload", &[TARGET]),
        action("unload", &[TARGET]),
    ],
};

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::text(text(message.into()))
}

pub struct PluginCommand;

impl Executor for PluginCommand {
    fn suggest(
        &self,
        path: &[ParsedNode],
        partial: &str,
        input: &SuggestInput,
    ) -> Option<Vec<String>> {
        let literal = |name| path.iter().any(|node| node.node.name == name);
        match (literal("grant"), literal("revoke"), literal("grants")) {
            (true, _, _) => suggest_grants(path, partial, input, false),
            (_, true, _) => suggest_grants(path, partial, input, true),
            (_, _, true) => suggest_grants(path, partial, input, false),
            _ => None,
        }
    }

    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            if !ctx.permits(NODE) {
                return Err(CommandError::simple("command.unknown.command"));
            }
            let shared = ctx.shared().clone();
            let host = shared
                .services
                .get::<WasmHost>()
                .ok_or_else(|| fail("the plugin host is not running"))?;
            let action = ctx.literals().get(2).copied().unwrap_or("list");
            if action == "catalog" {
                let Some(player) = ctx.actor.player() else {
                    return Err(fail("only a player can open the catalog"));
                };
                host.menus()
                    .open_catalog(player.profile.id, 0, &shared.custom);
                return Ok(1);
            }
            if matches!(action, "grants" | "grant" | "revoke") {
                return grants(ctx, host, action).await;
            }
            let target = match ctx.arg("target") {
                Some(Value::Str(target)) => target.clone(),
                _ => String::new(),
            };
            let done = match action {
                "load" => host.load(&target).await,
                "unload" => host.unload(&target).await,
                "reload" => host.reload(&target).await,
                "purge" => host.purge(&target).await,
                _ => {
                    let plugins = host.list().await;
                    ctx.reply(text(format!("{} runtime plugins loaded", plugins.len())));
                    for plugin in &plugins {
                        let mut line = format!("{} {}", plugin.id, plugin.version);
                        if !plugin.commands.is_empty() {
                            line.push_str(&format!(" /{}", plugin.commands.join(" /")));
                        }
                        if !plugin.permissions.is_empty() {
                            line.push_str(&format!(" [{}]", plugin.permissions.join(" ")));
                        }
                        if !plugin.description.is_empty() {
                            line.push_str(&format!(": {}", plugin.description));
                        }
                        ctx.reply(text(line));
                    }
                    return Ok(plugins.len() as i32);
                }
            };
            let done = done.map_err(fail)?;
            ctx.success(text(done), true).await;
            Ok(1)
        })
    }
}

fn word(ctx: &Context<'_>, name: &str) -> String {
    match ctx.arg(name) {
        Some(Value::Str(text)) => text.clone(),
        _ => String::new(),
    }
}

async fn grants(ctx: &mut Context<'_>, host: &WasmHost, action: &str) -> Result<i32, CommandError> {
    let id = word(ctx, "id");
    if action == "grants" {
        let report = host.grants(&id).await.map_err(fail)?;
        ctx.reply(text(format!(
            "{id} requests {} capabilities",
            report.requested.len()
        )));
        for (entry, reason, granted) in &report.requested {
            let reason = match reason.is_empty() {
                true => String::new(),
                false => format!(" ({reason})"),
            };
            let line = match granted {
                true => format!("  granted: {entry}{reason}"),
                false => format!("  NOT GRANTED: {entry}{reason}"),
            };
            ctx.reply(text(line));
        }
        for entry in &report.extra {
            ctx.reply(text(format!("  granted, not requested: {entry}")));
        }
        for note in &report.notes {
            ctx.reply(text(format!("  note: {note}")));
        }
        return Ok(report.requested.len() as i32);
    }
    let (cap, entry) = (word(ctx, "cap"), word(ctx, "entry"));
    let change = match action {
        "grant" => GrantChange::Grant { cap, entry },
        _ => GrantChange::Revoke { cap, entry },
    };
    let done = host.change_grant(&id, change).await.map_err(fail)?;
    ctx.success(text(done), true).await;
    Ok(1)
}

fn suggest_grants(
    path: &[ParsedNode],
    partial: &str,
    input: &SuggestInput,
    revoke: bool,
) -> Option<Vec<String>> {
    let host = input.services.get::<WasmHost>()?;
    let index = host.snapshot();
    let words: Vec<&str> = path
        .iter()
        .filter(|node| matches!(node.node.kind, CommandNodeKind::Argument { .. }))
        .map(|node| node.node.name)
        .collect();
    let typed: Vec<String> = path
        .iter()
        .filter_map(|node| match &node.value {
            Some(Value::Str(text)) => Some(text.clone()),
            _ => None,
        })
        .collect();
    let candidates: Vec<String> = match words.len() {
        0 => index.requests.keys().cloned().collect(),
        1 => {
            let mut kinds: Vec<String> = index
                .requests
                .get(typed.first()?)
                .map(|r| r.iter().map(|(kind, _)| kind.clone()).collect())
                .unwrap_or_default();
            kinds.sort();
            kinds.dedup();
            kinds
        }
        _ => {
            let mut entries: Vec<String> = index
                .requests
                .get(typed.first()?)
                .map(|r| {
                    r.iter()
                        .filter(|(kind, _)| Some(kind) == typed.get(1))
                        .map(|(_, entry)| entry.clone())
                        .collect()
                })
                .unwrap_or_default();
            entries.push(if revoke { "all" } else { "all-requested" }.to_owned());
            entries
        }
    };
    Some(suggest_resources(
        partial,
        candidates.iter().map(String::as_str),
        "",
    ))
}
