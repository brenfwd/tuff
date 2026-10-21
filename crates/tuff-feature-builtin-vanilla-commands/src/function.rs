//! Data pack functions: `/function`, `/return`, `execute if function`,
//! and the service that runs the `#minecraft:load` and `#minecraft:tick`
//! tags.
//!
//! A call runs its commands in order until one of them returns; each
//! command's errors go to the (silent) function source. Every call and
//! command spends from the execution's budget
//! (`max_command_sequence_length`); once it is spent everything stops.
//!
//! Calls a function line makes directly (`function ...`, `execute ... run
//! function ...` without `store`, either behind `return run`) go on an
//! explicit stack, so recursion through them costs no Rust stack. Any
//! other call (through `execute store`, `execute if function`, or another
//! command) nests as a future; those are capped at [`MAX_NESTING`], and
//! past the first few levels each run of them is polled on a fresh thread
//! stack so the cap is reachable without overflowing.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;

use tokio::sync::watch;
use tuff_core::command::args::Value;
use tuff_core::command::nbt_path::NbtPath;
use tuff_core::command::{CommandError, Parsed, ParsedNode};
use tuff_core::datapack::function::{Call, PERMISSION};
use tuff_core::datapack::{Contents, Function};
use tuff_core::world::Shared;
use tuff_core::world::commands::console::{self, ConsoleRuntime};
use tuff_core::world::commands::{Actor, Frame, Outcome, TraceCallbacks};
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_storage::admin::PermissionLevel;

use super::data::Accessor;
use super::schedule::{Callback, Event, Scheduler};
use super::{Context, Executor, Source, executor_future, text};

/// How many calls may nest as futures before a call is refused.
pub const MAX_NESTING: u32 = 1024;
/// How many nested calls run on the stack of the task that started them.
const CALLER_LEVELS: u32 = 32;
/// How many further nested calls share each fresh thread stack; a level
/// costs up to ~13 KiB of stack in dev builds.
const SEGMENT_LEVELS: u32 = 256;
const SEGMENT_STACK_BYTES: usize = 16 << 20;
const LOAD: &str = "minecraft:load";
const TICK: &str = "minecraft:tick";

fn function_id(function: &Function) -> TextComponent {
    TextComponent::new(function.id.clone())
}

/// The functions `name` (a function, or a tag with `tag`) stands for.
pub fn resolve(
    contents: &Contents,
    name: &str,
    tag: bool,
) -> Result<Vec<Arc<Function>>, CommandError> {
    if tag {
        if !contents.has_tag(name) {
            return Err(CommandError::dynamic(
                "arguments.function.tag.unknown",
                [name.to_owned()],
            ));
        }
        Ok(contents.tag(name))
    } else {
        contents
            .function(name)
            .cloned()
            .map(|function| vec![function])
            .ok_or_else(|| CommandError::dynamic("arguments.function.unknown", [name.to_owned()]))
    }
}

fn named(nodes: &[ParsedNode], name: &str) -> Option<(String, bool)> {
    nodes.iter().find_map(|node| match &node.value {
        Some(Value::ResourceOrTag { name: id, tag }) if node.node.name == name => {
            Some((id.clone(), *tag))
        }
        _ => None,
    })
}

/// The source a function runs as: silent, and at most a gamemaster.
pub fn function_source(source: &Source) -> Source {
    let mut source = source.clone();
    source.silent = true;
    if source.permission > PERMISSION {
        source.permission = PERMISSION;
    }
    source
}

pub(crate) fn budget(shared: &Shared) -> i64 {
    i64::from(shared.rule_int("max_command_sequence_length").max(1))
}

fn spend(frame: &Frame) -> bool {
    if frame.exhausted() {
        return false;
    }
    if frame.spend() {
        return true;
    }
    tuff_core::log::info!("Command execution stopped due to limit");
    false
}

