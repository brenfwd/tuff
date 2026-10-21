//! Running parsed commands. A command runs on behalf of a [`Source`] (a
//! player or the console) inside an [`Actor`], which is the task that can
//! reach the world: a player's own session, or the console runtime.
//! Features register executors in [`Registry`], keyed by the top-level
//! literal; with no feature nothing is registered and every command is
//! unknown.
//!
//! Actors never wait on each other: a command that acts on other players
//! sends them directives and a detached collector delivers the feedback
//! once their replies are in (see [`targets`] and [`feedback`]).

pub mod block;
pub mod console;
pub mod feedback;
pub mod gamemode;
pub mod predicates;
pub mod registry;
pub mod session_state;
pub mod targets;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rand::rngs::StdRng;
use tokio::task::JoinHandle;

use crate::command::args::Anchor;
use crate::command::coords::Pose;
use crate::command::{CommandError, ParseContext, Parsed, ParsedNode, Value};
use crate::permission::Subject;
use crate::world::dimension::DimensionId;
use crate::world::messages::{
    CoordinatorMessage, Directive, DirectiveReply, DirectiveSource, PlayerHandle, PlayerId,
    RegionHandle,
};
use crate::world::session::PlayerSession;
use crate::world::{EYE_HEIGHT, Shared};
use tuff_item::text::TextComponent;
use tuff_storage::admin::PermissionLevel;
use tuff_world_model::pos::RegionPos;

pub use crate::world::messages::FeedbackTarget;
pub use registry::{
    ClientExtras, ClientNode, ClientTree, Registry, TextFuture, TextResolver, intern_name,
};

pub type ExecutorFuture<'a> = Pin<Box<dyn Future<Output = Result<i32, CommandError>> + Send + 'a>>;

/// What runs a command once its arguments are parsed.
pub trait Executor: Send + Sync {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> ExecutorFuture<'a>;

    /// Completions for an argument the client asks the server to complete
    /// (`ask_server`), given the parsed `path` so far. `None` leaves it to
    /// the argument's own suggestions.
    fn suggest(
        &self,
        _path: &[ParsedNode],
        _partial: &str,
        _input: &SuggestInput,
    ) -> Option<Vec<String>> {
        None
    }

    /// A tooltip for the completion `candidate` of `typed`, the text the
    /// player has typed so far (with its leading `/`).
    fn describe(
        &self,
        _typed: &str,
        _candidate: &str,
        _input: &SuggestInput,
    ) -> Option<TextComponent> {
        None
    }
}

/// What a suggestion request knows beyond the text typed so far.
pub struct SuggestInput<'a> {
    /// The dimension the asking player is in.
    pub dimension: &'a str,
    pub player_names: &'a [String],
    /// The live runtime settings, for completing config keys.
    pub config: &'a crate::config::ConfigStore,
    pub tracer: &'a tuff_trace::Tracer,
    /// The data packs, for completing pack and function names.
    pub datapacks: &'a crate::datapack::DataPacks,
    /// The permission nodes and settings, for completing node ids and
    /// group names.
    pub permissions: &'a crate::permission::View,
    /// The world's services, for completing what one of them holds
    /// (scheduled function ids, boss bar ids).
    pub services: &'a crate::world::services::Services,
}

pub fn executor_future<'a, F: Future<Output = Result<i32, CommandError>> + Send + 'a>(
    future: F,
) -> ExecutorFuture<'a> {
    Box::pin(future)
}

pub fn text(value: impl ToString) -> TextComponent {
    TextComponent::new(value.to_string())
}

#[derive(Debug, Clone)]
pub enum SourceKind {
    Player {
        id: PlayerId,
        uuid: u128,
        handle: PlayerHandle,
    },
    Console(FeedbackTarget),
}

/// Who a command runs as and from where.
#[derive(Debug, Clone)]
pub struct Source {
    pub kind: SourceKind,
    pub name: String,
    pub permission: PermissionLevel,
    pub dimension: DimensionId,
    pub position: [f64; 3],
    pub rotation: [f32; 2],
    pub anchor: Anchor,
    /// The entity the command runs as, for `@s`.
    pub entity: Option<i32>,
    /// Drop all output, like a source that accepts none.
    pub silent: bool,
}

