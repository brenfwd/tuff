use tokio::sync::{mpsc, oneshot};
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{BlockState, Item};

use crate::world::dimension::DimensionId;
use crate::world::entity::RegionEntity;
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, RegionPos};

use super::*;

/// What a client's copy of an entity collides with, when that differs from
/// the plain box its kind and position give.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Shape {
    pub non_solid: bool,
    /// The box relative to the entity's position, as of the last report.
    pub offset: Option<[f64; 6]>,
}

/// An entity's current state for players who see its chunk. `spawn`
/// recreates it from nothing; `update` brings an existing copy up to date.
#[derive(Debug, Clone)]
pub struct Track {
    pub entity: i32,
    pub kind: EntityType,
    pub dimension: DimensionId,
    pub chunk: ChunkPos,
    pub seq: u64,
    pub position: [f64; 3],
    /// How far, in blocks, viewers may be to see it (before the broadcast
    /// range percentage and the viewer's view distance cap it).
    pub range: i32,
    pub spawn: Packets,
    pub update: Packets,
    /// The head rotation viewers need on a resync; spawn packets carry it already.
    pub head: Option<bytes::Bytes>,
    /// Only what changed since the report with the previous `seq`, for
    /// viewers that applied it; others get `update`.
    pub moves: Option<Packets>,
    pub player: Option<PlayerHandle>,
    /// A spectating player, visible only to other spectators.
    pub spectator: bool,
    pub shape: Option<Shape>,
}

#[derive(Debug, Clone)]
pub struct RegionHandle {
    pub dimension: DimensionId,
    pub pos: RegionPos,
    requests: mpsc::Sender<RegionRequest>,
    internal: mpsc::UnboundedSender<RegionInternal>,
}

impl RegionHandle {
    pub fn new(
        dimension: DimensionId,
        pos: RegionPos,
        request_capacity: usize,
    ) -> (
        Self,
        mpsc::Receiver<RegionRequest>,
        mpsc::UnboundedReceiver<RegionInternal>,
    ) {
        let (requests, requests_rx) = mpsc::channel(request_capacity);
        let (internal, internal_rx) = mpsc::unbounded_channel();
        (
            Self {
                dimension,
                pos,
                requests,
                internal,
            },
            requests_rx,
            internal_rx,
        )
    }

    /// Waits for queue space; returns false if the region has closed.
    pub async fn request(&self, request: RegionRequest) -> bool {
        self.requests.send(request).await.is_ok()
    }

    /// Like [`Self::request`], but hands the request back if the region
    /// has closed, so nothing it carries is lost.
    pub async fn try_request(&self, request: RegionRequest) -> Result<(), RegionRequest> {
        self.requests.send(request).await.map_err(|error| error.0)
    }

    /// Runs `run` on the region and answers what it returned; `None` if
    /// the region has closed.
    pub async fn ask<T: Send + 'static>(
        &self,
        run: impl FnOnce(&mut dyn crate::world::region_api::RegionWorld) -> T + Send + 'static,
    ) -> Option<T> {
        let (reply, answer) = oneshot::channel();
        let task = crate::world::region_api::RegionTask::new(move |world| {
            let _ = reply.send(run(world));
        });
        if !self.request(RegionRequest::Run(task)).await {
            return None;
        }
        answer.await.ok()
    }

    pub fn internal(&self, message: RegionInternal) -> bool {
        self.internal.send(message).is_ok()
    }

    /// Like [`Self::internal`], but hands the message back when the region
    /// is gone, so the stacks it carries go back to their sender.
    pub fn try_internal(&self, message: RegionInternal) -> Result<(), Box<RegionInternal>> {
        self.internal
            .send(message)
            .map_err(|error| Box::new(error.0))
    }

    /// Like [`Self::internal`], but hands the message back if the region is
    /// gone, so nothing it carries is lost.
    pub fn try_entity(
        &self,
        entity: Box<dyn RegionEntity>,
        seq: u64,
    ) -> Result<(), Box<dyn RegionEntity>> {
        self.internal
            .send(RegionInternal::Entity { entity, seq })
            .map_err(|error| match error.0 {
                RegionInternal::Entity { entity, .. } => entity,
                _ => unreachable!("the message just sent was an entity"),
            })
    }

    pub fn same(&self, other: &RegionHandle) -> bool {
        self.requests.same_channel(&other.requests)
    }
}

