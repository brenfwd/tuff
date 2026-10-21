//! `/scoreboard`, `/team` and `/trigger`: every change is one job run by
//! the scoreboard's actor, after the score holders have been resolved.

use tuff_core::command::CommandError;
use tuff_core::command::args::{ScoreHolderInput, Value};
use tuff_core::world::scoreboard::{
    CollisionRule, Criterion, Holder, NumberFormat, Objective, RenderType, Scoreboard, Team,
    Visibility,
};
use tuff_item::text::TextComponent;
use tuff_storage::admin::{format_uuid, parse_uuid_text};

use super::targets::{self, Target};
use super::{Context, Executor, executor_future, text};

fn bad_argument() -> CommandError {
    CommandError::simple("command.unknown.argument")
}

fn stopped() -> CommandError {
    CommandError::simple("command.failed")
}

/// Items joined by a gray comma, as the game lists names.
pub fn format_list(items: Vec<TextComponent>) -> TextComponent {
    let mut items = items.into_iter();
    let Some(first) = items.next() else {
        return TextComponent::default();
    };
    let rest: Vec<TextComponent> = items.collect();
    if rest.is_empty() {
        return first;
    }
    let mut out = TextComponent::new("").append(first);
    for item in rest {
        out = out
            .append(TextComponent::new(", ").color("gray"))
            .append(item);
    }
    out
}

/// What a selector target is to the scoreboard.
pub fn target_holder(target: &Target) -> Holder {
    match target {
        Target::Player(player) => Holder {
            name: player.name.clone(),
            display: Some(TextComponent::new(player.name.clone())),
        },
        Target::Entity(entity) => Holder {
            name: format_uuid(entity.uuid),
            display: Some(target.display_name()),
        },
    }
}

/// A typed name: hidden names stay names; a UUID is that entity when it
/// is around, and an online player's name is that player.
async fn named_holder(ctx: &mut Context<'_>, name: &str) -> Holder {
    if name.starts_with(tuff_core::world::scoreboard::HIDDEN_PREFIX) {
        return Holder::named(name);
    }
    if let Some(uuid) = parse_uuid_text(name) {
        let players = targets::players(ctx).await;
        if let Some(player) = players.iter().find(|p| p.uuid == uuid) {
            return target_holder(&Target::Player(player.clone()));
        }
        let entities = targets::entities(ctx).await;
        return match entities.into_iter().find(|e| e.uuid == uuid) {
            Some(entity) => target_holder(&Target::Entity(entity)),
            None => Holder::named(name),
        };
    }
    let players = targets::players(ctx).await;
    match players.iter().find(|p| p.name.eq_ignore_ascii_case(name)) {
        Some(player) => target_holder(&Target::Player(player.clone())),
        None => Holder::named(name),
    }
}

/// The holders a `score_holder` argument names. `*` means every holder
/// with a score only where the command allows it.
pub async fn holders(
    ctx: &mut Context<'_>,
    argument: &str,
    wildcard: bool,
) -> Result<Vec<Holder>, CommandError> {
    let Some(Value::ScoreHolder(input)) = ctx.arg(argument) else {
        return Err(bad_argument());
    };
    resolve_holders(ctx, input.clone(), wildcard).await
}

