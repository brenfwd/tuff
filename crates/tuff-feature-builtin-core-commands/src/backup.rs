//! `/tuff backup create|list|status|cancel|delete <name>`: world backups
//! made by the backup jobs actor (see `tuff_storage::backup`). Each
//! child needs its own `tuff.command.backup.<child>` node.

use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, CommandNode};
use tuff_core::storage::backup::jobs::BackupError;
use tuff_core::storage::backup::jobs::CreateOptions;
use tuff_core::world::commands::{Context, text};
use tuff_storage::backup::Phase;

use crate::tuff::{any_child, guarded, literal, string_argument};

pub const CREATE_NODE: &str = "tuff.command.backup.create";
pub const LIST_NODE: &str = "tuff.command.backup.list";
pub const STATUS_NODE: &str = "tuff.command.backup.status";
pub const CANCEL_NODE: &str = "tuff.command.backup.cancel";
pub const DELETE_NODE: &str = "tuff.command.backup.delete";

const NAME: CommandNode = string_argument("name", r#"{"type":"greedy"}"#, true, &[]);
const CREATE: CommandNode = guarded(
    literal("create", true, &[]),
    r#"{"type":"tuff:permission","node":"tuff.command.backup.create"}"#,
);
const LIST: CommandNode = guarded(
    literal("list", true, &[]),
    r#"{"type":"tuff:permission","node":"tuff.command.backup.list"}"#,
);
const STATUS: CommandNode = guarded(
    literal("status", true, &[]),
    r#"{"type":"tuff:permission","node":"tuff.command.backup.status"}"#,
);
const CANCEL: CommandNode = guarded(
    literal("cancel", true, &[]),
    r#"{"type":"tuff:permission","node":"tuff.command.backup.cancel"}"#,
);
const DELETE: CommandNode = guarded(
    literal("delete", false, &[NAME]),
    r#"{"type":"tuff:permission","node":"tuff.command.backup.delete"}"#,
);
pub const BACKUP: CommandNode = any_child(literal(
    "backup",
    false,
    &[CANCEL, CREATE, DELETE, LIST, STATUS],
));

fn fail(err: BackupError) -> CommandError {
    CommandError::text(text(err.to_string()))
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn phase(phase: Phase) -> &'static str {
    match phase {
        Phase::Saving => "saving the world",
        Phase::Copying => "copying",
        Phase::Compressing => "compressing",
        Phase::Verifying => "verifying",
        Phase::Finishing => "finishing",
    }
}

pub async fn run(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    let child = ctx.literals().get(2).copied();
    let node = match child {
        Some("create") => CREATE_NODE,
        Some("list") => LIST_NODE,
        Some("status") => STATUS_NODE,
        Some("cancel") => CANCEL_NODE,
        Some("delete") => DELETE_NODE,
        _ => return Err(CommandError::simple("command.unknown.command")),
    };
    if !ctx.permits(node) {
        return Err(CommandError::simple("command.unknown.command"));
    }
    let backups = ctx.shared().backups.clone();
    match child {
        Some("create") => {
            let running = backups
                .create(CreateOptions::by(ctx.source.name.clone()))
                .await
                .map_err(fail)?;
            ctx.reply(text(format!(
                "Backup {} started; /tuff backup status shows its progress",
                running.name
            )));
            Ok(1)
        }
        Some("list") => {
            let list = backups.list().await.map_err(fail)?;
            if list.is_empty() {
                ctx.reply(text("No backups"));
            } else {
                let names: Vec<String> = list
                    .iter()
                    .map(|b| format!("{} ({})", b.name, mib(b.size)))
                    .collect();
                ctx.reply(text(format!(
                    "{} backups: {}",
                    list.len(),
                    names.join(", ")
                )));
            }
            Ok(list.len() as i32)
        }
        Some("status") => {
            let status = backups.status().await;
            match &status.running {
                Some(running) => {
                    let p = &running.progress;
                    ctx.reply(text(format!(
                        "Backup {} by {}: {}, {}/{} files, {} of {}",
                        running.name,
                        running.by,
                        phase(p.phase),
                        p.files_done,
                        p.files_total,
                        mib(p.bytes_done),
                        mib(p.bytes_total)
                    )));
                }
                None => ctx.reply(text("No backup is running")),
            }
            if let Some(last) = &status.last {
                ctx.reply(text(match &last.error {
                    None => format!(
                        "Last backup {}: done, {}, saving held for {} ms",
                        last.name,
                        mib(last.size),
                        last.hold_ms
                    ),
                    Some(err) => format!("Last backup {} failed: {err}", last.name),
                }));
            }
            Ok(i32::from(status.running.is_some()))
        }
        Some("cancel") => {
            let name = backups.cancel().await.map_err(fail)?;
            ctx.reply(text(format!("Cancelling backup {name}")));
            Ok(1)
        }
        Some("delete") => {
            let Some(Value::Str(name)) = ctx.arg("name").cloned() else {
                return Err(CommandError::simple("command.unknown.command"));
            };
            let name = name.trim().to_owned();
            backups.delete(&name).await.map_err(fail)?;
            ctx.reply(text(format!("Deleted backup {name}")));
            Ok(1)
        }
        _ => Err(CommandError::simple("command.unknown.command")),
    }
}
