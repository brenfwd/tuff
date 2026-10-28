//! Commands that act on players: give, clear, kill, teleport, experience,
//! effect, enchant, spawnpoint, title, tellraw, playsound, stopsound, kick,
//! msg and list.

use rand::Rng;
use tuff_item::enchantment::EnchantmentDef;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::registry::MobEffect;

use tuff_core::command::args::{Anchor, Value};
use tuff_core::command::coords::Rotation;
use tuff_core::command::selector::Selector;
use tuff_core::command::{CommandError, java_float, java_format_f};
use tuff_core::world::effects::ActiveEffect;
use tuff_core::world::messages::{
    CoordinatorMessage, Directive, DirectiveReply, EffectCommand, EnchantAnswer, LookAt,
    ManagerMessage, PlayerSnapshot, ProfileQuery, Teleport, TitleAction,
};
use tuff_item::text::TextComponent;
use tuff_storage::admin::format_uuid;
use tuff_world_model::BlockPos;

use super::entity_teleport::teleport_entities;
use super::feedback::Messages;
use super::targets::{self, Target, resolve_message};
use super::{Context, Executor, executor_future, text};

const MAX_GIVE_STACKS: i32 = 100;
const DEFAULT_EFFECT_TICKS: i32 = 600;
const INFINITE_EFFECT: i32 = -1;
const SOUND_CATEGORIES: [&str; 11] = [
    "master", "music", "record", "weather", "block", "hostile", "neutral", "player", "ambient",
    "voice", "ui",
];
const MAX_HORIZONTAL: f64 = 30_000_000.0;
const MAX_VERTICAL: f64 = 20_000_000.0;

fn selector<'a>(ctx: &'a Context<'_>, name: &str) -> Option<&'a Selector> {
    match ctx.arg(name) {
        Some(Value::Selector(selector)) => Some(selector),
        _ => None,
    }
}

/// The players an optional targets argument names, defaulting to the
/// source's own player.
async fn target_players(
    ctx: &mut Context<'_>,
    name: &str,
) -> Result<Vec<PlayerSnapshot>, CommandError> {
    match selector(ctx, name).cloned() {
        Some(selector) => targets::select_players(ctx, &selector).await,
        None => Ok(vec![targets::source_player(ctx).await?]),
    }
}

/// The entities an optional targets argument names, defaulting to the
/// source's own entity.
async fn target_entities(ctx: &mut Context<'_>, name: &str) -> Result<Vec<Target>, CommandError> {
    match selector(ctx, name).cloned() {
        Some(selector) => targets::select_entities(ctx, &selector).await,
        None => Ok(vec![Target::Player(targets::source_player(ctx).await?)]),
    }
}

fn players_of(targets: &[Target]) -> Vec<PlayerSnapshot> {
    targets
        .iter()
        .filter_map(|t| match t {
            Target::Player(player) => Some(player.clone()),
            Target::Entity(_) => None,
        })
        .collect()
}

pub fn item_name<M: tuff_item::item::Mode>(
    item: Item,
    components: &tuff_item::item::Components<M>,
) -> TextComponent {
    if let Some(name) = &components.custom_name {
        return (**name).clone();
    }
    let path = item
        .name()
        .strip_prefix("minecraft:")
        .unwrap_or(item.name());
    let kind = if item.block().is_some() {
        "block"
    } else {
        "item"
    };
    TextComponent::translate(format!("{kind}.minecraft.{path}"), [])
}

fn effect_name(effect: MobEffect) -> TextComponent {
    let path = effect
        .name()
        .strip_prefix("minecraft:")
        .unwrap_or(effect.name());
    TextComponent::translate(format!("effect.minecraft.{path}"), [])
}

/// The enchantment's name with its level, as tooltips show it.
pub fn enchantment_full_name(id: &str, enchantment: &EnchantmentDef, level: i32) -> TextComponent {
    let name = TextComponent::from_json_str(&enchantment.description)
        .unwrap_or_else(|| TextComponent::new(id));
    if level == 1 && enchantment.max_level == 1 {
        return name;
    }
    name.append(" ").append(TextComponent::translate(
        format!("enchantment.level.{level}"),
        [],
    ))
}

fn out_of_bounds(position: [f64; 3]) -> bool {
    position[0].abs() >= MAX_HORIZONTAL
        || position[2].abs() >= MAX_HORIZONTAL
        || position[1].abs() >= MAX_VERTICAL
}

/// A `block_pos` argument checked as a spawnable position.
fn spawnable_pos(ctx: &Context<'_>, name: &str) -> Result<BlockPos, CommandError> {
    let Some(Value::Coordinates(coordinates)) = ctx.arg(name) else {
        let position = ctx.source.position;
        return Ok(BlockPos::new(
            position[0].floor() as i32,
            position[1].floor() as i32,
            position[2].floor() as i32,
        ));
    };
    let [x, y, z] = coordinates.block(&ctx.source.pose());
    let horizontal = -MAX_HORIZONTAL..MAX_HORIZONTAL;
    if !horizontal.contains(&f64::from(x)) || !horizontal.contains(&f64::from(z)) {
        return Err(CommandError::simple("argument.pos.outofbounds"));
    }
    Ok(BlockPos::new(x, y, z))
}

