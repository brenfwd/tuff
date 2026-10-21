//! The administration commands a server needs even without vanilla
//! gameplay: stop, save-all, save-off, save-on, op, deop, whitelist, ban,
//! ban-ip, banlist, pardon and pardon-ip. They run in the coordinator, which
//! owns the lists and the world (save-off and save-on in the chunk storage
//! actor, which owns the save gate); this crate only parses arguments and
//! sends the request.

pub mod backup;
pub mod chunk;
mod icon;
mod minecraft_config;
mod perm;
mod restart;
pub mod timings;
mod tuff;
mod web;

use std::sync::Arc;
use std::time::Duration;

use tuff_core::auth::offline_uuid;
use tuff_core::command::args::{ProfileSelector, Value, suggest_words};
use tuff_core::command::{ASK_SERVER, CommandError, ParsedNode};
use tuff_core::feature::{Feature, LoadContext};
use tuff_core::permission::Node;
use tuff_core::registry::Registries;
use tuff_core::world::Shared;
use tuff_core::world::commands::feedback;
use tuff_core::world::commands::targets::{self, resolve_message};
use tuff_core::world::commands::{
    Actor, Context, Executor, ExecutorFuture, Registry, SuggestInput, executor_future,
};
use tuff_core::world::messages::{CoordinatorMessage, FeedbackTarget, ListOp, ListOutcome};
use tuff_item::load::{LoadError, Phase};
use tuff_item::text::TextComponent;
use tuff_storage::admin::NameAndId;

/// Registers the administration commands.
pub struct CoreCommandsFeature;

impl Feature for CoreCommandsFeature {
    fn id(&self) -> &'static str {
        "core-commands"
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            register(&mut cx.registries().commands);
            cx.registries().config_keys.push(
                tuff_core::config::Key::new(
                    tuff_core::restart::MODE_KEY,
                    tuff_core::config::Value::String("self".to_owned()),
                    "What /tuff restart does after saving and stopping: self re-executes the server binary with the same arguments, exit exits with status 75 for a supervisor to restart it",
                )
                .hinted(&["self", "exit"])
                .validated(|v| match v {
                    tuff_core::config::Value::String(s) if tuff_core::restart::Mode::parse(s).is_some() => Ok(()),
                    _ => Err("must be self or exit".to_owned()),
                }),
            );
            use tuff_core::config::{Key, Value};
            use tuff_core::restart as r;
            cx.registries().config_keys.push(Key::new(
                r::BROADCAST_KEY,
                Value::Bool(true),
                "Whether a delayed /tuff restart announces its countdown and cancellation to every player (false: only the source and operators)",
            ));
            cx.registries().config_keys.push(Key::new(
                r::DISCONNECT_MESSAGE_KEY,
                Value::Bool(true),
                "Whether players are disconnected with tuff.restart.disconnect_text on /tuff restart instead of the generic server closed text",
            ));
            cx.registries().config_keys.push(Key::new(
                r::DISCONNECT_TEXT_KEY,
                Value::String(r::DEFAULT_DISCONNECT_TEXT.to_owned()),
                "The disconnect text shown to players on /tuff restart",
            ));
            register_nodes(cx.registries(), self.id())?;
            cx.registries()
                .session_hooks
                .push(Arc::new(chunk::ProgressBar));
        }
        if cx.phase() == Phase::Resolve {
            tuff::merge_added_subcommands(&mut cx.registries().commands);
        }
        Ok(())
    }
}

/// Adds the administration commands to `r`.
pub fn register(r: &mut Registry) {
    let targets = &[("targets", ASK_SERVER)];
    r.vanilla("op", targets, Op);
    r.vanilla("deop", targets, Deop);
    r.vanilla("whitelist", targets, Whitelist);
    r.vanilla("ban", &[], Ban);
    r.vanilla("ban-ip", &[], BanIp);
    r.vanilla("banlist", &[], BanList);
    r.vanilla("pardon", targets, Pardon);
    r.vanilla("pardon-ip", &[], PardonIp);
    r.vanilla("stop", &[], Stop);
    r.vanilla("save-all", &[], SaveAll);
    r.vanilla("perf", &[], Perf);
    r.vanilla("save-off", &[], SaveSwitch { on: false });
    r.vanilla("save-on", &[], SaveSwitch { on: true });
    r.register(tuff::root(), tuff::Tuff::default());
}

