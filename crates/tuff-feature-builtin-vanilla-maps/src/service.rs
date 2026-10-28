//! The map service: one actor owning every loaded map, the id counter and
//! who is holding what. Sessions report each held map every tick; the actor
//! tracks the holders' markers, asks regions to sample terrain for maps in
//! hand, and sends each holder the patches and markers it is owed. Saves
//! run on the blocking pool, chained so they land in order.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tuff_core::protocol::play::{MapData as MapPacket, MapPatch};
use tuff_core::storage::level::{inner, saved};
use tuff_core::world::dimension::{DimensionId, Dimensions};
use tuff_core::world::messages::{ManagerMessage, PlayerId, RegionHandle, RegionRequest};
use tuff_core::world::region_api::{RegionTask, RegionWorld};
use tuff_core::world::services::{ServiceFactory, WorldService};
use tuff_core::world::{Shared, framed};
use tuff_nbt::Compound;
use tuff_storage::backend::WorldBackend;
use tuff_storage::document::Document;
use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::registry::MapDecorationType;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, RegionPos};

use crate::data::{MapBanner, MapData, MapFrame, SIZE};
use crate::markers::{Frame, Markers, Spot, banner_type};
use crate::render::{Plan, Sample, Terrain, area_origin, sample};
use tuff_core::world::messages::{PlayerEvent, PlayerHandle};
use tuff_core::world::packets;

pub type MapId = u32;

pub const DECORATIONS: &str = "minecraft:map_decorations";
const INDEX: &str = "minecraft:maps/last_id";
const AUTOSAVE_TICKS: u64 = 600;
const PENDING_TICKS: u64 = 100;
const TRACKED_LIMIT: usize = 256;
const BANNER_BLOCK_SUFFIX: &str = "banner";

fn document(id: MapId) -> String {
    format!("minecraft:maps/{id}")
}

/// A session's report that its player carries map `id` this tick.
#[derive(Debug)]
pub struct Hold {
    pub id: MapId,
    pub player: PlayerId,
    pub name: String,
    pub outbound: mpsc::UnboundedSender<Bytes>,
    pub dimension: DimensionId,
    pub position: [f64; 3],
    pub yaw: f32,
    /// Held in a hand, so the map is drawn onto.
    pub render: bool,
    /// Wearing something that hides the player's marker.
    pub hidden: bool,
    /// The markers the item itself carries (an explorer map's target).
    pub decorations: Vec<StaticMarker>,
}

/// A marker an item carries in its `map_decorations` component.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticMarker {
    pub key: String,
    pub kind: MapDecorationType,
    pub x: f64,
    pub z: f64,
    pub rotation: f32,
}

impl StaticMarker {
    pub fn to_nbt(&self) -> Compound {
        Compound::new()
            .with("type", self.kind.name())
            .with("x", self.x)
            .with("z", self.z)
            .with("rotation", self.rotation)
    }

    /// The markers of a `map_decorations` component.
    pub fn read_all(component: &Compound) -> Vec<Self> {
        component
            .iter()
            .filter_map(|(key, value)| {
                let entry = value.as_compound()?;
                Some(Self {
                    key: key.to_owned(),
                    kind: MapDecorationType::from_name(entry.get_str("type")?)?,
                    x: entry.get_double("x")?,
                    z: entry.get_double("z")?,
                    rotation: entry.get_float("rotation")?,
                })
            })
            .collect()
    }
}

/// An item frame's report that it shows map `id`.
#[derive(Debug, Clone)]
pub struct Framed {
    pub id: MapId,
    pub dimension: DimensionId,
    pub frame: MapFrame,
}

fn frame_key(entity_id: i32) -> String {
    format!("frame-{entity_id}")
}

const FRAME_VIEW_TICKS: u64 = 20;
const FRAME_REPORT_KEY: &str = "tuff.maps.frameReportInterval";

