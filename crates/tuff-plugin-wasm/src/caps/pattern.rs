//! Capability entries as written in a manifest request or an owner
//! grant, and the host, port and path patterns inside them.
//!
//! ```text
//! http      [http://|https://]host[:ports][/path-prefix] [private]
//! websocket [ws://|wss://]host[:ports][/path-prefix]     [private]
//! tcp, udp  host:ports                                   [private]
//! route     /path-prefix
//! listen-tcp, listen-udp  port [bind=<ip>]
//! files     <size>
//! ```
//!
//! A host is `*`, `*.suffix`, a DNS name, an IPv4 address, or a
//! bracketed IPv6 address or CIDR range (`[::1]`, `[10.0.0.0/8]`). Ports
//! are `*`, one port, or `a-b`. A bare http host means https only and a
//! bare websocket host wss only.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr};

use super::address;
use super::size;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Files,
    Http,
    WebSocket,
    Tcp,
    Udp,
    Route,
    ListenTcp,
    ListenUdp,
    Dimension,
    Content,
    WorldWrite,
    PlayerWrite,
    PlayerRead,
    CrossPlugin,
    Panel,
}

impl Kind {
    pub const ALL: [Kind; 15] = [
        Kind::Files,
        Kind::Http,
        Kind::WebSocket,
        Kind::Tcp,
        Kind::Udp,
        Kind::Route,
        Kind::ListenTcp,
        Kind::ListenUdp,
        Kind::Dimension,
        Kind::Content,
        Kind::WorldWrite,
        Kind::PlayerWrite,
        Kind::PlayerRead,
        Kind::CrossPlugin,
        Kind::Panel,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Files => "files",
            Kind::Http => "http",
            Kind::WebSocket => "websocket",
            Kind::Tcp => "tcp",
            Kind::Udp => "udp",
            Kind::Route => "route",
            Kind::ListenTcp => "listen-tcp",
            Kind::ListenUdp => "listen-udp",
            Kind::Dimension => "dimension",
            Kind::Content => "content",
            Kind::WorldWrite => "world-write",
            Kind::PlayerWrite => "player-write",
            Kind::PlayerRead => "player-read",
            Kind::CrossPlugin => "cross-plugin",
            Kind::Panel => "panel",
        }
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scheme {
    Http,
    Https,
    Ws,
    Wss,
    /// A raw TCP or UDP socket.
    Raw,
}

impl Scheme {
    pub fn name(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
            Scheme::Ws => "ws",
            Scheme::Wss => "wss",
            Scheme::Raw => "",
        }
    }

    pub fn default_port(self) -> Option<u16> {
        match self {
            Scheme::Http | Scheme::Ws => Some(80),
            Scheme::Https | Scheme::Wss => Some(443),
            Scheme::Raw => None,
        }
    }

    pub fn parse(text: &str) -> Option<Scheme> {
        Some(match text.to_ascii_lowercase().as_str() {
            "http" => Scheme::Http,
            "https" => Scheme::Https,
            "ws" => Scheme::Ws,
            "wss" => Scheme::Wss,
            _ => return None,
        })
    }

    fn fits(self, kind: Kind) -> bool {
        match kind {
            Kind::Http => matches!(self, Scheme::Http | Scheme::Https),
            Kind::WebSocket => matches!(self, Scheme::Ws | Scheme::Wss),
            _ => false,
        }
    }
}

/// A destination host as a target names it: never a pattern.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TargetHost {
    Name(String),
    Ip(IpAddr),
}

