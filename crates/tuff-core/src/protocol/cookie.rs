//! Cookie packets, which let a server park up to 5 KiB per key on the client
//! across transfers. Store and request exist in configuration and play with
//! different IDs, so each state has its own clientbound types.

use bytes::Bytes;
use tuff_wire::{PacketReader, PacketWriter, Result};

use crate::protocol::Encode;

pub const MAX_COOKIE_PAYLOAD: usize = 5120;
const MAX_KEY_BYTES: usize = 32_767 * 3;

/// A client's answer to a cookie request; `payload` is `None` when it holds no
/// cookie under `key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieResponse {
    pub key: String,
    pub payload: Option<Bytes>,
}

impl CookieResponse {
    pub fn decode(r: &mut PacketReader) -> Result<Self> {
        let key = r.read_string_with_max(MAX_KEY_BYTES)?;
        let payload = if r.read_bool()? {
            Some(r.read_prefixed_bytes(MAX_COOKIE_PAYLOAD)?)
        } else {
            None
        };
        Ok(Self { key, payload })
    }
}

impl CookieResponse {
    pub fn encode(&self, w: &mut PacketWriter) {
        w.write_string(&self.key);
        w.write_bool(self.payload.is_some());
        if let Some(payload) = &self.payload {
            w.write_prefixed_bytes(payload);
        }
    }
}

fn write_store(w: &mut PacketWriter, key: &str, payload: &[u8]) {
    debug_assert!(payload.len() <= MAX_COOKIE_PAYLOAD);
    w.write_string(key);
    w.write_prefixed_bytes(payload);
}

macro_rules! cookie_packets {
    ($state:ident, $store:ident, $request:ident) => {
        pub struct $store<'a> {
            pub key: &'a str,
            pub payload: &'a [u8],
        }

        impl Encode for $store<'_> {
            const ID: i32 = tuff_vanilla_data::packet::$state::Clientbound::StoreCookie.id();

            fn encode(&self, w: &mut PacketWriter) {
                write_store(w, self.key, self.payload);
            }
        }

        pub struct $request<'a>(pub &'a str);

        impl Encode for $request<'_> {
            const ID: i32 = tuff_vanilla_data::packet::$state::Clientbound::CookieRequest.id();

            fn encode(&self, w: &mut PacketWriter) {
                w.write_string(self.0);
            }
        }
    };
}

cookie_packets!(configuration, ConfigStoreCookie, ConfigCookieRequest);
cookie_packets!(play, PlayStoreCookie, PlayCookieRequest);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::encode_packet;
    use tuff_wire::Buffer;

    fn reader(w: PacketWriter) -> PacketReader {
        let mut buf = Buffer::new();
        buf.extend(&w.finish()).unwrap();
        PacketReader::new(buf.try_split_packet().unwrap().unwrap())
    }

    #[tuff_core::watchdog]
    #[test]
    fn store_cookie_uses_each_states_id_and_encodes_key_and_payload() {
        let mut r = reader(encode_packet(&ConfigStoreCookie {
            key: "tuff:session",
            payload: &[1, 2, 3],
        }));
        assert_eq!(
            r.read_var_int().unwrap(),
            tuff_vanilla_data::packet::configuration::Clientbound::StoreCookie.id()
        );
        assert_eq!(r.read_string().unwrap(), "tuff:session");
        assert_eq!(&r.read_prefixed_bytes(16).unwrap()[..], &[1, 2, 3]);

        let mut r = reader(encode_packet(&PlayCookieRequest("tuff:session")));
        assert_eq!(
            r.read_var_int().unwrap(),
            tuff_vanilla_data::packet::play::Clientbound::CookieRequest.id()
        );
        assert_eq!(r.read_string().unwrap(), "tuff:session");
    }

    fn response(payload: Option<&[u8]>) -> PacketReader {
        let mut w = PacketWriter::new();
        w.write_string("tuff:session");
        w.write_bool(payload.is_some());
        if let Some(p) = payload {
            w.write_prefixed_bytes(p);
        }
        reader(w)
    }

    #[tuff_core::watchdog]
    #[test]
    fn cookie_response_decodes_present_and_absent_payloads() {
        let got = CookieResponse::decode(&mut response(Some(&[9; 5120]))).unwrap();
        assert_eq!(got.key, "tuff:session");
        assert_eq!(got.payload.unwrap().len(), 5120);
        assert_eq!(
            CookieResponse::decode(&mut response(None)).unwrap().payload,
            None
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn cookie_response_rejects_oversized_and_truncated_payloads() {
        assert!(CookieResponse::decode(&mut response(Some(&[0; 5121]))).is_err());
        let mut w = PacketWriter::new();
        w.write_string("k");
        w.write_bool(true);
        w.write_var_int(100);
        w.write_bytes(&[0; 4]);
        assert!(CookieResponse::decode(&mut reader(w)).is_err());
    }
}