pub struct Give;

impl Executor for Give {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::ItemStack(input)) = ctx.arg("item").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let count = match ctx.arg("count") {
                Some(Value::Int(count)) => *count,
                _ => 1,
            };
            let stack = input.template(1);
            let max = i32::from(stack.max_stack_size()) * MAX_GIVE_STACKS;
            let name = item_name(input.item, &input.components);
            if count > max {
                return Err(CommandError::dynamic(
                    "commands.give.failed.toomanyitems",
                    [text(max), name],
                ));
            }
            let players = target_players(ctx, "targets").await?;
            let single_name = name.clone();
            let messages = Messages::new(
                move |reply| {
                    TextComponent::translate(
                        "commands.give.success.single",
                        [text(count), single_name.clone(), text(&reply.name)],
                    )
                },
                move |players, _| {
                    TextComponent::translate(
                        "commands.give.success.multiple",
                        [text(count), name.clone(), text(players)],
                    )
                },
            );
            targets::dispatch(
                ctx,
                &players,
                |_| Directive::Give {
                    stack: stack.clone(),
                    count,
                },
                messages,
            )
            .await;
            Ok(count)
        })
    }
}

pub struct Clear;

impl Executor for Clear {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let predicate = match ctx.arg("item") {
                Some(Value::ItemPredicate(predicate)) => Some(
                    predicate
                        .clone()
                        .resolved(&ctx.shared().datapacks.contents().tags)?,
                ),
                _ => None,
            };
            let max_count = match ctx.arg("maxCount") {
                Some(Value::Int(count)) => *count,
                _ => -1,
            };
            let players = target_players(ctx, "targets").await?;
            let counting = max_count == 0;
            let (single_key, multiple_key) = if counting {
                ("commands.clear.test.single", "commands.clear.test.multiple")
            } else {
                (
                    "commands.clear.success.single",
                    "commands.clear.success.multiple",
                )
            };
            let messages = Messages::new(
                move |reply| {
                    TextComponent::translate(single_key, [text(reply.value), text(&reply.name)])
                },
                move |players, total| {
                    TextComponent::translate(multiple_key, [text(total), text(players)])
                },
            )
            .on_zero(|replies: &[DirectiveReply]| match replies {
                [only] => TextComponent::translate("clear.failed.single", [text(&only.name)]),
                _ => TextComponent::translate("clear.failed.multiple", [text(replies.len())]),
            });
            targets::dispatch_result(
                ctx,
                &players,
                |_| Directive::Clear {
                    predicate: predicate.clone(),
                    max_count,
                },
                messages,
            )
            .await
        })
    }
}

pub struct Kill;

impl Executor for Kill {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let found = target_entities(ctx, "targets").await?;
            let players = players_of(&found);
            let entities: Vec<&Target> = found
                .iter()
                .filter(|t| matches!(t, Target::Entity(_)))
                .collect();
            let mut killed = 0usize;
            let mut only_entity: Option<TextComponent> = None;
            if !entities.is_empty() {
                let (reply, regions) = tokio::sync::oneshot::channel();
                let _ = ctx
                    .shared()
                    .manager
                    .send(ManagerMessage::Regions { reply })
                    .await;
                let ids: Vec<i32> = entities.iter().map(|t| t.id()).collect();
                for region in regions.await.unwrap_or_default() {
                    let ids = ids.clone();
                    killed += region
                        .ask(move |world| {
                            ids.into_iter().filter(|id| world.kill_entity(*id)).count()
                        })
                        .await
                        .unwrap_or(0);
                }
                if entities.len() == 1 {
                    only_entity = Some(ctx.name_of(entities[0]));
                }
            }
            let single = |name: TextComponent| {
                TextComponent::translate("commands.kill.success.single", [name])
            };
            let multiple = |count: usize| {
                TextComponent::translate("commands.kill.success.multiple", [text(count)])
            };
            if players.is_empty() {
                let message = match only_entity {
                    Some(name) if killed == 1 => single(name),
                    _ => multiple(killed),
                };
                ctx.success(message, true).await;
                return Ok(killed as i32);
            }
            let messages = Messages::new(
                move |reply| {
                    if killed == 0 {
                        single(text(&reply.name))
                    } else {
                        multiple(killed + 1)
                    }
                },
                move |count, _| multiple(count + killed),
            );
            targets::dispatch(ctx, &players, |_| Directive::Kill, messages).await;
            Ok(1)
        })
    }
}

pub struct TeleportCommand;

