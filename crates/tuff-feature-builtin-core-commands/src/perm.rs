//! `/tuff perm`: grant, deny or clear a player's permission nodes, check
//! what a player holds, list settings and registered nodes, and manage
//! roles (`/tuff perm role ...`) whose settings their members share.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, CommandNode, CommandNodeKind};
use tuff_core::permission::{
    Attribute, Change, DEFAULT_ROLE, Decision, Role, Subject, valid_pattern,
};
use tuff_core::world::commands::{Actor, Context, targets, text};
use tuff_core::world::messages::CoordinatorMessage;
use tuff_storage::admin::NameAndId;
use tuff_storage::admin::PermissionLevel;

use crate::Output;
use crate::tuff::{guarded, literal, string_argument};

pub const PERM_NODE: &str = "tuff.command.perm";

const WORD: &str = r#"{"type":"word"}"#;
const NODE: CommandNode = string_argument("node", r#"{"type":"greedy"}"#, true, &[]);
const TARGET: CommandNode = string_argument("player", WORD, false, &[NODE]);
const LISTED: CommandNode = string_argument("player", WORD, true, &[]);
const PRIORITY: CommandNode = CommandNode {
    name: "priority",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:integer",
        properties: None,
    },
    executable: true,
    redirect: None,
    permissions: None,
    children: &[],
};
const ROLE_NODE: CommandNode = string_argument("role", WORD, false, &[NODE]);
const ROLE_MEMBER: CommandNode = string_argument("role", WORD, false, &[LISTED]);
const ROLE_CREATED: CommandNode = string_argument("role", WORD, true, &[PRIORITY]);
const ROLE_PRIORITY: CommandNode = string_argument("role", WORD, false, &[PRIORITY]);
const ROLE_NAMED: CommandNode = string_argument("role", WORD, true, &[]);
const VALUE: CommandNode = string_argument("value", r#"{"type":"greedy"}"#, true, &[]);
const ATTRIBUTE_SET: CommandNode = string_argument("attribute", WORD, false, &[VALUE]);
const ATTRIBUTE_CLEAR: CommandNode = string_argument("attribute", WORD, true, &[]);
const ROLE_SET: CommandNode = string_argument("role", WORD, false, &[ATTRIBUTE_SET]);
const ROLE_CLEAR: CommandNode = string_argument("role", WORD, false, &[ATTRIBUTE_CLEAR]);
const ROLE: CommandNode = literal(
    "role",
    false,
    &[
        literal("add", false, &[ROLE_MEMBER]),
        literal("clear", false, &[ROLE_CLEAR]),
        literal("create", false, &[ROLE_CREATED]),
        literal("delete", false, &[ROLE_NAMED]),
        literal("deny", false, &[ROLE_NODE]),
        literal("grant", false, &[ROLE_NODE]),
        literal("list", true, &[ROLE_NAMED]),
        literal("priority", false, &[ROLE_PRIORITY]),
        literal("remove", false, &[ROLE_MEMBER]),
        literal("revoke", false, &[ROLE_NODE]),
        literal("set", false, &[ROLE_SET]),
    ],
);
pub const PERM: CommandNode = guarded(
    literal(
        "perm",
        false,
        &[
            literal("check", false, &[TARGET]),
            literal("deny", false, &[TARGET]),
            literal("grant", false, &[TARGET]),
            ROLE,
            literal("list", true, &[LISTED]),
            literal("nodes", true, &[]),
            literal("revoke", false, &[TARGET]),
        ],
    ),
    r#"{"type":"tuff:permission","node":"tuff.command.perm"}"#,
);

type Then = Box<
    dyn FnOnce(Output, NameAndId) -> Pin<Box<dyn Future<Output = Result<i32, CommandError>> + Send>>
        + Send,
>;

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::text(text(message.into()))
}

fn unknown() -> CommandError {
    CommandError::simple("command.unknown.command")
}

fn string(ctx: &Context<'_>, name: &str) -> Option<String> {
    match ctx.arg(name) {
        Some(Value::Str(value)) => Some(value.trim().to_owned()),
        _ => None,
    }
}

/// What `grant`, `deny` and `revoke` set a node to.
fn held_for(action: &str) -> Option<bool> {
    match action {
        "grant" => Some(true),
        "deny" => Some(false),
        _ => None,
    }
}