/// A player's state as seen by the regions it is in or near.
#[derive(Clone, Debug)]
pub struct PlayerUpdate {
    pub handle: PlayerHandle,
    pub uuid: u128,
    pub name: String,
    pub seq: u64,
    /// Where the player was when it last published; block contact is swept
    /// from there.
    pub swept_from: [f64; 3],
    pub position: [f64; 3],
    pub chunk: ChunkPos,
    pub alive: bool,
    /// Spectators are hidden from non-spectators and neither pick up items
    /// nor block placement.
    pub spectator: bool,
    /// Operators (gamemaster level) are exempt from spawn protection.
    pub gamemaster: bool,
    /// How much less explosions push the player (Blast Protection).
    pub explosion_knockback_resistance: f64,
    /// A flying creative player: explosions do not push it.
    pub creative_flying: bool,
    /// Standing on the ground, alive and not riding.
    pub grounded: bool,
    /// What the player wears on its feet.
    pub feet: Option<tuff_item::item::ItemSnapshot>,
    /// It landed with this update, after falling this far.
    pub landed: Option<f64>,
    /// What mobs see of the player.
    pub looks: PlayerLooks,
    pub spawn: Packets,
    pub update: Packets,
    /// The head rotation viewers need on a resync; spawn packets carry it already.
    pub head: Option<bytes::Bytes>,
    /// What changed since the previous `seq`, for viewers that applied it.
    pub moves: Option<Packets>,
}

/// What mobs see of a player: where it looks and what it holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerLooks {
    pub yaw: f32,
    pub pitch: f32,
    pub eye_height: f64,
    /// The box height, which follows the pose.
    pub height: f64,
    /// The items in the main and off hand.
    pub held: [Option<Item>; 2],
    pub creative: bool,
    pub sneaking: bool,
    pub invisible: bool,
    /// Mobs do not attack it (creative mode).
    pub invulnerable: bool,
    /// Feet, legs, chest and head.
    pub armor: [Option<Item>; 4],
    /// Spinning from a Riptide launch: what it runs into gets hit.
    pub spin_attack: bool,
    /// Its active effects, one bit per registry id.
    pub effects: u64,
    /// How far its client last moved it in one tick (zero once it stops).
    pub movement: [f64; 3],
    /// Ticks it has lain in bed (capped at 100) while asleep.
    pub sleeping: Option<u32>,
    /// Its warden spawn tracker's warning level and whether a warning
    /// is on cooldown.
    pub warden_warning: u8,
    pub warden_cooldown: bool,
    /// Who hurt it lately, if wolves answer that damage: entity id and
    /// the session tick it happened.
    pub last_hurt_by: Option<(i32, u64)>,
    /// The entity it attacked last, and when.
    pub last_hurt_mob: Option<(i32, u64)>,
    /// A tamed parrot may land on its shoulder: not flying, swimming or
    /// stuck in powder snow.
    pub perchable: bool,
    /// Running, which scares an armadillo.
    pub sprinting: bool,
}

const _: () = assert!(
    <tuff_vanilla_data::registry::MobEffect as tuff_vanilla_data::Registry>::ALL.len() <= 64
);

impl PlayerLooks {
    pub fn effect_bits(
        effects: impl IntoIterator<Item = tuff_vanilla_data::registry::MobEffect>,
    ) -> u64 {
        effects
            .into_iter()
            .fold(0, |bits, effect| bits | 1u64 << effect.id())
    }

    pub fn has_effect(&self, effect: tuff_vanilla_data::registry::MobEffect) -> bool {
        self.effects & 1u64 << effect.id() != 0
    }
}