impl Executor for TeleportCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let found = match selector(ctx, "targets").cloned() {
                Some(selector) => targets::select_entities(ctx, &selector).await?,
                None => vec![Target::Player(targets::source_player(ctx).await?)],
            };
            let players = players_of(&found);
            let entities: Vec<&Target> = found
                .iter()
                .filter(|t| matches!(t, Target::Entity(_)))
                .collect();
            if players.is_empty() && entities.is_empty() {
                return Err(CommandError::simple("argument.entity.notfound.entity"));
            }
            let only_entity_name = (entities.len() == 1).then(|| ctx.name_of(entities[0]));
            let dimension = ctx.source.dimension;
            if let Some(destination) = selector(ctx, "destination").cloned() {
                let target = targets::select_entity(ctx, &destination).await?;
                let teleport = Teleport {
                    dimension: target.dimension(),
                    position: target.position(),
                    relative: [false; 3],
                    rotation: Some(target.rotation()),
                    relative_rotation: [false; 2],
                    look_at: None,
                };
                let moved = teleport_entities(ctx, &entities, &teleport).await;
                let name = ctx.name_of(&target);
                let single = {
                    let name = name.clone();
                    move |mover: TextComponent| {
                        TextComponent::translate(
                            "commands.teleport.success.entity.single",
                            [mover, name.clone()],
                        )
                    }
                };
                let multiple = move |count: usize| {
                    TextComponent::translate(
                        "commands.teleport.success.entity.multiple",
                        [text(count), name.clone()],
                    )
                };
                if players.is_empty() {
                    let message = match only_entity_name {
                        Some(entity_name) if moved == 1 => single(entity_name),
                        _ => multiple(moved),
                    };
                    ctx.success(message, true).await;
                    return Ok(moved as i32);
                }
                let messages = Messages::new(
                    {
                        let multiple = multiple.clone();
                        move |reply| {
                            if moved == 0 {
                                single(text(&reply.name))
                            } else {
                                multiple(moved + 1)
                            }
                        }
                    },
                    move |count, _| multiple(count + moved),
                );
                targets::dispatch(
                    ctx,
                    &players,
                    |_| Directive::Teleport(teleport.clone()),
                    messages,
                )
                .await;
                return Ok((players.len() + moved) as i32);
            }
            let Some(Value::Coordinates(location)) = ctx.arg("location").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let pose = ctx.source.pose();
            let position = location.position(&pose);
            if out_of_bounds(position) {
                return Err(CommandError::simple("commands.teleport.invalidPosition"));
            }
            let rotation = match ctx.arg("rotation") {
                Some(Value::Rotation(rotation)) => Some(rotation.get(ctx.source.rotation)),
                _ => None,
            };
            let look_at = if let Some(Value::Coordinates(facing)) = ctx.arg("facingLocation") {
                Some(LookAt {
                    point: facing.position(&pose),
                    from: ctx.source.anchor,
                })
            } else if let Some(facing) = selector(ctx, "facingEntity").cloned() {
                let target = targets::select_entity(ctx, &facing).await?;
                let anchor = match ctx.arg("facingAnchor") {
                    Some(Value::Anchor(anchor)) => *anchor,
                    _ => Anchor::Feet,
                };
                Some(LookAt {
                    point: match anchor {
                        Anchor::Feet => target.position(),
                        Anchor::Eyes => targets::eye_of(&target),
                    },
                    from: ctx.source.anchor,
                })
            } else {
                None
            };
            let teleport = Teleport {
                dimension,
                position,
                relative: location.is_relative(),
                rotation,
                relative_rotation: [false; 2],
                look_at,
            };
            let moved = teleport_entities(ctx, &entities, &teleport).await;
            let coordinates: Vec<TextComponent> =
                position.iter().map(|c| text(java_format_f(*c))).collect();
            let single = {
                let coordinates = coordinates.clone();
                move |mover: TextComponent| {
                    let mut args = vec![mover];
                    args.extend(coordinates.iter().cloned());
                    TextComponent::translate("commands.teleport.success.location.single", args)
                }
            };
            let multiple = move |count: usize| {
                let mut args = vec![text(count)];
                args.extend(coordinates.iter().cloned());
                TextComponent::translate("commands.teleport.success.location.multiple", args)
            };
            if players.is_empty() {
                let message = match only_entity_name {
                    Some(entity_name) if moved == 1 => single(entity_name),
                    _ => multiple(moved),
                };
                ctx.success(message, true).await;
                return Ok(moved as i32);
            }
            let messages = Messages::new(
                {
                    let multiple = multiple.clone();
                    move |reply| {
                        if moved == 0 {
                            single(text(&reply.name))
                        } else {
                            multiple(moved + 1)
                        }
                    }
                },
                move |count, _| multiple(count + moved),
            );
            targets::dispatch(
                ctx,
                &players,
                |_| Directive::Teleport(teleport.clone()),
                messages,
            )
            .await;
            Ok((players.len() + moved) as i32)
        })
    }
}

pub struct ExperienceCommand;

