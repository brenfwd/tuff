//! `/raid`: the game's developer command to start, stop, inspect and
//! provoke raids around the player. Off unless
//! `tuff.features.raid-command.enabled = true`, as in the game's release
//! builds.

use crate::world::region::raids::{DebugAction, DebugReply, debug};
use tokio::sync::oneshot;
use tuff_core::command::args::Value;
use tuff_core::command::{CommandNode, CommandNodeKind, RootCommand};
use tuff_core::world::messages::ManagerMessage;
use tuff_item::text::TextComponent;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

use tuff_core::world::commands::{
    Context, Executor, ExecutorFuture, executor_future, targets, text,
};

pub const ID: &str = "raid-command";

const ADMINS: &str = r#"{"type":"minecraft:require","permission":{"type":"minecraft:command_level","level":"admins"}}"#;

const fn node(
    name: &'static str,
    kind: CommandNodeKind,
    executable: bool,
    children: &'static [CommandNode],
) -> CommandNode {
    CommandNode {
        name,
        kind,
        executable,
        redirect: None,
        permissions: None,
        children,
    }
}

const OMEN_LEVEL: CommandNode = node(
    "omenlvl",
    CommandNodeKind::Argument {
        parser: "brigadier:integer",
        properties: Some(r#"{"min":0}"#),
    },
    true,
    &[],
);
const LEVEL: CommandNode = node(
    "level",
    CommandNodeKind::Argument {
        parser: "brigadier:integer",
        properties: Some(r#"{"min":0}"#),
    },
    true,
    &[],
);
const SOUND_TYPE: CommandNode = node(
    "type",
    CommandNodeKind::Argument {
        parser: "minecraft:component",
        properties: None,
    },
    true,
    &[],
);
const START: CommandNode = node("start", CommandNodeKind::Literal, false, &[OMEN_LEVEL]);
const STOP: CommandNode = node("stop", CommandNodeKind::Literal, true, &[]);
const CHECK: CommandNode = node("check", CommandNodeKind::Literal, true, &[]);
const SOUND: CommandNode = node("sound", CommandNodeKind::Literal, false, &[SOUND_TYPE]);
const SPAWN_LEADER: CommandNode = node("spawnleader", CommandNodeKind::Literal, true, &[]);
const SET_OMEN: CommandNode = node("setomen", CommandNodeKind::Literal, false, &[LEVEL]);
const GLOW: CommandNode = node("glow", CommandNodeKind::Literal, true, &[]);
pub const RAID: CommandNode = CommandNode {
    name: "raid",
    kind: CommandNodeKind::Literal,
    executable: false,
    redirect: None,
    permissions: Some(ADMINS),
    children: &[START, STOP, CHECK, SOUND, SPAWN_LEADER, SET_OMEN, GLOW],
};

pub fn root() -> RootCommand {
    RootCommand::custom("raid", &RAID, &[])
}

fn block_of(position: [f64; 3]) -> BlockPos {
    BlockPos::new(
        position[0].floor() as i32,
        position[1].floor() as i32,
        position[2].floor() as i32,
    )
}

fn action_of(ctx: &Context<'_>) -> Option<DebugAction> {
    let position = ctx.source.position;
    let int = |name: &str| match ctx.arg(name) {
        Some(Value::Int(value)) => Some(*value),
        _ => None,
    };
    match ctx.literals().get(1).copied()? {
        "start" => Some(DebugAction::Start(int("omenlvl")?)),
        "stop" => Some(DebugAction::Stop),
        "check" => Some(DebugAction::Check),
        "sound" => match ctx.arg("type") {
            Some(Value::Component(kind)) if kind.plain() == "local" => {
                Some(DebugAction::LocalSound(position))
            }
            _ => None,
        },
        "spawnleader" => Some(DebugAction::SpawnLeader(position)),
        "setomen" => Some(DebugAction::SetOmen(int("level")?)),
        "glow" => Some(DebugAction::Glow),
        _ => None,
    }
}

pub struct Raid;

impl Executor for Raid {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            targets::source_player(ctx).await?;
            let Some(action) = action_of(ctx) else {
                return Ok(1);
            };
            let reply = ask(ctx, action).await;
            for line in reply.messages {
                ctx.success(text(line), false).await;
            }
            if let Some(failure) = reply.failure {
                ctx.fail(TextComponent::new(failure));
            }
            Ok(reply.result)
        })
    }
}

async fn ask(ctx: &Context<'_>, action: DebugAction) -> DebugReply {
    let none = || DebugReply {
        result: 1,
        ..DebugReply::default()
    };
    let at = block_of(ctx.source.position);
    let region = ChunkPos::of_block(at).region();
    let (regions, answer) = oneshot::channel();
    if ctx
        .shared()
        .manager
        .send(ManagerMessage::Regions { reply: regions })
        .await
        .is_err()
    {
        return none();
    }
    let dimension = ctx.source.dimension;
    let Some(handle) = answer
        .await
        .unwrap_or_default()
        .into_iter()
        .find(|handle| handle.dimension == dimension && handle.pos == region)
    else {
        return none();
    };
    let (reply, answer) = oneshot::channel();
    handle
        .request(tuff_core::world::messages::RegionRequest::Run(
            tuff_core::world::region_api::RegionTask::new(move |world| {
                debug(world, at, action, reply);
            }),
        ))
        .await;
    answer.await.unwrap_or_default_reply()
}

trait OrNoRaid {
    fn unwrap_or_default_reply(self) -> DebugReply;
}

impl OrNoRaid for Result<DebugReply, oneshot::error::RecvError> {
    fn unwrap_or_default_reply(self) -> DebugReply {
        self.unwrap_or_else(|_| DebugReply {
            failure: Some("No raid here".to_owned()),
            result: 1,
            ..DebugReply::default()
        })
    }
}

/// Registers `/raid`.
pub struct RaidFeature;

impl tuff_core::feature::Feature for RaidFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            cx.registries().commands.register(root(), Raid);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_command_tree_lists_every_subcommand() {
        let names: Vec<_> = RAID.children.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "start",
                "stop",
                "check",
                "sound",
                "spawnleader",
                "setomen",
                "glow"
            ]
        );
        assert_eq!(root().name, "raid");
    }
}