/// [`holders`] for an argument value already in hand.
pub async fn resolve_holders(
    ctx: &mut Context<'_>,
    input: ScoreHolderInput,
    wildcard: bool,
) -> Result<Vec<Holder>, CommandError> {
    let found = match input {
        ScoreHolderInput::Selector(selector) => targets::select_entities(ctx, &selector)
            .await?
            .iter()
            .map(target_holder)
            .collect(),
        ScoreHolderInput::Wildcard => {
            let names = if wildcard {
                ctx.shared()
                    .scoreboard
                    .read(|board| board.holders().map(str::to_owned).collect::<Vec<String>>())
                    .await
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            if names.is_empty() {
                return Err(CommandError::simple("argument.scoreHolder.empty"));
            }
            names.into_iter().map(Holder::named).collect()
        }
        ScoreHolderInput::Name(name) => vec![named_holder(ctx, &name).await],
    };
    if found.is_empty() {
        return Err(CommandError::simple("argument.entity.notfound.entity"));
    }
    Ok(found)
}

pub async fn holder(ctx: &mut Context<'_>, argument: &str) -> Result<Holder, CommandError> {
    Ok(holders(ctx, argument, false).await?.remove(0))
}

fn string_arg(ctx: &Context<'_>, name: &str) -> Result<String, CommandError> {
    match ctx.arg(name) {
        Some(Value::Str(value)) => Ok(value.clone()),
        _ => Err(bad_argument()),
    }
}

async fn resolved_text_arg(
    ctx: &mut Context<'_>,
    name: &str,
) -> Result<Option<TextComponent>, CommandError> {
    match text_arg(ctx, name) {
        Some(component) => Ok(Some(super::resolve::resolve(ctx, &component).await?)),
        None => Ok(None),
    }
}

fn text_arg(ctx: &Context<'_>, name: &str) -> Option<TextComponent> {
    match ctx.arg(name) {
        Some(Value::Component(component)) => Some(component.clone()),
        _ => None,
    }
}

/// The objective an `objective` argument names; `writable` rejects the
/// criteria only the game sets.
pub async fn objective(
    ctx: &Context<'_>,
    argument: &str,
    writable: bool,
) -> Result<Objective, CommandError> {
    objective_named(ctx, string_arg(ctx, argument)?, writable).await
}

/// [`objective`] by name.
pub async fn objective_named(
    ctx: &Context<'_>,
    name: String,
    writable: bool,
) -> Result<Objective, CommandError> {
    let lookup = name.clone();
    let found = ctx
        .shared()
        .scoreboard
        .read(move |board| board.objective(&lookup).cloned())
        .await
        .ok_or_else(stopped)?;
    let Some(objective) = found else {
        return Err(CommandError::dynamic(
            "arguments.objective.notFound",
            [name],
        ));
    };
    if writable && objective.criterion.read_only() {
        return Err(CommandError::dynamic(
            "arguments.objective.readonly",
            [name],
        ));
    }
    Ok(objective)
}

async fn team(ctx: &Context<'_>, argument: &str) -> Result<Team, CommandError> {
    let name = string_arg(ctx, argument)?;
    let lookup = name.clone();
    ctx.shared()
        .scoreboard
        .read(move |board| board.team(&lookup).cloned())
        .await
        .ok_or_else(stopped)?
        .ok_or_else(|| CommandError::dynamic("team.notFound", [name]))
}

async fn update<T: Send + 'static>(
    ctx: &Context<'_>,
    change: impl FnOnce(&mut Scoreboard) -> T + Send + 'static,
) -> Result<T, CommandError> {
    ctx.shared()
        .scoreboard
        .update(change)
        .await
        .ok_or_else(stopped)
}

/// Counts what a command did to each holder, to pick between its single
/// and multiple messages as the game does.
#[derive(Default)]
struct Tracker {
    total: i32,
    count: usize,
    only: Option<Holder>,
    non_zero: usize,
    only_non_zero: Option<Holder>,
}

impl Tracker {
    fn track(&mut self, holder: &Holder, value: i32) {
        self.total = self.total.wrapping_add(value);
        self.count += 1;
        self.only = (self.count == 1).then(|| holder.clone());
        if value != 0 {
            self.non_zero += 1;
            self.only_non_zero = (self.non_zero == 1).then(|| holder.clone());
        }
    }

    /// The single holder and the count, counting every holder (`any`) or
    /// only those with a non-zero value.
    fn pick(&self, any: bool) -> (Option<&Holder>, usize) {
        if any {
            (self.only.as_ref(), self.count)
        } else {
            (self.only_non_zero.as_ref(), self.non_zero)
        }
    }
}

/// Sends the single or multiple message and returns the total; `on_zero`
/// fails the command when nothing counted.
async fn report(
    ctx: &Context<'_>,
    tracker: &Tracker,
    any: bool,
    on_zero: Option<&str>,
    single: impl FnOnce(&Holder, i32) -> TextComponent,
    multiple: impl FnOnce(usize, i32) -> TextComponent,
) -> Result<i32, CommandError> {
    let (only, count) = tracker.pick(any);
    if count == 0
        && let Some(key) = on_zero
    {
        return Err(CommandError::simple(key));
    }
    let message = match only {
        Some(holder) => single(holder, tracker.total),
        None => multiple(count, tracker.total),
    };
    ctx.success(message, true).await;
    Ok(tracker.total)
}

fn number_format(ctx: &Context<'_>, after: &str) -> Option<NumberFormat> {
    let literals = ctx.literals();
    let index = literals.iter().position(|l| *l == after)?;
    match literals.get(index + 1).copied() {
        Some("blank") => Some(NumberFormat::Blank),
        Some("fixed") => Some(NumberFormat::Fixed(text_arg(ctx, "contents")?)),
        Some("styled") => match ctx.arg("style") {
            Some(Value::Style(style)) => Some(NumberFormat::Styled(style.clone())),
            _ => None,
        },
        _ => None,
    }
}

pub struct ScoreboardCommand;

impl Executor for ScoreboardCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let literals = ctx.literals();
            match (literals.get(1).copied(), literals.get(2).copied()) {
                (Some("objectives"), Some(action)) => objectives(ctx, action).await,
                (Some("players"), Some(action)) => players(ctx, action).await,
                _ => Err(CommandError::simple("command.unknown.command")),
            }
        })
    }
}

