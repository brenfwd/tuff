//! `/serverpack push|pop`: the game's developer command that pushes a
//! resource pack to, or pops one from, every connected player. Registered
//! only with `tuff.features.serverpack-command.enabled = true`.

use tuff_core::auth::name_uuid;
use tuff_core::command::{CommandNode, CommandNodeKind, RootCommand, Value};
use tuff_core::resource_pack::PackOffer;
use tuff_core::world::messages::PlayerEvent;

use super::{Context, Executor, executor_future, targets};

pub const ID: &str = "serverpack-command";

const GAMEMASTERS: &str = r#"{"type":"minecraft:require","permission":{"type":"minecraft:command_level","level":"gamemasters"}}"#;

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

const fn argument(
    name: &'static str,
    parser: &'static str,
    properties: Option<&'static str>,
    executable: bool,
    children: &'static [CommandNode],
) -> CommandNode {
    node(
        name,
        CommandNodeKind::Argument { parser, properties },
        executable,
        children,
    )
}

const UUID: CommandNode = argument("uuid", "minecraft:uuid", None, true, &[]);
const HASH: CommandNode = argument(
    "hash",
    "brigadier:string",
    Some(r#"{"type":"word"}"#),
    true,
    &[],
);
const PUSH_UUID: CommandNode = argument("uuid", "minecraft:uuid", None, true, &[HASH]);
const URL: CommandNode = argument(
    "url",
    "brigadier:string",
    Some(r#"{"type":"phrase"}"#),
    true,
    &[PUSH_UUID],
);
const PUSH: CommandNode = node("push", CommandNodeKind::Literal, false, &[URL]);
const POP: CommandNode = node("pop", CommandNodeKind::Literal, false, &[UUID]);
pub const SERVERPACK: CommandNode = CommandNode {
    name: "serverpack",
    kind: CommandNodeKind::Literal,
    executable: false,
    redirect: None,
    permissions: Some(GAMEMASTERS),
    children: &[POP, PUSH],
};

pub fn root() -> RootCommand {
    RootCommand::custom("serverpack", &SERVERPACK, &[])
}

pub struct ServerPack;

impl Executor for ServerPack {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let (offer, popped) = match ctx.literals().get(1).copied() {
                Some("push") => {
                    let Some(Value::Str(url)) = ctx.arg("url").cloned() else {
                        return Ok(0);
                    };
                    let id = match ctx.arg("uuid") {
                        Some(Value::Uuid(id)) => *id,
                        _ => name_uuid(url.as_bytes()),
                    };
                    let sha1 = match ctx.arg("hash") {
                        Some(Value::Str(hash)) => hash.clone(),
                        _ => String::new(),
                    };
                    (
                        Some(PackOffer {
                            id,
                            url,
                            sha1,
                            required: false,
                            prompt: None,
                        }),
                        None,
                    )
                }
                Some("pop") => {
                    let Some(Value::Uuid(id)) = ctx.arg("uuid") else {
                        return Ok(0);
                    };
                    (None, Some(*id))
                }
                _ => return Ok(0),
            };
            for player in targets::players(ctx).await {
                player.handle.send(match &offer {
                    Some(offer) => PlayerEvent::ResourcePack(offer.clone()),
                    None => PlayerEvent::ResourcePackPop(popped),
                });
            }
            Ok(0)
        })
    }
}

/// Registers `/serverpack`.
pub struct ServerPackFeature;

impl tuff_core::feature::Feature for ServerPackFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(
        &self,
        cx: &mut tuff_core::feature::LoadContext,
    ) -> Result<(), tuff_item::load::LoadError> {
        if cx.phase() == tuff_item::load::Phase::Content {
            cx.registries().commands.register(root(), ServerPack);
        }
        Ok(())
    }
}
