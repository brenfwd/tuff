//! What region-level feature behaviour sees and when core calls it.
//!
//! [`RegionWorld`] is a region as behaviour code reads and changes it;
//! `RegionActor` implements it, and features extend it with their own
//! extension traits (`impl<T: RegionWorld + ?Sized> MyExt for T`), so
//! their code reads like methods on the region without reaching into its
//! fields. [`RegionHooks`] are the points core calls features at; a feature
//! registers any number in `Registries::region_hooks`, and they run in
//! registration order. Per-region state a feature keeps (timers, cooldowns,
//! found block entities) lives in [`RegionState`], not in the actor.

/// Blocks a sound of volume one carries; louder ones scale with volume.
pub const SOUND_RANGE: f64 = 16.0;
pub const LEVEL_EVENT_RANGE: f64 = 64.0;
pub const PARTICLE_RANGE: f64 = 32.0;

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;
use tuff_vanilla_data::Block;

use crate::world::Shared;
use crate::world::dimension::{Dimension, DimensionId};
use crate::world::entity::{RegionEntity, TrackedEntity};
use crate::world::growth::GrowthLevel;
use tuff_nbt::Compound;
use tuff_vanilla_data::fluid::LiquidPlacement;
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::{BlockState, Item};
use tuff_world_model::BlockPos;

use crate::world::explosion::Explosion;
use crate::world::game_event::{Context as GameContext, Emission, GameEvent};
use crate::world::messages::{
    ActionOutcome, BoatPlace, BucketUse, Igniter, Packets, Place, PlayerHandle, PlayerId,
    RegionHandle, RespawnSpot,
};
use tuff_item::item::ItemStack;
use tuff_world_model::pos::{ChunkPos, RegionPos};

type RegionJob = Box<dyn FnOnce(&mut dyn RegionWorld) + Send>;

/// Work for a region actor to run on itself.
pub struct RegionTask(RegionJob);

impl RegionTask {
    pub fn new(run: impl FnOnce(&mut dyn RegionWorld) + Send + 'static) -> Self {
        Self(Box::new(run))
    }

    pub fn run(self, world: &mut dyn RegionWorld) {
        (self.0)(world);
    }
}

impl std::fmt::Debug for RegionTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RegionTask")
    }
}

/// A player standing in (or at the edge of) a region.
#[derive(Debug, Clone)]
pub struct PresentPlayer {
    pub id: PlayerId,
    pub position: [f64; 3],
    pub height: f64,
    pub eye_height: f64,
    pub handle: PlayerHandle,
    /// Worn feet, legs, chest and head items.
    pub armor: [Option<Item>; 4],
}

/// A living, non-spectating player a region can see, with what it looks
/// like (game mode, effects, eye height).
#[derive(Debug, Clone)]
pub struct SeenPlayer {
    pub handle: PlayerHandle,
    pub uuid: u128,
    pub name: String,
    pub position: [f64; 3],
    pub looks: crate::world::messages::PlayerLooks,
}

/// What a dispenser did with an item a hook fired.
#[derive(Debug, PartialEq)]
pub enum DispenseOutcome {
    /// Used one of the item in place (the plain dispense sound).
    Dispensed,
    /// Launched one as a projectile, with `sound` as the level event
    /// played (the dispenser launch sound when `None`).
    Launched { sound: Option<i32> },
    /// Acted without using the item up (the plain dispense sound).
    Kept,
    /// Acted and wore the item by this many points of durability.
    Worn(u32),
    /// Used one of the item and got this back (an empty bucket, a filled
    /// bottle), which goes into the dispenser or, failing that, out of its
    /// face.
    Exchanged(ItemStack),
    /// Nothing to act on; the item stays (the fail click).
    Failed,
    /// Nothing to act on; the item drops out as a plain dispense does.
    Dropped,
    /// [`Self::Dropped`], but with the fail click (a glass bottle with no
    /// water in front).
    DroppedFailing,
    /// The target block is a neighbour region's: the dispenser forwards the
    /// item there and applies what that region answers.
    Remote,
}

/// What a player did to a block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockAction {
    Broke,
    Placed,
    /// Opened its container.
    Opened,
}

/// A player's right click that a block reacted to (a door, a lever, a cake).
#[derive(Clone, Copy, Debug)]
pub struct PlayerBlockUse {
    pub player: PlayerId,
    pub pos: BlockPos,
    /// The block as it was clicked.
    pub old: BlockState,
    /// The block after the use, `None` when it is no longer loaded here.
    pub new: Option<BlockState>,
}

/// A player's sign edit, with the whole block entity before and after.
#[derive(Clone, Debug)]
pub struct SignEdit {
    pub player: PlayerId,
    pub pos: BlockPos,
    pub state: BlockState,
    pub front: bool,
    pub old: tuff_nbt::Compound,
    pub new: tuff_nbt::Compound,
}

/// A player acting on a block, for features that react to it.
#[derive(Clone, Copy, Debug)]
pub struct PlayerBlockAction {
    pub player: PlayerId,
    pub pos: BlockPos,
    /// The block before a break, after a placement.
    pub state: BlockState,
    pub action: BlockAction,
}

/// What a region is doing while blocks change, as [`RegionHooks::block_changed`]
/// reports it. Core sets it around player actions, block and fluid ticks,
/// block events, entity effects, weather and explosions; reactions a change
/// sets off (a torch losing its wall) keep the cause of what started them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlockCause {
    /// Nothing more specific: commands, world edits, generation, updates
    /// deferred to a later tick.
    #[default]
    World,
    /// A player's break, placement, click or bucket.
    Player(PlayerId),
    /// An entity's own doing (an enderman, a landing falling block, a
    /// lightning bolt).
    Entity {
        kind: tuff_vanilla_data::registry::EntityType,
        uuid: u128,
    },
    /// A block's own tick or event: fire spreading, leaves decaying, a
    /// sapling growing, sand starting to fall, water or lava flowing (their
    /// source block), a piston pushing, a dispenser firing.
    Block(Block),
    /// Snow piling up and water freezing under the sky.
    Weather,
    /// An explosion, with the player who lit it or the entity that set it
    /// off.
    Explosion {
        igniter: Option<u128>,
        culprit: Option<tuff_vanilla_data::registry::EntityType>,
    },
}

impl BlockCause {
    pub fn explosion(cause: &crate::world::explosion::ExplosionCause) -> BlockCause {
        BlockCause::Explosion {
            igniter: cause.igniter.as_ref().map(|igniter| igniter.uuid),
            culprit: cause.culprit.as_ref().map(|culprit| culprit.kind),
        }
    }

    pub fn entity(entity: &dyn RegionEntity) -> BlockCause {
        let base = entity.base();
        BlockCause::Entity {
            kind: base.kind,
            uuid: base.uuid,
        }
    }
}

/// One block about to change in a region that owns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockChange {
    pub pos: BlockPos,
    pub old: BlockState,
    pub new: BlockState,
    pub cause: BlockCause,
}