fn frame_view_ticks(config: &tuff_core::config::ConfigStore) -> u64 {
    let interval = config
        .get_int(FRAME_REPORT_KEY)
        .unwrap_or(10)
        .clamp(1, 1200) as u64;
    FRAME_VIEW_TICKS.max(interval * 2)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Derivation {
    Lock,
    ZoomOut,
}

enum Message {
    #[cfg(test)]
    Panic,
    Create {
        dimension: String,
        x: f64,
        z: f64,
        scale: u8,
        tracking: bool,
        unlimited: bool,
        reply: oneshot::Sender<MapId>,
    },
    Derive {
        id: MapId,
        how: Derivation,
        reply: oneshot::Sender<Option<MapId>>,
    },
    Peek {
        id: MapId,
        reply: oneshot::Sender<Option<MapData>>,
    },
    Hold(Hold),
    Paint {
        id: MapId,
        colors: Vec<u8>,
    },
    Framed(Framed),
    Unframed {
        id: MapId,
        pos: BlockPos,
        entity_id: i32,
    },
    Banner {
        id: MapId,
        banner: MapBanner,
    },
    Part(Part),
    Loaded {
        id: MapId,
        data: Option<MapData>,
    },
    Saved(Vec<MapId>),
    Sync(oneshot::Sender<()>),
    Flush(oneshot::Sender<()>),
}

struct Part {
    id: MapId,
    player: PlayerId,
    plan: u64,
    samples: Vec<((i32, i32), Sample)>,
    stale: Vec<MapBanner>,
}

/// Where the actor is reached.
pub struct MapService {
    inbox: mpsc::UnboundedSender<Message>,
    start: Mutex<Option<mpsc::UnboundedReceiver<Message>>>,
}

impl MapService {
    fn new() -> Self {
        let (inbox, rx) = mpsc::unbounded_channel();
        Self {
            inbox,
            start: Mutex::new(Some(rx)),
        }
    }

    fn send(&self, message: Message) {
        let _ = self.inbox.send(message);
    }

    async fn ask<T>(&self, message: impl FnOnce(oneshot::Sender<T>) -> Message) -> Option<T> {
        let (reply, answer) = oneshot::channel();
        self.inbox.send(message(reply)).ok()?;
        answer.await.ok()
    }

    /// Makes a blank map centered for a player at (`x`, `z`) and returns
    /// its id.
    pub async fn create(
        &self,
        dimension: &str,
        x: f64,
        z: f64,
        scale: u8,
        tracking: bool,
        unlimited: bool,
    ) -> Option<MapId> {
        self.ask(|reply| Message::Create {
            dimension: dimension.to_owned(),
            x,
            z,
            scale,
            tracking,
            unlimited,
            reply,
        })
        .await
    }

    /// A locked copy of map `id` under a new id, or a blank map one scale
    /// larger around the same center.
    pub async fn derive(&self, id: MapId, how: Derivation) -> Option<MapId> {
        self.ask(|reply| Message::Derive { id, how, reply })
            .await
            .flatten()
    }

    /// A copy of map `id` as the actor has it.
    pub async fn data(&self, id: MapId) -> Option<MapData> {
        self.ask(|reply| Message::Peek { id, reply })
            .await
            .flatten()
    }

    /// Never waits: sessions and regions report without holding up their
    /// own ticks, and the volume is bounded by players times maps carried.
    pub fn hold(&self, hold: Hold) {
        self.send(Message::Hold(hold));
    }

    /// Replaces the pixels of map `id`.
    pub fn paint(&self, id: MapId, colors: Vec<u8>) {
        self.send(Message::Paint { id, colors });
    }

    /// An item frame shows the map: its marker is kept and players near
    /// it are sent the map. Never waits.
    pub fn framed(&self, framed: Framed) {
        self.send(Message::Framed(framed));
    }

    /// The map left the frame of entity `entity_id` at `pos`.
    pub fn unframed(&self, id: MapId, pos: BlockPos, entity_id: i32) {
        self.send(Message::Unframed { id, pos, entity_id });
    }

    /// Adds the banner to map `id`, or removes it if it is already there.
    pub fn toggle_banner(&self, id: MapId, banner: MapBanner) {
        self.send(Message::Banner { id, banner });
    }

    /// Resolves once every message sent before it has been handled.
    pub async fn sync(&self) {
        let _ = self.ask(Message::Sync).await;
    }
}

impl tuff_core::world::services::MapFrames for MapService {
    fn framed(
        &self,
        id: u32,
        dimension: DimensionId,
        pos: BlockPos,
        rotation: i32,
        entity_id: i32,
    ) {
        MapService::framed(
            self,
            Framed {
                id,
                dimension,
                frame: MapFrame {
                    pos,
                    rotation,
                    entity_id,
                },
            },
        );
    }

    fn unframed(&self, id: u32, pos: BlockPos, entity_id: i32) {
        MapService::unframed(self, id, pos, entity_id);
    }
}

impl WorldService for MapService {
    fn map_frames(&self) -> Option<&dyn tuff_core::world::services::MapFrames> {
        Some(self)
    }

    fn start(&self, shared: &Arc<Shared>) {
        let Some(inbox) = self.start.lock().expect("map service lock").take() else {
            return;
        };
        let mut actor = Actor::new(shared.clone(), self.inbox.clone());
        tuff_supervise::spawn("the map service", async move {
            actor.load_index().await;
            tuff_supervise::restarting("the map service", (actor, inbox), |(actor, inbox)| {
                Box::pin(actor.run(inbox))
            })
            .await;
        });
    }

    fn settled(&self, _tick: u64) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(self.sync())
    }

    fn flush(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            if self.start.lock().expect("map service lock").is_some() {
                return;
            }
            let _ = self.ask(Message::Flush).await;
        })
    }
}