/// The `node` argument, refusing malformed patterns and, for grant and
/// deny, unregistered nodes (typos); revoke can clear anything.
fn checked_node(ctx: &Context<'_>, held: Option<bool>) -> Result<String, CommandError> {
    let node = string(ctx, "node").ok_or_else(unknown)?;
    if !valid_pattern(&node) {
        return Err(fail(format!(
            "'{node}' is not a permission node or a pattern ending in *"
        )));
    }
    if held.is_some()
        && !node.ends_with('*')
        && !ctx.shared().permissions.view().nodes().contains(&node)
    {
        return Err(fail(format!(
            "Unknown permission node '{node}' (/tuff perm nodes lists them)"
        )));
    }
    Ok(node)
}

pub async fn run(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let action = ctx.literals().get(2).copied();
    match action {
        Some("nodes") => Ok(nodes(ctx)),
        Some("role") => role(ctx).await,
        Some("list") => match string(ctx, "player") {
            Some(name) => {
                let then: Then = Box::new(|output, user| Box::pin(list(output, user)));
                for_player(ctx, name, then).await
            }
            None => Ok(list_all(ctx)),
        },
        Some(action @ ("grant" | "deny" | "revoke")) => {
            let held = held_for(action);
            let name = string(ctx, "player").ok_or_else(unknown)?;
            let node = checked_node(ctx, held)?;
            let then: Then = Box::new(move |output, user| Box::pin(set(output, user, node, held)));
            for_player(ctx, name, then).await
        }
        Some("check") => {
            let (Some(name), Some(node)) = (string(ctx, "player"), string(ctx, "node")) else {
                return Err(unknown());
            };
            let then: Then = Box::new(move |output, user| Box::pin(check(output, user, node)));
            for_player(ctx, name, then).await
        }
        _ => Err(unknown()),
    }
}

async fn role(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let action = ctx.literals().get(3).copied().ok_or_else(unknown)?;
    let name = string(ctx, "role");
    if action == "list" {
        return match name {
            Some(name) => list_role(ctx, &name),
            None => Ok(list_roles(ctx)),
        };
    }
    let role = name.ok_or_else(unknown)?;
    let priority = match ctx.arg("priority") {
        Some(Value::Int(priority)) => Some(*priority),
        _ => None,
    };
    let (change, unchanged, done) = match action {
        "create" => {
            let priority = priority.unwrap_or_default();
            (
                Change::CreateRole {
                    role: role.clone(),
                    priority,
                },
                String::new(),
                format!("Created role {role} with priority {priority}"),
            )
        }
        "delete" => (
            Change::DeleteRole { role: role.clone() },
            String::new(),
            format!("Deleted role {role}"),
        ),
        "priority" => {
            let priority = priority.ok_or_else(unknown)?;
            (
                Change::Priority {
                    role: role.clone(),
                    priority,
                },
                format!("Role {role} already has priority {priority}"),
                format!("Set role {role}'s priority to {priority}"),
            )
        }
        "grant" | "deny" | "revoke" => {
            let held = held_for(action);
            let node = checked_node(ctx, held)?;
            let (unchanged, done) = match held {
                Some(true) => (
                    format!("Role {role} already has {node} granted"),
                    format!("Granted {node} to role {role}"),
                ),
                Some(false) => (
                    format!("Role {role} already has {node} denied"),
                    format!("Denied {node} to role {role}"),
                ),
                None => (
                    format!("Role {role} has no setting for {node}"),
                    format!("Cleared role {role}'s setting for {node}"),
                ),
            };
            (
                Change::Role {
                    role: role.clone(),
                    pattern: node,
                    held,
                },
                unchanged,
                done,
            )
        }
        "set" | "clear" => {
            let key = string(ctx, "attribute").ok_or_else(unknown)?;
            let attribute = Attribute::from_key(&key).ok_or_else(|| {
                fail(format!(
                    "Unknown role attribute '{key}' (prefix, suffix, name_color, display_name)"
                ))
            })?;
            let value = if action == "set" {
                Some(
                    ctx.arg("value")
                        .and_then(|v| match v {
                            Value::Str(v) => Some(v.clone()),
                            _ => None,
                        })
                        .ok_or_else(unknown)?,
                )
            } else {
                None
            };
            let done = match &value {
                Some(value) => format!("Set role {role}'s {key} to {value}"),
                None => format!("Cleared role {role}'s {key}"),
            };
            (
                Change::Attribute {
                    role: role.clone(),
                    attribute,
                    value,
                },
                format!("Role {role}'s {key} is already that"),
                done,
            )
        }
        "add" | "remove" => {
            let player = string(ctx, "player").ok_or_else(unknown)?;
            let member = action == "add";
            let then: Then =
                Box::new(move |output, user| Box::pin(membership(output, user, role, member)));
            return for_player(ctx, player, then).await;
        }
        _ => return Err(unknown()),
    };
    apply(&Output::of(ctx), change, unchanged, done).await
}

