//! A block click can cost the item it was made with. The item travels with
//! the click to the region, which spends it with the effect or hands it
//! back; until the region answers, a use of the same hand waits so it finds
//! the item again.

use std::collections::VecDeque;

use super::PlayerSession;
use crate::protocol::play::Serverbound;

const HOLD_TICKS: u64 = 20;

#[derive(Default)]
pub struct ClicksInFlight {
    sequences: Vec<i32>,
    held_uses: VecDeque<(u64, Serverbound)>,
}

impl PlayerSession {
    /// A click with sequence `sequence` carries an item the region answers
    /// for.
    pub(super) fn expect_answer(&mut self, sequence: i32) {
        self.actions.clicks.sequences.push(sequence);
    }

    /// The region answered the click with `sequence` (or could not be asked).
    pub(super) fn answered(&mut self, sequence: i32) {
        self.actions.clicks.sequences.retain(|s| *s != sequence);
    }

    /// Holds an item use back while a click's item is with the region;
    /// returns the packet when it need not wait.
    pub(super) fn hold_use(&mut self, packet: Serverbound) -> Option<Serverbound> {
        if matches!(packet, Serverbound::UseItem { .. })
            && !self.actions.clicks.sequences.is_empty()
        {
            self.actions.clicks.held_uses.push_back((self.tick, packet));
            return None;
        }
        Some(packet)
    }

    /// The next held-back item use, once the clicks are answered or have
    /// waited long enough.
    pub(super) fn next_held_use(&mut self) -> Option<Serverbound> {
        let clicks = &mut self.actions.clicks;
        let (since, _) = clicks.held_uses.front()?;
        if !clicks.sequences.is_empty() {
            if self.tick.saturating_sub(*since) < HOLD_TICKS {
                return None;
            }
            clicks.sequences.clear();
        }
        clicks.held_uses.pop_front().map(|(_, packet)| packet)
    }
}