pub struct MapServiceFactory;

impl ServiceFactory for MapServiceFactory {
    fn create(&self, _dimensions: &Dimensions) -> Arc<dyn WorldService> {
        Arc::new(MapService::new())
    }
}

#[derive(Clone, Copy)]
struct Rect {
    min_x: usize,
    min_y: usize,
    max_x: usize,
    max_y: usize,
}

struct Pending {
    plan_id: u64,
    plan: Plan,
    started: u64,
    remaining: usize,
    samples: HashMap<(i32, i32), Sample>,
    stale: Vec<MapBanner>,
}

enum Sink {
    Session(mpsc::UnboundedSender<Bytes>),
    Player(PlayerHandle),
}

impl Sink {
    fn send(&self, packet: Bytes) -> bool {
        match self {
            Self::Session(outbound) => outbound.send(packet).is_ok(),
            Self::Player(handle) => handle.send(PlayerEvent::Packets(packets([packet]))),
        }
    }
}

struct Holder {
    player: PlayerId,
    name: String,
    outbound: Sink,
    last_seen: u64,
    framed_until: u64,
    step: u32,
    dirty: Option<Rect>,
    markers_dirty: bool,
    marker_calls: u32,
    pending: Option<Pending>,
}

impl Holder {
    fn new(player: PlayerId, name: String, outbound: Sink, now: u64) -> Self {
        Self {
            player,
            name,
            outbound,
            last_seen: now,
            framed_until: 0,
            step: 0,
            dirty: Some(Rect {
                min_x: 0,
                min_y: 0,
                max_x: SIZE - 1,
                max_y: SIZE - 1,
            }),
            markers_dirty: true,
            marker_calls: 0,
            pending: None,
        }
    }

    fn mark_color(&mut self, x: usize, y: usize) {
        self.dirty = Some(match self.dirty {
            Some(r) => Rect {
                min_x: r.min_x.min(x),
                min_y: r.min_y.min(y),
                max_x: r.max_x.max(x),
                max_y: r.max_y.max(y),
            },
            None => Rect {
                min_x: x,
                min_y: y,
                max_x: x,
                max_y: y,
            },
        });
    }
}

struct Live {
    data: MapData,
    markers: Markers,
    holders: Vec<Holder>,
    dirty: bool,
}

impl Live {
    fn new(data: MapData, dirty: bool) -> Self {
        let mut markers = Markers::default();
        let frame = Frame::of(&data);
        for banner in &data.banners {
            markers.place(
                &frame,
                banner_type(banner.color),
                &banner.key(),
                Spot::new(
                    f64::from(banner.pos.x),
                    f64::from(banner.pos.z),
                    180.0,
                    None,
                ),
                banner.name.clone(),
            );
        }
        if data.tracking_position {
            for map_frame in &data.frames {
                markers.place(
                    &frame,
                    MapDecorationType::Frame,
                    &frame_key(map_frame.entity_id),
                    Spot::new(
                        f64::from(map_frame.pos.x),
                        f64::from(map_frame.pos.z),
                        f64::from(map_frame.rotation),
                        None,
                    ),
                    None,
                );
            }
        }
        Self {
            data,
            markers,
            holders: Vec::new(),
            dirty,
        }
    }

    fn mark_markers(&mut self) {
        for holder in &mut self.holders {
            holder.markers_dirty = true;
        }
    }

    fn mark_color(&mut self, x: usize, y: usize) {
        self.dirty = true;
        for holder in &mut self.holders {
            holder.mark_color(x, y);
        }
    }

    fn patch_for(&self, holder: &mut Holder) -> Option<(Rect, Vec<u8>)> {
        let rect = holder.dirty.take()?;
        let width = rect.max_x + 1 - rect.min_x;
        let height = rect.max_y + 1 - rect.min_y;
        let mut colors = Vec::with_capacity(width * height);
        for y in 0..height {
            let row = (rect.min_y + y) * SIZE + rect.min_x;
            colors.extend_from_slice(&self.data.colors[row..row + width]);
        }
        Some((rect, colors))
    }

    /// The packet owed to `holder`, if any; false if it is gone.
    fn update(&mut self, id: MapId, index: usize) -> bool {
        let mut holder = self.holders.swap_remove(index);
        let patch = self.patch_for(&mut holder);
        let markers = if holder.markers_dirty {
            let call = holder.marker_calls;
            holder.marker_calls += 1;
            if call.is_multiple_of(5) {
                holder.markers_dirty = false;
                Some(self.markers.markers())
            } else {
                None
            }
        } else {
            None
        };
        let alive = if patch.is_some() || markers.is_some() {
            let packet = MapPacket {
                id,
                scale: self.data.scale,
                locked: self.data.locked,
                markers: markers.as_deref(),
                patch: patch.as_ref().map(|(rect, colors)| MapPatch {
                    x: rect.min_x as u8,
                    y: rect.min_y as u8,
                    width: (rect.max_x + 1 - rect.min_x) as u8,
                    height: (rect.max_y + 1 - rect.min_y) as u8,
                    colors,
                }),
            };
            holder.outbound.send(framed(&packet))
        } else {
            true
        };
        self.holders.push(holder);
        let last = self.holders.len() - 1;
        self.holders.swap(index, last);
        alive
    }
}

