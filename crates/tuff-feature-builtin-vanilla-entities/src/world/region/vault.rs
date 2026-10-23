//! Vaults.
//!
//! A vault stays saved block entity NBT, ticked by its region. It wakes
//! when a player who has not yet been rewarded comes close, cycles a
//! display item from its loot table, and takes a matching key from the
//! hand of such a player: it then spends a moment unlocking and ejects the
//! loot an item at a time, remembering the player so a key buys one reward
//! per player.

use std::collections::HashMap;

use tuff_vanilla_data::Block;
use tuff_vanilla_data::registry::SoundEvent;

use rand::Rng;
use tuff_core::world::messages::{ActionOutcome, HandEffect, Place};
use tuff_core::world::region_api::RegionWorld;
use tuff_item::item::ItemStack;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::BlockState;
use tuff_world_model::BlockPos;

use super::trial::eject_item;

pub const VAULT_ID: &str = "minecraft:vault";

const UNLOCK_DELAY_TICKS: i64 = 14;
const STATE_DELAY_TICKS: i64 = 20;
const DISPLAY_CYCLE_TICKS: u64 = 20;
const INSERT_FAIL_BUFFER_TICKS: u64 = 15;
const MAX_REWARDED_PLAYERS: usize = 128;
const DEFAULT_LOOT_TABLE: &str = "minecraft:chests/trial_chambers/reward";
const DEFAULT_ACTIVATION_RANGE: f64 = 4.0;
const DEFAULT_DEACTIVATION_RANGE: f64 = 4.5;
const EVENT_ACTIVATE: i32 = 3015;
const EVENT_DEACTIVATE: i32 = 3016;
const EVENT_EJECT_ITEM: i32 = 3017;

const STATE: &str = "vault_state";
const OMINOUS: &str = "ominous";

#[derive(Default)]
struct Vaults(Vec<BlockPos>, Option<u64>);

#[derive(Default)]
struct InsertFailures(HashMap<BlockPos, u64>);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Inactive,
    Active,
    Unlocking,
    Ejecting,
}

impl State {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "inactive" => Self::Inactive,
            "active" => Self::Active,
            "unlocking" => Self::Unlocking,
            "ejecting" => Self::Ejecting,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Active => "active",
            Self::Unlocking => "unlocking",
            Self::Ejecting => "ejecting",
        }
    }
}

struct Config {
    loot_table: String,
    activation_range: f64,
    deactivation_range: f64,
    key_item: Option<tuff_item::item::ItemPicture>,
    display_table: Option<String>,
}

struct Vault {
    config: Config,
    display_item: Option<tuff_item::item::ItemSnapshot>,
    connected: Vec<u128>,
    connected_range: f64,
    rewarded: Vec<u128>,
    resumes_at: i64,
    items_to_eject: Vec<ItemStack>,
    total_ejections: i32,
    shared_dirty: bool,
    server_dirty: bool,
}

/// A vault is read from and written back to its block entity's NBT, which
/// keeps its items between ticks.
impl Drop for Vault {
    fn drop(&mut self) {
        tuff_item::item::conservation::release(std::mem::take(&mut self.items_to_eject));
    }
}

fn is_vault(nbt: &Compound) -> bool {
    nbt.get_str("id") == Some(VAULT_ID)
}

fn double(nbt: &Compound, key: &str, default: f64) -> f64 {
    match nbt.get(key) {
        Some(Tag::Double(v)) => *v,
        Some(Tag::Float(v)) => f64::from(*v),
        Some(Tag::Int(v)) => f64::from(*v),
        _ => default,
    }
}

fn long(nbt: &Compound, key: &str) -> i64 {
    match nbt.get(key) {
        Some(Tag::Long(v)) => *v,
        Some(Tag::Int(v)) => i64::from(*v),
        _ => 0,
    }
}

fn uuids(nbt: &Compound, key: &str) -> Vec<u128> {
    nbt.get_list(key)
        .into_iter()
        .flatten()
        .filter_map(Tag::as_uuid)
        .collect()
}

/// A stack the vault's data names (its key, what it displays): a picture.
fn stack_in(nbt: &Compound, key: &str) -> Option<tuff_item::item::ItemPicture> {
    tuff_item::item::ItemPicture::from_nbt(nbt.get_compound(key)?)
}

