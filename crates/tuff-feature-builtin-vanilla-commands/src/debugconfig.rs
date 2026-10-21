//! `/debugconfig config|unconfig`: the game's developer command that sends a
//! player from play back to the configuration phase and holds them there
//! until `unconfig` re-syncs registries and tags and returns them to the
//! world. Off unless `tuff.features.debugconfig-command.enabled = true`, as
//! in the game's release builds.

use tuff_core::command::{CommandNode, CommandNodeKind, RootCommand, Value};
use tuff_core::world::messages::PhaseRequest;
use tuff_item::text::TextComponent;
use tuff_storage::admin::format_uuid;

use super::{Context, Executor, executor_future, targets};

pub const ID: &str = "debugconfig-command";

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

const PLAYER: CommandNode = node(
    "target",
    CommandNodeKind::Argument {
        parser: "minecraft:entity",
        properties: Some(r#"{"amount":"single","type":"players"}"#),
    },
    true,
    &[],
);
const UUID: CommandNode = node(
    "target",
    CommandNodeKind::Argument {
        parser: "minecraft:uuid",
        properties: None,
    },
    true,
    &[],
);
const CONFIG: CommandNode = node("config", CommandNodeKind::Literal, false, &[PLAYER]);
const UNCONFIG: CommandNode = node("unconfig", CommandNodeKind::Literal, false, &[UUID]);
pub const DEBUGCONFIG: CommandNode = CommandNode {
    name: "debugconfig",
    kind: CommandNodeKind::Literal,
    executable: false,
    redirect: None,
    permissions: Some(ADMINS),
    children: &[CONFIG, UNCONFIG],
};

pub fn root() -> RootCommand {
    RootCommand::custom("debugconfig", &DEBUGCONFIG, &[])
}

pub fn switched(name: &str, uuid: u128) -> String {
    format!(
        "Switched player {name}({}) to config mode",
        format_uuid(uuid)
    )
}

pub struct DebugConfig;

impl Executor for DebugConfig {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            match ctx.literals().get(1).copied() {
                Some("config") => {
                    let Some(Value::Selector(selector)) = ctx.arg("target").cloned() else {
                        return Ok(0);
                    };
                    let player = targets::select_player(ctx, &selector).await?;
                    let moved = ctx
                        .shared()
                        .request_phase(player.uuid, |reply| PhaseRequest::Configure {
                            hold: true,
                            reply,
                        })
                        .await;
                    if !moved {
                        return Err(tuff_core::command::CommandError::simple(
                            "argument.entity.notfound.player",
                        ));
                    }
                    ctx.success(
                        TextComponent::new(switched(&player.name, player.uuid)),
                        false,
                    )
                    .await;
                    Ok(1)
                }
                Some("unconfig") => {
                    let Some(Value::Uuid(uuid)) = ctx.arg("target").cloned() else {
                        return Ok(0);
                    };
                    let released = ctx
                        .shared()
                        .request_phase(uuid, |reply| PhaseRequest::Release { reply })
                        .await;
                    if released {
                        Ok(1)
                    } else {
                        ctx.fail(TextComponent::new("Can't find player to unconfig"));
                        Ok(0)
                    }
                }
                _ => Ok(0),
            }
        })
    }
}

/// Registers `/debugconfig`.
pub struct DebugConfigFeature;

impl tuff_core::feature::Feature for DebugConfigFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            cx.registries().commands.register(root(), DebugConfig);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_switch_message_names_the_player_and_uuid() {
        assert_eq!(
            switched("Alex", 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
            "Switched player Alex(01234567-89ab-cdef-0123-456789abcdef) to config mode"
        );
    }
}
