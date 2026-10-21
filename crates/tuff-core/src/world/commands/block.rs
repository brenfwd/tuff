//! Command blocks as a command source: gamemaster permission at the
//! block's centre, facing along the block, with the output it tracks.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::command::args::Anchor;
use crate::world::Shared;
use crate::world::dimension::DimensionId;
use tuff_item::text::TextComponent;
use tuff_storage::admin::PermissionLevel;

use super::console::ConsoleRuntime;
use super::{Actor, FeedbackTarget, Source, SourceKind};

pub struct BlockCommand {
    pub dimension: DimensionId,
    pub position: [f64; 3],
    pub yaw: f32,
    pub name: TextComponent,
    pub command: String,
    pub track_output: bool,
}

pub struct BlockOutcome {
    pub success: u32,
    pub output: Option<TextComponent>,
}

fn clock(seconds: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// Runs the command and waits for its feedback; the output is the last
/// message, stamped with the time of day (UTC).
pub async fn run(shared: Arc<Shared>, block: BlockCommand) -> BlockOutcome {
    let (feedback, mut lines) = mpsc::unbounded_channel();
    let target = if block.track_output {
        FeedbackTarget::CommandBlock(feedback.clone())
    } else {
        FeedbackTarget::Silent
    };
    let mut runtime = ConsoleRuntime::new(shared.clone(), feedback.clone());
    let source = Source {
        kind: SourceKind::Console(target),
        name: block.name.plain(),
        permission: PermissionLevel::Gamemasters,
        dimension: block.dimension,
        position: block.position,
        rotation: [0.0, block.yaw],
        anchor: Anchor::Feet,
        entity: None,
        silent: false,
    };
    let success = super::run_counted(Actor::Console(&mut runtime), source, &block.command).await;
    runtime.finish().await;
    let (reply, synced) = tokio::sync::oneshot::channel();
    if shared
        .coordinator
        .send(crate::world::messages::CoordinatorMessage::Sync { reply })
        .await
        .is_ok()
    {
        let _ = synced.await;
    }
    drop(feedback);
    let mut last = None;
    while let Ok(line) = lines.try_recv() {
        last = Some(line);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let output = last.map(|line| TextComponent::new(format!("[{}] {}", clock(now), line.plain())));
    BlockOutcome { success, output }
}