async fn objectives(ctx: &mut Context<'_>, action: &str) -> Result<i32, CommandError> {
    match action {
        "list" => {
            let names = ctx
                .shared()
                .scoreboard
                .read(|board| {
                    board
                        .objectives()
                        .map(Objective::formatted_display_name)
                        .collect::<Vec<_>>()
                })
                .await
                .ok_or_else(stopped)?;
            let count = names.len() as i32;
            let message = if names.is_empty() {
                TextComponent::translate("commands.scoreboard.objectives.list.empty", [])
            } else {
                TextComponent::translate(
                    "commands.scoreboard.objectives.list.success",
                    [text(count), format_list(names)],
                )
            };
            ctx.success(message, false).await;
            Ok(count)
        }
        "add" => {
            let name = string_arg(ctx, "objective")?;
            let Some(Value::Criterion(criterion)) = ctx.arg("criteria") else {
                return Err(bad_argument());
            };
            let criterion = criterion.clone();
            let display_name = resolved_text_arg(ctx, "displayName")
                .await?
                .unwrap_or_else(|| text(&name));
            let objective = Objective {
                name,
                render_type: criterion.default_render_type(),
                criterion,
                display_name,
                display_auto_update: false,
                number_format: None,
            };
            let formatted = objective.formatted_display_name();
            let count = update(ctx, move |board| {
                board
                    .add_objective(objective)
                    .then(|| board.objectives().count() as i32)
            })
            .await?
            .ok_or_else(|| CommandError::simple("commands.scoreboard.objectives.add.duplicate"))?;
            ctx.success(
                TextComponent::translate("commands.scoreboard.objectives.add.success", [formatted]),
                true,
            )
            .await;
            Ok(count)
        }
        "remove" => {
            let objective = objective(ctx, "objective", false).await?;
            let name = objective.name.clone();
            let count = update(ctx, move |board| {
                board.remove_objective(&name);
                board.objectives().count() as i32
            })
            .await?;
            ctx.success(
                TextComponent::translate(
                    "commands.scoreboard.objectives.remove.success",
                    [objective.formatted_display_name()],
                ),
                true,
            )
            .await;
            Ok(count)
        }
        "setdisplay" => {
            let Some(Value::DisplaySlot(slot)) = ctx.arg("slot") else {
                return Err(bad_argument());
            };
            let slot = *slot;
            let shown = match ctx.arg("objective") {
                Some(_) => Some(objective(ctx, "objective", false).await?),
                None => None,
            };
            let name = shown.as_ref().map(|o| o.name.clone());
            let current = update(ctx, move |board| {
                let current = board.display_objective(slot).map(|o| o.name.clone());
                match (&name, &current) {
                    (None, None) => {
                        return Err("commands.scoreboard.objectives.display.alreadyEmpty");
                    }
                    (Some(new), Some(old)) if new == old => {
                        return Err("commands.scoreboard.objectives.display.alreadySet");
                    }
                    _ => {}
                }
                board.set_display(slot, name.as_deref());
                Ok(())
            })
            .await?;
            current.map_err(CommandError::simple)?;
            let message = match shown {
                Some(objective) => TextComponent::translate(
                    "commands.scoreboard.objectives.display.set",
                    [text(slot.name()), objective.display_name],
                ),
                None => TextComponent::translate(
                    "commands.scoreboard.objectives.display.cleared",
                    [text(slot.name())],
                ),
            };
            ctx.success(message, true).await;
            Ok(0)
        }
        "modify" => modify_objective(ctx).await,
        _ => Err(CommandError::simple("command.unknown.command")),
    }
}

async fn modify_objective(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let objective = objective(ctx, "objective", false).await?;
    let name = objective.name.clone();
    let literals = ctx.literals();
    let what = literals.get(4).copied().unwrap_or_default();
    let message = match what {
        "displayname" => {
            let display = resolved_text_arg(ctx, "displayName")
                .await?
                .ok_or_else(bad_argument)?;
            if display == objective.display_name {
                return Ok(0);
            }
            let changed = display.clone();
            update(ctx, move |board| {
                board.modify_objective(&name, |o| o.display_name = changed);
            })
            .await?;
            let mut renamed = objective.clone();
            renamed.display_name = display;
            TextComponent::translate(
                "commands.scoreboard.objectives.modify.displayname",
                [text(&objective.name), renamed.formatted_display_name()],
            )
        }
        "rendertype" => {
            let render = literals
                .get(5)
                .and_then(|r| RenderType::from_name(r))
                .ok_or_else(bad_argument)?;
            if render == objective.render_type {
                return Ok(0);
            }
            update(ctx, move |board| {
                board.modify_objective(&name, |o| o.render_type = render);
            })
            .await?;
            TextComponent::translate(
                "commands.scoreboard.objectives.modify.rendertype",
                [objective.formatted_display_name()],
            )
        }
        "displayautoupdate" => {
            let Some(Value::Bool(value)) = ctx.arg("value") else {
                return Err(bad_argument());
            };
            let value = *value;
            if value == objective.display_auto_update {
                return Ok(0);
            }
            update(ctx, move |board| {
                board.modify_objective(&name, |o| o.display_auto_update = value);
            })
            .await?;
            let key = if value { "enable" } else { "disable" };
            TextComponent::translate(
                format!("commands.scoreboard.objectives.modify.displayAutoUpdate.{key}"),
                [text(&objective.name), objective.formatted_display_name()],
            )
        }
        "numberformat" => {
            let format = number_format(ctx, "numberformat");
            let set = format.is_some();
            update(ctx, move |board| {
                board.modify_objective(&name, |o| o.number_format = format);
            })
            .await?;
            let key = if set { "set" } else { "clear" };
            TextComponent::translate(
                format!("commands.scoreboard.objectives.modify.objectiveFormat.{key}"),
                [text(&objective.name)],
            )
        }
        _ => return Err(CommandError::simple("command.unknown.command")),
    };
    ctx.success(message, true).await;
    Ok(0)
}

