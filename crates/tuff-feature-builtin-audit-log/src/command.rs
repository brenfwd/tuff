//! `/tuff audit lookup|rollback|restore|purge|status|cancel|help`. Work
//! that reads the file or changes the world runs detached from the
//! caller's session; the console waits for it, so its output is complete
//! when the command returns.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tuff_core::command::args::Value;
use tuff_core::command::{ASK_SERVER, CommandError, CommandNode, CommandNodeKind, ParsedNode};
use tuff_core::world::Shared;
use tuff_core::world::commands::{
    Actor, Context, Executor, ExecutorFuture, SourceKind, SuggestInput, executor_future, text,
};
use tuff_core::world::messages::{FeedbackTarget, PlayerEvent};
use tuff_item::text::{ClickEvent, TextComponent};

use crate::filter::{ACTIONS, Area, FLAGS, Origin, Params, parse};
use crate::record::{Kind, now_ms};
use crate::rollback::{self, Outcome};
use crate::service::{AuditLog, Preview, retention};
use crate::store::{Order, Row};

pub const LOOKUP_NODE: &str = "tuff.command.audit.lookup";
pub const ROLLBACK_NODE: &str = "tuff.command.audit.rollback";
pub const RESTORE_NODE: &str = "tuff.command.audit.restore";
pub const PURGE_NODE: &str = "tuff.command.audit.purge";
pub const STATUS_NODE: &str = "tuff.command.audit.status";

/// Lines a lookup page shows.
pub const PAGE: usize = 10;
/// The radius a player's rollback uses when it names none.
pub const DEFAULT_RADIUS: i32 = 10;

const fn guarded(
    name: &'static str,
    executable: bool,
    requirement: &'static str,
    children: &'static [CommandNode],
) -> CommandNode {
    CommandNode {
        name,
        kind: CommandNodeKind::Literal,
        executable,
        redirect: None,
        permissions: Some(requirement),
        children,
    }
}

const PARAMS: CommandNode = CommandNode {
    name: "params",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:string",
        properties: Some(r#"{"type":"greedy"}"#),
    },
    executable: true,
    redirect: None,
    permissions: None,
    children: &[],
};

const LOOKUP: &str = r#"{"type":"tuff:permission","node":"tuff.command.audit.lookup"}"#;
const ROLLBACK: &str = r#"{"type":"tuff:permission","node":"tuff.command.audit.rollback"}"#;
const RESTORE: &str = r#"{"type":"tuff:permission","node":"tuff.command.audit.restore"}"#;
const PURGE: &str = r#"{"type":"tuff:permission","node":"tuff.command.audit.purge"}"#;
const STATUS: &str = r#"{"type":"tuff:permission","node":"tuff.command.audit.status"}"#;

pub static AUDIT: CommandNode = CommandNode {
    name: "audit",
    kind: CommandNodeKind::Literal,
    executable: false,
    redirect: None,
    permissions: Some(r#"{"type":"tuff:any_child"}"#),
    children: &[
        guarded("cancel", true, ROLLBACK, &[]),
        guarded("confirm", true, ROLLBACK, &[]),
        guarded("inspect", true, LOOKUP, &[]),
        guarded("help", true, LOOKUP, &[]),
        guarded("lookup", true, LOOKUP, &[PARAMS]),
        guarded("near", true, LOOKUP, &[PARAMS]),
        guarded("purge", false, PURGE, &[PARAMS]),
        guarded("restore", false, RESTORE, &[PARAMS]),
        guarded("rollback", false, ROLLBACK, &[PARAMS]),
        guarded("reload", true, PURGE, &[]),
        guarded("status", true, STATUS, &[]),
        guarded("undo", true, ROLLBACK, &[]),
    ],
};

pub const PROVIDERS: &[(&str, &str)] = &[("params", ASK_SERVER)];

fn node_for(sub: &str) -> &'static str {
    match sub {
        "rollback" | "cancel" | "confirm" | "undo" => ROLLBACK_NODE,
        "restore" => RESTORE_NODE,
        "purge" | "reload" => PURGE_NODE,
        "status" => STATUS_NODE,
        _ => LOOKUP_NODE,
    }
}

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::text(text(message.into()))
}

