use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tuff_core::world::messages::PlayerId;
use tuff_core::world::session::PlayerSession;

static ANY: AtomicBool = AtomicBool::new(false);
static SKEW: OnceLock<Mutex<HashMap<(usize, PlayerId), Duration>>> = OnceLock::new();

fn key(session: &PlayerSession) -> (usize, PlayerId) {
    (std::sync::Arc::as_ptr(&session.shared) as usize, session.id)
}

/// Wall-clock time as the checks see it for one player: real time plus any
/// skew a test added with [`advance`].
pub fn now(session: &PlayerSession) -> Instant {
    let real = Instant::now();
    if !ANY.load(Ordering::Relaxed) {
        return real;
    }
    let skew = SKEW
        .get()
        .and_then(|map| map.lock().ok()?.get(&key(session)).copied())
        .unwrap_or_default();
    real + skew
}

/// Moves one player's check clock forward without waiting.
pub fn advance(session: &PlayerSession, by: Duration) {
    let map = SKEW.get_or_init(Default::default);
    *map.lock().unwrap().entry(key(session)).or_default() += by;
    ANY.store(true, Ordering::Relaxed);
}
