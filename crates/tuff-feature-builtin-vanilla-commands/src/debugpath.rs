//! `/debugpath`: the game's developer command that makes a ground path for
//! the mob it runs as and reports whether it reaches the target. Off unless
//! `tuff.features.debugpath-command.enabled = true`, as in the game's
//! release builds.

use tokio::sync::oneshot;
use tuff_core::command::args::Value;
use tuff_core::command::{CommandNode, CommandNodeKind, RootCommand};
use tuff_core::world::entity::DebugPath;
use tuff_core::world::messages::{ManagerMessage, RegionRequest};
use tuff_core::world::region_api::RegionTask;
use tuff_item::text::TextComponent;
use tuff_world_model::BlockPos;

use super::{Context, Executor, executor_future};

pub const ID: &str = "debugpath-command";

const GAMEMASTERS: &str = r#"{"type":"minecraft:require","permission":{"type":"minecraft:command_level","level":"gamemasters"}}"#;

const TO: CommandNode = CommandNode {
    name: "to",
    kind: CommandNodeKind::Argument {
        parser: "minecraft:block_pos",
        properties: None,
    },
    executable: true,
    redirect: None,
    permissions: None,
    children: &[],
};
pub const DEBUGPATH: CommandNode = CommandNode {
    name: "debugpath",
    kind: CommandNodeKind::Literal,
    executable: false,
    redirect: None,
    permissions: Some(GAMEMASTERS),
    children: &[TO],
};

pub fn root() -> RootCommand {
    RootCommand::custom("debugpath", &DEBUGPATH, &[])
}

pub fn report(answer: DebugPath) -> Result<&'static str, &'static str> {
    match answer {
        DebugPath::NotMob => Err("Source is not a mob"),
        DebugPath::NoPath => Err("Path not found"),
        DebugPath::NotComplete => Err("Target not reached"),
        DebugPath::Made => Ok("Made path"),
    }
}

fn block_of(position: [f64; 3]) -> BlockPos {
    BlockPos::new(
        position[0].floor() as i32,
        position[1].floor() as i32,
        position[2].floor() as i32,
    )
}

pub struct DebugPathCommand;

impl Executor for DebugPathCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let Some(Value::Coordinates(to)) = ctx.arg("to").cloned() else {
                return Ok(0);
            };
            let target = block_of(to.position(&ctx.source.pose()));
            let answer = match ctx.source.entity {
                Some(id) => ask(ctx, id, target).await.unwrap_or(DebugPath::NotMob),
                None => DebugPath::NotMob,
            };
            match report(answer) {
                Ok(message) => {
                    ctx.success(TextComponent::new(message), true).await;
                    Ok(1)
                }
                Err(message) => {
                    ctx.fail(TextComponent::new(message));
                    Ok(0)
                }
            }
        })
    }
}

async fn ask(ctx: &Context<'_>, id: i32, target: BlockPos) -> Option<DebugPath> {
    let (regions, answer) = oneshot::channel();
    ctx.shared()
        .manager
        .send(ManagerMessage::Regions { reply: regions })
        .await
        .ok()?;
    let dimension = ctx.source.dimension;
    for handle in answer.await.ok()? {
        if handle.dimension != dimension {
            continue;
        }
        let (reply, answer) = oneshot::channel();
        handle
            .request(RegionRequest::Run(RegionTask::new(move |world| {
                let _ = reply.send(world.debug_path_entity(id, target));
            })))
            .await;
        if let Ok(Some(found)) = answer.await {
            return Some(found);
        }
    }
    None
}

/// Registers `/debugpath`.
pub struct DebugPathFeature;

impl tuff_core::feature::Feature for DebugPathFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            cx.registries().commands.register(root(), DebugPathCommand);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn every_outcome_has_the_games_message() {
        assert_eq!(report(DebugPath::Made), Ok("Made path"));
        assert_eq!(report(DebugPath::NotMob), Err("Source is not a mob"));
        assert_eq!(report(DebugPath::NoPath), Err("Path not found"));
        assert_eq!(report(DebugPath::NotComplete), Err("Target not reached"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_tree_takes_a_block_position() {
        assert_eq!(root().name, "debugpath");
        assert_eq!(DEBUGPATH.children[0].name, "to");
    }
}