/// What holds a container's slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContainerHolder {
    /// A block entity (one half of a double chest is its own holder).
    Block(BlockPos),
    /// An entity's inventory or held items (a chest boat, an item frame),
    /// at the block it stands in.
    Entity {
        uuid: u128,
        kind: tuff_vanilla_data::registry::EntityType,
        pos: BlockPos,
    },
}

impl ContainerHolder {
    pub fn pos(&self) -> BlockPos {
        match *self {
            ContainerHolder::Block(pos) | ContainerHolder::Entity { pos, .. } => pos,
        }
    }

    pub fn entity(entity: &dyn RegionEntity) -> ContainerHolder {
        let base = entity.base();
        let [x, y, z] = base.position;
        ContainerHolder::Entity {
            uuid: base.uuid,
            kind: base.kind,
            pos: BlockPos::new(x.floor() as i32, y.floor() as i32, z.floor() as i32),
        }
    }
}

/// One slot of a container before and after a change.
#[derive(Debug, PartialEq)]
pub struct SlotEdit {
    pub slot: usize,
    pub old: Option<tuff_item::item::ItemSnapshot>,
    pub new: Option<tuff_item::item::ItemSnapshot>,
}

impl SlotEdit {
    pub fn of(slot: usize, old: Option<&ItemStack>, new: Option<&ItemStack>) -> Self {
        Self {
            slot,
            old: old.map(tuff_item::item::ItemSnapshot::of),
            new: new.map(tuff_item::item::ItemSnapshot::of),
        }
    }
}

/// Container slots that just changed together (one click, one hopper
/// move), as [`RegionHooks::container_changed`] reports them, with the
/// cause in effect (the clicking player, `Block(Hopper)` for a hopper).
#[derive(Debug, PartialEq)]
pub struct ContainerEdit {
    pub holder: ContainerHolder,
    pub slots: Vec<SlotEdit>,
    pub cause: BlockCause,
}

/// Who took a dropped item.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemTaker {
    Player(PlayerId),
    Entity {
        kind: tuff_vanilla_data::registry::EntityType,
        uuid: u128,
    },
}

/// A dropped item entity picked up, as [`RegionHooks::item_picked_up`]
/// reports it.
#[derive(Debug)]
pub struct ItemPickup {
    pub taker: ItemTaker,
    pub stack: tuff_item::item::ItemSnapshot,
    pub position: [f64; 3],
    pub thrower: Option<u128>,
}

/// A region entity that died, as [`RegionHooks::entity_died`] reports it.
#[derive(Clone, Debug)]
pub struct EntityDeath {
    pub id: i32,
    pub uuid: u128,
    pub kind: tuff_vanilla_data::registry::EntityType,
    pub position: [f64; 3],
    /// Its saved form at the moment of death (`id` included).
    pub nbt: Compound,
    /// The player to blame, by UUID.
    pub player: Option<u128>,
    /// The entity to blame (a player too).
    pub attacker: Option<tuff_vanilla_data::registry::EntityType>,
    /// The damage type's name.
    pub damage: &'static str,
}

impl dyn RegionWorld + '_ {
    /// Runs `f` with block changes attributed to `cause`, then restores the
    /// cause it replaced.
    pub fn caused<R>(&mut self, cause: BlockCause, f: impl FnOnce(&mut Self) -> R) -> R {
        let previous = self.set_block_cause(cause);
        let result = f(self);
        self.set_block_cause(previous);
        result
    }
}

/// What `RegionWorld::insert_item` did with the stack.
#[derive(Debug, PartialEq)]
pub enum Insert {
    /// Nothing there takes items; the stack comes back untouched.
    Nothing(ItemStack),
    /// All of it went in.
    Stored,
    /// The target took part or none; this is what is left.
    Refused(ItemStack),
}

/// Fires a dispenser or dropper at `pos` once per redstone activation.
pub trait Dispensers: Send + Sync {
    fn fire(&self, world: &mut dyn RegionWorld, pos: BlockPos);
}

/// How far a bell's ring carries.
pub const BELL_HEARING: f64 = 32.0;

pub trait RegionWorld: GrowthLevel {
    fn shared(&self) -> &Arc<Shared>;
    /// This world as a trait object, for APIs taking `&mut dyn RegionWorld`.
    fn as_dyn(&mut self) -> &mut dyn RegionWorld;
    /// Puts `stack` into whatever takes items at `target` through `face`: a
    /// modeled container, else an item port, under the current cause.
    fn insert_item(
        &mut self,
        target: BlockPos,
        face: Option<tuff_vanilla_data::block::Facing>,
        stack: ItemStack,
        by: crate::world::item_port::Porter,
    ) -> Insert;
    /// The game tick this region last ran.
    fn tick(&self) -> u64;
    /// Ticks players have spent near a loaded chunk (zero elsewhere).
    fn inhabited_time(&self, chunk: ChunkPos) -> u64;
    fn region_pos(&self) -> RegionPos;
    /// Runs `task` at the start of the next tick of each loaded region
    /// `area` reaches (this one only when `local`); returns its send order.
    /// Use [`crate::world::area::broadcast`] and [`crate::world::area::query`].
    fn area_run(
        &mut self,
        area: crate::world::area::Area,
        local: bool,
        task: crate::world::area::AreaTask,
    ) -> u64;
    /// Tells this region's idle mobs of `kinds` within `area` to attack
    /// `attacker`.
    fn alert_entities(
        &mut self,
        kinds: &'static [tuff_vanilla_data::registry::EntityType],
        area: [f64; 6],
        attacker: i32,
    );
    /// `/kill` on one entity: gone at once, or dying its own way. False if
    /// there is no such entity here.
    fn kill_entity(&mut self, id: i32) -> bool;
    /// A melee swing by entity `id` at the player or entity `victim`; false
    /// if there is no such entity here, it cannot strike or the victim is
    /// out of its reach.
    fn strike_entity(&mut self, id: i32, victim: u128) -> bool;
    /// `/damage` on one entity; false if there is no such entity here or
    /// it is invulnerable to the damage.
    fn hurt_entity(
        &mut self,
        id: i32,
        kind: &'static tuff_vanilla_data::damage_type::DamageType,
        amount: f32,
        culprit: Option<crate::world::entity::Culprit>,
    ) -> bool;
    /// `/data` on one entity: reloads it from `nbt`, keeping its network
    /// id. False if `nbt` does not load or there is no such entity here.
    fn replace_entity(&mut self, id: i32, nbt: tuff_nbt::Compound) -> bool;
    /// `/tag` on one entity; false if nothing changed.
    fn edit_entity_tags(&mut self, id: i32, edit: &crate::world::messages::TagEdit) -> bool;
    /// `/effect` on one entity; false if nothing changed.
    fn effect_entity(&mut self, id: i32, command: &crate::world::messages::EffectCommand) -> bool;
    /// `/enchant` on one entity; `None` if there is no such entity here.
    fn enchant_entity(
        &mut self,
        id: i32,
        enchantment: &str,
        level: i32,
    ) -> Option<crate::world::messages::EnchantAnswer>;
    /// `/swing` on one entity: `None` if there is no such entity here,
    /// `Some(false)` if it is not a living entity.
    fn swing_entity(
        &mut self,
        id: i32,
        hand: i32,
        kind: tuff_item::item::SwingKind,
        duration: i32,
    ) -> Option<bool>;
    /// `/debugpath` on one entity; `None` if there is no such entity here.
    fn debug_path_entity(
        &mut self,
        id: i32,
        target: tuff_world_model::BlockPos,
    ) -> Option<crate::world::entity::DebugPath>;
    /// `/attribute` on one entity; `None` if there is no such entity here.
    fn attribute_entity(
        &mut self,
        id: i32,
        command: &crate::world::messages::AttributeCommand,
    ) -> Option<crate::world::messages::AttributeAnswer>;
    /// `/tp` on one entity within this dimension.
    fn teleport_entity(&mut self, id: i32, teleport: &crate::world::messages::Teleport) -> bool;
    /// Moves one entity within this region as a step of its own: motion is
    /// cleared and viewers get relative moves. False when it is not here or
    /// the move would leave the region.
    fn glide_entity(&mut self, id: i32, position: [f64; 3], rotation: Option<[f32; 2]>) -> bool;
    /// `/tp` to another dimension: removes the entity and gives its saved form.
    fn take_teleported_entity(
        &mut self,
        id: i32,
        teleport: &crate::world::messages::Teleport,
    ) -> Option<tuff_nbt::Compound>;
    /// One `/ride` step for the entities of this region.
    fn ride(&mut self, op: crate::world::messages::RideOp) -> crate::world::messages::RideAnswer;
    fn dimension_id(&self) -> DimensionId;
    fn dimension_info(&self) -> &Dimension;
    /// Whether this region owns `chunk` (not just sees it in its halo).
    fn owns_chunk(&self, chunk: ChunkPos) -> bool {
        self.region_pos().contains(chunk)
    }

