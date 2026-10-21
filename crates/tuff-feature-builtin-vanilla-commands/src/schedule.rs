//! `/schedule`: functions and function tags queued to run at a later game
//! tick. One actor owns the queue and saves it as the
//! `minecraft:scheduled_events` saved data, like the game: when dirty on
//! autosave, and on every flush. The function ticker takes the due events
//! each tick, after `#minecraft:tick`.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tuff_core::command::CommandError;
use tuff_core::command::ParsedNode;
use tuff_core::command::args::Value;
use tuff_core::storage::level::{inner, saved};
use tuff_core::world::Shared;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_storage::backend::WorldBackend;
use tuff_storage::document::Document;

use super::function::{FunctionTicker, resolve, suggest_functions};
use super::{Context, Executor, executor_future, text};

/// The saved data the queue is kept in.
pub const SAVED_DATA: &str = "minecraft:scheduled_events";
const FUNCTION: &str = "minecraft:function";
const FUNCTION_TAG: &str = "minecraft:function_tag";
/// How often a changed queue is saved, in ticks.
const AUTOSAVE_TICKS: u64 = 6000;
/// Bounded: senders are commands and the ticker, which can wait their turn.
const INBOX: usize = 256;

/// What runs when an event is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callback {
    Function(String),
    Tag(String),
}

impl Callback {
    fn to_nbt(&self) -> Compound {
        let (kind, id) = match self {
            Self::Function(id) => (FUNCTION, id),
            Self::Tag(id) => (FUNCTION_TAG, id),
        };
        Compound::new().with("type", kind).with("id", id.as_str())
    }

    fn from_nbt(nbt: &Compound) -> Option<Self> {
        let id = nbt.get_str("id")?.to_owned();
        match tuff_core::command::args::qualify(nbt.get_str("type")?).as_str() {
            FUNCTION => Some(Self::Function(id)),
            FUNCTION_TAG => Some(Self::Tag(id)),
            _ => None,
        }
    }
}

/// A queued callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub trigger_time: i64,
    pub callback: Callback,
}

/// The events by trigger time, then by the order they were queued in.
#[derive(Debug, Default)]
pub struct Queue {
    events: BTreeMap<(i64, u64), Event>,
    sequence: u64,
}

impl Queue {
    /// Queues `callback` under `id` at `time`, unless `id` already waits
    /// for that exact time.
    pub fn schedule(&mut self, id: &str, time: i64, callback: Callback) -> bool {
        if self
            .events
            .values()
            .any(|e| e.id == id && e.trigger_time == time)
        {
            return false;
        }
        self.sequence += 1;
        self.events.insert(
            (time, self.sequence),
            Event {
                id: id.to_owned(),
                trigger_time: time,
                callback,
            },
        );
        true
    }

    /// Drops every event queued under `id`, returning how many there were.
    pub fn remove(&mut self, id: &str) -> usize {
        let before = self.events.len();
        self.events.retain(|_, event| event.id != id);
        before - self.events.len()
    }

    /// Takes the events due at or before `now`, in order.
    pub fn take_due(&mut self, now: i64) -> Vec<Event> {
        let later = self.events.split_off(&(now.saturating_add(1), 0));
        std::mem::replace(&mut self.events, later)
            .into_values()
            .collect()
    }

