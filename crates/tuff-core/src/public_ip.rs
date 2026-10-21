use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

const MAGIC_COOKIE: u32 = 0x2112_A442;
const HEADER_LEN: usize = 20;
const MAX_DATAGRAM: usize = 1280;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_SUCCESS: u16 = 0x0101;
const BINDING_ERROR: u16 = 0x0111;
const MAPPED_ADDRESS: u16 = 0x0001;
const XOR_MAPPED_ADDRESS: u16 = 0x0020;
const FAMILY_V4: u8 = 0x01;
const FAMILY_V6: u8 = 0x02;

pub type TransactionId = [u8; 12];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    V4,
    V6,
}

impl Family {
    fn matches(self, addr: &SocketAddr) -> bool {
        matches!(
            (self, addr),
            (Family::V4, SocketAddr::V4(_)) | (Family::V6, SocketAddr::V6(_))
        )
    }

    fn unspecified(self) -> SocketAddr {
        match self {
            Family::V4 => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            Family::V6 => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
        }
    }
}

impl fmt::Display for Family {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Family::V4 => "v4",
            Family::V6 => "v6",
        })
    }
}

/// A STUN server queried over one address family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub family: Family,
}

impl Endpoint {
    pub fn new(host: impl Into<String>, port: u16, family: Family) -> Self {
        Self {
            host: host.into(),
            port,
            family,
        }
    }

