//! Custom-payload style packets shared between connection states.

use tuff_nbt::{NbtLimits, Tag};
use tuff_wire::{PacketReader, PacketWriter, Result};

pub const MAX_DETAIL_COUNT: usize = 32;
const MAX_DETAIL_KEY: usize = 128;
const MAX_DETAIL_VALUE: usize = 4096;
const MAX_CLICK_PAYLOAD_BYTES: usize = 32768;
const MAX_CLICK_PAYLOAD_DEPTH: usize = 16;
pub const MAX_QUERY_PAYLOAD: usize = 1 << 20;

/// Writes the crash report detail map, at most [`MAX_DETAIL_COUNT`] entries.
pub fn write_report_details(w: &mut PacketWriter, details: &[(&str, &str)]) -> Result<()> {
    if details.len() > MAX_DETAIL_COUNT {
        return Err(tuff_wire::BufferError::StringTooLong(details.len()));
    }
    w.write_var_int(details.len() as i32);
    for (key, value) in details {
        w.write_string_chars(key, MAX_DETAIL_KEY)?;
        w.write_string_chars(value, MAX_DETAIL_VALUE)?;
    }
    Ok(())
}

/// A dialog button's custom click action: an identifier and optional NBT.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomClickAction {
    pub id: String,
    pub payload: Option<Tag>,
}

impl CustomClickAction {
    pub fn decode(r: &mut PacketReader) -> anyhow::Result<Self> {
        let id = r.read_string_with_max(256)?;
        let len = r.read_var_int()?;
        anyhow::ensure!(
            (0..=MAX_CLICK_PAYLOAD_BYTES as i32).contains(&len),
            "custom click payload too large"
        );
        let bytes = r.read_bytes(len as usize)?;
        let mut input: &[u8] = &bytes;
        let limits = NbtLimits {
            max_depth: MAX_CLICK_PAYLOAD_DEPTH,
            max_bytes: MAX_CLICK_PAYLOAD_BYTES,
        };
        let payload = Tag::read_network(&mut input, limits)?;
        anyhow::ensure!(input.is_empty(), "trailing bytes in custom click payload");
        Ok(Self { id, payload })
    }

    pub fn encode(&self, w: &mut PacketWriter) {
        w.write_string(&self.id);
        let mut body = Vec::new();
        match &self.payload {
            Some(tag) => tag.write_network(&mut body).expect("in-memory NBT encodes"),
            None => body.push(0),
        }
        w.write_var_int(body.len() as i32);
        w.write_bytes(&body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reader(w: PacketWriter) -> PacketReader {
        let mut buf = tuff_wire::Buffer::new();
        buf.extend(&w.finish()).unwrap();
        PacketReader::new(buf.try_split_packet().unwrap().unwrap())
    }

    #[tuff_core::watchdog]
    #[test]
    fn click_action_roundtrips() {
        for payload in [None, Some(Tag::Int(7))] {
            let action = CustomClickAction {
                id: "minecraft:test".into(),
                payload,
            };
            let mut w = PacketWriter::new();
            action.encode(&mut w);
            assert_eq!(CustomClickAction::decode(&mut reader(w)).unwrap(), action);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn click_action_rejects_oversized_and_trailing_payloads() {
        let mut w = PacketWriter::new();
        w.write_string("a:b");
        w.write_var_int(MAX_CLICK_PAYLOAD_BYTES as i32 + 1);
        w.write_bytes(&vec![0; MAX_CLICK_PAYLOAD_BYTES + 1]);
        assert!(CustomClickAction::decode(&mut reader(w)).is_err());

        let mut w = PacketWriter::new();
        w.write_string("a:b");
        w.write_var_int(2);
        w.write_bytes(&[0, 0]);
        assert!(CustomClickAction::decode(&mut reader(w)).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn report_details_enforce_limits() {
        let mut w = PacketWriter::new();
        assert!(write_report_details(&mut w, &[("k", "v")]).is_ok());
        let long = "x".repeat(MAX_DETAIL_VALUE + 1);
        assert!(write_report_details(&mut w, &[("k", &long)]).is_err());
        let many = vec![("k", "v"); MAX_DETAIL_COUNT + 1];
        assert!(write_report_details(&mut w, &many).is_err());
    }
}