/// Applies `change`, refreshes the command trees of the online players it
/// affects, and answers `done`, or fails with `unchanged`.
async fn apply(
    output: &Output,
    change: Change,
    unchanged: String,
    done: String,
) -> Result<i32, CommandError> {
    let applied = output
        .shared
        .permissions
        .apply(change)
        .await
        .map_err(|err| fail(err.to_string()))?;
    if !applied.changed {
        return Err(fail(unchanged));
    }
    tuff_core::permission::notify(&output.shared.coordinator, &applied.affected).await;
    output.success(text(done), true).await;
    Ok(1)
}

async fn membership(
    output: Output,
    user: NameAndId,
    role: String,
    member: bool,
) -> Result<i32, CommandError> {
    let name = &user.name;
    let (unchanged, done) = if member {
        (
            format!("{name} is already in role {role}"),
            format!("Added {name} to role {role}"),
        )
    } else {
        (
            format!("{name} is not in role {role}"),
            format!("Removed {name} from role {role}"),
        )
    };
    let change = Change::Member {
        uuid: user.uuid,
        name: user.name.clone(),
        role,
        member,
    };
    apply(&output, change, unchanged, done).await
}

/// Runs `then` for the player called `name`: an online player at once,
/// anyone else after a profile lookup, which a player's session does not
/// wait for unless something consumes the result.
async fn for_player(ctx: &mut Context<'_>, name: String, then: Then) -> Result<i32, CommandError> {
    let output = Output::of(ctx);
    let online = targets::players(ctx)
        .await
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(&name));
    if let Some(online) = online {
        let user = NameAndId {
            uuid: online.uuid,
            name: online.name,
        };
        return then(output, user).await;
    }
    let detach = matches!(ctx.actor, Actor::Player(_)) && !ctx.needs_result;
    if !detach {
        let user = crate::look_up(&output, name)
            .await
            .ok_or_else(crate::unknown_player)?;
        return then(output, user).await;
    }
    let task = tokio::spawn(async move {
        let outcome = match crate::look_up(&output, name).await {
            Some(user) => then(output.clone(), user).await,
            None => Err(crate::unknown_player()),
        };
        if let Err(error) = outcome {
            output.fail(*error.message);
        }
    });
    ctx.actor.track(task);
    Ok(1)
}

async fn set(
    output: Output,
    user: NameAndId,
    node: String,
    held: Option<bool>,
) -> Result<i32, CommandError> {
    let name = &user.name;
    let (unchanged, done) = match held {
        Some(true) => (
            format!("{name} already has {node} granted"),
            format!("Granted {node} to {name}"),
        ),
        Some(false) => (
            format!("{name} already has {node} denied"),
            format!("Denied {node} to {name}"),
        ),
        None => (
            format!("{name} has no setting for {node}"),
            format!("Cleared {name}'s setting for {node}"),
        ),
    };
    let change = Change::Player {
        uuid: user.uuid,
        name: user.name.clone(),
        pattern: node,
        held,
    };
    apply(&output, change, unchanged, done).await
}

async fn level_of(output: &Output, uuid: u128) -> PermissionLevel {
    let (reply, level) = tokio::sync::oneshot::channel();
    let _ = output
        .shared
        .coordinator
        .send(CoordinatorMessage::PermissionOf { uuid, reply })
        .await;
    level.await.unwrap_or(PermissionLevel::All)
}

/// Why a decision came out the way it did, for feedback.
fn reason(decision: &Decision) -> String {
    let verb = |held: bool| if held { "granted" } else { "denied" };
    match decision {
        Decision::Server => "console".to_owned(),
        Decision::Explicit { pattern, held } => format!("{} as {pattern}", verb(*held)),
        Decision::Role {
            role,
            pattern,
            held,
        } => format!("{} as {pattern} by role {role}", verb(*held)),
        Decision::OpBypass(at) => format!("operator level {} or above", at.id()),
        Decision::Default(_) => "default".to_owned(),
    }
}

async fn check(output: Output, user: NameAndId, node: String) -> Result<i32, CommandError> {
    let level = level_of(&output, user.uuid).await;
    let subject = Subject::Player {
        uuid: user.uuid,
        level,
    };
    let decision = output.shared.permissions.view().decide(subject, &node);
    let held = decision.held();
    let verb = if held { "has" } else { "lacks" };
    let message = format!("{} {verb} {node} ({})", user.name, reason(&decision));
    output.success(text(message), false).await;
    Ok(i32::from(held))
}

