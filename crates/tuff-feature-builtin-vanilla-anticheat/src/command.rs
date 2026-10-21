//! `/anticheat`: `alerts [on|off]` toggles the caller's staff alerts,
//! `violations [player]` lists who was flagged or one player's per-check
//! history, `reset <player> [check]` clears a player's levels and
//! `debug <player> [check|all|off]` traces a player's checks. Each
//! subcommand needs its permission node.

use std::time::Instant;

use tuff_core::command::args::{Value, suggest_words};
use tuff_core::command::{
    ASK_SERVER, CommandError, CommandNode, CommandNodeKind, ParsedNode, RootCommand,
};
use tuff_core::world::commands::{
    Context, Executor, ExecutorFuture, SuggestInput, executor_future, text,
};

use crate::checks;
use crate::service::{Board, PlayerStats};

const WORD: &str = r#"{"type":"word"}"#;

const fn literal(
    name: &'static str,
    executable: bool,
    permissions: Option<&'static str>,
    children: &'static [CommandNode],
) -> CommandNode {
    CommandNode {
        name,
        kind: CommandNodeKind::Literal,
        executable,
        redirect: None,
        permissions,
        children,
    }
}

const fn word(name: &'static str, children: &'static [CommandNode]) -> CommandNode {
    CommandNode {
        name,
        kind: CommandNodeKind::Argument {
            parser: "brigadier:string",
            properties: Some(WORD),
        },
        executable: true,
        redirect: None,
        permissions: None,
        children,
    }
}

const ALERTS: CommandNode = literal(
    "alerts",
    true,
    Some(r#"{"type":"tuff:permission","node":"tuff.anticheat.alerts"}"#),
    &[
        literal("off", true, None, &[]),
        literal("on", true, None, &[]),
    ],
);
const VIOLATIONS: CommandNode = literal(
    "violations",
    true,
    Some(r#"{"type":"tuff:permission","node":"tuff.anticheat.command.violations"}"#),
    &[word("player", &[])],
);
const RESET: CommandNode = literal(
    "reset",
    false,
    Some(r#"{"type":"tuff:permission","node":"tuff.anticheat.command.reset"}"#),
    &[word("player", &[word("check", &[])])],
);
const DEBUG: CommandNode = literal(
    "debug",
    false,
    Some(r#"{"type":"tuff:permission","node":"tuff.anticheat.command.debug"}"#),
    &[word("player", &[word("check", &[])])],
);
static ROOT: CommandNode = literal(
    "anticheat",
    false,
    Some(r#"{"type":"tuff:any_child"}"#),
    &[ALERTS, DEBUG, RESET, VIOLATIONS],
);

const PROVIDERS: &[(&str, &str)] = &[("player", ASK_SERVER), ("check", ASK_SERVER)];

pub fn root() -> RootCommand {
    RootCommand::custom("anticheat", &ROOT, PROVIDERS)
}

pub struct Anticheat;

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::text(text(message.into()))
}

fn string(ctx: &Context<'_>, name: &str) -> Option<String> {
    match ctx.arg(name) {
        Some(Value::Str(value)) => Some(value.trim().to_owned()),
        _ => None,
    }
}

fn node_of(sub: &str) -> Option<&'static str> {
    match sub {
        "alerts" => Some(crate::permissions::ALERTS),
        "violations" => Some(crate::permissions::VIOLATIONS),
        "reset" => Some(crate::permissions::RESET),
        "debug" => Some(crate::permissions::DEBUG),
        _ => None,
    }
}

impl Executor for Anticheat {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            let sub = ctx.literals().get(1).copied().unwrap_or_default();
            if node_of(sub).is_none_or(|node| !ctx.permits(node)) {
                return Err(CommandError::simple("command.unknown.command"));
            }
            let shared = ctx.shared().clone();
            let Some(board) = shared.services.get::<Board>() else {
                return Err(fail("The anti-cheat is not running"));
            };
            match sub {
                "alerts" => alerts(ctx, board).await,
                "violations" => match string(ctx, "player") {
                    Some(name) => player(ctx, board, &name).await,
                    None => list(ctx, board).await,
                },
                "reset" => reset(ctx, board).await,
                "debug" => debug(ctx, board),
                _ => Err(CommandError::simple("command.unknown.command")),
            }
        })
    }

    fn suggest(
        &self,
        path: &[ParsedNode],
        partial: &str,
        input: &SuggestInput,
    ) -> Option<Vec<String>> {
        match path.last()?.node.name {
            "violations" | "reset" | "debug" => Some(suggest_words(
                partial,
                input.player_names.iter().map(String::as_str),
            )),
            "player" if path.iter().any(|p| p.node.name == "reset") => Some(suggest_words(
                partial,
                checks::INFOS.iter().map(|info| info.name),
            )),
            "player" if path.iter().any(|p| p.node.name == "debug") => Some(suggest_words(
                partial,
                ["all", "off"]
                    .into_iter()
                    .chain(checks::INFOS.iter().map(|info| info.name)),
            )),
            _ => None,
        }
    }
}

async fn alerts(ctx: &mut Context<'_>, board: &Board) -> Result<i32, CommandError> {
    let Some(session) = ctx.actor.player() else {
        return Err(fail("Only a player can toggle their alerts"));
    };
    let uuid = session.profile.id;
    let wanted = if ctx.has("on") {
        Some(true)
    } else if ctx.has("off") {
        Some(false)
    } else {
        None
    };
    let on = board.set_alerts(uuid, wanted).await;
    ctx.reply(text(if on {
        "Anti-cheat alerts on"
    } else {
        "Anti-cheat alerts off"
    }));
    Ok(i32::from(on))
}

