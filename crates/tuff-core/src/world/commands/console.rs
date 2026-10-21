//! The server console as a command source: no position beyond the world
//! spawn, every permission, and feedback collected as lines.

use std::collections::HashMap;
use std::sync::Arc;

use rand::SeedableRng;
use rand::rngs::StdRng;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::command::args::Anchor;
use crate::world::Shared;
use crate::world::dimension::DimensionId;
use crate::world::messages::{ManagerMessage, RegionHandle};
use tuff_item::text::TextComponent;
use tuff_storage::admin::PermissionLevel;
use tuff_world_model::pos::RegionPos;

use super::{Actor, FeedbackTarget, Source, SourceKind};

pub const CONSOLE_NAME: &str = "Server";

/// What a console command has while it runs: region tickets it took, and
/// the feedback collectors it is waiting on.
pub struct ConsoleRuntime {
    pub shared: Arc<Shared>,
    pub feedback: mpsc::UnboundedSender<TextComponent>,
    regions: HashMap<(DimensionId, RegionPos), RegionHandle>,
    pending: Vec<JoinHandle<()>>,
    pub rng: StdRng,
}

impl ConsoleRuntime {
    pub fn new(shared: Arc<Shared>, feedback: mpsc::UnboundedSender<TextComponent>) -> Self {
        Self {
            shared,
            feedback,
            regions: HashMap::new(),
            pending: Vec::new(),
            rng: StdRng::from_entropy(),
        }
    }

    pub async fn region(
        &mut self,
        dimension: DimensionId,
        region: RegionPos,
    ) -> Option<RegionHandle> {
        if let Some(handle) = self.regions.get(&(dimension, region)) {
            return Some(handle.clone());
        }
        let (reply, handle) = oneshot::channel();
        self.shared
            .manager
            .send(ManagerMessage::Acquire {
                dimension,
                region,
                reply,
            })
            .await
            .ok()?;
        let handle = handle.await.ok()?;
        self.regions.insert((dimension, region), handle.clone());
        Some(handle)
    }

    pub fn track(&mut self, task: JoinHandle<()>) {
        self.pending.push(task);
    }

    /// Waits for outstanding feedback and gives the region tickets back.
    pub async fn finish(mut self) {
        for task in self.pending.drain(..) {
            let _ = task.await;
        }
        for ((dimension, region), _) in self.regions.drain() {
            let _ = self
                .shared
                .manager
                .send(ManagerMessage::Release { dimension, region })
                .await;
        }
    }
}

/// The console's source: the world spawn, looking straight ahead.
pub fn source(shared: &Shared, feedback: FeedbackTarget) -> Source {
    let spawn = shared.spawn();
    Source {
        kind: SourceKind::Console(feedback),
        name: CONSOLE_NAME.to_owned(),
        permission: PermissionLevel::Owners,
        dimension: shared.spawn_dimension(),
        position: [
            spawn.pos.x as f64 + 0.5,
            spawn.pos.y as f64,
            spawn.pos.z as f64 + 0.5,
        ],
        rotation: [0.0, 0.0],
        anchor: Anchor::Feet,
        entity: None,
        silent: false,
    }
}

/// Runs `line` (with or without a leading `/`) as the console, returning
/// every line of feedback once the command has fully run.
pub async fn run(shared: Arc<Shared>, line: &str) -> Vec<TextComponent> {
    let input = line
        .trim()
        .strip_prefix('/')
        .unwrap_or(line.trim())
        .to_owned();
    let (feedback, mut lines) = mpsc::unbounded_channel();
    let mut runtime = ConsoleRuntime::new(shared.clone(), feedback.clone());
    let source = source(&shared, FeedbackTarget::Channel(feedback.clone()));
    super::run(Actor::Console(&mut runtime), source, &input).await;
    runtime.finish().await;
    let (reply, synced) = oneshot::channel();
    if shared
        .coordinator
        .send(crate::world::messages::CoordinatorMessage::Sync { reply })
        .await
        .is_ok()
    {
        let _ = synced.await;
    }
    drop(feedback);
    let mut out = Vec::new();
    while let Ok(line) = lines.try_recv() {
        out.push(line);
    }
    out
}