impl Source {
    pub fn feedback_target(&self) -> FeedbackTarget {
        if self.silent {
            return FeedbackTarget::Silent;
        }
        match &self.kind {
            SourceKind::Player { handle, .. } => FeedbackTarget::Player(handle.clone()),
            SourceKind::Console(target) => target.clone(),
        }
    }

    pub fn player_id(&self) -> Option<PlayerId> {
        match &self.kind {
            SourceKind::Player { id, .. } => Some(*id),
            SourceKind::Console(_) => None,
        }
    }

    pub fn parse_context(&self) -> ParseContext {
        ParseContext {
            allow_selectors: self.permission.allows_selectors(),
        }
    }

    pub fn display_name(&self) -> TextComponent {
        TextComponent::new(self.name.clone())
    }

    /// Whose permission nodes apply: the player, the web panel user a
    /// console-like source runs for, the console at owner level, or else
    /// a source with only its level.
    pub fn subject(&self) -> Subject {
        let level = self.permission;
        match &self.kind {
            SourceKind::Player { uuid, .. } => Subject::Player { uuid: *uuid, level },
            SourceKind::Console(_) => match tuff_web_ui::panel_principal() {
                Some(tuff_web_ui::Principal::Operator { uuid, .. }) => {
                    Subject::Player { uuid, level }
                }
                _ if level >= PermissionLevel::Owners => Subject::Server,
                _ => Subject::Anonymous(level),
            },
        }
    }

    /// The source's eye height, which only a player entity has.
    fn eye_height(&self) -> f64 {
        match self.kind {
            SourceKind::Player { id, .. } if self.entity == Some(id) => EYE_HEIGHT,
            _ => 0.0,
        }
    }

    /// The position `~` and `^` resolve against.
    pub fn pose(&self) -> Pose {
        let mut anchor = self.position;
        if self.anchor == Anchor::Eyes {
            anchor[1] += self.eye_height();
        }
        Pose {
            position: self.position,
            rotation: self.rotation,
            anchor,
        }
    }

    /// The anchor point itself (`execute anchored eyes` moves `~` too
    /// only for `^`; commands like `facing` use it directly).
    pub fn anchor_position(&self) -> [f64; 3] {
        self.pose().anchor
    }
}

/// The task a command runs in.
pub enum Actor<'a> {
    Player(&'a mut PlayerSession),
    Console(&'a mut console::ConsoleRuntime),
}

impl Actor<'_> {
    pub fn player(&mut self) -> Option<&mut PlayerSession> {
        match self {
            Self::Player(session) => Some(session),
            Self::Console(_) => None,
        }
    }

    pub fn shared(&self) -> &Arc<Shared> {
        match self {
            Self::Player(session) => session.shared(),
            Self::Console(console) => &console.shared,
        }
    }

    pub fn rng(&mut self) -> &mut StdRng {
        match self {
            Self::Player(session) => session.rng(),
            Self::Console(console) => &mut console.rng,
        }
    }

    pub async fn region(
        &mut self,
        dimension: DimensionId,
        region: RegionPos,
    ) -> Option<RegionHandle> {
        match self {
            Self::Player(session) => session.region_in(dimension, region).await,
            Self::Console(console) => console.region(dimension, region).await,
        }
    }

    /// Applies a directive to this actor's own player, if `target` is it.
    pub async fn apply_self(
        &mut self,
        target: PlayerId,
        directive: Directive,
        source: DirectiveSource,
    ) -> Option<DirectiveReply> {
        match self {
            Self::Player(session) if session.id() == target => {
                Some(session.apply_directive(directive, source).await)
            }
            _ => None,
        }
    }

    /// This actor's own player's saved form, if `target` is it.
    pub fn own_selector_nbt(&self, target: PlayerId) -> Option<tuff_nbt::Compound> {
        match self {
            Self::Player(session) if session.id() == target => Some(session.selector_nbt()),
            _ => None,
        }
    }

    /// This actor's own player's live facts, if `target` is it.
    pub fn own_live_facts(&self, target: PlayerId) -> Option<crate::world::predicate::LiveFacts> {
        match self {
            Self::Player(session) if session.id() == target => Some(session.live_facts()),
            _ => None,
        }
    }

    /// Remembers a feedback collector; the console waits for them.
    pub fn track(&mut self, task: JoinHandle<()>) {
        match self {
            Self::Player(_) => {}
            Self::Console(console) => console.track(task),
        }
    }
}

