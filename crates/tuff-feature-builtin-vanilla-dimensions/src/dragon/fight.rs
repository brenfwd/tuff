//! The dragon fight of a dimension, as its own actor. It owns the fight's
//! saved state and boss bar; regions and the dragon talk to it through an
//! unbounded inbox (they must never wait on it), and it publishes what the
//! dragon needs to know (crystals left, crystals destroyed) on a `watch`.
//!
//! While players are near the island it keeps the arena loaded, spawns the
//! dragon when there is none, and counts the crystals on the spikes. When
//! the dragon dies it builds the exit portal, a gateway and the egg through
//! region requests, which the regions apply once their chunks are in.

use std::collections::HashMap;
use std::sync::Arc;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use tokio::sync::{mpsc, oneshot, watch};
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::protocol::play::{self, BossAction, BossEvent};
use tuff_core::world::dimension::DimensionId;
use tuff_core::world::entity::{take, take_bool, take_number};
use tuff_core::world::explosion::{BlockInteraction, Explosion};
use tuff_core::world::messages::{
    AdvancementEvent, BlockOp, BlockOpResult, ChunkTicket, EntitySnapshot, ManagerMessage,
    PlayerEvent, PlayerHandle, PlayerId, RegionHandle, RegionRequest,
};
use tuff_core::world::predicate::EntityFacts;
use tuff_core::world::region_api::RegionWorld;
use tuff_core::world::{Shared, framed, packets};
use tuff_item::random::{LegacyRandom, Random, RandomSource};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_storage::document::Document;
pub use tuff_vanilla_common::end_spikes::podium_blocks;
use tuff_vanilla_common::end_spikes::{EndSpike, spike_blocks, spike_crystal, spikes_for_seed};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, RegionPos, regions_near};

/// The island's centre; the podium stands on its column.
pub const ORIGIN: BlockPos = BlockPos::new(0, 0, 0);
pub const DRAGON_SPAWN_Y: i32 = 128;
const DRAGON_UNSEEN_TICKS: u32 = 1200;
const CRYSTAL_SCAN_TICKS: u32 = 100;
const PLAYER_SCAN_TICKS: u32 = 20;
/// Chunks this far from the origin's stay loaded while players fight.
const RESPAWN_SAVE_TICKS: i32 = 20;
const ARENA_CHUNKS: i32 = 8;
const GATEWAY_COUNT: i32 = 20;
const GATEWAY_DISTANCE: f64 = 96.0;
const GATEWAY_Y: i32 = 75;
/// Players this close to the spawn point of the dragon see its boss bar.
const BOSS_BAR_RANGE: f64 = 192.0;
const FIGHT_DATA: &str = "minecraft:ender_dragon_fight";
/// How many crystal destructions the view remembers for the dragon.
const KEPT_DESTROYED: usize = 16;
/// The dragon's boss bar is pink with no notches, and plays the end music
/// with fog.
const BOSS_COLOR: i32 = 0;
const BOSS_OVERLAY: i32 = 0;
const BOSS_FLAGS: u8 = play::boss_flags::PLAY_MUSIC | play::boss_flags::CREATE_FOG;
/// The exit portal is not sunk below this height.
pub const PODIUM_MIN_Y: i32 = 63;
/// Where the respawn crystals aim while they summon, and the summoning
/// roars come from.
const SUMMON_POINT: BlockPos = BlockPos::new(0, 128, 0);
/// A respawn crystal stands this far out from the portal's centre.
const RESPAWN_CRYSTAL_DISTANCE: i32 = 3;
const SPIKE_CLEAR_RADIUS: i32 = 10;
const SPIKE_BLAST_POWER: f32 = 5.0;
const PILLAR_BEAM_TICKS: i32 = 40;

/// Fires once every `period` calls to [`Countdown::step`].
#[derive(Debug, Clone, Copy)]
struct Countdown {
    period: u32,
    left: u32,
}

impl Countdown {
    const fn new(period: u32) -> Self {
        Self {
            period,
            left: period,
        }
    }

    const fn expired(period: u32) -> Self {
        Self { period, left: 0 }
    }

    fn rewind(&mut self) {
        self.left = self.period;
    }

    fn step(&mut self) -> bool {
        self.left = self.left.saturating_sub(1);
        if self.left == 0 {
            self.rewind();
        }
        self.left == self.period
    }
}

/// The stages of summoning a new dragon with four crystals on the exit
/// portal, with the game's saved names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RespawnStage {
    Start,
    PreparingToSummonPillars,
    SummoningPillars,
    SummoningDragon,
    End,
}

impl RespawnStage {
    const ALL: [Self; 5] = [
        Self::Start,
        Self::PreparingToSummonPillars,
        Self::SummoningPillars,
        Self::SummoningDragon,
        Self::End,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::PreparingToSummonPillars => "preparing_to_summon_pillars",
            Self::SummoningPillars => "summoning_pillars",
            Self::SummoningDragon => "summoning_dragon",
            Self::End => "end",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }
}

/// What a region does to the end crystals of a respawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CrystalAction {
    Beam(Option<BlockPos>),
    /// No longer invulnerable and aiming nowhere.
    Release,
    /// Stops aiming, goes off without breaking blocks, and is gone.
    Detonate,
}

/// A crystal that was destroyed, for the dragon to react to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrystalDestroyed {
    /// Increases with every destruction, so the dragon sees each once.
    pub seq: u64,
    pub uuid: u128,
    pub pos: BlockPos,
    /// The player who destroyed it, by entity id and UUID.
    pub player: Option<(i32, u128)>,
}

/// What the fight tells the dragon and the regions.
#[derive(Debug, Clone, Default)]
pub struct FightView {
    pub origin: BlockPos,
    /// Crystals on the spikes at the last count.
    pub alive_crystals: i32,
    pub dragon: Option<u128>,
    pub previously_killed: bool,
    /// The crystals on the spikes and where they are, so a dragon can heal
    /// from one in another region.
    pub crystals: Vec<(u128, [f64; 3])>,
    /// Recent destructions, oldest first.
    pub destroyed: Vec<CrystalDestroyed>,
    /// `(x, z, height)` of the dragon's flight node columns: one above the
    /// highest motion-blocking block, once the arena is loaded.
    pub ground: Vec<(i32, i32, i32)>,
}

