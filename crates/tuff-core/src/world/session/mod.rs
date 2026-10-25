//! One actor per connected player: the owner of that player's state
//! (position, health, hunger, inventory), of what its client has been sent
//! (chunks and visible entities), and of its player data file. It talks to
//! the regions it views and stands in, and to other players directly.

pub mod hooks;

mod directives;

mod advancements;
mod beacon_menu;
mod debug_stick;
mod environment;
mod hands;
mod health;
mod menus;
pub use menus::{Place, Reasons, Sim};
mod merchant_menu;
pub mod mirror;
pub mod plugin_inventory;
mod recipe_placement;
mod resource_packs;
mod respawn;
mod riding;
mod scores;
mod stats;
pub mod structure_block;

use crate::world::stats::Stat;
use tuff_supervise::{CatchUnwind, panic_message};
mod accessors;
mod actions;
mod body;
mod books;
mod camera;
mod commands;
mod conn;
mod events;
mod game_events;
mod mode;
mod motion;
mod net;
pub use net::TickThrottler;
mod packets;
mod persist;
mod place_offer;
mod pose;
mod progress;
mod published;
mod rate;
mod secure_chat;
mod shoulders;
mod sleep;
mod surroundings;
mod sync;
mod view;

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use tuff_vanilla_data::registry::{CustomStat, StatType};

use bytes::Bytes;
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use tokio::sync::{mpsc, oneshot, watch};
use tuff_vanilla_data::Item;
use tuff_vanilla_data::damage_type::{self, DamageType};
use tuff_vanilla_data::entity::{MAX_AIR_SUPPLY, Pose};
use tuff_vanilla_data::item::EquipmentSlot;
use tuff_vanilla_data::registry::{Attribute, EntityType, MobEffect, SoundEvent};

use crate::auth::GameProfile;
use crate::command::SuggestContext;
use crate::protocol::Encode;
use crate::protocol::codec::HashedSlot;
use crate::protocol::configuration::ClientInformation;
use crate::protocol::play::{
    self, GameMode, MetadataValue, Movement, PlayerInfoEntry, Serverbound, metadata,
};
use crate::storage::player_data::{
    self, ENDER_CHEST_SLOTS, ExperienceSave, FoodSave, PlayerRecord, PlayerState, RecipeBookSave,
    RespawnSave,
};
use crate::world::attributes;
use crate::world::commands::{Actor, Source, SourceKind, gamemode};
use crate::world::dimension::{Dimension, DimensionId};
use crate::world::effects::ActiveEffects;
use crate::world::entity::Culprit;
use crate::world::inventory::Inventory;
use crate::world::menu::{Drag, GridHook, MenuKind};
use crate::world::messages::{
    ActionOutcome, CoordinatorMessage, HandEffect, ManagerMessage, Packets, PlayerEvent,
    PlayerHandle, PlayerId, PlayerUpdate, RegionHandle, RegionInternal, RegionRequest,
};
use crate::world::player::{
    self, Abilities, Destroying, Experience, Fall, Food, FoodEffect, Health, UsingItem, exhaustion,
};
use crate::world::recipe_book::RecipeBook;
use crate::world::tracking::{EntityView, ViewArea};
use crate::world::trading::{MerchantOffer, MerchantView};
use crate::world::{Shared, block_contact, distance_sq, framed, packets};
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_storage::admin::{ChatPermission, PermissionLevel};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, RegionPos, regions_near};

/// The client keeps its sprint speed as this movement speed modifier, and
/// an attribute update replaces every modifier it holds, so updates must
/// carry it while the player sprints or the client drops to walking speed.
const SPRINTING_MODIFIER: play::AttributeModifierData<'static> = play::AttributeModifierData {
    id: "minecraft:sprinting",
    amount: 0.30000001192092896,
    operation: tuff_vanilla_data::item::ModifierOperation::AddMultipliedTotal,
};

pub const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);
pub const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(15);
pub const ATTACK_REACH: f64 = 6.0;
/// Air gained back each tick out of water.
pub const AIR_REFILL: i32 = 4;
/// The air at which a submerged player takes drowning damage and gets a
/// little breath back.
pub const DROWNING_AIR: i32 = -20;
pub const DROWNING_DAMAGE: f32 = 2.0;
pub const VOID_DAMAGE: f32 = 4.0;
/// How long a Riptide launch spins its player.
pub const SPIN_ATTACK_TICKS: u32 = 20;
/// How long a selector reading a player's saved form keeps the session
/// publishing it every tick.
pub const SELECTOR_INTEREST_TICKS: u64 = 1200;
/// How often a session publishes its saved form when no selector reads it.
pub const SELECTOR_IDLE_PERIOD: u64 = 100;
/// How long standing in fire sets a player alight for.
pub const IN_FIRE_BURN_SECONDS: f32 = 8.0;
/// How many ticks a player that is not burning stands in fire before it
/// catches.
pub const FIRE_IMMUNE_TICKS: i32 = 20;

