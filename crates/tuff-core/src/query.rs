//! The GameSpy4 query protocol (`enable-query`, `query.port`): a UDP
//! handshake that issues a challenge token, then basic and full stat replies.
//!
//! Every datagram is untrusted: input is bounded, only whole well-formed
//! requests are answered, a stat needs a token tied to the sender's address,
//! and replies never exceed one datagram.

use std::hash::{BuildHasher, RandomState};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::{UdpSocket, lookup_host};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::Instant;

use crate::protocol::VERSION_NAME;
use crate::status::ServerListInfo;

const MAGIC: [u8; 2] = [0xFE, 0xFD];
const HANDSHAKE: u8 = 9;
const STAT: u8 = 0;
const SESSION_MASK: i32 = 0x0F0F_0F0F;
const RECV_BUFFER: usize = 1500;
const MAX_RESPONSE: usize = 1400;
const BASIC_REQUEST: usize = 11;
const FULL_REQUEST: usize = 15;
const TOKEN_LIFETIME: Duration = Duration::from_secs(30);
const SPLITNUM: &[u8] = b"splitnum\0\x80\0";
const PLAYER_SECTION: &[u8] = b"\x01player_\0\0";

/// What the replies report besides the live server list info.
#[derive(Clone)]
pub struct QueryInfo {
    pub list: watch::Receiver<Arc<ServerListInfo>>,
    pub map: String,
    pub host_port: u16,
    pub host_ip: String,
}

/// The addresses vanilla binds the query socket to: `server-ip` when set,
/// otherwise every IPv4 and IPv6 interface.
pub fn bind_addrs(ip: &str, port: u16) -> Vec<String> {
    let ip = ip.trim();
    if ip.is_empty() {
        vec![format!("0.0.0.0:{port}"), format!("[::]:{port}")]
    } else if ip.contains(':') && !ip.starts_with('[') {
        vec![format!("[{ip}]:{port}")]
    } else {
        vec![format!("{ip}:{port}")]
    }
}

fn bind_udp(addr: SocketAddr) -> Result<UdpSocket> {
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    if addr.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    Ok(UdpSocket::from_std(socket.into())?)
}

pub struct QueryServer {
    sockets: Vec<UdpSocket>,
    info: QueryInfo,
}

impl QueryServer {
    pub async fn bind(binds: &[String], info: QueryInfo) -> Result<Self> {
        let mut sockets = Vec::new();
        for bind in binds {
            let addrs = lookup_host(bind.as_str())
                .await
                .with_context(|| format!("resolving query bind address {bind}"))?;
            for addr in addrs {
                sockets
                    .push(bind_udp(addr).with_context(|| format!("binding query socket {addr}"))?);
            }
        }
        ensure!(!sockets.is_empty(), "no query bind addresses configured");
        Ok(Self { sockets, info })
    }

    pub fn local_addrs(&self) -> Vec<SocketAddr> {
        self.sockets
            .iter()
            .filter_map(|socket| socket.local_addr().ok())
            .collect()
    }

    pub async fn run(self) {
        let mut loops = JoinSet::new();
        for socket in self.sockets {
            loops.spawn(serve_socket(socket, self.info.clone()));
        }
        while loops.join_next().await.is_some() {}
    }
}

/// Stateless challenge tokens: a keyed hash of the sender's address under a
/// secret that rotates every [`TOKEN_LIFETIME`]; the previous secret is still
/// accepted, so a token lives between 30 and 60 seconds.
struct Challenges {
    current: RandomState,
    previous: RandomState,
    rotated: Instant,
}

impl Challenges {
    fn new() -> Self {
        Self {
            current: RandomState::new(),
            previous: RandomState::new(),
            rotated: Instant::now(),
        }
    }

    fn rotate_if_due(&mut self) {
        if self.rotated.elapsed() >= TOKEN_LIFETIME {
            self.previous = std::mem::replace(&mut self.current, RandomState::new());
            self.rotated = Instant::now();
        }
    }

    fn token(keys: &RandomState, from: SocketAddr) -> i32 {
        (keys.hash_one(from) & 0x7FFF_FFFF) as i32
    }

    fn issue(&mut self, from: SocketAddr) -> i32 {
        self.rotate_if_due();
        Self::token(&self.current, from)
    }

    fn accepts(&mut self, from: SocketAddr, token: i32) -> bool {
        self.rotate_if_due();
        token == Self::token(&self.current, from) || token == Self::token(&self.previous, from)
    }
}

async fn serve_socket(socket: UdpSocket, info: QueryInfo) {
    let mut challenges = Challenges::new();
    let mut buffer = [0u8; RECV_BUFFER];
    loop {
        let Ok((len, from)) = socket.recv_from(&mut buffer).await else {
            continue;
        };
        if len >= RECV_BUFFER {
            continue;
        }
        if let Some(reply) = respond(&buffer[..len], from, &mut challenges, &info) {
            let _ = socket.send_to(&reply, from).await;
        }
    }
}