impl Executor for ExperienceCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let literals = ctx.literals();
            let levels = literals.contains(&"levels");
            let kind = if levels { "levels" } else { "points" };
            let Some(selector) = selector(ctx, "target").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            if literals.contains(&"query") {
                let player = targets::select_player(ctx, &selector).await?;
                let key = format!("commands.experience.query.{kind}");
                let messages = Messages::new(
                    move |reply| {
                        TextComponent::translate(
                            key.clone(),
                            [text(&reply.name), text(reply.value)],
                        )
                    },
                    |_, _| TextComponent::new(""),
                )
                .any()
                .quiet();
                return targets::dispatch_result(
                    ctx,
                    &[player],
                    |_| Directive::ExperienceQuery { levels },
                    messages,
                )
                .await;
            }
            let amount = match ctx.arg("amount") {
                Some(Value::Int(amount)) => *amount,
                _ => return Err(CommandError::simple("command.unknown.argument")),
            };
            let set = literals.contains(&"set");
            let players = targets::select_players(ctx, &selector).await?;
            let verb = if set { "set" } else { "add" };
            let single_key = format!("commands.experience.{verb}.{kind}.success.single");
            let multiple_key = format!("commands.experience.{verb}.{kind}.success.multiple");
            let mut messages = Messages::new(
                move |reply| {
                    TextComponent::translate(single_key.clone(), [text(amount), text(&reply.name)])
                },
                move |count, _| {
                    TextComponent::translate(multiple_key.clone(), [text(amount), text(count)])
                },
            );
            if set {
                messages = messages.on_zero(|_| {
                    TextComponent::translate("commands.experience.set.points.invalid", [])
                });
            }
            targets::dispatch_result(
                ctx,
                &players,
                |_| Directive::Experience {
                    amount,
                    levels,
                    set,
                },
                messages,
            )
            .await
        })
    }
}

pub struct Effect;

type SingleText = std::sync::Arc<dyn Fn(TextComponent) -> TextComponent + Send + Sync>;
type MultipleText = std::sync::Arc<dyn Fn(usize) -> TextComponent + Send + Sync>;

impl Executor for Effect {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let literals = ctx.literals();
            let effect = match ctx.arg("effect") {
                Some(Value::Resource(name)) => Some(
                    MobEffect::from_name(name)
                        .ok_or_else(|| CommandError::simple("command.unknown.argument"))?,
                ),
                _ => None,
            };
            let found = target_entities(ctx, "targets").await?;
            if literals.contains(&"clear") {
                let (single, multiple, failed): (SingleText, MultipleText, _) = match effect {
                    None => (
                        std::sync::Arc::new(|name| {
                            TextComponent::translate(
                                "commands.effect.clear.everything.success.single",
                                [name],
                            )
                        }),
                        std::sync::Arc::new(|count| {
                            TextComponent::translate(
                                "commands.effect.clear.everything.success.multiple",
                                [text(count)],
                            )
                        }),
                        "commands.effect.clear.everything.failed",
                    ),
                    Some(effect) => (
                        std::sync::Arc::new(move |name| {
                            TextComponent::translate(
                                "commands.effect.clear.specific.success.single",
                                [effect_name(effect), name],
                            )
                        }),
                        std::sync::Arc::new(move |count| {
                            TextComponent::translate(
                                "commands.effect.clear.specific.success.multiple",
                                [effect_name(effect), text(count)],
                            )
                        }),
                        "commands.effect.clear.specific.failed",
                    ),
                };
                return apply_effect(
                    ctx,
                    &found,
                    EffectCommand::Clear(effect),
                    Directive::EffectClear(effect),
                    single,
                    multiple,
                    failed,
                )
                .await;
            }
            let Some(effect) = effect else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let instantaneous = matches!(
                effect,
                MobEffect::InstantHealth | MobEffect::InstantDamage | MobEffect::Saturation
            );
            let duration = if literals.contains(&"infinite") {
                INFINITE_EFFECT
            } else {
                match ctx.arg("seconds") {
                    Some(Value::Int(seconds)) if instantaneous => *seconds,
                    Some(Value::Int(seconds)) => *seconds * 20,
                    _ if instantaneous => 1,
                    _ => DEFAULT_EFFECT_TICKS,
                }
            };
            let amplifier = match ctx.arg("amplifier") {
                Some(Value::Int(amplifier)) => *amplifier as u8,
                _ => 0,
            };
            let particles = match ctx.arg("hideParticles") {
                Some(Value::Bool(hide)) => !hide,
                _ => true,
            };
            let seconds = duration / 20;
            let mut active = ActiveEffect::timed(effect, i64::from(duration), amplifier);
            active.show_particles = particles;
            apply_effect(
                ctx,
                &found,
                EffectCommand::Give(active),
                Directive::EffectGive {
                    effect,
                    duration,
                    amplifier,
                    particles,
                },
                std::sync::Arc::new(move |name| {
                    TextComponent::translate(
                        "commands.effect.give.success.single",
                        [effect_name(effect), name, text(seconds)],
                    )
                }),
                std::sync::Arc::new(move |count| {
                    TextComponent::translate(
                        "commands.effect.give.success.multiple",
                        [effect_name(effect), text(count), text(seconds)],
                    )
                }),
                "commands.effect.give.failed",
            )
            .await
        })
    }
}