/// Where a detached task's lines go.
#[derive(Clone)]
struct Reply(FeedbackTarget);

impl Reply {
    fn line(&self, line: impl ToString) {
        self.0.send(text(line));
    }

    fn component(&self, line: TextComponent) {
        self.0.send(line);
    }
}

/// Runs `work` off the caller's session; the console tracks it.
fn detach(ctx: &mut Context<'_>, work: impl Future<Output = ()> + Send + 'static) {
    let task = tokio::spawn(work);
    if let Actor::Console(console) = &mut ctx.actor {
        console.track(task);
    }
}

fn origin(ctx: &Context<'_>) -> Origin {
    let shared = ctx.shared();
    let [x, y, z] = ctx.source.position;
    Origin {
        world: shared.dimensions.get(ctx.source.dimension).key.clone(),
        pos: [x.floor() as i32, y.floor() as i32, z.floor() as i32],
    }
}

fn params(ctx: &Context<'_>) -> Result<Params, CommandError> {
    let line = match ctx.arg("params") {
        Some(Value::Str(line)) => line.clone(),
        _ => String::new(),
    };
    parse(&line, &origin(ctx), now_ms()).map_err(fail)
}

fn running(shared: &Arc<Shared>) -> Result<(), CommandError> {
    match shared.services.get::<AuditLog>() {
        Some(log) if log.running() => Ok(()),
        _ => Err(fail("the audit log is not running")),
    }
}

/// The service, from a detached task.
fn service(shared: &Shared) -> &AuditLog {
    shared
        .services
        .get::<AuditLog>()
        .expect("checked before detaching")
}

pub struct AuditCommand;

