//! `/rotate` and `/swing`.

use tuff_core::command::CommandError;
use tuff_core::command::args::{Anchor, Value};
use tuff_core::protocol::play::SwingAnimation;
use tuff_core::world::messages::{Directive, LookAt, ManagerMessage, Teleport};
use tuff_item::item::SwingKind;
use tuff_item::text::TextComponent;

use super::targets::{self, Target};
use super::{Context, Executor, executor_future, text};

pub struct Rotate;

impl Executor for Rotate {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Selector(selector)) = ctx.arg("target").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let target = targets::select_entity(ctx, &selector).await?;
            let pose = ctx.source.pose();
            let mut rotation = None;
            let mut look_at = None;
            if let Some(Value::Rotation(value)) = ctx.arg("rotation") {
                rotation = Some(value.get(ctx.source.rotation));
            } else if let Some(Value::Coordinates(facing)) = ctx.arg("facingLocation") {
                look_at = Some(LookAt {
                    point: facing.position(&pose),
                    from: ctx.source.anchor,
                });
            } else if let Some(Value::Selector(facing)) = ctx.arg("facingEntity").cloned() {
                let facing = targets::select_entity(ctx, &facing).await?;
                let anchor = match ctx.arg("facingAnchor") {
                    Some(Value::Anchor(anchor)) => *anchor,
                    _ => Anchor::Feet,
                };
                look_at = Some(LookAt {
                    point: match anchor {
                        Anchor::Feet => facing.position(),
                        Anchor::Eyes => targets::eye_of(&facing),
                    },
                    from: ctx.source.anchor,
                });
            }
            let teleport = Teleport {
                dimension: target.dimension(),
                position: target.position(),
                relative: [false; 3],
                rotation,
                relative_rotation: [false; 2],
                look_at,
            };
            match &target {
                Target::Player(player) => {
                    targets::dispatch_wait(ctx, std::slice::from_ref(player), |_| {
                        Directive::Teleport(teleport.clone())
                    })
                    .await;
                }
                Target::Entity(_) => {
                    super::entity_teleport::teleport_entities(ctx, &[&target], &teleport).await;
                }
            }
            let message =
                TextComponent::translate("commands.rotate.success", [ctx.name_of(&target)]);
            ctx.success(message, true).await;
            Ok(1)
        })
    }
}

pub struct Swing;

async fn swing_entity(ctx: &Context<'_>, id: i32, animation: &SwingAnimation) -> Option<bool> {
    let (reply, regions) = tokio::sync::oneshot::channel();
    let _ = ctx
        .shared()
        .manager
        .send(ManagerMessage::Regions { reply })
        .await;
    for region in regions.await.unwrap_or_default() {
        let (hand, kind, duration) = (animation.hand, animation.kind, animation.duration_ticks);
        if let Some(Some(done)) = region
            .ask(move |world| world.swing_entity(id, hand, kind, duration))
            .await
        {
            return Some(done);
        }
    }
    None
}

impl Executor for Swing {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let found = match ctx.arg("targets").cloned() {
                Some(Value::Selector(selector)) => targets::select_entities(ctx, &selector).await?,
                _ => vec![Target::Player(targets::source_player(ctx).await?)],
            };
            let hand = i32::from(ctx.has("offhand"));
            let kind = match ctx.arg("animation") {
                Some(Value::Str(name)) => match name.as_str() {
                    "none" => SwingKind::None,
                    "stab" => SwingKind::Stab,
                    _ => SwingKind::Whack,
                },
                _ => SwingKind::Whack,
            };
            let duration = match ctx.arg("duration") {
                Some(Value::Time(ticks)) => *ticks,
                _ => SwingAnimation::DEFAULT_DURATION_TICKS,
            };
            let animation = SwingAnimation {
                entity_id: 0,
                hand,
                kind,
                duration_ticks: duration,
            };
            let mut swung: Vec<TextComponent> = Vec::new();
            for target in &found {
                match target {
                    Target::Player(player) => {
                        targets::dispatch_wait(ctx, std::slice::from_ref(player), |_| {
                            Directive::Swing {
                                hand,
                                kind,
                                duration,
                            }
                        })
                        .await;
                        swung.push(ctx.name_of(target));
                    }
                    Target::Entity(entity) => {
                        if swing_entity(ctx, entity.id, &animation).await == Some(true) {
                            swung.push(ctx.name_of(target));
                        }
                    }
                }
            }
            let message = match swung.as_slice() {
                [] => return Err(CommandError::simple("commands.swing.failed.notliving")),
                [only] => TextComponent::translate("commands.swing.success.single", [only.clone()]),
                many => {
                    TextComponent::translate("commands.swing.success.multiple", [text(many.len())])
                }
            };
            ctx.success(message, true).await;
            Ok(swung.len() as i32)
        })
    }
}