/// The dragon's state, sent every tick it lives.
#[derive(Debug, Clone)]
pub struct DragonReport {
    pub uuid: u128,
    pub health: f32,
    pub max_health: f32,
    pub position: [f64; 3],
    pub name: Option<TextComponent>,
}

#[derive(Debug)]
pub enum FightMessage {
    CrystalDestroyed {
        uuid: u128,
        pos: BlockPos,
        player: Option<(i32, u128)>,
    },
    /// An end crystal was placed; four on the exit portal summon a new
    /// dragon.
    TryRespawn,
    Dragon(DragonReport),
    /// The dragon's death animation ended.
    DragonKilled {
        uuid: u128,
    },
    /// Answers once the fight has run `tick`.
    Settled {
        tick: u64,
        reply: oneshot::Sender<()>,
    },
}

/// A region's part of building or inspecting the arena.
#[derive(Debug)]
pub enum FightRequest {
    /// Where the exit portal goes: the top of the podium column, pushed
    /// down through bedrock.
    ExitPortalLocation {
        reply: oneshot::Sender<Option<BlockPos>>,
    },
    /// Blocks of `block` in this region's part of the box.
    FindBlocks {
        block: Block,
        min: BlockPos,
        max: BlockPos,
        reply: oneshot::Sender<Vec<BlockPos>>,
    },
    /// Puts the dragon egg on top of the podium column.
    PlaceEgg { reply: oneshot::Sender<()> },
    /// Answers once this region's chunks in the box are loaded.
    Loaded {
        min: BlockPos,
        max: BlockPos,
        reply: oneshot::Sender<()>,
    },
    /// Acts on the end crystals of `uuids` this region holds.
    Crystals {
        uuids: Vec<u128>,
        action: CrystalAction,
        reply: oneshot::Sender<()>,
    },
    /// Sets off an explosion here once its chunk is in.
    Explode {
        explosion: tuff_core::world::explosion::Explosion,
        reply: oneshot::Sender<()>,
    },
    /// A level event for the viewers of `pos`'s chunk.
    LevelEvent {
        event: i32,
        pos: BlockPos,
        data: i32,
    },
    /// One above the highest motion-blocking block of each column this
    /// region owns, as `(x, z, height)`.
    Heights {
        columns: Vec<(i32, i32)>,
        reply: oneshot::Sender<Vec<(i32, i32, i32)>>,
    },
}

impl FightRequest {
    /// The region request that answers this once its blocks are loaded.
    pub fn request(self) -> RegionRequest {
        let (min, max) = match &self {
            Self::Explode { explosion, .. } => {
                let [x, y, z] = explosion.center;
                let center = BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32);
                (center, center)
            }
            Self::Crystals { .. } | Self::ExitPortalLocation { .. } | Self::PlaceEgg { .. } => {
                (ORIGIN, ORIGIN)
            }
            Self::FindBlocks { min, max, .. } | Self::Loaded { min, max, .. } => (*min, *max),
            Self::LevelEvent { pos, .. } => (*pos, *pos),
            Self::Heights { columns, .. } => {
                let min_x = columns.iter().map(|c| c.0).min().unwrap_or(0);
                let max_x = columns.iter().map(|c| c.0).max().unwrap_or(0);
                let min_z = columns.iter().map(|c| c.1).min().unwrap_or(0);
                let max_z = columns.iter().map(|c| c.1).max().unwrap_or(0);
                (
                    BlockPos::new(min_x, 0, min_z),
                    BlockPos::new(max_x, 0, max_z),
                )
            }
        };
        RegionRequest::WhenLoaded {
            min,
            max,
            run: tuff_core::world::region_api::RegionTask::new(move |world| self.answer(world)),
        }
    }

    fn answer(self, world: &mut dyn RegionWorld) {
        match self {
            Self::ExitPortalLocation { reply } => {
                let location = world.owns(ORIGIN).then(|| {
                    let min_y = world.dimension_info().shape.min_y;
                    let top = world
                        .motion_blocking_height(ORIGIN.x, ORIGIN.z)
                        .unwrap_or(min_y);
                    let mut pos = BlockPos::new(ORIGIN.x, top - 1, ORIGIN.z);
                    while world.block(pos).map(|s| s.block()) == Some(Block::Bedrock)
                        && pos.y > PODIUM_MIN_Y
                    {
                        pos.y -= 1;
                    }
                    BlockPos::new(pos.x, pos.y.max(min_y + 1), pos.z)
                });
                let _ = reply.send(location);
            }
            Self::FindBlocks {
                block,
                min,
                max,
                reply,
            } => {
                let _ = reply.send(world.find_blocks(block, min, max));
            }
            Self::PlaceEgg { reply } => {
                if world.owns(ORIGIN)
                    && let Some(top) = world.motion_blocking_height(ORIGIN.x, ORIGIN.z)
                {
                    let pos = BlockPos::new(ORIGIN.x, top, ORIGIN.z);
                    world.put_block(pos, Block::DragonEgg.default_state());
                }
                let _ = reply.send(());
            }
            Self::Loaded { reply, .. } => {
                let _ = reply.send(());
            }
            Self::Heights { columns, reply } => {
                let heights = columns
                    .into_iter()
                    .filter(|(x, z)| world.owns(BlockPos::new(*x, 0, *z)))
                    .filter_map(|(x, z)| Some((x, z, world.motion_blocking_height(x, z)?)))
                    .collect();
                let _ = reply.send(heights);
            }
            Self::Explode { explosion, reply } => {
                world.explode(explosion);
                let _ = reply.send(());
            }
            Self::Crystals {
                uuids,
                action,
                reply,
            } => {
                crystal_action(world, &uuids, action);
                let _ = reply.send(());
            }
            Self::LevelEvent { event, pos, data } => world.level_event(event, pos, data),
        }
    }
}

