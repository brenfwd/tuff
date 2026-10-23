//! The pre-1.7 server list ping: a raw `0xFE` byte instead of a framed
//! handshake. Answered with a legacy kick packet and the connection closed.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::protocol::VERSION_NAME;
use crate::status::ServerListInfo;

const GET_INFO: u8 = 0xFE;
const GET_INFO_VERSION: u8 = 1;
const CUSTOM_PAYLOAD: u8 = 0xFA;
const DISCONNECT: u8 = 0xFF;
const PING_CHANNEL: &str = "MC|PingHost";
const FAKE_PROTOCOL: i32 = 127;
const MIN_PING_PROTOCOL: u8 = 73;
const PEEK_BUFFER: usize = 1024;
const PEEK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Ping {
    Beta,
    V14,
    V16,
}

/// Whether the first bytes of a connection are a legacy ping and, if so, its
/// flavor. Anything malformed falls through to the normal handshake.
fn classify(bytes: &[u8]) -> Option<Ping> {
    let (&first, rest) = bytes.split_first()?;
    if first != GET_INFO {
        return None;
    }
    let Some((&version, rest)) = rest.split_first() else {
        return Some(Ping::Beta);
    };
    if version != GET_INFO_VERSION {
        return None;
    }
    if rest.is_empty() {
        return Some(Ping::V14);
    }
    custom_payload_is_ping(rest).then_some(Ping::V16)
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(n)?;
        self.0 = tail;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }

    fn legacy_string(&mut self) -> Option<String> {
        let chars = usize::from(self.u16()?);
        let raw = self.take(chars * 2)?;
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        Some(String::from_utf16_lossy(&units))
    }
}

fn custom_payload_is_ping(bytes: &[u8]) -> bool {
    let mut cursor = Cursor(bytes);
    let parsed = (|| {
        if cursor.u8()? != CUSTOM_PAYLOAD || cursor.legacy_string()? != PING_CHANNEL {
            return None;
        }
        let payload = usize::from(cursor.u16()?);
        if cursor.0.len() != payload || cursor.u8()? < MIN_PING_PROTOCOL {
            return None;
        }
        cursor.legacy_string()?;
        let port = cursor.take(4)?;
        (i32::from_be_bytes([port[0], port[1], port[2], port[3]]) <= 65535).then_some(())
    })();
    parsed.is_some()
}

fn response_body(ping: Ping, list: &ServerListInfo) -> String {
    let motd = list.motd.plain();
    let online = list.players.len();
    let max = list.max_players;
    match ping {
        Ping::Beta => format!("{motd}\u{a7}{online}\u{a7}{max}"),
        Ping::V14 | Ping::V16 => {
            format!("\u{a7}1\0{FAKE_PROTOCOL}\0{VERSION_NAME}\0{motd}\0{online}\0{max}")
        }
    }
}

fn kick_packet(body: &str) -> Vec<u8> {
    let units: Vec<u16> = body.encode_utf16().collect();
    let mut out = Vec::with_capacity(3 + units.len() * 2);
    out.push(DISCONNECT);
    out.extend_from_slice(&(units.len().min(usize::from(u16::MAX)) as u16).to_be_bytes());
    for unit in units.iter().take(usize::from(u16::MAX)) {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    out
}

/// Answers and closes the connection when it opens with a legacy ping.
/// Returns the socket untouched (nothing consumed) when it does not.
pub async fn try_answer(
    mut socket: TcpStream,
    list: &ServerListInfo,
) -> std::io::Result<Option<TcpStream>> {
    let mut buffer = [0u8; PEEK_BUFFER];
    let peeked = match tokio::time::timeout(PEEK_TIMEOUT, socket.peek(&mut buffer)).await {
        Ok(result) => result?,
        Err(_) => return Ok(Some(socket)),
    };
    let Some(ping) = classify(&buffer[..peeked]) else {
        return Ok(Some(socket));
    };
    socket.read_exact(&mut buffer[..peeked]).await?;
    let reply = kick_packet(&response_body(ping, list));
    socket.write_all(&reply).await?;
    socket.shutdown().await?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::net::TcpListener;

    use super::*;

    fn list() -> ServerListInfo {
        ServerListInfo {
            motd: "A Server".into(),
            favicon: None,
            max_players: 20,
            players: Vec::new(),
            hide_players: false,
            enforces_secure_chat: false,
        }
    }

    fn legacy_string(s: &str) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().collect();
        let mut out = (units.len() as u16).to_be_bytes().to_vec();
        for u in units {
            out.extend_from_slice(&u.to_be_bytes());
        }
        out
    }

    fn v16_ping(protocol: u8, port: i32) -> Vec<u8> {
        let host = legacy_string("localhost");
        let mut payload = vec![protocol];
        payload.extend_from_slice(&host);
        payload.extend_from_slice(&port.to_be_bytes());
        let mut out = vec![0xFE, 1, 0xFA];
        out.extend_from_slice(&legacy_string(PING_CHANNEL));
        out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&payload);
        out
    }

    #[tuff_core::watchdog]
    #[test]
    fn classifies_each_flavor_and_rejects_the_rest() {
        assert_eq!(classify(&[0xFE]), Some(Ping::Beta));
        assert_eq!(classify(&[0xFE, 1]), Some(Ping::V14));
        assert_eq!(classify(&v16_ping(78, 25565)), Some(Ping::V16));
        assert_eq!(classify(&v16_ping(72, 25565)), None);
        assert_eq!(classify(&v16_ping(78, 70000)), None);
        assert_eq!(classify(&[0xFE, 2]), None);
        assert_eq!(classify(&[0x10, 0x00]), None);
        assert_eq!(classify(&[]), None);
        let mut truncated = v16_ping(78, 25565);
        truncated.pop();
        assert_eq!(classify(&truncated), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn bodies_match_the_legacy_formats() {
        let list = list();
        assert_eq!(response_body(Ping::Beta, &list), "A Server\u{a7}0\u{a7}20");
        assert_eq!(
            response_body(Ping::V14, &list),
            format!("\u{a7}1\0127\0{VERSION_NAME}\0A Server\00\020")
        );
        let packet = kick_packet("ab");
        assert_eq!(packet, [0xFF, 0, 2, 0, b'a', 0, b'b']);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    #[tuff_core::watchdog]
    async fn answers_a_legacy_ping_and_leaves_modern_clients_alone() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let list = Arc::new(list());

        let mut client = TcpStream::connect(address).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        client.write_all(&[0xFE, 1]).await.unwrap();
        assert!(try_answer(server, &list).await.unwrap().is_none());
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).await.unwrap();
        assert_eq!(reply[0], 0xFF);
        let chars = usize::from(u16::from_be_bytes([reply[1], reply[2]]));
        assert_eq!(reply.len(), 3 + chars * 2);

        let mut client = TcpStream::connect(address).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        client.write_all(&[0x10, 0x00]).await.unwrap();
        assert!(try_answer(server, &list).await.unwrap().is_some());
    }
}
