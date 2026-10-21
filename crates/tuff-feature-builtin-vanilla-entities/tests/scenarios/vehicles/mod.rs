//! Boats, minecarts and ridden mounts through a real region.

use super::*;
use crate::protocol::play::{Movement, Serverbound};
use crate::world::entity::EntityAttack;
use crate::world::messages::VehicleRequest;
use tuff_vanilla_data::registry::EntityType;

mod boats;
mod chest_boats;
mod death_and_borders;
mod ground_fall;
mod steering;

use boats::punch;
use chest_boats::{chest_boat_data, sneak_open};
use ground_fall::{load_chunk, pump};