/// Aims, releases or sets off the end crystals of `uuids` a region holds.
fn crystal_action(world: &mut dyn RegionWorld, uuids: &[u128], action: CrystalAction) {
    use crate::dragon::crystal::{self, EndCrystal};
    let ids: Vec<i32> = world
        .entity_ids()
        .into_iter()
        .filter(|id| {
            world.entity(*id).is_some_and(|t| {
                let base = t.entity.base();
                base.kind == EntityType::EndCrystal && uuids.contains(&base.uuid)
            })
        })
        .collect();
    let mut blasts = Vec::new();
    for id in ids {
        if action == CrystalAction::Detonate {
            if let Some(tracked) = world.remove_entity(id) {
                blasts.push(tracked.entity.base().position);
            }
            continue;
        }
        let Some(crystal) = world
            .entity_mut(id)
            .and_then(|t| t.entity.downcast_mut::<EndCrystal>())
        else {
            continue;
        };
        match action {
            CrystalAction::Beam(target) => crystal.set_beam_target(target),
            CrystalAction::Release => {
                crystal.base.invulnerable = false;
                crystal.set_beam_target(None);
            }
            CrystalAction::Detonate => {}
        }
    }
    for center in blasts {
        world.explode(Explosion {
            center,
            radius: crystal::EXPLOSION_POWER,
            fire: false,
            blocks: BlockInteraction::Keep,
            damage_type: &tuff_vanilla_data::damage_type::EXPLOSION,
            igniter: None,
            culprit: None,
            dangerous_wither_skull: false,
            spares_nether_portals: false,
        });
    }
}

/// A handle to a dimension's fight.
#[derive(Debug, Clone)]
pub struct FightLink {
    messages: mpsc::UnboundedSender<FightMessage>,
    view: watch::Receiver<Arc<FightView>>,
}

impl FightLink {
    pub fn send(&self, message: FightMessage) {
        let _ = self.messages.send(message);
    }

    pub fn view(&self) -> Arc<FightView> {
        self.view.borrow().clone()
    }

    /// Waits until the fight has run `tick` (or is gone).
    pub async fn settled(&self, tick: u64) {
        let (reply, done) = oneshot::channel();
        self.send(FightMessage::Settled { tick, reply });
        let _ = done.await;
    }
}

/// The actor's ends of a fight's channels.
pub struct FightInbox {
    messages: mpsc::UnboundedReceiver<FightMessage>,
    view: watch::Sender<Arc<FightView>>,
}

/// A fight's channels. The inbox is unbounded because regions must never
/// wait on the fight; its traffic is a report a tick from one dragon plus
/// rare crystal events.
pub fn channel() -> (FightLink, FightInbox) {
    let (messages, inbox) = mpsc::unbounded_channel();
    let (view, watched) = watch::channel(Arc::new(FightView::default()));
    (
        FightLink {
            messages,
            view: watched,
        },
        FightInbox {
            messages: inbox,
            view,
        },
    )
}

/// A fight for each dimension whose type has one, by dimension index,
/// with the inboxes their actors start from.
pub fn channels(
    dimensions: &tuff_core::world::dimension::Dimensions,
) -> (Vec<Option<FightLink>>, Vec<(DimensionId, FightInbox)>) {
    let mut links = Vec::new();
    let mut inboxes = Vec::new();
    for dimension in dimensions.iter() {
        if dimension.kind.has_ender_dragon_fight {
            let (link, inbox) = channel();
            links.push(Some(link));
            inboxes.push((dimension.id, inbox));
        } else {
            links.push(None);
        }
    }
    (links, inboxes)
}

/// Starts the fight actors.
pub fn start(shared: &Arc<Shared>, inboxes: Vec<(DimensionId, FightInbox)>) {
    for (dimension, inbox) in inboxes {
        FightActor::spawn(shared.clone(), dimension, inbox);
    }
}

/// The dragon fights of a world, as a world service.
pub struct Fights {
    links: Vec<Option<FightLink>>,
    inboxes: std::sync::Mutex<Vec<(DimensionId, FightInbox)>>,
}

impl Fights {
    pub fn new(dimensions: &tuff_core::world::dimension::Dimensions) -> Self {
        let (links, inboxes) = channels(dimensions);
        Self {
            links,
            inboxes: std::sync::Mutex::new(inboxes),
        }
    }

    /// The dragon fight of `dimension`, if it has one.
    pub fn link(&self, dimension: DimensionId) -> Option<&FightLink> {
        self.links.get(dimension.index())?.as_ref()
    }
}

impl tuff_core::world::services::WorldService for Fights {
    fn start(&self, shared: &Arc<Shared>) {
        let inboxes = std::mem::take(&mut *self.inboxes.lock().expect("fight inboxes"));
        start(shared, inboxes);
    }

    fn settled(
        &self,
        tick: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            for link in self.links.iter().flatten() {
                link.settled(tick).await;
            }
        })
    }
}

/// Makes each world's [`Fights`].
pub struct FightsFactory;

impl tuff_core::world::services::ServiceFactory for FightsFactory {
    fn create(
        &self,
        dimensions: &tuff_core::world::dimension::Dimensions,
    ) -> Arc<dyn tuff_core::world::services::WorldService> {
        Arc::new(Fights::new(dimensions))
    }
}

/// Finding a dimension's dragon fight from the world's shared state.
pub trait FightsExt {
    fn fight(&self, dimension: DimensionId) -> Option<&FightLink>;
}

impl FightsExt for Shared {
    fn fight(&self, dimension: DimensionId) -> Option<&FightLink> {
        self.services.get::<Fights>()?.link(dimension)
    }
}

