use super::*;

/// The bed the player sleeps in and how long it has slept.
#[derive(Default)]
pub struct SleepState {
    /// The bed this player sleeps in.
    pub sleeping: Option<Sleep>,
    /// Ticks asleep, capped at 100; after waking it runs on to 110 and
    /// resets.
    pub sleep_counter: u32,
}