    /// Chunks that are lit and whose neighbours are loaded, in a fixed
    /// order: where random ticks and weather run.
    fn lit_ready_chunks(&self) -> Vec<ChunkPos>;
    /// Every chunk loaded here (owned, not the halo).
    fn loaded_chunks(&self) -> Vec<ChunkPos>;
    /// Whether `chunk` is owned here, loaded, lit and not unloading.
    fn chunk_lit(&self, chunk: ChunkPos) -> bool;
    /// A loaded chunk's saved data this region does not model (structure
    /// references and starts, ...).
    fn chunk_extra(&self, chunk: ChunkPos) -> Option<&Compound>;
    /// The biome name at `pos`, here or in the halo.
    fn biome_name(&self, pos: BlockPos) -> Option<&'static str>;
    /// The highest non-air block of a column loaded here, plus one.
    fn world_surface(&self, x: i32, z: i32) -> Option<i32>;
    /// Sky and block light as the light engine has it now, `None` while
    /// unknown or while the engine is away on a job.
    fn engine_light(&self, pos: BlockPos) -> Option<(u8, u8)>;
    /// Whether the light engine is here (not away on a job).
    fn lighting_available(&self) -> bool;
    /// A Block Event (spawner flames, chest lids, ...) to the block's
    /// viewers.
    fn block_event(&self, pos: BlockPos, action: u8, parameter: u8);
    /// The game's cheap random block position in a 16-wide cube.
    fn random_block_pos(&mut self, x: i32, y: i32, z: i32, y_mask: i32) -> BlockPos;

    /// Players physically present here (not spectators).
    fn present_players(&self) -> Vec<PresentPlayer>;
    /// Positions of every living player this region's entities see: those
    /// present, and those the player index lists near this region.
    fn seen_player_positions(&mut self) -> Vec<[f64; 3]>;
    /// The sessions and positions of those players.
    fn seen_players(&mut self) -> Vec<(PlayerHandle, [f64; 3])>;
    /// [`Self::seen_players`] with their UUIDs and looks.
    fn seen_player_details(&mut self) -> Vec<SeenPlayer>;

    /// Whether rain falls on a thing standing at `position` whose top is
    /// at `top`.
    fn rained_on(&self, position: [f64; 3], top: f64) -> bool;
    /// This region's entity ids, in ascending order.
    fn entity_ids(&self) -> Vec<i32>;
    /// The ids of this region's entities of any of `kinds`, ascending.
    fn entity_ids_of(&self, kinds: &[tuff_vanilla_data::registry::EntityType]) -> Vec<i32>;
    fn entity(&self, id: i32) -> Option<&TrackedEntity>;
    fn entity_mut(&mut self, id: i32) -> Option<&mut TrackedEntity>;
    /// Rings the bell at `pos`, struck from `face` (else its facing): it
    /// swings and sounds, living things within hearing here and in the
    /// neighbouring regions take note, and features hear of it.
    fn ring_bell(&mut self, pos: BlockPos, face: Option<tuff_vanilla_data::block::Facing>);
    /// A bell at `pos` rang: every living entity of this region within
    /// hearing takes note.
    fn bell_rung(&mut self, pos: BlockPos) {
        let center = pos.center();
        for id in self.entity_ids() {
            let Some(tracked) = self.entity_mut(id) else {
                continue;
            };
            let heard = tracked.entity.living().is_some_and(|l| l.alive)
                && crate::world::distance_sq(tracked.entity.base().position, center)
                    < BELL_HEARING * BELL_HEARING;
            if heard {
                tracked
                    .entity
                    .notice(crate::world::entity::Notice::BellRung);
            }
        }
    }
    /// Takes in a new entity (handing it on if it belongs to a neighbour).
    fn add_entity(&mut self, entity: Box<dyn RegionEntity>);
    /// Applies an entity's hit to a player or entity of this region, as a
    /// mob's melee hit lands.
    fn hit_entity(&mut self, target: i32, hit: crate::world::entity::EntityHit);
    /// Shears the first entity in the block at `pos` that has something
    /// to shear (a dispenser's shears); false if none had.
    fn shear_entity_in(&mut self, pos: BlockPos) -> bool;
    /// Offers `stack` (a dispenser's) to each entity in the block at
    /// `pos` until one uses it.
    fn dispense_on_entity_in(
        &mut self,
        pos: BlockPos,
        stack: &ItemStack,
    ) -> Option<DispenseOutcome>;
    /// Puts one of `stack` (a dispenser's wearable) on an entity, else on a
    /// player with that armor slot free, standing in the block at `pos`;
    /// `None` if nobody there takes it.
    fn dispense_equip_in(&mut self, pos: BlockPos, stack: &ItemStack) -> Option<DispenseOutcome> {
        use tuff_vanilla_data::item::EquipmentSlot;
        if let Some(outcome) = self.dispense_on_entity_in(pos, stack) {
            return Some(outcome);
        }
        let equippable = stack.equippable()?;
        if !equippable.dispensable
            || stack.restricted_for(tuff_vanilla_data::registry::EntityType::Player)
        {
            return None;
        }
        let index = match equippable.slot {
            EquipmentSlot::Feet => 0,
            EquipmentSlot::Legs => 1,
            EquipmentSlot::Chest => 2,
            EquipmentSlot::Head => 3,
            _ => return None,
        };
        let half = crate::world::PLAYER_HALF_WIDTH;
        let (x0, y0, z0) = (f64::from(pos.x), f64::from(pos.y), f64::from(pos.z));
        let player = self.present_players().into_iter().find(|p| {
            let [x, y, z] = p.position;
            x + half > x0
                && x - half < x0 + 1.0
                && y + p.height > y0
                && y < y0 + 1.0
                && z + half > z0
                && z - half < z0 + 1.0
                && p.armor[index].is_none()
        })?;
        player
            .handle
            .send(crate::world::messages::PlayerEvent::DispenserEquip {
                slot: equippable.slot,
                stack: stack.dup_count_unchecked(1),
            });
        Some(DispenseOutcome::Dispensed)
    }
    /// Takes an entity out of the region (no death, no drops), burning
    /// the items still in it.
    fn remove_entity(&mut self, id: i32) -> Option<TrackedEntity>;
    /// Like `remove_entity` for an entity whose items live on: it is
    /// saved elsewhere (a hive, a pearl list) or moved to another region.
    fn remove_entity_intact(&mut self, id: i32) -> Option<TrackedEntity>;
    /// Like `remove_entity` for an entity that lives on elsewhere: its
    /// scores and team stay.
    fn remove_entity_kept(&mut self, id: i32) -> Option<TrackedEntity>;
    /// Runs a freshly created entity's spawn setup (`finalize_spawn`) with
    /// a tick context, applying what it asks for.
    fn finalize_spawn(&mut self, entity: &mut Box<dyn RegionEntity>, index: u32);