    /// The ids with something queued.
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.events.values().map(|e| e.id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn to_nbt(&self) -> Compound {
        let events = self
            .events
            .values()
            .map(|event| {
                Tag::Compound(
                    Compound::new()
                        .with("trigger_time", Tag::Long(event.trigger_time))
                        .with("id", event.id.as_str())
                        .with("callback", event.callback.to_nbt()),
                )
            })
            .collect();
        Compound::new().with("events", Tag::List(events))
    }

    /// Reads a saved queue; events that cannot be read are dropped.
    pub fn from_nbt(data: &Compound) -> Self {
        let mut queue = Self::default();
        for event in data.get_list("events").unwrap_or_default() {
            let Some(event) = event.as_compound() else {
                continue;
            };
            let (Some(time), Some(id), Some(callback)) = (
                event.get_long("trigger_time"),
                event.get_str("id"),
                event.get_compound("callback").and_then(Callback::from_nbt),
            ) else {
                tuff_core::log::warn!("Dropping a scheduled event that cannot be read");
                continue;
            };
            queue.schedule(id, time, callback);
        }
        queue
    }
}

enum Message {
    #[cfg(test)]
    Panic,
    Schedule {
        id: String,
        time: i64,
        callback: Callback,
        replace: bool,
    },
    Remove {
        id: String,
        reply: oneshot::Sender<usize>,
    },
    Ids(oneshot::Sender<Vec<String>>),
    Due {
        now: i64,
        reply: oneshot::Sender<Vec<Event>>,
    },
    Flush(oneshot::Sender<()>),
}

/// Where the queue is read and changed.
#[derive(Clone)]
pub struct Scheduler {
    inbox: mpsc::Sender<Message>,
    /// The queued ids as of the last change, for completing `clear`
    /// without waiting on the actor.
    ids: watch::Receiver<Arc<[String]>>,
}

impl Scheduler {
    /// Starts the queue's actor, loading the saved queue first.
    pub fn start(shared: Arc<Shared>) -> Self {
        let (inbox, messages) = mpsc::channel(INBOX);
        let (ids_tx, ids) = watch::channel(Arc::from([]));
        tuff_supervise::spawn("the function schedule", run(shared, messages, ids_tx));
        Self { inbox, ids }
    }

    /// The queued ids as last published; never waits.
    pub fn known_ids(&self) -> Arc<[String]> {
        self.ids.borrow().clone()
    }

    /// Queues `callback` as `id` at game time `time`; with `replace`,
    /// first drops whatever `id` had queued.
    pub async fn schedule(&self, id: String, time: i64, callback: Callback, replace: bool) {
        let _ = self
            .inbox
            .send(Message::Schedule {
                id,
                time,
                callback,
                replace,
            })
            .await;
    }

    pub async fn remove(&self, id: String) -> usize {
        let (reply, answer) = oneshot::channel();
        if self
            .inbox
            .send(Message::Remove { id, reply })
            .await
            .is_err()
        {
            return 0;
        }
        answer.await.unwrap_or(0)
    }

    pub async fn ids(&self) -> Vec<String> {
        let (reply, answer) = oneshot::channel();
        if self.inbox.send(Message::Ids(reply)).await.is_err() {
            return Vec::new();
        }
        answer.await.unwrap_or_default()
    }

    /// Takes the events due by game time `now`.
    pub async fn take_due(&self, now: i64) -> Vec<Event> {
        let (reply, answer) = oneshot::channel();
        if self.inbox.send(Message::Due { now, reply }).await.is_err() {
            return Vec::new();
        }
        answer.await.unwrap_or_default()
    }

    /// Resolves once the queue as it is now is saved.
    pub async fn flush(&self) {
        let (done, saved) = oneshot::channel();
        if self.inbox.send(Message::Flush(done)).await.is_ok() {
            let _ = saved.await;
        }
    }
}

struct Actor {
    backend: Arc<dyn WorldBackend>,
    queue: Queue,
    ids: watch::Sender<Arc<[String]>>,
    dirty: bool,
    readable: bool,
    last_save: Option<JoinHandle<()>>,
}

impl Actor {
    fn save(&mut self) {
        if !self.dirty || !self.readable {
            return;
        }
        self.dirty = false;
        let data = saved(self.queue.to_nbt());
        let backend = self.backend.clone();
        let previous = self.last_save.take();
        self.last_save = Some(tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let written = tokio::task::spawn_blocking(move || {
                backend
                    .documents()
                    .write(Document::SavedData(SAVED_DATA), data)
            })
            .await;
            if let Ok(Err(err)) = written {
                tuff_core::log::error!("failed to save scheduled events: {err}");
            }
        }));
    }

    fn publish_ids(&self) {
        let ids = self.queue.ids();
        if self.ids.borrow()[..] != ids[..] {
            self.ids.send_replace(ids.into());
        }
    }

    fn handle(&mut self, message: Message) {
        self.apply(message);
        self.publish_ids();
    }

    fn apply(&mut self, message: Message) {
        match message {
            #[cfg(test)]
            Message::Panic => panic!("test panic"),
            Message::Schedule {
                id,
                time,
                callback,
                replace,
            } => {
                if replace {
                    self.queue.remove(&id);
                }
                self.queue.schedule(&id, time, callback);
                self.dirty = true;
            }
            Message::Remove { id, reply } => {
                let removed = self.queue.remove(&id);
                self.dirty |= removed > 0;
                let _ = reply.send(removed);
            }
            Message::Ids(reply) => {
                let _ = reply.send(self.queue.ids());
            }
            Message::Due { now, reply } => {
                let due = self.queue.take_due(now);
                self.dirty |= !due.is_empty();
                let _ = reply.send(due);
            }
            Message::Flush(done) => {
                self.save();
                match self.last_save.take() {
                    Some(pending) => {
                        self.last_save = Some(tokio::spawn(async move {
                            let _ = pending.await;
                            let _ = done.send(());
                        }));
                    }
                    None => {
                        let _ = done.send(());
                    }
                }
            }
        }
    }
}