/// The fight's saved data.
#[derive(Debug, Clone, PartialEq)]
pub struct FightData {
    pub needs_state_scanning: bool,
    pub dragon_killed: bool,
    pub previously_killed: bool,
    pub respawn_stage: Option<RespawnStage>,
    pub respawn_time: i32,
    pub dragon_uuid: Option<u128>,
    pub exit_portal: Option<BlockPos>,
    pub gateways: Vec<i32>,
    pub respawn_crystals: Vec<u128>,
    /// Keys this server does not know, saved back as they were.
    pub extra: Compound,
}

impl Default for FightData {
    fn default() -> Self {
        Self {
            needs_state_scanning: true,
            dragon_killed: false,
            previously_killed: false,
            respawn_stage: None,
            respawn_time: 0,
            dragon_uuid: None,
            exit_portal: None,
            gateways: Vec::new(),
            respawn_crystals: Vec::new(),
            extra: Compound::new(),
        }
    }
}

fn block_pos(tag: &Tag) -> Option<BlockPos> {
    match tag {
        Tag::IntArray(v) if v.len() == 3 => Some(BlockPos::new(v[0], v[1], v[2])),
        _ => None,
    }
}

impl FightData {
    pub fn load(mut nbt: Compound) -> Self {
        let mut data = Self::default();
        if let Some(v) = take_bool(&mut nbt, "needs_state_scanning") {
            data.needs_state_scanning = v;
        }
        data.dragon_killed = take_bool(&mut nbt, "dragon_killed").unwrap_or(false);
        data.previously_killed = take_bool(&mut nbt, "previously_killed").unwrap_or(false);
        data.respawn_stage = take(&mut nbt, "respawn_stage", |t| {
            t.as_str().and_then(RespawnStage::from_name)
        });
        data.respawn_time = take_number(&mut nbt, "respawn_time").map_or(0, |v| v as i32);
        data.dragon_uuid = take(&mut nbt, "dragon_uuid", Tag::as_uuid);
        data.exit_portal = take(&mut nbt, "exit_portal_location", block_pos);
        data.gateways = take(&mut nbt, "gateways", |t| {
            t.as_list()?
                .iter()
                .map(|v| match v {
                    Tag::Int(i) => Some(*i),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
        data.respawn_crystals = take(&mut nbt, "respawn_crystals", |t| {
            t.as_list()?.iter().map(Tag::as_uuid).collect()
        })
        .unwrap_or_default();
        data.extra = nbt;
        data
    }

    /// The saved form, with the game's key order and omissions.
    pub fn save(&self) -> Compound {
        let mut out = Compound::new()
            .with("needs_state_scanning", self.needs_state_scanning)
            .with("dragon_killed", self.dragon_killed)
            .with("previously_killed", self.previously_killed);
        if let Some(stage) = self.respawn_stage {
            out.insert("respawn_stage", stage.name());
        }
        out.insert("respawn_time", self.respawn_time);
        if let Some(uuid) = self.dragon_uuid {
            out.insert("dragon_uuid", Tag::uuid(uuid));
        }
        if let Some(pos) = self.exit_portal {
            out.insert(
                "exit_portal_location",
                Tag::IntArray(vec![pos.x, pos.y, pos.z]),
            );
        }
        if !self.gateways.is_empty() {
            out.insert(
                "gateways",
                Tag::List(self.gateways.iter().map(|g| Tag::Int(*g)).collect()),
            );
        }
        if !self.respawn_crystals.is_empty() {
            out.insert(
                "respawn_crystals",
                Tag::List(
                    self.respawn_crystals
                        .iter()
                        .map(|u| Tag::uuid(*u))
                        .collect(),
                ),
            );
        }
        for (key, value) in self.extra.iter() {
            out.insert(key, value.clone());
        }
        out
    }

    /// Fills the gateway list with the twenty gateway slots in the order a
    /// world's seed shuffles them (they are used from the back).
    pub fn init_gateways(&mut self, seed: i64) {
        if !self.gateways.is_empty() {
            return;
        }
        let mut gateways: Vec<i32> = (0..GATEWAY_COUNT).collect();
        let mut random = Random::Legacy(LegacyRandom::new(seed));
        for i in (2..=gateways.len()).rev() {
            let swap = random.next_int_bounded(i as i32) as usize;
            gateways.swap(i - 1, swap);
        }
        self.gateways = gateways;
    }
}

/// Where gateway `index` (0 to 19) of the ring around the island goes.
pub fn gateway_position(index: i32) -> BlockPos {
    let angle = 2.0 * (-std::f64::consts::PI + 0.15707963267948966 * f64::from(index));
    BlockPos::new(
        (GATEWAY_DISTANCE * angle.cos()).floor() as i32,
        GATEWAY_Y,
        (GATEWAY_DISTANCE * angle.sin()).floor() as i32,
    )
}

/// The column above a spike, where the fight looks for its crystal.
pub fn spike_top(spike: &EndSpike) -> [f64; 6] {
    let r = f64::from(spike.radius);
    let (x, z) = (f64::from(spike.center_x), f64::from(spike.center_z));
    [x - r, -2032.0, z - r, x + r, 2031.0, z + r]
}

fn intersects(a: [f64; 6], b: [f64; 6]) -> bool {
    a[0] < b[3] && a[3] > b[0] && a[1] < b[4] && a[4] > b[1] && a[2] < b[5] && a[5] > b[2]
}

/// Players who see the boss bar and the ones who got it.
struct BossBar {
    uuid: u128,
    name: TextComponent,
    progress: f32,
    visible: bool,
    players: HashMap<PlayerId, PlayerHandle>,
}

impl BossBar {
    fn notify_summoned(&self, dragon: &EntityFacts) {
        for player in self.players.values() {
            player.send(PlayerEvent::Advancement(Box::new(
                AdvancementEvent::SummonedEntity(dragon.clone()),
            )));
        }
    }

    fn add_packet(&self) -> tuff_core::world::messages::Packets {
        packets([framed(&BossEvent {
            uuid: self.uuid,
            action: BossAction::Add {
                title: &self.name,
                progress: self.progress,
                color: BOSS_COLOR,
                overlay: BOSS_OVERLAY,
                flags: BOSS_FLAGS,
            },
        })])
    }

    fn remove_packet(&self) -> tuff_core::world::messages::Packets {
        packets([framed(&BossEvent {
            uuid: self.uuid,
            action: BossAction::Remove,
        })])
    }

    fn broadcast(&self, packet: tuff_core::world::messages::Packets) {
        if !self.visible {
            return;
        }
        for player in self.players.values() {
            player.send(PlayerEvent::Packets(packet.clone()));
        }
    }

    fn set_visible(&mut self, visible: bool) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        let packet = if visible {
            self.add_packet()
        } else {
            self.remove_packet()
        };
        for player in self.players.values() {
            player.send(PlayerEvent::Packets(packet.clone()));
        }
    }

    fn set_progress(&mut self, progress: f32) {
        if self.progress == progress {
            return;
        }
        self.progress = progress;
        self.broadcast(packets([framed(&BossEvent {
            uuid: self.uuid,
            action: BossAction::Progress(progress),
        })]));
    }

    fn set_name(&mut self, name: TextComponent) {
        if self.name == name {
            return;
        }
        self.name = name;
        self.broadcast(packets([framed(&BossEvent {
            uuid: self.uuid,
            action: BossAction::Title(&self.name),
        })]));
    }

    fn set_players(&mut self, now: HashMap<PlayerId, PlayerHandle>) {
        if self.visible {
            let add = self.add_packet();
            let remove = self.remove_packet();
            for (id, player) in &now {
                if !self.players.contains_key(id) {
                    player.send(PlayerEvent::Packets(add.clone()));
                }
            }
            for (id, player) in &self.players {
                if !now.contains_key(id) {
                    player.send(PlayerEvent::Packets(remove.clone()));
                }
            }
        }
        self.players = now;
    }
}

pub struct FightActor {
    shared: Arc<Shared>,
    dimension: DimensionId,
    inbox: mpsc::UnboundedReceiver<FightMessage>,
    view_tx: watch::Sender<Arc<FightView>>,
    data: FightData,
    boss: BossBar,
    spikes: Vec<EndSpike>,
    view: FightView,
    dragon_lookup: Countdown,
    crystal_census: Countdown,
    audience_refresh: Countdown,
    /// The arena regions this fight holds while players are near.
    regions: Vec<RegionHandle>,
    arena_loaded: bool,
    destroyed_seq: u64,
    /// The last tick run, and the `Settled` requests waiting for later ones.
    ticked: u64,
    settle_waiters: Vec<(u64, oneshot::Sender<()>)>,
    /// Saves land one after another in the order they were made; `saves`
    /// counts those made and `landed` those written.
    last_save: Option<tokio::task::JoinHandle<()>>,
    saves: u64,
    landed: watch::Sender<u64>,
    rng: StdRng,
}

impl FightActor {
    /// Loads the fight of `dimension` and starts its actor.
    pub fn spawn(shared: Arc<Shared>, dimension: DimensionId, inbox: FightInbox) {
        tuff_supervise::spawn("the dragon fight", async move {
            let key = shared.dimension(dimension).key.clone();
            let backend = shared.backend.clone();
            let saved = tokio::task::spawn_blocking(move || {
                backend.documents().read(Document::DimensionData {
                    dimension: &key,
                    id: FIGHT_DATA,
                })
            })
            .await;
            let data = match saved {
                Ok(Ok(root)) => FightData::load(tuff_storage::document::inner(root)),
                Ok(Err(err)) => {
                    tuff_core::log::error!("dragon fight data unreadable, starting afresh: {err}");
                    FightData::default()
                }
                Err(_) => FightData::default(),
            };
            let actor = Self::new(shared, dimension, inbox, data);
            actor.run().await;
        });
    }

    fn new(
        shared: Arc<Shared>,
        dimension: DimensionId,
        inbox: FightInbox,
        mut data: FightData,
    ) -> Self {
        data.init_gateways(shared.seed);
        let spikes = spikes_for_seed(shared.seed);
        let view = FightView {
            origin: ORIGIN,
            dragon: data.dragon_uuid,
            previously_killed: data.previously_killed,
            ..Default::default()
        };
        let mut rng = match shared.random_seed {
            Some(seed) => StdRng::seed_from_u64(seed ^ ((dimension.index() as u64) << 48) ^ 0xd4a6),
            None => StdRng::from_entropy(),
        };
        let boss = BossBar {
            uuid: rng.r#gen(),
            name: TextComponent::translate("entity.minecraft.ender_dragon", []),
            progress: 1.0,
            visible: true,
            players: HashMap::new(),
        };
        let actor = Self {
            shared,
            dimension,
            inbox: inbox.messages,
            view_tx: inbox.view,
            data,
            boss,
            spikes,
            view,
            dragon_lookup: Countdown::new(DRAGON_UNSEEN_TICKS),
            crystal_census: Countdown::new(CRYSTAL_SCAN_TICKS),
            audience_refresh: Countdown::expired(PLAYER_SCAN_TICKS),
            regions: Vec::new(),
            arena_loaded: false,
            destroyed_seq: 0,
            ticked: 0,
            settle_waiters: Vec::new(),
            last_save: None,
            saves: 0,
            landed: watch::channel(0).0,
            rng,
        };
        actor.publish();
        actor
    }

    async fn run(mut self) {
        let mut clock = self.shared.clock.clone();
        let tick_member = self.shared.tick_barrier.join(*clock.borrow());
        loop {
            tokio::select! {
                changed = clock.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let tick = *clock.borrow_and_update();
                    self.tick().await;
                    self.ticked = tick;
                    tick_member.finished(tick);
                    self.answer_settled();
                }
                message = self.inbox.recv() => match message {
                    Some(message) => self.handle(message).await,
                    None => break,
                },
            }
        }
        drop(tick_member);
        self.release_arena().await;
        if let Some(save) = self.last_save.take() {
            let _ = save.await;
        }
    }

    fn answer_settled(&mut self) {
        let ticked = self.ticked;
        let (ready, waiting) = std::mem::take(&mut self.settle_waiters)
            .into_iter()
            .partition(|(tick, _)| *tick <= ticked);
        self.settle_waiters = waiting;
        if ready.is_empty() {
            return;
        }
        let saves = self.saves;
        let mut landed = self.landed.subscribe();
        tokio::spawn(async move {
            let _ = landed.wait_for(|n| *n >= saves).await;
            for (_, reply) in ready {
                let _ = reply.send(());
            }
        });
    }

    fn publish(&self) {
        self.view_tx.send_replace(Arc::new(self.view.clone()));
    }

    fn save(&mut self) {
        let key = self.shared.dimension(self.dimension).key.clone();
        let backend = self.shared.backend.clone();
        let root = tuff_storage::document::saved(self.data.save());
        self.saves += 1;
        let seq = self.saves;
        let previous = self.last_save.take();
        let landed = self.landed.clone();
        self.last_save = Some(tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let _ = tokio::task::spawn_blocking(move || {
                let document = Document::DimensionData {
                    dimension: &key,
                    id: FIGHT_DATA,
                };
                if let Err(err) = backend.documents().write(document, root) {
                    tuff_core::log::error!("failed to save the dragon fight: {err}");
                }
            })
            .await;
            landed.send_replace(seq);
        }));
    }

