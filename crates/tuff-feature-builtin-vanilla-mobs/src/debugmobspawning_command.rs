//! `/debugmobspawning <category> <at>`: the game's developer command that
//! makes one natural spawn attempt for a mob category from a block, outside
//! the caps. Off unless
//! `tuff.features.debugmobspawning-command.enabled = true`, as in the
//! game's release builds.

use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, CommandNode, CommandNodeKind, RootCommand};
use tuff_core::world::commands::{Context, Executor, ExecutorFuture, executor_future};
use tuff_vanilla_data::entity::MobCategory;
use tuff_world_model::pos::ChunkPos;

use crate::world::region::spawning::debug_spawn;

pub const ID: &str = "debugmobspawning-command";

const GAMEMASTERS: &str = r#"{"type":"minecraft:require","permission":{"type":"minecraft:command_level","level":"gamemasters"}}"#;

const AT: CommandNode = CommandNode {
    name: "at",
    kind: CommandNodeKind::Argument {
        parser: "minecraft:block_pos",
        properties: None,
    },
    executable: true,
    redirect: None,
    permissions: None,
    children: &[],
};

const fn category(name: &'static str) -> CommandNode {
    CommandNode {
        name,
        kind: CommandNodeKind::Literal,
        executable: false,
        redirect: None,
        permissions: None,
        children: &[AT],
    }
}

const CATEGORIES: [CommandNode; MobCategory::ALL.len()] = {
    let mut nodes = [category(""); MobCategory::ALL.len()];
    let mut i = 0;
    while i < nodes.len() {
        nodes[i] = category(MobCategory::ALL[i].name());
        i += 1;
    }
    nodes
};

pub const DEBUGMOBSPAWNING: CommandNode = CommandNode {
    name: "debugmobspawning",
    kind: CommandNodeKind::Literal,
    executable: false,
    redirect: None,
    permissions: Some(GAMEMASTERS),
    children: &CATEGORIES,
};

pub fn root() -> RootCommand {
    RootCommand::custom("debugmobspawning", &DEBUGMOBSPAWNING, &[])
}

pub fn category_named(name: &str) -> Option<MobCategory> {
    MobCategory::ALL.into_iter().find(|c| c.name() == name)
}

pub struct DebugMobSpawning;

impl Executor for DebugMobSpawning {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a> {
        executor_future(async move {
            let unloaded = || CommandError::simple("argument.pos.unloaded");
            let Some(category) = ctx.literals().get(1).copied().and_then(category_named) else {
                return Ok(0);
            };
            let Some(Value::Coordinates(at)) = ctx.arg("at").cloned() else {
                return Ok(0);
            };
            let [x, y, z] = at.block(&ctx.source.pose());
            let at = tuff_world_model::BlockPos::new(x, y, z);
            let dimension = ctx.source.dimension;
            let region = ctx
                .actor
                .region(dimension, ChunkPos::of_block(at).region())
                .await
                .ok_or_else(unloaded)?;
            let loaded = region
                .ask(move |world| debug_spawn(world, category, at))
                .await
                .unwrap_or(false);
            if !loaded {
                return Err(unloaded());
            }
            Ok(1)
        })
    }
}

/// Registers `/debugmobspawning`.
pub struct DebugMobSpawningFeature;

impl tuff_core::feature::Feature for DebugMobSpawningFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            cx.registries().commands.register(root(), DebugMobSpawning);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn every_category_is_a_literal_taking_a_block() {
        assert_eq!(DEBUGMOBSPAWNING.children.len(), MobCategory::ALL.len());
        for node in DEBUGMOBSPAWNING.children {
            assert_eq!(category_named(node.name).map(|c| c.name()), Some(node.name));
            assert_eq!(node.children[0].name, "at");
        }
        assert!(category_named("monster").is_some());
        assert!(category_named("bogus").is_none());
    }
}
