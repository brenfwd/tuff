//! Resource packs features register: hosted over the HTTP server and offered
//! to clients in the configuration phase or on demand.
//!
//! One actor owns the pack table. Packs build (read, generate, hash) on
//! rayon; requests for a pack still building wait on its build. Per-player
//! state stays in the session; the actor only keeps each online player's
//! event handle so a feature can push or pop a pack for one player or all.

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;

use bytes::Bytes;
use sha1::{Digest, Sha1};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::protocol::pack::PackStatus;
use crate::world::messages::PlayerHandle;
use tuff_http::{Request, Response};
use tuff_item::text::TextComponent;

pub const ROUTE_PREFIX: &str = "/resourcepacks/";
pub const ENABLED_KEY: &str = "tuff.resourcePacks.enabled";
pub const MAX_PACK_BYTES: usize = 256 * 1024 * 1024;

pub type Generator = Arc<dyn Fn() -> io::Result<Bytes> + Send + Sync>;

#[derive(Clone)]
pub enum PackSource {
    Bytes(Bytes),
    File(PathBuf),
    /// Built lazily on a rayon thread; rerun on `update`.
    Generated(Generator),
}

impl PackSource {
    fn load(&self) -> io::Result<Bytes> {
        let bytes = match self {
            Self::Bytes(bytes) => bytes.clone(),
            Self::File(path) => {
                let len = std::fs::metadata(path)?.len();
                if len > MAX_PACK_BYTES as u64 {
                    return Err(io::Error::other("resource pack is too large"));
                }
                Bytes::from(std::fs::read(path)?)
            }
            Self::Generated(generate) => generate()?,
        };
        if bytes.len() > MAX_PACK_BYTES {
            return Err(io::Error::other("resource pack is too large"));
        }
        Ok(bytes)
    }
}

#[derive(Clone)]
pub struct ResourcePack {
    pub id: u128,
    pub name: String,
    pub prompt: Option<TextComponent>,
    pub required: bool,
    /// Offered to every client in the configuration phase.
    pub offer_on_join: bool,
    /// Offered even while [`ENABLED_KEY`] is off: the server cannot be
    /// played without it.
    pub essential: bool,
    /// Where the pack sits among the join offers: ascending, so a higher
    /// order is offered later and stacks above the others on the client.
    pub order: i32,
    pub source: PackSource,
}

/// The order of the custom content pack: offered after every other pack
/// at join, so no other pack can paint over custom content.
pub const CUSTOM_CONTENT_ORDER: i32 = i32::MAX;
/// The orders a pack the server does not own may take.
pub const ORDERS: std::ops::RangeInclusive<i32> = -1000..=1000;

/// What may change about a hosted pack after it is registered.
#[derive(Debug, Clone, PartialEq)]
pub struct PackSettings {
    pub required: bool,
    pub prompt: Option<TextComponent>,
    pub offer_on_join: bool,
    pub order: i32,
}

/// How far a hosted pack's build has got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildState {
    Building,
    Ready { sha1: String, size: usize },
    Failed,
}

/// A hosted pack as the actor sees it, for listings.
#[derive(Debug, Clone)]
pub struct HostedPack {
    pub id: u128,
    pub name: String,
    pub settings: PackSettings,
    pub essential: bool,
    pub state: BuildState,
    /// Online players it was pushed to and who have not declined it.
    pub holders: usize,
}

impl ResourcePack {
    pub fn new(id: u128, name: &str, source: PackSource) -> Self {
        Self {
            id,
            name: name.to_owned(),
            prompt: None,
            required: false,
            offer_on_join: true,
            essential: false,
            order: 0,
            source,
        }
    }

    pub fn order(mut self, order: i32) -> Self {
        self.order = order;
        self
    }

    pub fn settings(&self) -> PackSettings {
        PackSettings {
            required: self.required,
            prompt: self.prompt.clone(),
            offer_on_join: self.offer_on_join,
            order: self.order,
        }
    }

    pub fn required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    pub fn prompt(mut self, prompt: TextComponent) -> Self {
        self.prompt = Some(prompt);
        self
    }

    pub fn offer_on_join(mut self, on_join: bool) -> Self {
        self.offer_on_join = on_join;
        self
    }

    pub fn essential(mut self, essential: bool) -> Self {
        self.essential = essential;
        self
    }
}

/// Everything a Push packet carries.
#[derive(Debug, Clone, PartialEq)]
pub struct PackOffer {
    pub id: u128,
    pub url: String,
    pub sha1: String,
    pub required: bool,
    pub prompt: Option<TextComponent>,
}

