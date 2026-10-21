//! The web panel's event bus: topics fanned out to server-sent event
//! streams, with a short history per topic so a reconnecting browser
//! (`Last-Event-ID`) misses nothing, and a `resync` event instead of
//! unbounded buffering for a subscriber that falls behind.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};

use tokio::sync::{broadcast, mpsc, oneshot};

use super::auth::{AuthContext, Scope};
use tuff_http::Event;

const HISTORY: usize = 256;
const BROADCAST: usize = 1024;
const QUEUE: usize = 1024;
pub const MAX_PER_SESSION: usize = 4;
pub const MAX_TOTAL: usize = 32;

/// Each metrics snapshot, as event `snapshot`.
pub const METRICS: &str = "metrics";
/// Anti-cheat operator alerts, as event `alert`.
pub const ALERTS: &str = "alerts";
/// Online players: events `join` and `update` (a player row) and `leave`
/// (`{uuid, name}`).
pub const PLAYERS: &str = "players";

/// The scope a topic needs; `None` for an unknown topic.
pub fn topic_scope(topic: &str) -> Option<Scope> {
    match topic {
        METRICS => Some(Scope::MetricsRead),
        "logs" | ALERTS => Some(Scope::LogsRead),
        PLAYERS => Some(Scope::PlayersRead),
        "audit" => Some(Scope::PanelAdmin),
        super::backups::TOPIC => Some(Scope::BackupsRead),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct Published {
    pub topic: Arc<str>,
    pub event: Event,
    pub seq: u64,
}

pub struct Subscription {
    /// Missed events since the `Last-Event-ID`, oldest first.
    pub backfill: Vec<Published>,
    /// The history no longer reaches back to the given id.
    pub resync: bool,
    pub live: broadcast::Receiver<Arc<Published>>,
    /// Counts against the stream caps while held.
    pub token: Arc<()>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SubscribeError {
    TooManyForSession,
    TooMany,
}

enum Request {
    #[cfg(test)]
    Panic,
    Publish {
        topic: String,
        name: String,
        data: Arc<str>,
    },
    Subscribe {
        topics: Vec<String>,
        session: String,
        after: Option<u64>,
        reply: oneshot::Sender<Result<Subscription, SubscribeError>>,
    },
    Recent {
        topic: String,
        limit: usize,
        reply: oneshot::Sender<Vec<Arc<Published>>>,
    },
}

/// The event bus handle.
#[derive(Clone)]
pub struct PanelEvents {
    tx: mpsc::Sender<Request>,
}

impl PanelEvents {
    pub fn spawn() -> PanelEvents {
        let (tx, rx) = mpsc::channel(QUEUE);
        tuff_supervise::spawn("the panel event bus", restarting(rx));
        PanelEvents { tx }
    }

    pub fn detached() -> PanelEvents {
        PanelEvents {
            tx: mpsc::channel(1).0,
        }
    }

    /// Publishes `data` (JSON text) as event `name` on `topic`; dropped
    /// when the bus is full.
    pub fn publish(&self, topic: &str, name: &str, data: impl Into<Arc<str>>) -> bool {
        self.tx
            .try_send(Request::Publish {
                topic: topic.to_owned(),
                name: name.to_owned(),
                data: data.into(),
            })
            .is_ok()
    }

    /// Publishes an operator alert line on [`ALERTS`].
    pub fn publish_alert(&self, text: &str) -> bool {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let data = serde_json::json!({ "ts": ts, "text": text });
        self.publish(ALERTS, "alert", data.to_string())
    }

    /// The newest `limit` events still in `topic`'s history, oldest first.
    pub async fn recent(&self, topic: &str, limit: usize) -> Vec<Arc<Published>> {
        let (reply, rx) = oneshot::channel();
        let request = Request::Recent {
            topic: topic.to_owned(),
            limit,
            reply,
        };
        if self.tx.send(request).await.is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub async fn subscribe(
        &self,
        topics: Vec<String>,
        session: &str,
        after: Option<u64>,
    ) -> Result<Subscription, SubscribeError> {
        let (reply, rx) = oneshot::channel();
        let request = Request::Subscribe {
            topics,
            session: session.to_owned(),
            after,
            reply,
        };
        if self.tx.send(request).await.is_err() {
            return Err(SubscribeError::TooMany);
        }
        rx.await.unwrap_or(Err(SubscribeError::TooMany))
    }

    /// An SSE stream of `topics` the context may read, resuming after
    /// `after`.
    pub async fn stream(
        &self,
        ctx: &AuthContext,
        topics: &str,
        after: Option<u64>,
    ) -> Result<mpsc::Receiver<Event>, SubscribeError> {
        self.stream_with(ctx, topics, after, |_| false).await
    }

    /// [`stream`](Self::stream), also admitting the topics `extra` allows
    /// (plugin page topics).
    pub async fn stream_with(
        &self,
        ctx: &AuthContext,
        topics: &str,
        after: Option<u64>,
        extra: impl Fn(&str) -> bool,
    ) -> Result<mpsc::Receiver<Event>, SubscribeError> {
        let topics: Vec<String> = topics
            .split(',')
            .map(str::trim)
            .filter(|t| topic_scope(t).is_some_and(|s| ctx.allows(s)) || extra(t))
            .map(str::to_owned)
            .take(8)
            .collect();
        let sub = self.subscribe(topics.clone(), &ctx.session, after).await?;
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(forward(sub, topics, tx));
        Ok(rx)
    }
}

fn wire(p: &Published) -> Event {
    let mut event = p.event.clone();
    event.id = Some(p.seq.to_string());
    event
}

async fn forward(sub: Subscription, topics: Vec<String>, tx: mpsc::Sender<Event>) {
    let Subscription {
        backfill,
        resync,
        mut live,
        token,
    } = sub;
    let _token = token;
    if resync && tx.send(Event::new("resync", "{}")).await.is_err() {
        return;
    }
    let mut last = 0;
    for p in &backfill {
        last = p.seq;
        if tx.send(wire(p)).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            got = live.recv() => match got {
                Ok(p) => {
                    if p.seq <= last || !topics.iter().any(|t| **t == *p.topic) {
                        continue;
                    }
                    last = p.seq;
                    if tx.send(wire(&p)).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if tx.send(Event::new("resync", "{}")).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            _ = tx.closed() => return,
        }
    }
}

async fn restarting(mut rx: mpsc::Receiver<Request>) {
    use tuff_supervise::{CatchUnwind, count_panic, panic_message};
    while let Err(panic) = CatchUnwind::new(Box::pin(run(&mut rx))).await {
        count_panic();
        log::error!(
            "the panel event bus panicked: {}; restarting it with empty history",
            panic_message(&*panic)
        );
    }
}

async fn run(rx: &mut mpsc::Receiver<Request>) {
    let (live, _) = broadcast::channel(BROADCAST);
    let mut history: HashMap<String, VecDeque<Arc<Published>>> = HashMap::new();
    let mut streams: HashMap<String, Vec<Weak<()>>> = HashMap::new();
    let mut seq = 0u64;
    while let Some(request) = rx.recv().await {
        match request {
            #[cfg(test)]
            Request::Panic => panic!("test panic"),
            Request::Publish { topic, name, data } => {
                seq += 1;
                let published = Arc::new(Published {
                    topic: Arc::from(topic.as_str()),
                    event: Event {
                        id: None,
                        event: Some(name),
                        data,
                    },
                    seq,
                });
                let ring = history.entry(topic).or_default();
                if ring.len() >= HISTORY {
                    ring.pop_front();
                }
                ring.push_back(published.clone());
                let _ = live.send(published);
            }
            Request::Recent {
                topic,
                limit,
                reply,
            } => {
                let recent = history.get(&topic).map_or_else(Vec::new, |ring| {
                    let skip = ring.len().saturating_sub(limit);
                    ring.iter().skip(skip).cloned().collect()
                });
                let _ = reply.send(recent);
            }
            Request::Subscribe {
                topics,
                session,
                after,
                reply,
            } => {
                streams.retain(|_, tokens| {
                    tokens.retain(|t| t.strong_count() > 0);
                    !tokens.is_empty()
                });
                let total: usize = streams.values().map(Vec::len).sum();
                let mine = streams.get(&session).map_or(0, Vec::len);
                if mine >= MAX_PER_SESSION {
                    let _ = reply.send(Err(SubscribeError::TooManyForSession));
                    continue;
                }
                if total >= MAX_TOTAL {
                    let _ = reply.send(Err(SubscribeError::TooMany));
                    continue;
                }
                let mut backfill = Vec::new();
                let mut resync = false;
                if let Some(after) = after {
                    for topic in &topics {
                        let Some(ring) = history.get(topic) else {
                            continue;
                        };
                        if ring.front().is_some_and(|first| first.seq > after + 1) {
                            resync = true;
                        }
                        backfill
                            .extend(ring.iter().filter(|p| p.seq > after).map(|p| (**p).clone()));
                    }
                    backfill.sort_by_key(|p| p.seq);
                }
                let token = Arc::new(());
                streams
                    .entry(session)
                    .or_default()
                    .push(Arc::downgrade(&token));
                let _ = reply.send(Ok(Subscription {
                    backfill,
                    resync,
                    live: live.subscribe(),
                    token,
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Principal;
    use tuff_storage::admin::PermissionLevel;

    fn ctx(level: PermissionLevel, session: &str) -> AuthContext {
        AuthContext {
            principal: Principal::Operator {
                uuid: 1,
                name: "op".into(),
            },
            level,
            session: session.into(),
            csrf: String::new(),
            permissions: std::sync::Arc::new(crate::auth::LevelDefaults),
        }
    }

    async fn sync(bus: &PanelEvents) {
        drop(bus.subscribe(Vec::new(), "sync", None).await);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn bus_panic_restarts_with_empty_history() {
        let bus = PanelEvents::spawn();
        assert!(bus.publish("t", "n", "{}"));
        assert_eq!(bus.recent("t", 10).await.len(), 1);
        bus.tx.send(Request::Panic).await.unwrap();
        assert!(bus.recent("t", 10).await.is_empty());
        assert!(bus.publish("t", "n", "{}"));
        assert_eq!(bus.recent("t", 10).await.len(), 1);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn streams_filter_by_scope_and_backfill_after_an_id() {
        let bus = PanelEvents::spawn();
        let mut mod_rx = bus
            .stream(
                &ctx(PermissionLevel::Gamemasters, "m"),
                "logs,audit,bogus",
                None,
            )
            .await
            .unwrap();
        let mut owner_rx = bus
            .stream(&ctx(PermissionLevel::Owners, "o"), "logs,audit", None)
            .await
            .unwrap();
        bus.publish("audit", "audit", "{\"a\":1}");
        bus.publish("logs", "log", "{\"l\":1}");
        let first = mod_rx.recv().await.unwrap();
        assert_eq!(first.event.as_deref(), Some("log"));
        assert_eq!(first.id.as_deref(), Some("2"));
        assert_eq!(
            owner_rx.recv().await.unwrap().event.as_deref(),
            Some("audit")
        );
        assert_eq!(owner_rx.recv().await.unwrap().event.as_deref(), Some("log"));

        let mut resumed = bus
            .stream(&ctx(PermissionLevel::Owners, "o"), "logs,audit", Some(1))
            .await
            .unwrap();
        let again = resumed.recv().await.unwrap();
        assert_eq!(again.id.as_deref(), Some("2"));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn metrics_and_alerts_topics_follow_scopes_and_keep_recent_alerts() {
        let bus = PanelEvents::spawn();
        let mut admin = bus
            .stream(&ctx(PermissionLevel::Admins, "a"), "metrics,alerts", None)
            .await
            .unwrap();
        let mut owner = bus
            .stream(&ctx(PermissionLevel::Owners, "o"), "metrics,alerts", None)
            .await
            .unwrap();
        bus.publish(METRICS, "snapshot", "{}");
        for i in 0..3 {
            bus.publish_alert(&format!("alert {i}"));
        }
        assert_eq!(
            owner.recv().await.unwrap().event.as_deref(),
            Some("snapshot")
        );
        assert_eq!(owner.recv().await.unwrap().event.as_deref(), Some("alert"));
        let first = admin.recv().await.unwrap();
        assert_eq!(first.event.as_deref(), Some("alert"));
        let alert: serde_json::Value = serde_json::from_str(&first.data).unwrap();
        assert_eq!(alert["text"], "alert 0");
        assert!(alert["ts"].as_u64().unwrap() > 0);
        let recent = bus.recent(ALERTS, 2).await;
        let texts: Vec<String> = recent
            .iter()
            .map(|p| {
                serde_json::from_str::<serde_json::Value>(&p.event.data).unwrap()["text"]
                    .to_string()
            })
            .collect();
        assert_eq!(texts, ["\"alert 1\"", "\"alert 2\""]);
        assert!(bus.recent("nothing", 5).await.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn players_and_jobs_topics_need_their_scopes() {
        assert_eq!(topic_scope(PLAYERS), Some(Scope::PlayersRead));
        assert_eq!(topic_scope(crate::backups::TOPIC), Some(Scope::BackupsRead));
        let allowed = |level| {
            let ctx = ctx(level, "x");
            (
                ctx.allows(Scope::PlayersRead),
                ctx.allows(Scope::PlayersModerate),
                ctx.allows(Scope::BackupsRead),
            )
        };
        assert_eq!(allowed(PermissionLevel::Moderators), (false, false, false));
        assert_eq!(allowed(PermissionLevel::Gamemasters), (true, true, false));
        assert_eq!(allowed(PermissionLevel::Owners), (true, true, true));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn players_events_reach_only_readers() {
        let bus = PanelEvents::spawn();
        let reader = ctx(PermissionLevel::Gamemasters, "r");
        let other = ctx(PermissionLevel::Moderators, "o");
        let mut readers = bus.stream(&reader, "players,logs", None).await.unwrap();
        let mut others = bus.stream(&other, "players,logs", None).await.unwrap();
        bus.publish(PLAYERS, "join", "{\"name\":\"Alex\"}");
        bus.publish("logs", "log", "{}");
        assert_eq!(readers.recv().await.unwrap().event.as_deref(), Some("join"));
        assert_eq!(readers.recv().await.unwrap().event.as_deref(), Some("log"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), others.recv())
                .await
                .is_err()
        );
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn old_ids_and_lag_resync_and_caps_hold() {
        let bus = PanelEvents::spawn();
        for i in 0..(HISTORY + 10) {
            bus.publish("logs", "log", i.to_string());
        }
        sync(&bus).await;
        let mut stale = bus
            .stream(&ctx(PermissionLevel::Owners, "a"), "logs", Some(1))
            .await
            .unwrap();
        assert_eq!(stale.recv().await.unwrap().event.as_deref(), Some("resync"));

        let mut slow = bus
            .stream(&ctx(PermissionLevel::Owners, "b"), "logs", None)
            .await
            .unwrap();
        for i in 0..(BROADCAST + 200) {
            while !bus.publish("logs", "log", i.to_string()) {
                tokio::task::yield_now().await;
            }
        }
        sync(&bus).await;
        let mut saw_resync = false;
        for _ in 0..200 {
            match slow.recv().await {
                Some(e) if e.event.as_deref() == Some("resync") => {
                    saw_resync = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw_resync);

        let mut held = Vec::new();
        for _ in 0..MAX_PER_SESSION {
            held.push(bus.subscribe(vec!["logs".into()], "c", None).await.unwrap());
        }
        assert_eq!(
            bus.subscribe(vec!["logs".into()], "c", None).await.err(),
            Some(SubscribeError::TooManyForSession)
        );
        held.pop();
        assert!(bus.subscribe(vec!["logs".into()], "c", None).await.is_ok());
        let mut many = Vec::new();
        for i in 0..MAX_TOTAL {
            match bus.subscribe(vec![], &format!("s{i}"), None).await {
                Ok(s) => many.push(s),
                Err(e) => {
                    assert_eq!(e, SubscribeError::TooMany);
                    return;
                }
            }
        }
        panic!("no global cap");
    }
}