/// Runs `/effect` on players (as directives) and on other entities (in
/// their regions), with one piece of feedback over both.
async fn apply_effect(
    ctx: &mut Context<'_>,
    found: &[Target],
    command: EffectCommand,
    directive: Directive,
    single: SingleText,
    multiple: MultipleText,
    failed: &'static str,
) -> Result<i32, CommandError> {
    let players = players_of(found);
    let entities: Vec<&Target> = found
        .iter()
        .filter(|t| matches!(t, Target::Entity(_)))
        .collect();
    let ids: Vec<i32> = entities.iter().map(|t| t.id()).collect();
    let affected = effect_entities(ctx, &ids, &command).await;
    if players.is_empty() {
        if affected == 0 {
            return Err(CommandError::simple(failed));
        }
        let message = if entities.len() == 1 {
            single(ctx.name_of(entities[0]))
        } else {
            multiple(affected)
        };
        ctx.success(message, true).await;
        return Ok(affected as i32);
    }
    let single_multiple = multiple.clone();
    let mut messages = Messages::new(
        move |reply| {
            if affected == 0 {
                single(text(&reply.name))
            } else {
                single_multiple(affected + 1)
            }
        },
        move |count, _| multiple(count + affected),
    );
    if affected == 0 {
        messages = messages.on_zero(move |_| TextComponent::translate(failed, []));
    }
    let total = targets::dispatch_result(ctx, &players, |_| directive.clone(), messages).await?;
    Ok(total + affected as i32)
}

/// `/effect` on entities other than players, broadcast to every region
/// like `/kill`. Returns how many changed.
async fn effect_entities(ctx: &Context<'_>, ids: &[i32], command: &EffectCommand) -> usize {
    if ids.is_empty() {
        return 0;
    }
    let (reply, regions) = tokio::sync::oneshot::channel();
    let _ = ctx
        .shared()
        .manager
        .send(ManagerMessage::Regions { reply })
        .await;
    let mut affected = 0;
    for region in regions.await.unwrap_or_default() {
        let (ids, command) = (ids.to_vec(), command.clone());
        affected += region
            .ask(move |world| {
                ids.into_iter()
                    .filter(|id| world.effect_entity(*id, &command))
                    .count()
            })
            .await
            .unwrap_or(0);
    }
    affected
}

pub struct Enchant;

impl Executor for Enchant {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Resource(name)) = ctx.arg("enchantment").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let registry = tuff_item::enchantment::current();
            let definition = registry
                .get(&name)
                .ok_or_else(|| CommandError::simple("command.unknown.argument"))?;
            let level = match ctx.arg("level") {
                Some(Value::Int(level)) => *level,
                _ => 1,
            };
            if level > definition.max_level as i32 {
                return Err(CommandError::dynamic(
                    "commands.enchant.failed.level",
                    [text(level), text(definition.max_level)],
                ));
            }
            let found = target_entities(ctx, "targets").await?;
            let alone = found.len() == 1;
            let mut done: Vec<TextComponent> = Vec::new();
            for target in &found {
                let Target::Entity(entity) = target else {
                    continue;
                };
                let answer = enchant_entity(ctx, entity.id, &name, level).await;
                match answer {
                    Some(EnchantAnswer::Done) => done.push(text(&entity.name)),
                    Some(failure) if alone => {
                        return Err(match failure {
                            EnchantAnswer::Itemless => CommandError::dynamic(
                                "commands.enchant.failed.itemless",
                                [text(&entity.name)],
                            ),
                            EnchantAnswer::Incompatible(item) => CommandError::dynamic(
                                "commands.enchant.failed.incompatible",
                                [text(item)],
                            ),
                            _ => CommandError::dynamic(
                                "commands.enchant.failed.entity",
                                [text(&entity.name)],
                            ),
                        });
                    }
                    None if alone => {
                        return Err(CommandError::dynamic(
                            "commands.enchant.failed.entity",
                            [text(&entity.name)],
                        ));
                    }
                    _ => {}
                }
            }
            let players = players_of(&found);
            let full = enchantment_full_name(&name, definition, level);
            if players.is_empty() {
                let message = match done.as_slice() {
                    [] => return Err(CommandError::simple("commands.enchant.failed")),
                    [only] => TextComponent::translate(
                        "commands.enchant.success.single",
                        [full, only.clone()],
                    ),
                    many => TextComponent::translate(
                        "commands.enchant.success.multiple",
                        [full, text(many.len())],
                    ),
                };
                ctx.success(message, true).await;
                return Ok(done.len() as i32);
            }
            let single_full = full.clone();
            let entities_done = done.len();
            let messages = Messages::new(
                move |reply| {
                    if entities_done == 0 {
                        TextComponent::translate(
                            "commands.enchant.success.single",
                            [single_full.clone(), text(&reply.name)],
                        )
                    } else {
                        TextComponent::translate(
                            "commands.enchant.success.multiple",
                            [single_full.clone(), text(entities_done + 1)],
                        )
                    }
                },
                move |count, _| {
                    TextComponent::translate(
                        "commands.enchant.success.multiple",
                        [full.clone(), text(count + entities_done)],
                    )
                },
            );
            let messages = if entities_done == 0 {
                messages.on_zero(|_| TextComponent::translate("commands.enchant.failed", []))
            } else {
                messages
            };
            let total = targets::dispatch_result(
                ctx,
                &players,
                |_| Directive::Enchant {
                    enchantment: name.clone(),
                    level,
                },
                messages,
            )
            .await?;
            Ok(total + entities_done as i32)
        })
    }
}

