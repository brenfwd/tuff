//! A short trace of where nearby entities and the player's own eyes were,
//! so an attack can be judged against what the attacker's client showed
//! some ticks ago rather than against the current tick alone.

use std::collections::{HashMap, VecDeque};

/// Ticks of history kept: a 400 ms round trip plus interpolation.
pub const KEEP_TICKS: u64 = 20;
/// Only entities this close are traced; anything farther is out of reach.
const NEAR: f64 = 10.0;
const MAX_TARGETS: usize = 64;

type Trace = VecDeque<(u64, [f64; 3])>;

#[derive(Debug, Default)]
pub struct PositionHistory {
    eyes: Trace,
    targets: HashMap<i32, Trace>,
}

fn note(trace: &mut Trace, tick: u64, position: [f64; 3]) {
    if trace.back().is_some_and(|(_, last)| *last == position) {
        if let Some(back) = trace.back_mut() {
            back.0 = tick;
        }
    } else {
        trace.push_back((tick, position));
    }
    while trace
        .front()
        .is_some_and(|(at, _)| tick.saturating_sub(*at) > KEEP_TICKS)
        && trace.len() > 1
    {
        trace.pop_front();
    }
}

impl PositionHistory {
    /// Records the eyes and every entity near them as of `tick`.
    pub fn record(
        &mut self,
        tick: u64,
        eye: [f64; 3],
        entities: impl Iterator<Item = (i32, [f64; 3])>,
    ) {
        note(&mut self.eyes, tick, eye);
        for (id, position) in entities {
            let d = [
                position[0] - eye[0],
                position[1] - eye[1],
                position[2] - eye[2],
            ];
            if d.iter().any(|c| c.abs() > NEAR) {
                continue;
            }
            if !self.targets.contains_key(&id) && self.targets.len() >= MAX_TARGETS {
                continue;
            }
            note(self.targets.entry(id).or_default(), tick, position);
        }
        self.targets.retain(|_, trace| {
            trace
                .back()
                .is_some_and(|(at, _)| tick.saturating_sub(*at) <= KEEP_TICKS)
        });
    }

    /// Where `entity` was within the last `window` ticks before `tick`.
    pub fn target(
        &self,
        entity: i32,
        tick: u64,
        window: u64,
    ) -> impl Iterator<Item = [f64; 3]> + '_ {
        within(self.targets.get(&entity), tick, window)
    }

    /// Where the eyes were within the last `window` ticks before `tick`.
    pub fn eyes(&self, tick: u64, window: u64) -> impl Iterator<Item = [f64; 3]> + '_ {
        within(Some(&self.eyes), tick, window)
    }
}

fn within(trace: Option<&Trace>, tick: u64, window: u64) -> impl Iterator<Item = [f64; 3]> + '_ {
    trace
        .into_iter()
        .flatten()
        .filter(move |(at, _)| tick.saturating_sub(*at) <= window)
        .map(|(_, position)| *position)
}

/// The ticks of history an attack from a client with this latency may
/// have aimed at: a round trip plus the client's interpolation.
pub fn window(latency_ms: i32) -> u64 {
    let round_trip = (u64::try_from(latency_ms.max(0)).unwrap_or(0) * 2).div_ceil(50);
    (round_trip + 3).min(KEEP_TICKS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_trace_keeps_only_the_window_and_nearby_entities() {
        let mut history = PositionHistory::default();
        for tick in 0..40u64 {
            let x = tick as f64;
            history.record(
                tick,
                [0.0, 65.0, 0.0],
                [(1, [x * 0.1, 64.0, 0.0]), (2, [50.0, 64.0, 0.0])].into_iter(),
            );
        }
        assert_eq!(history.target(2, 39, 20).count(), 0);
        let recent: Vec<_> = history.target(1, 39, 5).collect();
        assert_eq!(recent.len(), 6);
        assert!(history.target(1, 39, 100).count() <= KEEP_TICKS as usize + 1);
        assert_eq!(history.eyes(39, 20).count(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn entities_gone_for_a_while_are_forgotten() {
        let mut history = PositionHistory::default();
        history.record(0, [0.0; 3], [(1, [1.0, 0.0, 0.0])].into_iter());
        history.record(30, [0.0; 3], std::iter::empty());
        assert_eq!(history.target(1, 30, 100).count(), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_window_grows_with_ping_up_to_the_kept_history() {
        assert_eq!(window(0), 3);
        assert_eq!(window(100), 7);
        assert_eq!(window(5000), KEEP_TICKS);
    }
}