fn respond(
    packet: &[u8],
    from: SocketAddr,
    challenges: &mut Challenges,
    info: &QueryInfo,
) -> Option<Vec<u8>> {
    let [m0, m1, kind, s0, s1, s2, s3, rest @ ..] = packet else {
        return None;
    };
    if [*m0, *m1] != MAGIC {
        return None;
    }
    let session = i32::from_be_bytes([*s0, *s1, *s2, *s3]) & SESSION_MASK;
    let mut reply = vec![*kind];
    reply.extend_from_slice(&session.to_be_bytes());
    match *kind {
        HANDSHAKE => {
            reply.extend_from_slice(challenges.issue(from).to_string().as_bytes());
            reply.push(0);
        }
        STAT => {
            let token = i32::from_be_bytes(rest.get(..4)?.try_into().ok()?);
            if !challenges.accepts(from, token) {
                return None;
            }
            let list = info.list.borrow().clone();
            match packet.len() {
                BASIC_REQUEST => basic_stat(&mut reply, &list, info),
                FULL_REQUEST => full_stat(&mut reply, &list, info),
                _ => return None,
            }
        }
        _ => return None,
    }
    Some(reply)
}

fn push_text(out: &mut Vec<u8>, text: &str) {
    out.extend(
        text.chars()
            .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
            .filter(|&b| b != 0),
    );
    out.push(0);
}

fn host_ip(info: &QueryInfo) -> &str {
    if info.host_ip.trim().is_empty() {
        "0.0.0.0"
    } else {
        info.host_ip.trim()
    }
}

fn basic_stat(out: &mut Vec<u8>, list: &ServerListInfo, info: &QueryInfo) {
    push_text(out, &list.motd.plain());
    push_text(out, "SMP");
    push_text(out, &info.map);
    push_text(out, &list.players.len().to_string());
    push_text(out, &list.max_players.to_string());
    out.extend_from_slice(&info.host_port.to_le_bytes());
    push_text(out, host_ip(info));
}