impl Vault {
    fn read(nbt: &Compound) -> Self {
        let empty = Compound::new();
        let config = nbt.get_compound("config").unwrap_or(&empty);
        let shared = nbt.get_compound("shared_data").unwrap_or(&empty);
        let server = nbt.get_compound("server_data").unwrap_or(&empty);
        Self {
            config: Config {
                loot_table: config
                    .get_str("loot_table")
                    .unwrap_or(DEFAULT_LOOT_TABLE)
                    .to_owned(),
                activation_range: double(config, "activation_range", DEFAULT_ACTIVATION_RANGE),
                deactivation_range: double(
                    config,
                    "deactivation_range",
                    DEFAULT_DEACTIVATION_RANGE,
                ),
                key_item: stack_in(config, "key_item"),
                display_table: config
                    .get_str("override_loot_table_to_display")
                    .map(str::to_owned),
            },
            display_item: stack_in(shared, "display_item").map(Into::into),
            connected: uuids(shared, "connected_players"),
            connected_range: double(
                shared,
                "connected_particles_range",
                DEFAULT_DEACTIVATION_RANGE,
            ),
            rewarded: uuids(server, "rewarded_players"),
            resumes_at: long(server, "state_updating_resumes_at"),
            items_to_eject: server
                .get_list("items_to_eject")
                .into_iter()
                .flatten()
                .filter_map(|tag| ItemStack::from_storage(tag.as_compound()?))
                .collect(),
            total_ejections: long(server, "total_ejections_needed") as i32,
            shared_dirty: false,
            server_dirty: false,
        }
    }

    fn write(&self, nbt: &mut Compound) {
        let ids = |ids: &[u128]| Tag::List(ids.iter().map(|id| Tag::uuid(*id)).collect());
        let mut shared = nbt.get_compound("shared_data").cloned().unwrap_or_default();
        match &self.display_item {
            Some(stack) => {
                shared.insert("display_item", stack.to_nbt());
            }
            None => {
                shared.remove("display_item");
            }
        }
        shared.insert("connected_players", ids(&self.connected));
        shared.insert("connected_particles_range", self.connected_range);
        nbt.insert("shared_data", shared);
        let mut server = nbt.get_compound("server_data").cloned().unwrap_or_default();
        server.insert("rewarded_players", ids(&self.rewarded));
        server.insert("state_updating_resumes_at", self.resumes_at);
        server.insert(
            "items_to_eject",
            Tag::List(
                self.items_to_eject
                    .iter()
                    .map(|s| Tag::Compound(s.to_nbt()))
                    .collect(),
            ),
        );
        server.insert("total_ejections_needed", self.total_ejections);
        nbt.insert("server_data", server);
    }

    fn set_display(&mut self, stack: Option<tuff_item::item::ItemSnapshot>) {
        if self.display_item != stack {
            self.display_item = stack;
            self.shared_dirty = true;
        }
    }

    fn can_eject(&self, state: State) -> bool {
        self.config.key_item.is_some() && state != State::Inactive
    }

    fn pause_until(&mut self, tick: i64) {
        self.resumes_at = tick;
        self.server_dirty = true;
    }

    fn next_to_eject(&self) -> Option<tuff_item::item::ItemSnapshot> {
        self.items_to_eject
            .last()
            .map(tuff_item::item::ItemSnapshot::of)
    }

    fn ejection_progress(&self) -> f32 {
        if self.total_ejections == 1 {
            return 1.0;
        }
        let remaining = self.items_to_eject.len() as f32;
        let total = self.total_ejections as f32;
        1.0 - (remaining - 1.0) / (total - 1.0)
    }

    fn update_connected(&mut self, world: &mut dyn RegionWorld, pos: BlockPos, limit: f64) {
        let limit_sq = limit * limit;
        let mut found: Vec<u128> = world
            .seen_player_details()
            .into_iter()
            .filter(|p| {
                let at = tuff_core::world::physics::block_pos(p.position);
                let (dx, dy, dz) = (
                    f64::from(at.x - pos.x),
                    f64::from(at.y - pos.y),
                    f64::from(at.z - pos.z),
                );
                dx * dx + dy * dy + dz * dz < limit_sq
            })
            .map(|p| p.uuid)
            .filter(|uuid| !self.rewarded.contains(uuid))
            .collect();
        found.sort_unstable();
        let mut current = self.connected.clone();
        current.sort_unstable();
        if found != current {
            self.connected = found;
            self.shared_dirty = true;
        }
    }