/// How a function call ended early: `return <value>`, `return fail`, or a
/// `return run` whose command gave no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Value(i32),
    Failure,
    Ended,
}

struct FrameState {
    depth: u32,
    /// Calls that nest as futures rather than through a caller's own
    /// queue, which is what uses stack.
    nesting: u32,
    outcome: std::sync::Mutex<Option<Outcome>>,
    /// How the last function called directly from this call ended, for
    /// `return run function`.
    last_call: std::sync::Mutex<Option<Option<Outcome>>>,
    budget: Arc<std::sync::atomic::AtomicI64>,
    /// False for a frame that only watches calls made outside any function.
    call: bool,
    tracer: Option<Arc<dyn TraceCallbacks>>,
}

/// Receives every function call and command an execution makes, for
/// `/debug function`.
pub trait TraceCallbacks: Send + Sync {
    fn on_command(&self, depth: u32, command: &str);
    fn on_return(&self, depth: u32, command: &str, result: i32);
    fn on_error(&self, message: &str);
    fn on_call(&self, depth: u32, function: &str, size: usize);
}

/// The function call a command runs inside: how deep it is, whether a
/// `return` ended it, and the command budget its whole execution shares.
#[derive(Clone)]
pub struct Frame(Arc<FrameState>);

impl Frame {
    /// The outermost call of an execution that may run `budget` commands.
    pub fn root(budget: i64) -> Self {
        Self(Arc::new(FrameState {
            depth: 0,
            nesting: 0,
            outcome: std::sync::Mutex::new(None),
            last_call: std::sync::Mutex::new(None),
            budget: Arc::new(std::sync::atomic::AtomicI64::new(budget)),
            call: true,
            tracer: None,
        }))
    }

    /// Not a call, like [`Self::outside`]; calls made from it are roots
    /// whose calls and commands are reported to `tracer`.
    pub fn traced(budget: i64, tracer: Arc<dyn TraceCallbacks>) -> Self {
        Self(Arc::new(FrameState {
            depth: 0,
            nesting: 0,
            outcome: std::sync::Mutex::new(None),
            last_call: std::sync::Mutex::new(None),
            budget: Arc::new(std::sync::atomic::AtomicI64::new(budget)),
            call: false,
            tracer: Some(tracer),
        }))
    }

    pub fn tracer(&self) -> Option<&Arc<dyn TraceCallbacks>> {
        self.0.tracer.as_ref()
    }

    /// Not a call: stands where no function runs so the outermost call
    /// can record how it ended. Calls made from it are roots.
    pub fn outside(budget: i64) -> Self {
        Self(Arc::new(FrameState {
            depth: 0,
            nesting: 0,
            outcome: std::sync::Mutex::new(None),
            last_call: std::sync::Mutex::new(None),
            budget: Arc::new(std::sync::atomic::AtomicI64::new(budget)),
            call: false,
            tracer: None,
        }))
    }

    /// A call made from this one, sharing its budget.
    pub fn child(&self) -> Self {
        self.descendant(0)
    }

    /// A call made from inside a command this call runs, so it nests.
    pub fn nested(&self) -> Self {
        self.descendant(1)
    }

    fn descendant(&self, nesting: u32) -> Self {
        let (depth, nesting) = match self.0.call {
            true => (self.0.depth + 1, self.0.nesting + nesting),
            false => (0, 0),
        };
        Self(Arc::new(FrameState {
            depth,
            nesting,
            outcome: std::sync::Mutex::new(None),
            last_call: std::sync::Mutex::new(None),
            budget: self.0.budget.clone(),
            call: true,
            tracer: self.0.tracer.clone(),
        }))
    }

    pub fn depth(&self) -> u32 {
        self.0.depth
    }

    pub fn nesting(&self) -> u32 {
        self.0.nesting
    }