    /// The strongest redstone signal reaching `pos` from its neighbours.
    fn best_neighbour_signal(&self, _pos: BlockPos) -> u8 {
        0
    }

    /// Saved block entity data this region keeps raw, by position.
    fn raw_block_entity(&self, pos: BlockPos) -> Option<&Compound>;
    /// The same, to change; marks the chunk for saving.
    fn raw_block_entity_mut(&mut self, pos: BlockPos) -> Option<&mut Compound>;
    /// Positions of raw block entities in loaded (not unloading) chunks
    /// that `matches` accepts.
    fn raw_block_entities_where(&self, matches: &dyn Fn(&Compound) -> bool) -> Vec<BlockPos>;
    /// [`Self::raw_block_entities_where`] limited to the cube of `radius`
    /// blocks around `center`, ordered by y, z, then x.
    fn raw_block_entities_near(
        &self,
        center: BlockPos,
        radius: i32,
        matches: &dyn Fn(&Compound) -> bool,
    ) -> Vec<BlockPos>;
    /// Changes whenever a block entity may have appeared or gone (a block
    /// that has one changed, or a chunk loaded or unloaded), so a scan of
    /// block entities can be reused until it changes.
    fn block_entity_generation(&self) -> u64;

    /// Marks the chunk holding `pos` as changed, so it is saved.
    fn mark_blocks_dirty(&mut self, pos: BlockPos);
    /// The raw block entity at `pos` changed: save it with its chunk and
    /// send viewers its client data.
    fn block_entity_changed(&mut self, pos: BlockPos);
    /// A loaded chunk this region owns.
    fn chunk(&self, pos: ChunkPos) -> Option<&tuff_world_model::chunk::Chunk>;
    /// This region's own handle, for async work to post back to it with
    /// `RegionInternal::Run`.
    fn handle(&self) -> RegionHandle;
    /// Moves entity `id` to `position`, handing it to `target` (the region
    /// owning that spot) when that is not this one. `still` stops it and
    /// faces it forward, as an ender pearl comes out of a gateway. If the
    /// target cannot take it, it stays where it was.
    fn relocate_entity(&mut self, id: i32, position: [f64; 3], target: RegionHandle, still: bool);

    /// Whether this region owns the block at `pos`.
    fn owns(&self, pos: BlockPos) -> bool;
    /// A wind burst reached the block at `pos`: a lever flips, a button
    /// presses. Nothing happens unless this region owns it.
    fn wind_reached(&mut self, pos: BlockPos);
    /// A projectile struck the block at `hit.pos`: the block reacts here,
    /// or in the neighbour that owns it.
    fn projectile_hit_block(
        &mut self,
        hit: crate::world::raycast::Hit,
        impact: &crate::world::entity::ProjectileImpact,
    );
    /// A projectile struck the target block at `hit.pos`: it emits a signal
    /// the stronger the nearer the hit is to the face's centre.
    fn target_hit(
        &mut self,
        hit: crate::world::raycast::Hit,
        impact: &crate::world::entity::ProjectileImpact,
    );
    /// Awards `amount` of `stat` to player `id` when it is seen from this
    /// region.
    fn award_stat(&mut self, id: i32, stat: tuff_vanilla_data::registry::CustomStat, amount: i32);
    /// Sets a block with full updates, forwarding it to the owning
    /// neighbour when it is not here.
    fn put_block(&mut self, pos: BlockPos, state: BlockState);
    /// Places `state` for the block item `stack` at `pos` as a dispenser
    /// does: the item's components go to the block entity, the place sound
    /// plays. False when it is not this region's or a player is in the way.
    fn place_item_block(&mut self, pos: BlockPos, state: BlockState, stack: &ItemStack) -> bool;
    /// Sets a block without neighbour updates or placement hooks,
    /// forwarding it to the owning neighbour when it is not here.
    fn set_block_anywhere(&mut self, pos: BlockPos, state: BlockState);
    /// Where `block` is in this region's loaded chunks within the box.
    fn find_blocks(&self, block: Block, min: BlockPos, max: BlockPos) -> Vec<BlockPos>;
    /// This region's points of interest of `kind` within `radius` blocks
    /// of `center` (a square in x and z), closest first.
    fn pois_near(
        &self,
        center: BlockPos,
        radius: i32,
        kind: tuff_vanilla_data::registry::PointOfInterestType,
    ) -> Vec<BlockPos>;
    /// This region's points of interest and how claimed they are.
    fn pois(&self) -> &crate::world::poi::PoiStore;
    /// Sets a block here with the game's update flags.
    fn set_block_flags(&mut self, pos: BlockPos, state: BlockState, flags: u32);
    /// Breaks `state` at `pos` as a block update does (it lost its
    /// support), dropping its loot when `drops` is set.
    fn break_by_update(&mut self, pos: BlockPos, state: BlockState, drops: bool);
    /// A block sound at `pos` for the chunk's viewers (but `except`).
    fn play_sound(
        &self,
        sound: SoundEvent,
        pos: BlockPos,
        volume: f32,
        pitch: f32,
        except: Option<PlayerId>,
    );
    /// Packets for the players physically in this region within `range` of
    /// `center`, whatever chunks they view, but those in `skip`.
    fn send_to_players_near(
        &self,
        center: [f64; 3],
        range: f64,
        skip: &[PlayerId],
        packets: Packets,
    );
    /// Packets for the viewers of the chunk at `center` within `range` of
    /// it (but `except`).
    fn send_near(&self, center: [f64; 3], range: f64, except: Option<PlayerId>, packets: Packets);
    /// Packets for everyone viewing `chunk` (but `except`).
    fn send_to_chunk(&self, chunk: ChunkPos, except: Option<PlayerId>, packets: Packets);
    /// The block-break particles and sound at `pos`.
    fn break_event(&self, pos: BlockPos, state: BlockState, except: Option<PlayerId>);
    /// Current block updates for `positions`, to undo a rejected action on
    /// the client.
    fn corrections(&self, positions: &[BlockPos]) -> Packets;
    /// Whether every feature lets `player` break or place `state` at `pos`.
    fn allows_block_action(
        &mut self,
        player: PlayerId,
        pos: BlockPos,
        state: BlockState,
        action: BlockAction,
    ) -> bool;
    /// The handle of the neighbouring region at `pos`, if running.
    fn neighbor(&self, pos: RegionPos) -> Option<RegionHandle>;