/// What lava does to a player inside it each tick.
pub const LAVA_DAMAGE: f32 = 4.0;
pub const LAVA_BURN_SECONDS: f32 = 15.0;
/// How far past the world border a move from inside may reach before it
/// is refused, as the game tolerates small differences from the client.
pub const BORDER_TOLERANCE: f64 = 0.0625;
pub(crate) const PLAYER_HALF_WIDTH: f64 = 0.3;
pub const CLIENT_LOAD_TICKS: u64 = 60;
/// How long a teleport may go unconfirmed before a move sends it again.
pub const TELEPORT_RESEND_TICKS: u64 = 20;
pub const MAX_CHAT_CHARS: usize = 256;
pub const THROW_SPEED: f64 = 0.3;
pub const MIN_VIEW_DISTANCE: i32 = 2;
/// Players register with every region within this many blocks: pickup and
/// block-placement checks work across region boundaries, and mobs see
/// players (to spawn, despawn, follow and look at) as far as the game's
/// spawning and despawning distance.
pub const PRESENCE_MARGIN: f64 = 3.0;
/// Vanilla's chunk batch pacing: one unacknowledged batch until the client
/// first answers, then up to ten.
pub const INITIAL_UNACKED_BATCHES: u32 = 1;
pub const MAX_UNACKED_BATCHES: u32 = 10;
pub const INITIAL_CHUNKS_PER_TICK: f32 = 9.0;
pub const MAX_CHUNKS_PER_TICK: f32 = 64.0;
/// Vanilla's creative drop throttle: each drop costs 20, one is regained per
/// tick, and drops stop at 1480.
pub const CREATIVE_DROP_COST: u32 = 20;
pub const CREATIVE_DROP_LIMIT: u32 = 1480;
pub const MIN_WINDOW_SLOT: i16 = 1;
pub const MAX_WINDOW_SLOT: i16 = 45;
/// A container menu stays open while the player's eyes are within the
/// block interaction range plus this buffer of the container block.
pub const BLOCK_INTERACTION_RANGE: f64 = 4.5;
pub const MENU_RANGE_BUFFER: f64 = 4.0;
pub const ENTITY_INTERACTION_RANGE: f64 = 3.0;
/// A closing menu waits this long for its transaction's answer at most.
pub const MENU_CLOSE_TIMEOUT_TICKS: u64 = 100;
/// Inventory window slots whose changes count for advancements:
/// everything but the crafting result and grid.
pub const UNLOCK_WATCHED_SLOTS: std::ops::Range<usize> = 5..46;
pub const SMITHING_TABLE_USED: i32 = 1044;

/// Equipment slots in the order the game reports them.
pub const EQUIPMENT_SLOTS: [EquipmentSlot; 6] = [
    EquipmentSlot::Mainhand,
    EquipmentSlot::Offhand,
    EquipmentSlot::Feet,
    EquipmentSlot::Legs,
    EquipmentSlot::Chest,
    EquipmentSlot::Head,
];
pub const ARMOR_SLOTS: [EquipmentSlot; 4] = [
    EquipmentSlot::Feet,
    EquipmentSlot::Legs,
    EquipmentSlot::Chest,
    EquipmentSlot::Head,
];

/// The protocol's Set Equipment slot of an equipment slot.
pub fn equipment_slot_id(slot: EquipmentSlot) -> u8 {
    match slot {
        EquipmentSlot::Mainhand => play::equipment_slot::MAIN_HAND,
        EquipmentSlot::Offhand => play::equipment_slot::OFF_HAND,
        EquipmentSlot::Feet => play::equipment_slot::FEET,
        EquipmentSlot::Legs => play::equipment_slot::LEGS,
        EquipmentSlot::Chest => play::equipment_slot::CHEST,
        EquipmentSlot::Head => play::equipment_slot::HEAD,
        EquipmentSlot::Body => play::equipment_slot::BODY,
        EquipmentSlot::Saddle => play::equipment_slot::SADDLE,
    }
}

/// A bed this player sleeps in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sleep {
    /// The head of the bed.
    pub pos: BlockPos,
    pub rule: tuff_vanilla_data::block::BedRuleAttribute,
}

/// Ticks asleep before a sleeper counts towards skipping the night.
pub const DEEP_SLEEP_TICKS: u32 = 100;
pub const SLEEP_COUNTER_RESET: u32 = 110;

/// Who and what hurt a player, as hooks hear of it after the hit.
#[derive(Debug, Clone)]
pub struct HurtBy {
    pub kind: &'static DamageType,
    pub attacker: Option<Culprit>,
    pub projectile: Option<EntityType>,
    pub direct: bool,
}

/// Where damage comes from.
pub struct DamageSource {
    pub kind: &'static DamageType,
    /// The player or mob who caused it.
    pub attacker: Option<Culprit>,
    /// The projectile that hit, if one did.
    pub projectile: Option<EntityType>,
    /// Where it comes from, for shields and knockback.
    pub position: Option<[f64; 3]>,
    /// The attacker's weapon, which may disable a shield.
    pub weapon: Option<tuff_item::item::ItemSnapshot>,
    /// The attacker dealt it itself (a melee hit, not a projectile or
    /// TNT); only such hits knock a shield holder back.
    pub direct: bool,
}

/// What a hit did.
#[derive(Debug, Clone, Copy, Default)]
pub struct HurtResult {
    /// Damage got through (or at least was not stopped by a shield).
    pub landed: bool,
    /// The velocity the hit gave, when it moves the player.
    pub motion: Option<[f64; 3]>,
    /// Health lost.
    pub health_lost: f32,
    /// A raised shield stopped some of a direct melee hit.
    pub shield_blocked: bool,
}

pub struct QueuedChunk {
    pub pos: ChunkPos,
    pub packet: Bytes,
    pub snapshot: Option<Arc<tuff_world_model::chunk::Chunk>>,
    pub followups: Vec<Bytes>,
}

/// A region actor this player talks to.
pub type RegionKey = (DimensionId, RegionPos);

/// What backs the container section of an open menu.
#[derive(PartialEq)]
pub enum MenuBacking {
    /// Block entities in slot order, each with its slot count.
    Blocks(Vec<(BlockPos, usize)>),
    /// The player's ender chest, opened at this block.
    EnderChest(BlockPos),
    /// A crafting station whose slots belong to this session.
    Station(BlockPos),
    /// A merchant's screen, by the merchant's entity id; its payment
    /// slots belong to this session.
    Merchant(i32),
    /// A region entity's container (a chest boat), by its entity id.
    Entity(i32),
}

const MAX_BUNDLE_PACKETS: usize = 4096;

impl MenuBacking {
    pub fn positions(&self) -> Vec<BlockPos> {
        match self {
            Self::Blocks(parts) => parts.iter().map(|(pos, _)| *pos).collect(),
            Self::EnderChest(pos) | Self::Station(pos) => vec![*pos],
            Self::Merchant(_) | Self::Entity(_) => Vec::new(),
        }
    }

    fn viewed(&self) -> bool {
        !matches!(self, Self::Station(_) | Self::Merchant(_))
    }