/// Registers the permission nodes `/tuff`'s subcommands need.
pub fn register_nodes(r: &mut Registries, feature: &'static str) -> Result<(), LoadError> {
    let nodes = [
        Node::new(
            tuff::CONFIG_NODE,
            "Read and change runtime settings with /tuff config",
        ),
        Node::new(
            tuff::EXECUTE_NODE,
            "Run the commands in a file with /tuff execute",
        ),
        Node::new(
            tuff::TRACING_NODE,
            "Record a player's packets with /tuff tracing",
        ),
        Node::new(
            tuff::RESTART_NODE,
            "Save and restart the server with /tuff restart",
        ),
        Node::new(
            tuff::TIMINGS_NODE,
            "Record and export timings reports with /tuff timings",
        ),
        Node::new(
            backup::CREATE_NODE,
            "Start a world backup with /tuff backup create",
        ),
        Node::new(
            backup::LIST_NODE,
            "List world backups with /tuff backup list",
        ),
        Node::new(
            backup::STATUS_NODE,
            "Show backup progress with /tuff backup status",
        ),
        Node::new(
            backup::CANCEL_NODE,
            "Cancel the running backup with /tuff backup cancel",
        ),
        Node::new(
            backup::DELETE_NODE,
            "Delete a world backup with /tuff backup delete",
        ),
        Node::new(
            chunk::CHUNK_NODE,
            "Pre-generate chunks with /tuff chunk and see job progress",
        ),
        Node::new(
            icon::ICON_NODE,
            "Change the server list icon with /tuff icon",
        ),
        Node::new(
            perm::PERM_NODE,
            "Grant, deny, clear and inspect permission nodes with /tuff perm",
        ),
    ];
    for node in nodes {
        r.permissions.register(feature, node)?;
    }
    Ok(())
}

/// Online player names, for the `targets` argument clients ask the server
/// to complete.
fn suggest_players(partial: &str, input: &SuggestInput) -> Option<Vec<String>> {
    Some(suggest_words(
        partial,
        input.player_names.iter().map(String::as_str),
    ))
}

/// Where a command's feedback goes, owned so a detached task can send it.
#[derive(Clone)]
struct Output {
    shared: Arc<Shared>,
    target: FeedbackTarget,
    source_name: TextComponent,
    source: String,
}

impl Output {
    fn of(ctx: &Context<'_>) -> Self {
        Self {
            shared: ctx.shared().clone(),
            target: ctx.source.feedback_target(),
            source_name: ctx.source.display_name(),
            source: ctx.source.name.clone(),
        }
    }

    async fn success(&self, message: TextComponent, broadcast: bool) {
        feedback::success(
            &self.shared,
            &self.target,
            self.source_name.clone(),
            message,
            broadcast,
        )
        .await;
    }

    fn fail(&self, message: TextComponent) {
        feedback::fail(&self.target, message);
    }
}

/// The players a `game_profile` argument names when that needs no lookup:
/// online players by selector or name. `Err(name)` for a name that is not
/// online.
async fn online_profiles(
    ctx: &mut Context<'_>,
    argument: &str,
) -> Result<Result<Vec<NameAndId>, String>, CommandError> {
    let Some(Value::Profile(profile)) = ctx.arg(argument).cloned() else {
        return Err(CommandError::simple("command.unknown.argument"));
    };
    match profile {
        ProfileSelector::Selector(selector) => Ok(Ok(targets::select_players(ctx, &selector)
            .await?
            .into_iter()
            .map(|p| NameAndId {
                uuid: p.uuid,
                name: p.name,
            })
            .collect())),
        ProfileSelector::Name(name) => Ok(targets::players(ctx)
            .await
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case(&name))
            .map(|online| {
                vec![NameAndId {
                    uuid: online.uuid,
                    name: online.name,
                }]
            })
            .ok_or(name)),
    }
}

/// An offline name as the coordinator knows it, asking the profile API in
/// online mode; in offline mode any name is its offline profile.
async fn look_up(output: &Output, name: String) -> Option<NameAndId> {
    let (reply, known) = tokio::sync::oneshot::channel();
    let _ = output
        .shared
        .coordinator
        .send(CoordinatorMessage::LookupProfile {
            name: name.clone(),
            reply,
        })
        .await;
    if let Ok(Some(profile)) = known.await {
        return Some(profile);
    }
    (!output.shared.online_mode).then(|| NameAndId {
        uuid: offline_uuid(&name),
        name,
    })
}