    /// Drops `stacks` around the block at `pos`.
    fn drop_items(&mut self, pos: BlockPos, stacks: Vec<ItemStack>);
    /// An item entity at `position` moving at `velocity`.
    fn spawn_item_at(
        &mut self,
        stack: ItemStack,
        position: [f64; 3],
        velocity: [f64; 3],
        pickup_delay: u32,
    );
    /// A block entity's saved data, modeled or raw.
    fn saved_block_entity(&self, pos: BlockPos) -> Option<Compound>;
    /// An infested block broken without silk touch lets its silverfish out.
    fn release_infestation(&mut self, pos: BlockPos, state: BlockState, tool: Option<&ItemStack>);

    /// Sets off an explosion centred here.
    fn explode(&mut self, explosion: Explosion);
    /// The same, with water's resistance at `shield`.
    fn explode_shielded(&mut self, explosion: Explosion, shield: BlockPos);
    /// Lights TNT at `pos` (the block already gone).
    fn spawn_primed_tnt(&mut self, pos: BlockPos, fuse: Option<i32>, igniter: Option<Igniter>);
    /// The sound and pitch of lighting something with `item`.
    fn fire_sound(&mut self, item: Item) -> (SoundEvent, f32);
    /// A level event every player in the dimension hears.
    fn broadcast_level_event(&mut self, event: i32, pos: BlockPos, data: i32);
    /// A level event every player in every dimension hears.
    fn broadcast_global_level_event(&mut self, event: i32, pos: BlockPos, data: i32);
    /// Places a liquid into a block that holds it (waterlogging).
    fn apply_liquid_placement(&mut self, pos: BlockPos, placement: &'static LiquidPlacement);

    /// A redstone component's own reaction to a click (buttons, levers,
    /// repeaters); false if it has none.
    fn use_redstone(&mut self, pos: BlockPos, state: BlockState) -> bool;
    /// Adds `player` to the viewers of the block menu at `pos`.
    fn add_menu_viewer(&mut self, pos: BlockPos, player: PlayerHandle);
    /// The players viewing the block menu at `pos`.
    fn menu_viewers(&self, pos: BlockPos) -> Vec<PlayerHandle>;
    /// Closes every menu open on the block at `pos`.
    fn close_menu_viewers(&mut self, pos: BlockPos);
    /// Whether the player `id` present here is a spectator.
    fn is_spectator(&self, id: PlayerId) -> bool;
    /// A click on a sign (core owns sign editing).
    fn use_sign(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome>;
    /// Wakes a mob sleeping in the bed at `pos`/`foot`; false if none is.
    fn wake_bed_occupant(&mut self, pos: BlockPos, foot: BlockPos) -> bool;

    /// Features' per-region state.
    fn state(&mut self) -> &mut RegionState;
    /// Announces a game event: features in this region hear it, and in
    /// the neighbour regions its radius reaches.
    fn game_event(&mut self, event: GameEvent, pos: [f64; 3], context: GameContext);
    /// Delivers an already announced event to this region's features only.
    fn hear_game_event(&mut self, emission: &Emission);
    /// A player's active mob effects react to its death or hurt: features
    /// hear it.
    fn player_effect(&mut self, event: PlayerEffectEvent);
    /// Runs `f` with an entity tick context over this region's world, as
    /// if an entity at `position` ticked, then carries out the effects it
    /// queued.
    fn with_tick_context(
        &mut self,
        position: [f64; 3],
        f: &mut dyn FnMut(&mut crate::world::entity::TickContext),
    );
    /// Tells the neighbours of `pos` and the block it was that it changed
    /// (a signal source switching on or off).
    fn update_neighbors_at(&mut self, pos: BlockPos, block: Block);
    /// Tells comparators reading `pos` (directly or through a solid block)
    /// that its analog output changed.
    fn update_output_signal(&mut self, pos: BlockPos, block: Block);
    /// The signal the comparator at `pos` holds (0 for anything else).
    fn comparator_output(&self, pos: BlockPos) -> u8;
    /// What block changes are attributed to now.
    fn block_cause(&self) -> BlockCause;
    /// Attributes the block changes that follow to `cause`, returning the
    /// cause it replaces (see `caused`).
    fn set_block_cause(&mut self, cause: BlockCause) -> BlockCause;

    /// Tells the features observing containers that `holder`'s slots
    /// changed from `old` to `new` (`(slot, old, new)`), under the current
    /// cause. For feature code that keeps items itself (item frames,
    /// bookshelves); does nothing when no feature observes containers.
    fn report_container_edit(
        &mut self,
        _holder: ContainerHolder,
        _slots: &[(usize, Option<&ItemStack>, Option<&ItemStack>)],
    ) {
    }

    /// The kind of the modeled block entity at `pos`.
    fn container_kind(&self, _pos: BlockPos) -> Option<crate::world::block_entity::ContainerKind> {
        None
    }

    /// The modeled block entity's own data at `pos`, to change; marks it
    /// for saving.
    fn block_entity_state_mut(
        &mut self,
        _pos: BlockPos,
    ) -> Option<&mut crate::world::block_entity::State> {
        None
    }

    /// The slots of the block container at `pos` this region owns.
    fn container_items(&self, _pos: BlockPos) -> Option<&[Option<ItemStack>]> {
        None
    }

    /// Sets slots of the block container at `pos` and shows its viewers,
    /// unless a transaction holds it; false if nothing was set. The
    /// change is reported to container observers under the current cause.
    fn set_container_items(
        &mut self,
        _pos: BlockPos,
        _slots: Vec<(usize, Option<ItemStack>)>,
    ) -> bool {
        false
    }

    /// Changes the slots of the block container at `pos` in place and
    /// shows its viewers the ones that changed, unless a transaction
    /// holds it; false if nothing could be edited.
    fn edit_container(
        &mut self,
        _pos: BlockPos,
        _edit: &mut dyn FnMut(&mut [Option<ItemStack>]),
    ) -> bool {
        false
    }

    /// Changes slot `slot` of the block container at `pos` in place and
    /// shows its viewers, unless a transaction holds it; false if nothing
    /// could be edited. The change is reported to container observers
    /// under the current cause.
    fn edit_container_slot(
        &mut self,
        _pos: BlockPos,
        _slot: usize,
        _edit: &mut dyn FnMut(&mut Option<ItemStack>),
    ) -> bool {
        false
    }

    /// The container slots of the entity `uuid` in this region (a chest
    /// boat, a minecart).
    fn entity_container_items(
        &self,
        _uuid: u128,
    ) -> Option<crate::world::entity_slots::ContainerSlots<'_>> {
        None
    }

    /// The held or worn slots of the entity `uuid` in this region (an
    /// item frame's item, an armor stand's equipment), indexed as its
    /// held-item edits report them.
    fn entity_held_items(&self, _uuid: u128) -> Option<Vec<Option<tuff_item::item::ItemView<'_>>>> {
        None
    }

