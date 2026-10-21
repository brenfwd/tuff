use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot, watch};
use tuff_vanilla_data::damage_type::DamageType;
use tuff_vanilla_data::registry::MobEffect;
use tuff_vanilla_data::{BlockState, Item};

use crate::world::dimension::DimensionId;
use tuff_item::item::{ItemSnapshot, ItemStack};
use tuff_item::text::TextComponent;
use tuff_nbt::Compound;
use tuff_storage::admin::PermissionLevel;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::Chunk;
use tuff_world_model::pos::ChunkPos;

use super::*;

pub type PlayerId = i32;
pub type Packets = Arc<[Bytes]>;

/// Where regions and other players send a player's events.
#[derive(Debug, Clone)]
pub struct PlayerHandle {
    pub id: PlayerId,
    events: mpsc::UnboundedSender<PlayerEvent>,
    selector_data: Arc<SelectorData>,
    /// A connectionless player that only needs chunk contents, never their
    /// encoded packets.
    pub mirror_only: bool,
}

impl PlayerHandle {
    pub fn new(id: PlayerId) -> (Self, mpsc::UnboundedReceiver<PlayerEvent>) {
        let (events, rx) = mpsc::unbounded_channel();
        let selector_data = Arc::new(SelectorData {
            form: watch::Sender::new(None),
            live: watch::Sender::new(None),
            wanted: AtomicBool::new(false),
            seen_credits: AtomicBool::new(false),
        });
        (
            Self {
                id,
                events,
                selector_data,
                mirror_only: false,
            },
            rx,
        )
    }

    /// The player's last published saved form, for selectors testing
    /// `nbt`. Reading it never waits on the session; it also asks the
    /// session to keep the form current for a while (see
    /// [`SelectorData`]).
    pub fn selector_data(&self) -> Option<Arc<Compound>> {
        self.selector_data.wanted.store(true, Ordering::Relaxed);
        self.selector_data.form.borrow().clone()
    }

    /// Whether a selector read the form since the last call.
    pub fn take_selector_interest(&self) -> bool {
        self.selector_data.wanted.swap(false, Ordering::Relaxed)
    }

    pub fn publish_selector_data(&self, form: Compound) {
        self.selector_data.form.send_replace(Some(Arc::new(form)));
    }

    /// The player's last published live facts, for predicates; read
    /// like [`Self::selector_data`].
    pub fn live_facts(&self) -> Option<Arc<crate::world::predicate::LiveFacts>> {
        self.selector_data.wanted.store(true, Ordering::Relaxed);
        self.selector_data.live.borrow().clone()
    }

    pub fn publish_live_facts(&self, live: crate::world::predicate::LiveFacts) {
        self.selector_data.live.send_replace(Some(Arc::new(live)));
    }

    /// Whether the player has seen the end credits, as its session last
    /// set it; regions read it to decide if a vehicle may leave the end.
    pub fn seen_credits(&self) -> bool {
        self.selector_data.seen_credits.load(Ordering::Relaxed)
    }

    pub fn set_seen_credits(&self, seen: bool) {
        self.selector_data
            .seen_credits
            .store(seen, Ordering::Relaxed);
    }

    pub fn has_selector_data(&self) -> bool {
        self.selector_data.form.borrow().is_some()
    }

    /// Returns false once the player has left.
    pub fn send(&self, event: PlayerEvent) -> bool {
        self.events.send(event).is_ok()
    }

    /// Like [`Self::send`], but hands the event back when the session is
    /// gone, so the stacks it carries go back to their sender.
    pub fn try_send(&self, event: PlayerEvent) -> Result<(), Box<PlayerEvent>> {
        self.events.send(event).map_err(|error| Box::new(error.0))
    }

    pub fn is_connected(&self) -> bool {
        !self.events.is_closed()
    }
}

/// A player's saved form as its session last published it. The session
/// owns the writes; command execution only reads, so selectors never await
/// another session. Serializing a player is not free, so sessions publish
/// on demand: every tick while a selector has read the form recently, and
/// otherwise only at a slow cadence.
#[derive(Debug)]
struct SelectorData {
    form: watch::Sender<Option<Arc<Compound>>>,
    live: watch::Sender<Option<Arc<crate::world::predicate::LiveFacts>>>,
    wanted: AtomicBool,
    seen_credits: AtomicBool,
}

use crate::world::session::hooks::HookFuture;

