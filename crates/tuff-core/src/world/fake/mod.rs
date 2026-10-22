//! Fake players: ordinary player sessions whose connection is not a socket.
//! Every input enters as a serverbound packet through the session's normal
//! packet path; the [`client`] answers what a real client must.

pub mod actions;
pub mod client;
pub mod driver;
pub mod motion;

use tokio::sync::{mpsc, watch};

use crate::protocol::play::{GameMode, Serverbound};
use crate::world::messages::PlayerId;

/// The most fakes one owner may have at once.
pub const OWNER_LIMIT: usize = 64;

/// The skin model a fake's uuid is chosen to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Model {
    #[default]
    Wide,
    Slim,
}

#[derive(Debug, Clone)]
pub struct FakeSpec {
    pub name: String,
    /// Derived from the owner and name when absent; never a version-4 uuid.
    pub uuid: Option<u128>,
    pub model: Model,
    /// A dimension key; the spawn dimension when absent.
    pub dimension: Option<String>,
    pub position: Option<[f64; 3]>,
    pub rotation: Option<(f32, f32)>,
    pub game_mode: Option<GameMode>,
    /// Whether the tab list shows it.
    pub listed: bool,
    pub view_distance: i8,
}

impl FakeSpec {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            uuid: None,
            model: Model::Wide,
            dimension: None,
            position: None,
            rotation: None,
            game_mode: None,
            listed: true,
            view_distance: 4,
        }
    }
}

/// What the fake's client last learned from the server about itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observed {
    pub position: [f64; 3],
    pub rotation: (f32, f32),
    /// Server-driven position changes confirmed so far.
    pub teleports: u64,
    pub chunk_batches: u64,
    /// Velocity the server last gave it (teleport or knockback).
    pub velocity: [f64; 3],
    /// Knockback impulses received so far.
    pub impulses: u64,
    /// The body's state after its last tick, for cheap reads by owners.
    pub body: BodyStatus,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BodyStatus {
    pub position: [f64; 3],
    pub rotation: (f32, f32),
    pub dimension: Option<crate::world::dimension::DimensionId>,
    pub velocity: [f64; 3],
    pub on_ground: bool,
    pub in_fluid: bool,
    pub collided: bool,
    pub health: f32,
    pub food: i32,
    pub selected_slot: u8,
    /// The item in each hotbar slot.
    pub hotbar: [Option<tuff_vanilla_data::Item>; 9],
    pub dig_progress: f32,
    pub alive: bool,
    /// Every occupied inventory slot in window numbering.
    pub items: Vec<(u8, tuff_item::item::ItemSnapshot)>,
    /// How its gear and effects change its mining speed.
    pub miner: Option<crate::world::player::Miner>,
}

#[derive(Debug, Clone)]
pub struct FakeHandle {
    pub uuid: u128,
    pub entity: PlayerId,
    pub owner: String,
    /// Where the driver injects packets, as a client's connection would.
    pub packets: mpsc::Sender<Serverbound>,
    pub observed: watch::Receiver<Observed>,
    /// Replaces the held keys; send only when they change. Unbounded because
    /// changes are sparse and the receiver drains every tick.
    pub held: mpsc::UnboundedSender<motion::Held>,
    pub actions: mpsc::UnboundedSender<actions::FakeAction>,
}

impl FakeHandle {
    /// Holds `held` until the next call.
    pub fn hold(&self, held: motion::Held) {
        let _ = self.held.send(held);
    }

    /// Queues a hand action for the next client tick.
    pub fn act(&self, action: actions::FakeAction) {
        let _ = self.actions.send(action);
    }
}

/// A stable uuid for `name` under `owner` that is not version 4, so it
/// never collides with a random player id.
pub fn derive_uuid(owner: &str, name: &str, model: Model) -> u128 {
    const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013b;
    let mut hash = OFFSET;
    for byte in owner.bytes().chain([0]).chain(name.bytes()) {
        hash ^= byte as u128;
        hash = hash.wrapping_mul(PRIME);
    }
    let versioned = (hash & !(0xF << 76)) | (0x8 << 76);
    let uuid = (versioned & !(0x3 << 62)) | (0x2 << 62);
    let hashed = (uuid >> 96) as u32 ^ (uuid >> 64) as u32 ^ (uuid >> 32) as u32 ^ uuid as u32;
    let wanted_slim = model == Model::Slim;
    if (hashed & 1 == 1) == wanted_slim {
        uuid
    } else {
        uuid ^ 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn derived_uuids_are_stable_distinct_and_not_version_four() {
        let a = derive_uuid("plugin", "Bot1", Model::Wide);
        assert_eq!(a, derive_uuid("plugin", "Bot1", Model::Wide));
        assert_ne!(a, derive_uuid("plugin", "Bot2", Model::Wide));
        assert_ne!(a, derive_uuid("other", "Bot1", Model::Wide));
        assert_eq!((a >> 76) & 0xF, 8);
        let slim = derive_uuid("plugin", "Bot1", Model::Slim);
        assert_eq!((slim >> 76) & 0xF, 8);
        assert_ne!(a, slim);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_uuid_hash_parity_picks_the_default_skin_model() {
        for name in ["A", "B", "C", "D"] {
            for (model, slim) in [(Model::Wide, false), (Model::Slim, true)] {
                let uuid = derive_uuid("o", name, model);
                let hash =
                    (uuid >> 96) as u32 ^ (uuid >> 64) as u32 ^ (uuid >> 32) as u32 ^ uuid as u32;
                assert_eq!(hash & 1 == 1, slim);
            }
        }
    }
}
