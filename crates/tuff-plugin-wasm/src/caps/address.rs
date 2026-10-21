//! Which network a destination address belongs to. Only public addresses
//! are reachable by default; everything else needs an owner grant that
//! names it, and some addresses are never reachable.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Globally routable unicast.
    Public,
    /// Loopback, private, link-local, shared, documentation or reserved
    /// space: reachable only through a grant marked `private` or one that
    /// names the address (or a range holding it) literally.
    Private(&'static str),
    /// A cloud instance metadata service: reachable only through a grant
    /// naming exactly this address.
    Metadata,
    /// Never a destination: unspecified, broadcast or multicast.
    Forbidden(&'static str),
}

impl Class {
    pub fn describe(self) -> &'static str {
        match self {
            Class::Public => "public",
            Class::Private(what) | Class::Forbidden(what) => what,
            Class::Metadata => "cloud metadata",
        }
    }
}

const METADATA_V4: [Ipv4Addr; 3] = [
    Ipv4Addr::new(169, 254, 169, 254),
    Ipv4Addr::new(100, 100, 100, 200),
    Ipv4Addr::new(192, 0, 0, 192),
];
const METADATA_V6: [Ipv6Addr; 1] = [Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254)];

pub fn classify(ip: IpAddr) -> Class {
    match ip {
        IpAddr::V4(ip) => classify_v4(ip),
        IpAddr::V6(ip) => classify_v6(ip),
    }
}

fn in_v4(ip: Ipv4Addr, net: [u8; 4], len: u32) -> bool {
    let mask = u32::MAX.checked_shl(32 - len).unwrap_or(0);
    u32::from(ip) & mask == u32::from_be_bytes(net) & mask
}

fn classify_v4(ip: Ipv4Addr) -> Class {
    if METADATA_V4.contains(&ip) {
        return Class::Metadata;
    }
    const TABLE: &[([u8; 4], u32, Class)] = &[
        ([0, 0, 0, 0], 8, Class::Forbidden("unspecified")),
        ([255, 255, 255, 255], 32, Class::Forbidden("broadcast")),
        ([224, 0, 0, 0], 4, Class::Forbidden("multicast")),
        ([127, 0, 0, 0], 8, Class::Private("loopback")),
        ([10, 0, 0, 0], 8, Class::Private("private")),
        ([172, 16, 0, 0], 12, Class::Private("private")),
        ([192, 168, 0, 0], 16, Class::Private("private")),
        ([100, 64, 0, 0], 10, Class::Private("shared address space")),
        ([169, 254, 0, 0], 16, Class::Private("link-local")),
        ([192, 0, 0, 0], 24, Class::Private("protocol assignment")),
        ([192, 0, 2, 0], 24, Class::Private("documentation")),
        ([198, 51, 100, 0], 24, Class::Private("documentation")),
        ([203, 0, 113, 0], 24, Class::Private("documentation")),
        ([198, 18, 0, 0], 15, Class::Private("benchmarking")),
        ([192, 88, 99, 0], 24, Class::Private("6to4 relay")),
        ([240, 0, 0, 0], 4, Class::Private("reserved")),
    ];
    TABLE
        .iter()
        .find(|(net, len, _)| in_v4(ip, *net, *len))
        .map_or(Class::Public, |(_, _, class)| *class)
}

fn in_v6(ip: Ipv6Addr, net: [u16; 8], len: u32) -> bool {
    let mask = u128::MAX.checked_shl(128 - len).unwrap_or(0);
    u128::from(ip) & mask == u128::from(Ipv6Addr::from(net)) & mask
}

fn embedded_v4(bits: u128, shift: u32) -> Ipv4Addr {
    Ipv4Addr::from((bits >> shift) as u32)
}

