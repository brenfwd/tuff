//! One plugin's effective permissions: what its manifest requests,
//! intersected with what the owner grants. Every capability asks this
//! before doing anything, and again (through [`Approval::admit`]) with
//! the addresses a name resolved to, which are the only addresses it may
//! then connect to.

use std::fmt;
use std::net::{IpAddr, SocketAddr};

use super::address::{self, Class};
use super::grants::Grant;
use super::pattern::{self, Entry, Kind, NetRule, Rule};
use super::quota::Limits;
use super::target::Target;

/// The most capability requests one manifest may make.
pub const MAX_REQUESTS: usize = 64;
/// The most resolved addresses considered for one destination.
pub const MAX_ADDRESSES: usize = 32;

/// One entry a manifest requests (`cap.<kind> = entries [-- reason]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub entry: Entry,
    pub reason: String,
}

impl Request {
    /// Parses manifest line `key = value`; `None` when `key` is not a
    /// capability key.
    pub fn parse_line(key: &str, value: &str) -> Option<Result<Vec<Request>, String>> {
        let kind = key.strip_prefix("cap.")?;
        let Some(kind) = Kind::from_name(kind) else {
            return Some(Err(format!("{key:?} is not a capability")));
        };
        let (entries, reason) = match value.split_once(" -- ") {
            Some((entries, reason)) => (entries, reason.trim()),
            None => (value, ""),
        };
        Some(Entry::parse_list(kind, entries).map(|entries| {
            entries
                .into_iter()
                .map(|entry| Request {
                    entry,
                    reason: reason.to_owned(),
                })
                .collect()
        }))
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ", self.kind.name())?;
        match &self.rule {
            Rule::Files(bytes) if bytes % (1 << 20) == 0 => write!(f, "{} MiB", bytes >> 20),
            Rule::Files(bytes) => write!(f, "{bytes} B"),
            Rule::Net(rule) => write!(f, "{rule}"),
            Rule::Route(path) => f.write_str(path),
            Rule::TopRoute(path) => write!(f, "{path} top"),
            Rule::Dimension(id) | Rule::Content(id) => f.write_str(id),
            Rule::Group => f.write_str("all"),
            Rule::Listen(rule) => match rule.bind {
                Some(bind) => write!(f, "{} bind={bind}", rule.port),
                None => write!(f, "{}", rule.port),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Denied {
    Malformed(String),
    NotRequested(Kind, String),
    NotGranted(Kind, String),
    Address {
        target: String,
        ip: IpAddr,
        class: Class,
    },
    Unresolved(String, String),
    Reserved(u16),
}

impl fmt::Display for Denied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Denied::Malformed(why) => f.write_str(why),
            Denied::NotRequested(kind, target) => write!(
                f,
                "the plugin's manifest does not request {} {target}",
                kind.name()
            ),
            Denied::NotGranted(kind, target) => write!(
                f,
                "the server owner has not granted {} {target}",
                kind.name()
            ),
            Denied::Address { target, ip, class } => write!(
                f,
                "{target} resolves to {ip}, a {} address the owner has not granted",
                class.describe()
            ),
            Denied::Unresolved(target, why) => write!(f, "cannot resolve {target}: {why}"),
            Denied::Reserved(port) => write!(f, "port {port} belongs to the server"),
        }
    }
}

impl std::error::Error for Denied {}

/// A destination that passed the name-level checks, with the grant rules
/// that matched it. Connect only to addresses [`Approval::admit`] returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    pub target: Target,
    rules: Vec<NetRule>,
}

impl Approval {
    /// The addresses of `resolved` this plugin may connect to, with the
    /// target's port. Non-public addresses need a grant marked `private`
    /// or one naming them; metadata services need a grant naming exactly
    /// that address; unspecified, broadcast and multicast never pass.
    pub fn admit(
        &self,
        resolved: impl IntoIterator<Item = SocketAddr>,
    ) -> Result<Vec<SocketAddr>, Denied> {
        let mut admitted: Vec<SocketAddr> = Vec::new();
        let mut first_refused = None;
        for addr in resolved.into_iter().take(MAX_ADDRESSES) {
            let ip = address::canonical(addr.ip());
            let class = address::classify(ip);
            let allowed = match class {
                Class::Public => true,
                Class::Private(_) => self
                    .rules
                    .iter()
                    .any(|rule| rule.private || rule.host.names_ip(ip, false)),
                Class::Metadata => self.rules.iter().any(|rule| rule.host.names_ip(ip, true)),
                Class::Forbidden(_) => false,
            };
            let addr = SocketAddr::new(ip, self.target.port);
            if allowed {
                if !admitted.contains(&addr) {
                    admitted.push(addr);
                }
            } else {
                first_refused.get_or_insert((ip, class));
            }
        }
        if !admitted.is_empty() {
            return Ok(admitted);
        }
        Err(match first_refused {
            Some((ip, class)) => Denied::Address {
                target: self.target.to_string(),
                ip,
                class,
            },
            None => Denied::Unresolved(self.target.to_string(), "no addresses".into()),
        })
    }
}