    /// Sets held slots of the entity `uuid` and shows its viewers; false
    /// if nothing was set. The change is reported to container observers
    /// under the current cause.
    fn set_entity_held_items(
        &mut self,
        _uuid: u128,
        _slots: Vec<(usize, Option<ItemStack>)>,
    ) -> bool {
        false
    }

    /// Sets slots of the entity `uuid`'s container like
    /// [`Self::set_container_items`].
    fn set_entity_container_items(
        &mut self,
        _uuid: u128,
        _slots: Vec<(usize, Option<ItemStack>)>,
    ) -> bool {
        false
    }
}

/// What happened to a player bearing mob effects that react to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerEffectTrigger {
    Died,
    Hurt,
}

/// A player died or was hurt while bearing one of `effects`.
#[derive(Debug, Clone)]
pub struct PlayerEffectEvent {
    pub trigger: PlayerEffectTrigger,
    pub effects: Vec<tuff_vanilla_data::registry::MobEffect>,
    pub culprit: crate::world::entity::Culprit,
    pub position: [f64; 3],
    pub height: f64,
    pub yaw: f32,
    pub pitch: f32,
}

/// Something that fell landed on the block at `pos`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Landing {
    /// The entity (or player) id that landed, and where it stands.
    pub entity: i32,
    pub position: [f64; 3],
    pub pos: BlockPos,
    /// How far it fell, before any landing block softened it.
    pub fall_distance: f64,
    pub kind: tuff_vanilla_data::registry::EntityType,
    /// Its box width and height.
    pub size: [f64; 2],
    pub living: bool,
    /// A player at gamemaster level, exempt from spawn protection.
    pub gamemaster: bool,
}

/// When in a region tick core runs [`RegionHooks::tick`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickPhase {
    /// After random ticks, with block changes batched (weather).
    Weather,
    /// After entities ticked, with block changes batched (spawning).
    Spawning,
    /// After block entities ticked.
    Late,
}

impl TickPhase {
    pub fn label(self) -> &'static str {
        match self {
            TickPhase::Weather => "weather",
            TickPhase::Spawning => "spawning",
            TickPhase::Late => "late",
        }
    }
}

/// The points core calls region behaviour at. Every method does nothing
/// by default.
pub trait RegionHooks: Send + Sync {
    fn tick(&self, _world: &mut dyn RegionWorld, _phase: TickPhase) {}

    /// The neighbour of `state` at `pos` in `direction` became
    /// `neighbour`: this feature's new state for one of its own blocks (a
    /// portal losing its frame), or `None` to leave it to the placement
    /// rules. `level` reads blocks here or in the halo.
    fn update_shape(
        &self,
        _state: BlockState,
        _pos: BlockPos,
        _direction: tuff_vanilla_data::block::Facing,
        _neighbour: BlockState,
        _level: &dyn Fn(BlockPos) -> BlockState,
        _min_y: i32,
    ) -> Option<BlockState> {
        None
    }

    /// A living player whose home region this is reported where it is.
    fn player_moved(
        &self,
        _world: &mut dyn RegionWorld,
        _update: &crate::world::messages::PlayerUpdate,
    ) {
    }

    /// A player's melee hit on `target` landed (weapon effects such as
    /// setting it alight).
    fn entity_melee_hit(
        &self,
        _target: &mut dyn RegionEntity,
        _attack: &crate::world::entity::EntityAttack,
        _cx: &mut crate::world::entity::TickContext,
    ) {
    }

    /// A dispenser at `pos` facing `facing` fires one of `stack`, from
    /// `origin` just outside its face. `None` leaves it to other hooks;
    /// a dispenser nobody fires for does nothing.
    fn dispense(
        &self,
        _world: &mut dyn RegionWorld,
        _pos: BlockPos,
        _facing: tuff_vanilla_data::block::Facing,
        _origin: [f64; 3],
        _stack: &ItemStack,
    ) -> Option<DispenseOutcome> {
        None
    }

    /// Where a player without a spawn block appears around the world
    /// spawn `center` (a search `radius` wide, starting at `offset`), over
    /// the blocks this region sees. The first answer wins.
    fn spawn_position(
        &self,
        _world: &mut dyn RegionWorld,
        _center: BlockPos,
        _radius: i32,
        _offset: u32,
    ) -> Option<[f64; 3]> {
        None
    }

    /// A player or mob landed on a block (trampling it, say). Runs in the
    /// region the lander is in; block changes go through `set`, which
    /// forwards to the owning region.
    fn entity_landed(&self, _world: &mut dyn RegionWorld, _landing: &Landing) {}

    /// A game event happened where this region can hear it.
    fn game_event(&self, _world: &mut dyn RegionWorld, _emission: &Emission) {}

    /// A player bore mob effects that react to its death or hurt (Weaving,
    /// Oozing, Wind Charged, Infested).
    fn player_effect(&self, _world: &mut dyn RegionWorld, _event: &PlayerEffectEvent) {}

    /// A new entity (not one arriving from another region or a save) is
    /// about to be added to this region.
    fn entity_spawned(&self, _world: &mut dyn RegionWorld, _entity: &dyn RegionEntity) {}

    /// A bell at `pos` was rung.
    fn bell_rung(&self, _world: &mut dyn RegionWorld, _pos: BlockPos) {}

    /// An explosion is about to destroy `state` at `pos` (still in place).
    fn block_exploded(
        &self,
        _world: &mut dyn RegionWorld,
        _pos: BlockPos,
        _state: BlockState,
        _cause: &crate::world::explosion::ExplosionCause,
    ) {
    }