    async fn handle(&mut self, message: FightMessage) {
        match message {
            FightMessage::Dragon(report) => self.update_dragon(report),
            FightMessage::DragonKilled { uuid } => self.set_dragon_killed(uuid).await,
            FightMessage::CrystalDestroyed { uuid, pos, player } => {
                if self.data.respawn_stage.is_some() && self.data.respawn_crystals.contains(&uuid) {
                    self.abort_respawn().await;
                    return;
                }
                self.update_crystal_count().await;
                self.destroyed_seq += 1;
                self.view.destroyed.push(CrystalDestroyed {
                    seq: self.destroyed_seq,
                    uuid,
                    pos,
                    player,
                });
                let excess = self.view.destroyed.len().saturating_sub(KEPT_DESTROYED);
                self.view.destroyed.drain(..excess);
                self.publish();
            }
            FightMessage::TryRespawn => self.try_respawn().await,
            FightMessage::Settled { tick, reply } => {
                self.settle_waiters.push((tick, reply));
                self.answer_settled();
            }
        }
    }

    async fn tick(&mut self) {
        self.boss.set_visible(!self.data.dragon_killed);
        if self.audience_refresh.step() {
            self.update_players();
        }
        if self.boss.players.is_empty() {
            self.release_arena().await;
            return;
        }
        if !self.hold_arena().await {
            return;
        }
        if self.data.needs_state_scanning {
            self.scan_state().await;
            self.data.needs_state_scanning = false;
            self.save();
        }
        if self.advance_summoning().await.is_break() {
            return;
        }
        if self.data.dragon_killed {
            return;
        }
        let dragon_known = self.data.dragon_uuid.is_some();
        if self.dragon_lookup.step() || !dragon_known {
            self.find_or_create_dragon().await;
            self.dragon_lookup.rewind();
        }
        if self.crystal_census.step() {
            self.update_crystal_count().await;
        }
    }

