//! `/particle <name> [<pos> [<delta> <speed> <count> [force|normal [<viewers>]]]]`.

use std::sync::Arc;

use tuff_core::command::CommandError;
use tuff_core::command::args::Value;
use tuff_core::command::reader::Reader;
use tuff_core::command::selector::Selector;
use tuff_core::world::messages::Directive;
use tuff_item::text::TextComponent;

use super::feedback::Messages;
use super::targets::{self, Target};
use super::{Context, Executor, executor_future};

pub struct Particle;

impl Executor for Particle {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Particle(input)) = ctx.arg("name").cloned() else {
                return Err(CommandError::simple("command.unknown.argument"));
            };
            let pose = ctx.source.pose();
            let vec3 = |ctx: &Context<'_>, name: &str| match ctx.arg(name) {
                Some(Value::Coordinates(c)) => Some(c.position(&pose)),
                _ => None,
            };
            let position = vec3(ctx, "pos").unwrap_or(ctx.source.position);
            let delta = vec3(ctx, "delta").unwrap_or_default().map(|c| c as f32);
            let speed = match ctx.arg("speed") {
                Some(Value::Float(speed)) => *speed,
                _ => 0.0,
            };
            let count = match ctx.arg("count") {
                Some(Value::Int(count)) => *count,
                _ => 0,
            };
            let force = ctx.literals().contains(&"force");
            let viewers = match ctx.arg("viewers") {
                Some(Value::Selector(selector)) => {
                    let selector = selector.as_ref().clone();
                    targets::select_entities(ctx, &selector).await?
                }
                _ => {
                    let everyone = Selector::parse(&mut Reader::new("@a"), true)?;
                    targets::select_entities(ctx, &everyone).await?
                }
            };
            let players: Vec<_> = viewers
                .into_iter()
                .filter_map(|t| match t {
                    Target::Player(player) => Some(player),
                    Target::Entity(_) => None,
                })
                .collect();
            let dimension = ctx.source.dimension;
            let options: Arc<[u8]> = input.options.clone().into();
            let id = input.kind.id();
            let name = input.name();
            let messages = Messages::new(
                {
                    let name = name.clone();
                    move |_| {
                        TextComponent::translate(
                            "commands.particle.success",
                            [TextComponent::new(name.clone())],
                        )
                    }
                },
                move |_, _| {
                    TextComponent::translate(
                        "commands.particle.success",
                        [TextComponent::new(name.clone())],
                    )
                },
            )
            .on_zero(|_| TextComponent::translate("commands.particle.failed", []));
            targets::dispatch_result(
                ctx,
                &players,
                |_| Directive::Particle {
                    dimension,
                    particle: id,
                    options: options.clone(),
                    force,
                    position,
                    delta,
                    speed,
                    count,
                },
                messages,
            )
            .await
        })
    }
}