async fn run(
    shared: Arc<Shared>,
    messages: mpsc::Receiver<Message>,
    ids: watch::Sender<Arc<[String]>>,
) {
    let actor = load(&shared, ids).await;
    tuff_supervise::restarting(
        "the function schedule",
        (actor, messages),
        |(actor, messages)| Box::pin(serve(actor, shared.clone(), messages)),
    )
    .await;
}

async fn load(shared: &Arc<Shared>, ids: watch::Sender<Arc<[String]>>) -> Actor {
    let backend = shared.backend.clone();
    let loaded = {
        let backend = backend.clone();
        tokio::task::spawn_blocking(move || {
            backend.documents().read(Document::SavedData(SAVED_DATA))
        })
        .await
    };
    let (queue, readable) = match loaded {
        Ok(Ok(root)) => (Queue::from_nbt(&inner(root)), true),
        Ok(Err(err)) => {
            tuff_core::log::warn!("failed to read scheduled events: {err}");
            (Queue::default(), false)
        }
        Err(_) => (Queue::default(), false),
    };
    let actor = Actor {
        backend,
        queue,
        ids,
        dirty: false,
        readable,
        last_save: None,
    };
    actor.publish_ids();
    actor
}

async fn serve(actor: &mut Actor, shared: Arc<Shared>, messages: &mut mpsc::Receiver<Message>) {
    let mut clock = shared.clock.clone();
    let mut last_autosave = *clock.borrow();
    loop {
        tokio::select! {
            message = messages.recv() => {
                let Some(message) = message else { break };
                actor.handle(message);
            }
            changed = clock.changed() => {
                if changed.is_err() {
                    break;
                }
                let now = *clock.borrow_and_update();
                if now >= last_autosave + AUTOSAVE_TICKS && shared.saves().autosaves() {
                    last_autosave = now;
                    actor.save();
                }
            }
        }
    }
    while let Ok(message) = messages.try_recv() {
        actor.handle(message);
    }
    actor.save();
    if let Some(last) = actor.last_save.take() {
        let _ = last.await;
    }
}

fn scheduler(ctx: &Context<'_>) -> Result<Scheduler, CommandError> {
    ctx.shared()
        .services
        .get::<FunctionTicker>()
        .and_then(FunctionTicker::scheduler)
        .ok_or_else(|| CommandError::simple("commands.reload.failure"))
}

/// `/schedule function <function> <time> [append|replace]` and
/// `/schedule clear <id>`.
pub struct ScheduleCommand;

impl ScheduleCommand {
    async fn schedule(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
        let (name, tag) = match ctx.arg("function") {
            Some(Value::ResourceOrTag { name, tag }) => (name.clone(), *tag),
            _ => return Err(CommandError::simple("command.unknown.argument")),
        };
        let delay = match ctx.arg("time") {
            Some(Value::Time(ticks)) => *ticks,
            _ => return Err(CommandError::simple("command.unknown.argument")),
        };
        let contents = ctx.shared().datapacks.contents();
        let functions = resolve(&contents, &name, tag)?;
        if delay == 0 {
            return Err(CommandError::simple("commands.schedule.same_tick"));
        }
        let now = *ctx.shared().clock.borrow() as i64;
        let time = now + i64::from(delay);
        let replace = !ctx.has("append");
        let scheduler = scheduler(ctx)?;
        let (id, callback, key) = if tag {
            (
                format!("#{name}"),
                Callback::Tag(name.clone()),
                "commands.schedule.created.tag",
            )
        } else {
            if functions.first().is_some_and(|f| f.is_macro()) {
                return Err(CommandError::simple("commands.schedule.macro"));
            }
            (
                name.clone(),
                Callback::Function(name.clone()),
                "commands.schedule.created.function",
            )
        };
        scheduler.schedule(id, time, callback, replace).await;
        ctx.success(
            TextComponent::translate(key, [text(&name), text(delay), text(time)]),
            true,
        )
        .await;
        Ok(time.rem_euclid(i64::from(i32::MAX)) as i32)
    }