    /// Runs one tick of a summoning in progress; breaks when it was
    /// called off because its crystals are gone.
    async fn advance_summoning(&mut self) -> std::ops::ControlFlow<()> {
        let Some(stage) = self.data.respawn_stage else {
            return std::ops::ControlFlow::Continue(());
        };
        let crystals = self.respawn_crystals().await;
        if crystals.is_empty() {
            self.abort_respawn().await;
            return std::ops::ControlFlow::Break(());
        }
        let clock = self.data.respawn_time;
        self.data.respawn_time = clock + 1;
        if self.data.respawn_time % RESPAWN_SAVE_TICKS == 0 {
            self.save();
        }
        self.respawn_tick(stage, &crystals, clock).await;
        std::ops::ControlFlow::Continue(())
    }

    /// The players alive within range of the dragon's spawn point.
    fn update_players(&mut self) {
        let center = [
            f64::from(ORIGIN.x),
            f64::from(ORIGIN.y + DRAGON_SPAWN_Y),
            f64::from(ORIGIN.z),
        ];
        let index = self.shared.players.borrow().clone();
        let mut now = HashMap::new();
        for region in regions_near(center, BOSS_BAR_RANGE) {
            for player in index.near(self.dimension, region) {
                if tuff_core::world::distance_sq(player.position, center) <= BOSS_BAR_RANGE.powi(2)
                {
                    now.insert(player.handle.id, player.handle.clone());
                }
            }
        }
        self.boss.set_players(now);
    }

    fn arena_regions() -> Vec<RegionPos> {
        let origin = ChunkPos::of_block(ORIGIN);
        let low = ChunkPos::new(origin.x - ARENA_CHUNKS, origin.z - ARENA_CHUNKS).region();
        let high = ChunkPos::new(origin.x + ARENA_CHUNKS, origin.z + ARENA_CHUNKS).region();
        (low.x..=high.x)
            .flat_map(|x| (low.z..=high.z).map(move |z| RegionPos::new(x, z)))
            .collect()
    }

    fn arena_chunks() -> Vec<ChunkPos> {
        let origin = ChunkPos::of_block(ORIGIN);
        let mut chunks = Vec::new();
        for x in -ARENA_CHUNKS..=ARENA_CHUNKS {
            for z in -ARENA_CHUNKS..=ARENA_CHUNKS {
                chunks.push(ChunkPos::new(origin.x + x, origin.z + z));
            }
        }
        chunks
    }

