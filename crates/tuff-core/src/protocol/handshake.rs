use tuff_vanilla_data::packet::handshake::Serverbound;

use crate::protocol::Packet;
use crate::protocol::field::{ReadField, WriteField};
use tuff_wire::{BufferError, PacketReader, PacketWriter, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Status,
    Login,
    Transfer,
}

impl ReadField for Intent {
    fn read(r: &mut PacketReader) -> Result<Self> {
        match r.read_var_int()? {
            1 => Ok(Self::Status),
            2 => Ok(Self::Login),
            3 => Ok(Self::Transfer),
            other => Err(BufferError::InvalidEnum(other)),
        }
    }
}

impl WriteField for Intent {
    fn write(&self, w: &mut PacketWriter) {
        w.write_var_int(match self {
            Self::Status => 1,
            Self::Login => 2,
            Self::Transfer => 3,
        });
    }
}

/// Serverbound: the first packet on every connection.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = Serverbound::Intention)]
pub struct Intention {
    #[packet(var_int)]
    pub protocol_version: i32,
    #[packet(max = 255)]
    pub server_address: String,
    pub server_port: u16,
    pub intent: Intent,
}
