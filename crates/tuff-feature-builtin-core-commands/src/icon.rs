//! `/tuff icon set <file>|clear`: changes the server list icon while the
//! server runs and saves it in the world's config store.

use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, CommandNode};
use tuff_core::world::commands::{Context, text};
use tuff_core::world::messages::CoordinatorMessage;
use tuff_storage::config::MAX_ICON_BYTES;

use crate::tuff::{literal, string_argument};

pub const ICON_NODE: &str = "tuff.command.icon";

const FILE: CommandNode = string_argument("file", r#"{"type":"greedy"}"#, true, &[]);
const SET: CommandNode = literal("set", false, &[FILE]);
const CLEAR: CommandNode = literal("clear", true, &[]);
pub const ICON: CommandNode = crate::tuff::guarded(
    literal("icon", false, &[CLEAR, SET]),
    r#"{"type":"tuff:permission","node":"tuff.command.icon"}"#,
);

fn fail(message: impl Into<String>) -> CommandError {
    CommandError::text(text(message.into()))
}

pub async fn run(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let png = match ctx.literals().get(2).copied() {
        Some("clear") => None,
        Some("set") => {
            let Some(Value::Str(file)) = ctx.arg("file").cloned() else {
                return Err(CommandError::simple("command.unknown.command"));
            };
            let allow_outside = ctx
                .shared()
                .config
                .get_bool(tuff_core::config::EXECUTE_ALLOW_OUTSIDE)
                .unwrap_or(false)
                || tuff_core::server::is_operator_command();
            let path = crate::tuff::locate(&file, allow_outside).await?;
            let meta = tokio::fs::metadata(&path)
                .await
                .map_err(|e| fail(format!("Cannot read file: {e}")))?;
            if !meta.is_file() || meta.len() > MAX_ICON_BYTES as u64 {
                return Err(fail(format!(
                    "Not a file, or larger than {MAX_ICON_BYTES} bytes"
                )));
            }
            Some(
                tokio::fs::read(&path)
                    .await
                    .map_err(|e| fail(format!("Cannot read file: {e}")))?,
            )
        }
        _ => return Err(CommandError::simple("command.unknown.command")),
    };
    let cleared = png.is_none();
    let (reply, answer) = tokio::sync::oneshot::channel();
    ctx.coordinator(CoordinatorMessage::SetServerIcon { png, reply })
        .await;
    match answer.await {
        Ok(Ok(())) => {
            ctx.reply(text(if cleared {
                "Removed the server icon"
            } else {
                "Set the server icon"
            }));
            Ok(1)
        }
        Ok(Err(err)) => Err(fail(err)),
        Err(_) => Err(fail("The server is stopping")),
    }
}
