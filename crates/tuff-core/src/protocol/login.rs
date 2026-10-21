use bytes::Bytes;
use tuff_vanilla_data::packet::login::{Clientbound, Serverbound};

use crate::protocol::Packet;
use crate::protocol::custom::MAX_QUERY_PAYLOAD;
use tuff_item::text::TextComponent;
use tuff_wire::{PacketReader, PacketWriter};

const MAX_USERNAME_BYTES: usize = 16 * 4;
const MAX_KEY_BYTES: usize = 1024;

/// Serverbound.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = Serverbound::Hello)]
pub struct LoginStart {
    #[packet(max = MAX_USERNAME_BYTES)]
    pub name: String,
    pub uuid: u128,
}

/// Clientbound. The reason is a JSON text component.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = Clientbound::LoginDisconnect)]
pub struct LoginDisconnect {
    pub reason_json: String,
}

impl LoginDisconnect {
    pub fn new(reason: &TextComponent) -> Self {
        Self {
            reason_json: reason.to_json(),
        }
    }
}

/// Clientbound.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = Clientbound::Hello)]
pub struct EncryptionRequest {
    #[packet(max = 20 * 4)]
    pub server_id: String,
    #[packet(max = MAX_KEY_BYTES)]
    pub public_key: Bytes,
    #[packet(max = MAX_KEY_BYTES)]
    pub verify_token: Bytes,
    pub should_authenticate: bool,
}

/// Serverbound. Both fields are encrypted with the server's RSA public key.
#[derive(Clone, PartialEq, Eq, Packet)]
#[packet(id = Serverbound::Key)]
pub struct EncryptionResponse {
    #[packet(max = MAX_KEY_BYTES)]
    pub shared_secret: Bytes,
    #[packet(max = MAX_KEY_BYTES)]
    pub verify_token: Bytes,
}

impl std::fmt::Debug for EncryptionResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionResponse")
            .field(
                "shared_secret",
                &format_args!("<{} bytes redacted>", self.shared_secret.len()),
            )
            .field(
                "verify_token",
                &format_args!("<{} bytes redacted>", self.verify_token.len()),
            )
            .finish()
    }
}

/// Serverbound. Acknowledges Login Success and moves to configuration.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = Serverbound::LoginAcknowledged)]
pub struct LoginAcknowledged;

/// Clientbound.
#[derive(Debug, Clone, PartialEq, Eq, Packet)]
#[packet(id = Clientbound::LoginCompression)]
pub struct SetCompression {
    #[packet(var_int)]
    pub threshold: i32,
}

/// Clientbound login plugin request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginQuery {
    pub transaction_id: i32,
    pub channel: String,
    pub data: Bytes,
}

impl Packet for LoginQuery {
    const ID: i32 = Clientbound::CustomQuery.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.transaction_id);
        w.write_string(&self.channel);
        w.write_bytes(&self.data);
    }

    fn decode(r: &mut PacketReader) -> tuff_wire::Result<Self> {
        let transaction_id = r.read_var_int()?;
        let channel = r.read_string_with_max(256)?;
        if r.remaining() > MAX_QUERY_PAYLOAD {
            return Err(tuff_wire::BufferError::StringTooLong(r.remaining()));
        }
        let data = r.read_bytes(r.remaining())?;
        Ok(Self {
            transaction_id,
            channel,
            data,
        })
    }
}

/// Serverbound answer to a [`LoginQuery`]; `data` is `None` when the client
/// did not understand the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginQueryAnswer {
    pub transaction_id: i32,
    pub data: Option<Bytes>,
}

impl Packet for LoginQueryAnswer {
    const ID: i32 = Serverbound::CustomQueryAnswer.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(self.transaction_id);
        w.write_bool(self.data.is_some());
        if let Some(data) = &self.data {
            w.write_bytes(data);
        }
    }

    fn decode(r: &mut PacketReader) -> tuff_wire::Result<Self> {
        let transaction_id = r.read_var_int()?;
        let data = if r.read_bool()? {
            if r.remaining() > MAX_QUERY_PAYLOAD {
                return Err(tuff_wire::BufferError::StringTooLong(r.remaining()));
            }
            Some(r.read_bytes(r.remaining())?)
        } else {
            None
        };
        Ok(Self {
            transaction_id,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_wire::Buffer;

    fn roundtrip<P: Packet + std::fmt::Debug + PartialEq>(packet: P) {
        let mut buf = Buffer::new();
        buf.extend(&crate::protocol::encode_packet(&packet).finish())
            .unwrap();
        let mut r = PacketReader::new(buf.try_split_packet().unwrap().unwrap());
        assert_eq!(r.read_var_int().unwrap(), P::ID);
        assert_eq!(P::decode(&mut r).unwrap(), packet);
        assert_eq!(r.remaining(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn login_packets_roundtrip() {
        roundtrip(LoginStart {
            name: "Notch".into(),
            uuid: 0x069a79f444e94726a5befca90e38aaf5,
        });
        roundtrip(EncryptionRequest {
            server_id: String::new(),
            public_key: Bytes::from_static(&[1, 2, 3]),
            verify_token: Bytes::from_static(&[4, 5, 6, 7]),
            should_authenticate: true,
        });
        roundtrip(EncryptionResponse {
            shared_secret: Bytes::from_static(&[9; 128]),
            verify_token: Bytes::from_static(&[8; 128]),
        });
        roundtrip(LoginDisconnect::new(&TextComponent::new("bye")));
        roundtrip(LoginAcknowledged);
        roundtrip(SetCompression { threshold: 256 });
        roundtrip(LoginQuery {
            transaction_id: 3,
            channel: "a:b".into(),
            data: Bytes::from_static(&[1, 2]),
        });
        roundtrip(LoginQueryAnswer {
            transaction_id: 3,
            data: Some(Bytes::from_static(&[9])),
        });
        roundtrip(LoginQueryAnswer {
            transaction_id: 4,
            data: None,
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn login_start_rejects_long_name() {
        let mut w = PacketWriter::new();
        w.write_string(&"x".repeat(100));
        w.write_uuid(0);
        let mut buf = Buffer::new();
        buf.extend(&w.finish()).unwrap();
        let mut r = PacketReader::new(buf.try_split_packet().unwrap().unwrap());
        assert!(LoginStart::decode(&mut r).is_err());
    }
}
