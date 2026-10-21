use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

/// Largest frame an unauthenticated peer may send (handshake, status, login, configuration).
pub const PRE_LOGIN_FRAME_LIMIT: usize = 8 * 1024;
/// Status requests answered per connection before it is dropped.
pub const MAX_STATUS_REQUESTS: u32 = 4;

/// The address a connection is counted under: IPv6 peers share a /64.
pub fn limit_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return IpAddr::V4(v4);
            }
            let mut octets = v6.octets();
            octets[8..].fill(0);
            IpAddr::V6(octets.into())
        }
    }
}

#[derive(Default)]
struct State {
    total: usize,
    per_ip: HashMap<IpAddr, usize>,
}

/// Counts connections that have not finished logging in.
#[derive(Clone, Default)]
pub struct Admission {
    state: Arc<Mutex<State>>,
}

/// Held while a connection is unauthenticated; dropping it frees the slot.
pub struct PreLoginPermit {
    state: Arc<Mutex<State>>,
    key: IpAddr,
}

impl Admission {
    /// Reserves a slot, or `None` when a cap (0 = unlimited) is reached.
    pub fn admit(&self, ip: IpAddr, max_total: usize, max_per_ip: usize) -> Option<PreLoginPermit> {
        let key = limit_key(ip);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let used = state.per_ip.get(&key).copied().unwrap_or(0);
        if (max_total != 0 && state.total >= max_total) || (max_per_ip != 0 && used >= max_per_ip) {
            return None;
        }
        state.total += 1;
        state.per_ip.insert(key, used + 1);
        Some(PreLoginPermit {
            state: self.state.clone(),
            key,
        })
    }

    pub fn in_flight(&self) -> usize {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).total
    }
}

impl Drop for PreLoginPermit {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.total = state.total.saturating_sub(1);
        if let Some(count) = state.per_ip.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                state.per_ip.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn per_ip_and_global_caps_hold_and_release() {
        let admission = Admission::default();
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        let first = admission.admit(a, 3, 2).unwrap();
        let _second = admission.admit(a, 3, 2).unwrap();
        assert!(admission.admit(a, 3, 2).is_none());
        let _third = admission.admit(b, 3, 2).unwrap();
        assert!(admission.admit(b, 3, 2).is_none());
        drop(first);
        let _fourth = admission.admit(a, 3, 2).unwrap();
        assert_eq!(admission.in_flight(), 3);
    }

    #[tuff_core::watchdog]
    #[test]
    fn ipv6_shares_a_slash_64_and_zero_is_unlimited() {
        let admission = Admission::default();
        let a: IpAddr = "2001:db8::1".parse().unwrap();
        let same_net: IpAddr = "2001:db8::ffff:1".parse().unwrap();
        let other: IpAddr = "2001:db8:0:1::1".parse().unwrap();
        let _p = admission.admit(a, 0, 1).unwrap();
        assert!(admission.admit(same_net, 0, 1).is_none());
        assert!(admission.admit(other, 0, 1).is_some());
        let mapped: IpAddr = "::ffff:10.0.0.9".parse().unwrap();
        assert_eq!(limit_key(mapped), "10.0.0.9".parse::<IpAddr>().unwrap());
        let permits: Vec<_> = (0..100)
            .map(|_| admission.admit(a, 0, 0).unwrap())
            .collect();
        assert_eq!(permits.len(), 100);
    }
}