async fn players(ctx: &mut Context<'_>, action: &str) -> Result<i32, CommandError> {
    match action {
        "list" => {
            if ctx.arg("target").is_some() {
                let holder = holder(ctx, "target").await?;
                let name = holder.name.clone();
                let scores = ctx
                    .shared()
                    .scoreboard
                    .read(move |board| {
                        board
                            .scores_of(&name)
                            .into_iter()
                            .map(|(o, value)| (o.formatted_display_name(), value))
                            .collect::<Vec<_>>()
                    })
                    .await
                    .ok_or_else(stopped)?;
                if scores.is_empty() {
                    ctx.success(
                        TextComponent::translate(
                            "commands.scoreboard.players.list.entity.empty",
                            [holder.feedback_name()],
                        ),
                        false,
                    )
                    .await;
                } else {
                    ctx.success(
                        TextComponent::translate(
                            "commands.scoreboard.players.list.entity.success",
                            [holder.feedback_name(), text(scores.len())],
                        ),
                        false,
                    )
                    .await;
                    for (objective, value) in &scores {
                        ctx.success(
                            TextComponent::translate(
                                "commands.scoreboard.players.list.entity.entry",
                                [objective.clone(), text(value)],
                            ),
                            false,
                        )
                        .await;
                    }
                }
                return Ok(scores.len() as i32);
            }
            let names = ctx
                .shared()
                .scoreboard
                .read(|board| board.holders().map(str::to_owned).collect::<Vec<_>>())
                .await
                .ok_or_else(stopped)?;
            let count = names.len() as i32;
            let message = if names.is_empty() {
                TextComponent::translate("commands.scoreboard.players.list.empty", [])
            } else {
                TextComponent::translate(
                    "commands.scoreboard.players.list.success",
                    [text(count), format_list(names.iter().map(text).collect())],
                )
            };
            ctx.success(message, false).await;
            Ok(count)
        }
        "get" => {
            let holder = holder(ctx, "target").await?;
            let objective = objective(ctx, "objective", false).await?;
            let (name, objective_name) = (holder.name.clone(), objective.name.clone());
            let value = ctx
                .shared()
                .scoreboard
                .read(move |board| board.score(&name, &objective_name).map(|s| s.value))
                .await
                .ok_or_else(stopped)?;
            let Some(value) = value else {
                return Err(CommandError::dynamic(
                    "commands.scoreboard.players.get.null",
                    [text(&objective.name), holder.feedback_name()],
                ));
            };
            ctx.success(
                TextComponent::translate(
                    "commands.scoreboard.players.get.success",
                    [
                        holder.feedback_name(),
                        text(value),
                        objective.formatted_display_name(),
                    ],
                ),
                false,
            )
            .await;
            Ok(value)
        }
        "set" | "add" | "remove" => {
            let targets = holders(ctx, "targets", true).await?;
            let objective = objective(ctx, "objective", true).await?;
            let Some(Value::Int(amount)) = ctx.arg("score") else {
                return Err(bad_argument());
            };
            let amount = *amount;
            let name = objective.name.clone();
            let kind = action.to_owned();
            let changed = targets.clone();
            let values = update(ctx, move |board| {
                changed
                    .iter()
                    .map(|holder| match kind.as_str() {
                        "set" => {
                            board.set(holder, &name, amount);
                            amount
                        }
                        "add" => board.add(holder, &name, amount),
                        _ => board.add(holder, &name, amount.wrapping_neg()),
                    })
                    .collect::<Vec<i32>>()
            })
            .await?;
            let mut tracker = Tracker::default();
            for (holder, value) in targets.iter().zip(&values) {
                tracker.track(holder, *value);
            }
            let formatted = objective.formatted_display_name();
            let key = format!("commands.scoreboard.players.{action}.success");
            if action == "set" {
                report(
                    ctx,
                    &tracker,
                    false,
                    None,
                    |holder, _| {
                        TextComponent::translate(
                            format!("{key}.single"),
                            [formatted.clone(), holder.feedback_name(), text(amount)],
                        )
                    },
                    |count, _| {
                        TextComponent::translate(
                            format!("{key}.multiple"),
                            [formatted.clone(), text(count), text(amount)],
                        )
                    },
                )
                .await
            } else {
                report(
                    ctx,
                    &tracker,
                    true,
                    None,
                    |holder, total| {
                        TextComponent::translate(
                            format!("{key}.single"),
                            [
                                text(amount),
                                formatted.clone(),
                                holder.feedback_name(),
                                text(total),
                            ],
                        )
                    },
                    |count, _| {
                        TextComponent::translate(
                            format!("{key}.multiple"),
                            [text(amount), formatted.clone(), text(count)],
                        )
                    },
                )
                .await
            }
        }
        "reset" => {
            let targets = holders(ctx, "targets", true).await?;
            let shown = match ctx.arg("objective") {
                Some(_) => Some(objective(ctx, "objective", false).await?),
                None => None,
            };
            let names: Vec<String> = targets.iter().map(|h| h.name.clone()).collect();
            let objective_name = shown.as_ref().map(|o| o.name.clone());
            update(ctx, move |board| {
                for name in &names {
                    match &objective_name {
                        Some(objective) => board.reset(name, objective),
                        None => board.reset_all(name),
                    }
                }
            })
            .await?;
            let mut tracker = Tracker::default();
            for holder in &targets {
                tracker.track(holder, 1);
            }
            match shown {
                Some(objective) => {
                    let formatted = objective.formatted_display_name();
                    report(
                        ctx,
                        &tracker,
                        false,
                        None,
                        |holder, _| {
                            TextComponent::translate(
                                "commands.scoreboard.players.reset.specific.single",
                                [formatted.clone(), holder.feedback_name()],
                            )
                        },
                        |count, _| {
                            TextComponent::translate(
                                "commands.scoreboard.players.reset.specific.multiple",
                                [formatted.clone(), text(count)],
                            )
                        },
                    )
                    .await
                }
                None => {
                    report(
                        ctx,
                        &tracker,
                        false,
                        None,
                        |holder, _| {
                            TextComponent::translate(
                                "commands.scoreboard.players.reset.all.single",
                                [holder.feedback_name()],
                            )
                        },
                        |count, _| {
                            TextComponent::translate(
                                "commands.scoreboard.players.reset.all.multiple",
                                [text(count)],
                            )
                        },
                    )
                    .await
                }
            }
        }
        "enable" => {
            let targets = holders(ctx, "targets", true).await?;
            let objective = objective(ctx, "objective", false).await?;
            if objective.criterion != Criterion::Trigger {
                return Err(CommandError::simple(
                    "commands.scoreboard.players.enable.invalid",
                ));
            }
            let name = objective.name.clone();
            let names: Vec<String> = targets.iter().map(|h| h.name.clone()).collect();
            let unlocked = update(ctx, move |board| {
                names
                    .iter()
                    .map(|holder| {
                        board.get_or_create(holder, &name);
                        let locked = board.score(holder, &name).is_some_and(|s| s.locked);
                        if locked {
                            board.set_locked(holder, &name, false);
                        }
                        locked
                    })
                    .collect::<Vec<bool>>()
            })
            .await?;
            let mut tracker = Tracker::default();
            for (holder, unlocked) in targets.iter().zip(&unlocked) {
                if *unlocked {
                    tracker.track(holder, 1);
                }
            }
            let formatted = objective.formatted_display_name();
            report(
                ctx,
                &tracker,
                false,
                Some("commands.scoreboard.players.enable.failed"),
                |holder, _| {
                    TextComponent::translate(
                        "commands.scoreboard.players.enable.success.single",
                        [formatted.clone(), holder.feedback_name()],
                    )
                },
                |count, _| {
                    TextComponent::translate(
                        "commands.scoreboard.players.enable.success.multiple",
                        [formatted.clone(), text(count)],
                    )
                },
            )
            .await
        }
        "operation" => {
            let targets = holders(ctx, "targets", true).await?;
            let target_objective = objective(ctx, "targetObjective", true).await?;
            let Some(Value::Operation(operation)) = ctx.arg("operation") else {
                return Err(bad_argument());
            };
            let operation = *operation;
            let sources = holders(ctx, "source", true).await?;
            let source_objective = objective(ctx, "sourceObjective", false).await?;
            let (to, from) = (target_objective.name.clone(), source_objective.name.clone());
            let changed = targets.clone();
            let values = update(ctx, move |board| {
                let mut values = Vec::new();
                for target in &changed {
                    let mut value = board.get_or_create(&target.name, &to);
                    for source in &sources {
                        let other = board.get_or_create(&source.name, &from);
                        let (next, swapped) = operation.apply(value, other)?;
                        board.set(target, &to, next);
                        value = next;
                        if let Some(swapped) = swapped {
                            board.set(source, &from, swapped);
                            value = board.get_or_create(&target.name, &to);
                        }
                    }
                    values.push(value);
                }
                Ok::<_, CommandError>(values)
            })
            .await??;
            let mut tracker = Tracker::default();
            for (holder, value) in targets.iter().zip(&values) {
                tracker.track(holder, *value);
            }
            let formatted = target_objective.formatted_display_name();
            report(
                ctx,
                &tracker,
                true,
                None,
                |holder, total| {
                    TextComponent::translate(
                        "commands.scoreboard.players.operation.success.single",
                        [formatted.clone(), holder.feedback_name(), text(total)],
                    )
                },
                |count, _| {
                    TextComponent::translate(
                        "commands.scoreboard.players.operation.success.multiple",
                        [formatted.clone(), text(count)],
                    )
                },
            )
            .await
        }
        "display" => {
            let literals = ctx.literals();
            let targets = holders(ctx, "targets", true).await?;
            let objective = objective(ctx, "objective", false).await?;
            let name = objective.name.clone();
            let names: Vec<String> = targets.iter().map(|h| h.name.clone()).collect();
            let mut tracker = Tracker::default();
            for holder in &targets {
                tracker.track(holder, 1);
            }
            let formatted = objective.formatted_display_name();
            if literals.get(3) == Some(&"name") {
                let display = text_arg(ctx, "name");
                let shown = display.clone();
                update(ctx, move |board| {
                    for holder in &names {
                        board.set_score_display(holder, &name, shown.clone());
                    }
                })
                .await?;
                return match display {
                    Some(display) => {
                        report(
                            ctx,
                            &tracker,
                            false,
                            None,
                            |holder, _| {
                                TextComponent::translate(
                                    "commands.scoreboard.players.display.name.set.success.single",
                                    [display.clone(), holder.feedback_name(), formatted.clone()],
                                )
                            },
                            |count, _| {
                                TextComponent::translate(
                                    "commands.scoreboard.players.display.name.set.success.multiple",
                                    [display.clone(), text(count), formatted.clone()],
                                )
                            },
                        )
                        .await
                    }
                    None => report(
                        ctx,
                        &tracker,
                        false,
                        None,
                        |holder, _| {
                            TextComponent::translate(
                                "commands.scoreboard.players.display.name.clear.success.single",
                                [holder.feedback_name(), formatted.clone()],
                            )
                        },
                        |count, _| {
                            TextComponent::translate(
                                "commands.scoreboard.players.display.name.clear.success.multiple",
                                [text(count), formatted.clone()],
                            )
                        },
                    )
                    .await,
                };
            }
            let format = number_format(ctx, "objective");
            let key = if format.is_some() { "set" } else { "clear" };
            update(ctx, move |board| {
                for holder in &names {
                    board.set_score_format(holder, &name, format.clone());
                }
            })
            .await?;
            report(
                ctx,
                &tracker,
                false,
                None,
                |holder, _| {
                    TextComponent::translate(
                        format!(
                            "commands.scoreboard.players.display.numberFormat.{key}.success.single"
                        ),
                        [holder.feedback_name(), formatted.clone()],
                    )
                },
                |count, _| {
                    TextComponent::translate(
                        format!(
                            "commands.scoreboard.players.display.numberFormat.{key}.success.multiple"
                        ),
                        [text(count), formatted.clone()],
                    )
                },
            )
            .await
        }
        _ => Err(CommandError::simple("command.unknown.command")),
    }
}