async fn enchant_entity(
    ctx: &Context<'_>,
    id: i32,
    enchantment: &str,
    level: i32,
) -> Option<EnchantAnswer> {
    let (reply, regions) = tokio::sync::oneshot::channel();
    let _ = ctx
        .shared()
        .manager
        .send(ManagerMessage::Regions { reply })
        .await;
    for region in regions.await.unwrap_or_default() {
        let enchantment = enchantment.to_owned();
        if let Some(Some(answer)) = region
            .ask(move |world| world.enchant_entity(id, &enchantment, level))
            .await
        {
            return Some(answer);
        }
    }
    None
}

pub struct SpawnPoint;

impl Executor for SpawnPoint {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let players = target_players(ctx, "targets").await?;
            let pos = spawnable_pos(ctx, "pos")?;
            let rotation = match ctx.arg("rotation") {
                Some(Value::Rotation(rotation)) => rotation.get(ctx.source.rotation),
                _ => Rotation::ZERO.get(ctx.source.rotation),
            };
            let yaw = tuff_core::command::coords::wrap_degrees(rotation[0]);
            let pitch = rotation[1].clamp(-90.0, 90.0);
            let dimension = ctx.source.dimension;
            let key = ctx.dimension_key().to_owned();
            let args = move |name: TextComponent| {
                vec![
                    text(pos.x),
                    text(pos.y),
                    text(pos.z),
                    text(tuff_core::command::java_float(yaw)),
                    text(tuff_core::command::java_float(pitch)),
                    text(key.clone()),
                    name,
                ]
            };
            let multiple_args = args.clone();
            let messages = Messages::new(
                move |reply| {
                    TextComponent::translate(
                        "commands.spawnpoint.success.single",
                        args(text(&reply.name)),
                    )
                },
                move |count, _| {
                    TextComponent::translate(
                        "commands.spawnpoint.success.multiple",
                        multiple_args(text(count)),
                    )
                },
            );
            targets::dispatch(
                ctx,
                &players,
                |_| Directive::SpawnPoint {
                    dimension,
                    pos,
                    yaw,
                    pitch,
                },
                messages,
            )
            .await;
            Ok(1)
        })
    }
}

pub struct SetWorldSpawn;

impl Executor for SetWorldSpawn {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let pos = spawnable_pos(ctx, "pos")?;
            let rotation = match ctx.arg("rotation") {
                Some(Value::Rotation(rotation)) => rotation.get(ctx.source.rotation),
                _ => [0.0, 0.0],
            };
            let spawn = tuff_core::storage::level::Spawn {
                dimension: ctx.dimension_key().to_owned(),
                pos,
                yaw: rotation[0],
                pitch: rotation[1],
            };
            let message = TextComponent::translate(
                "commands.setworldspawn.success",
                [
                    text(spawn.pos.x),
                    text(spawn.pos.y),
                    text(spawn.pos.z),
                    text(java_float(spawn.yaw)),
                    text(java_float(spawn.pitch)),
                    text(&spawn.dimension),
                ],
            );
            ctx.ask(|done| CoordinatorMessage::SetWorldSpawn { spawn, done })
                .await;
            ctx.success(message, true).await;
            Ok(1)
        })
    }
}

pub struct Title;

impl Executor for Title {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(selector) = selector(ctx, "targets").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let players = targets::select_players(ctx, &selector).await?;
            let component = match ctx.arg("title").cloned() {
                Some(Value::Component(component)) => {
                    super::resolve::resolve(ctx, &component).await?
                }
                _ => TextComponent::new(""),
            };
            let literals = ctx.literals();
            let (action, key) = match literals.get(2).copied() {
                Some("clear") => (TitleAction::Clear, "cleared"),
                Some("reset") => (TitleAction::Reset, "reset"),
                Some("title") => (TitleAction::Title(component), "show.title"),
                Some("subtitle") => (TitleAction::Subtitle(component), "show.subtitle"),
                Some("actionbar") => (TitleAction::ActionBar(component), "show.actionbar"),
                Some("times") => {
                    let time = |name: &str| match ctx.arg(name) {
                        Some(Value::Time(ticks)) => *ticks,
                        _ => 0,
                    };
                    (
                        TitleAction::Times {
                            fade_in: time("fadeIn"),
                            stay: time("stay"),
                            fade_out: time("fadeOut"),
                        },
                        "times",
                    )
                }
                _ => return Err(CommandError::simple("command.unknown.argument")),
            };
            let single_key = format!("commands.title.{key}.single");
            let multiple_key = format!("commands.title.{key}.multiple");
            let messages = Messages::new(
                move |reply| TextComponent::translate(single_key.clone(), [text(&reply.name)]),
                move |count, _| TextComponent::translate(multiple_key.clone(), [text(count)]),
            );
            targets::dispatch(
                ctx,
                &players,
                |_| Directive::Title(action.clone()),
                messages,
            )
            .await;
            Ok(1)
        })
    }
}