    /// The menu slot at which the block entity at `pos` starts.
    fn offset(&self, pos: BlockPos) -> Option<usize> {
        let Self::Blocks(parts) = self else {
            return None;
        };
        let mut offset = 0;
        for (part, size) in parts {
            if *part == pos {
                return Some(offset);
            }
            offset += size;
        }
        None
    }
}

/// A click waiting for the transaction before it to settle.
pub struct QueuedClick {
    pub state_id: i32,
    pub slot: i16,
    pub button: i8,
    pub mode: i32,
    pub changed: Vec<(i16, Option<HashedSlot>)>,
    pub carried: Option<HashedSlot>,
}

/// A transaction in flight: where the stacks of the pool it carries came
/// from, to put back on refusal, and the click's pictures to settle
/// against once accepted.
pub struct Pending {
    pub origins: Vec<Option<menus::Place>>,
    pub sim: menus::Sim,
    pub reasons: Option<menus::Reasons>,
    /// The client's view may differ: send the screen once settled.
    pub resend: bool,
    /// A merchant screen's offers before the trade.
    pub merchant: Option<Vec<MerchantOffer>>,
}

/// A transaction over both regions of a split double chest, waiting for
/// both regions' votes.
pub struct SplitTransaction {
    pub txn: u64,
    pub near_voted: bool,
    pub far_voted: bool,
    pub prepared: bool,
    pub contents: Vec<(BlockPos, Vec<Option<tuff_item::item::ItemSnapshot>>)>,
    /// What the player's side gave up.
    pub pool: Vec<ItemStack>,
    pub near_changes: Vec<crate::world::messages::ContainerChange>,
    pub far_changes: Vec<crate::world::messages::ContainerChange>,
    /// What each region took out of its slots once prepared.
    pub near_taken: Vec<ItemStack>,
    pub far_taken: Vec<ItemStack>,
}

/// A two-region transaction whose menu closed before every region voted:
/// a late vote's taken stacks go straight back to its region.
pub struct AbandonedSplit {
    pub txn: u64,
    pub far_pos: Option<BlockPos>,
    pub near: Option<RegionHandle>,
    pub far: Option<RegionHandle>,
}

/// The state of a crafting station menu beyond its slots.
pub enum Station {
    Crafting,
    Stonecutter {
        input: Option<Item>,
        /// The stonecutter recipes the input allows (see
        /// `Recipes::stonecutter_options`).
        options: Vec<usize>,
        /// The chosen option, as a button index into `options`.
        selected: Option<usize>,
    },
    Smithing {
        error: bool,
    },
    /// An enchanting table: the shelves around it when it opened, the
    /// seed the offers derive from and what the client was last shown.
    Enchantment {
        bookcases: i32,
        seed: i32,
        offers: tuff_item::item::enchanting::TableOffers,
    },
    /// A beacon: the pyramid levels and the effects the client was last
    /// shown, as the menu data slots hold them.
    Beacon {
        levels: i32,
    },
    Anvil {
        name: Option<String>,
        outcome: crate::world::anvil::AnvilOutcome,
    },
    /// A cartography table. Core only tracks its inputs: when they change
    /// the result is cleared and marked `stale`, and the feature owning
    /// maps fills it in again.
    Cartography {
        inputs: Box<[Option<tuff_item::item::ItemSnapshot>; 2]>,
        stale: bool,
    },
    Grindstone,
    /// A loom: the patterns on offer and the selected index the client was
    /// last shown (-1 for none).
    Loom {
        patterns: &'static [&'static str],
        selected: i32,
    },
    Merchant {
        view: MerchantView,
        /// The chosen offer, as the client last selected it.
        hint: usize,
        /// The offer the payment slots buy.
        active: Option<usize>,
    },
}

/// A container window the client has open.
pub struct OpenMenu {
    pub window_id: i32,
    pub kind: MenuKind,
    pub title: TextComponent,
    pub state_id: i32,
    pub container: Vec<Option<ItemStack>>,
    pub drag: Option<Drag>,
    pub region: RegionHandle,
    pub backing: MenuBacking,
    pub station: Option<Station>,
    pub pending: Option<Pending>,
    /// The part of a block menu another region owns, and that region.
    pub remote: Option<(BlockPos, RegionHandle)>,
    /// The two-region transaction whose votes are being collected.
    pub split: Option<SplitTransaction>,
    pub queued: VecDeque<QueuedClick>,
    /// The client's window is shut; the menu is torn down once no
    /// transaction is in flight.
    pub closing: Option<u64>,
}

impl OpenMenu {
    fn next_state_id(&mut self) -> i32 {
        self.state_id = (self.state_id + 1) & 0x7FFF;
        self.state_id
    }
}

/// The chunks touching `view` (diagonals included) outside it: every
/// neighbour a view chunk's light depends on.
pub fn lit_ring(view: &HashSet<ChunkPos>) -> HashSet<ChunkPos> {
    view.iter()
        .flat_map(|c| {
            (-1..=1).flat_map(move |dx| (-1..=1).map(move |dz| ChunkPos::new(c.x + dx, c.z + dz)))
        })
        .filter(|c| !view.contains(c))
        .collect()
}

fn by_region(chunks: Vec<ChunkPos>) -> HashMap<RegionPos, Vec<ChunkPos>> {
    let mut grouped: HashMap<RegionPos, Vec<ChunkPos>> = HashMap::new();
    for chunk in chunks {
        grouped.entry(chunk.region()).or_default().push(chunk);
    }
    grouped
}

/// Squared distance from `point` to the block at `pos`.
pub fn block_distance_sq(pos: BlockPos, point: [f64; 3]) -> f64 {
    let min = [pos.x as f64, pos.y as f64, pos.z as f64];
    (0..3)
        .map(|axis| {
            let d = (min[axis] - point[axis]).max(point[axis] - min[axis] - 1.0);
            d.max(0.0).powi(2)
        })
        .sum()
}

