//! Destinations a plugin asks for, parsed strictly. Executors connect to
//! the parsed parts (and send [`Target::url`], the canonical form), never
//! to the plugin's original text, so no second parser can read it
//! differently from the policy.

use std::fmt;
use std::net::SocketAddr;

use super::pattern::{self, Kind, Scheme, TargetHost};

/// The longest URL a plugin may pass.
pub const MAX_URL: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub scheme: Scheme,
    pub host: TargetHost,
    pub port: u16,
    /// Path and query; `/` for raw sockets.
    pub path: String,
}

impl Target {
    /// Parses an absolute `http(s)://` or `ws(s)://` URL for `kind`. No
    /// user info, no fragment, no whitespace or non-ASCII, a strict host
    /// and a clean path (see [`pattern::check_path`]).
    pub fn url(kind: Kind, text: &str) -> Result<Target, String> {
        if text.len() > MAX_URL {
            return Err(format!("the URL is over {MAX_URL} bytes"));
        }
        let (scheme, rest) = text
            .split_once("://")
            .ok_or_else(|| format!("{text:?} is not an absolute URL"))?;
        let scheme = Scheme::parse(scheme)
            .filter(|scheme| match kind {
                Kind::Http => matches!(scheme, Scheme::Http | Scheme::Https),
                Kind::WebSocket => matches!(scheme, Scheme::Ws | Scheme::Wss),
                _ => false,
            })
            .ok_or_else(|| {
                format!(
                    "{text:?}: scheme {scheme:?} is not one {} uses",
                    kind.name()
                )
            })?;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, path) = rest.split_at(end);
        if authority.contains('@') {
            return Err(format!("{text:?} carries user info"));
        }
        let (host, port) = pattern::split_port(authority)?;
        let host = pattern::target_host(host)?;
        let port = match port {
            Some(port) => pattern::port(port)?,
            None => scheme.default_port().unwrap_or_default(),
        };
        if port == 0 {
            return Err(format!("{text:?} names port 0"));
        }
        let path = match path {
            "" => "/".to_owned(),
            query if query.starts_with('?') => format!("/{query}"),
            path => path.to_owned(),
        };
        pattern::check_path(&path)?;
        Ok(Target {
            scheme,
            host,
            port,
            path,
        })
    }

    pub fn socket(host: &str, port: u16) -> Result<Target, String> {
        if port == 0 {
            return Err("port 0 is not a destination".into());
        }
        let host = match host.parse::<std::net::Ipv6Addr>() {
            Ok(ip) => TargetHost::Ip(ip.into()),
            Err(_) => pattern::target_host(host)?,
        };
        Ok(Target {
            scheme: Scheme::Raw,
            host,
            port,
            path: "/".to_owned(),
        })
    }

    /// The canonical URL to hand to an HTTP or WebSocket client.
    pub fn url_text(&self) -> String {
        format!(
            "{}://{}:{}{}",
            self.scheme.name(),
            self.host,
            self.port,
            self.path
        )
    }

    /// The address to connect to when the host is an IP literal (which no
    /// resolver ever sees).
    pub fn literal(&self) -> Option<SocketAddr> {
        match self.host {
            TargetHost::Ip(ip) => Some(SocketAddr::new(ip, self.port)),
            TargetHost::Name(_) => None,
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.scheme {
            Scheme::Raw => write!(f, "{}:{}", self.host, self.port),
            _ => f.write_str(&self.url_text()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn urls_parse_into_canonical_parts() {
        let target =
            Target::url(Kind::Http, "HTTPS://Discord.com/api/webhooks/1?wait=true").unwrap();
        assert_eq!(target.scheme, Scheme::Https);
        assert_eq!(target.host, TargetHost::Name("discord.com".into()));
        assert_eq!(target.port, 443);
        assert_eq!(target.path, "/api/webhooks/1?wait=true");
        assert_eq!(
            target.url_text(),
            "https://discord.com:443/api/webhooks/1?wait=true"
        );
        let target = Target::url(Kind::WebSocket, "ws://[::1]:8080").unwrap();
        assert_eq!(target.literal(), Some("[::1]:8080".parse().unwrap()));
        assert_eq!(target.path, "/");
        assert_eq!(
            Target::url(Kind::Http, "http://a.example?x=1")
                .unwrap()
                .path,
            "/?x=1"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn hostile_urls_are_refused() {
        for bad in [
            "discord.com/api",
            "ftp://example.com/",
            "ws://example.com/",
            "https://user:pass@example.com/",
            "https://example.com@127.0.0.1/",
            "https://example.com/#frag",
            "https://127.1/",
            "https://2130706433/",
            "https://0x7f000001/",
            "https://example.com:0/",
            "https://example.com:99999/",
            "https://example.com/a/../../admin",
            "https://example.com/%2e%2e/admin",
            "https://example.com/ a",
            "https://exa mple.com/",
            "https://example.com\\@evil.com/",
            "https://[::1/",
            "https:///path",
        ] {
            assert!(Target::url(Kind::Http, bad).is_err(), "{bad}");
        }
        assert!(
            Target::url(
                Kind::Http,
                &format!("https://a.example/{}", "a".repeat(MAX_URL))
            )
            .is_err()
        );
        assert!(Target::socket("db.example", 0).is_err());
    }
}
