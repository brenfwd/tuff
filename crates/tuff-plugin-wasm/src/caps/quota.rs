//! Per-plugin resource limits and the shared counters every capability
//! executor charges. Time is passed in, so tests are deterministic.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::size;

/// Every limit an owner can set per plugin with `limit.<name> = value`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Open outbound connections and sockets (HTTP requests in flight,
    /// WebSockets, TCP and UDP sockets) at once.
    pub connections: u32,
    /// Accepted inbound connections at once, over all listeners.
    pub inbound_connections: u32,
    /// New outbound requests and connections per minute.
    pub requests_per_minute: u32,
    pub bytes_in_per_minute: u64,
    pub bytes_out_per_minute: u64,
    /// The largest HTTP response body, after any decompression.
    pub response_bytes: u64,
    /// The largest HTTP request body a plugin may send.
    pub request_bytes: u64,
    /// The largest WebSocket message (after any decompression) or raw
    /// socket read delivered as one event.
    pub message_bytes: u64,
    pub connect_timeout: Duration,
    /// Whole HTTP request, from connect to the last body byte.
    pub request_timeout: Duration,
    /// A socket with no traffic either way for this long is closed.
    pub idle_timeout: Duration,
    /// Results waiting for the plugin to handle them; I/O for the plugin
    /// pauses while this many are queued.
    pub pending_events: u32,
    /// How many files and directories the plugin's file store may hold.
    pub files: u32,
    /// The largest request body a plugin route accepts.
    pub route_body_bytes: u64,
    /// Requests to the plugin's routes being handled at once; more get 503.
    pub route_concurrency: u32,
    /// How long a route request waits for the plugin's response.
    pub route_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            connections: 8,
            inbound_connections: 64,
            requests_per_minute: 120,
            bytes_in_per_minute: 16 << 20,
            bytes_out_per_minute: 16 << 20,
            response_bytes: 4 << 20,
            request_bytes: 1 << 20,
            message_bytes: 1 << 20,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(300),
            pending_events: 256,
            files: 4096,
            route_body_bytes: 1 << 20,
            route_concurrency: 16,
            route_timeout: Duration::from_secs(10),
        }
    }
}

impl Limits {
    pub const NAMES: [&'static str; 16] = [
        "connections",
        "inbound-connections",
        "requests-per-minute",
        "bytes-in-per-minute",
        "bytes-out-per-minute",
        "response-bytes",
        "request-bytes",
        "message-bytes",
        "connect-timeout",
        "request-timeout",
        "idle-timeout",
        "pending-events",
        "files",
        "route-body-bytes",
        "route-concurrency",
        "route-timeout",
    ];

    /// The limit called `name` as text `set` reads back (counts, bytes or
    /// milliseconds).
    pub fn get(&self, name: &str) -> String {
        let ms = |d: Duration| format!("{}ms", d.as_millis());
        match name {
            "connections" => self.connections.to_string(),
            "inbound-connections" => self.inbound_connections.to_string(),
            "requests-per-minute" => self.requests_per_minute.to_string(),
            "bytes-in-per-minute" => self.bytes_in_per_minute.to_string(),
            "bytes-out-per-minute" => self.bytes_out_per_minute.to_string(),
            "response-bytes" => self.response_bytes.to_string(),
            "request-bytes" => self.request_bytes.to_string(),
            "message-bytes" => self.message_bytes.to_string(),
            "connect-timeout" => ms(self.connect_timeout),
            "request-timeout" => ms(self.request_timeout),
            "idle-timeout" => ms(self.idle_timeout),
            "pending-events" => self.pending_events.to_string(),
            "files" => self.files.to_string(),
            "route-body-bytes" => self.route_body_bytes.to_string(),
            "route-concurrency" => self.route_concurrency.to_string(),
            "route-timeout" => ms(self.route_timeout),
            _ => String::new(),
        }
    }

    /// Sets the limit called `name` from owner text.
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), String> {
        let count = || -> Result<u32, String> {
            value
                .trim()
                .parse::<u32>()
                .map_err(|_| format!("limit.{name}: {value:?} is not a count"))
        };
        let bytes = || size::bytes(value).map_err(|err| format!("limit.{name}: {err}"));
        let time = || size::duration(value).map_err(|err| format!("limit.{name}: {err}"));
        match name {
            "connections" => self.connections = count()?,
            "inbound-connections" => self.inbound_connections = count()?,
            "requests-per-minute" => self.requests_per_minute = count()?,
            "bytes-in-per-minute" => self.bytes_in_per_minute = bytes()?,
            "bytes-out-per-minute" => self.bytes_out_per_minute = bytes()?,
            "response-bytes" => self.response_bytes = bytes()?,
            "request-bytes" => self.request_bytes = bytes()?,
            "message-bytes" => self.message_bytes = bytes()?,
            "connect-timeout" => self.connect_timeout = time()?,
            "request-timeout" => self.request_timeout = time()?,
            "idle-timeout" => self.idle_timeout = time()?,
            "pending-events" => self.pending_events = count()?.max(1),
            "files" => self.files = count()?,
            "route-body-bytes" => self.route_body_bytes = bytes()?,
            "route-concurrency" => self.route_concurrency = count()?,
            "route-timeout" => self.route_timeout = time()?,
            _ => return Err(format!("there is no limit called {name:?}")),
        }
        Ok(())
    }
}