/// A status a client reported, for features.
#[derive(Debug, Clone, PartialEq)]
pub struct PackEvent {
    pub player: u128,
    pub pack: u128,
    pub status: PackStatus,
}

/// What one client reported for one pack it was offered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackState {
    /// The SHA-1 of the version last offered.
    pub hash: String,
    /// The latest status for that version, `None` until the client answers.
    pub status: Option<PackStatus>,
    /// Whether some version of the pack is applied on the client: set by a
    /// load, kept across re-pushes, cleared by a failure.
    pub loaded: bool,
}

impl PackState {
    /// A new version was offered.
    pub fn offered(&mut self, hash: &str) {
        hash.clone_into(&mut self.hash);
        self.status = None;
    }

    pub fn apply(&mut self, status: PackStatus) {
        self.status = Some(status);
        if status == PackStatus::Loaded {
            self.loaded = true;
        } else if status.is_failure() {
            self.loaded = false;
        }
    }
}

/// How long configuration-phase statuses wait for the player's session.
const JOIN_STATE_TTL: std::time::Duration = std::time::Duration::from_secs(600);

pub struct Built {
    pub bytes: Bytes,
    pub sha1: String,
}

fn build(source: &PackSource) -> io::Result<Arc<Built>> {
    let bytes = source.load()?;
    let digest = Sha1::digest(&bytes);
    let mut sha1 = String::with_capacity(40);
    for byte in digest {
        sha1.push_str(&format!("{byte:02x}"));
    }
    Ok(Arc::new(Built { bytes, sha1 }))
}

