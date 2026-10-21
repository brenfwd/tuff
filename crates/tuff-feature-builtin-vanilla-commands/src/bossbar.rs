//! `/bossbar`: custom boss bars. One actor owns every bar, the players
//! each is shown to and who of them is online; commands and the session
//! hooks change them through jobs it runs one at a time. The bars are
//! saved as the `minecraft:custom_boss_events` saved data, like the game:
//! when dirty on autosave, and on every flush.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tuff_core::command::args::{Value, suggest_resources};
use tuff_core::command::{CommandError, ParsedNode};
use tuff_core::protocol::play::{BossAction, BossEvent, boss_flags};
use tuff_core::storage::level::{inner, saved};
use tuff_core::world::Shared;
use tuff_core::world::messages::{Packets, PlayerEvent, PlayerHandle, PlayerId};
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{HookFuture, SessionHooks};
use tuff_core::world::{framed, packets};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_storage::backend::WorldBackend;
use tuff_storage::document::Document;

use super::targets;
use super::{Context, Executor, executor_future, scoreboard::format_list, text};

/// The saved data the bars are kept in.
pub const SAVED_DATA: &str = "minecraft:custom_boss_events";
const DEFAULT_MAX: i32 = 100;
/// How often changed bars are saved, in ticks.
const AUTOSAVE_TICKS: u64 = 6000;
/// Bounded: senders are commands and joining or leaving sessions, which
/// can wait their turn.
const INBOX: usize = 256;

/// A bar's color, in protocol order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Pink,
    Blue,
    Red,
    Green,
    Yellow,
    Purple,
    White,
}

impl Color {
    const ALL: [Self; 7] = [
        Self::Pink,
        Self::Blue,
        Self::Red,
        Self::Green,
        Self::Yellow,
        Self::Purple,
        Self::White,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Pink => "pink",
            Self::Blue => "blue",
            Self::Red => "red",
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Purple => "purple",
            Self::White => "white",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == name)
    }

    /// The chat color of the bar's name in command feedback.
    fn chat_color(self) -> &'static str {
        match self {
            Self::Pink => "red",
            Self::Blue => "blue",
            Self::Red => "dark_red",
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Purple => "dark_blue",
            Self::White => "white",
        }
    }
}

/// How a bar is divided, in protocol order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    Progress,
    Notched6,
    Notched10,
    Notched12,
    Notched20,
}

impl Overlay {
    const ALL: [Self; 5] = [
        Self::Progress,
        Self::Notched6,
        Self::Notched10,
        Self::Notched12,
        Self::Notched20,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Progress => "progress",
            Self::Notched6 => "notched_6",
            Self::Notched10 => "notched_10",
            Self::Notched12 => "notched_12",
            Self::Notched20 => "notched_20",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.name() == name)
    }
}

/// An online player a bar can be shown to.
#[derive(Debug, Clone)]
pub struct Viewer {
    pub id: PlayerId,
    pub uuid: u128,
    pub name: String,
    pub handle: PlayerHandle,
}

/// One custom boss bar.
#[derive(Debug)]
pub struct Bar {
    id: String,
    uuid: u128,
    name: TextComponent,
    /// The saved name when this server cannot model all of it, kept until
    /// the name is changed.
    saved_name: Option<Tag>,
    visible: bool,
    value: i32,
    max: i32,
    progress: f32,
    color: Color,
    overlay: Overlay,
    darken_screen: bool,
    play_music: bool,
    create_fog: bool,
    /// Everyone the bar is shown to, online or not.
    players: BTreeSet<u128>,
    /// Those of `players` online now, in the order they were added.
    online: Vec<Viewer>,
}

/// The game's float clamp, which keeps NaN.
fn clamp_progress(progress: f32) -> f32 {
    if progress < 0.0 {
        0.0
    } else if progress.is_nan() {
        progress
    } else {
        progress.min(1.0)
    }
}