fn tag_type_name(tag: &Tag) -> &'static str {
    match tag {
        Tag::Byte(_) => "BYTE",
        Tag::Short(_) => "SHORT",
        Tag::Int(_) => "INT",
        Tag::Long(_) => "LONG",
        Tag::Float(_) => "FLOAT",
        Tag::Double(_) => "DOUBLE",
        Tag::ByteArray(_) => "BYTE[]",
        Tag::String(_) => "STRING",
        Tag::List(_) => "LIST",
        Tag::Compound(_) => "COMPOUND",
        Tag::IntArray(_) => "INT[]",
        Tag::LongArray(_) => "LONG[]",
    }
}

fn single_compound(path: &NbtPath, data: Compound) -> Result<Compound, CommandError> {
    let mut found = path.get(&Tag::Compound(data))?;
    if found.len() > 1 {
        return Err(CommandError::simple("commands.data.get.multiple"));
    }
    match found.pop() {
        Some(Tag::Compound(compound)) => Ok(compound),
        Some(other) => Err(CommandError::dynamic(
            "commands.function.error.argument_not_compound",
            [tag_type_name(&other)],
        )),
        None => Err(CommandError::simple("commands.data.get.multiple")),
    }
}

/// The macro arguments a `/function` command (`nodes`) gives, read as
/// `ctx.source`.
async fn arguments(
    ctx: &mut Context<'_>,
    nodes: &[ParsedNode],
) -> Result<Option<Compound>, CommandError> {
    if let Some(Value::Nbt(Tag::Compound(arguments))) = Parsed::value(nodes, "arguments") {
        return Ok(Some(arguments.clone()));
    }
    if !nodes.iter().any(|n| n.node.name == "with") {
        return Ok(None);
    }
    let source = Parsed::value(nodes, "source").cloned();
    let path = match Parsed::value(nodes, "path") {
        Some(Value::NbtPath(path)) => Some(path.clone()),
        _ => None,
    };
    let accessor = Accessor::from_value(ctx, source).await?;
    let data = accessor.read(ctx).await?;
    match path {
        Some(path) => single_compound(&path, data).map(Some),
        None => Ok(Some(data)),
    }
}

/// The functions a `/function` command (`nodes`) calls for `source`, each
/// prepared with its arguments.
async fn prepare(
    ctx: &mut Context<'_>,
    nodes: &[ParsedNode],
    source: &Source,
    key: &str,
) -> Result<Prepared, CommandError> {
    let Some((name, tag)) = named(nodes, "name") else {
        return Err(CommandError::simple("command.unknown.argument"));
    };
    let contents = ctx.shared().datapacks.contents();
    let functions = resolve(&contents, &name, tag)?;
    if functions.is_empty() {
        return Err(CommandError::dynamic(
            "commands.function.scheduled.no_functions",
            [name],
        ));
    }
    let outer = std::mem::replace(&mut ctx.source, source.clone());
    let arguments = arguments(ctx, nodes).await;
    ctx.source = outer;
    let arguments = arguments?;
    let roots = ctx.shared().commands.clone();
    functions
        .into_iter()
        .map(|function| {
            let call = function
                .prepare(arguments.as_ref(), roots.roots())
                .map_err(|reason| CommandError::dynamic(key, [function_id(&function), reason]))?;
            Ok((function, call))
        })
        .collect()
}

type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type Prepared = Vec<(Arc<Function>, Call)>;

// Futures are boxed from functions that are never inlined, so the frames
// that poll nested calls never hold them; see `execute`.
#[inline(never)]
fn prepare_boxed<'a>(
    ctx: &'a mut Context<'_>,
    nodes: &'a [ParsedNode],
    source: &'a Source,
    key: &'a str,
) -> Boxed<'a, Result<Prepared, CommandError>> {
    Box::pin(prepare(ctx, nodes, source, key))
}

#[inline(never)]
fn success_boxed<'a>(ctx: &'a Context<'_>, message: TextComponent) -> Boxed<'a, ()> {
    Box::pin(ctx.success(message, true))
}

fn split_chain(nodes: &[ParsedNode]) -> (&[ParsedNode], &[ParsedNode]) {
    let next = nodes
        .iter()
        .skip(1)
        .position(|n| n.chain_start)
        .map_or(nodes.len(), |i| i + 1);
    nodes.split_at(next)
}