    async fn clear(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
        let id = match ctx.arg("function") {
            Some(Value::Identifier(id) | Value::Resource(id)) => id.clone(),
            _ => return Err(CommandError::simple("command.unknown.argument")),
        };
        let removed = scheduler(ctx)?.remove(id.clone()).await;
        if removed == 0 {
            return Err(CommandError::dynamic(
                "commands.schedule.cleared.failure",
                [id],
            ));
        }
        ctx.success(
            TextComponent::translate(
                "commands.schedule.cleared.success",
                [text(removed), text(&id)],
            ),
            true,
        )
        .await;
        Ok(removed as i32)
    }
}

impl Executor for ScheduleCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            if ctx.has("clear") {
                Self::clear(ctx).await
            } else {
                Self::schedule(ctx).await
            }
        })
    }

    fn suggest(
        &self,
        path: &[ParsedNode],
        partial: &str,
        input: &super::SuggestInput,
    ) -> Option<Vec<String>> {
        if path.iter().any(|node| node.node.name == "clear") {
            let ids = input
                .services
                .get::<FunctionTicker>()
                .and_then(FunctionTicker::scheduler)?
                .known_ids();
            return Some(
                ids.iter()
                    .filter(|id| id.starts_with(partial))
                    .cloned()
                    .collect(),
            );
        }
        Some(suggest_functions(&input.datapacks.contents(), partial))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn function(id: &str) -> Callback {
        Callback::Function(id.to_owned())
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panic_restarts_the_queue_keeping_its_events() {
        let (_clock, clock_rx) = watch::channel(0u64);
        let mut shared = tuff_test_support::bare_shared();
        shared.clock = clock_rx;
        let scheduler = Scheduler::start(Arc::new(shared));
        scheduler
            .schedule("a:f".to_owned(), 40, function("a:f"), false)
            .await;
        scheduler.inbox.send(Message::Panic).await.unwrap();
        assert_eq!(scheduler.ids().await, ["a:f"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn events_come_due_by_time_then_by_queue_order() {
        let mut queue = Queue::default();
        assert!(queue.schedule("a:late", 30, function("a:late")));
        assert!(queue.schedule("a:second", 20, function("a:second")));
        assert!(queue.schedule("#a:first", 20, Callback::Tag("a:first".into())));
        assert!(queue.schedule("a:second", 10, function("a:second")));
        assert!(!queue.schedule("a:second", 10, function("a:second")));
        assert_eq!(queue.ids(), ["#a:first", "a:late", "a:second"]);
        assert!(queue.take_due(9).is_empty());
        let due: Vec<String> = queue.take_due(20).into_iter().map(|e| e.id).collect();
        assert_eq!(due, ["a:second", "a:second", "#a:first"]);
        assert_eq!(queue.remove("a:late"), 1);
        assert_eq!(queue.remove("a:late"), 0);
        assert!(queue.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_queue_saves_in_the_game_format() {
        let mut queue = Queue::default();
        queue.schedule("a:f", 40, function("a:f"));
        queue.schedule("#a:t", 12, Callback::Tag("a:t".into()));
        let nbt = queue.to_nbt();
        let events = nbt.get_list("events").unwrap();
        let first = events[0].as_compound().unwrap();
        assert_eq!(first.get_long("trigger_time"), Some(12));
        assert_eq!(first.get_str("id"), Some("#a:t"));
        let callback = first.get_compound("callback").unwrap();
        assert_eq!(callback.get_str("type"), Some("minecraft:function_tag"));
        assert_eq!(callback.get_str("id"), Some("a:t"));

        let mut read = Queue::from_nbt(&nbt);
        let due = read.take_due(100);
        assert_eq!(due.len(), 2);
        assert_eq!(due[1].callback, function("a:f"));
        let broken = Compound::new().with(
            "events",
            Tag::List(vec![Tag::Compound(Compound::new().with("id", "x"))]),
        );
        assert!(Queue::from_nbt(&broken).is_empty());
    }
}