impl fmt::Display for TargetHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetHost::Name(name) => f.write_str(name),
            TargetHost::Ip(IpAddr::V4(ip)) => write!(f, "{ip}"),
            TargetHost::Ip(IpAddr::V6(ip)) => write!(f, "[{ip}]"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Host {
    Any,
    /// Every name strictly below this one.
    Suffix(String),
    Name(String),
    Ip(IpAddr),
    Cidr(IpAddr, u8),
}

impl Host {
    pub fn matches(&self, host: &TargetHost) -> bool {
        match (self, host) {
            (Host::Any, _) => true,
            (Host::Suffix(suffix), TargetHost::Name(name)) => below(name, suffix),
            (Host::Name(pattern), TargetHost::Name(name)) => pattern == name,
            (Host::Ip(pattern), TargetHost::Ip(ip)) => {
                address::canonical(*pattern) == address::canonical(*ip)
            }
            (Host::Cidr(net, len), TargetHost::Ip(ip)) => {
                address::in_network(address::canonical(*ip), *net, *len)
            }
            _ => false,
        }
    }

    /// Whether this pattern holds every host `other` does.
    pub fn covers(&self, other: &Host) -> bool {
        match (self, other) {
            (Host::Any, _) => true,
            (Host::Suffix(a), Host::Suffix(b)) => a == b || below(b, a),
            (Host::Suffix(a), Host::Name(b)) => below(b, a),
            (Host::Name(a), Host::Name(b)) => a == b,
            (Host::Ip(a), Host::Ip(b)) => address::canonical(*a) == address::canonical(*b),
            (Host::Cidr(net, len), Host::Ip(ip)) => {
                address::in_network(address::canonical(*ip), *net, *len)
            }
            (Host::Cidr(net, len), Host::Cidr(other, other_len)) => {
                other_len >= len && address::in_network(*other, *net, *len)
            }
            _ => false,
        }
    }

    /// Whether `ip` is named literally (exactly, or inside a range).
    pub fn names_ip(&self, ip: IpAddr, exact_only: bool) -> bool {
        let ip = address::canonical(ip);
        match self {
            Host::Ip(pattern) => address::canonical(*pattern) == ip,
            Host::Cidr(net, len) => !exact_only && address::in_network(ip, *net, *len),
            _ => false,
        }
    }
}

fn below(name: &str, suffix: &str) -> bool {
    name.len() > suffix.len()
        && name.ends_with(suffix)
        && name.as_bytes()[name.len() - suffix.len() - 1] == b'.'
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Host::Any => f.write_str("*"),
            Host::Suffix(suffix) => write!(f, "*.{suffix}"),
            Host::Name(name) => f.write_str(name),
            Host::Ip(IpAddr::V4(ip)) => write!(f, "{ip}"),
            Host::Ip(IpAddr::V6(ip)) => write!(f, "[{ip}]"),
            Host::Cidr(net, len) => write!(f, "[{net}/{len}]"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ports {
    Any,
    Range(u16, u16),
}

impl Ports {
    pub fn matches(self, port: u16) -> bool {
        match self {
            Ports::Any => true,
            Ports::Range(low, high) => (low..=high).contains(&port),
        }
    }

    pub fn covers(self, other: Ports) -> bool {
        match (self, other) {
            (Ports::Any, _) => true,
            (Ports::Range(..), Ports::Any) => false,
            (Ports::Range(low, high), Ports::Range(a, b)) => low <= a && b <= high,
        }
    }

    pub fn parse(text: &str) -> Result<Ports, String> {
        if text == "*" {
            return Ok(Ports::Any);
        }
        let (low, high) = text.split_once('-').unwrap_or((text, text));
        let (low, high) = (port(low)?, port(high)?);
        if low > high {
            return Err(format!("port range {text:?} is backwards"));
        }
        Ok(Ports::Range(low, high))
    }
}

impl fmt::Display for Ports {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ports::Any => f.write_str("*"),
            Ports::Range(low, high) if low == high => write!(f, "{low}"),
            Ports::Range(low, high) => write!(f, "{low}-{high}"),
        }
    }
}

/// A port number: 1 to 5 decimal digits, at most 65535.
pub fn port(text: &str) -> Result<u16, String> {
    if text.is_empty() || text.len() > 5 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("{text:?} is not a port"));
    }
    text.parse().map_err(|_| format!("{text:?} is not a port"))
}