/// A function line the call stack handles itself: `function`, behind an
/// optional `return run`, `execute ... run` and `return run`, in that
/// order.
struct DirectCall<'a> {
    /// `return run` before `execute`: the line ends the call even when
    /// `execute` leads nowhere.
    returning: bool,
    /// `return run` right before `function`.
    returns_result: bool,
    execute: Option<&'a [ParsedNode]>,
    function: &'a [ParsedNode],
}

fn is_return_run(head: &[ParsedNode]) -> bool {
    head.first().is_some_and(|n| n.node.name == "return")
        && head.iter().any(|n| n.node.name == "run")
}

fn direct_call(nodes: &[ParsedNode]) -> Option<DirectCall<'_>> {
    let (mut head, mut rest) = split_chain(nodes);
    let returning = is_return_run(head);
    if returning {
        (head, rest) = split_chain(rest);
    }
    let mut execute = None;
    if head.first()?.node.name == "execute" {
        execute = Some(head);
        (head, rest) = split_chain(rest);
    }
    let mut returns_result = returning;
    if execute.is_some() && is_return_run(head) {
        returns_result = true;
        (head, rest) = split_chain(rest);
    }
    (head.first()?.node.name == "function" && rest.is_empty()).then_some(DirectCall {
        returning,
        returns_result,
        execute,
        function: head,
    })
}

/// What happens when a task on the call stack ends.
enum Finish {
    /// The call the stack was started for; its outcome is the result.
    Root,
    Discard,
    /// `return run`: the outcome ends the caller, which sits at `keep - 1`.
    ReturnParent {
        keep: usize,
    },
}

struct Task {
    function: Arc<Function>,
    call: Arc<Call>,
    next: usize,
    started: bool,
    source: Source,
    frame: Frame,
    finish: Finish,
}