    /// An explosion at `cause.center` of `radius` found `blocks` blocks to
    /// destroy (before hooks spare any). Observe only.
    fn explosion_detonated(
        &self,
        _world: &mut dyn RegionWorld,
        _cause: &crate::world::explosion::ExplosionCause,
        _radius: f32,
        _blocks: usize,
    ) {
    }

    /// Whether a block change by a block's own doing (fire, flowing
    /// liquid), an entity or the weather may turn `old` into `new` at
    /// `pos`. Any hook saying no leaves the block as it is.
    fn allows_environment_change(
        &self,
        _world: &mut dyn RegionWorld,
        _pos: BlockPos,
        _old: BlockState,
        _new: BlockState,
        _cause: BlockCause,
    ) -> bool {
        true
    }

    /// What damage of type `kind` to the non-player `victim` is
    /// multiplied by before it lands (1 leaves it alone, 0 cancels it);
    /// each hook's factor applies. `attacker` is the uuid and type of
    /// whoever caused it.
    fn entity_hurt_scale(
        &self,
        _world: &dyn RegionWorld,
        _victim: &dyn crate::world::entity::RegionEntity,
        _kind: &'static tuff_vanilla_data::damage_type::DamageType,
        _attacker: Option<(u128, tuff_vanilla_data::registry::EntityType)>,
    ) -> f32 {
        1.0
    }

    /// A projectile landed on a block or an entity. Observe only.
    fn projectile_landed(
        &self,
        _world: &mut dyn RegionWorld,
        _landing: &crate::world::entity::ProjectileLanding,
    ) {
    }

    /// Whether an explosion may go off at all. Any hook saying no cancels
    /// it whole: no damage, knockback, block loss, fire, sound or event.
    fn allows_detonation(
        &self,
        _world: &mut dyn RegionWorld,
        _cause: &crate::world::explosion::ExplosionCause,
    ) -> bool {
        true
    }

    /// Whether an explosion may destroy `state` at `pos`. Any hook saying
    /// no spares the block.
    fn allows_explosion(
        &self,
        _world: &mut dyn RegionWorld,
        _pos: BlockPos,
        _state: BlockState,
        _cause: &crate::world::explosion::ExplosionCause,
    ) -> bool {
        true
    }

    /// Whether `player` may break `state` at `pos`, or place `state` there
    /// when `action` is [`BlockAction::Placed`]. Any hook saying no
    /// cancels it and resends the block to the player.
    fn allows_block_action(
        &self,
        _world: &mut dyn RegionWorld,
        _action: &PlayerBlockAction,
    ) -> bool {
        true
    }

    /// A mob died at `position` worth `amount` experience: true if this
    /// feature took it (a sculk catalyst), so no orbs drop.
    fn mob_experience(
        &self,
        _world: &mut dyn RegionWorld,
        _position: [f64; 3],
        _amount: i32,
    ) -> bool {
        false
    }

    /// A player is about to break the block at `action.pos` (still there,
    /// with its block entity) using `tool`.
    fn player_breaking_block(
        &self,
        _world: &mut dyn RegionWorld,
        _action: &PlayerBlockAction,
        _tool: Option<&tuff_item::item::ItemStack>,
    ) {
    }

    /// A block reacted to a player's right click.
    fn player_used_block(&self, _world: &mut dyn RegionWorld, _used: &PlayerBlockUse) {}

    /// A player changed a sign's text.
    fn sign_edited(&self, _world: &mut dyn RegionWorld, _edit: &SignEdit) {}

    /// A player broke, placed or opened a block.
    fn player_block_action(&self, _world: &mut dyn RegionWorld, _action: &PlayerBlockAction) {}

    /// Whether this feature wants [`Self::block_changed`]. Each region reads
    /// it once; with no feature saying yes, block changes cost nothing more.
    fn observes_block_changes(&self) -> bool {
        false
    }

    /// A block this region owns is about to change: `change.old` is still
    /// in place with its block entity. Observe only; a hook that wants to
    /// edit the world queues a [`RegionTask`] instead.
    fn block_changed(&self, _world: &mut dyn RegionWorld, _change: &BlockChange) {}

    /// Whether this feature wants [`Self::container_changed`]. Each region
    /// reads it once; with no feature saying yes, container changes cost
    /// nothing more.
    fn observes_container_changes(&self) -> bool {
        false
    }

    /// Container slots in this region changed. Observe only.
    fn container_changed(&self, _world: &mut dyn RegionWorld, _edit: &ContainerEdit) {}

    /// Whether this feature wants [`Self::item_picked_up`] and
    /// [`Self::entity_died`].
    fn observes_items_and_deaths(&self) -> bool {
        false
    }

    /// A dropped item was picked up by a player or a mob. Observe only.
    fn item_picked_up(&self, _world: &mut dyn RegionWorld, _pickup: &ItemPickup) {}

    /// A region entity died (it is still in the region). Observe only.
    fn entity_died(&self, _world: &mut dyn RegionWorld, _death: &EntityDeath) {}

    /// A player opened or broke a chest boat or cart.
    fn guarded_container_disturbed(&self, _world: &mut dyn RegionWorld, _player: PlayerId) {}

    /// `chunk` was just generated and lit (the creatures it starts with).
    fn chunk_generated(&self, _world: &mut dyn RegionWorld, _chunk: ChunkPos) {}

    /// An entity moved this tick from `before` (to walk it through a
    /// gateway, say).
    fn entity_moved(
        &self,
        _world: &mut dyn RegionWorld,
        _tracked: &mut TrackedEntity,
        _before: [f64; 3],
    ) {
    }

    /// A button press in the block menu `player` has open at `pos`; true
    /// if this feature handled it.
    fn menu_button(
        &self,
        _world: &mut dyn RegionWorld,
        _player: &PlayerHandle,
        _pos: BlockPos,
        _button: i32,
        _may_build: bool,
    ) -> bool {
        false
    }

    /// A click on a block, after containers and stations: the block's own
    /// reaction (doors, levers, beds, ...). The first hook to answer
    /// `Some` handles it; `None` lets the held item act.
    fn use_block(
        &self,
        _world: &mut dyn RegionWorld,
        _place: &Place,
        _state: BlockState,
    ) -> Option<ActionOutcome> {
        None
    }

    /// The held item used on the clicked block before any placement
    /// (tools, spawn eggs, minecarts on rails). The first `Some` wins;
    /// `None` goes on to placement.
    fn use_item_on(
        &self,
        _world: &mut dyn RegionWorld,
        _place: &Place,
        _state: BlockState,
    ) -> Option<ActionOutcome> {
        None
    }

    /// A player's click on the region's entity `target`, before the entity
    /// reacts itself (riding, its container, its own interaction): the
    /// first `Some` handles it and costs the hand that.
    fn interact_entity(
        &self,
        _world: &mut dyn RegionWorld,
        _player: &PlayerHandle,
        _target: i32,
        _interaction: &crate::world::entity::EntityInteraction,
    ) -> crate::world::entity::InteractOutcome {
        None
    }