    /// Keeps the arena loaded and its regions at hand; true once its
    /// chunks are in.
    async fn hold_arena(&mut self) -> bool {
        if self.regions.is_empty() {
            let (reply, done) = oneshot::channel();
            let force = ManagerMessage::ForceLoad {
                dimension: self.dimension,
                chunks: Self::arena_chunks(),
                ticket: ChunkTicket::DragonFight,
                on: true,
                reply,
            };
            if self.shared.manager.send(force).await.is_err() {
                return false;
            }
            let _ = done.await;
            for region in Self::arena_regions() {
                let (reply, handle) = oneshot::channel();
                let acquire = ManagerMessage::Acquire {
                    dimension: self.dimension,
                    region,
                    reply,
                };
                if self.shared.manager.send(acquire).await.is_err() {
                    return false;
                }
                if let Ok(handle) = handle.await {
                    self.regions.push(handle);
                }
            }
        }
        if !self.arena_loaded {
            let (min, max) = (
                BlockPos::new(-ARENA_CHUNKS * 16, 0, -ARENA_CHUNKS * 16),
                BlockPos::new(ARENA_CHUNKS * 16 + 15, 0, ARENA_CHUNKS * 16 + 15),
            );
            for region in self.regions.clone() {
                let (reply, done) = oneshot::channel();
                region
                    .request(FightRequest::Loaded { min, max, reply }.request())
                    .await;
                let _ = done.await;
            }
            self.arena_loaded = true;
            self.measure_ground().await;
        }
        true
    }

    /// The ground under the dragon's flight nodes, for its graph.
    async fn measure_ground(&mut self) {
        let columns: Vec<(i32, i32)> = (0..super::phase::NODE_COUNT)
            .map(|i| {
                let (x, z, _) = super::phase::node_column(i);
                (x, z)
            })
            .collect();
        let mut ground = Vec::new();
        for region in self.regions.clone() {
            let (reply, answer) = oneshot::channel();
            region
                .request(
                    FightRequest::Heights {
                        columns: columns.clone(),
                        reply,
                    }
                    .request(),
                )
                .await;
            ground.extend(answer.await.unwrap_or_default());
        }
        self.view.ground = ground;
        self.publish();
    }

    async fn release_arena(&mut self) {
        if self.regions.is_empty() {
            return;
        }
        let (reply, done) = oneshot::channel();
        let unforce = ManagerMessage::ForceLoad {
            dimension: self.dimension,
            chunks: Self::arena_chunks(),
            ticket: ChunkTicket::DragonFight,
            on: false,
            reply,
        };
        if self.shared.manager.send(unforce).await.is_ok() {
            let _ = done.await;
        }
        for region in std::mem::take(&mut self.regions) {
            let _ = self
                .shared
                .manager
                .send(ManagerMessage::Release {
                    dimension: self.dimension,
                    region: region.pos,
                })
                .await;
        }
        self.arena_loaded = false;
    }

    async fn region(&self, pos: RegionPos) -> Option<RegionHandle> {
        if let Some(handle) = self.regions.iter().find(|r| r.pos == pos) {
            return Some(handle.clone());
        }
        let (reply, handle) = oneshot::channel();
        self.shared
            .manager
            .send(ManagerMessage::Acquire {
                dimension: self.dimension,
                region: pos,
                reply,
            })
            .await
            .ok()?;
        let handle = handle.await.ok()?;
        let _ = self
            .shared
            .manager
            .send(ManagerMessage::Release {
                dimension: self.dimension,
                region: pos,
            })
            .await;
        Some(handle)
    }

    /// The arena's regions: the held ones, or borrowed while nobody is near.
    async fn arena(&self) -> Vec<RegionHandle> {
        if !self.regions.is_empty() {
            return self.regions.clone();
        }
        let mut handles = Vec::new();
        for pos in Self::arena_regions() {
            if let Some(handle) = self.region(pos).await {
                handles.push(handle);
            }
        }
        handles
    }

    async fn entities(&self) -> Vec<EntitySnapshot> {
        let mut all = Vec::new();
        for region in &self.arena().await {
            let (reply, answer) = oneshot::channel();
            region.request(RegionRequest::Entities { reply }).await;
            if let Ok(found) = answer.await {
                all.extend(found);
            }
        }
        all
    }

    async fn dragons(&self) -> Vec<EntitySnapshot> {
        let mut dragons: Vec<EntitySnapshot> = self
            .entities()
            .await
            .into_iter()
            .filter(|e| e.kind == EntityType::EnderDragon)
            .collect();
        dragons.sort_by_key(|d| d.id);
        dragons
    }

    /// What a world that never ran this fight holds: an exit portal from an
    /// earlier kill, a dragon still alive, or neither.
    /// Blocks of `block` in the box, across the arena's regions.
    async fn find_blocks(&self, block: Block, min: BlockPos, max: BlockPos) -> Vec<BlockPos> {
        let mut found = Vec::new();
        for region in self.arena().await {
            let (reply, answer) = oneshot::channel();
            region
                .request(
                    FightRequest::FindBlocks {
                        block,
                        min,
                        max,
                        reply,
                    }
                    .request(),
                )
                .await;
            found.extend(answer.await.unwrap_or_default());
        }
        found
    }

    /// Looks for an active exit portal in the arena and remembers where it
    /// is if nothing was saved.
    async fn find_exit_portal(&mut self) -> bool {
        let (min, max) = (
            BlockPos::new(-ARENA_CHUNKS * 16, i32::MIN, -ARENA_CHUNKS * 16),
            BlockPos::new(ARENA_CHUNKS * 16 + 15, i32::MAX, ARENA_CHUNKS * 16 + 15),
        );
        let portal_blocks = self.find_blocks(Block::EndPortal, min, max).await;
        if self.data.exit_portal.is_none() {
            let lowest = match portal_blocks.iter().map(|p| p.y).min() {
                Some(y) => Some(y),
                None => self.find_inactive_podium_y(min, max).await,
            };
            if let Some(y) = lowest {
                self.data.exit_portal = Some(BlockPos::new(ORIGIN.x, y, ORIGIN.z));
                self.save();
            }
        }
        !portal_blocks.is_empty()
    }