/// Runs `line` as a command block would: at `position` in `dimension`
/// with gamemaster permission, named `name`, and no feedback.
pub async fn run_at(
    shared: Arc<Shared>,
    line: &str,
    name: String,
    dimension: DimensionId,
    position: [f64; 3],
) {
    let input = line.trim().strip_prefix('/').unwrap_or(line.trim());
    let (feedback, _lines) = mpsc::unbounded_channel();
    let mut runtime = ConsoleRuntime::new(shared.clone(), feedback);
    let mut at = source(&shared, FeedbackTarget::Silent);
    at.name = name;
    at.permission = PermissionLevel::Gamemasters;
    at.dimension = dimension;
    at.position = position;
    super::run(Actor::Console(&mut runtime), at, input).await;
    runtime.finish().await;
}

/// Runs `line` like [`run`], but as `name` with `permission`, so level
/// checks and operator feedback treat it as that source (a web panel
/// user rather than the owner console).
pub async fn run_as(
    shared: Arc<Shared>,
    line: &str,
    name: String,
    permission: PermissionLevel,
) -> Vec<TextComponent> {
    run_as_counted(shared, line, name, permission).await.0
}

/// [`run_as`], also returning whether the command succeeded (1) or not
/// (0).
pub async fn run_as_counted(
    shared: Arc<Shared>,
    line: &str,
    name: String,
    permission: PermissionLevel,
) -> (Vec<TextComponent>, u32) {
    let input = line
        .trim()
        .strip_prefix('/')
        .unwrap_or(line.trim())
        .to_owned();
    let (feedback, mut lines) = mpsc::unbounded_channel();
    let mut runtime = ConsoleRuntime::new(shared.clone(), feedback.clone());
    let mut as_source = source(&shared, FeedbackTarget::Channel(feedback.clone()));
    as_source.name = name;
    as_source.permission = permission;
    let succeeded = super::run_counted(Actor::Console(&mut runtime), as_source, &input).await;
    runtime.finish().await;
    let (reply, synced) = oneshot::channel();
    if shared
        .coordinator
        .send(crate::world::messages::CoordinatorMessage::Sync { reply })
        .await
        .is_ok()
    {
        let _ = synced.await;
    }
    drop(feedback);
    let mut out = Vec::new();
    while let Ok(line) = lines.try_recv() {
        out.push(line);
    }
    (out, succeeded)
}

/// Completions for `text` (with or without the leading `/`) for a
/// console-like source acting for `subject`.
pub async fn suggest(
    shared: &Arc<Shared>,
    text: &str,
    subject: crate::permission::Subject,
) -> crate::command::Suggestions {
    let claimed_source = source(shared, FeedbackTarget::Silent);
    if let Some(found) = super::registry::claimed_suggestions(shared, &claimed_source, text).await {
        return found;
    }
    let permission = subject.level();
    let text = if text.starts_with('/') {
        text.to_owned()
    } else {
        format!("/{text}")
    };
    let (reply, players) = oneshot::channel();
    let _ = shared
        .coordinator
        .send(crate::world::messages::CoordinatorMessage::Players { reply })
        .await;
    let player_names: Vec<String> = players
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.name)
        .collect();
    let (objectives, teams) = shared
        .scoreboard
        .read(|board| {
            (
                board.objectives().map(|o| o.name.clone()).collect(),
                board.teams().map(|t| t.name.clone()).collect(),
            )
        })
        .await
        .unwrap_or_default();
    let context = crate::command::SuggestContext {
        allow_selectors: permission.allows_selectors(),
        player_names: player_names.clone(),
        dimensions: shared
            .dimensions
            .keys()
            .into_iter()
            .map(str::to_owned)
            .collect(),
        objectives,
        teams,
        advancements: Some(shared.advancements()),
        tags: Some(shared.datapacks.contents().tags.clone()),
    };
    let dimension = shared.dimension(shared.spawn_dimension()).key.clone();
    let commands = shared.commands.clone();
    let view = shared.permissions.view();
    let input = super::SuggestInput {
        dimension: &dimension,
        player_names: &player_names,
        config: &shared.config,
        tracer: &shared.tracer,
        datapacks: &shared.datapacks,
        permissions: &view,
        services: &shared.services,
    };
    let custom = |_provider: &str, path: &[crate::command::ParsedNode], partial: &str| {
        let command = path.first()?.node.name;
        commands.executor(command)?.suggest(path, partial, &input)
    };
    commands
        .roots()
        .suggest(&text, view.access(subject), &context, &custom)
}
