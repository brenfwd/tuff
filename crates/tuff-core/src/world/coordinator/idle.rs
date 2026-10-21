/// Server ticks in a row with nobody online.
#[derive(Default)]
pub(super) struct IdleClock {
    empty_ticks: u64,
}

impl IdleClock {
    pub(super) fn count(&mut self, empty: bool) {
        self.empty_ticks = if empty {
            self.empty_ticks.saturating_add(1)
        } else {
            0
        };
    }

    pub(super) fn paused(&self, seconds: i32) -> bool {
        seconds >= 0 && self.empty_ticks >= seconds as u64 * 20
    }
}
