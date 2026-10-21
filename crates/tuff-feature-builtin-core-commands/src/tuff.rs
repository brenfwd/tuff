//! `/tuff`: the server's own administration commands. `config` reads and
//! writes the live runtime settings, completing keys and values from the
//! store as it is right now; `execute` runs the commands in a file;
//! `timings` records span timings into a report (see `crate::timings`);
//! `tracing <player|*> start|stop` records that player's packets (`*`: every connection, one file) to a file in
//! the trace directory (see `tuff_core::trace`); `web` manages web panel
//! logins (see `crate::web`); `perm` manages permission nodes (see
//! `crate::perm`); `backup` makes and manages world backups (see
//! `crate::backup`); `chunk` pre-generates chunks (see `crate::chunk`). Each subcommand needs its permission node, so `/tuff`
//! shows only the subcommands a source may use.

use std::path::{Component, Path, PathBuf};

use tuff_core::command::args::{Value, suggest_words};
use tuff_core::command::{
    ASK_SERVER, CommandError, CommandNode, CommandNodeKind, Parsed, ParsedNode, RootCommand,
};
use tuff_core::config::{ConfigStore, Key};
use tuff_core::world::commands::console::{self, ConsoleRuntime};
use tuff_core::world::commands::registry::TuffSubcommand;
use tuff_core::world::commands::{
    Actor, Context, Executor, ExecutorFuture, Registry, SuggestInput, executor_future, run_counted,
    text,
};
use tuff_item::text::TextComponent;

pub const MAX_FILE_BYTES: u64 = 1 << 20;
pub const MAX_LINES: usize = 10_000;
const STOP_FLAG: &str = "--stop-on-error";

const ANY_CHILD: &str = r#"{"type":"tuff:any_child"}"#;

pub const CONFIG_NODE: &str = "tuff.command.config";
pub const EXECUTE_NODE: &str = "tuff.command.execute";
pub const TRACING_NODE: &str = "tuff.command.tracing";
pub const RESTART_NODE: &str = "tuff.command.restart";
pub const TIMINGS_NODE: &str = "tuff.command.timings";

/// The permission node a subcommand needs; `web` checks per child.
fn subcommand_node(name: &str) -> Option<String> {
    (name != "web" && name != "backup").then(|| format!("tuff.command.{name}"))
}

/// `node` usable only by sources holding the permission named in
/// `requirement` (a `tuff:permission` or `tuff:any_child` requirement).
pub(crate) const fn guarded(node: CommandNode, requirement: &'static str) -> CommandNode {
    CommandNode {
        permissions: Some(requirement),
        ..node
    }
}

pub(crate) const fn any_child(node: CommandNode) -> CommandNode {
    guarded(node, ANY_CHILD)
}

pub(crate) const fn literal(
    name: &'static str,
    executable: bool,
    children: &'static [CommandNode],
) -> CommandNode {
    CommandNode {
        name,
        kind: CommandNodeKind::Literal,
        executable,
        redirect: None,
        permissions: None,
        children,
    }
}

const fn argument(
    name: &'static str,
    properties: &'static str,
    children: &'static [CommandNode],
) -> CommandNode {
    string_argument(name, properties, true, children)
}

pub(crate) const fn string_argument(
    name: &'static str,
    properties: &'static str,
    executable: bool,
    children: &'static [CommandNode],
) -> CommandNode {
    CommandNode {
        name,
        kind: CommandNodeKind::Argument {
            parser: "brigadier:string",
            properties: Some(properties),
        },
        executable,
        redirect: None,
        permissions: None,
        children,
    }
}