type SessionJob =
    Box<dyn for<'a> FnOnce(&'a mut crate::world::session::PlayerSession) -> HookFuture<'a> + Send>;

/// Work for a player session to run on itself, the session counterpart of
/// a region task.
pub struct SessionTask {
    job: SessionJob,
    data: Option<Box<dyn std::any::Any + Send>>,
}

impl SessionTask {
    pub fn new(
        run: impl for<'a> FnOnce(&'a mut crate::world::session::PlayerSession) -> HookFuture<'a>
        + Send
        + 'static,
    ) -> Self {
        Self {
            job: Box::new(run),
            data: None,
        }
    }

    /// Attaches what the task does, for tests and observers to read back
    /// with [`Self::data`].
    pub fn with_data(mut self, data: impl std::any::Any + Send) -> Self {
        self.data = Some(Box::new(data));
        self
    }

    pub fn data<T: 'static>(&self) -> Option<&T> {
        self.data.as_ref()?.downcast_ref()
    }

    pub async fn run(self, session: &mut crate::world::session::PlayerSession) {
        (self.job)(session).await;
    }
}

impl std::fmt::Debug for SessionTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionTask")
    }
}

/// Events from regions name their dimension, so a player who has just left
/// it can drop what its old regions still send.
#[derive(Debug)]
pub enum PlayerEvent {
    /// Work a feature queued for this session to run on itself.
    Run(SessionTask),
    /// Framed Chunk Data for a subscribed chunk, current as of sending.
    Chunk {
        dimension: DimensionId,
        pos: ChunkPos,
        packet: Bytes,
        /// The chunk as that packet describes it, shared with the region.
        snapshot: Option<Arc<Chunk>>,
    },
    /// Block changes that went out as `ChunkPackets` for the same chunk,
    /// in the same order, for the session's chunk mirror.
    BlockChanges {
        dimension: DimensionId,
        pos: ChunkPos,
        changes: Arc<[(BlockPos, BlockState)]>,
    },
    /// Packets about a chunk's contents (block updates, sounds), to be
    /// delivered only after that chunk's data.
    ChunkPackets {
        dimension: DimensionId,
        pos: ChunkPos,
        packets: Packets,
    },
    Track(Track),
    /// Packets for this player alone that no chunk or entity scopes (a
    /// boss bar).
    Packets(Packets),
    /// A world border change, for players still in `dimension`.
    Border {
        dimension: DimensionId,
        packet: Bytes,
    },
    Untrack {
        entity: i32,
        seq: u64,
    },
    /// Packets about one entity, for players who can see it.
    EntityPackets {
        entity: i32,
        packets: Packets,
    },
    /// An entity's latest value of debug feed `feed` (a Debug Entity
    /// Value packet), shown now if the player sees the entity, else kept
    /// for when it does.
    DebugEntityValue {
        entity: i32,
        feed: u8,
        packet: Bytes,
    },
    ActionResult {
        sequence: i32,
        outcome: ActionOutcome,
    },
    /// The held `item` acted on the block described by `used_on` without
    /// placing anything (a tool, honeycomb, shears, a brush).
    ItemUsedOnBlock {
        off_hand: bool,
        item: tuff_vanilla_data::Item,
        used_on: Box<crate::world::predicate::LocationFacts>,
    },
    /// An item this player may pick up. The session answers with
    /// `RegionInternal::ItemTaken` naming `offer` and changes nothing
    /// until the region grants it.
    OfferItem {
        item: i32,
        offer: u64,
        stack: tuff_item::item::ItemSnapshot,
        region: RegionHandle,
    },
    /// What the region handed over for an accepted offer; the session
    /// adds it to its inventory, dropping what no longer fits.
    ItemGranted {
        stack: ItemStack,
        picked_up: bool,
    },
    /// A tamed parrot touching this player wants to land on its shoulder.
    /// The session answers with `RegionInternal::ShoulderTaken` naming
    /// `offer`, holding a free shoulder for it until the region settles.
    OfferShoulder {
        entity: i32,
        offer: u64,
        region: RegionHandle,
    },
    /// The region's settlement of shoulder offer `offer`: the entity's
    /// saved form when it left the world for the shoulder, `None` when it
    /// is gone or no longer offered.
    ShoulderGranted {
        offer: u64,
        entity: Option<Box<tuff_nbt::Compound>>,
    },
    /// A dispenser put `stack` on this player; it goes in `slot` if that
    /// is still empty, else into the inventory.
    DispenserEquip {
        slot: tuff_vanilla_data::item::EquipmentSlot,
        stack: ItemStack,
    },
    Attacked(Attack),
    /// An explosion hurt and pushed this player. The Explosion packet with
    /// the push was already sent if `push_sent`.
    Exploded {
        damage: f32,
        blast: Blast,
        push: [f64; 3],
        push_sent: bool,
    },
    /// A pure knockback push with no damage: a mace smash's area knockback
    /// on a nearby player, or Wind Burst launching its own attacker. The
    /// receiver applies its own knockback resistance, as `direction`
    /// (horizontal, not necessarily normalized) and `power` before it.
    Knocked {
        direction: [f64; 2],
        power: f64,
    },
    /// This player's attack on `target` hurt it: the weapon it was made
    /// with wears, the attacker stops sprinting if it knocked the target
    /// back, and a sweep hits the players around the target.
    AttackLanded {
        weapon: Option<tuff_item::item::ItemSnapshot>,
        target: PlayerId,
        target_position: [f64; 3],
        knockback: bool,
        sweep: Option<Sweep>,
        health_lost: f32,
        target_kind: tuff_vanilla_data::registry::EntityType,
        dealt: f32,
    },
    /// The merchant's answer to a [`RegionRequest::Trade`].
    TradeAnswered {
        window_id: i32,
        accepted: bool,
    },
    /// A custom statistic went up by some amount (talking to a villager).
    AwardStat(tuff_vanilla_data::registry::CustomStat, i32),
    /// A shrieker's warning left this player's warden spawn tracker like so.
    WardenTracker(crate::storage::player_data::WardenTracker),
    /// `/warden_spawn_tracker`: set the warning level, or reset the
    /// tracker on `None`.
    WardenWarningLevel(Option<i32>),
    /// A region entity used the item this player clicked it with.
    EntityInteracted {
        off_hand: bool,
        /// The held item when the click was made; `None` for an empty hand.
        item: Option<Item>,
        effect: HandEffect,
    },
    /// Damage caused by another player's doing that is not an attack (an
    /// enchantment such as Thorns).
    Hurt {
        amount: f32,
        kind: &'static DamageType,
        attacker: Option<(PlayerId, String)>,
        position: Option<[f64; 3]>,
    },
    /// A mob or projectile hit this player.
    EntityHurt(Box<crate::world::entity::EntityHit>),
    /// This player, spinning from a Riptide launch, ran into the entity
    /// `target`.
    SpinAttackTouched {
        target: i32,
    },
    /// Something shoved this player (a dragon's wing).
    Pushed([f64; 3]),
    /// A cart or boat shoved this player; the client adds it itself.
    Nudged([f64; 3]),
    /// A moving block pushed this player by `delta`, or a honey block
    /// carried it. Only the server's copy moves: the client moves itself.
    PistonPushed {
        dimension: DimensionId,
        delta: [f64; 3],
    },
    /// This player got on a vehicle, or the vehicle's riders changed.
    Riding(Box<Riding>),
    /// The server moved the vehicle this player drives.
    VehicleMotion {
        vehicle: i32,
        motion: VehicleMotion,
    },
    /// This player is off its vehicle and stands at `position`.
    Dismounted {
        vehicle: i32,
        position: [f64; 3],
    },
    /// A spear stab knocked this player off its vehicle.
    StabDismounted,
    /// The mount took this player's spur: the item wears.
    VehicleBoosted {
        vehicle: i32,
    },
    /// This player killed an entity.
    KilledEntity(Box<Kill>),
    /// Something another actor saw that counts towards this player's
    /// advancements.
    Advancement(Box<AdvancementEvent>),
    /// This player's fishing bobber was reeled in (`damage` wears the rod,
    /// `fish` counts the fish caught) or vanished.
    Reeled {
        damage: u32,
        fish: u32,
    },
    BobberGone(i32),
    /// A splash potion or lingering cloud reached this player.
    Potion(Box<crate::world::effects::PotionHit>),
    /// If the player has `from`, it becomes `to` at amplifier 0, lasting
    /// `ticks_per_level` for each level `from` had (Bad Omen turning into
    /// Trial Omen at a trial spawner).
    ConvertEffect {
        from: MobEffect,
        to: MobEffect,
        ticks_per_level: i64,
    },
    /// If the player has `from`, it becomes `to` at the same amplifier,
    /// lasting `duration`; `taken` hears the amplifier when it did.
    SwapEffect {
        from: MobEffect,
        to: MobEffect,
        duration: i64,
        taken: Option<oneshot::Sender<u8>>,
    },
    /// The player gains this effect as given.
    AddEffect(crate::world::effects::ActiveEffect),
    /// Unless the player has `effect` with more than `cap` ticks left, it
    /// gains `add` ticks of it on top of what is left, at most `cap`.
    StackEffect {
        effect: MobEffect,
        add: i64,
        cap: i64,
    },
    /// The player loses `effect`.
    RemoveEffect(MobEffect),
    /// A block gave this player a lasting effect (a beacon, a conduit).
    GiveEffect {
        effect: MobEffect,
        duration: i32,
        amplifier: u8,
        ambient: bool,
    },
    /// The block the player stands in grew to full height under it
    /// (farmland turning to dirt): it moves up to `y`.
    PushedUp {
        dimension: DimensionId,
        y: f64,
    },
    /// The vehicle this player rides is going through a portal: the player
    /// follows into `dimension` at `position`, turned by `turn` degrees, and
    /// the target region seats it again.
    PortalRide {
        dimension: DimensionId,
        position: [f64; 3],
        turn: f32,
    },
    /// An ender pearl this player threw landed: it moves there and takes
    /// `fall_damage`.
    PearlTeleport {
        dimension: DimensionId,
        position: [f64; 3],
        fall_damage: f32,
    },
    /// From the player's home region: what its body touched as of the
    /// update with this `seq`.
    Surroundings {
        dimension: DimensionId,
        seq: u64,
        in_water: bool,
        submersion: crate::world::player::Submersion,
        /// Inside lava, which burns and hurts.
        in_lava: bool,
        /// Standing in fire: the damage it deals.
        in_fire: Option<f32>,
        /// On a ladder, vine or anything else climbable.
        climbable: bool,
        /// Where a fall from what is climbed began, when climbing.
        climb_location: Option<crate::world::death::FallLocation>,
        /// Inside a cobweb, sweet berry bush or powder snow, which make it
        /// forget its fall.
        resets_fall: bool,
        /// The block its feet land on, which shapes fall damage.
        landed_on: Option<tuff_vanilla_data::BlockState>,
        /// The portal block the body is inside.
        portal: Option<crate::world::contact::PortalContact>,
        /// The biome at the player's feet, for advancement locations.
        biome: Option<&'static str>,
        /// The block the player stands on.
        stepping_on: Option<tuff_vanilla_data::BlockState>,
        /// Magma underfoot, campfires, berry bushes and suffocating blocks
        /// around the body.
        contact: crate::world::block_contact::Contact,
    },
    /// The home region noticed a suffocating block now fills the space at
    /// this player's eyes (a falling block landed on it, a piston pushed
    /// one into it) and it should start taking damage without waiting for
    /// its next movement report.
    Suffocating {
        dimension: DimensionId,
    },
    /// The home region noticed a liquid or fire block change near this
    /// player's body (water or lava flowed onto or away from it, fire
    /// appeared or went out) and rechecked its contact with them, without
    /// waiting for its next movement report. Fields mean the same as in
    /// `Surroundings`.
    FluidContact {
        dimension: DimensionId,
        in_water: bool,
        submersion: crate::world::player::Submersion,
        in_lava: bool,
        in_fire: Option<f32>,
    },
    /// The block this player was destroying is gone, so its break ended.
    DestroyEnded(BlockPos),
    /// A container menu the player just opened by clicking a block.
    MenuOpen(MenuOpen),
    /// Slots of a block container this player is viewing changed.
    ContainerSlots {
        dimension: DimensionId,
        pos: BlockPos,
        slots: Vec<(usize, Option<tuff_item::item::ItemSnapshot>)>,
    },
    /// Container data (a furnace's burn and cook progress) of a block
    /// container this player is viewing changed, as `(property, value)`.
    ContainerData {
        dimension: DimensionId,
        pos: BlockPos,
        data: Vec<(usize, i32)>,
    },
    /// The player collected an experience orb worth `amount`.
    Experience {
        amount: i32,
    },
    /// Recipes a furnace this player took from had cooked, for the recipe
    /// book.
    RecipesAwarded(Vec<tuff_vanilla_data::Frozen<str>>),
    /// The container at `pos` is gone; a menu over it must close.
    ContainerClosed {
        dimension: DimensionId,
        pos: BlockPos,
    },
    /// A region's vote on its share of a two-region transaction:
    /// `prepared` says it validated and locked the slots and took out
    /// `taken`, what they lose. `contents` pictures the containers it
    /// touched either way.
    TransactionPrepared {
        window_id: i32,
        txn: u64,
        prepared: bool,
        contents: Vec<(BlockPos, Vec<Option<ItemSnapshot>>)>,
        taken: Vec<ItemStack>,
    },
    /// The region's answer to a container transaction, handing back the
    /// pool: what is left of it once accepted, all of it unchanged once
    /// rejected. `contents` pictures the containers a rejected
    /// transaction touched, keyed by block entity position.
    Transaction {
        window_id: i32,
        accepted: bool,
        contents: Vec<(BlockPos, Vec<Option<ItemSnapshot>>)>,
        pool: Vec<ItemStack>,
    },
    /// Slots of an entity's container (a chest boat) this player is
    /// viewing changed.
    EntitySlots {
        dimension: DimensionId,
        entity: i32,
        slots: Vec<(usize, Option<tuff_item::item::ItemSnapshot>)>,
    },
    /// The entity whose container this player views is gone from its
    /// region (broken, unloaded, handed off); the menu must close.
    EntityContainerMoved {
        dimension: DimensionId,
        entity: i32,
        region: RegionHandle,
    },
    /// See [`Self::EntityContainerMoved`]: the entity's menu closes.
    EntityContainerClosed {
        dimension: DimensionId,
        entity: i32,
    },
    /// The region's answer to an entity container transaction, handing
    /// back the pool as [`Self::Transaction`] does; a rejected one
    /// pictures the container's current contents.
    EntityTransaction {
        window_id: i32,
        accepted: bool,
        contents: Vec<Option<ItemSnapshot>>,
        pool: Vec<ItemStack>,
    },
    /// A system chat message for this player.
    Message(TextComponent),
    /// The world is nearly out of disk space; sent to administrators.
    LowDiskSpace,
    /// A message over the hotbar.
    Overlay(TextComponent),
    /// Commands from a clicked sign's click events, run as this player
    /// from the sign's centre with gamemaster permission and no output.
    SignCommands {
        dimension: crate::world::dimension::DimensionId,
        position: [f64; 3],
        commands: Vec<String>,
    },
    /// Morning came while this player slept.
    WakeUp,
    Kick(TextComponent),
    /// The hardcore property changed; the client needs a fresh Login.
    HardcoreChanged,
    /// A live server.properties value this session uses changed.
    LiveSetting(crate::world::live::LiveChange),
    ResourcePack(crate::resource_pack::PackOffer),
    ResourcePackPop(Option<u128>),
    /// A command acts on this player; the outcome goes back on `reply`.
    Directive {
        directive: Directive,
        source: DirectiveSource,
        reply: mpsc::UnboundedSender<DirectiveReply>,
    },
    /// A feature moves this player; their portal cooldown starts on arrival.
    PluginTeleport(Teleport),
    /// A feature call's inventory edits for this player, in call order.
    PluginInventory(Vec<crate::world::session::plugin_inventory::InventoryOp>),
    /// The operator list changed this player's level.
    SetPermission(PermissionLevel),
    /// The player's permission nodes changed: resend the command tree.
    PermissionsChanged,
    /// The `reduced_debug_info` rule changed to this value.
    ReducedDebugInfo(bool),
    /// A save of the player's data, taken at a barrier shared with the
    /// regions' saves (see [`SaveBarrier`]).
    Save(SaveBarrier),
    Shutdown,
}

/// Makes a player's save and the regions' saves one snapshot of the world.
/// The session snapshots once no container transaction is in flight, tells
/// `snapshotted`, then takes no new transaction until `release` fires or
/// drops (the regions have snapshotted) or a bound passes.
#[derive(Debug)]
pub struct SaveBarrier {
    pub snapshotted: oneshot::Sender<()>,
    pub release: oneshot::Receiver<()>,
    /// Told once the player's data is written.
    pub written: Option<oneshot::Sender<()>>,
    /// The world save the player's data belongs to.
    pub ticket: Option<tuff_storage::journal::EpochTicket>,
}