    /// How the call ended, once something returned from it.
    pub fn outcome(&self) -> Option<Outcome> {
        *self.0.outcome.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Ends the call; the first outcome wins.
    pub fn finish(&self, outcome: Outcome) {
        let mut slot = self.0.outcome.lock().unwrap_or_else(|p| p.into_inner());
        slot.get_or_insert(outcome);
    }

    /// Notes how a function this call ran ended (`None`: without a
    /// `return`).
    pub fn record_call(&self, outcome: Option<Outcome>) {
        *self.0.last_call.lock().unwrap_or_else(|p| p.into_inner()) = Some(outcome);
    }

    /// The last [`Self::record_call`] since the previous take.
    pub fn take_call(&self) -> Option<Option<Outcome>> {
        self.0
            .last_call
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }

    /// Takes one command from the budget; false once it is used up, which
    /// stops the whole execution.
    pub fn spend(&self) -> bool {
        self.0
            .budget
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed)
            > 0
    }

    pub fn exhausted(&self) -> bool {
        self.0.budget.load(std::sync::atomic::Ordering::Relaxed) <= 0
    }
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("depth", &self.depth())
            .field("outcome", &self.outcome())
            .finish()
    }
}

pub struct Context<'a> {
    pub actor: Actor<'a>,
    pub source: Source,
    /// The parsed nodes of this command, from its top-level literal.
    pub nodes: &'a [ParsedNode],
    /// The nodes after this command's redirect to the root, for
    /// `execute ... run`.
    pub rest: &'a [ParsedNode],
    pub input: &'a str,
    /// Something consumes this command's result value (`execute store`),
    /// so a command must not finish detached and report a guess.
    pub needs_result: bool,
    /// The function call this command is part of, if any.
    pub frame: Option<Frame>,
}

impl Context<'_> {
    pub fn arg(&self, name: &str) -> Option<&Value> {
        Parsed::value(self.nodes, name)
    }

    pub fn has(&self, literal: &str) -> bool {
        self.nodes.iter().any(|n| n.node.name == literal)
    }

    /// The names of the parsed nodes, literals and arguments alike.
    pub fn literals(&self) -> Vec<&'static str> {
        self.nodes.iter().map(|n| n.node.name).collect()
    }

    pub fn shared(&self) -> &Arc<Shared> {
        self.actor.shared()
    }

    /// The source's name as chat shows it: a player's decorated by its team.
    pub fn source_display_name(&self) -> TextComponent {
        match self.source.kind {
            SourceKind::Player { uuid, .. } => self
                .shared()
                .scoreboard
                .teams()
                .player_name(&self.source.name, uuid),
            SourceKind::Console(_) => self.source.display_name(),
        }
    }

    /// `target`'s name as command feedback shows it.
    pub fn name_of(&self, target: &targets::Target) -> TextComponent {
        target.feedback_name(&self.shared().scoreboard.teams())
    }

    /// Whether the source holds permission node `node`.
    pub fn permits(&self, node: &str) -> bool {
        self.shared()
            .permissions
            .allows(self.source.subject(), node)
    }

    pub fn rng(&mut self) -> &mut StdRng {
        self.actor.rng()
    }

    pub fn dimension_key(&self) -> &str {
        &self.shared().dimension(self.source.dimension).key
    }

    pub async fn coordinator(&self, message: CoordinatorMessage) {
        let _ = self.shared().coordinator.send(message).await;
    }

    /// Sends the coordinator a request built around a reply channel and
    /// waits for the answer. `None` if the world stopped first.
    pub async fn ask<T>(
        &self,
        request: impl FnOnce(tokio::sync::oneshot::Sender<T>) -> CoordinatorMessage,
    ) -> Option<T> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.coordinator(request(reply)).await;
        answer.await.ok()
    }

    /// Feedback to the source only, as the game's `sendSystemMessage`.
    pub fn reply(&self, message: TextComponent) {
        self.source.feedback_target().send(message);
    }

    /// A successful result: to the source (unless feedback is off) and,
    /// when `broadcast`, to operators.
    pub async fn success(&self, message: TextComponent, broadcast: bool) {
        feedback::success(
            self.shared(),
            &self.source.feedback_target(),
            self.source.display_name(),
            message,
            broadcast,
        )
        .await;
    }

    pub fn fail(&self, message: TextComponent) {
        feedback::fail(&self.source.feedback_target(), message);
    }

    /// Runs the command after `execute ... run` for `source`.
    pub async fn run_rest(&mut self, source: Source) -> Result<i32, CommandError> {
        if self.rest.is_empty() {
            return Err(CommandError::simple("command.unknown.command"));
        }
        let registry = self.shared().commands.clone();
        let (nodes, rest) = split_chain(self.rest);
        let Some(executor) = registry.executor(nodes[0].node.name) else {
            return Err(CommandError::simple("command.unknown.command"));
        };
        let actor = match &mut self.actor {
            Actor::Player(session) => Actor::Player(session),
            Actor::Console(console) => Actor::Console(console),
        };
        let mut inner = Context {
            actor,
            source,
            nodes,
            rest,
            input: self.input,
            needs_result: self.needs_result,
            frame: self.frame.clone(),
        };
        executor.execute(&mut inner).await
    }

    /// Whether a `return` has ended the function call this command is in.
    pub fn returned(&self) -> bool {
        self.frame.as_ref().is_some_and(|f| f.outcome().is_some())
    }

    /// Runs `parsed` (a parse of `input`) for `source` in this actor as part
    /// of `frame`, as a function runs each of its lines.
    pub async fn run_parsed(
        &mut self,
        source: Source,
        input: &str,
        parsed: &Parsed,
        frame: Option<Frame>,
    ) -> Result<i32, CommandError> {
        let registry = self.shared().commands.clone();
        let (nodes, rest) = split_chain(&parsed.nodes);
        let Some(executor) = nodes.first().and_then(|n| registry.executor(n.node.name)) else {
            return Err(CommandError::simple("command.unknown.command"));
        };
        let actor = match &mut self.actor {
            Actor::Player(session) => Actor::Player(session),
            Actor::Console(console) => Actor::Console(console),
        };
        let mut inner = Context {
            actor,
            source,
            nodes,
            rest,
            input,
            needs_result: false,
            frame,
        };
        let _timer = tuff_timings::timer!("command", tuff_timings::Tag::Name(nodes[0].node.name));
        executor.execute(&mut inner).await
    }
}