impl Executor for AuditCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            let sub = ctx.literals().get(2).copied().unwrap_or("help");
            if !ctx.permits(node_for(sub)) {
                return Err(CommandError::simple("command.unknown.command"));
            }
            match sub {
                "help" => {
                    for line in HELP {
                        ctx.reply(text(*line));
                    }
                    Ok(1)
                }
                "status" => status(ctx),
                "cancel" => cancel(ctx),
                "confirm" => confirm(ctx),
                "inspect" => inspect(ctx),
                "lookup" => lookup(ctx, ""),
                "near" => lookup(ctx, NEAR),
                "undo" => undo(ctx),
                "reload" => reload(ctx),
                "rollback" => job(ctx, false),
                "restore" => job(ctx, true),
                "purge" => purge(ctx),
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
        (path.last()?.node.name == "params").then(|| {
            let mut names = input.player_names.to_vec();
            if let Some(log) = input.services.get::<AuditLog>() {
                for name in log.names() {
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            suggest(partial, &names)
        })
    }
}

const HELP: &[&str] = &[
    "/tuff audit lookup <params> - search the log (add a page number)",
    "/tuff audit near [params] - lookup within 5 blocks, last 3 hours",
    "/tuff audit undo - revert your last rollback or restore",
    "/tuff audit reload - re-read the settings",
    "/tuff audit rollback <params> - undo logged block changes",
    "/tuff audit restore <params> - redo rolled back changes",
    "/tuff audit purge t:<age> [r:#world] - delete rows older than age",
    "/tuff audit inspect - toggle the inspector: break or use a block to see its history",
    "/tuff audit rollback|restore <params> #preview, then confirm or cancel",
    "/tuff audit rollback <params> #strict - also take container items back from the player who took them",
    "/tuff audit status | cancel",
    "u:<users,#causes> t:<2w5d7h2m10s or 1h-2h> r:<radius|#global|#world>",
    "a:<block,+block,-block,chat,command,session,...> i:<ids> e:<ids or users>",
    "#count #verbose #silent #preview #strict",
];

fn suggest(partial: &str, players: &[String]) -> Vec<String> {
    let (head, word) = match partial.rfind(' ') {
        Some(at) => partial.split_at(at + 1),
        None => ("", partial),
    };
    let lower = word.to_ascii_lowercase();
    let (prefix, list) = match lower.split_once(':') {
        Some((key, value)) => (format!("{}:", &word[..key.len()]), value),
        None => (String::new(), lower.as_str()),
    };
    let (done, current) = match list.rfind(',') {
        Some(at) => list.split_at(at + 1),
        None => ("", list),
    };
    let candidates: Vec<String> = match lower.split_once(':').map(|(key, _)| key) {
        Some("a" | "action") => ACTIONS.iter().map(|a| (*a).to_owned()).collect(),
        Some("u" | "user" | "users") => {
            let mut names: Vec<String> = players.to_vec();
            names.extend(
                ["#tnt", "#creeper", "#explosion", "#wither", "#end_crystal"]
                    .iter()
                    .map(|c| (*c).to_owned()),
            );
            names
        }
        Some("r" | "radius") => [
            "#global",
            "#world",
            "#overworld",
            "#nether",
            "#end",
            "5",
            "10",
            "50",
        ]
        .iter()
        .map(|r| (*r).to_owned())
        .collect(),
        Some("t" | "time") => {
            let digits: String = current.chars().take_while(char::is_ascii_digit).collect();
            let mut times: Vec<String> = ["30s", "5m", "1h", "1d", "1w"]
                .iter()
                .map(|t| (*t).to_owned())
                .collect();
            if !digits.is_empty() && digits.len() == current.len() {
                times.extend(
                    ["w", "d", "h", "m", "s"]
                        .iter()
                        .map(|u| format!("{digits}{u}")),
                );
            }
            times
        }
        Some(_) => Vec::new(),
        None => ["u:", "t:", "r:", "a:", "i:", "e:"]
            .iter()
            .chain(FLAGS)
            .map(|p| (*p).to_owned())
            .collect(),
    };
    candidates
        .into_iter()
        .filter(|c| c.to_ascii_lowercase().starts_with(current))
        .map(|c| format!("{head}{prefix}{done}{c}"))
        .collect()
}

fn status(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let shared = ctx.shared().clone();
    running(&shared)?;
    let reply = Reply(ctx.source.feedback_target());
    detach(ctx, async move {
        let log = service(&shared);
        let rows = log.total().await.map_or_else(|err| err, |n| n.to_string());
        reply.line(format!(
            "Audit log: {} ({rows} rows)",
            log.path().map_or("-".into(), |p| p.display().to_string())
        ));
        let keep = retention(&shared.config).map_or("forever".to_owned(), |ms| {
            format!("{} days", ms / 86_400_000)
        });
        reply.line(format!(
            "Rows are kept {keep}; {} lost to a full queue",
            log.dropped()
        ));
        if let Some(job) = log.job() {
            reply.line(format!(
                "{} by {}: {}/{} regions",
                if job.restore { "Restore" } else { "Rollback" },
                job.by,
                job.done.load(Ordering::Relaxed),
                job.regions
            ));
        }
    });
    Ok(1)
}

fn cancel(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let shared = ctx.shared().clone();
    running(&shared)?;
    match service(&shared).job() {
        Some(job) => {
            job.cancelled.store(true, Ordering::Relaxed);
            ctx.reply(text("Cancelling; regions already changed stay changed"));
            Ok(1)
        }
        None => match service(&shared).take_preview(&ctx.source.name) {
            Some(preview) => {
                preview.player.send(PlayerEvent::Packets(preview.revert));
                ctx.reply(text("Preview cancelled"));
                Ok(1)
            }
            None => Err(fail("no rollback or restore is running")),
        },
    }
}

fn confirm(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let shared = ctx.shared().clone();
    running(&shared)?;
    let Some(preview) = service(&shared).take_preview(&ctx.source.name) else {
        return Err(fail("you have no preview to confirm"));
    };
    let params = Params {
        page: 1,
        silent: false,
        strict: preview.strict,
        ..Params::default()
    };
    start_job(ctx, params, preview.restore, Some(preview.ids))
}

fn inspect(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let shared = ctx.shared().clone();
    running(&shared)?;
    let Some(id) = ctx.source.player_id() else {
        return Err(fail("only a player can inspect"));
    };
    let on = service(&shared).toggle_inspector(id);
    ctx.reply(text(if on {
        "Inspector on: break or use a block to see its history"
    } else {
        "Inspector off"
    }));
    Ok(1)
}

fn preview(ctx: &mut Context<'_>, params: Params, restore: bool) -> Result<i32, CommandError> {
    let shared = ctx.shared().clone();
    running(&shared)?;
    let SourceKind::Player { handle, .. } = &ctx.source.kind else {
        return Err(fail("only a player can preview"));
    };
    let handle = handle.clone();
    let strict = params.strict;
    let by = ctx.source.name.clone();
    let world = origin(ctx).world;
    let reply = Reply(ctx.source.feedback_target());
    detach(ctx, async move {
        let log = service(&shared);
        let _ = log.sync().await;
        let (filter, order) = rollback::job_filter(params.filter, restore);
        let rows = match log.lookup(filter, order, crate::store::MAX_ROWS, 0).await {
            Ok(rows) => rows,
            Err(err) => return reply.line(format!("Failed: {err}")),
        };
        let (shown, revert) = rollback::preview_packets(&rows, restore, &world);
        let items = rows
            .iter()
            .filter(|row| {
                row.kind
                    .is_some_and(|kind| kind.is_container() || kind == Kind::ItemAdd)
            })
            .count();
        if shown.is_empty() && items == 0 {
            return reply.line("Nothing to preview in your world");
        }
        reply.line(format!(
            "Previewing {} blocks and {items} item rows; /tuff audit confirm to apply or cancel to undo the preview",
            shown.len()
        ));
        handle.send(PlayerEvent::Packets(shown));
        log.set_preview(
            &by,
            Preview {
                restore,
                strict,
                ids: rows.iter().map(|row| row.id).collect(),
                player: handle,
                revert,
            },
        );
    });
    Ok(1)
}

fn age(ms: i64) -> String {
    let seconds = ms.max(0) as f64 / 1000.0;
    if seconds < 60.0 {
        format!("{seconds:.0}s")
    } else if seconds < 3600.0 {
        format!("{:.1}m", seconds / 60.0)
    } else if seconds < 86_400.0 {
        format!("{:.1}h", seconds / 3600.0)
    } else {
        format!("{:.1}d", seconds / 86_400.0)
    }
}

/// One lookup line.
pub fn describe(row: &Row, now: i64, verbose: bool) -> String {
    let what = row
        .material
        .as_deref()
        .map(|m| m.strip_prefix("minecraft:").unwrap_or(m).to_owned());
    let action = match row.kind {
        Some(Kind::BlockBreak) => format!("broke {}", what.unwrap_or_default()),
        Some(Kind::BlockPlace) => format!("placed {}", what.unwrap_or_default()),
        Some(Kind::Chat) => format!("said {}", row.text.as_deref().unwrap_or("")),
        Some(Kind::Command) => format!("ran {}", row.text.as_deref().unwrap_or("")),
        Some(Kind::ContainerAdd) => format!("added {} {}", row.amount, what.unwrap_or_default()),
        Some(Kind::ContainerRemove) => {
            format!("removed {} {}", row.amount, what.unwrap_or_default())
        }
        Some(Kind::Login) => "logged in".to_owned(),
        Some(Kind::Logout) => "logged out".to_owned(),
        Some(kind) => format!("{} {}", kind.name(), what.unwrap_or_default()),
        None => "did something unknown".to_owned(),
    };
    let world = if verbose {
        format!(" {}", row.world)
    } else {
        String::new()
    };
    format!(
        "{}{} ago - {} {} ({} {} {}{world})",
        if row.rolled_back { "[undone] " } else { "" },
        age(now - row.time),
        row.user,
        action.trim_end(),
        row.pos[0],
        row.pos[1],
        row.pos[2],
    )
}

const NEAR: &str = "t:3h r:5 ";

fn teleport(row: &Row) -> TextComponent {
    let [x, y, z] = row.pos;
    text(format!("({x} {y} {z})"))
        .color("aqua")
        .hover_text(text("Click to teleport"))
        .click(ClickEvent::RunCommand(format!(
            "/execute in {} run tp @s {x} {y} {z}",
            row.world
        )))
}

/// A lookup line whose position teleports on click.
pub fn line_component(row: &Row, now: i64, verbose: bool) -> TextComponent {
    let plain = describe(row, now, verbose);
    let (head, _) = plain.rsplit_once(" (").unwrap_or((&plain, ""));
    let mut line = text(format!("{head} ")).append(teleport(row));
    if verbose {
        line = line.append(text(format!(" {}", row.world)));
    }
    line
}

fn page_links(command: &str, page: usize, pages: usize) -> TextComponent {
    let link = |label: &str, to: usize| {
        text(label)
            .color("yellow")
            .click(ClickEvent::RunCommand(format!("{command} {to}")))
    };
    let mut line = text(format!("Page {page}/{pages} "));
    if page > 1 {
        line = line.append(link("[<]", page - 1)).append(text(" "));
    }
    if page < pages {
        line = line.append(link("[>]", page + 1));
    }
    line
}

fn lookup(ctx: &mut Context<'_>, prefix: &str) -> Result<i32, CommandError> {
    let line = match ctx.arg("params") {
        Some(Value::Str(line)) => line.trim().to_owned(),
        _ => String::new(),
    };
    let raw = line.clone();
    let line = format!("{prefix}{line}");
    let params = parse(&line, &origin(ctx), now_ms()).map_err(fail)?;
    let rerun = format!(
        "/tuff audit {} {}",
        if prefix.is_empty() { "lookup" } else { "near" },
        raw.split_whitespace()
            .filter(|word| word.parse::<usize>().is_err())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let shared = ctx.shared().clone();
    running(&shared)?;
    let reply = Reply(ctx.source.feedback_target());
    detach(ctx, async move {
        let log = service(&shared);
        let _ = log.sync().await;
        let total = match log.count(params.filter.clone()).await {
            Ok(total) => total as usize,
            Err(err) => return reply.line(format!("Lookup failed: {err}")),
        };
        if params.count {
            return reply.line(format!("{total} rows"));
        }
        if total == 0 {
            return reply.line("No results");
        }
        let pages = total.div_ceil(PAGE);
        let page = params.page.min(pages);
        let rows = match log
            .lookup(params.filter, Order::NewestFirst, PAGE, (page - 1) * PAGE)
            .await
        {
            Ok(rows) => rows,
            Err(err) => return reply.line(format!("Lookup failed: {err}")),
        };
        reply.line(format!("----- Audit log ({total} rows) -----"));
        let now = now_ms();
        for row in &rows {
            reply.component(line_component(row, now, params.verbose));
        }
        reply.component(page_links(rerun.trim_end(), page, pages));
    });
    Ok(1)
}

fn reload(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let shared = ctx.shared().clone();
    running(&shared)?;
    service(&shared).reload(&shared.config);
    ctx.reply(text("Audit log settings re-read"));
    Ok(1)
}

fn undo(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let shared = ctx.shared().clone();
    running(&shared)?;
    let log = service(&shared);
    if log.job().is_some() {
        return Err(fail("a rollback or restore is already running"));
    }
    let by = ctx.source.name.clone();
    let Some(last) = log.take_undo(&by) else {
        return Err(fail("you have no rollback or restore to undo"));
    };
    let params = Params {
        page: 1,
        silent: false,
        ..Params::default()
    };
    start_job(ctx, params, !last.restore, Some(last.ids))
}

fn job(ctx: &mut Context<'_>, restore: bool) -> Result<i32, CommandError> {
    let mut params = params(ctx)?;
    if !params.has_time {
        return Err(fail("a rollback or restore needs t:, e.g. t:1h"));
    }
    if !params.has_area && ctx.source.player_id().is_some() {
        let origin = origin(ctx);
        params.filter.area = Area::Radius {
            world: origin.world,
            center: origin.pos,
            radius: DEFAULT_RADIUS,
        };
    }
    if params.preview {
        return preview(ctx, params, restore);
    }
    start_job(ctx, params, restore, None)
}

fn start_job(
    ctx: &mut Context<'_>,
    params: Params,
    restore: bool,
    only: Option<Vec<i64>>,
) -> Result<i32, CommandError> {
    let shared = ctx.shared().clone();
    running(&shared)?;
    if service(&shared).job().is_some() {
        return Err(fail("a rollback or restore is already running"));
    }
    let by = ctx.source.name.clone();
    let reply = Reply(ctx.source.feedback_target());
    detach(ctx, async move {
        let log = service(&shared);
        let mut filter = params.filter;
        if let Some(ids) = only {
            filter.row_ids = ids;
        }
        let outcome =
            match rollback::request(&shared, log, filter, restore, params.strict, by).await {
                Ok(outcome) => outcome,
                Err(err) => return reply.line(err),
            };
        if !params.silent || outcome.cancelled || outcome.failed_regions > 0 {
            report(&reply, restore, &outcome);
        }
    });
    Ok(1)
}

fn report(reply: &Reply, restore: bool, outcome: &Outcome) {
    let verb = if restore { "Restored" } else { "Rolled back" };
    let items = if outcome.items > 0 {
        format!(" and {} items", outcome.items)
    } else {
        String::new()
    };
    reply.line(format!(
        "{verb} {} blocks{items} from {} rows in {} regions{}",
        outcome.blocks,
        outcome.rows,
        outcome.regions,
        if outcome.cancelled {
            " (cancelled)"
        } else {
            ""
        }
    ));
    if outcome.failed_regions > 0 {
        reply.line(format!(
            "{} regions could not be changed",
            outcome.failed_regions
        ));
    }
    if outcome.skipped > 0 {
        reply.line(format!(
            "{} rows had unusable data and were skipped",
            outcome.skipped
        ));
    }
    if outcome.refused > 0 {
        reply.line(format!(
            "{} item rows could not be applied: the player no longer holds the items or has no room",
            outcome.refused
        ));
    }
    if outcome.strict_refused > 0 {
        reply.line(format!(
            "{} container rows were not put back: the player no longer holds the items",
            outcome.strict_refused
        ));
    }
    if outcome.truncated {
        reply.line("Only the first rows were used; run it again for the rest");
    }
}

fn purge(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let params = params(ctx)?;
    let before = match params.filter.since {
        Some(before) if params.filter.until.is_none() => before,
        _ => return Err(fail("purge needs one age, e.g. t:30d")),
    };
    let world = match params.filter.area {
        Area::Global => None,
        Area::World(world) => Some(world),
        Area::Radius { .. } => {
            return Err(fail("purge takes r:#world or r:#<dimension>, not a radius"));
        }
    };
    let shared = ctx.shared().clone();
    running(&shared)?;
    let reply = Reply(ctx.source.feedback_target());
    detach(ctx, async move {
        match service(&shared).purge(before, world).await {
            Ok(removed) => reply.line(format!("Purged {removed} rows")),
            Err(err) => reply.line(format!("Purge failed: {err}")),
        }
    });
    Ok(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn completions_finish_the_last_word() {
        let input = ["Alex".to_owned()];
        assert!(suggest("", &input).contains(&"u:".to_owned()));
        assert!(suggest("t:1h a:-b", &input).contains(&"t:1h a:-block".to_owned()));
        assert_eq!(suggest("a:chat,com", &input), ["a:chat,command"]);
        assert!(suggest("u:a", &input).contains(&"u:Alex".to_owned()));
        assert_eq!(suggest("r:#gl", &input), ["r:#global"]);
        assert!(suggest("#c", &input).contains(&"#count".to_owned()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn time_units_and_links_complete() {
        let input = [];
        assert!(suggest("t:5", &input).contains(&"t:5m".to_owned()));
        let links = page_links("/tuff audit lookup u:a", 2, 3).to_json();
        assert!(links.contains("lookup u:a 1") && links.contains("lookup u:a 3"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn rows_read_as_sentences() {
        let row = Row {
            id: 1,
            time: 0,
            user: "Alex".into(),
            kind: Some(Kind::BlockBreak),
            world: "minecraft:overworld".into(),
            pos: [1, 2, 3],
            material: Some("minecraft:stone".into()),
            old: None,
            new: None,
            amount: 1,
            text: None,
            data: None,
            rolled_back: true,
            uuid: None,
        };
        assert_eq!(
            describe(&row, 90_000, false),
            "[undone] 1.5m ago - Alex broke stone (1 2 3)"
        );
    }
}