const NANOS_PER_MINUTE: i128 = 60_000_000_000;

/// A token bucket refilled continuously at `per_minute`, holding at most
/// one minute's worth. It may go into debt for one large charge; the
/// caller then waits until it is repaid.
#[derive(Debug)]
pub struct Bucket {
    per_minute: u64,
    /// Tokens times nanoseconds per minute, so refill is exact integer math.
    level: i128,
    last: Instant,
}

impl Bucket {
    pub fn new(per_minute: u64, now: Instant) -> Self {
        Self {
            per_minute,
            level: i128::from(per_minute) * NANOS_PER_MINUTE,
            last: now,
        }
    }

    /// Changes the rate; the tokens held stay, capped at the new minute.
    pub fn set_rate(&mut self, per_minute: u64, now: Instant) {
        self.refill(now);
        self.per_minute = per_minute;
        let cap = i128::from(per_minute) * NANOS_PER_MINUTE;
        self.level = self.level.min(cap);
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_nanos();
        self.last = self.last.max(now);
        let cap = i128::from(self.per_minute) * NANOS_PER_MINUTE;
        let gained = i128::try_from(elapsed)
            .unwrap_or(i128::MAX)
            .saturating_mul(i128::from(self.per_minute));
        self.level = self.level.saturating_add(gained).min(cap);
    }

    /// Takes `n` tokens if they are all there.
    pub fn try_take(&mut self, now: Instant, n: u64) -> bool {
        self.refill(now);
        let need = i128::from(n) * NANOS_PER_MINUTE;
        if self.level < need {
            return false;
        }
        self.level -= need;
        true
    }

    /// Takes `n` tokens, going into debt if short, and says how long the
    /// caller must wait before the debt is repaid.
    pub fn charge(&mut self, now: Instant, n: u64) -> Duration {
        self.refill(now);
        self.level -= i128::from(n) * NANOS_PER_MINUTE;
        if self.level >= 0 {
            return Duration::ZERO;
        }
        if self.per_minute == 0 {
            return Duration::MAX;
        }
        let rate = i128::from(self.per_minute);
        let nanos = (-self.level + rate - 1) / rate;
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exceeded {
    Connections(u32),
    InboundConnections(u32),
    RequestRate(u32),
}

impl std::fmt::Display for Exceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Exceeded::Connections(n) => write!(f, "the plugin already has {n} open connections"),
            Exceeded::InboundConnections(n) => {
                write!(f, "the plugin already has {n} inbound connections")
            }
            Exceeded::RequestRate(n) => {
                write!(f, "the plugin made {n} requests in the last minute")
            }
        }
    }
}

/// One plugin's live usage, shared by all its executors. Dropping the
/// plugin's capability record does not reset it while slots are held;
/// a reload builds a new one.
#[derive(Debug)]
pub struct Usage {
    limits: Mutex<Limits>,
    outbound: AtomicU32,
    inbound: AtomicU32,
    requests: Mutex<Bucket>,
    bytes_in: Mutex<Bucket>,
    bytes_out: Mutex<Bucket>,
}

impl Usage {
    pub fn new(limits: Limits, now: Instant) -> Arc<Self> {
        Arc::new(Self {
            limits: Mutex::new(limits),
            outbound: AtomicU32::new(0),
            inbound: AtomicU32::new(0),
            requests: Mutex::new(Bucket::new(u64::from(limits.requests_per_minute), now)),
            bytes_in: Mutex::new(Bucket::new(limits.bytes_in_per_minute, now)),
            bytes_out: Mutex::new(Bucket::new(limits.bytes_out_per_minute, now)),
        })
    }

    pub fn limits(&self) -> Limits {
        *self.limits.lock().unwrap()
    }

    /// Applies changed owner limits to everything that checks them from
    /// now on; open connections stay.
    pub fn set_limits(&self, limits: Limits) {
        let now = Instant::now();
        let mut current = self.limits.lock().unwrap();
        if *current == limits {
            return;
        }
        *current = limits;
        self.requests
            .lock()
            .unwrap()
            .set_rate(u64::from(limits.requests_per_minute), now);
        self.bytes_in
            .lock()
            .unwrap()
            .set_rate(limits.bytes_in_per_minute, now);
        self.bytes_out
            .lock()
            .unwrap()
            .set_rate(limits.bytes_out_per_minute, now);
    }