pub struct Tellraw;

impl Executor for Tellraw {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(selector) = selector(ctx, "targets").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let Some(Value::Component(message)) = ctx.arg("message").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let players = targets::select_players(ctx, &selector).await?;
            let message = super::resolve::resolve(ctx, &message).await?;
            let messages =
                Messages::new(|_| TextComponent::new(""), |_, _| TextComponent::new("")).silent();
            targets::dispatch(
                ctx,
                &players,
                |_| Directive::Tellraw(message.clone()),
                messages,
            )
            .await;
            Ok(players.len() as i32)
        })
    }
}

pub struct PlaySound;

impl Executor for PlaySound {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Identifier(sound)) = ctx.arg("sound").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let literals = ctx.literals();
            let category = literals
                .get(2)
                .and_then(|name| SOUND_CATEGORIES.iter().position(|c| c == name))
                .unwrap_or(0) as i32;
            let players = match selector(ctx, "targets").cloned() {
                Some(selector) => targets::select_players(ctx, &selector).await?,
                None => match targets::source_player(ctx).await {
                    Ok(player) => vec![player],
                    Err(_) => Vec::new(),
                },
            };
            let position = match ctx.arg("pos") {
                Some(Value::Coordinates(coordinates)) => coordinates.position(&ctx.source.pose()),
                _ => ctx.source.position,
            };
            let float = |name: &str, default: f32| match ctx.arg(name) {
                Some(Value::Float(value)) => *value,
                _ => default,
            };
            let volume = float("volume", 1.0);
            let pitch = float("pitch", 1.0);
            let min_volume = float("minVolume", 0.0);
            let seed: i64 = ctx.rng().r#gen();
            let dimension = ctx.source.dimension;
            let same_dimension: Vec<PlayerSnapshot> = players
                .into_iter()
                .filter(|p| p.state.dimension == dimension)
                .collect();
            let single_sound = sound.clone();
            let multiple_sound = sound.clone();
            let messages = Messages::new(
                move |reply| {
                    TextComponent::translate(
                        "commands.playsound.success.single",
                        [text(&single_sound), text(&reply.name)],
                    )
                },
                move |count, _| {
                    TextComponent::translate(
                        "commands.playsound.success.multiple",
                        [text(&multiple_sound), text(count)],
                    )
                },
            )
            .on_zero(|_| TextComponent::translate("commands.playsound.failed", []));
            targets::dispatch(
                ctx,
                &same_dimension,
                |_| Directive::PlaySound {
                    sound: sound.clone(),
                    category,
                    position,
                    volume,
                    pitch,
                    min_volume,
                    seed,
                },
                messages,
            )
            .await;
            Ok(1)
        })
    }
}

pub struct StopSound;

impl Executor for StopSound {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(selector) = selector(ctx, "targets").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let players = targets::select_players(ctx, &selector).await?;
            let literals = ctx.literals();
            let category_name = literals.get(2).copied().filter(|name| *name != "*");
            let category = category_name
                .and_then(|name| SOUND_CATEGORIES.iter().position(|c| *c == name))
                .map(|index| index as i32);
            let sound = match ctx.arg("sound") {
                Some(Value::Identifier(sound)) => Some(sound.clone()),
                _ => None,
            };
            let message = match (category_name, &sound) {
                (Some(category), Some(sound)) => TextComponent::translate(
                    "commands.stopsound.success.source.sound",
                    [text(sound), text(category)],
                ),
                (Some(category), None) => TextComponent::translate(
                    "commands.stopsound.success.source.any",
                    [text(category)],
                ),
                (None, Some(sound)) => TextComponent::translate(
                    "commands.stopsound.success.sourceless.sound",
                    [text(sound)],
                ),
                (None, None) => {
                    TextComponent::translate("commands.stopsound.success.sourceless.any", [])
                }
            };
            let messages =
                Messages::new(|_| TextComponent::new(""), |_, _| TextComponent::new("")).silent();
            targets::dispatch(
                ctx,
                &players,
                |_| Directive::StopSound {
                    category,
                    sound: sound.clone(),
                },
                messages,
            )
            .await;
            ctx.success(message, true).await;
            Ok(players.len() as i32)
        })
    }
}

pub struct Kick;

impl Executor for Kick {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(selector) = selector(ctx, "targets").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let players = targets::select_players(ctx, &selector).await?;
            let reason = match ctx.arg("reason") {
                Some(Value::Message(message)) => {
                    let message = message.clone();
                    resolve_message(ctx, &message).await?
                }
                _ => TextComponent::translate("multiplayer.disconnect.kicked", []),
            };
            for player in &players {
                ctx.success(
                    TextComponent::translate(
                        "commands.kick.success",
                        [text(&player.name), reason.clone()],
                    ),
                    true,
                )
                .await;
            }
            let messages =
                Messages::new(|_| TextComponent::new(""), |_, _| TextComponent::new("")).silent();
            targets::dispatch(ctx, &players, |_| Directive::Kick(reason.clone()), messages).await;
            Ok(players.len() as i32)
        })
    }
}