/// Runs `function` (prepared as `call`) for `source` as a call nested in
/// `ctx`'s frame, with every direct call it makes on one explicit stack.
/// `None` when it ran to its end (or was stopped) without returning.
#[inline(never)]
pub fn run_function<'a>(
    ctx: &'a mut Context<'_>,
    function: Arc<Function>,
    call: Call,
    source: Source,
) -> Pin<Box<dyn Future<Output = Option<Outcome>> + Send + 'a>> {
    let frame = match &ctx.frame {
        Some(parent) => parent.nested(),
        None => Frame::root(budget(ctx.shared())),
    };
    let nesting = frame.nesting();
    if nesting > MAX_NESTING {
        tuff_core::log::warn!(
            "Function {} not run: calls nested deeper than {MAX_NESTING}",
            function.id
        );
        return Box::pin(std::future::ready(None));
    }
    let body: Boxed<'a, Option<Outcome>> = Box::pin(async move {
        let mut stack = vec![Task {
            function,
            call: Arc::new(call),
            next: 0,
            started: false,
            source,
            frame,
            finish: Finish::Root,
        }];
        let mut result = None;
        while let Some(top) = stack.last_mut() {
            if !top.started {
                top.started = true;
                if let Some(tracer) = top.frame.tracer() {
                    tracer.on_call(
                        top.frame.depth(),
                        &top.function.id,
                        top.function.lines.len(),
                    );
                }
                if !spend(&top.frame) {
                    continue;
                }
            }
            let ended = top.frame.outcome().is_some()
                || top.frame.exhausted()
                || top.next >= top.function.lines.len();
            if ended {
                let Some(task) = stack.pop() else { break };
                let outcome = task.frame.outcome();
                match task.finish {
                    Finish::Root => result = outcome,
                    Finish::Discard => {}
                    Finish::ReturnParent { keep } => {
                        stack.truncate(keep);
                        if let Some(parent) = stack.last() {
                            parent.frame.finish(outcome.unwrap_or(Outcome::Ended));
                        }
                    }
                }
                continue;
            }
            let index = top.next;
            top.next += 1;
            if !spend(&top.frame) {
                continue;
            }
            let function = top.function.clone();
            let call = top.call.clone();
            let source = top.source.clone();
            let frame = top.frame.clone();
            let Some((input, parsed)) = function.command(&call, index) else {
                continue;
            };
            let tracer = frame.tracer().cloned();
            let depth = frame.depth() + 1;
            if let Some(tracer) = &tracer {
                tracer.on_command(depth, input);
            }
            let feedback = source.feedback_target();
            let report = |err: CommandError| {
                let lines = err.display(input);
                if let (Some(tracer), Some(first)) = (&tracer, lines.first()) {
                    tracer.on_error(&first.plain());
                }
                for line in lines {
                    feedback.send(line);
                }
            };
            let direct = direct_call(&parsed.nodes);
            let forked = match &direct {
                Some(DirectCall {
                    execute: Some(nodes),
                    ..
                }) => {
                    let outer = std::mem::replace(&mut ctx.source, source.clone());
                    let sources = super::execute::sources_before_run_boxed(ctx, nodes).await;
                    ctx.source = outer;
                    match sources {
                        Ok(sources) => sources,
                        Err(err) => {
                            report(err);
                            continue;
                        }
                    }
                }
                Some(_) => Some(vec![source.clone()]),
                None => None,
            };
            let (Some(direct), Some(sources)) = (direct, forked) else {
                match ctx
                    .run_parsed(source.clone(), input, parsed, Some(frame))
                    .await
                {
                    Ok(result) => {
                        if let Some(tracer) = &tracer {
                            tracer.on_return(depth, input, result);
                        }
                    }
                    Err(err) => report(err),
                }
                continue;
            };
            let keep = stack.len();
            let mut tasks = Vec::new();
            let mut failed = None;
            for source in &sources {
                let prepared = prepare_boxed(
                    ctx,
                    direct.function,
                    source,
                    "commands.function.instantiationFailure",
                )
                .await;
                match prepared {
                    Ok(calls) => {
                        for (function, call) in calls {
                            tasks.push(Task {
                                function,
                                call: Arc::new(call),
                                next: 0,
                                started: false,
                                source: function_source(source),
                                frame: frame.child(),
                                finish: if direct.returns_result {
                                    Finish::ReturnParent { keep }
                                } else {
                                    Finish::Discard
                                },
                            });
                        }
                    }
                    Err(err) => {
                        failed = Some(err);
                        break;
                    }
                }
            }
            if let Some(err) = failed {
                report(err);
                continue;
            }
            if direct.returning && tasks.is_empty() {
                frame.finish(Outcome::Ended);
            }
            stack.extend(tasks.into_iter().rev());
        }
        result
    });
    if nesting >= CALLER_LEVELS && (nesting - CALLER_LEVELS).is_multiple_of(SEGMENT_LEVELS) {
        Box::pin(FreshStack(body))
    } else {
        body
    }
}

/// Polls a future on a scoped thread with its own large stack, inside the
/// caller's runtime context and tags in effect.
struct FreshStack<'a, T>(Boxed<'a, T>);

impl<T: Send> Future for FreshStack<'_, T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<T> {
        let waker = cx.waker().clone();
        let runtime = tokio::runtime::Handle::try_current().ok();
        let tags = tuff_datapack::tags::current();
        let inner = self.0.as_mut();
        let polled = std::thread::scope(|scope| {
            let spawned = std::thread::Builder::new()
                .name("function-stack".into())
                .stack_size(SEGMENT_STACK_BYTES)
                .spawn_scoped(scope, move || {
                    let _entered = runtime.as_ref().map(|runtime| runtime.enter());
                    tuff_datapack::tags::sync_scope(tags, || {
                        inner.poll(&mut std::task::Context::from_waker(&waker))
                    })
                });
            spawned.ok().map(|thread| {
                thread
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
        });
        match polled {
            Some(poll) => poll,
            None => self.0.as_mut().poll(cx),
        }
    }
}

/// `/function <name> [arguments | with ...]`.
pub struct FunctionCommand;