const VALUE: CommandNode = argument("value", r#"{"type":"greedy"}"#, &[]);
const KEY: CommandNode = argument("key", r#"{"type":"word"}"#, &[VALUE]);
const CONFIG: CommandNode = guarded(
    literal("config", true, &[KEY]),
    r#"{"type":"tuff:permission","node":"tuff.command.config"}"#,
);
const START: CommandNode = literal("start", true, &[]);
const STOP: CommandNode = literal("stop", true, &[]);
const TRACE_PLAYER: CommandNode = argument("player", r#"{"type":"word"}"#, &[START, STOP]);
const TRACING: CommandNode = guarded(
    literal("tracing", false, &[TRACE_PLAYER]),
    r#"{"type":"tuff:permission","node":"tuff.command.tracing"}"#,
);
const ARGS: CommandNode = argument("args", r#"{"type":"greedy"}"#, &[]);
const EXECUTE: CommandNode = guarded(
    literal("execute", false, &[ARGS]),
    r#"{"type":"tuff:permission","node":"tuff.command.execute"}"#,
);
static TUFF: CommandNode = CommandNode {
    name: "tuff",
    kind: CommandNodeKind::Literal,
    executable: false,
    redirect: None,
    permissions: Some(ANY_CHILD),
    children: &[
        crate::backup::BACKUP,
        crate::chunk::CHUNK,
        CONFIG,
        EXECUTE,
        crate::icon::ICON,
        crate::perm::PERM,
        crate::restart::RESTART,
        crate::timings::TIMINGS,
        TRACING,
        crate::web::WEB,
    ],
};

const PROVIDERS: &[(&str, &str)] = &[
    ("key", ASK_SERVER),
    ("value", ASK_SERVER),
    ("player", ASK_SERVER),
    ("node", ASK_SERVER),
    ("group", ASK_SERVER),
];

pub fn root() -> RootCommand {
    RootCommand::custom("tuff", &TUFF, PROVIDERS)
}

/// Re-registers `/tuff` with the subcommands features added (leaked once per
/// registry, like every root node), sorted in with the built-in ones.
pub fn merge_added_subcommands(r: &mut Registry) {
    let added: Vec<TuffSubcommand> = r.tuff_subcommands().to_vec();
    if added.is_empty() {
        return;
    }
    let mut children: Vec<CommandNode> = TUFF.children.to_vec();
    children.retain(|child| !added.iter().any(|p| p.node.name == child.name));
    children.extend(added.iter().map(|p| *p.node));
    children.sort_by_key(|child| child.name);
    let mut providers: Vec<(&'static str, &'static str)> = PROVIDERS.to_vec();
    for provider in added.iter().flat_map(|p| p.providers) {
        if !providers.iter().any(|(name, _)| *name == provider.0) {
            providers.push(*provider);
        }
    }
    let node: &'static CommandNode = Box::leak(Box::new(CommandNode {
        children: Box::leak(children.into_boxed_slice()),
        ..TUFF
    }));
    r.register(
        RootCommand::custom("tuff", node, Box::leak(providers.into_boxed_slice())),
        Tuff { added },
    );
}

#[derive(Default)]
pub struct Tuff {
    added: Vec<TuffSubcommand>,
}

impl Tuff {
    fn subcommand(&self, name: Option<&str>) -> Option<&TuffSubcommand> {
        let name = name?;
        self.added.iter().find(|p| p.node.name == name)
    }
}

tokio::task_local! {
    static RUNNING_FILE: ();
}

impl Executor for Tuff {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            let sub = ctx.literals().get(1).copied();
            if let Some(subcommand) = self.subcommand(sub) {
                return subcommand.executor.execute(ctx).await;
            }
            if let Some(node) = sub.and_then(subcommand_node)
                && !ctx.permits(&node)
            {
                return Err(CommandError::simple("command.unknown.command"));
            }
            match sub {
                Some("backup") => crate::backup::run(ctx).await,
                Some("chunk") => crate::chunk::run(ctx).await,
                Some("config") => config(ctx).await,
                Some("perm") => crate::perm::run(ctx).await,
                Some("execute") => execute(ctx).await,
                Some("icon") => crate::icon::run(ctx).await,
                Some("restart") => crate::restart::run(ctx).await,
                Some("timings") => crate::timings::run(ctx).await,
                Some("tracing") => tracing(ctx).await,
                Some("web") => crate::web::run(ctx).await,
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
        if let Some(subcommand) = self.subcommand(path.get(1).map(|p| p.node.name)) {
            return subcommand.executor.suggest(path, partial, input);
        }
        let in_group = path.get(2).is_some_and(|p| p.node.name == "group");
        let node_ids = || {
            Some(suggest_words(
                partial,
                input.permissions.nodes().iter().map(|n| n.id.as_str()),
            ))
        };
        match path.last()?.node.name {
            "add" | "delete" | "grant" | "deny" | "revoke" | "list" | "priority" | "remove"
                if in_group =>
            {
                Some(suggest_words(
                    partial,
                    input.permissions.grants().roles.keys().map(String::as_str),
                ))
            }
            "group" if in_group && path.len() == 5 => match path[3].node.name {
                "add" | "remove" => Some(suggest_words(
                    partial,
                    input.player_names.iter().map(String::as_str),
                )),
                "grant" | "deny" | "revoke" => node_ids(),
                _ => None,
            },
            "grant" | "deny" | "revoke" | "check" | "list" => Some(suggest_words(
                partial,
                input.player_names.iter().map(String::as_str),
            )),
            "player" if path.iter().any(|p| p.node.name == "perm") => node_ids(),
            "config" => {
                let mut names: Vec<String> = input.config.names();
                names.extend(crate::minecraft_config::names());
                Some(suggest_words(partial, names.iter().map(String::as_str)))
            }
            "tracing" => {
                let mut names: Vec<&str> = input.player_names.iter().map(String::as_str).collect();
                let armed = input.tracer.armed_names();
                names.extend(armed.iter().map(String::as_str));
                Some(suggest_words(partial, names))
            }
            "key" => {
                let Some(Value::Str(name)) = Parsed::value(path, "key") else {
                    return None;
                };
                if name.starts_with(crate::minecraft_config::PREFIX) {
                    return Some(suggest_words(
                        partial,
                        crate::minecraft_config::value_suggestions(name)
                            .iter()
                            .map(String::as_str),
                    ));
                }
                let key = input.config.definition(name)?;
                Some(suggest_words(
                    partial,
                    value_suggestions(&key, input.config)
                        .iter()
                        .map(String::as_str),
                ))
            }
            _ => None,
        }
    }

    fn describe(
        &self,
        typed: &str,
        candidate: &str,
        input: &SuggestInput,
    ) -> Option<TextComponent> {
        let words: Vec<&str> = typed.strip_prefix('/')?.split(' ').collect();
        if let Some(subcommand) = self.subcommand(words.get(1).copied()) {
            return subcommand.executor.describe(typed, candidate, input);
        }
        if words.get(1) != Some(&"config") {
            return None;
        }
        match words.len() {
            3 => {
                if candidate.starts_with(crate::minecraft_config::PREFIX) {
                    return crate::minecraft_config::describe(candidate).map(text);
                }
                let key = input.config.definition(candidate)?;
                Some(text(format!("{}: {}", kind_name(&key), key.description)))
            }
            n if n >= 4 => {
                let key = input.config.definition(words[2])?;
                (candidate == key.default.to_string()).then(|| text("default"))
            }
            _ => None,
        }
    }
}

async fn tracing(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let Some(Value::Str(player)) = ctx.arg("player").cloned() else {
        return Err(CommandError::simple("command.unknown.command"));
    };
    let tracer = ctx.shared().tracer.clone();
    let starting = match ctx.literals().last().copied() {
        Some("start") => true,
        Some("stop") => false,
        _ => return Err(CommandError::simple("command.unknown.command")),
    };
    let result = if starting {
        tracer.start(&player).await
    } else {
        tracer.stop(&player).await
    };
    let path = result.map_err(fail)?;
    let verb = if starting {
        "Tracing"
    } else {
        "Stopped tracing"
    };
    ctx.reply(text(format!("{verb} {player}: {}", path.display())));
    Ok(1)
}

fn kind_name(key: &Key) -> String {
    format!("{:?}", key.kind()).to_lowercase()
}

fn value_suggestions(key: &Key, store: &ConfigStore) -> Vec<String> {
    let mut out = key.suggestions();
    if key.secret {
        return out;
    }
    for extra in [
        store.get(&key.name).map(|v| v.to_string()),
        Some(key.default.to_string()),
    ]
    .into_iter()
    .flatten()
    {
        if !extra.is_empty() && !out.contains(&extra) {
            out.push(extra);
        }
    }
    out
}

async fn config(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let store = ctx.shared().config.clone();
    let Some(Value::Str(name)) = ctx.arg("key").cloned() else {
        let names = store.names();
        ctx.reply(text(format!(
            "{} config keys: {}",
            names.len(),
            names.join(", ")
        )));
        return Ok(names.len() as i32);
    };
    if name.starts_with(crate::minecraft_config::PREFIX) {
        let raw = match ctx.arg("value").cloned() {
            Some(Value::Str(raw)) => Some(raw),
            Some(_) => return Err(CommandError::simple("command.unknown.argument")),
            None => None,
        };
        return crate::minecraft_config::run(ctx, &name, raw).await;
    }
    let Some(key) = store.definition(&name) else {
        return Err(CommandError::text(text(format!(
            "Unknown config key '{name}'"
        ))));
    };
    match ctx.arg("value").cloned() {
        None => {
            let current = store.get(&name).map(|v| v.to_string()).unwrap_or_default();
            let (current, default) = if key.secret {
                (
                    secret_state(&current),
                    secret_state(&key.default.to_string()),
                )
            } else {
                (current, key.default.to_string())
            };
            ctx.reply(text(format!(
                "{name} = {current} ({}, default '{default}'): {}",
                kind_name(&key),
                key.description
            )));
            Ok(1)
        }
        Some(Value::Str(raw)) => {
            let raw = raw.trim();
            let raw = if raw == "\"\"" { "" } else { raw };
            store
                .set_text(&name, raw)
                .map_err(|err| CommandError::text(text(err.to_string())))?;
            let now = store.get(&name).map(|v| v.to_string()).unwrap_or_default();
            let now = if key.secret { secret_state(&now) } else { now };
            ctx.reply(text(format!("Set {name} to {now}")));
            Ok(1)
        }
        Some(_) => Err(CommandError::simple("command.unknown.argument")),
    }
}

fn secret_state(value: &str) -> String {
    if value.is_empty() { "<unset>" } else { "<set>" }.to_owned()
}

struct Plan {
    file: String,
    console: bool,
    stop: bool,
}

fn plan(args: &str) -> Plan {
    let mut words: Vec<&str> = args.split_whitespace().collect();
    let mut console = false;
    let mut stop = false;
    loop {
        match words.as_slice() {
            [.., "as", "console"] => {
                console = true;
                words.truncate(words.len() - 2);
            }
            [.., last] if *last == STOP_FLAG => {
                stop = true;
                words.pop();
            }
            _ => break,
        }
    }
    let file = words.join(" ");
    let file = file
        .strip_prefix('"')
        .and_then(|f| f.strip_suffix('"'))
        .unwrap_or(&file)
        .to_owned();
    Plan {
        file,
        console,
        stop,
    }
}

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::text(text(message.into()))
}