/// The entity event that shows the item in `slot` breaking.
pub fn break_event(slot: EquipmentSlot) -> i8 {
    match slot {
        EquipmentSlot::Head => play::entity_event::HEAD_BREAK,
        EquipmentSlot::Chest => play::entity_event::CHEST_BREAK,
        EquipmentSlot::Legs => play::entity_event::LEGS_BREAK,
        EquipmentSlot::Feet => play::entity_event::FEET_BREAK,
        EquipmentSlot::Offhand => play::entity_event::OFF_HAND_BREAK,
        _ => play::entity_event::MAIN_HAND_BREAK,
    }
}

/// One velocity the server gave a player's client.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GivenVelocity {
    /// The session tick it was sent at.
    pub tick: u64,
    /// What the client receives: a motion packet's velocity as quantized
    /// on the wire, a blast's knockback exactly.
    pub velocity: [f64; 3],
    /// A blast's knockback adds to the client's velocity; a motion packet
    /// replaces it.
    pub added: bool,
}

/// The last few velocities the server gave a player's client (knockback,
/// blasts, pushes).
#[derive(Clone, Debug, Default)]
pub struct VelocitySent {
    /// How many were ever sent.
    pub total: u64,
    pub recent: VecDeque<GivenVelocity>,
}

impl VelocitySent {
    const KEPT: usize = 8;

    /// A velocity sent in a Set Entity Motion packet.
    pub fn push(&mut self, tick: u64, velocity: [f64; 3]) {
        self.record(GivenVelocity {
            tick,
            velocity: crate::protocol::codec::lp_vec3_received(velocity),
            added: false,
        });
    }

    /// A blast's knockback, which the client adds to its velocity.
    pub fn push_added(&mut self, tick: u64, velocity: [f64; 3]) {
        self.record(GivenVelocity {
            tick,
            velocity,
            added: true,
        });
    }

    fn record(&mut self, given: GivenVelocity) {
        if self.recent.len() == Self::KEPT {
            self.recent.pop_front();
        }
        self.recent.push_back(given);
        self.total += 1;
    }

    /// Those sent after the first `seen`, oldest first.
    pub fn since(&self, seen: u64) -> impl Iterator<Item = GivenVelocity> + '_ {
        let fresh = usize::try_from(self.total.saturating_sub(seen)).unwrap_or(usize::MAX);
        self.recent
            .iter()
            .skip(self.recent.len().saturating_sub(fresh))
            .copied()
    }
}

/// A displacement the client applies to itself when the server tells
/// it nothing but the event: a moving block, a Riptide launch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShoveKind {
    /// Moved by this much.
    Moved([f64; 3]),
    /// Launched along a look with this strength.
    Riptide { strength: f32, yaw: f32, pitch: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shove {
    pub tick: u64,
    pub kind: ShoveKind,
}

/// The last few shoves of a player, oldest first.
#[derive(Clone, Debug, Default)]
pub struct ShoveLog {
    /// How many were ever recorded.
    pub total: u64,
    pub recent: VecDeque<Shove>,
}

impl ShoveLog {
    const KEPT: usize = 8;

    pub fn push(&mut self, tick: u64, kind: ShoveKind) {
        if self.recent.len() == Self::KEPT {
            self.recent.pop_front();
        }
        self.recent.push_back(Shove { tick, kind });
        self.total += 1;
    }

    /// Each kept shove with its sequence number.
    pub fn numbered(&self) -> impl Iterator<Item = (u64, Shove)> + '_ {
        let first = self.total - self.recent.len() as u64;
        (first..).zip(self.recent.iter().copied())
    }
}

pub struct PlayerSession {
    pub shared: Arc<Shared>,
    /// State features keep per player (see `hooks`).
    pub ext: hooks::Extensions,
    pub id: PlayerId,
    pub profile: GameProfile,
    pub info: ClientInformation,
    /// The server tick this player last ran; it goes on while frozen.
    pub tick: u64,
    pub rng: StdRng,
    pub record: PlayerRecord,

    pub pose: pose::Pose,
    pub inventory: Inventory,
    pub disconnected: bool,

    pub dimension: DimensionId,

    pub published: published::Published,
    pub net: net::Net,
    /// Set for a fake player: no socket, no saved data, no chunk bytes.
    pub fake: Option<crate::world::fake::FakeSpec>,
    /// The profile's lease, held from load to the end of the final save.
    pub lease: Option<crate::world::player_lease::PlayerLease>,
    pub fake_driver: Option<crate::world::fake::driver::Driver>,
    pub surroundings: surroundings::Surroundings,
    pub progress: progress::Progress,
    pub sync: sync::MoveSync,
    pub view: view::ChunkView,
    pub motion: motion::Motion,
    pub actions: actions::Actions,
    pub sleep: sleep::SleepState,
    pub menus: menus::MenuState,
    pub mode: mode::Mode,
    pub body: body::Body,
    pub conn: conn::Conn,
    pub ride: riding::RideState,
}