pub struct Transfer;

impl Executor for Transfer {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Str(host)) = ctx.arg("hostname").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let port = match ctx.arg("port") {
                Some(Value::Int(port)) => *port,
                _ => 25565,
            };
            let players = target_players(ctx, "players").await?;
            if players.is_empty() {
                return Err(CommandError::simple("commands.transfer.error.no_players"));
            }
            let feedback = match players.as_slice() {
                [player] => TextComponent::translate(
                    "commands.transfer.success.single",
                    [text(&player.name), text(&host), text(port.to_string())],
                ),
                _ => TextComponent::translate(
                    "commands.transfer.success.multiple",
                    [
                        text(players.len().to_string()),
                        text(&host),
                        text(port.to_string()),
                    ],
                ),
            };
            ctx.success(feedback, true).await;
            let messages =
                Messages::new(|_| TextComponent::new(""), |_, _| TextComponent::new("")).silent();
            let directive = |_: &PlayerSnapshot| Directive::Transfer {
                host: host.clone(),
                port,
            };
            targets::dispatch(ctx, &players, directive, messages).await;
            Ok(players.len() as i32)
        })
    }
}

pub struct Msg;

impl Executor for Msg {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(selector) = selector(ctx, "targets").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let Some(Value::Message(message)) = ctx.arg("message").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let players = targets::select_players(ctx, &selector).await?;
            let message = resolve_message(ctx, &message).await?;
            let sender = ctx.source_display_name();
            for player in &players {
                let outgoing = TextComponent::translate(
                    "commands.message.display.outgoing",
                    [text(&player.name), message.clone()],
                )
                .color("gray")
                .italic(true);
                ctx.reply(outgoing);
            }
            let messages =
                Messages::new(|_| TextComponent::new(""), |_, _| TextComponent::new("")).silent();
            let incoming =
                TextComponent::translate("commands.message.display.incoming", [sender, message])
                    .color("gray")
                    .italic(true);
            targets::dispatch(
                ctx,
                &players,
                |_| Directive::PrivateMessage(incoming.clone()),
                messages,
            )
            .await;
            Ok(players.len() as i32)
        })
    }
}

pub struct List;

impl Executor for List {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let uuids = ctx.has("uuids");
            let players = targets::players(ctx).await;
            let names: Vec<TextComponent> = {
                let teams = ctx.shared().scoreboard.teams();
                players
                    .iter()
                    .map(|p| {
                        if uuids {
                            TextComponent::translate(
                                "commands.list.nameAndId",
                                [text(&p.name), text(format_uuid(p.uuid))],
                            )
                        } else {
                            teams.player_name(&p.name, p.uuid)
                        }
                    })
                    .collect()
            };
            let max = ctx.shared().live.max_players();
            ctx.success(
                TextComponent::translate(
                    "commands.list.players",
                    [
                        text(players.len()),
                        text(max),
                        crate::scoreboard::format_list(names),
                    ],
                ),
                false,
            )
            .await;
            Ok(players.len() as i32)
        })
    }
}

pub struct FetchProfile;

impl Executor for FetchProfile {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let (message, argument, query) = match (ctx.arg("name"), ctx.arg("id")) {
                (Some(Value::Str(name)), _) => (
                    "name",
                    TextComponent::new(name.clone()),
                    Some(ProfileQuery::Name(name.clone())),
                ),
                (_, Some(Value::Uuid(id))) => (
                    "id",
                    TextComponent::new(format_uuid(*id)),
                    Some(ProfileQuery::Id(*id)),
                ),
                _ => ("entity", TextComponent::new(""), None),
            };
            let (profile, argument) = match query {
                Some(query) => {
                    let (reply, answer) = tokio::sync::oneshot::channel();
                    ctx.coordinator(CoordinatorMessage::FetchProfile { query, reply })
                        .await;
                    (answer.await.ok().flatten(), argument)
                }
                None => {
                    let Some(Value::Selector(selector)) = ctx.arg("entity").cloned() else {
                        return Err(CommandError::simple("command.unknown.argument"));
                    };
                    let target = targets::select_entity(ctx, &selector).await?;
                    let name = ctx.name_of(&target);
                    let Target::Player(player) = &target else {
                        return Err(CommandError::text(TextComponent::translate(
                            "commands.fetchprofile.no_profile",
                            [name],
                        )));
                    };
                    let (reply, answer) = tokio::sync::oneshot::channel();
                    ctx.coordinator(CoordinatorMessage::ProfileOf {
                        player: player.id,
                        reply,
                    })
                    .await;
                    (answer.await.ok().flatten(), name)
                }
            };
            match profile {
                Some(profile) => {
                    let report = tuff_core::profile_text::profile_report(
                        &format!("commands.fetchprofile.{message}.success"),
                        argument,
                        &profile,
                    );
                    ctx.success(report, false).await;
                }
                None => ctx.fail(TextComponent::translate(
                    format!("commands.fetchprofile.{message}.failure"),
                    [argument],
                )),
            }
            Ok(1)
        })
    }
}