fn settings(nodes: &BTreeMap<String, bool>) -> Vec<String> {
    nodes
        .iter()
        .map(|(pattern, held)| {
            let state = if *held { "granted" } else { "denied" };
            format!("{pattern} ({state})")
        })
        .collect()
}

async fn list(output: Output, user: NameAndId) -> Result<i32, CommandError> {
    let view = output.shared.permissions.view();
    let roles: Vec<&str> = view.roles_of(user.uuid).iter().map(|g| g.0).collect();
    let entries = view
        .player(user.uuid)
        .map(|g| settings(&g.nodes))
        .unwrap_or_default();
    let mut message = if entries.is_empty() {
        format!("{} has no permission settings", user.name)
    } else {
        format!(
            "{} has {} permission setting(s): {}",
            user.name,
            entries.len(),
            entries.join(", ")
        )
    };
    if !roles.is_empty() {
        message.push_str(&format!("; roles: {}", roles.join(", ")));
    }
    output.success(text(message), false).await;
    Ok(entries.len() as i32)
}

fn list_all(ctx: &Context<'_>) -> i32 {
    let view = ctx.shared().permissions.view();
    let mut players: Vec<(&str, usize)> = view
        .grants()
        .players
        .values()
        .filter(|p| !p.nodes.is_empty())
        .map(|p| (p.name.as_str(), p.nodes.len()))
        .collect();
    players.sort_unstable();
    if players.is_empty() {
        ctx.reply(text("No player has permission settings"));
        return 0;
    }
    let names: Vec<String> = players
        .iter()
        .map(|(name, count)| format!("{name} ({count})"))
        .collect();
    ctx.reply(text(format!(
        "{} player(s) have permission settings: {}",
        names.len(),
        names.join(", ")
    )));
    names.len() as i32
}

fn list_roles(ctx: &Context<'_>) -> i32 {
    let view = ctx.shared().permissions.view();
    let mut roles: Vec<(&String, &Role)> = view.grants().roles.iter().collect();
    if roles.is_empty() {
        ctx.reply(text("There are no permission roles"));
        return 0;
    }
    roles.sort_by(|a, b| b.1.priority.cmp(&a.1.priority).then(a.0.cmp(b.0)));
    let names: Vec<String> = roles
        .iter()
        .map(|(name, role)| format!("{name} (priority {})", role.priority))
        .collect();
    ctx.reply(text(format!(
        "{} permission role(s): {}",
        names.len(),
        names.join(", ")
    )));
    names.len() as i32
}

fn list_role(ctx: &Context<'_>, name: &str) -> Result<i32, CommandError> {
    let view = ctx.shared().permissions.view();
    let role = view
        .grants()
        .roles
        .get(name)
        .ok_or_else(|| fail(format!("No permission role named {name}")))?;
    let mut members: Vec<&str> = view
        .grants()
        .players
        .values()
        .filter(|p| p.roles.contains(name))
        .map(|p| p.name.as_str())
        .collect();
    members.sort_unstable();
    let entries = settings(&role.nodes);
    let members = if name == DEFAULT_ROLE {
        "every player".to_owned()
    } else if members.is_empty() {
        "no members".to_owned()
    } else {
        format!("members: {}", members.join(", "))
    };
    let nodes = if entries.is_empty() {
        "no settings".to_owned()
    } else {
        entries.join(", ")
    };
    ctx.reply(text(format!(
        "Role {name} (priority {}, {members}): {nodes}",
        role.priority
    )));
    for attribute in Attribute::ALL {
        if let Some(value) = role.attributes.get(attribute) {
            ctx.reply(
                text(format!("  {}: ", attribute.key())).append(match attribute {
                    Attribute::NameColor => text(value.to_owned()),
                    _ => tuff_core::minimessage::parse(value),
                }),
            );
        }
    }
    Ok(entries.len() as i32)
}

fn nodes(ctx: &Context<'_>) -> i32 {
    let view = ctx.shared().permissions.view();
    let nodes = view.nodes();
    ctx.reply(text(format!("{} permission nodes:", nodes.len())));
    for node in nodes.iter() {
        let default = if node.default { "everyone" } else { "nobody" };
        let operators = if node.op_bypass {
            format!("operators level {}+", node.op_level.id())
        } else {
            "no operator bypass".to_owned()
        };
        ctx.reply(text(format!(
            "{} - {} (default {default}, {operators})",
            node.id, node.description
        )));
    }
    nodes.len() as i32
}