struct Actor {
    shared: Arc<Shared>,
    inbox: mpsc::UnboundedSender<Message>,
    backend: Arc<dyn WorldBackend>,
    maps: HashMap<MapId, Live>,
    loading: HashSet<MapId>,
    deferred: HashMap<MapId, Vec<Message>>,
    saving: HashSet<MapId>,
    last_id: i32,
    index_dirty: bool,
    last_save: Option<JoinHandle<()>>,
    next_plan: u64,
    last_autosave: u64,
}

impl Actor {
    fn new(shared: Arc<Shared>, inbox: mpsc::UnboundedSender<Message>) -> Self {
        Self {
            backend: shared.backend.clone(),
            shared,
            inbox,
            maps: HashMap::new(),
            loading: HashSet::new(),
            deferred: HashMap::new(),
            saving: HashSet::new(),
            last_id: -1,
            index_dirty: false,
            last_save: None,
            next_plan: 0,
            last_autosave: 0,
        }
    }

    fn now(&self) -> u64 {
        *self.shared.clock.borrow()
    }

    async fn load_index(&mut self) {
        let backend = self.backend.clone();
        let index = tokio::task::spawn_blocking(move || {
            backend.documents().read(Document::SavedData(INDEX))
        })
        .await;
        match index {
            Ok(Ok(root)) => {
                self.last_id = inner(root).get_int("map").unwrap_or(-1);
            }
            Ok(Err(err)) => tuff_core::log::warn!("failed to read the map index: {err}"),
            Err(_) => {}
        }
    }