fn ago(at: Instant) -> String {
    let seconds = Instant::now().saturating_duration_since(at).as_secs();
    match seconds {
        0..60 => format!("{seconds}s ago"),
        60..3600 => format!("{}m ago", seconds / 60),
        _ => format!("{}h ago", seconds / 3600),
    }
}

async fn list(ctx: &mut Context<'_>, board: &Board) -> Result<i32, CommandError> {
    let players = board.list().await;
    if players.is_empty() {
        ctx.reply(text("No player has anti-cheat violations"));
        return Ok(0);
    }
    ctx.reply(text(format!(
        "{} player(s) with anti-cheat violations:",
        players.len()
    )));
    let now = Instant::now();
    let mut by_check: Vec<(&str, crate::service::CheckTotals)> =
        crate::service::totals(&players, now).into_iter().collect();
    by_check.sort_by_key(|(_, t)| std::cmp::Reverse(t.flags));
    for (check, t) in by_check {
        ctx.reply(text(format!(
            "{check}: {} flag(s) total, {} in the last hour, {} in the last day, {} kick(s)",
            t.flags, t.hour, t.day, t.kicks
        )));
    }
    for stats in &players {
        let mut worst: Vec<(&str, u32)> = stats
            .checks
            .iter()
            .map(|(check, c)| (*check, c.flags))
            .collect();
        worst.sort_by_key(|(_, flags)| std::cmp::Reverse(*flags));
        let top: Vec<String> = worst
            .iter()
            .take(3)
            .map(|(check, flags)| format!("{check} {flags}"))
            .collect();
        ctx.reply(text(format!(
            "{}: {} flag(s), {} kick(s) ({})",
            stats.name,
            stats.flags(),
            stats.kicks(),
            top.join(", ")
        )));
    }
    Ok(players.len() as i32)
}

fn describe(stats: &PlayerStats) -> Vec<String> {
    stats
        .checks
        .iter()
        .map(|(check, c)| {
            let now = Instant::now();
            format!(
                "{check}: {} flag(s) ({} last hour, {} last day), weight {:.1}, {} kick(s), level {:.1} at the last flag, {}",
                c.flags,
                c.recent.within(now, crate::service::HOUR),
                c.recent.within(now, crate::service::DAY),
                c.weight,
                c.kicks,
                c.level,
                ago(c.last)
            )
        })
        .collect()
}

async fn player(ctx: &mut Context<'_>, board: &Board, name: &str) -> Result<i32, CommandError> {
    let Some(stats) = board.player(name).await else {
        ctx.reply(text(format!("{name} has no anti-cheat violations")));
        return Ok(0);
    };
    ctx.reply(text(format!(
        "{} has {} flag(s) and {} kick(s):",
        stats.name,
        stats.flags(),
        stats.kicks()
    )));
    for line in describe(&stats) {
        ctx.reply(text(line));
    }
    Ok(stats.flags() as i32)
}

async fn reset(ctx: &mut Context<'_>, board: &Board) -> Result<i32, CommandError> {
    let Some(name) = string(ctx, "player") else {
        return Err(CommandError::simple("command.unknown.command"));
    };
    let check = match string(ctx, "check") {
        Some(check) => Some(
            checks::info_of(&check)
                .ok_or_else(|| fail(format!("Unknown anti-cheat check '{check}'")))?
                .name,
        ),
        None => None,
    };
    let Some(found) = board.reset(&name, check).await else {
        return Err(fail(format!("{name} has no anti-cheat violations")));
    };
    let message = match check {
        Some(check) => format!("Reset {found}'s {check} violations"),
        None => format!("Reset {found}'s anti-cheat violations"),
    };
    ctx.success(text(message), true).await;
    Ok(1)
}

fn debug(ctx: &mut Context<'_>, board: &Board) -> Result<i32, CommandError> {
    let Some(name) = string(ctx, "player") else {
        return Err(CommandError::simple("command.unknown.command"));
    };
    let (check, on) = match string(ctx, "check").as_deref() {
        None | Some("all") => (None, true),
        Some("off") => (None, false),
        Some(check) => (
            Some(
                checks::info_of(check)
                    .ok_or_else(|| fail(format!("Unknown anti-cheat check '{check}'")))?
                    .name,
            ),
            true,
        ),
    };
    if !board.set_debug(&name, check, on) {
        return Err(fail("Too many players are traced already"));
    }
    ctx.reply(text(match (on, check) {
        (false, _) => format!("Stopped tracing {name}'s anti-cheat checks"),
        (true, None) => format!("Tracing every anti-cheat check of {name}"),
        (true, Some(check)) => format!("Tracing {name}'s {check} check"),
    }));
    Ok(1)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use super::*;
    use crate::service::{CheckStats, Recent};

    #[tuff_core::watchdog]
    #[test]
    fn describe_shows_hour_and_day_counts() {
        let now = Instant::now();
        let mut recent = Recent::default();
        recent.add(now);
        recent.add(now);
        let stats = PlayerStats {
            player: 1,
            name: Arc::from("Alex"),
            checks: BTreeMap::from([(
                "fly",
                CheckStats {
                    flags: 5,
                    weight: 5.0,
                    kicks: 0,
                    level: 1.0,
                    last: now,
                    recent,
                },
            )]),
        };
        let lines = describe(&stats);
        assert!(
            lines[0].contains("5 flag(s) (2 last hour, 2 last day)"),
            "{lines:?}"
        );
    }
}