pub struct TeamCommand;

impl Executor for TeamCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let literals = ctx.literals();
            match literals.get(1).copied() {
                Some("list") => list_teams(ctx).await,
                Some("add") => {
                    let name = string_arg(ctx, "team")?;
                    let mut team = Team::new(&name);
                    if let Some(display) = text_arg(ctx, "displayName") {
                        team.display_name = display;
                    }
                    let formatted = team.formatted_display_name();
                    let count = update(ctx, move |board| {
                        board.add_team(team).then(|| board.teams().count() as i32)
                    })
                    .await?
                    .ok_or_else(|| CommandError::simple("commands.team.add.duplicate"))?;
                    ctx.success(
                        TextComponent::translate("commands.team.add.success", [formatted]),
                        true,
                    )
                    .await;
                    Ok(count)
                }
                Some("remove") => {
                    let found = team(ctx, "team").await?;
                    let name = found.name.clone();
                    let count = update(ctx, move |board| {
                        board.remove_team(&name);
                        board.teams().count() as i32
                    })
                    .await?;
                    ctx.success(
                        TextComponent::translate(
                            "commands.team.remove.success",
                            [found.formatted_display_name()],
                        ),
                        true,
                    )
                    .await;
                    Ok(count)
                }
                Some("empty") => {
                    let found = team(ctx, "team").await?;
                    if found.members.is_empty() {
                        return Err(CommandError::simple("commands.team.empty.unchanged"));
                    }
                    let members: Vec<String> = found.members.iter().cloned().collect();
                    let count = members.len() as i32;
                    update(ctx, move |board| {
                        for member in &members {
                            board.leave_team(member);
                        }
                    })
                    .await?;
                    ctx.success(
                        TextComponent::translate(
                            "commands.team.empty.success",
                            [text(count), found.formatted_display_name()],
                        ),
                        true,
                    )
                    .await;
                    Ok(count)
                }
                Some("join") => {
                    let found = team(ctx, "team").await?;
                    let members = if ctx.arg("members").is_some() {
                        holders(ctx, "members", true).await?
                    } else {
                        vec![source_holder(ctx).await?]
                    };
                    let names: Vec<String> = members.iter().map(|h| h.name.clone()).collect();
                    let name = found.name.clone();
                    let joined = update(ctx, move |board| {
                        names
                            .iter()
                            .map(|member| board.join_team(member, &name))
                            .collect::<Vec<bool>>()
                    })
                    .await?;
                    let mut tracker = Tracker::default();
                    for (holder, joined) in members.iter().zip(joined) {
                        tracker.track(holder, i32::from(joined));
                    }
                    let formatted = found.formatted_display_name();
                    report(
                        ctx,
                        &tracker,
                        false,
                        None,
                        |holder, _| {
                            TextComponent::translate(
                                "commands.team.join.success.single",
                                [holder.feedback_name(), formatted.clone()],
                            )
                        },
                        |count, _| {
                            TextComponent::translate(
                                "commands.team.join.success.multiple",
                                [text(count), formatted.clone()],
                            )
                        },
                    )
                    .await
                }
                Some("leave") => {
                    let members = holders(ctx, "members", true).await?;
                    let names: Vec<String> = members.iter().map(|h| h.name.clone()).collect();
                    let left = update(ctx, move |board| {
                        names
                            .iter()
                            .map(|member| board.leave_team(member))
                            .collect::<Vec<bool>>()
                    })
                    .await?;
                    let mut tracker = Tracker::default();
                    for (holder, left) in members.iter().zip(left) {
                        tracker.track(holder, i32::from(left));
                    }
                    report(
                        ctx,
                        &tracker,
                        false,
                        None,
                        |holder, _| {
                            TextComponent::translate(
                                "commands.team.leave.success.single",
                                [holder.feedback_name()],
                            )
                        },
                        |count, _| {
                            TextComponent::translate(
                                "commands.team.leave.success.multiple",
                                [text(count)],
                            )
                        },
                    )
                    .await
                }
                Some("modify") => modify_team(ctx).await,
                _ => Err(CommandError::simple("command.unknown.command")),
            }
        })
    }
}