    async fn run(&mut self, inbox: &mut mpsc::UnboundedReceiver<Message>) {
        let mut clock = self.shared.clock.clone();
        loop {
            tokio::select! {
                message = inbox.recv() => {
                    let Some(message) = message else { break };
                    self.handle(message);
                }
                changed = clock.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let now = *clock.borrow_and_update();
                    if now >= self.last_autosave + AUTOSAVE_TICKS {
                        self.last_autosave = now;
                        self.save();
                        self.evict();
                    }
                }
            }
        }
    }

    fn handle(&mut self, message: Message) {
        match message {
            #[cfg(test)]
            Message::Panic => panic!("test panic"),
            Message::Create {
                dimension,
                x,
                z,
                scale,
                tracking,
                unlimited,
                reply,
            } => {
                let id = self.next_id();
                let data = MapData::fresh(x, z, scale, tracking, unlimited, &dimension);
                self.maps.insert(id, Live::new(data, true));
                let _ = reply.send(id);
            }
            Message::Derive { id, how, reply } => {
                if !self.loaded(id) {
                    self.defer(id, Message::Derive { id, how, reply });
                    return;
                }
                let derived = self.maps.get(&id).map(|live| match how {
                    Derivation::Lock => live.data.locked_copy(),
                    Derivation::ZoomOut => live.data.zoomed_out(),
                });
                let answer = derived.map(|data| {
                    let new = self.next_id();
                    self.maps.insert(new, Live::new(data, true));
                    new
                });
                let _ = reply.send(answer);
            }
            Message::Peek { id, reply } => {
                if !self.loaded(id) {
                    self.defer(id, Message::Peek { id, reply });
                    return;
                }
                let _ = reply.send(self.maps.get(&id).map(|live| live.data.clone()));
            }
            Message::Hold(hold) => self.hold(hold),
            Message::Paint { id, colors } => {
                if !self.loaded(id) {
                    self.defer(id, Message::Paint { id, colors });
                    return;
                }
                if let Some(live) = self.maps.get_mut(&id)
                    && colors.len() == live.data.colors.len()
                {
                    live.data.colors = colors;
                    live.dirty = true;
                    for holder in &mut live.holders {
                        holder.mark_color(0, 0);
                        holder.mark_color(SIZE - 1, SIZE - 1);
                    }
                }
            }
            Message::Framed(framed) => {
                if !self.loaded(framed.id) {
                    self.defer(framed.id, Message::Framed(framed));
                    return;
                }
                self.framed(framed);
            }
            Message::Unframed { id, pos, entity_id } => {
                if !self.loaded(id) {
                    self.defer(id, Message::Unframed { id, pos, entity_id });
                    return;
                }
                if let Some(live) = self.maps.get_mut(&id) {
                    live.markers.remove(&frame_key(entity_id));
                    live.data.frames.retain(|f| f.pos != pos);
                    live.dirty = true;
                    live.mark_markers();
                }
            }
            Message::Banner { id, banner } => {
                if !self.loaded(id) {
                    self.defer(id, Message::Banner { id, banner });
                    return;
                }
                self.toggle_banner(id, banner);
            }
            Message::Part(part) => self.part(part),
            Message::Loaded { id, data } => {
                self.loading.remove(&id);
                let live = data.map(|data| Live::new(data, false));
                let found = live.is_some();
                if let Some(live) = live {
                    self.maps.entry(id).or_insert(live);
                }
                for message in self.deferred.remove(&id).unwrap_or_default() {
                    if found {
                        self.handle(message);
                    } else {
                        match message {
                            Message::Derive { reply, .. } => {
                                let _ = reply.send(None);
                            }
                            Message::Peek { reply, .. } => {
                                let _ = reply.send(None);
                            }
                            _ => {}
                        }
                    }
                }
            }
            Message::Saved(ids) => {
                for id in ids {
                    self.saving.remove(&id);
                }
            }
            Message::Sync(reply) => {
                let _ = reply.send(());
            }
            Message::Flush(reply) => {
                self.save();
                let previous = self.last_save.take();
                self.last_save = Some(tokio::spawn(async move {
                    if let Some(previous) = previous {
                        let _ = previous.await;
                    }
                    let _ = reply.send(());
                }));
            }
        }
    }

    fn next_id(&mut self) -> MapId {
        self.last_id += 1;
        self.index_dirty = true;
        self.last_id as MapId
    }

    fn loaded(&mut self, id: MapId) -> bool {
        if self.maps.contains_key(&id) {
            return true;
        }
        if self.loading.insert(id) {
            let backend = self.backend.clone();
            let inbox = self.inbox.clone();
            tokio::spawn(async move {
                let name = document(id);
                let read = tokio::task::spawn_blocking(move || {
                    backend.documents().read(Document::SavedData(&name))
                })
                .await;
                let data = match read {
                    Ok(Ok(root)) => root.and_then(|root| {
                        let data = inner(Some(root));
                        MapData::from_nbt(&data)
                    }),
                    Ok(Err(err)) => {
                        tuff_core::log::warn!("failed to read map {id}: {err}");
                        None
                    }
                    Err(_) => None,
                };
                let _ = inbox.send(Message::Loaded { id, data });
            });
        }
        false
    }

    fn defer(&mut self, id: MapId, message: Message) {
        self.deferred.entry(id).or_default().push(message);
    }

    fn save(&mut self) {
        let mut writes: Vec<(MapId, Compound)> = Vec::new();
        for (&id, live) in &mut self.maps {
            if live.dirty {
                live.dirty = false;
                writes.push((id, saved(live.data.to_nbt())));
                self.saving.insert(id);
            }
        }
        let index = std::mem::take(&mut self.index_dirty)
            .then(|| saved(Compound::new().with("map", self.last_id)));
        if writes.is_empty() && index.is_none() {
            return;
        }
        let backend = self.backend.clone();
        let inbox = self.inbox.clone();
        let previous = self.last_save.take();
        self.last_save = Some(tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let ids: Vec<MapId> = writes.iter().map(|(id, _)| *id).collect();
            let written = tokio::task::spawn_blocking(move || {
                for (id, data) in writes {
                    let name = document(id);
                    if let Err(err) = backend.documents().write(Document::SavedData(&name), data) {
                        tuff_core::log::error!("failed to save map {id}: {err}");
                    }
                }
                if let Some(index) = index
                    && let Err(err) = backend.documents().write(Document::SavedData(INDEX), index)
                {
                    tuff_core::log::error!("failed to save the map index: {err}");
                }
            })
            .await;
            let _ = written;
            let _ = inbox.send(Message::Saved(ids));
        }));
    }

    fn evict(&mut self) {
        let saving = &self.saving;
        self.maps
            .retain(|id, live| live.dirty || !live.holders.is_empty() || saving.contains(id));
    }

    fn hold(&mut self, hold: Hold) {
        let now = self.now();
        if !self.maps.contains_key(&hold.id) {
            self.loaded(hold.id);
            return;
        }
        let dimension = self.shared.dimensions.get(hold.dimension);
        let key = dimension.key.clone();
        let ceiling = dimension.kind.has_ceiling;
        let id = hold.id;
        let live = self.maps.get_mut(&id).expect("checked above");
        let index = match live.holders.iter().position(|h| h.player == hold.player) {
            Some(index) => index,
            None => {
                live.holders.push(Holder::new(
                    hold.player,
                    hold.name.clone(),
                    Sink::Session(hold.outbound.clone()),
                    now,
                ));
                live.holders.len() - 1
            }
        };
        live.holders[index].last_seen = now;
        live.holders[index].outbound = Sink::Session(hold.outbound.clone());

        let frame = Frame::of(&live.data);
        let mut changed = false;
        let mut gone: Vec<String> = Vec::new();
        live.holders.retain(|other| {
            let keep = other.player == hold.player
                || other.last_seen + 1 >= now
                || other.framed_until >= now;
            if !keep {
                gone.push(other.name.clone());
            }
            keep
        });
        for name in gone {
            changed |= live.markers.remove(&name);
        }
        if live.data.tracking_position && live.data.dimension == key {
            if hold.hidden {
                changed |= live.markers.remove(&hold.name);
            } else {
                changed |= live.markers.place(
                    &frame,
                    MapDecorationType::Player,
                    &hold.name,
                    Spot::new(
                        hold.position[0],
                        hold.position[2],
                        f64::from(hold.yaw),
                        Some(now),
                    ),
                    None,
                );
            }
        }
        for decoration in &hold.decorations {
            if !live.markers.contains(&decoration.key) {
                changed |= live.markers.place(
                    &frame,
                    decoration.kind,
                    &decoration.key,
                    Spot::new(
                        decoration.x,
                        decoration.z,
                        f64::from(decoration.rotation),
                        Some(now),
                    ),
                    None,
                );
            }
        }
        if changed {
            live.mark_markers();
        }

        let index = live
            .holders
            .iter()
            .position(|h| h.player == hold.player)
            .expect("the holder was just kept");
        if hold.render && !live.data.locked && live.data.dimension == key {
            self.start_render(id, index, &hold, ceiling, now);
        }
        let live = self.maps.get_mut(&id).expect("still loaded");
        if let Some(index) = live.holders.iter().position(|h| h.player == hold.player)
            && !live.update(id, index)
        {
            let name = live.holders.remove(index).name;
            if live.markers.remove(&name) {
                live.mark_markers();
            }
        }
    }

    fn start_render(&mut self, id: MapId, index: usize, hold: &Hold, ceiling: bool, now: u64) {
        let live = self.maps.get_mut(&id).expect("loaded");
        let holder = &mut live.holders[index];
        holder.step = holder.step.wrapping_add(1);
        if let Some(pending) = &holder.pending {
            if now < pending.started + PENDING_TICKS {
                return;
            }
            holder.pending = None;
        }
        let data = &live.data;
        let plan = Plan::new(
            data.scale,
            [data.center_x, data.center_z],
            hold.position,
            ceiling,
            holder.step,
        );
        let mut regions: HashMap<RegionPos, Vec<(i32, i32, i32, i32)>> = HashMap::new();
        for (x, y) in plan.pixels() {
            let bx = area_origin(data.center_x, data.scale, x);
            let bz = area_origin(data.center_z, data.scale, y);
            let region = ChunkPos::of_block(BlockPos::new(bx, 0, bz)).region();
            regions.entry(region).or_default().push((x, y, bx, bz));
        }
        if regions.is_empty() {
            return;
        }
        self.next_plan += 1;
        let plan_id = self.next_plan;
        let scale = data.scale;
        let banners = data.banners.clone();
        holder.pending = Some(Pending {
            plan_id,
            plan,
            started: now,
            remaining: regions.len(),
            samples: HashMap::new(),
            stale: Vec::new(),
        });
        for (region, pixels) in regions {
            let guard = PartGuard::new(self.inbox.clone(), id, hold.player, plan_id);
            let banners = banners.clone();
            let shared = self.shared.clone();
            let dimension = hold.dimension;
            tokio::spawn(sample_region(
                shared, dimension, region, scale, pixels, banners, guard,
            ));
        }
    }

    fn part(&mut self, part: Part) {
        let Some(live) = self.maps.get_mut(&part.id) else {
            return;
        };
        let Some(index) = live.holders.iter().position(|h| h.player == part.player) else {
            return;
        };
        let holder = &mut live.holders[index];
        let Some(pending) = holder.pending.as_mut() else {
            return;
        };
        if pending.plan_id != part.plan {
            return;
        }
        pending.samples.extend(part.samples);
        for banner in part.stale {
            if !pending.stale.contains(&banner) {
                pending.stale.push(banner);
            }
        }
        pending.remaining -= 1;
        if pending.remaining > 0 {
            return;
        }
        let pending = holder.pending.take().expect("checked above");
        let samples = |x: i32, y: i32| pending.samples.get(&(x, y)).copied();
        let mut colors = std::mem::take(&mut live.data.colors);
        let changed = pending.plan.draw(&samples, &mut colors);
        live.data.colors = colors;
        for (at, _) in changed {
            live.mark_color(at % SIZE, at / SIZE);
        }
        let mut removed = false;
        for banner in pending.stale {
            if let Some(at) = live.data.banners.iter().position(|b| *b == banner) {
                live.data.banners.remove(at);
                live.markers.remove(&banner.key());
                live.dirty = true;
                removed = true;
            }
        }
        if removed {
            live.mark_markers();
        }
        let id = part.id;
        if let Some(index) = live.holders.iter().position(|h| h.player == part.player)
            && !live.update(id, index)
        {
            let name = live.holders.remove(index).name;
            if live.markers.remove(&name) {
                live.mark_markers();
            }
        }
    }

    fn framed(&mut self, framed: Framed) {
        let now = self.now();
        let players = self.shared.players.borrow().clone();
        let Some(live) = self.maps.get_mut(&framed.id) else {
            return;
        };
        let map_frame = framed.frame;
        if live.data.tracking_position {
            let mut changed = false;
            if let Some(old) = live
                .data
                .frames
                .iter()
                .find(|f| f.pos == map_frame.pos && f.entity_id != map_frame.entity_id)
            {
                changed |= live.markers.remove(&frame_key(old.entity_id));
            }
            let frame = Frame::of(&live.data);
            let key = frame_key(map_frame.entity_id);
            let room = live.markers.has_room(&key, TRACKED_LIMIT);
            changed |= room
                && live.markers.place(
                    &frame,
                    MapDecorationType::Frame,
                    &key,
                    Spot::new(
                        f64::from(map_frame.pos.x),
                        f64::from(map_frame.pos.z),
                        f64::from(map_frame.rotation),
                        Some(now),
                    ),
                    None,
                );
            match live.data.frames.iter_mut().find(|f| f.pos == map_frame.pos) {
                Some(existing) if *existing == map_frame => {}
                Some(existing) => {
                    *existing = map_frame.clone();
                    live.dirty = true;
                }
                None => {
                    live.data.frames.push(map_frame.clone());
                    live.dirty = true;
                }
            }
            if changed {
                live.mark_markers();
            }
        }
        let region = ChunkPos::of_block(map_frame.pos).region();
        for viewer in players.near(framed.dimension, region) {
            let index = match live
                .holders
                .iter()
                .position(|h| h.player == viewer.handle.id)
            {
                Some(index) => index,
                None => {
                    let mut holder = Holder::new(
                        viewer.handle.id,
                        viewer.name.to_string(),
                        Sink::Player(viewer.handle.clone()),
                        now,
                    );
                    holder.last_seen = 0;
                    live.holders.push(holder);
                    live.holders.len() - 1
                }
            };
            live.holders[index].framed_until = now + frame_view_ticks(&self.shared.config);
            if !live.update(framed.id, index) {
                live.holders.remove(index);
            }
        }
    }

    fn toggle_banner(&mut self, id: MapId, banner: MapBanner) {
        let now = self.now();
        let Some(live) = self.maps.get_mut(&id) else {
            return;
        };
        let x = f64::from(banner.pos.x) + 0.5;
        let z = f64::from(banner.pos.z) + 0.5;
        let scale = f64::from(1 << live.data.scale);
        let dx = (x - f64::from(live.data.center_x)) / scale;
        let dz = (z - f64::from(live.data.center_z)) / scale;
        if !(-63.0..=63.0).contains(&dx) || !(-63.0..=63.0).contains(&dz) {
            return;
        }
        let key = banner.key();
        if let Some(at) = live.data.banners.iter().position(|b| *b == banner) {
            live.data.banners.remove(at);
            live.markers.remove(&key);
            live.mark_markers();
            live.dirty = true;
            return;
        }
        if live.markers.over_limit(TRACKED_LIMIT) {
            return;
        }
        let frame = Frame::of(&live.data);
        live.markers.place(
            &frame,
            banner_type(banner.color),
            &key,
            Spot::new(x, z, 180.0, Some(now)),
            banner.name.clone(),
        );
        live.data.banners.retain(|b| b.key() != key);
        live.data.banners.push(banner);
        live.mark_markers();
        live.dirty = true;
    }
}