fn classify_v6(ip: Ipv6Addr) -> Class {
    if METADATA_V6.contains(&ip) {
        return Class::Metadata;
    }
    let bits = u128::from(ip);
    if ip.is_unspecified() {
        return Class::Forbidden("unspecified");
    }
    if ip.is_loopback() {
        return Class::Private("loopback");
    }
    if in_v6(ip, [0, 0, 0, 0, 0, 0xffff, 0, 0], 96)
        || in_v6(ip, [0x64, 0xff9b, 0, 0, 0, 0, 0, 0], 96)
    {
        return classify_v4(embedded_v4(bits, 0));
    }
    if in_v6(ip, [0x2002, 0, 0, 0, 0, 0, 0, 0], 16) {
        return match classify_v4(embedded_v4(bits, 80)) {
            Class::Public => Class::Public,
            Class::Metadata => Class::Metadata,
            _ => Class::Private("6to4 of a non-public address"),
        };
    }
    const TABLE: &[([u16; 8], u32, Class)] = &[
        (
            [0xff00, 0, 0, 0, 0, 0, 0, 0],
            8,
            Class::Forbidden("multicast"),
        ),
        (
            [0, 0, 0, 0, 0, 0, 0, 0],
            96,
            Class::Private("IPv4-compatible"),
        ),
        (
            [0x64, 0xff9b, 1, 0, 0, 0, 0, 0],
            48,
            Class::Private("local NAT64"),
        ),
        ([0x100, 0, 0, 0, 0, 0, 0, 0], 64, Class::Private("discard")),
        (
            [0x2001, 0xdb8, 0, 0, 0, 0, 0, 0],
            32,
            Class::Private("documentation"),
        ),
        (
            [0x3fff, 0, 0, 0, 0, 0, 0, 0],
            20,
            Class::Private("documentation"),
        ),
        ([0x2001, 0, 0, 0, 0, 0, 0, 0], 32, Class::Private("Teredo")),
        (
            [0x2001, 0, 0, 0, 0, 0, 0, 0],
            23,
            Class::Private("protocol assignment"),
        ),
        (
            [0x5f00, 0, 0, 0, 0, 0, 0, 0],
            16,
            Class::Private("segment routing"),
        ),
        (
            [0xfc00, 0, 0, 0, 0, 0, 0, 0],
            7,
            Class::Private("unique local"),
        ),
        (
            [0xfe80, 0, 0, 0, 0, 0, 0, 0],
            10,
            Class::Private("link-local"),
        ),
        (
            [0xfec0, 0, 0, 0, 0, 0, 0, 0],
            10,
            Class::Private("site-local"),
        ),
        ([0x2000, 0, 0, 0, 0, 0, 0, 0], 3, Class::Public),
    ];
    TABLE
        .iter()
        .find(|(net, len, _)| in_v6(ip, *net, *len))
        .map_or(Class::Private("reserved"), |(_, _, class)| *class)
}

/// Whether `ip` lies in `net`/`len`; addresses of different families
/// never match.
pub fn in_network(ip: IpAddr, net: IpAddr, len: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(ip), IpAddr::V4(net)) => in_v4(ip, net.octets(), u32::from(len.min(32))),
        (IpAddr::V6(ip), IpAddr::V6(net)) => in_v6(ip, net.segments(), u32::from(len.min(128))),
        _ => false,
    }
}

/// `ip` with an IPv4-mapped IPv6 address turned back into IPv4, so grants
/// written for one form match the other.
pub fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(text: &str) -> Class {
        classify(text.parse().unwrap())
    }

    #[tuff_core::watchdog]
    #[test]
    fn public_addresses_pass() {
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "2606:4700::1111",
            "2a00:1450::1",
        ] {
            assert_eq!(class(ip), Class::Public, "{ip}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn local_and_reserved_ranges_are_private() {
        for ip in [
            "127.0.0.1",
            "127.255.0.9",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "100.64.0.1",
            "169.254.1.1",
            "192.0.2.5",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "2001:db8::1",
            "2001::1",
            "100::1",
            "::7f00:1",
            "4000::1",
        ] {
            assert!(
                matches!(class(ip), Class::Private(_)),
                "{ip}: {:?}",
                class(ip)
            );
        }
        assert_eq!(class("172.32.0.1"), Class::Public);
        assert_eq!(class("100.128.0.1"), Class::Public);
    }

    #[tuff_core::watchdog]
    #[test]
    fn embedded_ipv4_is_classified_by_the_embedded_address() {
        assert_eq!(class("::ffff:127.0.0.1"), Class::Private("loopback"));
        assert_eq!(class("::ffff:8.8.8.8"), Class::Public);
        assert_eq!(class("64:ff9b::7f00:1"), Class::Private("loopback"));
        assert_eq!(class("64:ff9b::808:808"), Class::Public);
        assert_eq!(class("::ffff:169.254.169.254"), Class::Metadata);
        assert!(matches!(class("2002:7f00:1::1"), Class::Private(_)));
        assert!(matches!(class("2002:c0a8:101::1"), Class::Private(_)));
        assert_eq!(class("2002:808:808::1"), Class::Public);
        assert!(matches!(class("64:ff9b:1::1"), Class::Private(_)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn metadata_and_never_reachable_addresses() {
        for ip in [
            "169.254.169.254",
            "100.100.100.200",
            "192.0.0.192",
            "fd00:ec2::254",
        ] {
            assert_eq!(class(ip), Class::Metadata, "{ip}");
        }
        for ip in [
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "239.1.1.1",
            "::",
            "ff02::1",
        ] {
            assert!(matches!(class(ip), Class::Forbidden(_)), "{ip}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn networks_match_by_family_and_prefix() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        assert!(in_network(ip("10.9.8.7"), ip("10.0.0.0"), 8));
        assert!(!in_network(ip("11.0.0.1"), ip("10.0.0.0"), 8));
        assert!(in_network(ip("1.2.3.4"), ip("0.0.0.0"), 0));
        assert!(!in_network(ip("::1"), ip("0.0.0.0"), 0));
        assert!(in_network(ip("fd00::5"), ip("fd00::"), 8));
        assert_eq!(canonical(ip("::ffff:10.0.0.1")), ip("10.0.0.1"));
    }
}
