use tuff_vanilla_data::packet::status::{Clientbound, Serverbound};

use crate::protocol::Packet;

/// Serverbound.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = Serverbound::StatusRequest)]
pub struct StatusRequest;

/// Clientbound.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = Clientbound::StatusResponse)]
pub struct StatusResponse {
    pub json: String,
}

/// Serverbound.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = Serverbound::PingRequest)]
pub struct PingRequest {
    #[packet(long)]
    pub timestamp: i64,
}

/// Clientbound.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = Clientbound::PongResponse)]
pub struct PongResponse {
    #[packet(long)]
    pub timestamp: i64,
}