struct PartGuard {
    inbox: mpsc::UnboundedSender<Message>,
    id: MapId,
    player: PlayerId,
    plan: u64,
    done: bool,
    _release: oneshot::Sender<()>,
    release_wait: Option<oneshot::Receiver<()>>,
}

impl PartGuard {
    fn new(inbox: mpsc::UnboundedSender<Message>, id: MapId, player: PlayerId, plan: u64) -> Self {
        let (release, release_wait) = oneshot::channel();
        Self {
            inbox,
            id,
            player,
            plan,
            done: false,
            _release: release,
            release_wait: Some(release_wait),
        }
    }

    fn finish(mut self, samples: Vec<((i32, i32), Sample)>, stale: Vec<MapBanner>) {
        self.done = true;
        let _ = self.inbox.send(Message::Part(Part {
            id: self.id,
            player: self.player,
            plan: self.plan,
            samples,
            stale,
        }));
    }
}

impl Drop for PartGuard {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.inbox.send(Message::Part(Part {
                id: self.id,
                player: self.player,
                plan: self.plan,
                samples: Vec::new(),
                stale: Vec::new(),
            }));
        }
    }
}

struct RegionTerrain<'a>(&'a dyn RegionWorld);

impl Terrain for RegionTerrain<'_> {
    fn state(&self, pos: BlockPos) -> Option<BlockState> {
        self.0.block(pos)
    }

    fn surface(&self, x: i32, z: i32) -> Option<i32> {
        self.0.world_surface(x, z)
    }

    fn min_y(&self) -> i32 {
        self.0.min_y()
    }

    fn has_ceiling(&self) -> bool {
        self.0.dimension_info().kind.has_ceiling
    }
}