    /// A player starts mining the block `state` at `pos` in survival, before
    /// any damage: blocks that react to being punched (the dragon egg).
    fn attack_block(&self, _world: &mut dyn RegionWorld, _pos: BlockPos, _state: BlockState) {}

    /// The note block at `pos` sounds (clicked or powered).
    fn play_note(&self, _world: &mut dyn RegionWorld, _pos: BlockPos) {}

    /// A mob opens or shuts the door at `pos` on its way.
    fn use_door(&self, _world: &mut dyn RegionWorld, _pos: BlockPos, _open: bool) {}

    /// A sleeper takes or leaves the bed at `pos`.
    fn occupy_bed(&self, _world: &mut dyn RegionWorld, _pos: BlockPos, _occupied: bool) {}

    /// A sleeping player gets up from the bed at `pos`.
    fn leave_bed(
        &self,
        _world: &mut dyn RegionWorld,
        _player: &PlayerHandle,
        _pos: BlockPos,
        _yaw: f32,
    ) {
    }

    /// Where a player respawns at `pos` (a bed, an anchor, or a forced
    /// point). `Some` from the first hook that knows; the inner `None`
    /// means there is no spot.
    fn respawn_point(
        &self,
        _world: &mut dyn RegionWorld,
        _query: &RespawnQuery,
    ) -> Option<Option<RespawnSpot>> {
        None
    }

    /// A bucket used at what the player looks at.
    fn use_bucket(&self, _world: &mut dyn RegionWorld, _request: &BucketUse) {}

    /// A boat item used at what the player looks at.
    fn place_boat(&self, _world: &mut dyn RegionWorld, _request: &BoatPlace) {}

    /// Fire was placed at `pos`, or is about to be lit there from its
    /// `from` side (a dispenser's flint and steel): lights a portal there
    /// if some frame around it is empty, returning whether it did (the
    /// fire is then replaced, or never placed). The first feature to say
    /// yes wins.
    fn light_portal(
        &self,
        _world: &mut dyn RegionWorld,
        _pos: BlockPos,
        _from: Option<tuff_vanilla_data::block::Facing>,
    ) -> bool {
        false
    }

    /// A thunderstorm is about to strike `target`: true when this feature
    /// turned the strike into something else (a skeleton trap), so the
    /// bolt is only seen. The first true wins.
    fn natural_lightning(&self, _world: &mut dyn RegionWorld, _target: BlockPos) -> bool {
        false
    }

    /// A projectile struck the block `state` at `hit.pos`, which this
    /// region owns: true when this feature reacted and the core reaction
    /// must be skipped. The first true wins.
    fn projectile_hit_block(
        &self,
        _world: &mut dyn RegionWorld,
        _hit: crate::world::raycast::Hit,
        _impact: &crate::world::entity::ProjectileImpact,
        _state: BlockState,
    ) -> bool {
        false
    }

    /// The portal block the player in `update` is inside, if this feature
    /// has one there that lets it through now. The first answer wins.
    fn portal_contact(
        &self,
        _world: &mut dyn RegionWorld,
        _update: &crate::world::messages::PlayerUpdate,
    ) -> Option<crate::world::contact::PortalContact> {
        None
    }
}

/// A respawn point being resolved.
#[derive(Debug, Clone, Copy)]
pub struct RespawnQuery {
    pub pos: BlockPos,
    pub yaw: f32,
    pub pitch: f32,
    /// Set with `/spawnpoint`, not by a bed or anchor.
    pub forced: bool,
    /// A death respawn, which spends an anchor charge.
    pub consume: bool,
}

/// Per-region state features keep, one value per type.
#[derive(Default)]
pub struct RegionState {
    values: HashMap<TypeId, Box<dyn Any + Send>>,
}

impl RegionState {
    /// The value of type `T`, created with its default on first use.
    pub fn get<T: Default + Send + 'static>(&mut self) -> &mut T {
        self.values
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(T::default()))
            .downcast_mut::<T>()
            .expect("keyed by its own TypeId")
    }

    /// The value of type `T` if one was created.
    pub fn peek<T: Send + 'static>(&self) -> Option<&T> {
        self.values.get(&TypeId::of::<T>())?.downcast_ref::<T>()
    }
}

/// A region's feature state shared by the entities ticking in parallel:
/// each `get` holds the region state until its guard drops. Holding one
/// guard while asking for another on the same thread panics instead of
/// deadlocking, so take what you need and drop the guard first.
#[derive(Default)]
pub struct RegionStateLock {
    state: std::sync::Mutex<RegionState>,
    holder: std::sync::atomic::AtomicUsize,
}

fn thread_token() -> usize {
    thread_local!(static TOKEN: u8 = const { 0 });
    TOKEN.with(|token| std::ptr::from_ref(token) as usize)
}

impl RegionStateLock {
    pub fn new(state: RegionState) -> Self {
        Self {
            state: std::sync::Mutex::new(state),
            holder: Default::default(),
        }
    }

    pub fn into_inner(self) -> RegionState {
        self.state
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The value of type `T`, created with its default on first use.
    pub fn get<T: Default + Send + 'static>(&self) -> RegionStateGuard<'_, T> {
        use std::sync::atomic::Ordering;
        let me = thread_token();
        assert_ne!(
            self.holder.load(Ordering::Relaxed),
            me,
            "region state asked for while this thread still holds it"
        );
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.get::<T>();
        self.holder.store(me, Ordering::Relaxed);
        RegionStateGuard {
            state,
            holder: &self.holder,
            kind: std::marker::PhantomData,
        }
    }

    /// Runs `f` with the whole region state held.
    pub fn with<R>(&self, f: impl FnOnce(&mut RegionState) -> R) -> R {
        use std::sync::atomic::Ordering;
        let me = thread_token();
        assert_ne!(
            self.holder.load(Ordering::Relaxed),
            me,
            "region state asked for while this thread still holds it"
        );
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(&mut state)
    }

    /// The value of type `T` if one was created, copied out.
    pub fn peek<T: Clone + Send + 'static>(&self) -> Option<T> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.peek::<T>().cloned()
    }
}

/// One value of a locked region state; see `RegionStateLock::get`.
pub struct RegionStateGuard<'a, T> {
    state: std::sync::MutexGuard<'a, RegionState>,
    holder: &'a std::sync::atomic::AtomicUsize,
    kind: std::marker::PhantomData<T>,
}

impl<T: Send + 'static> std::ops::Deref for RegionStateGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.state.peek::<T>().expect("created by get")
    }
}

impl<T: Default + Send + 'static> std::ops::DerefMut for RegionStateGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.state.get::<T>()
    }
}

impl<T> Drop for RegionStateGuard<'_, T> {
    fn drop(&mut self) {
        self.holder.store(0, std::sync::atomic::Ordering::Relaxed);
    }
}