/// The source entity as a holder, for `/team join <team>` and `/trigger`.
pub(crate) async fn source_holder(ctx: &mut Context<'_>) -> Result<Holder, CommandError> {
    let Some(id) = ctx.source.entity else {
        return Err(CommandError::simple("permissions.requires.entity"));
    };
    if let Some(player) = targets::players(ctx).await.into_iter().find(|p| p.id == id) {
        return Ok(target_holder(&Target::Player(player)));
    }
    targets::entities(ctx)
        .await
        .into_iter()
        .find(|e| e.id == id)
        .map(|entity| target_holder(&Target::Entity(entity)))
        .ok_or_else(|| CommandError::simple("permissions.requires.entity"))
}

async fn list_teams(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    if ctx.arg("team").is_some() {
        let found = team(ctx, "team").await?;
        let count = found.members.len() as i32;
        let message = if found.members.is_empty() {
            TextComponent::translate(
                "commands.team.list.members.empty",
                [found.formatted_display_name()],
            )
        } else {
            let mut members: Vec<&String> = found.members.iter().collect();
            members.sort();
            TextComponent::translate(
                "commands.team.list.members.success",
                [
                    found.formatted_display_name(),
                    text(count),
                    format_list(
                        members
                            .into_iter()
                            .map(|m| text(m).color("green"))
                            .collect(),
                    ),
                ],
            )
        };
        ctx.success(message, false).await;
        return Ok(count);
    }
    let teams = ctx
        .shared()
        .scoreboard
        .read(|board| {
            board
                .teams()
                .map(Team::formatted_display_name)
                .collect::<Vec<_>>()
        })
        .await
        .ok_or_else(stopped)?;
    let count = teams.len() as i32;
    let message = if teams.is_empty() {
        TextComponent::translate("commands.team.list.teams.empty", [])
    } else {
        TextComponent::translate(
            "commands.team.list.teams.success",
            [text(count), format_list(teams)],
        )
    };
    ctx.success(message, false).await;
    Ok(count)
}