    pub fn defaults() -> Vec<Self> {
        let mut out = Vec::new();
        for family in [Family::V4, Family::V6] {
            out.push(Self::new("stun.cloudflare.com", 3478, family));
            out.push(Self::new("stun.l.google.com", 19302, family));
        }
        out
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}/{}", self.host, self.port, self.family)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Dns(String),
    NoAddress,
    Socket(String),
    Send(String),
    Recv(String),
    Timeout,
    Malformed,
    WrongTransaction,
    ErrorResponse,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Dns(e) => write!(f, "dns: {e}"),
            Failure::NoAddress => f.write_str("no address of this family"),
            Failure::Socket(e) => write!(f, "socket: {e}"),
            Failure::Send(e) => write!(f, "send: {e}"),
            Failure::Recv(e) => write!(f, "recv: {e}"),
            Failure::Timeout => f.write_str("timeout"),
            Failure::Malformed => f.write_str("malformed response"),
            Failure::WrongTransaction => f.write_str("wrong transaction id"),
            Failure::ErrorResponse => f.write_str("error response"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PublicIpConfig {
    pub detect: bool,
    pub explicit_base_url: Option<String>,
    pub endpoints: Vec<Endpoint>,
    /// How long each transmission waits for a reply before retransmitting.
    pub timeout: Duration,
    pub attempts: u32,
}

impl PublicIpConfig {
    pub fn new(detect: bool, explicit_base_url: Option<String>) -> Self {
        Self {
            detect,
            explicit_base_url: explicit_base_url.filter(|u| !u.trim().is_empty()),
            endpoints: Endpoint::defaults(),
            timeout: Duration::from_secs(1),
            attempts: 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Detected {
    pub v4: Option<Ipv4Addr>,
    pub v6: Option<Ipv6Addr>,
}

impl Detected {
    pub fn preferred(&self) -> Option<IpAddr> {
        self.v4.map(IpAddr::V4).or(self.v6.map(IpAddr::V6))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub detected: Detected,
    pub failures: Vec<(Endpoint, Failure)>,
}

impl Report {
    pub fn summary(&self) -> String {
        if self.failures.is_empty() {
            return "no servers configured".into();
        }
        self.failures
            .iter()
            .map(|(e, f)| format!("{e} {f}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn binding_request(txn: &TransactionId) -> [u8; HEADER_LEN] {
    let mut out = [0u8; HEADER_LEN];
    out[..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    out[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    out[8..].copy_from_slice(txn);
    out
}

/// A Binding success response carrying `mapped` as XOR-MAPPED-ADDRESS.
pub fn binding_response(txn: &TransactionId, mapped: SocketAddr) -> Vec<u8> {
    encode_message(
        BINDING_SUCCESS,
        txn,
        &[(XOR_MAPPED_ADDRESS, encode_address(mapped, Some(txn)))],
    )
}

fn encode_message(kind: u16, txn: &TransactionId, attrs: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (kind, value) in attrs {
        body.extend_from_slice(&kind.to_be_bytes());
        body.extend_from_slice(&(value.len() as u16).to_be_bytes());
        body.extend_from_slice(value);
        body.resize(body.len().next_multiple_of(4), 0);
    }
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
    out.extend_from_slice(txn);
    out.extend_from_slice(&body);
    out
}

fn xor_masks(xor: Option<&TransactionId>) -> (u16, [u8; 16]) {
    let Some(txn) = xor else {
        return (0, [0; 16]);
    };
    let mut mask = [0u8; 16];
    mask[..4].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    mask[4..].copy_from_slice(txn);
    ((MAGIC_COOKIE >> 16) as u16, mask)
}

fn encode_address(addr: SocketAddr, xor: Option<&TransactionId>) -> Vec<u8> {
    let (port_mask, mask) = xor_masks(xor);
    let (family, mut ip) = match addr.ip() {
        IpAddr::V4(a) => (FAMILY_V4, a.octets().to_vec()),
        IpAddr::V6(a) => (FAMILY_V6, a.octets().to_vec()),
    };
    ip.iter_mut().zip(mask).for_each(|(b, m)| *b ^= m);
    let mut out = vec![0, family];
    out.extend_from_slice(&(addr.port() ^ port_mask).to_be_bytes());
    out.extend_from_slice(&ip);
    out
}

fn unmask<const N: usize>(raw: &[u8], mask: &[u8; 16]) -> [u8; N] {
    let mut out = [0u8; N];
    for (o, (b, m)) in out.iter_mut().zip(raw.iter().zip(mask)) {
        *o = b ^ m;
    }
    out
}

fn decode_address(value: &[u8], xor: Option<&TransactionId>) -> Result<SocketAddr, Failure> {
    let (port_mask, mask) = xor_masks(xor);
    let [_, family, p0, p1, raw @ ..] = value else {
        return Err(Failure::Malformed);
    };
    let port = u16::from_be_bytes([*p0, *p1]) ^ port_mask;
    let ip = match (*family, raw.len()) {
        (FAMILY_V4, 4) => IpAddr::V4(Ipv4Addr::from(unmask::<4>(raw, &mask))),
        (FAMILY_V6, 16) => IpAddr::V6(Ipv6Addr::from(unmask::<16>(raw, &mask))),
        _ => return Err(Failure::Malformed),
    };
    Ok(SocketAddr::new(ip, port))
}

/// Parses an untrusted datagram as the Binding response to `txn`.
/// Prefers XOR-MAPPED-ADDRESS over MAPPED-ADDRESS.
pub fn parse_binding_response(packet: &[u8], txn: &TransactionId) -> Result<SocketAddr, Failure> {
    if packet.len() < HEADER_LEN || packet.len() > MAX_DATAGRAM {
        return Err(Failure::Malformed);
    }
    let kind = u16::from_be_bytes([packet[0], packet[1]]);
    let length = u16::from_be_bytes([packet[2], packet[3]]) as usize;
    let cookie = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    if kind & 0xC000 != 0
        || cookie != MAGIC_COOKIE
        || !length.is_multiple_of(4)
        || HEADER_LEN + length != packet.len()
    {
        return Err(Failure::Malformed);
    }
    if packet[8..HEADER_LEN] != txn[..] {
        return Err(Failure::WrongTransaction);
    }
    match kind {
        BINDING_SUCCESS => {}
        BINDING_ERROR => return Err(Failure::ErrorResponse),
        _ => return Err(Failure::Malformed),
    }
    let mut xored = None;
    let mut plain = None;
    let mut rest = &packet[HEADER_LEN..];
    while !rest.is_empty() {
        let [t0, t1, l0, l1, tail @ ..] = rest else {
            return Err(Failure::Malformed);
        };
        let attr = u16::from_be_bytes([*t0, *t1]);
        let len = u16::from_be_bytes([*l0, *l1]) as usize;
        let value = tail.get(..len).ok_or(Failure::Malformed)?;
        rest = tail
            .get(len.next_multiple_of(4)..)
            .ok_or(Failure::Malformed)?;
        match attr {
            XOR_MAPPED_ADDRESS if xored.is_none() => {
                xored = Some(decode_address(value, Some(txn))?)
            }
            MAPPED_ADDRESS if plain.is_none() => plain = Some(decode_address(value, None)?),
            _ => {}
        }
    }
    let addr = xored.or(plain).ok_or(Failure::Malformed)?;
    if addr.ip().is_unspecified() {
        return Err(Failure::Malformed);
    }
    Ok(addr)
}

async fn resolve(endpoint: &Endpoint, timeout: Duration) -> Result<SocketAddr, Failure> {
    let lookup = tokio::net::lookup_host((endpoint.host.as_str(), endpoint.port));
    let addrs = tokio::time::timeout(timeout, lookup)
        .await
        .map_err(|_| Failure::Dns("timed out".into()))?
        .map_err(|e| Failure::Dns(e.to_string()))?;
    addrs
        .into_iter()
        .find(|a| endpoint.family.matches(a))
        .ok_or(Failure::NoAddress)
}

/// One STUN Binding transaction: up to `attempts` transmissions of the
/// same request, each waiting up to `timeout` for a valid reply.
pub async fn probe(
    endpoint: &Endpoint,
    timeout: Duration,
    attempts: u32,
) -> Result<IpAddr, Failure> {
    let target = resolve(endpoint, timeout).await?;
    let socket = UdpSocket::bind(endpoint.family.unspecified())
        .await
        .map_err(|e| Failure::Socket(e.to_string()))?;
    socket
        .connect(target)
        .await
        .map_err(|e| Failure::Socket(e.to_string()))?;
    let txn: TransactionId = rand::random();
    let request = binding_request(&txn);
    let mut buf = [0u8; MAX_DATAGRAM + 1];
    let mut rejected = None;
    for _ in 0..attempts.max(1) {
        socket
            .send(&request)
            .await
            .map_err(|e| Failure::Send(e.to_string()))?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, socket.recv(&mut buf)).await {
                Err(_) => break,
                Ok(Err(e)) => return Err(Failure::Recv(e.to_string())),
                Ok(Ok(n)) => match parse_binding_response(&buf[..n], &txn) {
                    Ok(addr) => return Ok(addr.ip()),
                    Err(f) => rejected = Some(f),
                },
            }
        }
    }
    Err(rejected.unwrap_or(Failure::Timeout))
}

pub async fn detect(endpoints: &[Endpoint], timeout: Duration, attempts: u32) -> Detected {
    detect_report(endpoints, timeout, attempts).await.detected
}

pub async fn detect_report(endpoints: &[Endpoint], timeout: Duration, attempts: u32) -> Report {
    let mut probes = JoinSet::new();
    for (i, e) in endpoints.iter().cloned().enumerate() {
        probes.spawn(async move { (i, probe(&e, timeout, attempts).await) });
    }
    let mut results: Vec<Option<Result<IpAddr, Failure>>> = vec![None; endpoints.len()];
    while let Some(joined) = probes.join_next().await {
        if let Ok((i, result)) = joined {
            results[i] = Some(result);
        }
    }
    let mut report = Report::default();
    for (endpoint, result) in endpoints.iter().zip(results) {
        match result.unwrap_or(Err(Failure::Timeout)) {
            Ok(IpAddr::V4(a)) => report.detected.v4 = report.detected.v4.or(Some(a)),
            Ok(IpAddr::V6(a)) => report.detected.v6 = report.detected.v6.or(Some(a)),
            Err(f) => {
                log::debug!("public address probe {endpoint} failed: {f}");
                report.failures.push((endpoint.clone(), f));
            }
        }
    }
    report
}

pub fn format_base_url(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(a) => format!("http://{a}:{port}"),
        IpAddr::V6(a) => format!("http://[{a}]:{port}"),
    }
}

pub fn fallback_base_url(bound: SocketAddr) -> String {
    let ip = if bound.ip().is_unspecified() {
        match bound.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        }
    } else {
        bound.ip()
    };
    format_base_url(ip, bound.port())
}

struct Reconfigure {
    config: PublicIpConfig,
    bound: SocketAddr,
}

impl std::fmt::Debug for BaseUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("BaseUrl").field(&self.current()).finish()
    }
}

#[derive(Clone)]
pub struct BaseUrl {
    rx: watch::Receiver<String>,
    commands: mpsc::UnboundedSender<Reconfigure>,
}

impl BaseUrl {
    pub fn detached(url: &str) -> Self {
        let (_tx, rx) = watch::channel(url.to_string());
        let (commands, _r) = mpsc::unbounded_channel();
        Self { rx, commands }
    }

    pub fn current(&self) -> String {
        self.rx.borrow().clone()
    }

    pub fn watch(&self) -> watch::Receiver<String> {
        self.rx.clone()
    }

    pub fn configure(&self, config: PublicIpConfig, bound: SocketAddr) {
        let _ = self.commands.send(Reconfigure { config, bound });
    }
}

pub fn config_from_store(
    store: &crate::config::ConfigStore,
    endpoints: Option<&[Endpoint]>,
) -> PublicIpConfig {
    let mut config = PublicIpConfig::new(
        store
            .get_bool(crate::config::HTTP_DETECT_PUBLIC_IP)
            .unwrap_or(false),
        store.get_string(crate::config::HTTP_RESOURCE_BASE_URL),
    );
    if let Some(endpoints) = endpoints {
        config.endpoints = endpoints.to_vec();
    }
    config
}

pub fn start() -> BaseUrl {
    let (tx, rx) = watch::channel(String::new());
    let (commands, mut inbox) = mpsc::unbounded_channel::<Reconfigure>();
    tokio::spawn(async move {
        let mut pending = inbox.recv().await;
        while let Some(Reconfigure { config, bound }) = pending.take() {
            let fallback = fallback_base_url(bound);
            if let Some(url) = &config.explicit_base_url {
                tx.send_replace(url.trim_end_matches('/').to_string());
                pending = inbox.recv().await;
                continue;
            }
            tx.send_replace(fallback.clone());
            if !config.detect {
                pending = inbox.recv().await;
                continue;
            }
            let detection = detect_report(&config.endpoints, config.timeout, config.attempts);
            tokio::select! {
                next = inbox.recv() => pending = next,
                report = detection => {
                    match report.detected.preferred() {
                        Some(ip) => {
                            log::info!(
                                "detected public address {ip}; this does not mean port {} is reachable",
                                bound.port()
                            );
                            tx.send_replace(format_base_url(ip, bound.port()));
                        }
                        None => log::warn!(
                            "public address detection failed ({}); using bound address {fallback}",
                            report.summary()
                        ),
                    }
                    pending = inbox.recv().await;
                }
            }
        }
    });
    BaseUrl { rx, commands }
}

pub fn spawn(config: PublicIpConfig, bound: SocketAddr) -> BaseUrl {
    let base = start();
    base.configure(config, bound);
    base
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type Reply = Box<dyn Fn(usize, &TransactionId) -> Vec<Vec<u8>> + Send + Sync>;

    async fn mock_on(bind: &str, family: Family, reply: Reply) -> (Endpoint, Arc<AtomicUsize>) {
        let socket = UdpSocket::bind(bind).await.unwrap();
        let addr = socket.local_addr().unwrap();
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = seen.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((n, peer)) = socket.recv_from(&mut buf).await else {
                    return;
                };
                let i = counter.fetch_add(1, Ordering::SeqCst);
                if n != HEADER_LEN || buf[..2] != BINDING_REQUEST.to_be_bytes() {
                    continue;
                }
                let txn: TransactionId = buf[8..HEADER_LEN].try_into().unwrap();
                for datagram in reply(i, &txn) {
                    let _ = socket.send_to(&datagram, peer).await;
                }
            }
        });
        (
            Endpoint::new(addr.ip().to_string(), addr.port(), family),
            seen,
        )
    }

    async fn mock(reply: Reply) -> Endpoint {
        mock_on("127.0.0.1:0", Family::V4, reply).await.0
    }

    fn answer(mapped: &'static str) -> Reply {
        Box::new(move |_: usize, txn: &TransactionId| {
            vec![binding_response(txn, mapped.parse().unwrap())]
        })
    }

    fn sock(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    const TXN: TransactionId = *b"0123456789ab";
    const T: Duration = Duration::from_millis(500);

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn detects_v4_and_v6_from_xor_mapped_address() {
        if UdpSocket::bind("[::1]:0").await.is_err() {
            return;
        }
        let a = mock(answer("203.0.113.7:54321")).await;
        let (b, _) = mock_on("[::1]:0", Family::V6, answer("[2001:db8::1]:40000")).await;
        let d = detect(&[a, b], T, 1).await;
        assert_eq!(d.v4, Some("203.0.113.7".parse().unwrap()));
        assert_eq!(d.v6, Some("2001:db8::1".parse().unwrap()));
        assert_eq!(d.preferred(), Some("203.0.113.7".parse().unwrap()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn xor_mapped_address_round_trips_and_is_xored() {
        for addr in ["203.0.113.7:54321", "[2001:db8::1]:40000"] {
            let packet = binding_response(&TXN, sock(addr));
            assert_eq!(parse_binding_response(&packet, &TXN), Ok(sock(addr)));
            let value = &packet[HEADER_LEN + 4..];
            assert_ne!(decode_address(value, None).unwrap(), sock(addr));
        }
        let packet = binding_response(&TXN, sock("192.0.2.1:32853"));
        assert_eq!(&packet[HEADER_LEN + 6..HEADER_LEN + 8], &[0xA1, 0x47]);
        assert_eq!(&packet[HEADER_LEN + 8..], &[0xE1, 0x12, 0xA6, 0x43]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn mapped_address_is_a_fallback_and_xor_wins() {
        let plain = encode_message(
            BINDING_SUCCESS,
            &TXN,
            &[(
                MAPPED_ADDRESS,
                encode_address(sock("198.51.100.4:1000"), None),
            )],
        );
        assert_eq!(
            parse_binding_response(&plain, &TXN),
            Ok(sock("198.51.100.4:1000"))
        );
        let both = encode_message(
            BINDING_SUCCESS,
            &TXN,
            &[
                (
                    MAPPED_ADDRESS,
                    encode_address(sock("198.51.100.4:1000"), None),
                ),
                (0x8022, b"tuff".to_vec()),
                (
                    XOR_MAPPED_ADDRESS,
                    encode_address(sock("[2001:db8::2]:2000"), Some(&TXN)),
                ),
            ],
        );
        assert_eq!(
            parse_binding_response(&both, &TXN),
            Ok(sock("[2001:db8::2]:2000"))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn malformed_responses_are_rejected() {
        let good = binding_response(&TXN, sock("203.0.113.7:1"));
        let with = |f: &dyn Fn(&mut Vec<u8>)| {
            let mut p = good.clone();
            f(&mut p);
            p
        };
        let attr =
            |kind: u16, value: Vec<u8>| encode_message(BINDING_SUCCESS, &TXN, &[(kind, value)]);
        let cases: Vec<(&str, Vec<u8>)> = vec![
            ("empty", Vec::new()),
            ("short header", good[..12].to_vec()),
            ("truncated datagram", good[..good.len() - 4].to_vec()),
            ("bad cookie", with(&|p| p[4] ^= 1)),
            ("top bits set", with(&|p| p[0] |= 0x80)),
            ("length mismatch", with(&|p| p[3] += 4)),
            (
                "length not aligned",
                with(&|p| {
                    p.extend_from_slice(&[0, 0]);
                    p[3] += 2;
                }),
            ),
            ("request type", with(&|p| p[0] = 0x00)),
            ("oversized", attr(0x8022, vec![0; MAX_DATAGRAM])),
            (
                "attribute overruns message",
                with(&|p| p[HEADER_LEN + 3] = 0xFF),
            ),
            ("no address", attr(0x8022, b"x".to_vec())),
            (
                "v4 wrong length",
                attr(XOR_MAPPED_ADDRESS, vec![0, FAMILY_V4, 0, 0, 1, 2]),
            ),
            (
                "v6 wrong length",
                attr(MAPPED_ADDRESS, vec![0, FAMILY_V6, 0, 1, 1, 1, 1, 1]),
            ),
            (
                "unknown family",
                attr(MAPPED_ADDRESS, vec![0, 3, 0, 1, 1, 2, 3, 4]),
            ),
            ("too short", attr(MAPPED_ADDRESS, vec![0, FAMILY_V4])),
            (
                "unspecified",
                attr(MAPPED_ADDRESS, encode_address(sock("0.0.0.0:5"), None)),
            ),
        ];
        for (name, packet) in cases {
            assert_eq!(
                parse_binding_response(&packet, &TXN),
                Err(Failure::Malformed),
                "{name}"
            );
        }
        let error = encode_message(BINDING_ERROR, &TXN, &[]);
        assert_eq!(
            parse_binding_response(&error, &TXN),
            Err(Failure::ErrorResponse)
        );
        assert_eq!(
            parse_binding_response(&good, b"ba9876543210"),
            Err(Failure::WrongTransaction)
        );
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn wrong_transaction_and_junk_are_ignored() {
        let e = mock(Box::new(|_: usize, txn: &TransactionId| {
            let mut wrong = *txn;
            wrong[0] ^= 0xFF;
            vec![
                binding_response(&wrong, sock("192.0.2.66:1")),
                vec![0xAB; 7],
                vec![0; MAX_DATAGRAM + 200],
                binding_response(txn, sock("203.0.113.9:1")),
            ]
        }))
        .await;
        assert_eq!(probe(&e, T, 1).await, Ok("203.0.113.9".parse().unwrap()));

        let short = Duration::from_millis(100);
        let e = mock(Box::new(|_: usize, txn: &TransactionId| {
            let mut wrong = *txn;
            wrong[11] ^= 1;
            vec![binding_response(&wrong, sock("192.0.2.66:1"))]
        }))
        .await;
        assert_eq!(probe(&e, short, 1).await, Err(Failure::WrongTransaction));

        let e = mock(Box::new(|_: usize, _: &TransactionId| {
            vec![vec![0; MAX_DATAGRAM + 200]]
        }))
        .await;
        assert_eq!(probe(&e, short, 1).await, Err(Failure::Malformed));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn silent_server_times_out_after_retransmits() {
        let silent: Reply = Box::new(|_: usize, _: &TransactionId| Vec::new());
        let (e, seen) = mock_on("127.0.0.1:0", Family::V4, silent).await;
        let start = std::time::Instant::now();
        assert_eq!(
            probe(&e, Duration::from_millis(100), 3).await,
            Err(Failure::Timeout)
        );
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(300), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        assert_eq!(seen.load(Ordering::SeqCst), 3);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn retransmit_recovers_from_lost_request() {
        let e = mock(Box::new(|i: usize, txn: &TransactionId| {
            if i == 0 {
                Vec::new()
            } else {
                vec![binding_response(txn, sock("203.0.113.10:1"))]
            }
        }))
        .await;
        assert_eq!(
            probe(&e, Duration::from_millis(100), 3).await,
            Ok("203.0.113.10".parse().unwrap())
        );
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn family_filter_and_failure_summary() {
        let mut e = mock(answer("203.0.113.7:1")).await;
        e.family = Family::V6;
        let report = detect_report(std::slice::from_ref(&e), T, 1).await;
        assert_eq!(report.detected, Detected::default());
        assert_eq!(report.failures, vec![(e.clone(), Failure::NoAddress)]);
        assert_eq!(
            report.summary(),
            format!("127.0.0.1:{}/v6 no address of this family", e.port)
        );
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn explicit_url_wins_without_probing() {
        let (e, seen) = mock_on("127.0.0.1:0", Family::V4, answer("203.0.113.7:1")).await;
        let mut cfg = PublicIpConfig::new(true, Some("https://packs.example.com/".into()));
        cfg.endpoints = vec![e];
        let b = spawn(cfg, "0.0.0.0:8080".parse().unwrap());
        b.watch()
            .wait_for(|u| u == "https://packs.example.com")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(b.current(), "https://packs.example.com");
        assert_eq!(seen.load(Ordering::SeqCst), 0);
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn detection_updates_watch_and_falls_back() {
        let e = mock(answer("[2001:db8::9]:1")).await;
        let mut cfg = PublicIpConfig::new(true, None);
        cfg.endpoints = vec![e];
        cfg.timeout = T;
        let b = spawn(cfg, "0.0.0.0:8080".parse().unwrap());
        let mut w = b.watch();
        w.wait_for(|u| u == "http://[2001:db8::9]:8080")
            .await
            .unwrap();

        let mut cfg = PublicIpConfig::new(true, None);
        cfg.endpoints = vec![
            mock(Box::new(|_: usize, _: &TransactionId| {
                vec![b"junk".to_vec()]
            }))
            .await,
        ];
        cfg.timeout = Duration::from_millis(50);
        cfg.attempts = 2;
        let b = spawn(cfg, "[::]:9000".parse().unwrap());
        b.watch()
            .wait_for(|u| u == "http://[::1]:9000")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(b.current(), "http://[::1]:9000");
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn disabled_never_probes() {
        let cfg = PublicIpConfig::new(false, None);
        let b = spawn(cfg, "192.0.2.1:25580".parse().unwrap());
        b.watch()
            .wait_for(|u| u == "http://192.0.2.1:25580")
            .await
            .unwrap();
        assert_eq!(b.current(), "http://192.0.2.1:25580");
    }
}