impl PlayerSession {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        shared: Arc<Shared>,
        id: PlayerId,
        profile: GameProfile,
        info: ClientInformation,
        outbound: mpsc::UnboundedSender<Bytes>,
        handle: PlayerHandle,
        events: mpsc::UnboundedReceiver<PlayerEvent>,
        packets: mpsc::Receiver<Serverbound>,
        permission: PermissionLevel,
    ) -> Self {
        let net = net::Net::new(&shared.live.network(), Instant::now());
        let clock = shared.clock.clone();
        let contents = shared.datapacks.contents();
        let datapack_generation = contents.generation;
        let advancement_tree = shared.advancements_in(&contents);
        let server_ticks = shared.server_ticks.clone();
        let tick = *server_ticks.borrow();
        let dimension = shared.spawn_dimension();
        Self {
            shared,
            ext: hooks::Extensions::default(),
            id,
            profile,
            info,
            tick,
            rng: StdRng::from_entropy(),
            record: PlayerRecord::default(),
            pose: pose::Pose::default(),
            inventory: Inventory::default(),
            disconnected: false,
            dimension,
            published: Default::default(),
            net,
            fake: None,
            lease: None,
            fake_driver: None,
            surroundings: Default::default(),
            progress: progress::Progress {
                recipe_book: RecipeBook::default(),
                datapack_generation,
                advancement_tree,
                entered_nether: None,
                last_inventory: Vec::new(),
                stats: Box::default(),
                fall_start: None,
                levitation_start: None,
                explosion_impact: None,
                lava_ride_start: None,
                arrow_kills: Vec::new(),
                seen_credits: false,
                warden_tracker: Default::default(),
            },
            sync: Default::default(),
            view: Default::default(),
            motion: Default::default(),
            actions: Default::default(),
            sleep: Default::default(),
            menus: Default::default(),
            mode: mode::Mode {
                game_mode: GameMode::Survival,
                previous_game_mode: None,
                client_hardcore: false,
                abilities: Abilities::new(GameMode::Survival, false),
                fly_speed: mode::DEFAULT_FLY_SPEED,
                permission,
                respawn: None,
                camera: None,
                post_effects: Vec::new(),
                last_death: None,
            },
            body: Default::default(),
            conn: conn::Conn {
                outbound,
                handle,
                events,
                packets,
                clock,
                server_ticks,
            },
            ride: Default::default(),
        }
    }

    /// Runs the player until disconnect, then saves its data. The session
    /// reads the file only once `lease` is granted, after any earlier owner
    /// of the profile returned it.
    pub async fn run(
        self,
        lease: crate::world::player_lease::LeaseRequest,
        done: watch::Sender<bool>,
    ) {
        let (tags, enchantments, registries) = {
            let contents = self.shared.datapacks.contents();
            (
                contents.tags.clone(),
                contents.enchantments.clone(),
                contents.registries.clone(),
            )
        };
        let run: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
            Box::pin(tuff_item::registries::scope(
                registries,
                tuff_item::enchantment::scope(enchantments, self.run_scoped(lease, done)),
            ));
        tuff_datapack::tags::scope(tags, run).await;
    }

    async fn run_scoped(
        mut self,
        lease: crate::world::player_lease::LeaseRequest,
        done: watch::Sender<bool>,
    ) {
        let lease = lease.granted().await;
        if lease.was_dirty() {
            log::warn!(
                "{} loads the last completed save: the previous session ended without its final save",
                self.profile.name
            );
        }
        self.lease = Some(lease);
        match self.load().await {
            Ok(()) => {
                let states = self
                    .shared
                    .resource_packs
                    .player_joined(self.profile.id, self.conn.handle.clone())
                    .await;
                self.joined_pack_states(states);
                let mut played = CatchUnwind::new(self.enter_boxed()).await;
                if played.is_ok() {
                    played = CatchUnwind::new(self.play_boxed()).await;
                }
                if let Err(panic) = played {
                    self.panicked(panic_message(&*panic));
                }
                self.shared
                    .resource_packs
                    .player_left(self.profile.id)
                    .await;
                self.leave().await;
            }
            Err(err) => {
                log::error!(
                    "failed to load player data for {}: {err}",
                    self.profile.name
                );
                self.send(&play::Disconnect(&TextComponent::translate(
                    "multiplayer.disconnect.invalid_player_data",
                    [],
                )));
            }
        }
        let _ = self
            .shared
            .coordinator
            .send(CoordinatorMessage::Left { player: self.id })
            .await;
        if let Some(lease) = self.lease.take() {
            lease.release();
        }
        tuff_item::item::conservation::release(self);
        let _ = done.send(true);
    }

    fn enter_boxed(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async {
            self.enter().await;
            self.joined_hooks().await;
        })
    }

    fn play_boxed(&mut self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(self.play())
    }

    pub fn send<P: Encode>(&mut self, packet: &P) {
        self.send_raw(framed(packet));
    }

    /// Never waits; a client that stops answering is dropped by the
    /// keep-alive timeout, as in vanilla.
    pub fn send_raw(&mut self, framed: Bytes) {
        if self.disconnected {
            return;
        }
        if self.conn.outbound.send(framed).is_err() {
            self.disconnected = true;
            log::info!(
                "{}: connection already closed by the client or socket",
                self.profile.name
            );
        }
    }

    /// Sends the packets between bundle delimiters so the client applies
    /// them together; vanilla's per-bundle limit is 4096 packets, so longer
    /// runs are split.
    pub fn send_bundle(&mut self, packets: impl IntoIterator<Item = Bytes>) {
        let mut packets = packets.into_iter().peekable();
        while packets.peek().is_some() {
            self.send(&play::BundleDelimiter);
            for packet in packets.by_ref().take(MAX_BUNDLE_PACKETS) {
                self.send_raw(packet);
            }
            self.send(&play::BundleDelimiter);
        }
    }

    pub fn send_all(&mut self, packets: impl IntoIterator<Item = Bytes>) {
        for packet in packets {
            self.send_raw(packet);
        }
    }

    /// The session's loop panicked: reports it and disconnects the client,
    /// after which the normal leave path runs on the state as it stands.
    fn panicked(&mut self, message: &str) {
        self.shared.metrics.core().session_panics.inc();
        log::error!(
            "session of {} panicked: {message}; saving its state as it stands and disconnecting",
            self.profile.name
        );
        self.kick(TextComponent::translate(
            "disconnect.genericReason",
            [TextComponent::new("Your session crashed")],
        ));
    }

    pub fn kick(&mut self, reason: TextComponent) {
        log::info!("kicking {}: {}", self.profile.name, reason.plain());
        self.send(&play::Disconnect(&reason));
        self.disconnected = true;
    }

    pub fn is_dead(&self) -> bool {
        self.body.health.is_dead()
    }

    /// The pose the player's state asks for. Whether the player fits in
    /// it is not checked, so a ceiling only counts once the region reports it (`Contact::cramped`).
    /// A rider keeps its pose: its legs sit inside the vehicle's block, so it
    /// never fits by the block rule.
    pub fn current_pose(&self) -> Pose {
        if self.is_dead() {
            Pose::Dying
        } else if self.sleep.sleeping.is_some() {
            Pose::Sleeping
        } else if self.motion.glide.active {
            Pose::FallFlying
        } else if self.pose.swimming
            || (self.ride.riding.is_none() && self.surroundings.contact.cramped >= 2)
        {
            Pose::Swimming
        } else if (self.pose.sneaking && !self.mode.abilities.flying)
            || (self.ride.riding.is_none() && self.surroundings.contact.cramped == 1)
        {
            Pose::Crouching
        } else {
            Pose::Standing
        }
    }

    pub fn height(&self) -> f64 {
        f64::from(self.current_pose().player_size().height)
    }

    pub fn eye(&self) -> [f64; 3] {
        let height = f64::from(self.current_pose().player_size().eye_height);
        [
            self.pose.position[0],
            self.pose.position[1] + height,
            self.pose.position[2],
        ]
    }

    pub fn chunk(&self) -> ChunkPos {
        ChunkPos::of_position(self.pose.position)
    }

    pub fn info_entry(&self) -> PlayerInfoEntry {
        PlayerInfoEntry {
            uuid: self.profile.id,
            name: self.profile.name.clone(),
            properties: self.profile.properties.clone(),
            game_mode: self.mode.game_mode,
            listed: true,
            latency: self.net.latency_ms,
            hat: self.info.skin_parts & 0x40 != 0,
            display_name: None,
            list_order: 0,
            chat_mode: self.info.chat_mode,
        }
    }

    pub async fn coordinator(&self, message: CoordinatorMessage) {
        let _ = self.shared.coordinator.send(message).await;
    }

    /// Sends the Login packet; its hardcore flag is the only way a client
    /// learns the mode, so it is also how a mid-play change reaches one.
    pub fn send_login(&mut self) {
        let shared = self.shared.clone();
        let dimension = shared.dimension(self.dimension);
        let hardcore = shared.hardcore.load(std::sync::atomic::Ordering::Relaxed);
        self.mode.client_hardcore = hardcore;
        let last_death = self.mode.last_death.clone();
        self.send(&play::Login {
            entity_id: self.id,
            dimensions: &shared.dimensions.keys(),
            max_players: shared.live.max_players() as i32,
            view_distance: shared.live.view_distance(),
            simulation_distance: shared.live.simulation_distance(),
            dimension_type: dimension.type_id(),
            dimension: &dimension.key,
            game_mode: self.mode.game_mode,
            previous_game_mode: self.mode.previous_game_mode,
            sea_level: dimension.sea_level(),
            online_mode: shared.online_mode,
            reduced_debug_info: shared.rule_bool("reduced_debug_info"),
            hardcore,
            last_death: last_death.as_ref().map(|(d, p)| (d.as_str(), *p)),
            is_debug: dimension.is_debug(),
            is_flat: dimension.is_flat(),
            portal_cooldown: self.actions.portal_cooldown.min(i32::MAX as u32) as i32,
        });
    }

    fn send_server_data(&mut self) {
        let Some(list) = self.shared.server_list.get() else {
            return;
        };
        let info = list.borrow().clone();
        let icon = info.favicon.as_ref().map(|f| f.png());
        self.send(&play::ServerData {
            motd: &info.motd,
            icon: icon.as_deref(),
        });
    }

    /// Re-sends Login and the world when the hardcore property changed
    /// under a living player; a dead one is brought up to date on respawn.
    pub async fn sync_hardcore(&mut self) {
        let hardcore = self
            .shared
            .hardcore
            .load(std::sync::atomic::Ordering::Relaxed);
        if hardcore == self.mode.client_hardcore || self.is_dead() {
            return;
        }
        self.send_login();
        let (dimension, position, yaw, pitch) = (
            self.dimension,
            self.pose.position,
            self.pose.yaw,
            self.pose.pitch,
        );
        self.relocate(dimension, position, yaw, pitch, play::respawn::KEEP_NOTHING)
            .await;
    }

    pub async fn enter(&mut self) {
        self.await_client_load();
        let shared = self.shared.clone();
        self.send_login();
        self.send_server_data();
        self.send(&play::ChangeDifficulty {
            difficulty: shared.difficulty().id(),
            locked: shared
                .difficulty_locked
                .load(std::sync::atomic::Ordering::Relaxed),
        });
        self.send_abilities();
        self.send(&play::SetHeldSlot(self.inventory.selected() as i32));
        self.send_permission_level();
        let (written, announced) = oneshot::channel();
        self.coordinator(CoordinatorMessage::Spawned {
            player: self.id,
            entry: self.info_entry(),
            listed: self.info.allow_server_listings,
            written,
        })
        .await;
        let _ = announced.await;
        self.published.last_equipment = self.equipment_snapshot();
        self.send_world().await;
        log::info!("{} joined the game", self.profile.name);
    }

    pub fn send_abilities(&mut self) {
        self.send(&play::PlayerAbilities {
            flags: self.mode.abilities.flags(),
            flying_speed: self.mode.fly_speed,
            fov_modifier: 0.1,
        });
    }

    /// The permission level unlocks the game mode switcher client-side; the
    /// command tree lists what the level allows.
    pub fn send_permission_level(&mut self) {
        self.send(&play::EntityEvent {
            entity_id: self.id,
            status: play::entity_event::PERMISSION_LEVEL_ALL + self.mode.permission.id(),
        });
        let access = self.shared.permissions.access(self.permission_subject());
        let extra = self.shared.commands.client_roots().get();
        let commands = self.shared.commands.roots().with_commands_packet(
            &access,
            Some(&self.shared.datapacks.features()),
            &extra,
            |packet| framed(packet),
        );
        self.send_raw(commands);
    }

    pub fn exhaust(&mut self, amount: f32) {
        if !self.mode.abilities.invulnerable {
            self.body.food.add_exhaustion(amount);
        }
    }

    pub fn system_message(&mut self, content: &TextComponent) {
        self.send(&play::SystemChat {
            content,
            overlay: false,
        });
    }

    /// Whether nothing overlaps the player and solid ground lies within one
    /// block below.
    fn near_ground(&self) -> bool {
        let [x, y, z] = self.pose.position;
        let half = crate::world::PLAYER_HALF_WIDTH;
        let bounds = [
            x - half,
            y,
            z - half,
            x + half,
            y + crate::world::PLAYER_HEIGHT,
            z + half,
        ];
        let mirror = &self.view.mirror;
        mirror.colliders(bounds).is_empty()
            && !mirror
                .colliders([bounds[0], y - 1.0, bounds[2], bounds[3], y, bounds[5]])
                .is_empty()
    }

    /// Returns false if the player was already in `mode`.
    pub async fn set_game_mode(&mut self, mode: GameMode) -> bool {
        if mode == self.mode.game_mode {
            return false;
        }
        let from = self.mode.game_mode;
        if !self.allows_game_mode_hooks(mode) {
            self.game_mode_changed_hooks(from, mode, true);
            return false;
        }
        self.game_mode_changed_hooks(from, mode, false);
        self.mode.previous_game_mode = Some(self.mode.game_mode);
        self.mode.game_mode = mode;
        if mode == GameMode::Spectator {
            self.drop_shoulder_entities().await;
        }
        if mode == GameMode::Creative {
            self.body.fall.reset_impulse();
        }
        let near_ground = self.near_ground();
        self.mode.abilities =
            Abilities::new(mode, self.mode.abilities.flying).landed(mode, near_ground);
        self.send(&play::GameEvent {
            event: play::game_event::CHANGE_GAME_MODE,
            value: mode.id() as f32,
        });
        self.send_abilities();
        self.coordinator(CoordinatorMessage::Info {
            player: self.id,
            actions: play::player_info::UPDATE_GAME_MODE,
            entry: self.info_entry(),
            listed: self.info.allow_server_listings,
        })
        .await;
        let mut out = Vec::new();
        let mut entities = std::mem::take(&mut self.view.entities);
        entities.refresh(&self.area(), self.tick, &mut out);
        self.view.entities = entities;
        self.send_all(out);
        if !self.is_dead() {
            self.publish(true).await;
        }
        log::info!("{} set game mode to {}", self.profile.name, mode.name());
        true
    }

    pub async fn play(&mut self) {
        let tick_member = self.shared.tick_barrier.join(*self.conn.clock.borrow());
        let mut client_roots = self.shared.commands.client_roots().subscribe();
        client_roots.mark_unchanged();
        while !self.disconnected {
            self.settle_save_barrier().await;
            if let Some(next) = self.next_deferred() {
                match next {
                    menus::Deferred::Packet(packet) => self.on_packet(packet).await,
                    menus::Deferred::Event(event) => self.on_event(*event).await,
                }
                continue;
            }
            let keep_alive_due = self.net.keep_alive_due();
            let idle_due = self.net.idle_due();
            tokio::select! {
                biased;
                event = self.conn.events.recv() => match event {
                    Some(event) => self.on_event(event).await,
                    None => break,
                },
                packet = self.conn.packets.recv() => match packet {
                    Some(packet) => self.on_packet(packet).await,
                    None => break,
                },
                changed = client_roots.changed() => {
                    if changed.is_ok() {
                        self.send_permission_level();
                    }
                }
                _ = tokio::time::sleep_until(keep_alive_due) => self.check_keep_alive(),
                _ = tokio::time::sleep_until(idle_due), if self.net.idle_timeout.is_some() => self.check_idle(),
                changed = self.conn.server_ticks.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    self.tick = *self.conn.server_ticks.borrow_and_update();
                    let game_tick = *self.conn.clock.borrow_and_update();
                    self.on_tick().await;
                    tick_member.finished(game_tick);
                }
            }
        }
    }

    pub async fn leave(&mut self) {
        self.progress.stats.award_custom(CustomStat::LeaveGame, 1);
        self.disconnected = true;
        self.close_menu(false).await;
        if let Some(menu) = self.menus.menu.as_mut() {
            menu.pending = None;
        }
        self.finish_menu_close().await;
        for stack in self.inventory.close() {
            self.throw_item(stack).await;
        }
        self.published.seq += 1;
        self.leave_vehicle_offline().await;
        self.leaving_hooks().await;
        self.detach_from_regions().await;
        self.keep_granted_items().await;
        let deferred = self
            .menus
            .save_barrier
            .take()
            .and_then(|barrier| barrier.ticket);
        let _ = self.save_task_in(deferred).await;
        self.shared
            .conservation
            .release(crate::world::conservation::Subject::Player(self.id));
        log::info!("{} left the game", self.profile.name);
    }

    /// Stops taking events and keeps every item a region already gave
    /// up for this player, so a pickup granted or a transaction answered
    /// while leaving is saved, and hands a late two-region vote's stacks
    /// back to its region.
    async fn keep_granted_items(&mut self) {
        self.conn.events.close();
        while let Ok(event) = self.conn.events.try_recv() {
            let stacks = match event {
                PlayerEvent::ItemGranted { stack, .. }
                | PlayerEvent::HandReturned {
                    stack,
                    creative: false,
                    ..
                } => vec![stack],
                PlayerEvent::HandReturned { stack, .. } => {
                    stack.burn(tuff_item::item::BurnReason::Creative);
                    Vec::new()
                }
                PlayerEvent::Transaction { pool, .. }
                | PlayerEvent::EntityTransaction { pool, .. } => pool,
                PlayerEvent::TransactionPrepared {
                    txn,
                    contents,
                    taken,
                    ..
                } => match self.abandoned_vote(txn, &contents) {
                    Some(region) => {
                        self.resolve_share(region, txn, false, taken).await;
                        Vec::new()
                    }
                    None => taken,
                },
                _ => Vec::new(),
            };
            for stack in stacks {
                if let Some(lost) = self.inventory.add(stack) {
                    log::warn!("{} lost {lost:?} granted while leaving", self.profile.name);
                }
            }
        }
    }

    /// Starts a Riptide spin: for a second, what the player runs into
    /// gets hit.
    pub fn start_spin_attack(&mut self) {
        self.motion.spin_attack_ticks = SPIN_ATTACK_TICKS;
        self.body.shoulders.spun = true;
    }

    pub fn max_health(&self) -> f32 {
        self.attribute_value(Attribute::MaxHealth).max(1.0) as f32
    }

    /// The full value of `attribute`: its base with the commands', gear's
    /// and effects' modifiers.
    pub fn attribute_value(&self, attribute: Attribute) -> f64 {
        let worn = self.worn();
        let effects = self.effect_levels();
        attributes::Modifiers {
            equipment: &worn,
            effects: &effects,
        }
        .value_from(
            attribute,
            self.body.attributes.base(attribute),
            &self.body.attributes.extras(attribute),
        )
    }

    pub async fn on_tick(&mut self) {
        let _timer = tuff_timings::timer!("session.tick");
        self.menus.creative_drops = self.menus.creative_drops.saturating_sub(1);
        self.flush_arrow_kills();
        if self.hooks_suspended() {
            self.publish_selector_data();
            return;
        }
        self.drive_fake().await;
        self.tick_combat();
        self.body.fall.tick_impulse();
        self.tick_floating();
        if !self.is_dead() {
            self.body.health.tick();
            let max_health = self.max_health();
            if self.body.health.value > max_health {
                self.body.health.value = max_health;
            }
            let food_before = self.body.food.level;
            let natural_regen = self.shared.rule_bool("natural_health_regeneration");
            let food_effect = self.body.food.tick(
                self.body.health.value,
                max_health,
                self.shared.difficulty(),
                natural_regen,
            );
            self.food_changed_hooks(food_before);
            match food_effect {
                FoodEffect::Heal(amount) => {
                    self.body.health.heal(amount, max_health);
                }
                FoodEffect::Starve => {
                    self.damage(1.0, &damage_type::STARVE, None).await;
                }
                FoodEffect::None => {}
            }
            if self.pose.position[1] < self.dimension().void_y() {
                self.damage(VOID_DAMAGE, &damage_type::OUT_OF_WORLD, None)
                    .await;
            }
            self.tick_fire().await;
            let fire_before = self.body.fire_ticks;
            self.tick_lava().await;
            self.tick_block_contact().await;
            self.tick_in_fire().await;
            self.settle_fire(fire_before).await;
            self.tick_freezing().await;
            self.body.stuck_arrows.tick();
            self.motion.spin_attack_ticks = self.motion.spin_attack_ticks.saturating_sub(1);
            self.ride.boarding_cooldown = self.ride.boarding_cooldown.saturating_sub(1);
            self.update_swimming();
            self.tick_air().await;
            self.tick_shoulders().await;
        }
        let main_hand = self.inventory.hand(false).map(|stack| stack.item());
        if main_hand != self.actions.last_main_hand {
            self.actions.last_main_hand = main_hand;
            self.actions.attack_reset_tick = self.tick;
        }
        let status = (
            self.body.health.value,
            self.body.food.level,
            self.body.food.saturation == 0.0,
        );
        if self.published.last_sent_status != Some(status) {
            self.published.last_sent_status = Some(status);
            self.send(&play::SetHealth {
                health: self.body.health.value,
                food: self.body.food.level,
                saturation: self.body.food.saturation,
            });
        }
        self.send_experience();
        self.tick_sleep().await;
        self.tick_effects().await;
        self.tick_lodestone_compasses().await;
        self.progress.warden_tracker.tick();
        self.tick_stats();
        self.tick_scores().await;
        self.run_session_tick_hooks(hooks::SessionTick::Use).await;
        self.tick_riding().await;
        self.tick_driving().await;
        self.tick_gliding().await;
        self.tick_camera().await;
        self.run_session_tick_hooks(hooks::SessionTick::Late).await;
        if !self.is_dead() {
            self.publish(
                self.hooks_want_reports()
                    || self.surroundings.in_lava
                    || self.surroundings.in_fire.is_some()
                    || self.body.fire_ticks > 0
                    || !self.surroundings.contact.is_none(),
            )
            .await;
        }
        self.tick_destroying().await;
        self.tick_menu().await;
        self.resend_reloaded_data();
        self.tick_advancements().await;
        self.send_chunk_batch();
        self.report_state().await;
        self.publish_selector_data();
    }

    /// What predicates read of this player beyond its saved form.
    pub fn live_facts(&self) -> crate::world::predicate::LiveFacts {
        crate::world::predicate::LiveFacts {
            sprinting: self.pose.sprinting,
            swimming: self.pose.swimming,
            input: self.sync.input_flags,
            tick_count: self.tick as i64,
            movement: self.sync.known_movement,
            fall_distance: self.body.fall.distance,
            stats: std::sync::Arc::new((*self.progress.stats).clone()),
        }
    }

    pub fn send_chunk_batch(&mut self) {
        if self.view.queue.is_empty() || self.view.unacked_batches >= self.view.max_unacked_batches
        {
            return;
        }
        let per_tick = self.view.chunks_per_tick;
        self.view.batch_quota = (self.view.batch_quota + per_tick).min(per_tick.max(1.0));
        if self.view.batch_quota < 1.0 {
            return;
        }
        let center = self.view.center;
        self.view.queue.make_contiguous().sort_by_key(|queued| {
            let (dx, dz) = (
                (queued.pos.x - center.x) as i64,
                (queued.pos.z - center.z) as i64,
            );
            dx * dx + dz * dz
        });
        let count = (self.view.batch_quota.floor() as usize).clamp(1, self.view.queue.len());
        self.send(&play::ChunkBatchStart);
        for _ in 0..count {
            let queued = self.view.queue.pop_front().unwrap();
            self.view.sent.insert(queued.pos);
            if let Some(chunk) = queued.snapshot {
                self.view.mirror.insert(queued.pos, chunk);
            }
            if self.fake.is_none() {
                self.send_raw(queued.packet);
                self.send_all(queued.followups);
            }
        }
        self.send(&play::ChunkBatchFinished(count as i32));
        self.view.unacked_batches += 1;
        self.view.batch_quota -= count as f32;
    }
}