impl Bar {
    pub fn new(id: String, uuid: u128, name: TextComponent) -> Self {
        Self {
            id,
            uuid,
            name,
            saved_name: None,
            visible: true,
            value: 0,
            max: DEFAULT_MAX,
            progress: 0.0,
            color: Color::White,
            overlay: Overlay::Progress,
            darken_screen: false,
            play_music: false,
            create_fog: false,
            players: BTreeSet::new(),
            online: Vec::new(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn uuid(&self) -> u128 {
        self.uuid
    }

    pub fn name(&self) -> &TextComponent {
        &self.name
    }

    pub fn value(&self) -> i32 {
        self.value
    }

    pub fn max(&self) -> i32 {
        self.max
    }

    pub fn progress(&self) -> f32 {
        self.progress
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn color(&self) -> Color {
        self.color
    }

    pub fn overlay(&self) -> Overlay {
        self.overlay
    }

    /// The online players the bar is shown to.
    pub fn online(&self) -> &[Viewer] {
        &self.online
    }

    /// Everyone the bar is shown to, online or not.
    pub fn players(&self) -> &BTreeSet<u128> {
        &self.players
    }

    /// The bar as command feedback names it: its name in brackets, in its
    /// color, with its id on hover.
    pub fn display_name(&self) -> TextComponent {
        TextComponent::translate("chat.square_brackets", [self.name.clone()])
            .color(self.color.chat_color())
            .hover_text(TextComponent::new(self.id.clone()))
            .insertion(self.id.clone())
    }

    fn flags(&self) -> u8 {
        let mut flags = 0;
        if self.darken_screen {
            flags |= boss_flags::DARKEN_SCREEN;
        }
        if self.play_music {
            flags |= boss_flags::PLAY_MUSIC;
        }
        if self.create_fog {
            flags |= boss_flags::CREATE_FOG;
        }
        flags
    }

    fn add_packet(&self) -> Packets {
        packets([framed(&BossEvent {
            uuid: self.uuid,
            action: BossAction::Add {
                title: &self.name,
                progress: self.progress,
                color: self.color as i32,
                overlay: self.overlay as i32,
                flags: self.flags(),
            },
        })])
    }

    fn remove_packet(&self) -> Packets {
        packets([framed(&BossEvent {
            uuid: self.uuid,
            action: BossAction::Remove,
        })])
    }

    fn send_online(&mut self, packet: Packets) {
        self.online
            .retain(|viewer| viewer.handle.send(PlayerEvent::Packets(packet.clone())));
    }

    fn broadcast(&mut self, action: BossAction<'_>) {
        if !self.visible || self.online.is_empty() {
            return;
        }
        let packet = packets([framed(&BossEvent {
            uuid: self.uuid,
            action,
        })]);
        self.send_online(packet);
    }

    fn set_progress(&mut self, progress: f32) {
        if progress.to_bits() != self.progress.to_bits() {
            self.progress = progress;
            self.broadcast(BossAction::Progress(progress));
        }
    }

    pub fn set_value(&mut self, value: i32) {
        self.value = value;
        self.set_progress(clamp_progress(value as f32 / self.max as f32));
    }

    pub fn set_max(&mut self, max: i32) {
        self.max = max;
        self.set_progress(clamp_progress(self.value as f32 / max as f32));
    }

    /// Renames the bar; false when the name is the same.
    pub fn set_name(&mut self, name: TextComponent) -> bool {
        if name == self.name {
            return false;
        }
        self.name = name;
        self.saved_name = None;
        let name = self.name.clone();
        self.broadcast(BossAction::Title(&name));
        true
    }

    fn send_style(&mut self) {
        let (color, overlay) = (self.color as i32, self.overlay as i32);
        self.broadcast(BossAction::Style { color, overlay });
    }

    pub fn set_color(&mut self, color: Color) -> bool {
        if color == self.color {
            return false;
        }
        self.color = color;
        self.send_style();
        true
    }

    pub fn set_overlay(&mut self, overlay: Overlay) -> bool {
        if overlay == self.overlay {
            return false;
        }
        self.overlay = overlay;
        self.send_style();
        true
    }

    /// Sets the darken screen, boss music and fog flags.
    pub fn set_flags(&mut self, darken_screen: bool, play_music: bool, create_fog: bool) {
        let before = self.flags();
        self.darken_screen = darken_screen;
        self.play_music = play_music;
        self.create_fog = create_fog;
        let flags = self.flags();
        if flags != before {
            self.broadcast(BossAction::Flags(flags));
        }
    }

    pub fn set_visible(&mut self, visible: bool) {
        if visible == self.visible {
            return;
        }
        self.visible = visible;
        let packet = if visible {
            self.add_packet()
        } else {
            self.remove_packet()
        };
        self.send_online(packet);
    }

    fn show(&mut self, viewer: Viewer) {
        if self.online.iter().any(|v| v.id == viewer.id) {
            return;
        }
        if self.visible {
            viewer.handle.send(PlayerEvent::Packets(self.add_packet()));
        }
        self.online.push(viewer);
    }

    fn hide(&mut self, uuid: u128) {
        let Some(index) = self.online.iter().position(|v| v.uuid == uuid) else {
            return;
        };
        let viewer = self.online.remove(index);
        if self.visible {
            viewer
                .handle
                .send(PlayerEvent::Packets(self.remove_packet()));
        }
    }

    /// Shows the bar to exactly `targets`; false when they already were
    /// the bar's players.
    pub fn set_players(&mut self, targets: Vec<Viewer>) -> bool {
        let removed: Vec<u128> = self
            .players
            .iter()
            .copied()
            .filter(|uuid| !targets.iter().any(|t| t.uuid == *uuid))
            .collect();
        let added: Vec<Viewer> = targets
            .into_iter()
            .filter(|t| !self.players.contains(&t.uuid))
            .collect();
        let changed = !removed.is_empty() || !added.is_empty();
        for uuid in removed {
            self.hide(uuid);
            self.players.remove(&uuid);
        }
        for viewer in added {
            self.players.insert(viewer.uuid);
            self.show(viewer);
        }
        changed
    }

    fn remove_all_players(&mut self) {
        let uuids: Vec<u128> = self.online.iter().map(|v| v.uuid).collect();
        for uuid in uuids {
            self.hide(uuid);
        }
        self.players.clear();
    }

    fn to_nbt(&self) -> Compound {
        let mut nbt = Compound::new().with(
            "Name",
            self.saved_name
                .clone()
                .unwrap_or_else(|| self.name.to_nbt()),
        );
        let flag = |nbt: &mut Compound, key: &str, value: bool| {
            if value {
                nbt.insert(key, Tag::Byte(1));
            }
        };
        flag(&mut nbt, "Visible", self.visible);
        if self.value != 0 {
            nbt.insert("Value", Tag::Int(self.value));
        }
        if self.max != DEFAULT_MAX {
            nbt.insert("Max", Tag::Int(self.max));
        }
        if self.color != Color::White {
            nbt.insert("Color", self.color.name());
        }
        if self.overlay != Overlay::Progress {
            nbt.insert("Overlay", self.overlay.name());
        }
        flag(&mut nbt, "DarkenScreen", self.darken_screen);
        flag(&mut nbt, "PlayBossMusic", self.play_music);
        flag(&mut nbt, "CreateWorldFog", self.create_fog);
        if !self.players.is_empty() {
            let players = self.players.iter().map(|uuid| Tag::uuid(*uuid)).collect();
            nbt.insert("Players", Tag::List(players));
        }
        nbt
    }

    /// Reads a saved bar; `None` when it has no name or a field is of the
    /// wrong kind.
    fn from_nbt(id: &str, uuid: u128, nbt: &Compound) -> Option<Self> {
        let raw = nbt.get("Name")?;
        let (name, saved_name) = match TextComponent::from_nbt(raw) {
            Some(name) => (name, None),
            None => (fallback_name(raw), Some(raw.clone())),
        };
        let number = |key: &str, default: i32| match nbt.get(key) {
            None => Some(default),
            Some(Tag::Byte(v)) => Some(i32::from(*v)),
            Some(Tag::Short(v)) => Some(i32::from(*v)),
            Some(Tag::Int(v)) => Some(*v),
            Some(Tag::Long(v)) => Some(*v as i32),
            Some(_) => None,
        };
        let flag = |key: &str| number(key, 0).map(|v| v != 0);
        let color = match nbt.get("Color") {
            None => Color::White,
            Some(tag) => Color::from_name(tag.as_str()?)?,
        };
        let overlay = match nbt.get("Overlay") {
            None => Overlay::Progress,
            Some(tag) => Overlay::from_name(tag.as_str()?)?,
        };
        let players = match nbt.get("Players") {
            None => BTreeSet::new(),
            Some(tag) => tag
                .as_list()?
                .iter()
                .map(Tag::as_uuid)
                .collect::<Option<BTreeSet<u128>>>()?,
        };
        let mut bar = Self::new(id.to_owned(), uuid, name);
        bar.saved_name = saved_name;
        bar.visible = flag("Visible")?;
        bar.value = number("Value", 0)?;
        bar.max = number("Max", DEFAULT_MAX)?;
        bar.progress = clamp_progress(bar.value as f32 / bar.max as f32);
        bar.color = color;
        bar.overlay = overlay;
        bar.darken_screen = flag("DarkenScreen")?;
        bar.play_music = flag("PlayBossMusic")?;
        bar.create_fog = flag("CreateWorldFog")?;
        bar.players = players;
        Some(bar)
    }
}

/// The text of a saved name this server cannot model, for display.
fn fallback_name(tag: &Tag) -> TextComponent {
    fn collect(tag: &Tag, out: &mut String) {
        match tag {
            Tag::String(text) => out.push_str(text),
            Tag::List(items) => items.iter().for_each(|item| collect(item, out)),
            Tag::Compound(map) => {
                if let Some(text) = map.get_str("text") {
                    out.push_str(text);
                } else if let Some(key) = map.get_str("translate") {
                    out.push_str(map.get_str("fallback").unwrap_or(key));
                }
                if let Some(extra) = map.get("extra") {
                    collect(extra, out);
                }
            }
            _ => {}
        }
    }
    let mut out = String::new();
    collect(tag, &mut out);
    TextComponent::new(out)
}

/// A random version 4 UUID, as the game gives each bar.
fn random_uuid(rng: &mut impl Rng) -> u128 {
    let high: u64 = rng.r#gen::<u64>() & 0xFFFF_FFFF_FFFF_0FFF | 0x4000;
    let low: u64 = rng.r#gen::<u64>() & 0x3FFF_FFFF_FFFF_FFFF | 0x8000_0000_0000_0000;
    (u128::from(high) << 64) | u128::from(low)
}

/// Every custom bar, by id.
#[derive(Default)]
pub struct Bars {
    bars: BTreeMap<String, Bar>,
    dirty: bool,
}

impl Bars {
    pub fn get(&self, id: &str) -> Option<&Bar> {
        self.bars.get(id)
    }

    /// The bar to change; marks the bars as needing a save.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Bar> {
        let bar = self.bars.get_mut(id)?;
        self.dirty = true;
        Some(bar)
    }

    pub fn len(&self) -> usize {
        self.bars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Bar> {
        self.bars.values()
    }

    pub fn ids(&self) -> Vec<String> {
        self.bars.keys().cloned().collect()
    }

    /// Adds a bar, unless `id` is taken.
    pub fn create(&mut self, id: &str, uuid: u128, name: TextComponent) -> Option<&Bar> {
        if self.bars.contains_key(id) {
            return None;
        }
        self.dirty = true;
        let bar = Bar::new(id.to_owned(), uuid, name);
        Some(self.bars.entry(id.to_owned()).or_insert(bar))
    }

    /// Hides the bar from everyone and drops it.
    pub fn remove(&mut self, id: &str) -> Option<Bar> {
        let mut bar = self.bars.remove(id)?;
        bar.remove_all_players();
        self.dirty = true;
        Some(bar)
    }

    /// A player joined: the bars they were shown to are shown again.
    pub fn connect(&mut self, viewer: Viewer) {
        for bar in self.bars.values_mut() {
            if bar.players.contains(&viewer.uuid) {
                bar.show(viewer.clone());
            }
        }
    }

    /// A player left: they stay the bars' player for when they return.
    pub fn disconnect(&mut self, player: PlayerId) {
        for bar in self.bars.values_mut() {
            bar.online.retain(|v| v.id != player);
        }
    }

    pub fn to_nbt(&self) -> Compound {
        self.bars
            .iter()
            .map(|(id, bar)| (id.clone(), Tag::Compound(bar.to_nbt())))
            .collect()
    }

    /// Reads saved bars, each under a fresh UUID; bars that cannot be read
    /// are dropped.
    pub fn from_nbt(data: &Compound, rng: &mut impl Rng) -> Self {
        let mut bars = Self::default();
        for (id, bar) in data.iter() {
            let id = tuff_core::command::args::qualify(id);
            match bar
                .as_compound()
                .and_then(|nbt| Bar::from_nbt(&id, random_uuid(rng), nbt))
            {
                Some(bar) => {
                    bars.bars.insert(id, bar);
                }
                None => tuff_core::log::warn!("Dropping custom boss bar {id}: it cannot be read"),
            }
        }
        bars
    }
}

type Job = Box<dyn FnOnce(&mut Bars) + Send>;

enum Message {
    Job(Job),
    Flush(oneshot::Sender<()>),
}

/// Where the bars are read and changed.
#[derive(Clone)]
pub struct BossBars {
    inbox: mpsc::Sender<Message>,
    ids: watch::Receiver<Arc<[String]>>,
}

impl BossBars {
    /// Starts the bars' actor, loading the saved bars first.
    pub fn start(shared: Arc<Shared>) -> Self {
        let (inbox, messages) = mpsc::channel(INBOX);
        let (ids, ids_rx) = watch::channel(Arc::from(Vec::new()));
        tuff_supervise::spawn("the boss bars", run(shared, messages, ids));
        Self { inbox, ids: ids_rx }
    }

    /// Runs `job` on the bars; `None` if the world stopped first.
    pub async fn with<R: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Bars) -> R + Send + 'static,
    ) -> Option<R> {
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |bars| {
            let _ = reply.send(job(bars));
        });
        self.inbox.send(Message::Job(job)).await.ok()?;
        answer.await.ok()
    }

    /// The bars' ids as of the last change, for completion.
    pub fn ids(&self) -> Arc<[String]> {
        self.ids.borrow().clone()
    }

    /// Resolves once the bars as they are now are saved.
    pub async fn flush(&self) {
        let (done, saved) = oneshot::channel();
        if self.inbox.send(Message::Flush(done)).await.is_ok() {
            let _ = saved.await;
        }
    }
}

struct Actor {
    backend: Arc<dyn WorldBackend>,
    bars: Bars,
    readable: bool,
    last_save: Option<JoinHandle<()>>,
    ids: watch::Sender<Arc<[String]>>,
}

impl Actor {
    fn save(&mut self) {
        if !self.bars.dirty || !self.readable {
            return;
        }
        self.bars.dirty = false;
        let data = saved(self.bars.to_nbt());
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
                tuff_core::log::error!("failed to save custom boss bars: {err}");
            }
        }));
    }

    fn publish_ids(&self) {
        let ids = self.bars.ids();
        self.ids.send_if_modified(|current| {
            if current.as_ref() == ids.as_slice() {
                return false;
            }
            *current = Arc::from(ids);
            true
        });
    }

    fn handle(&mut self, message: Message) {
        match message {
            Message::Job(job) => {
                job(&mut self.bars);
                self.publish_ids();
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
    tuff_supervise::restarting("the boss bars", (actor, messages), |(actor, messages)| {
        Box::pin(serve(actor, shared.clone(), messages))
    })
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
    let mut rng = StdRng::from_entropy();
    let (bars, readable) = match loaded {
        Ok(Ok(root)) => (Bars::from_nbt(&inner(root), &mut rng), true),
        Ok(Err(err)) => {
            tuff_core::log::warn!("failed to read custom boss bars: {err}");
            (Bars::default(), false)
        }
        Err(_) => (Bars::default(), false),
    };
    let actor = Actor {
        backend,
        bars,
        readable,
        last_save: None,
        ids,
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

/// The world service owning the bars' actor.
pub struct BossBarService {
    bars: OnceLock<BossBars>,
}

impl BossBarService {
    /// The bars, once the world has started.
    pub fn bars(&self) -> Option<BossBars> {
        self.bars.get().cloned()
    }

    /// The bars of the world `shared` belongs to.
    pub fn of(shared: &Shared) -> Option<BossBars> {
        shared.services.get::<Self>()?.bars()
    }
}

pub struct BossBarFactory;

impl ServiceFactory for BossBarFactory {
    fn create(
        &self,
        _dimensions: &tuff_core::world::dimension::Dimensions,
    ) -> Arc<dyn WorldService> {
        Arc::new(BossBarService {
            bars: OnceLock::new(),
        })
    }
}

impl WorldService for BossBarService {
    fn start(&self, shared: &Arc<Shared>) {
        self.bars.get_or_init(|| BossBars::start(shared.clone()));
    }

    fn flush(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if let Some(bars) = self.bars() {
                bars.flush().await;
            }
        })
    }
}

/// Shows joining players the bars they were added to, and forgets their
/// session when they leave.
pub struct BossBarHooks;

impl SessionHooks for BossBarHooks {
    fn joined<'a>(&'a self, session: &'a mut PlayerSession) -> HookFuture<'a> {
        Box::pin(async move {
            let Some(bars) = BossBarService::of(&session.shared) else {
                return;
            };
            let viewer = Viewer {
                id: session.conn.handle.id,
                uuid: session.profile.id,
                name: session.profile.name.clone(),
                handle: session.conn.handle.clone(),
            };
            bars.with(move |bars| bars.connect(viewer)).await;
        })
    }

    fn leaving<'a>(&'a self, session: &'a mut PlayerSession) -> HookFuture<'a> {
        Box::pin(async move {
            let Some(bars) = BossBarService::of(&session.shared) else {
                return;
            };
            let id = session.conn.handle.id;
            bars.with(move |bars| bars.disconnect(id)).await;
        })
    }
}

pub fn unknown(id: &str) -> CommandError {
    CommandError::dynamic("commands.bossbar.unknown", [id.to_owned()])
}

/// The bars of the command's world.
pub fn bars(ctx: &Context<'_>) -> Result<BossBars, CommandError> {
    BossBarService::of(ctx.shared()).ok_or_else(|| CommandError::simple("command.failed"))
}

fn bad_argument() -> CommandError {
    CommandError::simple("command.unknown.argument")
}

fn id_arg(ctx: &Context<'_>) -> Result<String, CommandError> {
    match ctx.arg("id") {
        Some(Value::Identifier(id)) => Ok(id.clone()),
        _ => Err(bad_argument()),
    }
}

/// `/bossbar add|remove|list|set|get`.
pub struct BossBarCommand;

impl BossBarCommand {
    async fn add(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
        let id = id_arg(ctx)?;
        let Some(Value::Component(name)) = ctx.arg("name").cloned() else {
            return Err(bad_argument());
        };
        let name = super::resolve::resolve(ctx, &name).await?;
        let uuid = random_uuid(ctx.rng());
        let created = bars(ctx)?
            .with({
                let id = id.clone();
                move |bars| {
                    let shown = bars.create(&id, uuid, name)?.display_name();
                    Some((shown, bars.len()))
                }
            })
            .await
            .ok_or_else(|| CommandError::simple("command.failed"))?;
        let Some((shown, count)) = created else {
            return Err(CommandError::dynamic(
                "commands.bossbar.create.failed",
                [id],
            ));
        };
        ctx.success(
            TextComponent::translate("commands.bossbar.create.success", [shown]),
            true,
        )
        .await;
        Ok(count as i32)
    }

    async fn remove(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
        let id = id_arg(ctx)?;
        let removed = bars(ctx)?
            .with({
                let id = id.clone();
                move |bars| {
                    let bar = bars.remove(&id)?;
                    Some((bar.display_name(), bars.len()))
                }
            })
            .await
            .flatten();
        let Some((shown, count)) = removed else {
            return Err(unknown(&id));
        };
        ctx.success(
            TextComponent::translate("commands.bossbar.remove.success", [shown]),
            true,
        )
        .await;
        Ok(count as i32)
    }

    async fn list(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
        let names = bars(ctx)?
            .with(|bars| bars.iter().map(Bar::display_name).collect::<Vec<_>>())
            .await
            .unwrap_or_default();
        let message = if names.is_empty() {
            TextComponent::translate("commands.bossbar.list.bars.none", [])
        } else {
            TextComponent::translate(
                "commands.bossbar.list.bars.some",
                [text(names.len()), format_list(names.clone())],
            )
        };
        ctx.success(message, false).await;
        Ok(names.len() as i32)
    }

    async fn get(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
        let id = id_arg(ctx)?;
        let property = ctx.literals().get(3).copied().unwrap_or_default();
        let found = bars(ctx)?
            .with({
                let id = id.clone();
                move |bars| {
                    let bar = bars.get(&id)?;
                    let online: Vec<(String, u128)> = bar
                        .online
                        .iter()
                        .map(|v| (v.name.clone(), v.uuid))
                        .collect();
                    Some((bar.display_name(), bar.value, bar.max, bar.visible, online))
                }
            })
            .await
            .flatten();
        let Some((shown, value, max, visible, online)) = found else {
            return Err(unknown(&id));
        };
        let (message, result) = match property {
            "value" => (
                TextComponent::translate("commands.bossbar.get.value", [shown, text(value)]),
                value,
            ),
            "max" => (
                TextComponent::translate("commands.bossbar.get.max", [shown, text(max)]),
                max,
            ),
            "visible" if visible => (
                TextComponent::translate("commands.bossbar.get.visible.visible", [shown]),
                1,
            ),
            "visible" => (
                TextComponent::translate("commands.bossbar.get.visible.hidden", [shown]),
                0,
            ),
            "players" if online.is_empty() => (
                TextComponent::translate("commands.bossbar.get.players.none", [shown]),
                0,
            ),
            "players" => {
                let teams = ctx.shared().scoreboard.teams();
                let names = online
                    .iter()
                    .map(|(name, uuid)| teams.player_name(name, *uuid))
                    .collect();
                (
                    TextComponent::translate(
                        "commands.bossbar.get.players.some",
                        [shown, text(online.len()), format_list(names)],
                    ),
                    online.len() as i32,
                )
            }
            _ => return Err(bad_argument()),
        };
        ctx.success(message, true).await;
        Ok(result)
    }

    async fn set(ctx: &mut Context<'_>) -> Result<i32, CommandError> {
        let id = id_arg(ctx)?;
        let literals = ctx.literals();
        let property = literals.get(3).copied().unwrap_or_default();
        let choice = literals.get(4).copied().unwrap_or_default();
        let bars = bars(ctx)?;
        if property == "players" {
            return Self::set_players(ctx, bars, id).await;
        }
        let change = match property {
            "name" => match ctx.arg("name").cloned() {
                Some(Value::Component(name)) => {
                    Change::Name(super::resolve::resolve(ctx, &name).await?)
                }
                _ => return Err(bad_argument()),
            },
            "color" => Change::Color(Color::from_name(choice).ok_or_else(bad_argument)?),
            "style" => Change::Overlay(Overlay::from_name(choice).ok_or_else(bad_argument)?),
            "value" => match ctx.arg("value") {
                Some(Value::Int(value)) => Change::Value(*value),
                _ => return Err(bad_argument()),
            },
            "max" => match ctx.arg("max") {
                Some(Value::Int(max)) => Change::Max(*max),
                _ => return Err(bad_argument()),
            },
            "visible" => match ctx.arg("visible") {
                Some(Value::Bool(visible)) => Change::Visible(*visible),
                _ => return Err(bad_argument()),
            },
            _ => return Err(bad_argument()),
        };
        let outcome = bars
            .with({
                let id = id.clone();
                move |bars| {
                    let bar = bars.get_mut(&id).ok_or_else(|| unknown(&id))?;
                    change.apply(bar)
                }
            })
            .await
            .ok_or_else(|| CommandError::simple("command.failed"))??;
        ctx.success(outcome.message, true).await;
        Ok(outcome.result)
    }

    async fn set_players(
        ctx: &mut Context<'_>,
        bars: BossBars,
        id: String,
    ) -> Result<i32, CommandError> {
        let exists = bars
            .with({
                let id = id.clone();
                move |bars| bars.get(&id).is_some()
            })
            .await
            .unwrap_or(false);
        if !exists {
            return Err(unknown(&id));
        }
        let players = match ctx.arg("targets").cloned() {
            Some(Value::Selector(selector)) => {
                match targets::select_players(ctx, &selector).await {
                    Ok(players) => players,
                    Err(err) if err.key() == Some("argument.entity.notfound.player") => Vec::new(),
                    Err(err) => return Err(err),
                }
            }
            _ => Vec::new(),
        };
        let teams = ctx.shared().scoreboard.teams();
        let names: Vec<TextComponent> = players
            .iter()
            .map(|p| teams.player_name(&p.name, p.uuid))
            .collect();
        let viewers: Vec<Viewer> = players
            .into_iter()
            .map(|p| Viewer {
                id: p.id,
                uuid: p.uuid,
                name: p.name,
                handle: p.handle,
            })
            .collect();
        let outcome = bars
            .with({
                let id = id.clone();
                move |bars| {
                    let bar = bars.get_mut(&id)?;
                    let changed = bar.set_players(viewers);
                    Some((changed, bar.display_name(), bar.online.len()))
                }
            })
            .await
            .flatten();
        let Some((changed, shown, online)) = outcome else {
            return Err(unknown(&id));
        };
        if !changed {
            return Err(CommandError::simple(
                "commands.bossbar.set.players.unchanged",
            ));
        }
        let message = if online == 0 {
            TextComponent::translate("commands.bossbar.set.players.success.none", [shown])
        } else {
            TextComponent::translate(
                "commands.bossbar.set.players.success.some",
                [shown, text(names.len()), format_list(names)],
            )
        };
        ctx.success(message, true).await;
        Ok(online as i32)
    }
}

enum Change {
    Name(TextComponent),
    Color(Color),
    Overlay(Overlay),
    Value(i32),
    Max(i32),
    Visible(bool),
}

struct Outcome {
    message: TextComponent,
    result: i32,
}

impl Change {
    fn apply(self, bar: &mut Bar) -> Result<Outcome, CommandError> {
        let unchanged = |key: &'static str| Err(CommandError::simple(key));
        let (key, result, with) = match self {
            Self::Name(name) => {
                if !bar.set_name(name) {
                    return unchanged("commands.bossbar.set.name.unchanged");
                }
                ("commands.bossbar.set.name.success", 0, None)
            }
            Self::Color(color) => {
                if !bar.set_color(color) {
                    return unchanged("commands.bossbar.set.color.unchanged");
                }
                ("commands.bossbar.set.color.success", 0, None)
            }
            Self::Overlay(overlay) => {
                if !bar.set_overlay(overlay) {
                    return unchanged("commands.bossbar.set.style.unchanged");
                }
                ("commands.bossbar.set.style.success", 0, None)
            }
            Self::Value(value) => {
                if bar.value == value {
                    return unchanged("commands.bossbar.set.value.unchanged");
                }
                bar.set_value(value);
                ("commands.bossbar.set.value.success", value, Some(value))
            }
            Self::Max(max) => {
                if bar.max == max {
                    return unchanged("commands.bossbar.set.max.unchanged");
                }
                bar.set_max(max);
                ("commands.bossbar.set.max.success", max, Some(max))
            }
            Self::Visible(visible) => {
                if bar.visible == visible {
                    return unchanged(if visible {
                        "commands.bossbar.set.visibility.unchanged.visible"
                    } else {
                        "commands.bossbar.set.visibility.unchanged.hidden"
                    });
                }
                bar.set_visible(visible);
                let key = if visible {
                    "commands.bossbar.set.visible.success.visible"
                } else {
                    "commands.bossbar.set.visible.success.hidden"
                };
                (key, 0, None)
            }
        };
        let mut args = vec![bar.display_name()];
        args.extend(with.map(text));
        Ok(Outcome {
            message: TextComponent::translate(key, args),
            result,
        })
    }
}

impl Executor for BossBarCommand {
    fn execute<'a>(&'a self, ctx: &'a mut Context<'_>) -> super::ExecutorFuture<'a> {
        executor_future(async move {
            match ctx.literals().get(1).copied() {
                Some("add") => Self::add(ctx).await,
                Some("remove") => Self::remove(ctx).await,
                Some("list") => Self::list(ctx).await,
                Some("set") => Self::set(ctx).await,
                Some("get") => Self::get(ctx).await,
                _ => Err(bad_argument()),
            }
        })
    }

    fn suggest(
        &self,
        path: &[ParsedNode],
        partial: &str,
        input: &super::SuggestInput,
    ) -> Option<Vec<String>> {
        if path.iter().any(|node| node.node.name == "add") {
            return Some(Vec::new());
        }
        Some(suggest_ids(input, partial))
    }
}

/// The ids of the bars, for an id argument being typed.
pub fn suggest_ids(input: &super::SuggestInput, partial: &str) -> Vec<String> {
    let Some(bars) = input
        .services
        .get::<BossBarService>()
        .and_then(BossBarService::bars)
    else {
        return Vec::new();
    };
    let ids = bars.ids();
    suggest_resources(partial, ids.iter().map(String::as_str), "")
}

#[cfg(test)]
#[path = "bossbar_tests.rs"]
mod tests;