    fn display_from_table(&mut self, world: &mut dyn RegionWorld, state: State) {
        if !self.can_eject(state) {
            self.set_display(None);
            return;
        }
        let table = self
            .config
            .display_table
            .clone()
            .unwrap_or_else(|| self.config.loot_table.clone());
        let mut items = world.shared().block_loot().roll(&table, world.rng());
        if items.is_empty() {
            self.set_display(None);
            return;
        }
        let pick = world.rng().gen_range(0..items.len());
        let shown = items.swap_remove(pick).into_picture().into();
        for rest in items {
            rest.burn(tuff_item::item::BurnReason::Picture);
        }
        self.set_display(Some(shown));
    }
}

pub fn tick(world: &mut dyn RegionWorld) {
    let generation = world.block_entity_generation();
    if world.state().get::<Vaults>().1 != Some(generation) {
        let mut found = world.raw_block_entities_where(&is_vault);
        found.sort_unstable_by_key(|p| (p.y, p.z, p.x));
        let cache = world.state().get::<Vaults>();
        cache.0 = found;
        cache.1 = Some(generation);
    }
    let positions = std::mem::take(&mut world.state().get::<Vaults>().0);
    for &pos in &positions {
        tick_vault(world, pos);
    }
    world.state().get::<Vaults>().0 = positions;
}

fn state_of(block: BlockState) -> State {
    block
        .get(STATE)
        .and_then(State::parse)
        .unwrap_or(State::Inactive)
}

fn is_ominous(block: BlockState) -> bool {
    block.get(OMINOUS) == Some("true")
}

fn save(world: &mut dyn RegionWorld, pos: BlockPos, vault: &Vault, force_client: bool) {
    let Some(nbt) = world.raw_block_entity_mut(pos) else {
        return;
    };
    let before = nbt.clone();
    vault.write(nbt);
    if *nbt == before && !force_client {
        return;
    }
    if vault.shared_dirty || force_client {
        world.block_entity_changed(pos);
    } else {
        world.mark_blocks_dirty(pos);
    }
}

fn tick_vault(world: &mut dyn RegionWorld, pos: BlockPos) {
    let Some(block) = world.block(pos) else {
        return;
    };
    if block.block() != Block::Vault {
        return;
    }
    let Some(nbt) = world.raw_block_entity(pos).filter(|n| is_vault(n)) else {
        return;
    };
    let mut vault = Vault::read(nbt);
    let now = world.tick();
    let state = state_of(block);
    if now.is_multiple_of(DISPLAY_CYCLE_TICKS) && state == State::Active {
        vault.display_from_table(world, state);
    }
    if now as i64 >= vault.resumes_at {
        let next = tick_state(world, pos, state, &mut vault);
        if next != state
            && let Some(updated) = block.with(STATE, next.name())
        {
            world.put_block(pos, updated);
            transition(world, pos, state, next, &mut vault, is_ominous(block));
        }
    }
    if vault.shared_dirty || vault.server_dirty {
        save(world, pos, &vault, false);
    }
}

fn tick_state(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    state: State,
    vault: &mut Vault,
) -> State {
    let now = world.tick() as i64;
    match state {
        State::Inactive => reconsider(world, pos, vault, vault.config.activation_range),
        State::Active => reconsider(world, pos, vault, vault.config.deactivation_range),
        State::Unlocking => {
            vault.pause_until(now + STATE_DELAY_TICKS);
            State::Ejecting
        }
        State::Ejecting => {
            if vault.items_to_eject.is_empty() {
                vault.total_ejections = 0;
                vault.server_dirty = true;
                return reconsider(world, pos, vault, vault.config.deactivation_range);
            }
            let progress = vault.ejection_progress();
            let item = vault.items_to_eject.pop().expect("a nonempty list");
            vault.server_dirty = true;
            let origin = [
                f64::from(pos.x) + 0.5,
                f64::from(pos.y) + 1.2 - 0.125,
                f64::from(pos.z) + 0.5,
            ];
            eject_item(world, item, origin);
            world.broadcast_level_event(EVENT_EJECT_ITEM, pos, 0);
            world.play_sound(
                SoundEvent::BlockVaultEjectItem,
                pos,
                1.0,
                0.8 + 0.4 * progress,
                None,
            );
            let next = vault.next_to_eject();
            vault.set_display(next);
            vault.pause_until(now + STATE_DELAY_TICKS);
            State::Ejecting
        }
    }
}