impl Default for PlayerLooks {
    fn default() -> Self {
        Self {
            yaw: 0.0,
            pitch: 0.0,
            eye_height: 1.62,
            height: 1.8,
            held: [None; 2],
            creative: false,
            sneaking: false,
            invisible: false,
            invulnerable: false,
            armor: [None; 4],
            spin_attack: false,
            effects: 0,
            movement: [0.0; 3],
            sleeping: None,
            warden_warning: 0,
            warden_cooldown: false,
            last_hurt_by: None,
            last_hurt_mob: None,
            perchable: false,
            sprinting: false,
        }
    }
}

/// A non-player entity that appears on locator bars.
#[derive(Debug, Clone, PartialEq)]
pub struct WaypointTransmitter {
    pub id: i32,
    pub uuid: u128,
    pub dimension: DimensionId,
    pub position: [f64; 3],
    pub range: f64,
    /// Everyone riding it, directly or through other mounts.
    pub indirect_passengers: Vec<i32>,
}

#[derive(Debug)]
pub enum RegionRequest {
    Subscribe {
        player: PlayerHandle,
        chunks: Vec<ChunkPos>,
    },
    /// Like `Subscribe`, but a chunk storage does not hold is not made:
    /// the player gets it only if it is or later becomes loaded.
    Peek {
        player: PlayerHandle,
        chunks: Vec<ChunkPos>,
    },
    /// Drops `player`'s subscriptions and lit-only holds on `chunks`.
    Unsubscribe {
        player: PlayerId,
        chunks: Vec<ChunkPos>,
    },
    /// Keeps `chunks` loaded and lit for `player` without sending them: the
    /// ring just outside a view, so view-edge chunks are sent with their
    /// final light. A later `Subscribe` turns a hold into a subscription.
    HoldLit {
        player: PlayerId,
        chunks: Vec<ChunkPos>,
    },
    Player(PlayerUpdate),
    /// Tells an entity of the region something (a hit it caused landed).
    Notify {
        target: i32,
        notice: crate::world::entity::Notice,
    },
    /// The player's entity is gone (left or respawning): viewers drop it.
    PlayerRemoved {
        player: PlayerId,
        seq: u64,
    },
    /// The player moved out of range of this region's pickup and placement
    /// checks.
    PresenceGone {
        player: PlayerId,
    },
    Break {
        player: PlayerHandle,
        pos: BlockPos,
        tool: Option<tuff_item::item::ItemSnapshot>,
        mode: BreakMode,
        sequence: i32,
    },
    /// A break by a player who may not build (adventure mode): allowed
    /// only when the tool's `can_break` predicate matches the block.
    BreakRestricted {
        player: PlayerHandle,
        pos: BlockPos,
        tool: Option<tuff_item::item::ItemSnapshot>,
        mode: BreakMode,
        sequence: i32,
    },
    Place(Place),
    /// A lily pad or frogspawn used in the air: the region casts the view
    /// ray to the water source surface and places it above what it hits.
    PlaceOnWater(Place),
    /// One tick of `player`'s survival break: shows the Level Event to the
    /// block's viewers, or ends the break if the block is gone.
    DestroyProgress(Box<DestroyTick>),
    /// `player` stopped mining `pos` without breaking it: clears the crack
    /// others see.
    DestroyAborted {
        player: PlayerId,
        pos: BlockPos,
    },
    /// A burning player stands in the powder snow at `pos`, which melts
    /// away under it.
    MeltPowderSnow {
        pos: BlockPos,
    },
    /// A burning player stands in the water or powder snow cauldron at
    /// `pos`, which loses a level.
    LowerCauldron {
        pos: BlockPos,
    },
    Resend {
        player: PlayerHandle,
        pos: BlockPos,
        sequence: i32,
    },
    /// The sign editor closed: `player` wants the lines on one side of the
    /// sign at `pos`. Only the player the editor was opened for may change
    /// them, from where `eye` is.
    UpdateSign {
        player: PlayerHandle,
        pos: BlockPos,
        front: bool,
        lines: Vec<String>,
        /// The text filter's view of `lines`, when it changed any of them.
        filtered: Option<Vec<String>>,
        eye: [f64; 3],
    },
    /// The block at `pos`, or `None` if it is not loaded here.
    BlockAt {
        pos: BlockPos,
        reply: oneshot::Sender<Option<BlockState>>,
    },
    /// A debug stick cycled the block at `pos` to `state`.
    DebugStickSet {
        pos: BlockPos,
        state: BlockState,
    },
    /// The block at `pos` with its block entity as saved, if this region
    /// owns `pos` and the chunk is loaded (pick block with data).
    BlockWithEntity {
        pos: BlockPos,
        reply: oneshot::Sender<Option<(BlockState, Option<tuff_nbt::Compound>)>>,
    },
    /// A vanilla-style random teleport target (chorus fruit) within
    /// `diameter` blocks of `origin`, or `None` if no attempt landed
    /// somewhere safe. `width`/`height` are the traveller's bounding box.
    RandomTeleport {
        origin: [f64; 3],
        diameter: f32,
        width: f64,
        height: f64,
        reply: oneshot::Sender<Option<[f64; 3]>>,
    },
    SpawnItem {
        stack: ItemStack,
        position: [f64; 3],
        velocity: [f64; 3],
        pickup_delay: u32,
    },
    /// A dropped item that cannot be picked up and despawns on its first
    /// tick (the visual of a full `/give`).
    SpawnFakeItem {
        stack: tuff_item::item::ItemSnapshot,
        position: [f64; 3],
    },
    /// Adds a new entity (a player's arrow or thrown item).
    SpawnEntity(Box<dyn RegionEntity>),
    /// Drops `amount` experience points as orbs at `position`.
    SpawnExperience {
        position: [f64; 3],
        amount: i32,
    },
    /// Packets for everyone viewing `chunk` (and, with `entity`, able to see
    /// that entity), except `except`.
    Broadcast {
        chunk: ChunkPos,
        entity: Option<i32>,
        packets: Packets,
        except: Option<PlayerId>,
    },
    /// A click on an open block container: applied if every `expected`
    /// matches, answered with [`PlayerEvent::Transaction`]. `took_result`
    /// says the click took from a result slot, which collects a furnace's
    /// experience.
    ContainerTransaction {
        player: PlayerHandle,
        window_id: i32,
        changes: Vec<ContainerChange>,
        took_result: bool,
    },
    /// The first phase of a transaction spanning two regions: validate
    /// `changes` like [`Self::ContainerTransaction`] and lock their slots,
    /// answering with [`PlayerEvent::TransactionPrepared`].
    PrepareTransaction {
        player: PlayerHandle,
        window_id: i32,
        txn: u64,
        changes: Vec<ContainerChange>,
    },
    /// The second phase: apply (`commit`) or drop the prepared changes.
    ResolveTransaction {
        player: PlayerId,
        txn: u64,
        commit: bool,
    },
    /// Sends a viewer the full contents of the container at `pos`, after
    /// any update already sent to it.
    RefreshContainer {
        player: PlayerHandle,
        pos: BlockPos,
    },
    /// A command block's command finished running.
    CommandBlockDone {
        pos: BlockPos,
        success: u32,
        output: Option<TextComponent>,
    },
    /// The command block editor's Done: a creative operator's edit.
    SetCommandBlock {
        player: PlayerHandle,
        pos: BlockPos,
        edit: CommandBlockEdit,
    },
    /// An anvil's result was taken: it wears down one stage if `damage`,
    /// and the use is heard.
    AnvilUsed {
        pos: BlockPos,
        damage: bool,
    },
    /// A grindstone's result was taken: its experience drops and the use
    /// is heard.
    GrindstoneUsed {
        pos: BlockPos,
        experience: i32,
    },
    /// A viewer switched a crafter slot on or off.
    CrafterSlotState {
        pos: BlockPos,
        slot: usize,
        enabled: bool,
    },
    /// A button press in an open lectern menu: 1 and 2 turn the page back
    /// and forward, 3 takes the book, 100 and up jump to a page.
    LecternButton {
        player: PlayerHandle,
        pos: BlockPos,
        button: i32,
        may_build: bool,
    },
    /// The effects chosen on a beacon menu, kept when they fit the
    /// beacon's pyramid.
    SetBeaconEffects {
        pos: BlockPos,
        primary: Option<tuff_vanilla_data::registry::MobEffect>,
        secondary: Option<tuff_vanilla_data::registry::MobEffect>,
    },
    /// The player no longer views the containers at `positions`.
    CloseContainer {
        player: PlayerId,
        positions: Vec<BlockPos>,
    },
    /// A click on an open entity container, answered with
    /// [`PlayerEvent::EntityTransaction`] the way block transactions are.
    EntityTransaction {
        player: PlayerHandle,
        window_id: i32,
        entity: i32,
        changes: Vec<EntitySlotChange>,
    },
    /// The player no longer views the container of `entity`.
    CloseEntityContainer {
        player: PlayerId,
        entity: i32,
    },
    /// A command's block read or write in this region.
    Blocks {
        op: BlockOp,
        reply: oneshot::Sender<BlockOpResult>,
    },
    /// The living entities of this region that show on locator bars.
    Transmitters {
        reply: oneshot::Sender<Vec<WaypointTransmitter>>,
    },
    /// Which of these stored chunks this region holds in memory, for the
    /// SQL console to refuse edits a save would overwrite.
    LoadedAmong {
        chunks: std::sync::Arc<[tuff_storage::sqlite::console::ChunkKey]>,
        reply: oneshot::Sender<Vec<tuff_storage::sqlite::console::ChunkKey>>,
    },
    /// Every entity this region owns, players aside.
    Entities {
        reply: oneshot::Sender<Vec<EntitySnapshot>>,
    },
    /// [`Self::Entities`] with each entity's saved form, for selectors
    /// testing `nbt`.
    EntitiesWithData {
        reply: oneshot::Sender<Vec<EntitySnapshot>>,
    },
    /// One entity's saved data, as it would be stored now.
    EntityData {
        id: i32,
        reply: oneshot::Sender<Option<tuff_nbt::Compound>>,
    },
    /// Answers once the region has run `tick` and every chunk load, light
    /// and I/O job it started has landed: a barrier for whoever steps the
    /// clock one tick at a time.
    Settled {
        tick: u64,
        reply: oneshot::Sender<()>,
    },
    /// Spawns an entity from its saved form (`id` included) at
    /// `position`; answers false if its type cannot be loaded. A `fresh`
    /// one (summoned without data) is set up like a newly spawned mob.
    Summon {
        nbt: Compound,
        position: [f64; 3],
        fresh: bool,
        reply: oneshot::Sender<bool>,
    },
    /// A player's melee attack on a region entity; a hit that lands is
    /// answered with [`PlayerEvent::AttackLanded`].
    AttackEntity {
        attacker: PlayerHandle,
        target: i32,
        attack: crate::world::entity::EntityAttack,
        sweep: Option<Sweep>,
    },
    /// A spear stab takes a region entity off whatever it rides.
    StabDismount {
        target: i32,
    },
    /// A spear stab's knockback-only hit on a region entity.
    StabKnock {
        target: i32,
        power: f32,
        dx: f64,
        dz: f64,
    },
    /// A player's right click on a region entity, answered with
    /// [`PlayerEvent::EntityInteracted`] when the entity used it.
    InteractEntity {
        player: PlayerHandle,
        target: i32,
        interaction: crate::world::entity::EntityInteraction,
    },
    /// A player took the result of a merchant's offer `times` times,
    /// answered with [`PlayerEvent::TradeAnswered`].
    Trade {
        player: PlayerHandle,
        window_id: i32,
        request: crate::world::trading::TradeRequest,
    },
    /// A player closed a merchant's screen.
    StopTrading {
        merchant: i32,
        player: u128,
    },
    /// A player uses a fluid bucket (filled or empty) at what it looks at.
    UseBucket(BucketUse),
    /// A rider's doing with the vehicle it is on.
    Vehicle(VehicleRequest),
    /// A player just joined or changed dimension: remounts it if one of
    /// this region's entities has it as an offline rider. No reply on a
    /// miss; a hit sends [`PlayerEvent::Riding`] like an ordinary mount.
    ReclaimVehicle {
        player: PlayerHandle,
        uuid: u128,
    },
    /// A player places a boat at what it looks at, answered with an
    /// [`PlayerEvent::ActionResult`].
    PlaceBoat(BoatPlace),
    /// Runs `run` on the region once every chunk it owns between `min`
    /// and `max` is loaded (loading them), for feature requests that must
    /// see those chunks (gateway exit searches).
    WhenLoaded {
        min: BlockPos,
        max: BlockPos,
        run: crate::world::region_api::RegionTask,
    },
    /// Runs `run` on the region as soon as the request is taken, for
    /// feature work sent from sessions or commands that only reads or
    /// changes what the region already holds.
    Run(crate::world::region_api::RegionTask),
    /// One chunk of a pre-generation job (see `world::pregen`).
    Pregen(crate::world::pregen::RegionOp),
    /// A player starts sleeping in the bed whose head is at `pos`.
    OccupyBed {
        pos: BlockPos,
    },
    /// A player gets out of the bed whose head is at `pos`; answered with
    /// [`PlayerEvent::LeftBed`].
    LeaveBed {
        player: PlayerHandle,
        pos: BlockPos,
        yaw: f32,
    },
    /// Where a player appears around the world spawn `center`: a column
    /// within `radius` a player can stand in, tried in the order `offset`
    /// starts, loading chunks first; the bottom centre of the block.
    FindSpawn {
        center: BlockPos,
        radius: i32,
        offset: u32,
        reply: oneshot::Sender<[f64; 3]>,
    },
    /// Where a player respawns at the spawn block (bed, respawn anchor) or
    /// forced spawn point at `pos`, loading the chunk first, or `None` if
    /// the spawn block is gone. A death respawn `consume`s one charge of a
    /// respawn anchor.
    RespawnPoint {
        pos: BlockPos,
        yaw: f32,
        pitch: f32,
        forced: bool,
        consume: bool,
        reply: oneshot::Sender<Option<RespawnSpot>>,
    },
    /// Loads (or generates) the chunk at `pos` and replies once it is
    /// installed, without reading or changing anything in it. Used to hold
    /// a teleport's destination back until its terrain exists, so a player
    /// is never released to fall through ungenerated ground.
    AwaitChunk {
        pos: BlockPos,
        reply: oneshot::Sender<()>,
    },
    /// A player's respawn anchor click set a spawn point that differs from
    /// its old one: play the anchor's set-spawn sound there.
    AnchorSpawnSet {
        pos: BlockPos,
    },
}

/// Where a player comes back at its spawn block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RespawnSpot {
    pub position: [f64; 3],
    pub yaw: f32,
    pub pitch: f32,
    /// The spawn block is a respawn anchor (after any charge was spent).
    pub anchor: bool,
}

/// A respawn anchor click the region found charged and working in this
/// dimension: the player's new spawn point.
#[derive(Debug, Clone)]
pub struct AnchorUse {
    pub dimension: DimensionId,
    pub pos: BlockPos,
}

/// A bed click the region found usable: whether the bed sets the spawn
/// point, and the sleeping height or why the player cannot sleep.
#[derive(Debug, Clone)]
pub struct BedUse {
    pub dimension: DimensionId,
    /// The head of the bed.
    pub pos: BlockPos,
    pub rule: tuff_vanilla_data::block::BedRuleAttribute,
    pub set_spawn: bool,
    pub sleep: Result<f64, Option<TextComponent>>,
    /// A monster that prevents rest is near; creative players ignore it.
    pub monsters_near: bool,
}
