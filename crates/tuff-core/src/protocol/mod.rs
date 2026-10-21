//! Packet definitions for each connection state
//! (https://minecraft.wiki/w/Java_Edition_protocol/Packets).

pub mod chat_session;
pub mod codec;
pub mod configuration;
pub mod cookie;
pub mod custom;
pub mod field;
pub mod handshake;
pub mod login;
pub mod pack;
pub mod play;
pub mod recipe;
pub mod status;

use tuff_wire::{PacketReader, PacketWriter, Result};

pub use tuff_macros::Packet;
pub use tuff_vanilla_data::version::{NAME as VERSION_NAME, PROTOCOL_VERSION};

/// A packet body within a single connection state and direction. `encode` and
/// `decode` handle only the fields after the packet ID. Both directions are
/// implemented for every packet so that test clients and proxies can reuse them.
pub trait Packet: Sized {
    const ID: i32;
    fn encode(&self, w: &mut PacketWriter);
    fn decode(r: &mut PacketReader) -> Result<Self>;
}

/// Anything the server can write: every [`Packet`], plus the configuration and
/// play clientbound packets, which only implement the server's direction.
pub trait Encode {
    const ID: i32;
    fn encode(&self, w: &mut PacketWriter);
}

impl<P: Packet> Encode for P {
    const ID: i32 = <P as Packet>::ID;

    fn encode(&self, w: &mut PacketWriter) {
        Packet::encode(self, w);
    }
}

/// Writes the packet ID followed by the body into a fresh [`PacketWriter`].
pub fn encode_packet<P: Encode>(packet: &P) -> PacketWriter {
    let mut w = PacketWriter::new();
    w.write_var_int(P::ID);
    packet.encode(&mut w);
    w
}