fn reconsider(world: &mut dyn RegionWorld, pos: BlockPos, vault: &mut Vault, range: f64) -> State {
    vault.update_connected(world, pos, range);
    vault.pause_until(world.tick() as i64 + STATE_DELAY_TICKS);
    if vault.connected.is_empty() {
        State::Inactive
    } else {
        State::Active
    }
}

fn transition(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    from: State,
    to: State,
    vault: &mut Vault,
    ominous: bool,
) {
    if from == State::Ejecting {
        world.play_sound(SoundEvent::BlockVaultCloseShutter, pos, 1.0, 1.0, None);
    }
    let flag = i32::from(ominous);
    match to {
        State::Inactive => {
            vault.set_display(None);
            world.broadcast_level_event(EVENT_DEACTIVATE, pos, flag);
        }
        State::Active => {
            if vault.display_item.is_none() {
                vault.display_from_table(world, to);
            }
            world.broadcast_level_event(EVENT_ACTIVATE, pos, flag);
        }
        State::Unlocking => {
            world.play_sound(SoundEvent::BlockVaultInsertItem, pos, 1.0, 1.0, None);
        }
        State::Ejecting => {
            world.play_sound(SoundEvent::BlockVaultOpenShutter, pos, 1.0, 1.0, None);
        }
    }
}

fn play_fail(world: &mut dyn RegionWorld, pos: BlockPos, sound: SoundEvent) {
    let now = world.tick();
    let failures = world.state().get::<InsertFailures>();
    let ready = failures
        .0
        .get(&pos)
        .is_none_or(|last| now >= last + INSERT_FAIL_BUFFER_TICKS);
    if ready {
        failures.0.insert(pos, now);
        world.play_sound(sound, pos, 1.0, 1.0, None);
    }
}

/// A click with an item on a vault: an active one takes a matching key.
pub fn use_block(
    world: &mut dyn RegionWorld,
    place: &Place,
    block: BlockState,
) -> Option<ActionOutcome> {
    let held = place.held?;
    if block.block() != Block::Vault || state_of(block) != State::Active || !place.interact {
        return None;
    }
    let pos = place.target;
    let nbt = world.raw_block_entity(pos).filter(|n| is_vault(n))?;
    let mut vault = Vault::read(nbt);
    let interacted = Some(ActionOutcome::Interacted {
        off_hand: place.off_hand,
    });
    if !vault.can_eject(State::Active) {
        return interacted;
    }
    let key = vault
        .config
        .key_item
        .as_ref()
        .map(tuff_item::item::ItemSnapshot::of)?;
    if key.item() != held || key.components != *place.components || place.held_count < key.count() {
        play_fail(world, pos, SoundEvent::BlockVaultInsertItemFail);
        return interacted;
    }
    if vault.rewarded.contains(&place.uuid) {
        play_fail(world, pos, SoundEvent::BlockVaultRejectRewardedPlayer);
        return interacted;
    }
    let loot = world
        .shared()
        .block_loot()
        .roll(&vault.config.loot_table, world.rng());
    if loot.is_empty() {
        return interacted;
    }
    let now = world.tick() as i64;
    vault.total_ejections = loot.len() as i32;
    vault.items_to_eject = loot;
    let next = vault.next_to_eject();
    vault.set_display(next);
    vault.pause_until(now + UNLOCK_DELAY_TICKS);
    vault.rewarded.push(place.uuid);
    if vault.rewarded.len() > MAX_REWARDED_PLAYERS {
        vault.rewarded.remove(0);
    }
    let deactivation = vault.config.deactivation_range;
    if let Some(unlocking) = block.with(STATE, State::Unlocking.name()) {
        world.put_block(pos, unlocking);
    }
    transition(
        world,
        pos,
        State::Active,
        State::Unlocking,
        &mut vault,
        is_ominous(block),
    );
    vault.update_connected(world, pos, deactivation);
    vault.shared_dirty = true;
    save(world, pos, &vault, true);
    Some(ActionOutcome::Used {
        off_hand: place.off_hand,
        item: held,
        effect: HandEffect::Consume(key.count()),
    })
}