impl Executor for FunctionCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let caller = ctx.source.clone();
            let nodes = ctx.nodes;
            let calls = prepare_boxed(
                ctx,
                nodes,
                &caller,
                "commands.function.instantiationFailure",
            )
            .await?;
            let message = match calls.as_slice() {
                [(single, _)] => TextComponent::translate(
                    "commands.function.scheduled.single",
                    [function_id(single)],
                ),
                many => {
                    let mut list = TextComponent::new("");
                    for (index, (function, _)) in many.iter().enumerate() {
                        if index > 0 {
                            list = list.append(", ");
                        }
                        list = list.append(function_id(function));
                    }
                    TextComponent::translate("commands.function.scheduled.multiple", [list])
                }
            };
            success_boxed(ctx, message).await;
            let source = function_source(&caller);
            let mut total = None::<i32>;
            let mut last = None;
            for (function, call) in calls {
                let id = function_id(&function);
                let outcome = run_function(ctx, function, call, source.clone()).await;
                last = outcome;
                if let Some(Outcome::Value(value)) = outcome {
                    if !caller.silent {
                        success_boxed(
                            ctx,
                            TextComponent::translate("commands.function.result", [id, text(value)]),
                        )
                        .await;
                    }
                    total = Some(total.unwrap_or(0).wrapping_add(value));
                }
            }
            if let Some(frame) = &ctx.frame {
                frame.record_call(last);
            }
            match (total, last) {
                (Some(total), _) => Ok(total),
                (None, Some(Outcome::Failure)) => Err(CommandError::silent()),
                _ => Ok(0),
            }
        })
    }

    fn suggest(
        &self,
        _path: &[ParsedNode],
        partial: &str,
        input: &super::SuggestInput,
    ) -> Option<Vec<String>> {
        Some(suggest_functions(&input.datapacks.contents(), partial))
    }
}

/// Function ids and `#`tags that complete `partial`.
pub fn suggest_functions(contents: &Contents, partial: &str) -> Vec<String> {
    let matches = |id: &str| {
        id.starts_with(partial)
            || id.split_once(':').is_some_and(|(namespace, path)| {
                path.starts_with(partial) || namespace.starts_with(partial)
            })
    };
    let tags = contents.function_tags.keys().map(|id| format!("#{id}"));
    let functions = contents.functions.keys().cloned();
    tags.chain(functions)
        .filter(|id| id.starts_with(partial) || matches(id.trim_start_matches('#')))
        .collect()
}

/// `/return <value> | fail | run <command>`.
pub struct ReturnCommand;

impl Executor for ReturnCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let frame = ctx.frame.clone();
            if ctx.has("run") {
                if let Some(frame) = &frame {
                    frame.take_call();
                }
                let source = ctx.source.clone();
                let result = ctx.run_rest(source).await;
                if let Some(frame) = &frame {
                    let outcome = match frame.take_call() {
                        Some(Some(outcome)) => outcome,
                        Some(None) => Outcome::Ended,
                        None => match &result {
                            Ok(value) => Outcome::Value(*value),
                            Err(_) => Outcome::Failure,
                        },
                    };
                    frame.finish(outcome);
                }
                return result;
            }
            if ctx.has("fail") {
                if let Some(frame) = &frame {
                    frame.finish(Outcome::Failure);
                }
                return Err(CommandError::silent());
            }
            let value = match ctx.arg("value") {
                Some(Value::Int(value)) => *value,
                _ => return Err(CommandError::simple("command.unknown.argument")),
            };
            if let Some(frame) = &frame {
                frame.finish(Outcome::Value(value));
            }
            Ok(value)
        })
    }
}