/// Runs the list change `op` builds for the players `argument` names. A
/// name that is not online may need the profile API. A player's session
/// never waits on the network, so unless something consumes the result
/// the rest runs on its own task; the console still waits, through
/// `track`.
async fn with_profiles(
    ctx: &mut Context<'_>,
    argument: &str,
    op: impl FnOnce(Vec<NameAndId>) -> ListOp + Send + 'static,
) -> Result<i32, CommandError> {
    let output = Output::of(ctx);
    let name = match online_profiles(ctx, argument).await? {
        Ok(users) => return lists(&output, op(users)).await,
        Err(name) => name,
    };
    let detach = matches!(ctx.actor, Actor::Player(_)) && !ctx.needs_result;
    if !detach {
        let user = look_up(&output, name).await.ok_or_else(unknown_player)?;
        return lists(&output, op(vec![user])).await;
    }
    let task = tokio::spawn(async move {
        let outcome = match look_up(&output, name).await {
            Some(user) => lists(&output, op(vec![user])).await,
            None => Err(unknown_player()),
        };
        if let Err(error) = outcome {
            output.fail(*error.message);
        }
    });
    ctx.actor.track(task);
    Ok(1)
}

fn unknown_player() -> CommandError {
    CommandError::simple("argument.player.unknown")
}

/// Applies `op` and sends its feedback. The result is the game's count,
/// or the error the game throws when nothing changed.
async fn lists(output: &Output, op: ListOp) -> Result<i32, CommandError> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    let _ = output
        .shared
        .coordinator
        .send(CoordinatorMessage::Lists {
            source: output.source.clone(),
            op,
            reply,
        })
        .await;
    match answer.await {
        Ok(outcome) => render(output, outcome).await.map_err(CommandError::text),
        Err(_) => Ok(0),
    }
}

fn line(key: &str, args: impl IntoIterator<Item = TextComponent>) -> TextComponent {
    TextComponent::translate(key, args)
}

fn name(user: &NameAndId) -> TextComponent {
    TextComponent::new(user.name.clone())
}

/// One broadcast success line per changed user, or `failed` if none.
async fn each(
    ctx: &Output,
    users: &[NameAndId],
    success: &str,
    failed: &str,
) -> Result<i32, TextComponent> {
    if users.is_empty() {
        return Err(line(failed, []));
    }
    for user in users {
        ctx.success(line(success, [name(user)]), true).await;
    }
    Ok(users.len() as i32)
}

