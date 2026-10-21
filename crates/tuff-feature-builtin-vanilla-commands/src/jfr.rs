//! `/jfr start|stop`: the game's owner command for the JVM flight recorder.
//! This server is not a JVM, so there is no recorder to start and both
//! subcommands report the game's failure messages.

use tuff_core::command::{CommandError, CommandNode, CommandNodeKind, RootCommand};

use super::{Context, Executor, executor_future};

const OWNERS: &str = r#"{"type":"minecraft:require","permission":{"type":"minecraft:command_level","level":"owners"}}"#;

const fn literal(name: &'static str) -> CommandNode {
    CommandNode {
        name,
        kind: CommandNodeKind::Literal,
        executable: true,
        redirect: None,
        permissions: None,
        children: &[],
    }
}

pub const JFR: CommandNode = CommandNode {
    name: "jfr",
    kind: CommandNodeKind::Literal,
    executable: false,
    redirect: None,
    permissions: Some(OWNERS),
    children: &[literal("start"), literal("stop")],
};

pub fn root() -> RootCommand {
    RootCommand::custom("jfr", &JFR, &[])
}

pub const NO_RECORDER: &str = "no flight recorder on a native server";

pub struct Jfr;

impl Executor for Jfr {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            if ctx.literals().contains(&"start") {
                Err(CommandError::simple("commands.jfr.start.failed"))
            } else {
                Err(CommandError::dynamic(
                    "commands.jfr.dump.failed",
                    [super::text(NO_RECORDER)],
                ))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_tree_has_start_and_stop_for_owners() {
        let names: Vec<_> = JFR.children.iter().map(|c| c.name).collect();
        assert_eq!(names, ["start", "stop"]);
        assert!(JFR.permissions.unwrap().contains("owners"));
        assert_eq!(root().name, "jfr");
    }
}
