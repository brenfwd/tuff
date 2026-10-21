//! `/tuff restart [seconds|cancel]`: saves and stops the server, which then
//! re-executes itself or exits with `tuff_core::restart::EXIT_CODE`
//! (`tuff.restart.mode`).

use std::time::Duration;

use tuff_core::command::args::Value;
use tuff_core::command::{CommandError, CommandNode, CommandNodeKind};
use tuff_core::restart;
use tuff_core::world::commands::{Context, text};
use tuff_core::world::messages::CoordinatorMessage;

use crate::Output;
use crate::tuff::literal;

const SECONDS: CommandNode = CommandNode {
    name: "seconds",
    kind: CommandNodeKind::Argument {
        parser: "brigadier:integer",
        properties: Some(r#"{"min":0,"max":3600}"#),
    },
    executable: true,
    redirect: None,
    permissions: None,
    children: &[],
};
const CANCEL: CommandNode = literal("cancel", true, &[]);
pub const RESTART: CommandNode = crate::tuff::guarded(
    literal("restart", true, &[CANCEL, SECONDS]),
    r#"{"type":"tuff:permission","node":"tuff.command.restart"}"#,
);

fn countdown_marks(seconds: u64) -> bool {
    matches!(seconds, 1..=5 | 10 | 15 | 30 | 60 | 120 | 300 | 600)
}

fn broadcast_countdown(ctx: &Context<'_>) -> bool {
    ctx.shared()
        .config
        .get_bool(restart::BROADCAST_KEY)
        .unwrap_or(true)
}

fn disconnect_text(ctx: &Context<'_>) -> Option<String> {
    let config = &ctx.shared().config;
    if !config
        .get_bool(restart::DISCONNECT_MESSAGE_KEY)
        .unwrap_or(true)
    {
        return None;
    }
    Some(
        config
            .get_string(restart::DISCONNECT_TEXT_KEY)
            .unwrap_or_else(|| restart::DEFAULT_DISCONNECT_TEXT.to_owned()),
    )
}

async fn say(
    out: &Output,
    coordinator: &tokio::sync::mpsc::Sender<CoordinatorMessage>,
    everyone: bool,
    message: String,
) {
    if everyone {
        let _ = coordinator
            .send(CoordinatorMessage::Announce(text(message.clone())))
            .await;
    }
    out.success(text(message), true).await;
}

pub async fn run(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
    if ctx.literals().get(2).copied() == Some("cancel") {
        if !restart::cancel() {
            return Err(CommandError::text(text("No restart is scheduled")));
        }
        if broadcast_countdown(ctx) {
            let _ = ctx
                .shared()
                .coordinator
                .send(CoordinatorMessage::Announce(text(
                    "Server restart cancelled",
                )))
                .await;
        }
        ctx.success(text("Server restart cancelled"), true).await;
        return Ok(1);
    }
    let seconds = match ctx.arg("seconds") {
        Some(Value::Int(s)) => *s as u64,
        _ => 0,
    };
    let Some(mut cancelled) = restart::begin_countdown() else {
        return Err(CommandError::text(text("A restart is already scheduled")));
    };
    let mode = restart::Mode::parse(
        ctx.shared()
            .config
            .get_string(restart::MODE_KEY)
            .as_deref()
            .unwrap_or("self"),
    )
    .unwrap_or(restart::Mode::SelfExec);
    let out = Output::of(ctx);
    let everyone = broadcast_countdown(ctx);
    let disconnect = disconnect_text(ctx);
    let coordinator = ctx.shared().coordinator.clone();
    if seconds == 0 {
        restart::finish_countdown();
        restart::set_disconnect_text(disconnect);
        restart::mark(mode);
        out.success(text("Restarting the server"), true).await;
        let _ = coordinator.send(CoordinatorMessage::Stop).await;
        return Ok(1);
    }
    say(
        &out,
        &coordinator,
        everyone,
        format!("Server restart in {seconds} s"),
    )
    .await;
    tokio::spawn(async move {
        let mut remaining = seconds;
        while remaining > 0 {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                _ = &mut cancelled => return,
            }
            remaining -= 1;
            if remaining > 0 && countdown_marks(remaining) {
                say(
                    &out,
                    &coordinator,
                    everyone,
                    format!("Server restart in {remaining} s"),
                )
                .await;
            }
        }
        restart::finish_countdown();
        restart::set_disconnect_text(disconnect);
        restart::mark(mode);
        out.success(text("Restarting the server"), true).await;
        let _ = coordinator.send(CoordinatorMessage::Stop).await;
    });
    Ok(1)
}