/// Sends the feedback the game gives for what a list change did, and
/// gives its result value or failure message.
async fn render(ctx: &Output, outcome: ListOutcome) -> Result<i32, TextComponent> {
    match outcome {
        ListOutcome::Opped(users) => {
            each(ctx, &users, "commands.op.success", "commands.op.failed").await
        }
        ListOutcome::Deopped(users) => {
            each(ctx, &users, "commands.deop.success", "commands.deop.failed").await
        }
        ListOutcome::WhitelistAdded(users) => {
            each(
                ctx,
                &users,
                "commands.whitelist.add.success",
                "commands.whitelist.add.failed",
            )
            .await
        }
        ListOutcome::WhitelistRemoved(users) => {
            each(
                ctx,
                &users,
                "commands.whitelist.remove.success",
                "commands.whitelist.remove.failed",
            )
            .await
        }
        ListOutcome::Whitelist(names) => {
            let message = if names.is_empty() {
                line("commands.whitelist.none", [])
            } else {
                line(
                    "commands.whitelist.list",
                    [
                        TextComponent::new(names.len().to_string()),
                        TextComponent::new(names.join(", ")),
                    ],
                )
            };
            ctx.success(message, false).await;
            Ok(names.len() as i32)
        }
        ListOutcome::WhitelistEnabled { on, changed: false } => {
            let key = if on {
                "commands.whitelist.alreadyOn"
            } else {
                "commands.whitelist.alreadyOff"
            };
            Err(line(key, []))
        }
        ListOutcome::WhitelistEnabled { on, changed: true } => {
            let key = if on {
                "commands.whitelist.enabled"
            } else {
                "commands.whitelist.disabled"
            };
            ctx.success(line(key, []), true).await;
            Ok(1)
        }
        ListOutcome::WhitelistReloaded => {
            ctx.success(line("commands.whitelist.reloaded", []), true)
                .await;
            Ok(1)
        }
        ListOutcome::Banned(bans) => {
            if bans.is_empty() {
                return Err(line("commands.ban.failed", []));
            }
            let count = bans.len() as i32;
            for (user, reason) in bans {
                ctx.success(line("commands.ban.success", [name(&user), reason]), true)
                    .await;
            }
            Ok(count)
        }
        ListOutcome::IpBanInvalid => Err(line("commands.banip.invalid", [])),
        ListOutcome::IpAlreadyBanned => Err(line("commands.banip.failed", [])),
        ListOutcome::IpBanned {
            ip,
            reason,
            affected,
        } => {
            ctx.success(
                line("commands.banip.success", [TextComponent::new(ip), reason]),
                true,
            )
            .await;
            if !affected.is_empty() {
                let message = line(
                    "commands.banip.info",
                    [
                        TextComponent::new(affected.len().to_string()),
                        TextComponent::new(affected.join(", ")),
                    ],
                );
                ctx.success(message, true).await;
            }
            Ok(affected.len() as i32)
        }
        ListOutcome::BanList(entries) => {
            if entries.is_empty() {
                ctx.success(line("commands.banlist.none", []), false).await;
                return Ok(0);
            }
            let count = entries.len() as i32;
            ctx.success(
                line(
                    "commands.banlist.list",
                    [TextComponent::new(count.to_string())],
                ),
                false,
            )
            .await;
            for (banned, by, reason) in entries {
                let message = line(
                    "commands.banlist.entry",
                    [TextComponent::new(banned), TextComponent::new(by), reason],
                );
                ctx.success(message, false).await;
            }
            Ok(count)
        }
        ListOutcome::Pardoned(users) => {
            each(
                ctx,
                &users,
                "commands.pardon.success",
                "commands.pardon.failed",
            )
            .await
        }
        ListOutcome::IpPardonInvalid => Err(line("commands.pardonip.invalid", [])),
        ListOutcome::IpNotBanned => Err(line("commands.pardonip.failed", [])),
        ListOutcome::IpPardoned(ip) => {
            ctx.success(
                line("commands.pardonip.success", [TextComponent::new(ip)]),
                true,
            )
            .await;
            Ok(1)
        }
    }
}

pub struct Op;

impl Executor for Op {
    fn suggest(
        &self,
        _path: &[ParsedNode],
        partial: &str,
        input: &SuggestInput,
    ) -> Option<Vec<String>> {
        suggest_players(partial, input)
    }

    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move { with_profiles(ctx, "targets", ListOp::Op).await })
    }
}

pub struct Deop;

impl Executor for Deop {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move { with_profiles(ctx, "targets", ListOp::Deop).await })
    }
}

pub struct Whitelist;

impl Executor for Whitelist {
    fn suggest(
        &self,
        _path: &[ParsedNode],
        partial: &str,
        input: &SuggestInput,
    ) -> Option<Vec<String>> {
        suggest_players(partial, input)
    }

    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            let op = match ctx.literals().get(1).copied() {
                Some("add") => return with_profiles(ctx, "targets", ListOp::WhitelistAdd).await,
                Some("remove") => {
                    return with_profiles(ctx, "targets", ListOp::WhitelistRemove).await;
                }
                Some("list") => ListOp::WhitelistList,
                Some("on") => ListOp::WhitelistOn,
                Some("off") => ListOp::WhitelistOff,
                Some("reload") => ListOp::WhitelistReload,
                _ => return Err(CommandError::simple("command.unknown.command")),
            };
            lists(&Output::of(ctx), op).await
        })
    }
}

async fn reason(ctx: &mut Context<'_>) -> Result<Option<String>, CommandError> {
    match ctx.arg("reason").cloned() {
        Some(Value::Message(message)) => Ok(Some(resolve_message(ctx, &message).await?.plain())),
        _ => Ok(None),
    }
}

pub struct Ban;

impl Executor for Ban {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            let reason = reason(ctx).await?;
            with_profiles(ctx, "targets", move |users| ListOp::Ban { users, reason }).await
        })
    }
}

pub struct BanIp;