pub fn format_uuid(id: u128) -> String {
    let h = format!("{id:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

pub fn parse_uuid(text: &str) -> Option<u128> {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok()
}

pub fn is_unreachable_base(base: &str) -> bool {
    let host = base
        .split_once("://")
        .map_or(base, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or("");
    let host = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => host.rsplit_once(':').map_or(host, |(h, _)| h),
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

enum Waiter {
    Fetch(oneshot::Sender<Option<Arc<Built>>>),
    Offer(oneshot::Sender<Option<PackOffer>>),
    Push(Option<u128>),
}

enum State {
    Building(Vec<Waiter>),
    Ready(Arc<Built>),
    Failed,
}

struct Entry {
    pack: ResourcePack,
    state: State,
    generation: u64,
    warned: bool,
}

enum Message {
    #[cfg(test)]
    Panic,
    Register(ResourcePack, oneshot::Sender<()>),
    Remove(u128, oneshot::Sender<bool>),
    Update(u128, PackSource, oneshot::Sender<bool>),
    Built {
        id: u128,
        generation: u64,
        result: io::Result<Arc<Built>>,
    },
    Fetch(u128, oneshot::Sender<Option<Arc<Built>>>),
    Offer(u128, oneshot::Sender<Option<PackOffer>>),
    JoinPacks(oneshot::Sender<Vec<u128>>),
    PlayerJoined(
        u128,
        PlayerHandle,
        oneshot::Sender<HashMap<u128, PackState>>,
    ),
    Configure(u128, bool, Option<TextComponent>),
    Settings(u128, Option<String>, PackSettings, oneshot::Sender<bool>),
    List(oneshot::Sender<Vec<HostedPack>>),
    PlayerStates(oneshot::Sender<HashMap<u128, HashMap<u128, PackState>>>),
    Status(PackEvent),
    JoinStatus(PackEvent, String),
    PlayerLeft(u128),
    Push(Option<u128>, u128),
    Pop(Option<u128>, Option<u128>),
}

#[derive(Clone)]
pub struct ResourcePacks {
    tx: mpsc::Sender<Message>,
    events: broadcast::Sender<PackEvent>,
}

impl std::fmt::Debug for ResourcePacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResourcePacks")
    }
}

impl ResourcePacks {
    pub fn detached() -> Self {
        let (tx, _) = mpsc::channel(1);
        Self {
            tx,
            events: broadcast::channel(1).0,
        }
    }

    pub fn spawn(base_url: crate::public_ip::BaseUrl, config: crate::config::ConfigStore) -> Self {
        let (tx, rx) = mpsc::channel(256);
        let (events, _) = broadcast::channel(256);
        let actor = Actor {
            rx,
            me: tx.clone(),
            base_url,
            config,
            entries: HashMap::new(),
            players: HashMap::new(),
            holders: HashMap::new(),
            join_states: HashMap::new(),
            states: HashMap::new(),
            next_generation: 0,
        };
        tuff_supervise::spawn(
            "the resource pack server",
            tuff_supervise::restarting("the resource pack server", actor, |actor| {
                Box::pin(actor.run())
            }),
        );
        Self { tx, events }
    }

    /// Adds a pack, replacing one with the same id. Offered to online
    /// players only through `push`.
    pub async fn register(&self, pack: ResourcePack) {
        let (reply, done) = oneshot::channel();
        if self.tx.send(Message::Register(pack, reply)).await.is_ok() {
            let _ = done.await;
        }
    }

    /// Unhosts a pack and removes it from every client holding it.
    pub async fn remove(&self, id: u128) -> bool {
        let (reply, done) = oneshot::channel();
        self.tx.send(Message::Remove(id, reply)).await.is_ok() && done.await.unwrap_or(false)
    }

    /// Replaces a pack's content, rehashes it and resends it to every
    /// client holding it.
    pub async fn update(&self, id: u128, source: PackSource) -> bool {
        let (reply, done) = oneshot::channel();
        self.tx
            .send(Message::Update(id, source, reply))
            .await
            .is_ok()
            && done.await.unwrap_or(false)
    }

    /// Offers a pack to one player, or everyone online.
    pub async fn push(&self, player: Option<u128>, id: u128) {
        let _ = self.tx.send(Message::Push(player, id)).await;
    }

    #[cfg(test)]
    async fn panic_for_test(&self) {
        let _ = self.tx.send(Message::Panic).await;
    }

    pub async fn pop(&self, player: Option<u128>, id: Option<u128>) {
        let _ = self.tx.send(Message::Pop(player, id)).await;
    }

    pub async fn fetch(&self, id: u128) -> Option<Arc<Built>> {
        let (reply, done) = oneshot::channel();
        self.tx.send(Message::Fetch(id, reply)).await.ok()?;
        done.await.ok()?
    }

    pub async fn offer(&self, id: u128) -> Option<PackOffer> {
        let (reply, done) = oneshot::channel();
        self.tx.send(Message::Offer(id, reply)).await.ok()?;
        done.await.ok()?
    }

    /// The offers every client gets in the configuration phase.
    pub async fn join_offers(&self) -> Vec<PackOffer> {
        let (reply, done) = oneshot::channel();
        if self.tx.send(Message::JoinPacks(reply)).await.is_err() {
            return Vec::new();
        }
        let mut offers = Vec::new();
        for id in done.await.unwrap_or_default() {
            if let Some(offer) = self.offer(id).await {
                offers.push(offer);
            }
        }
        offers
    }

    /// Tracks a player in play. Returns what the player reported for the
    /// packs offered to it in the configuration phase.
    pub async fn player_joined(
        &self,
        player: u128,
        handle: PlayerHandle,
    ) -> HashMap<u128, PackState> {
        let (reply, done) = oneshot::channel();
        if self
            .tx
            .send(Message::PlayerJoined(player, handle, reply))
            .await
            .is_err()
        {
            return HashMap::new();
        }
        done.await.unwrap_or_default()
    }

    /// Changes whether a pack is required and its prompt, for offers made
    /// from now on.
    pub async fn configure(&self, id: u128, required: bool, prompt: Option<TextComponent>) {
        let _ = self.tx.send(Message::Configure(id, required, prompt)).await;
    }

    /// Replaces a pack's settings (and its name, when given) for offers
    /// made from now on; false when no such pack is hosted.
    pub async fn set_settings(
        &self,
        id: u128,
        name: Option<String>,
        settings: PackSettings,
    ) -> bool {
        let (reply, done) = oneshot::channel();
        self.tx
            .send(Message::Settings(id, name, settings, reply))
            .await
            .is_ok()
            && done.await.unwrap_or(false)
    }

    /// Every hosted pack, in join offer order.
    pub async fn list(&self) -> Vec<HostedPack> {
        let (reply, done) = oneshot::channel();
        if self.tx.send(Message::List(reply)).await.is_err() {
            return Vec::new();
        }
        done.await.unwrap_or_default()
    }

    /// What each online player last reported per pack offered to it, as
    /// this actor heard it (statuses that arrive while its queue is full
    /// are missed).
    pub async fn player_states(&self) -> HashMap<u128, HashMap<u128, PackState>> {
        let (reply, done) = oneshot::channel();
        if self.tx.send(Message::PlayerStates(reply)).await.is_err() {
            return HashMap::new();
        }
        done.await.unwrap_or_default()
    }

    /// Publishes a configuration-phase status, offered with `hash`, and
    /// keeps it for the player's session to pick up when it joins.
    pub async fn report_join(&self, event: PackEvent, hash: &str) {
        let _ = self.events.send(event.clone());
        let _ = self
            .tx
            .send(Message::JoinStatus(event, hash.to_owned()))
            .await;
    }

    pub async fn player_left(&self, player: u128) {
        let _ = self.tx.send(Message::PlayerLeft(player)).await;
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PackEvent> {
        self.events.subscribe()
    }

    pub fn report(&self, event: PackEvent) {
        let _ = self.tx.try_send(Message::Status(event.clone()));
        let _ = self.events.send(event);
    }

    /// The HTTP handler serving `<uuid>.zip` below [`ROUTE_PREFIX`].
    pub fn route(&self) -> tuff_http::Handler {
        let packs = self.clone();
        tuff_http::handler(move |req: Request| {
            let packs = packs.clone();
            async move {
                let id = req
                    .rest
                    .trim_start_matches('/')
                    .strip_suffix(".zip")
                    .and_then(parse_uuid);
                let Some(id) = id else {
                    return Response::new(hyper::StatusCode::NOT_FOUND, Bytes::new());
                };
                match packs.fetch(id).await {
                    Some(built) => {
                        Response::conditional(&req, "application/zip", built.bytes.clone())
                    }
                    None => Response::new(hyper::StatusCode::NOT_FOUND, Bytes::new()),
                }
            }
        })
    }
}

struct Actor {
    rx: mpsc::Receiver<Message>,
    me: mpsc::Sender<Message>,
    base_url: crate::public_ip::BaseUrl,
    config: crate::config::ConfigStore,
    entries: HashMap<u128, Entry>,
    players: HashMap<u128, PlayerHandle>,
    holders: HashMap<u128, HashSet<u128>>,
    join_states: HashMap<u128, (std::time::Instant, HashMap<u128, PackState>)>,
    /// Online players' statuses per pack, for listings.
    states: HashMap<u128, HashMap<u128, PackState>>,
    next_generation: u64,
}

fn join_order(entries: &HashMap<u128, Entry>, wanted: impl Fn(&ResourcePack) -> bool) -> Vec<u128> {
    let mut ids: Vec<(i32, &str, u128)> = entries
        .values()
        .filter(|e| wanted(&e.pack))
        .map(|e| (e.pack.order, e.pack.name.as_str(), e.pack.id))
        .collect();
    ids.sort();
    ids.into_iter().map(|(_, _, id)| id).collect()
}

impl Actor {
    async fn run(&mut self) {
        while let Some(message) = self.rx.recv().await {
            self.handle(message);
        }
    }

    fn enabled(&self) -> bool {
        self.config.get_bool(ENABLED_KEY).unwrap_or(true)
    }

    fn serves(&self, pack: &ResourcePack) -> bool {
        pack.essential || self.enabled()
    }

    fn start_build(&mut self, id: u128, waiters: Vec<Waiter>) {
        self.next_generation += 1;
        let generation = self.next_generation;
        let Some(entry) = self.entries.get_mut(&id) else {
            return;
        };
        entry.generation = generation;
        entry.state = State::Building(waiters);
        let source = entry.pack.source.clone();
        let me = self.me.clone();
        tokio::task::spawn_blocking(move || {
            let result = build(&source);
            let _ = me.blocking_send(Message::Built {
                id,
                generation,
                result,
            });
        });
    }

    fn make_offer(&mut self, id: u128, built: &Built) -> Option<PackOffer> {
        let base = self.base_url.current();
        let entry = self.entries.get_mut(&id)?;
        if !entry.warned && is_unreachable_base(&base) {
            entry.warned = true;
            log::warn!(
                "resource pack {} is offered at {base}, which clients cannot reach; \
                 set tuff.http.resourcePathBaseUrl or enable tuff.http.detectPublicIp",
                entry.pack.name
            );
        }
        Some(PackOffer {
            id,
            url: format!(
                "{}{ROUTE_PREFIX}{}.zip",
                base.trim_end_matches('/'),
                format_uuid(id)
            ),
            sha1: built.sha1.clone(),
            required: entry.pack.required,
            prompt: entry.pack.prompt.clone(),
        })
    }

    fn send_push(&mut self, id: u128, target: Option<u128>, built: &Built) {
        if !self.entries.get(&id).is_some_and(|e| self.serves(&e.pack)) {
            return;
        }
        let Some(offer) = self.make_offer(id, built) else {
            return;
        };
        let targets: Vec<u128> = match target {
            Some(player) => vec![player],
            None => self.players.keys().copied().collect(),
        };
        for player in targets {
            if let Some(handle) = self.players.get(&player) {
                handle.send(crate::world::messages::PlayerEvent::ResourcePack(
                    offer.clone(),
                ));
                self.holders.entry(id).or_default().insert(player);
                self.states
                    .entry(player)
                    .or_default()
                    .entry(id)
                    .or_default()
                    .offered(&offer.sha1);
            }
        }
    }

    fn handle(&mut self, message: Message) {
        match message {
            #[cfg(test)]
            Message::Panic => panic!("test panic"),
            Message::Register(pack, reply) => {
                let id = pack.id;
                self.entries.insert(
                    id,
                    Entry {
                        pack,
                        state: State::Failed,
                        generation: 0,
                        warned: false,
                    },
                );
                self.start_build(id, Vec::new());
                let _ = reply.send(());
            }
            Message::Remove(id, reply) => {
                let existed = self.entries.remove(&id).is_some();
                for states in self.states.values_mut() {
                    states.remove(&id);
                }
                if let Some(players) = self.holders.remove(&id) {
                    for player in players {
                        if let Some(handle) = self.players.get(&player) {
                            handle.send(crate::world::messages::PlayerEvent::ResourcePackPop(
                                Some(id),
                            ));
                        }
                    }
                }
                let _ = reply.send(existed);
            }
            Message::Update(id, source, reply) => {
                let Some(entry) = self.entries.get_mut(&id) else {
                    let _ = reply.send(false);
                    return;
                };
                entry.pack.source = source;
                let resend = self.holders.get(&id).is_some_and(|h| !h.is_empty());
                let mut waiters = Vec::new();
                if resend {
                    let holders: Vec<u128> = self.holders[&id].iter().copied().collect();
                    waiters.extend(holders.into_iter().map(|p| Waiter::Push(Some(p))));
                }
                self.start_build(id, waiters);
                let _ = reply.send(true);
            }
            Message::Built {
                id,
                generation,
                result,
            } => {
                let Some(entry) = self.entries.get_mut(&id) else {
                    return;
                };
                if entry.generation != generation {
                    return;
                }
                let State::Building(waiters) = std::mem::replace(&mut entry.state, State::Failed)
                else {
                    return;
                };
                let built = match result {
                    Ok(built) => {
                        entry.state = State::Ready(built.clone());
                        Some(built)
                    }
                    Err(err) => {
                        log::warn!("resource pack {} failed to build: {err}", entry.pack.name);
                        None
                    }
                };
                for waiter in waiters {
                    self.resolve(id, waiter, built.as_ref());
                }
            }
            Message::Fetch(id, reply) => self.wait_or_resolve(id, Waiter::Fetch(reply)),
            Message::Offer(id, reply) => self.wait_or_resolve(id, Waiter::Offer(reply)),
            Message::JoinPacks(reply) => {
                let enabled = self.enabled();
                let ids = join_order(&self.entries, |pack| {
                    pack.offer_on_join && (pack.essential || enabled)
                });
                let _ = reply.send(ids);
            }
            Message::PlayerJoined(player, handle, reply) => {
                let states = self
                    .join_states
                    .remove(&player)
                    .map(|(_, states)| states)
                    .unwrap_or_default();
                self.players.insert(player, handle);
                self.states.insert(
                    player,
                    states
                        .iter()
                        .filter(|(id, _)| self.entries.contains_key(id))
                        .map(|(id, state)| (*id, state.clone()))
                        .collect(),
                );
                let joined: Vec<u128> = self
                    .entries
                    .values()
                    .filter(|e| e.pack.offer_on_join && self.serves(&e.pack))
                    .map(|e| e.pack.id)
                    .collect();
                for id in joined {
                    let state = states.get(&id);
                    if state.is_some_and(|s| s.status == Some(PackStatus::Declined)) {
                        continue;
                    }
                    self.holders.entry(id).or_default().insert(player);
                    self.catch_up(id, player, state.map(|s| s.hash.as_str()));
                }
                let _ = reply.send(states);
            }
            Message::Configure(id, required, prompt) => {
                if let Some(entry) = self.entries.get_mut(&id) {
                    entry.pack.required = required;
                    entry.pack.prompt = prompt;
                }
            }
            Message::Settings(id, name, settings, reply) => {
                let found = match self.entries.get_mut(&id) {
                    Some(entry) => {
                        if let Some(name) = name {
                            entry.pack.name = name;
                        }
                        entry.pack.required = settings.required;
                        entry.pack.prompt = settings.prompt;
                        entry.pack.offer_on_join = settings.offer_on_join;
                        entry.pack.order = settings.order;
                        true
                    }
                    None => false,
                };
                let _ = reply.send(found);
            }
            Message::List(reply) => {
                let ids = join_order(&self.entries, |_| true);
                let list = ids
                    .into_iter()
                    .filter_map(|id| {
                        let entry = self.entries.get(&id)?;
                        Some(HostedPack {
                            id,
                            name: entry.pack.name.clone(),
                            settings: entry.pack.settings(),
                            essential: entry.pack.essential,
                            state: match &entry.state {
                                State::Building(_) => BuildState::Building,
                                State::Ready(built) => BuildState::Ready {
                                    sha1: built.sha1.clone(),
                                    size: built.bytes.len(),
                                },
                                State::Failed => BuildState::Failed,
                            },
                            holders: self.holders.get(&id).map_or(0, HashSet::len),
                        })
                    })
                    .collect();
                let _ = reply.send(list);
            }
            Message::PlayerStates(reply) => {
                let _ = reply.send(self.states.clone());
            }
            Message::Status(event) => {
                if event.status == PackStatus::Declined
                    && let Some(holders) = self.holders.get_mut(&event.pack)
                {
                    holders.remove(&event.player);
                }
                if self.players.contains_key(&event.player)
                    && let Some(state) = self
                        .states
                        .get_mut(&event.player)
                        .and_then(|states| states.get_mut(&event.pack))
                {
                    state.apply(event.status);
                }
            }
            Message::JoinStatus(event, hash) => {
                let now = std::time::Instant::now();
                self.join_states
                    .retain(|_, (at, _)| now.duration_since(*at) < JOIN_STATE_TTL);
                let (at, states) = self
                    .join_states
                    .entry(event.player)
                    .or_insert_with(|| (now, HashMap::new()));
                *at = now;
                let state = states.entry(event.pack).or_default();
                if state.hash != hash {
                    state.offered(&hash);
                }
                state.apply(event.status);
            }
            Message::PlayerLeft(player) => {
                self.players.remove(&player);
                self.states.remove(&player);
                for holders in self.holders.values_mut() {
                    holders.remove(&player);
                }
            }
            Message::Push(player, id) => self.wait_or_resolve(id, Waiter::Push(player)),
            Message::Pop(player, id) => {
                let targets: Vec<u128> = match player {
                    Some(player) => vec![player],
                    None => self.players.keys().copied().collect(),
                };
                for player in targets {
                    if let Some(handle) = self.players.get(&player) {
                        handle.send(crate::world::messages::PlayerEvent::ResourcePackPop(id));
                    }
                    match id {
                        Some(id) => {
                            if let Some(h) = self.holders.get_mut(&id) {
                                h.remove(&player);
                            }
                            if let Some(states) = self.states.get_mut(&player) {
                                states.remove(&id);
                            }
                        }
                        None => {
                            self.holders.values_mut().for_each(|h| {
                                h.remove(&player);
                            });
                            if let Some(states) = self.states.get_mut(&player) {
                                states.clear();
                            }
                        }
                    }
                }
            }
        }
    }

    /// Re-offers pack `id` to a player that just entered play when the
    /// version it answered for in configuration is no longer current, or
    /// it was never offered one (the pack changed or appeared meanwhile).
    fn catch_up(&mut self, id: u128, player: u128, answered: Option<&str>) {
        let Some(entry) = self.entries.get_mut(&id) else {
            return;
        };
        match &mut entry.state {
            State::Building(waiters) => waiters.push(Waiter::Push(Some(player))),
            State::Ready(built) if answered != Some(built.sha1.as_str()) => {
                let built = built.clone();
                self.send_push(id, Some(player), &built);
            }
            State::Ready(_) | State::Failed => {}
        }
    }

    fn wait_or_resolve(&mut self, id: u128, waiter: Waiter) {
        let Some(entry) = self.entries.get_mut(&id) else {
            return self.resolve(id, waiter, None);
        };
        match &mut entry.state {
            State::Building(waiters) => waiters.push(waiter),
            State::Ready(built) => {
                let built = built.clone();
                self.resolve(id, waiter, Some(&built));
            }
            State::Failed => self.resolve(id, waiter, None),
        }
    }

    fn resolve(&mut self, id: u128, waiter: Waiter, built: Option<&Arc<Built>>) {
        match waiter {
            Waiter::Fetch(reply) => {
                let _ = reply.send(built.cloned());
            }
            Waiter::Offer(reply) => {
                let offer = built.and_then(|b| self.make_offer(id, b));
                let _ = reply.send(offer);
            }
            Waiter::Push(target) => {
                if let Some(built) = built {
                    self.send_push(id, target, built);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tuff_http::{HttpConfig, MAIN_LISTENER};

    async fn raw(addr: std::net::SocketAddr, path: &str, extra: &str) -> String {
        let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
        let req = format!("GET {path} HTTP/1.1\r\nHost: t\r\nConnection: close\r\n{extra}\r\n");
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        s.read_to_end(&mut out).await.unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    fn packs() -> ResourcePacks {
        ResourcePacks::spawn(
            crate::public_ip::BaseUrl::detached("http://203.0.113.5:8080/"),
            crate::config::ConfigStore::new(),
        )
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panic_restarts_the_actor_with_its_packs() {
        let packs = packs();
        packs
            .register(ResourcePack::new(
                7,
                "p",
                PackSource::Bytes(b"x".to_vec().into()),
            ))
            .await;
        packs.panic_for_test().await;
        let listed = packs.list().await;
        assert_eq!(listed.len(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn uuid_forms_round_trip() {
        let id = 0x0123456789abcdef0123456789abcdefu128;
        assert_eq!(parse_uuid(&format_uuid(id)), Some(id));
        assert_eq!(parse_uuid("zz"), None);
        assert_eq!(parse_uuid(&"g".repeat(32)), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn unreachable_bases_are_detected() {
        assert!(is_unreachable_base("http://127.0.0.1:0"));
        assert!(is_unreachable_base("http://localhost:8080/x"));
        assert!(is_unreachable_base("http://[::1]:80"));
        assert!(is_unreachable_base("http://0.0.0.0:80"));
        assert!(!is_unreachable_base("http://203.0.113.5:8080"));
        assert!(!is_unreachable_base("https://example.com"));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn pack_is_served_with_its_hash_and_unhosted_on_removal() {
        let http = http::spawn();
        let addr = http
            .bind(
                MAIN_LISTENER,
                HttpConfig {
                    hosts: vec!["127.0.0.1".into()],
                    port: 0,
                    ..HttpConfig::default()
                },
            )
            .await
            .unwrap()[0];
        let packs = packs();
        http.register_route(ROUTE_PREFIX, packs.route()).await;
        let data = Bytes::from_static(b"PK pretend zip");
        packs
            .register(ResourcePack::new(5, "demo", PackSource::Bytes(data.clone())).required(true))
            .await;
        let offer = packs.offer(5).await.unwrap();
        let mut hasher = Sha1::new();
        hasher.update(&data);
        let expected: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(offer.sha1, expected);
        assert!(offer.required);
        assert_eq!(
            offer.url,
            format!(
                "http://203.0.113.5:8080/resourcepacks/{}.zip",
                format_uuid(5)
            )
        );
        let path = format!("{ROUTE_PREFIX}{}.zip", format_uuid(5));
        let full = raw(addr, &path, "").await;
        assert!(full.starts_with("HTTP/1.1 200"));
        assert!(full.ends_with("PK pretend zip"));
        let etag = full
            .lines()
            .find_map(|l| l.strip_prefix("etag: "))
            .unwrap()
            .to_string();
        let part = raw(addr, &path, "Range: bytes=0-1\r\n").await;
        assert!(part.starts_with("HTTP/1.1 206"));
        assert!(part.ends_with("PK"));
        let cached = raw(addr, &path, &format!("If-None-Match: {etag}\r\n")).await;
        assert!(cached.starts_with("HTTP/1.1 304"));
        assert!(
            raw(addr, &format!("{ROUTE_PREFIX}nonsense.zip"), "")
                .await
                .starts_with("HTTP/1.1 404")
        );
        assert!(packs.remove(5).await);
        assert!(raw(addr, &path, "").await.starts_with("HTTP/1.1 404"));
        assert!(packs.offer(5).await.is_none());
        http.shutdown().await;
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn generated_and_file_sources_build_and_update_rehashes() {
        let packs = packs();
        let generated: Generator = Arc::new(|| Ok(Bytes::from_static(b"one")));
        packs
            .register(ResourcePack::new(
                1,
                "gen",
                PackSource::Generated(generated),
            ))
            .await;
        let first = packs.offer(1).await.unwrap().sha1;
        assert!(
            packs
                .update(1, PackSource::Bytes(Bytes::from_static(b"two")))
                .await
        );
        let second = packs.offer(1).await.unwrap().sha1;
        assert_ne!(first, second);
        assert!(!packs.update(2, PackSource::Bytes(Bytes::new())).await);

        let dir = std::env::temp_dir().join(format!("tuff-pack-{}", std::process::id()));
        std::fs::write(&dir, b"file bytes").unwrap();
        packs
            .register(ResourcePack::new(3, "file", PackSource::File(dir.clone())))
            .await;
        assert_eq!(packs.fetch(3).await.unwrap().bytes.as_ref(), b"file bytes");
        std::fs::remove_file(&dir).unwrap();
        packs
            .register(ResourcePack::new(4, "missing", PackSource::File(dir)))
            .await;
        assert!(packs.fetch(4).await.is_none());
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn disabled_switch_stops_join_offers() {
        let config = crate::config::ConfigStore::new();
        let packs = ResourcePacks::spawn(
            crate::public_ip::BaseUrl::detached("http://203.0.113.5:8080"),
            config.clone(),
        );
        packs
            .register(ResourcePack::new(
                1,
                "p",
                PackSource::Bytes(Bytes::from_static(b"x")),
            ))
            .await;
        assert_eq!(packs.join_offers().await.len(), 1);
        config
            .set(ENABLED_KEY, crate::config::Value::Bool(false))
            .unwrap();
        assert!(packs.join_offers().await.is_empty());
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn offers_do_not_queue_behind_a_saturated_rayon_pool() {
        let (release, held) = std::sync::mpsc::channel::<()>();
        let held = std::sync::Arc::new(std::sync::Mutex::new(held));
        let (started, running) = std::sync::mpsc::channel::<()>();
        let workers = rayon::current_num_threads();
        for _ in 0..workers {
            let held = held.clone();
            let started = started.clone();
            rayon::spawn(move || {
                let _ = started.send(());
                let _ = held.lock().unwrap().recv();
            });
        }
        for _ in 0..workers {
            running.recv().unwrap();
        }
        let packs = packs();
        packs
            .register(ResourcePack::new(
                1,
                "p",
                PackSource::Bytes(Bytes::from_static(b"x")),
            ))
            .await;
        let offers = tokio::time::timeout(std::time::Duration::from_secs(20), packs.join_offers())
            .await
            .expect("the pack build waited for the rayon pool");
        drop(release);
        assert_eq!(offers.len(), 1);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn join_offers_follow_order_and_settings_apply_live() {
        let packs = packs();
        let bytes = |b: &'static [u8]| PackSource::Bytes(Bytes::from_static(b));
        packs
            .register(
                ResourcePack::new(1, "custom", bytes(b"c"))
                    .essential(true)
                    .order(CUSTOM_CONTENT_ORDER),
            )
            .await;
        packs
            .register(ResourcePack::new(2, "b plugin", bytes(b"p")))
            .await;
        packs
            .register(ResourcePack::new(3, "a upload", bytes(b"u")).order(5))
            .await;
        packs
            .register(ResourcePack::new(4, "a under", bytes(b"v")).order(-1))
            .await;
        let order = |offers: Vec<PackOffer>| offers.iter().map(|o| o.id).collect::<Vec<_>>();
        assert_eq!(order(packs.join_offers().await), [4, 2, 3, 1]);

        let settings = PackSettings {
            required: true,
            prompt: Some(TextComponent::new("why")),
            offer_on_join: true,
            order: 1000,
        };
        assert!(
            packs
                .set_settings(4, Some("renamed".into()), settings.clone())
                .await
        );
        assert!(!packs.set_settings(9, None, settings.clone()).await);
        assert_eq!(order(packs.join_offers().await), [2, 3, 4, 1]);
        let offer = packs.offer(4).await.unwrap();
        assert!(offer.required);
        assert_eq!(offer.prompt.unwrap().plain(), "why");
        let listed = packs.list().await;
        assert_eq!(
            listed.iter().map(|p| p.id).collect::<Vec<_>>(),
            [2, 3, 4, 1]
        );
        let renamed = listed.iter().find(|p| p.id == 4).unwrap();
        assert_eq!(renamed.name, "renamed");
        assert_eq!(renamed.settings, settings);
        assert!(matches!(&renamed.state, BuildState::Ready { size: 1, .. }));
        assert!(listed.iter().find(|p| p.id == 1).unwrap().essential);
        packs
            .set_settings(
                3,
                None,
                PackSettings {
                    offer_on_join: false,
                    ..settings
                },
            )
            .await;
        assert_eq!(order(packs.join_offers().await), [2, 4, 1]);
        assert!(packs.player_states().await.is_empty());
    }
}