async fn modify_team(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let found = team(ctx, "team").await?;
    let literals = ctx.literals();
    let option = literals.get(3).copied().unwrap_or_default();
    let choice = literals.get(4).copied().unwrap_or_default();
    let mut changed = found.clone();
    let (message, broadcast, result) = match option {
        "displayName" => {
            let display = resolved_text_arg(ctx, "displayName")
                .await?
                .ok_or_else(bad_argument)?;
            if display == found.display_name {
                return Err(CommandError::simple("commands.team.option.name.unchanged"));
            }
            changed.display_name = display;
            (
                TextComponent::translate(
                    "commands.team.option.name.success",
                    [changed.formatted_display_name()],
                ),
                true,
                0,
            )
        }
        "color" => {
            if choice == "reset" {
                if found.color.is_none() {
                    return Err(CommandError::simple("commands.team.option.color.unchanged"));
                }
                changed.color = None;
                (
                    TextComponent::translate(
                        "commands.team.option.color.clear.success",
                        [changed.formatted_display_name()],
                    ),
                    true,
                    0,
                )
            } else {
                let Some(Value::TeamColor(color)) = ctx.arg("value") else {
                    return Err(bad_argument());
                };
                if found.color == Some(*color) {
                    return Err(CommandError::simple("commands.team.option.color.unchanged"));
                }
                changed.color = Some(*color);
                (
                    TextComponent::translate(
                        "commands.team.option.color.success",
                        [changed.formatted_display_name(), text(color.name())],
                    ),
                    true,
                    0,
                )
            }
        }
        "friendlyFire" | "seeFriendlyInvisibles" => {
            let Some(Value::Bool(allowed)) = ctx.arg("allowed") else {
                return Err(bad_argument());
            };
            let allowed = *allowed;
            let (current, key) = if option == "friendlyFire" {
                (&mut changed.friendly_fire, "friendlyfire")
            } else {
                (
                    &mut changed.see_friendly_invisibles,
                    "seeFriendlyInvisibles",
                )
            };
            if *current == allowed {
                let state = if allowed {
                    "alreadyEnabled"
                } else {
                    "alreadyDisabled"
                };
                return Err(CommandError::simple(&format!(
                    "commands.team.option.{key}.{state}"
                )));
            }
            *current = allowed;
            let state = if allowed { "enabled" } else { "disabled" };
            (
                TextComponent::translate(
                    format!("commands.team.option.{key}.{state}"),
                    [changed.formatted_display_name()],
                ),
                true,
                0,
            )
        }
        "nametagVisibility" | "deathMessageVisibility" => {
            let visibility = Visibility::from_name(choice).ok_or_else(bad_argument)?;
            let current = if option == "nametagVisibility" {
                &mut changed.name_tag_visibility
            } else {
                &mut changed.death_message_visibility
            };
            if *current == visibility {
                return Err(CommandError::simple(&format!(
                    "commands.team.option.{option}.unchanged"
                )));
            }
            *current = visibility;
            (
                TextComponent::translate(
                    format!("commands.team.option.{option}.success"),
                    [
                        changed.formatted_display_name(),
                        TextComponent::translate(
                            format!("team.visibility.{}", visibility.name()),
                            [],
                        ),
                    ],
                ),
                true,
                0,
            )
        }
        "collisionRule" => {
            let rule = CollisionRule::from_name(choice).ok_or_else(bad_argument)?;
            if changed.collision_rule == rule {
                return Err(CommandError::simple(
                    "commands.team.option.collisionRule.unchanged",
                ));
            }
            changed.collision_rule = rule;
            (
                TextComponent::translate(
                    "commands.team.option.collisionRule.success",
                    [
                        changed.formatted_display_name(),
                        TextComponent::translate(format!("team.collision.{}", rule.name()), []),
                    ],
                ),
                true,
                0,
            )
        }
        "prefix" | "suffix" => {
            let value = text_arg(ctx, option).ok_or_else(bad_argument)?;
            if option == "prefix" {
                changed.prefix = value.clone();
            } else {
                changed.suffix = value.clone();
            }
            (
                TextComponent::translate(format!("commands.team.option.{option}.success"), [value]),
                false,
                1,
            )
        }
        _ => return Err(CommandError::simple("command.unknown.command")),
    };
    let name = found.name.clone();
    update(ctx, move |board| {
        board.modify_team(&name, |team| {
            let members = std::mem::take(&mut team.members);
            *team = changed;
            team.members = members;
        });
    })
    .await?;
    ctx.success(message, broadcast).await;
    Ok(result)
}