const MAX_NAME: usize = 253;
const MAX_LABEL: usize = 63;

/// A DNS name lowercased with any trailing dot removed, or an error. Only
/// ASCII is accepted (internationalized names must be given in punycode).
/// A name whose last label looks numeric must be a dotted-quad IPv4
/// address, so `127.1` or `0x7f.1` can never sneak a loopback address
/// past the host patterns as a "name".
pub fn target_host(text: &str) -> Result<TargetHost, String> {
    if let Some(inner) = text.strip_prefix('[') {
        let inner = inner
            .strip_suffix(']')
            .ok_or_else(|| format!("{text:?} has an unclosed bracket"))?;
        return inner
            .parse::<std::net::Ipv6Addr>()
            .map(|ip| TargetHost::Ip(IpAddr::V6(ip)))
            .map_err(|_| format!("{text:?} is not an IPv6 address"));
    }
    let name = text.strip_suffix('.').unwrap_or(text).to_ascii_lowercase();
    if name.is_empty() || name.len() > MAX_NAME {
        return Err(format!("{text:?} is not a host name"));
    }
    let last = name.rsplit('.').next().unwrap_or_default();
    if last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x") {
        return strict_ipv4(&name)
            .map(|ip| TargetHost::Ip(IpAddr::V4(ip)))
            .ok_or_else(|| format!("{text:?} is neither a host name nor a dotted IPv4 address"));
    }
    for label in name.split('.') {
        let bytes = label.as_bytes();
        if label.is_empty()
            || label.len() > MAX_LABEL
            || bytes[0] == b'-'
            || bytes[bytes.len() - 1] == b'-'
            || !bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        {
            return Err(format!("{text:?} is not a host name"));
        }
    }
    Ok(TargetHost::Name(name))
}

fn strict_ipv4(text: &str) -> Option<Ipv4Addr> {
    let mut octets = [0u8; 4];
    let mut parts = text.split('.');
    for octet in &mut octets {
        let part = parts.next()?;
        if part.is_empty()
            || part.len() > 3
            || (part.len() > 1 && part.starts_with('0'))
            || !part.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        *octet = part.parse().ok()?;
    }
    parts.next().is_none().then_some(Ipv4Addr::from(octets))
}

pub fn host_pattern(text: &str) -> Result<Host, String> {
    if text == "*" {
        return Ok(Host::Any);
    }
    if let Some(suffix) = text.strip_prefix("*.") {
        return match target_host(suffix)? {
            TargetHost::Name(name) => Ok(Host::Suffix(name)),
            TargetHost::Ip(_) => Err(format!("{text:?} puts a wildcard on an address")),
        };
    }
    if let Some((net, len)) = text
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .and_then(|inner| inner.split_once('/'))
    {
        let net: IpAddr = net
            .parse()
            .map_err(|_| format!("{text:?} is not an address range"))?;
        let max = if net.is_ipv4() { 32 } else { 128 };
        let len = len
            .parse::<u8>()
            .ok()
            .filter(|len| *len <= max && !len.to_string().starts_with('+'))
            .ok_or_else(|| format!("{text:?} has a bad prefix length"))?;
        return Ok(Host::Cidr(net, len));
    }
    if text.contains('*') {
        return Err(format!("{text:?}: a wildcard may only be * or lead as *."));
    }
    Ok(match target_host(text)? {
        TargetHost::Name(name) => Host::Name(name),
        TargetHost::Ip(ip) => Host::Ip(ip),
    })
}