/// The banner at `pos` as a map would remember it.
pub fn banner_at(world: &dyn RegionWorld, pos: BlockPos) -> Option<MapBanner> {
    let state = world.block(pos)?;
    let color = banner_color(state)?;
    let name = world.saved_block_entity(pos).and_then(|nbt| {
        nbt.get("CustomName")
            .and_then(tuff_item::text::TextComponent::from_nbt)
    });
    Some(MapBanner { pos, color, name })
}

pub fn banner_color(state: BlockState) -> Option<tuff_vanilla_data::item::DyeColor> {
    let name = state.block().name().strip_prefix("minecraft:")?;
    let name = name
        .strip_suffix("_wall_banner")
        .or_else(|| name.strip_suffix(&format!("_{BANNER_BLOCK_SUFFIX}")))?;
    tuff_vanilla_data::item::DyeColor::from_name(name)
}

async fn sample_region(
    shared: Arc<Shared>,
    dimension: DimensionId,
    region: RegionPos,
    scale: u8,
    pixels: Vec<(i32, i32, i32, i32)>,
    banners: Vec<MapBanner>,
    mut guard: PartGuard,
) {
    let (reply, handle) = oneshot::channel::<RegionHandle>();
    if shared
        .manager
        .send(ManagerMessage::Acquire {
            dimension,
            region,
            reply,
        })
        .await
        .is_err()
    {
        return;
    }
    let Ok(handle) = handle.await else {
        return;
    };
    let (mut min, mut max) = (
        BlockPos::new(i32::MAX, 0, i32::MAX),
        BlockPos::new(i32::MIN, 0, i32::MIN),
    );
    let side = 1 << scale;
    for &(_, _, bx, bz) in &pixels {
        min = BlockPos::new(min.x.min(bx), 0, min.z.min(bz));
        max = BlockPos::new(max.x.max(bx + side - 1), 0, max.z.max(bz + side - 1));
    }
    let release = guard.release_wait.take();
    let task = RegionTask::new(move |world| {
        let terrain = RegionTerrain(&*world);
        let mut samples = Vec::with_capacity(pixels.len());
        for (x, y, bx, bz) in pixels {
            if let Some(found) = sample(&terrain, bx, bz, scale) {
                samples.push(((x, y), found));
            }
        }
        let mut stale = Vec::new();
        for banner in banners {
            if world.owns(banner.pos)
                && world.block(banner.pos).is_some()
                && banner_at(&*world, banner.pos).as_ref() != Some(&banner)
            {
                stale.push(banner);
            }
        }
        guard.finish(samples, stale);
    });
    if handle
        .request(RegionRequest::WhenLoaded {
            min,
            max,
            run: task,
        })
        .await
        && let Some(release) = release
    {
        let _ = release.await;
    }
    let _ = shared
        .manager
        .send(ManagerMessage::Release { dimension, region })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn a_panic_restarts_the_service_keeping_its_maps() {
        let (_clock, clock_rx) = tokio::sync::watch::channel(0u64);
        let mut shared = tuff_test_support::bare_shared();
        shared.clock = clock_rx;
        let service = MapService::new();
        service.start(&Arc::new(shared));
        let id = service
            .ask(|reply| Message::Create {
                dimension: "minecraft:overworld".to_owned(),
                x: 0.0,
                z: 0.0,
                scale: 0,
                tracking: true,
                unlimited: false,
                reply,
            })
            .await
            .unwrap();
        service.send(Message::Panic);
        let kept = service
            .ask(|reply| Message::Peek { id, reply })
            .await
            .unwrap();
        assert!(kept.is_some());
    }
}

#[cfg(test)]
mod frame_view_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_view_window_outlasts_any_report_interval() {
        let config = tuff_core::config::ConfigStore::default();
        assert_eq!(frame_view_ticks(&config), 20);
        config
            .register(tuff_core::config::Key::new(
                FRAME_REPORT_KEY,
                tuff_core::config::Value::Int(10),
                "interval",
            ))
            .unwrap();
        config
            .set(FRAME_REPORT_KEY, tuff_core::config::Value::Int(100))
            .unwrap();
        assert!(frame_view_ticks(&config) > 100);
    }
}
