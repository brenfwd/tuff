//! Resource pack packets of the configuration and play states.

use bytes::Bytes;
use tuff_vanilla_data::packet::{configuration as cfg, play};

use crate::protocol::Encode;
use crate::protocol::codec::WriteExt;
use tuff_item::text::TextComponent;
use tuff_wire::{PacketReader, PacketWriter, Result};

pub const SHA1_HEX_LEN: usize = 40;

/// What a client reports about a pack, as sent on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackStatus {
    Loaded,
    Declined,
    FailedDownload,
    Accepted,
    Downloaded,
    InvalidUrl,
    FailedReload,
    Discarded,
}

impl PackStatus {
    pub fn from_wire(value: i32) -> Option<PackStatus> {
        Some(match value {
            0 => Self::Loaded,
            1 => Self::Declined,
            2 => Self::FailedDownload,
            3 => Self::Accepted,
            4 => Self::Downloaded,
            5 => Self::InvalidUrl,
            6 => Self::FailedReload,
            7 => Self::Discarded,
            _ => return None,
        })
    }

    pub fn to_wire(self) -> i32 {
        match self {
            Self::Loaded => 0,
            Self::Declined => 1,
            Self::FailedDownload => 2,
            Self::Accepted => 3,
            Self::Downloaded => 4,
            Self::InvalidUrl => 5,
            Self::FailedReload => 6,
            Self::Discarded => 7,
        }
    }

    /// The client is done with the pack, one way or the other.
    pub fn is_final(self) -> bool {
        !matches!(self, Self::Accepted | Self::Downloaded)
    }

    /// The client does not have the pack applied after this status.
    pub fn is_failure(self) -> bool {
        matches!(
            self,
            Self::Declined
                | Self::FailedDownload
                | Self::InvalidUrl
                | Self::FailedReload
                | Self::Discarded
        )
    }
}

fn encode_push(
    w: &mut PacketWriter,
    id: u128,
    url: &str,
    hash: &str,
    required: bool,
    prompt: Option<&TextComponent>,
) {
    w.write_uuid(id);
    w.write_string(url);
    w.write_string(hash);
    w.write_bool(required);
    w.write_bool(prompt.is_some());
    if let Some(prompt) = prompt {
        w.write_text(prompt);
    }
}

fn encode_pop(w: &mut PacketWriter, id: Option<u128>) {
    w.write_bool(id.is_some());
    if let Some(id) = id {
        w.write_uuid(id);
    }
}

pub struct Push<'a> {
    pub id: u128,
    pub url: &'a str,
    pub hash: &'a str,
    pub required: bool,
    pub prompt: Option<&'a TextComponent>,
}

pub struct ConfigPush<'a>(pub Push<'a>);
pub struct PlayPush<'a>(pub Push<'a>);
pub struct ConfigPop(pub Option<u128>);
pub struct PlayPop(pub Option<u128>);
pub struct ConfigDisconnect<'a>(pub &'a TextComponent);

impl Encode for ConfigPush<'_> {
    const ID: i32 = cfg::Clientbound::ResourcePackPush.id();
    fn encode(&self, w: &mut PacketWriter) {
        let p = &self.0;
        encode_push(w, p.id, p.url, p.hash, p.required, p.prompt);
    }
}

impl Encode for PlayPush<'_> {
    const ID: i32 = play::Clientbound::ResourcePackPush.id();
    fn encode(&self, w: &mut PacketWriter) {
        let p = &self.0;
        encode_push(w, p.id, p.url, p.hash, p.required, p.prompt);
    }
}

impl Encode for ConfigPop {
    const ID: i32 = cfg::Clientbound::ResourcePackPop.id();
    fn encode(&self, w: &mut PacketWriter) {
        encode_pop(w, self.0);
    }
}

impl Encode for PlayPop {
    const ID: i32 = play::Clientbound::ResourcePackPop.id();
    fn encode(&self, w: &mut PacketWriter) {
        encode_pop(w, self.0);
    }
}

impl Encode for ConfigDisconnect<'_> {
    const ID: i32 = cfg::Clientbound::Disconnect.id();
    fn encode(&self, w: &mut PacketWriter) {
        w.write_text(self.0);
    }
}

/// A client's Resource Pack response; `None` status for a value outside
/// the protocol's range.
pub fn decode_response(r: &mut PacketReader) -> Result<(u128, Option<PackStatus>)> {
    let id = r.read_uuid()?;
    let status = PackStatus::from_wire(r.read_var_int()?);
    Ok((id, status))
}

pub fn encode_response_writer(id: u128, status: PackStatus, packet_id: i32) -> PacketWriter {
    let mut w = PacketWriter::new();
    w.write_var_int(packet_id);
    w.write_uuid(id);
    w.write_var_int(status.to_wire());
    w
}

pub fn encode_response(id: u128, status: PackStatus, packet_id: i32) -> Bytes {
    encode_response_writer(id, status, packet_id).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body<P: Encode>(packet: &P) -> Vec<u8> {
        let mut w = PacketWriter::new();
        packet.encode(&mut w);
        w.into_bytes().to_vec()
    }

    #[tuff_core::watchdog]
    #[test]
    fn push_without_prompt() {
        let hash = "a".repeat(40);
        let bytes = body(&ConfigPush(Push {
            id: 1,
            url: "http://h/p.zip",
            hash: &hash,
            required: true,
            prompt: None,
        }));
        let mut expected = vec![0u8; 15];
        expected.push(1);
        expected.push(14);
        expected.extend_from_slice(b"http://h/p.zip");
        expected.push(40);
        expected.extend_from_slice(hash.as_bytes());
        expected.extend_from_slice(&[1, 0]);
        assert_eq!(bytes, expected);
    }

    #[tuff_core::watchdog]
    #[test]
    fn push_with_prompt_is_network_nbt() {
        let prompt = TextComponent::new("hi");
        let bytes = body(&PlayPush(Push {
            id: 0,
            url: "u",
            hash: "h",
            required: false,
            prompt: Some(&prompt),
        }));
        let tail = &bytes[16 + 2 + 2..];
        assert_eq!(tail[0], 0);
        assert_eq!(tail[1], 1);
        assert_eq!(tail[2], 8);
        assert_eq!(&tail[3..], &[0, 2, b'h', b'i']);
    }

    #[tuff_core::watchdog]
    #[test]
    fn pop_forms() {
        assert_eq!(body(&ConfigPop(None)), vec![0]);
        let mut expected = vec![1];
        expected.extend_from_slice(&7u128.to_be_bytes());
        assert_eq!(body(&PlayPop(Some(7))), expected);
    }

    #[tuff_core::watchdog]
    #[test]
    fn responses_decode_and_reject_unknown_statuses() {
        let framed = encode_response(9, PackStatus::Declined, 0x30);
        let mut r = PacketReader::new(framed);
        assert_eq!(r.read_var_int().unwrap(), 0x30);
        assert_eq!(
            decode_response(&mut r).unwrap(),
            (9, Some(PackStatus::Declined))
        );
        let mut w = PacketWriter::new();
        w.write_uuid(1);
        w.write_var_int(99);
        let mut r = PacketReader::new(w.into_bytes());
        assert_eq!(decode_response(&mut r).unwrap(), (1, None));
        for v in 0..8 {
            assert_eq!(PackStatus::from_wire(v).unwrap().to_wire(), v);
        }
    }
}