    /// Opens an outbound connection or request: one request token and one
    /// connection slot, held until the returned [`Slot`] drops.
    pub fn connect(self: &Arc<Self>, now: Instant) -> Result<Slot, Exceeded> {
        let limits = self.limits();
        let slot = self
            .slot(&self.outbound, limits.connections, false)
            .ok_or(Exceeded::Connections(limits.connections))?;
        if !self.requests.lock().unwrap().try_take(now, 1) {
            return Err(Exceeded::RequestRate(limits.requests_per_minute));
        }
        Ok(slot)
    }

    /// Accepts an inbound connection, held until the [`Slot`] drops.
    pub fn accept(self: &Arc<Self>) -> Result<Slot, Exceeded> {
        let max = self.limits().inbound_connections;
        self.slot(&self.inbound, max, true)
            .ok_or(Exceeded::InboundConnections(max))
    }

    fn slot(self: &Arc<Self>, counter: &AtomicU32, max: u32, inbound: bool) -> Option<Slot> {
        counter
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < max).then_some(n + 1)
            })
            .ok()?;
        Some(Slot {
            usage: self.clone(),
            inbound,
        })
    }

    pub fn open_outbound(&self) -> u32 {
        self.outbound.load(Ordering::Acquire)
    }

    pub fn open_inbound(&self) -> u32 {
        self.inbound.load(Ordering::Acquire)
    }

    /// Charges `bytes` sent; the sender waits the returned time before
    /// sending more.
    pub fn sent(&self, now: Instant, bytes: u64) -> Duration {
        self.bytes_out.lock().unwrap().charge(now, bytes)
    }

    /// Charges `bytes` received; the reader pauses the returned time.
    pub fn received(&self, now: Instant, bytes: u64) -> Duration {
        self.bytes_in.lock().unwrap().charge(now, bytes)
    }
}

/// A held connection slot; dropping it frees the slot.
#[derive(Debug)]
pub struct Slot {
    usage: Arc<Usage>,
    inbound: bool,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let counter = match self.inbound {
            true => &self.usage.inbound,
            false => &self.usage.outbound,
        };
        counter.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn owner_limits_parse_by_name() {
        let mut limits = Limits::default();
        limits.set("connections", "32").unwrap();
        limits.set("response-bytes", "16 MiB").unwrap();
        limits.set("request-timeout", "90s").unwrap();
        assert_eq!(limits.connections, 32);
        assert_eq!(limits.response_bytes, 16 << 20);
        assert_eq!(limits.request_timeout, Duration::from_secs(90));
        assert!(limits.set("connection", "1").is_err());
        assert!(limits.set("connections", "-1").is_err());
        for name in Limits::NAMES {
            let err = limits.set(name, "x").unwrap_err();
            assert!(!err.starts_with("there is no limit"), "{name}: {err}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn connection_slots_are_capped_and_freed_on_drop() {
        let now = Instant::now();
        let limits = Limits {
            connections: 2,
            inbound_connections: 1,
            ..Limits::default()
        };
        let usage = Usage::new(limits, now);
        let a = usage.connect(now).unwrap();
        let _b = usage.connect(now).unwrap();
        assert_eq!(usage.connect(now).unwrap_err(), Exceeded::Connections(2));
        drop(a);
        assert_eq!(usage.open_outbound(), 1);
        assert!(usage.connect(now).is_ok());
        let inbound = usage.accept().unwrap();
        assert!(usage.accept().is_err());
        drop(inbound);
        assert!(usage.accept().is_ok());
    }

    #[tuff_core::watchdog]
    #[test]
    fn request_rate_refills_over_the_minute() {
        let start = Instant::now();
        let limits = Limits {
            requests_per_minute: 3,
            ..Limits::default()
        };
        let usage = Usage::new(limits, start);
        for _ in 0..3 {
            drop(usage.connect(start).unwrap());
        }
        assert_eq!(usage.connect(start).unwrap_err(), Exceeded::RequestRate(3));
        assert!(usage.connect(start + Duration::from_secs(19)).is_err());
        assert!(usage.connect(start + Duration::from_secs(20)).is_ok());
        assert!(usage.connect(start + Duration::from_secs(21)).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn byte_budgets_throttle_instead_of_failing() {
        let start = Instant::now();
        let mut bucket = Bucket::new(600, start);
        assert_eq!(bucket.charge(start, 600), Duration::ZERO);
        assert_eq!(bucket.charge(start, 60), Duration::from_secs(6));
        assert_eq!(
            bucket.charge(start + Duration::from_secs(6), 0),
            Duration::ZERO
        );
        let mut big = Bucket::new(600, start);
        assert_eq!(big.charge(start, 1800), Duration::from_secs(120));
        let mut never = Bucket::new(0, start);
        assert_eq!(never.charge(start, 1), Duration::MAX);
        let mut capped = Bucket::new(60, start);
        assert!(capped.try_take(start + Duration::from_secs(3600), 60));
        assert!(!capped.try_take(start + Duration::from_secs(3600), 1));
    }
}