pub struct TriggerCommand;

impl Executor for TriggerCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(id) = ctx.source.entity else {
                return Err(CommandError::simple("permissions.requires.player"));
            };
            let Some(player) = targets::players(ctx).await.into_iter().find(|p| p.id == id) else {
                return Err(CommandError::simple("permissions.requires.player"));
            };
            let holder = target_holder(&Target::Player(player));
            let objective = objective(ctx, "objective", false).await?;
            let literals = ctx.literals();
            let mode = literals.get(2).copied();
            let amount = match ctx.arg("value") {
                Some(Value::Int(value)) => *value,
                _ => 1,
            };
            if objective.criterion != Criterion::Trigger {
                return Err(CommandError::simple("commands.trigger.failed.invalid"));
            }
            let name = objective.name.clone();
            let set = mode == Some("set");
            let value = update(ctx, move |board| {
                if board.score(&holder.name, &name).is_none_or(|s| s.locked) {
                    return None;
                }
                board.set_locked(&holder.name, &name, true);
                Some(if set {
                    board.set(&holder, &name, amount);
                    amount
                } else {
                    board.add(&holder, &name, amount)
                })
            })
            .await?
            .ok_or_else(|| CommandError::simple("commands.trigger.failed.unprimed"))?;
            let formatted = objective.formatted_display_name();
            let message = match mode {
                Some("set") => TextComponent::translate(
                    "commands.trigger.set.success",
                    [formatted, text(amount)],
                ),
                Some("add") => TextComponent::translate(
                    "commands.trigger.add.success",
                    [formatted, text(amount)],
                ),
                _ => TextComponent::translate("commands.trigger.simple.success", [formatted]),
            };
            ctx.success(message, true).await;
            Ok(value)
        })
    }
}