/// `execute if|unless function <name>` for one source: runs the functions
/// in order until one returns, then tests the value (non-zero for `if`,
/// zero for `unless`; a failure counts as zero). A source whose functions
/// never return passes neither test.
pub async fn condition(
    ctx: &mut Context<'_>,
    source: &Source,
    nodes: &[ParsedNode],
    expected: bool,
) -> Result<bool, CommandError> {
    let Some((name, tag)) = named(nodes, "name") else {
        return Err(CommandError::simple("command.unknown.argument"));
    };
    let contents = ctx.shared().datapacks.contents();
    let functions = resolve(&contents, &name, tag)?;
    let roots = ctx.shared().commands.clone();
    let mut calls = Vec::with_capacity(functions.len());
    for function in functions {
        let call = function.prepare(None, roots.roots()).map_err(|reason| {
            CommandError::dynamic(
                "commands.execute.function.instantiationFailure",
                [function_id(&function), reason],
            )
        })?;
        calls.push((function, call));
    }
    let source = function_source(source);
    for (function, call) in calls {
        let value = match run_function(ctx, function, call, source.clone()).await {
            Some(Outcome::Value(value)) => value,
            Some(Outcome::Failure) => 0,
            Some(Outcome::Ended) | None => continue,
        };
        return Ok(if expected { value != 0 } else { value == 0 });
    }
    Ok(false)
}

/// Runs the functions of `tag` as the server: at the world spawn, silent,
/// with gamemaster permission.
pub async fn run_tag(shared: &Arc<Shared>, contents: &Contents, tag: &str) {
    run_as_server(shared, contents.tag(tag)).await;
}

/// Runs what a due scheduled event names; a function or tag that no
/// longer exists runs nothing.
async fn run_scheduled(shared: &Arc<Shared>, contents: &Contents, event: Event) {
    let functions = match event.callback {
        Callback::Function(id) => contents.function(&id).cloned().into_iter().collect(),
        Callback::Tag(id) => contents.tag(&id),
    };
    run_as_server(shared, functions).await;
}

/// Runs `functions` in order as the server (see [`run_tag`]).
pub async fn run_as_server(shared: &Arc<Shared>, functions: Vec<Arc<Function>>) {
    if functions.is_empty() {
        return;
    }
    let (feedback, _lines) = tokio::sync::mpsc::unbounded_channel();
    let mut runtime = ConsoleRuntime::new(shared.clone(), feedback);
    let mut source = console::source(shared, super::FeedbackTarget::Silent);
    source.permission = PermissionLevel::Gamemasters;
    source.silent = true;
    {
        let mut ctx = Context {
            actor: Actor::Console(&mut runtime),
            source: source.clone(),
            nodes: &[],
            rest: &[],
            input: "",
            needs_result: false,
            frame: None,
        };
        for function in functions {
            let Ok(call) = function.prepare(None, shared.commands.roots()) else {
                continue;
            };
            run_function(&mut ctx, function, call, source.clone()).await;
        }
    }
    runtime.finish().await;
}

/// Runs `#minecraft:load` after every reload, `#minecraft:tick` on every
/// game tick and then the scheduled functions that are due.
pub struct FunctionTicker {
    done: watch::Sender<u64>,
    scheduler: std::sync::OnceLock<Scheduler>,
}

impl FunctionTicker {
    /// The `/schedule` queue, once the world has started.
    pub fn scheduler(&self) -> Option<Scheduler> {
        self.scheduler.get().cloned()
    }
}

pub struct FunctionTickerFactory;

impl ServiceFactory for FunctionTickerFactory {
    fn create(
        &self,
        _dimensions: &tuff_core::world::dimension::Dimensions,
    ) -> Arc<dyn WorldService> {
        Arc::new(FunctionTicker {
            done: watch::Sender::new(0),
            scheduler: std::sync::OnceLock::new(),
        })
    }
}

impl WorldService for FunctionTicker {
    fn start(&self, shared: &Arc<Shared>) {
        let scheduler = self
            .scheduler
            .get_or_init(|| Scheduler::start(shared.clone()))
            .clone();
        let shared = shared.clone();
        let done = self.done.clone();
        tokio::spawn(async move {
            let mut clock = shared.clock.clone();
            let member = shared.tick_barrier.join(*clock.borrow());
            done.send_replace(*clock.borrow());
            let mut loaded = 0;
            while clock.changed().await.is_ok() {
                let tick = *clock.borrow_and_update();
                let contents = shared.datapacks.contents();
                if contents.generation != loaded {
                    loaded = contents.generation;
                    run_tag(&shared, &contents, LOAD).await;
                }
                run_tag(&shared, &contents, TICK).await;
                for event in scheduler.take_due(tick as i64).await {
                    run_scheduled(&shared, &contents, event).await;
                }
                member.finished(tick);
                done.send_replace(tick);
            }
        });
    }