pub(crate) async fn locate(file: &str, allow_outside: bool) -> Result<PathBuf, CommandError> {
    let relative = Path::new(file);
    if file.is_empty() {
        return Err(fail("No file given"));
    }
    if !allow_outside
        && (relative.is_absolute()
            || relative
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_))))
    {
        return Err(fail("Refusing to read outside the working directory"));
    }
    let cwd = std::env::current_dir().map_err(|e| fail(format!("No working directory: {e}")))?;
    let path = tokio::fs::canonicalize(cwd.join(relative))
        .await
        .map_err(|e| fail(format!("Cannot read {file}: {e}")))?;
    if !allow_outside {
        let root = tokio::fs::canonicalize(&cwd)
            .await
            .map_err(|e| fail(format!("No working directory: {e}")))?;
        if !path.starts_with(&root) {
            return Err(fail("Refusing to read outside the working directory"));
        }
    }
    Ok(path)
}

async fn read_script(path: &Path) -> Result<Vec<String>, CommandError> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| fail(format!("Cannot read file: {e}")))?;
    if !meta.is_file() {
        return Err(fail("Not a file"));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(fail(format!("File is larger than {MAX_FILE_BYTES} bytes")));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| fail(format!("Cannot read file: {e}")))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(fail(format!("File is larger than {MAX_FILE_BYTES} bytes")));
    }
    let text = String::from_utf8(bytes).map_err(|_| fail("File is not valid UTF-8"))?;
    if text.lines().count() > MAX_LINES {
        return Err(fail(format!("File has more than {MAX_LINES} lines")));
    }
    Ok(text.lines().map(str::to_owned).collect())
}