/// Splits parsed nodes into the first command's nodes and the nodes of a
/// command reached through a redirect to the root.
fn split_chain(nodes: &[ParsedNode]) -> (&[ParsedNode], &[ParsedNode]) {
    let next = nodes
        .iter()
        .skip(1)
        .position(|n| n.chain_start)
        .map(|i| i + 1)
        .unwrap_or(nodes.len());
    nodes.split_at(next)
}

/// Parses and runs `input` (without the leading `/`) for `source` in
/// `actor`, delivering errors to the source.
pub async fn run(actor: Actor<'_>, source: Source, input: &str) {
    run_counted(actor, source, input).await;
}

/// Like [`run`], returning how many commands succeeded (0 or 1).
pub async fn run_counted(actor: Actor<'_>, source: Source, input: &str) -> u32 {
    let shared = actor.shared().clone();
    let registry = shared.commands.clone();
    let access = shared.permissions.access(source.subject());
    let parsed = tuff_timings::timed!(
        "command.parse",
        registry
            .roots()
            .parse(input, &access, source.parse_context())
    );
    let feedback = source.feedback_target();
    if !matches!(actor, Actor::Player(_)) && parsed.error(input).is_some() {
        for claim in registry.line_claims() {
            if let Some(outcome) = claim.run(&source, &shared, input).await {
                for line in outcome.replies {
                    feedback.send(text(line));
                }
                return match outcome.result {
                    Ok(_) => 1,
                    Err(err) => {
                        feedback.send(text(err).color("red"));
                        0
                    }
                };
            }
        }
    }
    if let Some(err) = parsed.error(input) {
        for line in err.display(input) {
            feedback.send(line);
        }
        return 0;
    }
    let (nodes, rest) = split_chain(&parsed.nodes);
    let Some(first) = nodes.first() else {
        return 0;
    };
    let Some(executor) = registry.executor(first.node.name) else {
        for line in CommandError::simple("command.unknown.command")
            .at_cursor(0)
            .display(input)
        {
            feedback.send(line);
        }
        return 0;
    };
    let mut ctx = Context {
        actor,
        source,
        nodes,
        rest,
        input,
        needs_result: false,
        frame: None,
    };
    let _timer = tuff_timings::timer!("command", tuff_timings::Tag::Name(first.node.name));
    match executor.execute(&mut ctx).await {
        Ok(_) => 1,
        Err(err) => {
            for line in err.display(input) {
                feedback.send(line);
            }
            0
        }
    }
}