impl Executor for BanIp {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Str(target)) = ctx.arg("target").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let reason = reason(ctx).await?;
            lists(&Output::of(ctx), ListOp::BanIp { target, reason }).await
        })
    }
}

pub struct BanList;

impl Executor for BanList {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            let literals = ctx.literals();
            let players = !literals.contains(&"ips");
            let ips = !literals.contains(&"players");
            lists(&Output::of(ctx), ListOp::BanList { players, ips }).await
        })
    }
}

pub struct Pardon;

impl Executor for Pardon {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move { with_profiles(ctx, "targets", ListOp::Pardon).await })
    }
}

pub struct PardonIp;

impl Executor for PardonIp {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Str(target)) = ctx.arg("target").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            lists(&Output::of(ctx), ListOp::PardonIp(target)).await
        })
    }
}

pub struct Stop;

impl Executor for Stop {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            ctx.success(line("commands.stop.stopping", []), true).await;
            ctx.coordinator(CoordinatorMessage::Stop).await;
            Ok(1)
        })
    }
}

/// `/perf start` and `/perf stop`: a timings recording whose report is saved
/// when it stops.
pub struct Perf;

impl Executor for Perf {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            use tuff_timings::{RecorderError, StartOptions, TimingsError};
            let timings = ctx.shared().timings.clone();
            if ctx.has("start") {
                let by = ctx.source.name.clone();
                match timings
                    .start(StartOptions {
                        duration: Some(Duration::from_secs(10)),
                        by,
                    })
                    .await
                {
                    Ok(_) => {}
                    Err(TimingsError::Recorder(RecorderError::AlreadyRecording)) => {
                        return Err(CommandError::simple("commands.perf.alreadyRunning"));
                    }
                    Err(err) => {
                        return Err(CommandError::text(TextComponent::new(err.to_string())));
                    }
                }
                ctx.success(line("commands.perf.started", []), false).await;
                return Ok(0);
            }
            let summary = match timings.stop().await {
                Ok(summary) => summary,
                Err(TimingsError::Recorder(RecorderError::NotRecording)) => {
                    return Err(CommandError::simple("commands.perf.notRunning"));
                }
                Err(err) => return Err(CommandError::text(TextComponent::new(err.to_string()))),
            };
            let seconds = summary.duration.as_secs_f64();
            let ticks = summary.ticks;
            let rate = if seconds > 0.0 {
                ticks as f64 / seconds
            } else {
                0.0
            };
            ctx.success(
                line(
                    "commands.perf.stopped",
                    [
                        TextComponent::new(format!("{seconds:.2}")),
                        TextComponent::new(ticks.to_string()),
                        TextComponent::new(format!("{rate:.2}")),
                    ],
                ),
                false,
            )
            .await;
            match summary.file {
                Ok(file) => {
                    ctx.success(
                        line("commands.perf.reportSaved", [TextComponent::new(file)]),
                        false,
                    )
                    .await;
                }
                Err(_) => return Err(CommandError::simple("commands.perf.reportFailed")),
            }
            Ok(0)
        })
    }
}

pub struct SaveAll;

impl Executor for SaveAll {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            ctx.success(line("commands.save.saving", []), false).await;
            let flush = ctx.has("flush");
            let saved = ctx
                .ask(|done| CoordinatorMessage::SaveAll { flush, done })
                .await;
            match saved {
                Some(Ok(())) => {}
                Some(Err(holder)) => {
                    return Err(CommandError::text(TextComponent::new(format!(
                        "Saving is paused by {holder} until the backup finishes"
                    ))));
                }
                None => return Err(CommandError::simple("commands.save.failed")),
            }
            ctx.success(line("commands.save.success", []), true).await;
            Ok(1)
        })
    }
}

/// `/save-on` (`on`) and `/save-off`: automatic saving and unloading of
/// changed chunks.
pub struct SaveSwitch {
    pub on: bool,
}

impl Executor for SaveSwitch {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            let shared = ctx.shared().clone();
            let io = shared.chunk_io.get(&shared.backend);
            if !io.set_autosave(self.on).await {
                return Err(CommandError::simple(if self.on {
                    "commands.save.alreadyOn"
                } else {
                    "commands.save.alreadyOff"
                }));
            }
            let key = if self.on {
                "commands.save.enabled"
            } else {
                "commands.save.disabled"
            };
            ctx.success(line(key, []), true).await;
            Ok(1)
        })
    }
}