fn full_stat(out: &mut Vec<u8>, list: &ServerListInfo, info: &QueryInfo) {
    out.extend_from_slice(SPLITNUM);
    let pairs = [
        ("hostname", list.motd.plain()),
        ("gametype", "SMP".to_owned()),
        ("game_id", "MINECRAFT".to_owned()),
        ("version", VERSION_NAME.to_owned()),
        ("plugins", String::new()),
        ("map", info.map.clone()),
        ("numplayers", list.players.len().to_string()),
        ("maxplayers", list.max_players.to_string()),
        ("hostport", info.host_port.to_string()),
        ("hostip", host_ip(info).to_owned()),
    ];
    for (key, value) in pairs {
        push_text(out, key);
        push_text(out, &value);
    }
    out.push(0);
    out.extend_from_slice(PLAYER_SECTION);
    if !list.hide_players {
        for player in list.players.iter().filter(|p| p.listed) {
            if out.len() + player.name.len() + 2 > MAX_RESPONSE {
                break;
            }
            push_text(out, &player.name);
        }
    }
    out.push(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::OnlinePlayer;

    fn info(players: Vec<OnlinePlayer>) -> (QueryInfo, watch::Sender<Arc<ServerListInfo>>) {
        let list = ServerListInfo {
            motd: "A Server".into(),
            favicon: None,
            max_players: 20,
            players,
            hide_players: false,
            enforces_secure_chat: false,
        };
        let (tx, rx) = watch::channel(Arc::new(list));
        (
            QueryInfo {
                list: rx,
                map: "world".to_owned(),
                host_port: 25565,
                host_ip: String::new(),
            },
            tx,
        )
    }

    fn player(name: &str) -> OnlinePlayer {
        OnlinePlayer {
            name: name.to_owned(),
            uuid: 1,
            listed: true,
        }
    }

    fn handshake(session: i32) -> Vec<u8> {
        let mut p = vec![0xFE, 0xFD, 9];
        p.extend_from_slice(&session.to_be_bytes());
        p
    }

    fn stat(session: i32, token: i32, full: bool) -> Vec<u8> {
        let mut p = vec![0xFE, 0xFD, 0];
        p.extend_from_slice(&session.to_be_bytes());
        p.extend_from_slice(&token.to_be_bytes());
        if full {
            p.extend_from_slice(&[0; 4]);
        }
        p
    }

    async fn exchange(socket: &UdpSocket, to: SocketAddr, packet: &[u8]) -> Option<Vec<u8>> {
        socket.send_to(packet, to).await.unwrap();
        let mut buf = [0u8; 2048];
        let wait = tokio::time::timeout(Duration::from_millis(300), socket.recv_from(&mut buf));
        let (n, _) = wait.await.ok()?.unwrap();
        Some(buf[..n].to_vec())
    }

    async fn token_of(socket: &UdpSocket, to: SocketAddr) -> i32 {
        let reply = exchange(socket, to, &handshake(0x0102_0304)).await.unwrap();
        assert_eq!(reply[0], 9);
        assert_eq!(reply[1..5], 0x0102_0304i32.to_be_bytes());
        assert_eq!(*reply.last().unwrap(), 0);
        std::str::from_utf8(&reply[5..reply.len() - 1])
            .unwrap()
            .parse()
            .unwrap()
    }

    async fn server(players: Vec<OnlinePlayer>, v6: bool) -> (SocketAddr, UdpSocket) {
        let (info, tx) = info(players);
        std::mem::forget(tx);
        let bind = if v6 { "[::1]:0" } else { "127.0.0.1:0" };
        let query = QueryServer::bind(&[bind.to_owned()], info).await.unwrap();
        let addr = query.local_addrs()[0];
        tokio::spawn(query.run());
        let client = UdpSocket::bind(if v6 { "[::1]:0" } else { "127.0.0.1:0" })
            .await
            .unwrap();
        (addr, client)
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn handshake_then_basic_stat_over_ipv4() {
        let (addr, client) = server(vec![player("Ann"), player("Bob")], false).await;
        let token = token_of(&client, addr).await;
        let reply = exchange(&client, addr, &stat(0x0102_0304, token, false))
            .await
            .unwrap();
        assert_eq!(reply[0], 0);
        assert_eq!(reply[1..5], (0x0102_0304i32 & SESSION_MASK).to_be_bytes());
        let mut expected = b"A Server\0SMP\0world\x002\x0020\0".to_vec();
        expected.extend_from_slice(&25565u16.to_le_bytes());
        expected.extend_from_slice(b"0.0.0.0\0");
        assert_eq!(reply[5..], expected[..]);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn full_stat_lists_keys_and_players_over_ipv6() {
        let (addr, client) = server(vec![player("Ann"), player("Bob")], true).await;
        let token = token_of(&client, addr).await;
        let reply = exchange(&client, addr, &stat(7, token, true))
            .await
            .unwrap();
        let body = &reply[5..];
        assert!(body.starts_with(SPLITNUM));
        let text = String::from_utf8_lossy(body);
        for needle in [
            "hostname\0A Server\0",
            "gametype\0SMP\0",
            "game_id\0MINECRAFT\0",
            "map\0world\0",
            "numplayers\x002\0",
            "maxplayers\x0020\0",
        ] {
            assert!(text.contains(needle), "missing {needle:?}");
        }
        assert!(body.ends_with(b"\x01player_\0\0Ann\0Bob\0\0"));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn malformed_and_unauthenticated_packets_get_no_reply() {
        let (addr, client) = server(Vec::new(), false).await;
        let token = token_of(&client, addr).await;
        let bad_magic = [0xFE, 0xFC, 9, 0, 0, 0, 1];
        let wrong_token = stat(1, token.wrapping_add(1), false);
        let mut wrong_length = stat(1, token, false);
        wrong_length.extend_from_slice(&[0; 2]);
        let mut oversize = stat(1, token, true);
        oversize.resize(1400, 0);
        let unknown_kind = [0xFE, 0xFD, 5, 0, 0, 0, 1];
        let short = [0xFE, 0xFD, 9, 0];
        let missing_token = [0xFE, 0xFD, 0, 0, 0, 0, 1, 0, 0];
        for packet in [
            &bad_magic[..],
            &wrong_token,
            &wrong_length,
            &oversize,
            &unknown_kind,
            &short,
            &missing_token,
            &[],
        ] {
            assert!(exchange(&client, addr, packet).await.is_none());
        }
        assert!(
            exchange(&client, addr, &stat(1, token, false))
                .await
                .is_some()
        );
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn token_is_bound_to_the_sender_address() {
        let (addr, client) = server(Vec::new(), false).await;
        let token = token_of(&client, addr).await;
        let other = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert!(
            exchange(&other, addr, &stat(1, token, false))
                .await
                .is_none()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn tokens_expire_after_two_rotations() {
        let from: SocketAddr = "127.0.0.1:1000".parse().unwrap();
        let mut challenges = Challenges::new();
        let token = challenges.issue(from);
        challenges.current = RandomState::new();
        challenges.previous = RandomState::new();
        assert!(!challenges.accepts(from, token));
    }

    #[tuff_core::watchdog]
    #[test]
    fn full_stat_is_bounded_and_respects_hidden_players() {
        let names = (0..500).map(|i| player(&format!("Player{i:04}"))).collect();
        let (query, _tx) = info(names);
        let mut out = Vec::new();
        full_stat(&mut out, &query.list.borrow(), &query);
        assert!(out.len() <= MAX_RESPONSE);

        let mut hidden = ServerListInfo::clone(&query.list.borrow());
        hidden.hide_players = true;
        let mut out = Vec::new();
        full_stat(&mut out, &hidden, &query);
        assert!(out.ends_with(b"\x01player_\0\0\0"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn default_binds_cover_both_ip_families() {
        assert_eq!(bind_addrs("", 25565), ["0.0.0.0:25565", "[::]:25565"]);
        assert_eq!(bind_addrs("::1", 7), ["[::1]:7"]);
        assert_eq!(bind_addrs("10.0.0.1", 7), ["10.0.0.1:7"]);
    }
}
