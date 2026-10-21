//! `/warden_spawn_tracker clear | set <warning_level>`: the game's developer
//! command that resets or sets the executing player's warden spawn tracker.
//! Registered only with
//! `tuff.features.warden-spawn-tracker-command.enabled = true`.

use tuff_core::command::args::Value;
use tuff_core::command::{CommandNode, CommandNodeKind, RootCommand};
use tuff_core::world::messages::PlayerEvent;
use tuff_item::text::TextComponent;

use super::{Context, Executor, executor_future, targets};

pub const ID: &str = "warden-spawn-tracker-command";

const GAMEMASTERS: &str = r#"{"type":"minecraft:require","permission":{"type":"minecraft:command_level","level":"gamemasters"}}"#;

const LEVEL: CommandNode = CommandNode {
    name: "warning_level",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:integer",
        properties: Some(r#"{"min":0,"max":4}"#),
    },
    executable: true,
    redirect: None,
    permissions: None,
    children: &[],
};
const CLEAR: CommandNode = CommandNode {
    name: "clear",
    kind: CommandNodeKind::Literal,
    executable: true,
    redirect: None,
    permissions: None,
    children: &[],
};
const SET: CommandNode = CommandNode {
    name: "set",
    kind: CommandNodeKind::Literal,
    executable: false,
    redirect: None,
    permissions: None,
    children: &[LEVEL],
};
pub const WARDEN_SPAWN_TRACKER: CommandNode = CommandNode {
    name: "warden_spawn_tracker",
    kind: CommandNodeKind::Literal,
    executable: false,
    redirect: None,
    permissions: Some(GAMEMASTERS),
    children: &[CLEAR, SET],
};

pub fn root() -> RootCommand {
    RootCommand::custom("warden_spawn_tracker", &WARDEN_SPAWN_TRACKER, &[])
}

/// The tracker change and its feedback key for the parsed command.
pub fn change_of(literal: &str, level: Option<i32>) -> Option<(Option<i32>, &'static str)> {
    match literal {
        "clear" => Some((None, "commands.warden_spawn_tracker.clear.success.single")),
        "set" => Some((
            Some(level?),
            "commands.warden_spawn_tracker.set.success.single",
        )),
        _ => None,
    }
}

pub struct WardenSpawnTracker;

impl Executor for WardenSpawnTracker {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let player = targets::source_player(ctx).await?;
            let level = match ctx.arg("warning_level") {
                Some(Value::Int(level)) => Some(*level),
                _ => None,
            };
            let literal = ctx.literals().get(1).copied().unwrap_or_default();
            let Some((change, key)) = change_of(literal, level) else {
                return Ok(0);
            };
            player.handle.send(PlayerEvent::WardenWarningLevel(change));
            ctx.success(
                TextComponent::translate(key, [TextComponent::new(player.name.clone())]),
                true,
            )
            .await;
            Ok(1)
        })
    }
}

/// Registers `/warden_spawn_tracker`.
pub struct WardenSpawnTrackerFeature;

impl tuff_core::feature::Feature for WardenSpawnTrackerFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            cx.registries()
                .commands
                .register(root(), WardenSpawnTracker);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn clear_resets_and_set_takes_the_level() {
        assert_eq!(
            change_of("clear", None),
            Some((None, "commands.warden_spawn_tracker.clear.success.single"))
        );
        assert_eq!(
            change_of("set", Some(3)),
            Some((Some(3), "commands.warden_spawn_tracker.set.success.single"))
        );
        assert_eq!(change_of("set", None), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_level_is_bounded_like_the_games() {
        assert_eq!(root().name, "warden_spawn_tracker");
        let CommandNodeKind::Argument { properties, .. } = LEVEL.kind else {
            panic!("level is an argument");
        };
        assert_eq!(properties, Some(r#"{"min":0,"max":4}"#));
    }
}