/// What a plugin may do: its requests intersected with its grant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    requests: Vec<Request>,
    granted: Vec<Entry>,
    explicit: Vec<Entry>,
    limits: Limits,
}

impl Policy {
    /// The policy for a plugin requesting `requests` under `grant` (`None`
    /// when the owner granted it nothing).
    pub fn new(requests: Vec<Request>, grant: Option<&Grant>) -> Policy {
        let Some(grant) = grant else {
            return Policy {
                requests,
                ..Policy::default()
            };
        };
        Policy {
            requests,
            granted: grant.entries.clone(),
            explicit: grant.entries.clone(),
            limits: grant.limits,
        }
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    pub fn requests(&self) -> &[Request] {
        &self.requests
    }

    /// The plugin's disk quota, if it requested files and was granted
    /// them: the owner's explicit size wins over the requested one.
    pub fn files_quota(&self) -> Option<u64> {
        let requested = self.requests.iter().find_map(|r| match r.entry.rule {
            Rule::Files(bytes) => Some(bytes),
            _ => None,
        })?;
        let sizes = |entries: &[Entry]| {
            entries
                .iter()
                .filter_map(|entry| match entry.rule {
                    Rule::Files(bytes) => Some(bytes),
                    _ => None,
                })
                .max()
        };
        sizes(&self.explicit).or_else(|| sizes(&self.granted).map(|_| requested))
    }

    fn net_rules<'a>(
        entries: impl Iterator<Item = &'a Entry>,
        kind: Kind,
    ) -> impl Iterator<Item = &'a NetRule> {
        entries
            .filter(move |entry| entry.kind == kind)
            .filter_map(|entry| match &entry.rule {
                Rule::Net(rule) => Some(rule),
                _ => None,
            })
    }

    fn approve(&self, kind: Kind, target: Target) -> Result<Approval, Denied> {
        let fits = |rule: &&NetRule| {
            rule.scheme == target.scheme
                && rule.host.matches(&target.host)
                && rule.ports.matches(target.port)
                && pattern::path_under(&target.path, &rule.path)
        };
        let requested = self.requests.iter().map(|r| &r.entry);
        if !Self::net_rules(requested, kind).any(|rule| fits(&rule)) {
            return Err(Denied::NotRequested(kind, target.to_string()));
        }
        let rules: Vec<NetRule> = Self::net_rules(self.granted.iter(), kind)
            .filter(fits)
            .cloned()
            .collect();
        if rules.is_empty() {
            return Err(Denied::NotGranted(kind, target.to_string()));
        }
        Ok(Approval { target, rules })
    }

    /// Checks an HTTP or WebSocket URL before any DNS lookup.
    pub fn check_url(&self, kind: Kind, url: &str) -> Result<Approval, Denied> {
        let target = Target::url(kind, url).map_err(Denied::Malformed)?;
        self.approve(kind, target)
    }

    /// Checks a raw TCP or UDP destination before any DNS lookup.
    pub fn check_socket(&self, kind: Kind, host: &str, port: u16) -> Result<Approval, Denied> {
        if !matches!(kind, Kind::Tcp | Kind::Udp) {
            return Err(Denied::Malformed(format!(
                "{} is not a socket kind",
                kind.name()
            )));
        }
        let target = Target::socket(host, port).map_err(Denied::Malformed)?;
        self.approve(kind, target)
    }

    fn allowed(&self, kind: Kind, wanted: &Entry) -> Result<(), Denied> {
        let shown = wanted.to_string();
        let shown = shown
            .split_once(' ')
            .map_or(shown.as_str(), |(_, rest)| rest);
        if !self.requests.iter().any(|r| r.entry.covers(wanted)) {
            return Err(Denied::NotRequested(kind, shown.to_owned()));
        }
        if !self.granted.iter().any(|entry| entry.covers(wanted)) {
            return Err(Denied::NotGranted(kind, shown.to_owned()));
        }
        Ok(())
    }

    /// Checks a route path relative to the plugin's `/plugins/<id>`.
    pub fn check_route(&self, path: &str) -> Result<(), Denied> {
        pattern::check_path(path).map_err(Denied::Malformed)?;
        let path = path.split_once('?').map_or(path, |(path, _)| path);
        self.allowed(
            Kind::Route,
            &Entry {
                kind: Kind::Route,
                rule: Rule::Route(path.to_owned()),
            },
        )
    }

    /// The top-level route prefixes the plugin requested and the owner
    /// granted, in request order.
    pub fn top_routes(&self) -> Vec<String> {
        self.requests
            .iter()
            .filter_map(|request| match &request.entry.rule {
                Rule::TopRoute(path) if self.granted.iter().any(|g| g.covers(&request.entry)) => {
                    Some(path.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// Checks a route path on the server's HTTP root, outside the
    /// plugin's own `/plugins/<id>`.
    pub fn check_top_route(&self, path: &str) -> Result<(), Denied> {
        pattern::check_path(path).map_err(Denied::Malformed)?;
        let path = path.split_once('?').map_or(path, |(path, _)| path);
        self.allowed(
            Kind::Route,
            &Entry {
                kind: Kind::Route,
                rule: Rule::TopRoute(path.to_owned()),
            },
        )
    }

    /// Checks that the plugin may add dimension `id`.
    pub fn check_dimension(&self, id: &str) -> Result<(), Denied> {
        self.allowed(
            Kind::Dimension,
            &Entry {
                kind: Kind::Dimension,
                rule: Rule::Dimension(id.to_owned()),
            },
        )
    }

    /// Checks that the plugin may add custom content with id (or asset
    /// namespace) `id`; only its namespace counts.
    pub fn check_content(&self, id: &str) -> Result<(), Denied> {
        let namespace = id.split_once(':').map_or(id, |(namespace, _)| namespace);
        self.allowed(
            Kind::Content,
            &Entry {
                kind: Kind::Content,
                rule: Rule::Content(namespace.to_owned()),
            },
        )
    }

    /// Checks that the plugin may use the host-call group `kind`
    /// (world-write, player-write, player-read or cross-plugin).
    pub fn check_group(&self, kind: Kind) -> Result<(), Denied> {
        self.allowed(
            kind,
            &Entry {
                kind,
                rule: Rule::Group,
            },
        )
    }

    /// Checks binding a listener; `reserved` are the server's own ports
    /// (game, HTTP, query, RCON), never granted whatever the grant says.
    pub fn check_listen(
        &self,
        kind: Kind,
        bind: IpAddr,
        port: u16,
        reserved: &[u16],
    ) -> Result<(), Denied> {
        if !matches!(kind, Kind::ListenTcp | Kind::ListenUdp) {
            return Err(Denied::Malformed(format!(
                "{} is not a listen kind",
                kind.name()
            )));
        }
        if port != 0 && reserved.contains(&port) {
            return Err(Denied::Reserved(port));
        }
        self.allowed(
            kind,
            &Entry {
                kind,
                rule: Rule::Listen(pattern::ListenRule {
                    port,
                    bind: Some(bind),
                }),
            },
        )
    }

    /// Lines for the owner: requests the grant does not cover, and grant
    /// entries no request overlaps (which therefore do nothing).
    pub fn report(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for request in &self.requests {
            let entry = &request.entry;
            if !self.granted.iter().any(|grant| grant.covers(entry)) {
                let reason = match request.reason.as_str() {
                    "" => String::new(),
                    reason => format!(" ({reason})"),
                };
                lines.push(format!("requests {entry}{reason}, not granted"));
            } else if let Rule::Net(rule) = &entry.rule
                && rule.private
                && !self.explicit.iter().any(|grant| match &grant.rule {
                    Rule::Net(granted) => granted.private && grant.covers(entry),
                    _ => false,
                })
            {
                lines.push(format!(
                    "requests {entry}, non-public addresses not granted"
                ));
            }
        }
        for grant in &self.explicit {
            let overlaps = self
                .requests
                .iter()
                .any(|r| grant.covers(&r.entry) || r.entry.covers(grant));
            if !overlaps {
                lines.push(format!(
                    "grant {grant} is not requested, so it has no effect"
                ));
            }
        }
        lines
    }
}