    fn settled(&self, tick: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let mut done = self.done.subscribe();
        Box::pin(async move {
            let _ = done.wait_for(|finished| *finished >= tick).await;
        })
    }

    fn flush(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if let Some(scheduler) = self.scheduler() {
                scheduler.flush().await;
            }
        })
    }
}

static PROFILERS: std::sync::Mutex<Vec<(usize, std::time::Instant, u64)>> =
    std::sync::Mutex::new(Vec::new());

fn profiler_key(shared: &Arc<Shared>) -> usize {
    Arc::as_ptr(shared) as usize
}

/// `/debug start|stop|function`: a tick-rate profiler over a stretch of
/// play, and a run of functions with a file naming what ran.
pub struct DebugCommand;

impl Executor for DebugCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            let literals = ctx.literals();
            let key = profiler_key(ctx.shared());
            let ticks = *ctx.shared().clock.borrow();
            if literals.contains(&"start") {
                let began = {
                    let mut profilers = PROFILERS.lock().unwrap_or_else(|e| e.into_inner());
                    let free = !profilers.iter().any(|(k, ..)| *k == key);
                    if free {
                        profilers.push((key, std::time::Instant::now(), ticks));
                    }
                    free
                };
                if !began {
                    return Err(CommandError::simple("commands.debug.alreadyRunning"));
                }
                ctx.success(TextComponent::translate("commands.debug.started", []), true)
                    .await;
                return Ok(0);
            }
            if literals.contains(&"stop") {
                let started = {
                    let mut profilers = PROFILERS.lock().unwrap_or_else(|e| e.into_inner());
                    profilers
                        .iter()
                        .position(|(k, ..)| *k == key)
                        .map(|index| profilers.remove(index))
                };
                let Some((_, instant, first)) = started else {
                    return Err(CommandError::simple("commands.debug.notRunning"));
                };
                let seconds = instant.elapsed().as_secs_f64();
                let elapsed = ticks.saturating_sub(first);
                let rate = if seconds > 0.0 {
                    elapsed as f64 / seconds
                } else {
                    0.0
                };
                ctx.success(
                    TextComponent::translate(
                        "commands.debug.stopped",
                        [
                            text(format!("{seconds:.2}")),
                            text(elapsed),
                            text(format!("{rate:.2}")),
                        ],
                    ),
                    true,
                )
                .await;
                return Ok(rate as i32);
            }
            let caller = ctx.source.clone();
            let nodes = ctx.nodes;
            let calls = prepare_boxed(
                ctx,
                nodes,
                &caller,
                "commands.function.instantiationFailure",
            )
            .await?;
            if ctx.frame.as_ref().is_some_and(|f| f.tracer().is_some()) {
                return Err(CommandError::simple("commands.debug.function.noRecursion"));
            }
            let stamp = chrono_free_stamp();
            let output = format!("debug-trace-{stamp}.txt");
            let tracer = ctx.shared().world_path.as_deref().and_then(|world| {
                let dir = world.parent()?.join("debug");
                std::fs::create_dir_all(&dir).ok()?;
                FileTracer::create(&dir.join(&output))
            });
            let Some(tracer) = tracer.map(Arc::new) else {
                return Err(CommandError::simple("commands.debug.function.traceFailed"));
            };
            let source = function_source(&caller);
            let mut total = 0;
            let mut ids = Vec::new();
            let outer = ctx.frame.replace(Frame::traced(
                budget(ctx.shared()),
                tracer.clone() as Arc<dyn TraceCallbacks>,
            ));
            for (function, call) in calls {
                total += function.lines.len() as i32;
                ids.push(function_id(&function));
                tracer.line(&function.id);
                run_function(ctx, function, call, source.clone()).await;
            }
            ctx.frame = outer;
            tracer.finish();
            let message = match ids.as_slice() {
                [single] => TextComponent::translate(
                    "commands.debug.function.success.single",
                    [text(total), single.clone(), text(&output)],
                ),
                many => TextComponent::translate(
                    "commands.debug.function.success.multiple",
                    [text(total), text(many.len()), text(&output)],
                ),
            };
            ctx.success(message, true).await;
            Ok(total)
        })
    }
}