/// Whether `path` (with any query) is a clean absolute path: no `.` or
/// `..` segments, no backslashes, no control or non-ASCII bytes, and no
/// escapes that decode to a dot, slash, backslash or control byte, so a
/// prefix check on it cannot be walked out of by a later decoder.
pub fn check_path(path: &str) -> Result<(), String> {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    if !path.starts_with('/') {
        return Err(format!("path {path:?} must start with /"));
    }
    for part in [path, query] {
        let bytes = part.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if !(0x21..0x7f).contains(&b) || b == b'\\' || b == b'#' {
                return Err(format!("path {path:?} has a forbidden character"));
            }
            if b == b'%' {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .and_then(|hex| std::str::from_utf8(hex).ok())
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                    .ok_or_else(|| format!("path {path:?} has a bad escape"))?;
                if hex < 0x20 || hex == 0x7f || (part == path && matches!(hex, b'.' | b'/' | b'\\'))
                {
                    return Err(format!("path {path:?} escapes a forbidden character"));
                }
                i += 3;
                continue;
            }
            i += 1;
        }
    }
    if path
        .split('/')
        .any(|segment| segment == "." || segment == "..")
    {
        return Err(format!("path {path:?} has a dot segment"));
    }
    Ok(())
}

/// Whether `path` (without its query) starts with `prefix` on a segment
/// boundary.
pub fn path_under(path: &str, prefix: &str) -> bool {
    let path = path.split_once('?').map_or(path, |(path, _)| path);
    if prefix == "/" {
        return true;
    }
    match path.strip_prefix(prefix) {
        Some(rest) => prefix.ends_with('/') || rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

/// An outbound network rule: who may be reached, on which ports, under
/// which path, and whether non-public addresses are allowed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NetRule {
    pub scheme: Scheme,
    pub host: Host,
    pub ports: Ports,
    pub path: String,
    pub private: bool,
}

impl NetRule {
    pub fn covers(&self, other: &NetRule) -> bool {
        self.scheme == other.scheme
            && self.host.covers(&other.host)
            && self.ports.covers(other.ports)
            && path_under(&other.path, &self.path)
    }
}

impl fmt::Display for NetRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.scheme != Scheme::Raw {
            write!(f, "{}://", self.scheme.name())?;
        }
        write!(f, "{}", self.host)?;
        let default = self.scheme.default_port().map(|p| Ports::Range(p, p));
        if Some(self.ports) != default {
            write!(f, ":{}", self.ports)?;
        }
        if self.path != "/" {
            f.write_str(&self.path)?;
        }
        if self.private {
            f.write_str(" private")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ListenRule {
    pub port: u16,
    pub bind: Option<IpAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Rule {
    /// A disk quota in bytes.
    Files(u64),
    Net(NetRule),
    /// A path prefix below the plugin's own `/plugins/<id>` space.
    Route(String),
    /// A path prefix on the server's HTTP root, outside `/plugins/<id>`.
    TopRoute(String),
    Listen(ListenRule),
    /// A dimension id `namespace:path`, or `namespace:*` for every one.
    Dimension(String),
    /// A namespace the plugin may add custom content under.
    Content(String),
    /// A whole host-call group (world-write, player-write, ...).
    Group,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Entry {
    pub kind: Kind,
    pub rule: Rule,
}

impl Entry {
    pub fn covers(&self, other: &Entry) -> bool {
        if self.kind != other.kind {
            return false;
        }
        match (&self.rule, &other.rule) {
            (Rule::Files(_), Rule::Files(_)) => true,
            (Rule::Net(a), Rule::Net(b)) => a.covers(b),
            (Rule::Route(a), Rule::Route(b)) | (Rule::TopRoute(a), Rule::TopRoute(b)) => {
                path_under(b, a)
            }
            (Rule::Listen(a), Rule::Listen(b)) => {
                a.port == b.port && (a.bind.is_none() || a.bind == b.bind)
            }
            (Rule::Dimension(a), Rule::Dimension(b)) => {
                a == b
                    || a.strip_suffix('*')
                        .is_some_and(|prefix| b.starts_with(prefix))
            }
            (Rule::Content(a), Rule::Content(b)) => a == b,
            (Rule::Group, Rule::Group) => true,
            _ => false,
        }
    }

    /// Parses every comma-separated entry of `kind` in `value`.
    pub fn parse_list(kind: Kind, value: &str) -> Result<Vec<Entry>, String> {
        value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(|item| Entry::parse(kind, item))
            .collect()
    }

    pub fn parse(kind: Kind, text: &str) -> Result<Entry, String> {
        let mut words = text.split_whitespace();
        let pattern = words.next().ok_or("an empty entry")?;
        let flags: Vec<&str> = words.collect();
        let rule = match kind {
            Kind::Files => {
                let text = std::iter::once(pattern)
                    .chain(flags.iter().copied())
                    .collect::<Vec<_>>()
                    .join(" ");
                return Ok(Entry {
                    kind,
                    rule: Rule::Files(size::bytes(&text)?),
                });
            }
            Kind::Http | Kind::WebSocket | Kind::Tcp | Kind::Udp => {
                let private = match flags.as_slice() {
                    [] => false,
                    ["private"] => true,
                    _ => return Err(format!("{text:?}: the only flag is private")),
                };
                Rule::Net(net_rule(kind, pattern, private)?)
            }
            Kind::Route => {
                let top = match flags.as_slice() {
                    [] => false,
                    ["top"] => true,
                    _ => return Err(format!("{text:?}: the only flag is top")),
                };
                check_path(pattern)?;
                if pattern.contains('?') {
                    return Err(format!("route {pattern:?} has a query"));
                }
                match top {
                    true => Rule::TopRoute(pattern.to_owned()),
                    false => Rule::Route(pattern.to_owned()),
                }
            }
            Kind::Dimension => {
                if !flags.is_empty() {
                    return Err(format!("{text:?}: dimension takes one id"));
                }
                Rule::Dimension(dimension_id(pattern)?)
            }
            Kind::Content => {
                if !flags.is_empty() {
                    return Err(format!("{text:?}: content takes one namespace"));
                }
                Rule::Content(content_namespace(pattern)?)
            }
            Kind::WorldWrite
            | Kind::PlayerWrite
            | Kind::PlayerRead
            | Kind::CrossPlugin
            | Kind::Panel => {
                if pattern != "all" || !flags.is_empty() {
                    return Err(format!("{text:?}: {} takes only `all`", kind.name()));
                }
                Rule::Group
            }
            Kind::ListenTcp | Kind::ListenUdp => {
                let bind = match flags.as_slice() {
                    [] => None,
                    [flag] => Some(
                        flag.strip_prefix("bind=")
                            .map(|ip| ip.trim_start_matches('[').trim_end_matches(']'))
                            .and_then(|ip| ip.parse::<IpAddr>().ok())
                            .ok_or_else(|| format!("{text:?}: expected bind=<address>"))?,
                    ),
                    _ => return Err(format!("{text:?}: the only flag is bind=<address>")),
                };
                Rule::Listen(ListenRule {
                    port: port(pattern)?,
                    bind,
                })
            }
        };
        Ok(Entry { kind, rule })
    }
}

fn content_namespace(text: &str) -> Result<String, String> {
    let valid = !text.is_empty()
        && text
            .chars()
            .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-' | '.'));
    match valid {
        true => Ok(text.to_owned()),
        false => Err(format!("content namespace {text:?} is not valid")),
    }
}

fn dimension_id(text: &str) -> Result<String, String> {
    let (namespace, path) = text
        .split_once(':')
        .ok_or_else(|| format!("dimension {text:?} needs the form namespace:path"))?;
    let name_ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '-' | '.' | '/'))
    };
    let path_ok = name_ok(path) || path == "*" || path.strip_suffix('*').is_some_and(name_ok);
    match name_ok(namespace) && path_ok {
        true => Ok(text.to_owned()),
        false => Err(format!("dimension {text:?} is not a valid id")),
    }
}

fn net_rule(kind: Kind, text: &str, private: bool) -> Result<NetRule, String> {
    let (scheme, rest) = match text.split_once("://") {
        Some((scheme, rest)) => (
            Scheme::parse(scheme)
                .filter(|scheme| scheme.fits(kind))
                .ok_or_else(|| {
                    format!(
                        "{text:?}: scheme {scheme:?} is not one {} uses",
                        kind.name()
                    )
                })?,
            rest,
        ),
        None => (
            match kind {
                Kind::Http => Scheme::Https,
                Kind::WebSocket => Scheme::Wss,
                _ => Scheme::Raw,
            },
            text,
        ),
    };
    let from = match rest.starts_with('[') {
        true => rest
            .find(']')
            .ok_or_else(|| format!("{text:?} has an unclosed bracket"))?,
        false => 0,
    };
    let (authority, path) = match rest[from..].find('/') {
        Some(at) => rest.split_at(from + at),
        None => (rest, ""),
    };
    if scheme == Scheme::Raw && !path.is_empty() {
        return Err(format!("{text:?}: {} entries have no path", kind.name()));
    }
    let (host, ports) = split_port(authority)?;
    let ports = match (ports, scheme.default_port()) {
        (Some(ports), _) => Ports::parse(ports)?,
        (None, Some(default)) => Ports::Range(default, default),
        (None, None) => return Err(format!("{text:?} needs a port (host:port or host:*)")),
    };
    let path = if path.is_empty() { "/" } else { path };
    check_path(path)?;
    if path.contains('?') {
        return Err(format!("{text:?}: a path prefix cannot hold a query"));
    }
    Ok(NetRule {
        scheme,
        host: host_pattern(host)?,
        ports,
        path: path.to_owned(),
        private,
    })
}

/// Splits `host[:port]`, where an IPv6 host or range is bracketed.
pub fn split_port(authority: &str) -> Result<(&str, Option<&str>), String> {
    if authority.starts_with('[') {
        let close = authority
            .find(']')
            .ok_or_else(|| format!("{authority:?} has an unclosed bracket"))?;
        let (host, rest) = authority.split_at(close + 1);
        return match rest {
            "" => Ok((host, None)),
            _ => rest
                .strip_prefix(':')
                .map(|port| (host, Some(port)))
                .ok_or_else(|| format!("{authority:?} has junk after the address")),
        };
    }
    Ok(match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: Kind, text: &str) -> Entry {
        Entry::parse(kind, text).unwrap_or_else(|err| panic!("{text}: {err}"))
    }

    fn net(kind: Kind, text: &str) -> NetRule {
        match entry(kind, text).rule {
            Rule::Net(rule) => rule,
            other => panic!("{other:?}"),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn http_entries_default_to_https_on_443() {
        let rule = net(Kind::Http, "Discord.com/api/webhooks/");
        assert_eq!(rule.scheme, Scheme::Https);
        assert_eq!(rule.host, Host::Name("discord.com".into()));
        assert_eq!(rule.ports, Ports::Range(443, 443));
        assert_eq!(rule.path, "/api/webhooks/");
        assert_eq!(rule.to_string(), "https://discord.com/api/webhooks/");
        let plain = net(Kind::Http, "http://*.example.org:8000-8100 private");
        assert_eq!(plain.scheme, Scheme::Http);
        assert_eq!(plain.host, Host::Suffix("example.org".into()));
        assert_eq!(plain.ports, Ports::Range(8000, 8100));
        assert!(plain.private);
        assert_eq!(
            net(Kind::WebSocket, "gateway.discord.gg").scheme,
            Scheme::Wss
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn socket_entries_need_ports_and_take_addresses() {
        let rule = net(Kind::Tcp, "[::1]:5432");
        assert_eq!(rule.host, Host::Ip("::1".parse().unwrap()));
        let rule = net(Kind::Udp, "[10.0.0.0/8]:*");
        assert_eq!(rule.host, Host::Cidr("10.0.0.0".parse().unwrap(), 8));
        assert_eq!(rule.ports, Ports::Any);
        let rule = net(Kind::Tcp, "[fd00::/8]:6379");
        assert_eq!(rule.host, Host::Cidr("fd00::".parse().unwrap(), 8));
        for bad in [
            "db.lan",
            "db.lan/x:5",
            "[::1]5432",
            "host:70000",
            "host:9-2",
            "a*b.com:1",
        ] {
            assert!(Entry::parse(Kind::Tcp, bad).is_err(), "{bad}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn numeric_looking_hosts_must_be_strict_ipv4() {
        assert_eq!(
            target_host("127.0.0.1").unwrap(),
            TargetHost::Ip("127.0.0.1".parse().unwrap())
        );
        for sneaky in [
            "127.1",
            "2130706433",
            "0x7f.0.0.1",
            "0177.0.0.1",
            "127.0.0.01",
            "1.2.3.4.5",
            "a.0x10",
        ] {
            assert!(target_host(sneaky).is_err(), "{sneaky}");
        }
        assert!(target_host("xn--bcher-kva.example").is_ok());
        assert!(target_host("bücher.example").is_err());
        assert!(target_host("-bad.example").is_err());
        assert!(target_host("under_score.example").is_err());
        assert_eq!(
            target_host("Example.COM.").unwrap(),
            TargetHost::Name("example.com".into())
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn paths_reject_traversal_and_encoded_separators() {
        for good in ["/", "/api/webhooks/1", "/a%20b", "/x?y=%2F..", "/a.b/c..d"] {
            assert!(check_path(good).is_ok(), "{good}");
        }
        for bad in [
            "api",
            "/a/../b",
            "/a/./b",
            "/..",
            "/a/%2e%2e/b",
            "/a%2Fb",
            "/a%5cb",
            "/a\\b",
            "/a%00",
            "/a b",
            "/a#b",
            "/a%zz",
            "/a%2",
        ] {
            assert!(check_path(bad).is_err(), "{bad}");
        }
        assert!(path_under("/api/webhooks/1", "/api/webhooks/"));
        assert!(path_under("/api/webhooks", "/api/webhooks"));
        assert!(path_under("/api/webhooks/1?x", "/api/webhooks"));
        assert!(!path_under("/api/webhooksX", "/api/webhooks"));
        assert!(!path_under("/api", "/api/webhooks"));
        assert!(path_under("/anything", "/"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn coverage_follows_host_port_and_path_containment() {
        let wide = entry(Kind::Http, "*.discord.com");
        assert!(wide.covers(&entry(Kind::Http, "gateway.discord.com/x")));
        assert!(!wide.covers(&entry(Kind::Http, "discord.com")));
        assert!(!wide.covers(&entry(Kind::Http, "http://gateway.discord.com")));
        assert!(!wide.covers(&entry(Kind::WebSocket, "gateway.discord.com")));
        assert!(entry(Kind::Tcp, "[10.0.0.0/8]:*").covers(&entry(Kind::Tcp, "10.1.1.1:6379")));
        assert!(entry(Kind::Tcp, "[10.0.0.0/8]:*").covers(&entry(Kind::Tcp, "[10.2.0.0/16]:1-9")));
        assert!(!entry(Kind::Tcp, "[10.0.0.0/16]:*").covers(&entry(Kind::Tcp, "[10.0.0.0/8]:1")));
        assert!(entry(Kind::Route, "/").covers(&entry(Kind::Route, "/map")));
        assert!(!entry(Kind::Route, "/map").covers(&entry(Kind::Route, "/")));
        assert!(entry(Kind::ListenUdp, "19132").covers(&entry(Kind::ListenUdp, "19132 bind=::")));
        assert!(
            !entry(Kind::ListenUdp, "19132 bind=127.0.0.1")
                .covers(&entry(Kind::ListenUdp, "19132"))
        );
        assert_eq!(entry(Kind::Files, "64 MiB").rule, Rule::Files(64 << 20));
    }
}