    /// The height of an inactive podium: the bedrock pillar at the origin
    /// column topped by torches, with the bedrock rim around its floor.
    async fn find_inactive_podium_y(&self, min: BlockPos, max: BlockPos) -> Option<i32> {
        let bedrock = self.find_blocks(Block::Bedrock, min, max).await;
        let set: std::collections::HashSet<BlockPos> = bedrock.into_iter().collect();
        let mut ys: Vec<i32> = set
            .iter()
            .filter(|p| p.x == ORIGIN.x && p.z == ORIGIN.z)
            .map(|p| p.y)
            .collect();
        ys.sort_unstable();
        ys.into_iter().find(|&y| {
            (0..4).all(|d| set.contains(&BlockPos::new(ORIGIN.x, y + d, ORIGIN.z)))
                && set.contains(&BlockPos::new(ORIGIN.x, y - 1, ORIGIN.z))
                && set.contains(&BlockPos::new(ORIGIN.x + 3, y, ORIGIN.z))
                && set.contains(&BlockPos::new(ORIGIN.x - 3, y, ORIGIN.z))
                && set.contains(&BlockPos::new(ORIGIN.x, y, ORIGIN.z + 3))
                && set.contains(&BlockPos::new(ORIGIN.x, y, ORIGIN.z - 3))
        })
    }

    async fn scan_state(&mut self) {
        let active_portal = self.find_exit_portal().await;
        if active_portal {
            self.data.previously_killed = true;
        } else {
            self.data.previously_killed = false;
            self.spawn_exit_portal(false).await;
        }
        let dragons = self.dragons().await;
        match dragons.first() {
            None => self.data.dragon_killed = true,
            Some(dragon) => {
                self.data.dragon_uuid = Some(dragon.uuid);
                self.data.dragon_killed = false;
                if !active_portal {
                    self.kill_entities(dragon.id).await;
                    self.data.dragon_uuid = None;
                }
            }
        }
        if !self.data.previously_killed && self.data.dragon_killed {
            self.data.dragon_killed = false;
        }
        self.view.previously_killed = self.data.previously_killed;
        self.view.dragon = self.data.dragon_uuid;
        self.publish();
    }

    async fn kill_entities(&self, id: i32) {
        for region in &self.arena().await {
            let _ = region.ask(move |world| world.kill_entity(id)).await;
        }
    }

    async fn find_or_create_dragon(&mut self) {
        match self.dragons().await.first() {
            None => {
                self.create_dragon().await;
            }
            Some(dragon) => {
                self.data.dragon_uuid = Some(dragon.uuid);
                self.view.dragon = self.data.dragon_uuid;
                self.publish();
                self.save();
            }
        }
    }

    async fn create_dragon(&mut self) -> Option<EntityFacts> {
        let id = self
            .shared
            .next_entity_ids(super::entity::PART_COUNT as i32 + 1);
        let uuid: u128 = self.rng.r#gen();
        let yaw = self.rng.r#gen::<f32>() * 360.0;
        let position = [
            f64::from(ORIGIN.x),
            f64::from(ORIGIN.y + DRAGON_SPAWN_Y),
            f64::from(ORIGIN.z),
        ];
        let dragon = super::entity::EnderDragon::spawned(id, uuid, position, yaw, ORIGIN);
        let region = ChunkPos::of_position(position).region();
        let handle = self.region(region).await?;
        handle
            .request(RegionRequest::SpawnEntity(Box::new(dragon)))
            .await;
        self.data.dragon_uuid = Some(uuid);
        self.view.dragon = Some(uuid);
        self.publish();
        self.save();
        Some(EntityFacts {
            kind: Some(EntityType::EnderDragon),
            entity_id: Some(id),
            position,
            dimension: "minecraft:the_end".to_owned(),
            ..EntityFacts::default()
        })
    }

    fn notify_summoned(&self, dragon: &EntityFacts) {
        self.boss.notify_summoned(dragon);
    }

    fn update_dragon(&mut self, report: DragonReport) {
        if Some(report.uuid) != self.data.dragon_uuid {
            return;
        }
        let progress = if report.max_health > 0.0 {
            report.health / report.max_health
        } else {
            0.0
        };
        self.boss.set_progress(progress);
        self.dragon_lookup.rewind();
        if let Some(name) = report.name {
            self.boss.set_name(name);
        }
    }

    async fn set_dragon_killed(&mut self, uuid: u128) {
        if Some(uuid) != self.data.dragon_uuid {
            return;
        }
        self.boss.set_progress(0.0);
        self.boss.set_visible(false);
        self.spawn_exit_portal(true).await;
        self.spawn_new_gateway().await;
        if !self.data.previously_killed {
            self.place_egg().await;
        }
        self.data.previously_killed = true;
        self.data.dragon_killed = true;
        self.view.previously_killed = true;
        self.view.dragon = None;
        self.publish();
        self.save();
    }

    /// Sets blocks in whichever regions own them.
    async fn set_blocks(&self, changes: Vec<(BlockPos, BlockState)>, destroy: bool) {
        let mut by_region: HashMap<RegionPos, Vec<(BlockPos, BlockState)>> = HashMap::new();
        for (pos, state) in changes {
            by_region
                .entry(ChunkPos::of_block(pos).region())
                .or_default()
                .push((pos, state));
        }
        let mut regions: Vec<RegionPos> = by_region.keys().copied().collect();
        regions.sort_by_key(|r| (r.x, r.z));
        for pos in regions {
            let Some(handle) = self.region(pos).await else {
                continue;
            };
            let changes = by_region.remove(&pos).unwrap_or_default();
            let (reply, done) = oneshot::channel();
            handle
                .request(RegionRequest::Blocks {
                    op: BlockOp::Set {
                        changes,
                        destroy,
                        keep: false,
                        strict: false,
                        quiet: false,
                        data: None,
                    },
                    reply,
                })
                .await;
            let _: Result<BlockOpResult, _> = done.await;
        }
    }
}

mod respawn;

#[cfg(test)]
#[path = "fight_tests.rs"]
mod tests;