/// Writes a `/debug function` trace in vanilla's layout.
pub struct FileTracer {
    state: std::sync::Mutex<TraceState>,
}

struct TraceState {
    out: String,
    file: std::fs::File,
    last_indent: u32,
    waiting: bool,
}

impl TraceState {
    fn newline(&mut self) {
        if self.waiting {
            self.out.push('\n');
            self.waiting = false;
        }
    }

    fn indent(&mut self, value: u32) {
        for _ in 0..=value {
            self.out.push_str("    ");
        }
    }

    fn indent_and_save(&mut self, value: u32) {
        self.indent(value);
        self.last_indent = value;
    }
}

impl FileTracer {
    pub fn create(path: &std::path::Path) -> Option<Self> {
        Some(Self {
            state: std::sync::Mutex::new(TraceState {
                out: String::new(),
                file: std::fs::File::create(path).ok()?,
                last_indent: 0,
                waiting: false,
            }),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TraceState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn line(&self, text: &str) {
        let mut state = self.lock();
        state.out.push_str(text);
        state.out.push('\n');
    }

    pub fn finish(&self) {
        use std::io::Write;
        let mut state = self.lock();
        state.newline();
        let out = std::mem::take(&mut state.out);
        state.file.write_all(out.as_bytes()).ok();
    }
}

impl TraceCallbacks for FileTracer {
    fn on_command(&self, depth: u32, command: &str) {
        let mut state = self.lock();
        state.newline();
        state.indent_and_save(depth);
        state.out.push_str("[C] ");
        state.out.push_str(command);
        state.waiting = true;
    }

    fn on_return(&self, depth: u32, command: &str, result: i32) {
        let mut state = self.lock();
        if state.waiting {
            state.out.push_str(&format!(" -> {result}\n"));
            state.waiting = false;
        } else {
            state.indent_and_save(depth);
            state.out.push_str(&format!("[R = {result}] {command}\n"));
        }
    }

    fn on_error(&self, message: &str) {
        let mut state = self.lock();
        state.newline();
        let indent = state.last_indent + 1;
        state.indent_and_save(indent);
        state.out.push_str("[E] ");
        state.out.push_str(message);
        state.waiting = true;
    }

    fn on_call(&self, depth: u32, function: &str, size: usize) {
        let mut state = self.lock();
        state.newline();
        state.indent_and_save(depth);
        state.out.push_str(&format!("[F] {function} size={size}\n"));
    }
}

fn chrono_free_stamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    seconds.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::Block;
    use tuff_vanilla_data::tag::block::key::LOGS;

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn fresh_stacks_keep_the_tags_in_effect() {
        let pack = tuff_datapack::tags::vanilla_with(&[(
            "minecraft:block",
            "minecraft:logs",
            "minecraft:stone",
        )]);
        let probe = || -> Boxed<'static, bool> {
            Box::pin(async { tuff_datapack::tags::is(LOGS, Block::Stone) })
        };
        assert!(!FreshStack(probe()).await);
        assert!(tuff_datapack::tags::scope(pack, FreshStack(probe())).await);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_tracer_writes_calls_commands_results_and_errors() {
        let path = std::env::temp_dir().join(format!("tuff-trace-{}.txt", std::process::id()));
        let tracer = FileTracer::create(&path).unwrap();
        tracer.line("test:main");
        tracer.on_call(0, "test:main", 3);
        tracer.on_command(1, "say hi");
        tracer.on_return(1, "say hi", 1);
        tracer.on_command(1, "bad");
        tracer.on_error("Unknown command");
        tracer.on_call(1, "test:sub", 1);
        tracer.finish();
        let written = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(
            written,
            "test:main\n    [F] test:main size=3\n        [C] say hi -> 1\n        [C] bad\n            [E] Unknown command\n        [F] test:sub size=1\n"
        );
    }
}