async fn execute(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    if RUNNING_FILE.try_with(|_| ()).is_ok() {
        return Err(fail("tuff execute cannot be nested"));
    }
    let Some(Value::Str(args)) = ctx.arg("args").cloned() else {
        return Err(CommandError::simple("command.unknown.command"));
    };
    let plan = plan(&args);
    let allow_outside = ctx
        .shared()
        .config
        .get_bool(tuff_core::config::EXECUTE_ALLOW_OUTSIDE)
        .unwrap_or(false)
        || tuff_core::server::is_operator_command();
    let path = locate(&plan.file, allow_outside).await?;
    let lines = read_script(&path).await?;
    RUNNING_FILE.scope((), run_lines(ctx, &plan, lines)).await
}

async fn run_lines(
    ctx: &mut Context<'_>,
    plan: &Plan,
    lines: Vec<String>,
) -> Result<i32, CommandError> {
    let shared = ctx.shared().clone();
    let (sink, _unused) = tokio::sync::mpsc::unbounded_channel();
    let mut runtime = ConsoleRuntime::new(shared.clone(), sink);
    let console_source = console::source(&shared, ctx.source.feedback_target());
    let mut ran = 0;
    let mut failed = 0;
    let mut stopped = None;
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix('/').unwrap_or(line);
        let succeeded = if plan.console {
            run_counted(Actor::Console(&mut runtime), console_source.clone(), line).await
        } else {
            let actor = match &mut ctx.actor {
                Actor::Player(session) => Actor::Player(session),
                Actor::Console(console) => Actor::Console(console),
            };
            run_counted(actor, ctx.source.clone(), line).await
        };
        if succeeded == 0 {
            failed += 1;
            ctx.fail(text(format!("Line {}: failed: {line}", index + 1)));
            if plan.stop {
                stopped = Some(index + 1);
                break;
            }
        } else {
            ran += 1;
        }
    }
    runtime.finish().await;
    if let Some(line) = stopped {
        return Err(fail(format!("Stopped at line {line} ({ran} succeeded)")));
    }
    ctx.success(
        text(format!(
            "Executed {} commands from {} ({failed} failed)",
            ran + failed,
            plan.file
        )),
        false,
    )
    .await;
    Ok(ran)
}
