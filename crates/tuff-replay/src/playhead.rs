//! Playback position, pause and speed, advanced by elapsed real time.
//! Pure, so tests drive it with exact durations.

use std::time::Duration;

pub const MAX_SPEED: f64 = 64.0;
pub const MIN_SPEED: f64 = 0.0625;

#[derive(Clone, Debug, PartialEq)]
pub struct Playhead {
    position: i64,
    duration: i64,
    speed: f64,
    paused: bool,
}

impl Playhead {
    pub fn new(duration: i64) -> Self {
        Self {
            position: 0,
            duration: duration.max(0),
            speed: 1.0,
            paused: false,
        }
    }

    pub fn position(&self) -> i64 {
        self.position
    }

    pub fn duration(&self) -> i64 {
        self.duration
    }

    pub fn speed(&self) -> f64 {
        self.speed
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn finished(&self) -> bool {
        self.position >= self.duration
    }

    /// Moves forward by `real` elapsed time scaled by the speed; returns
    /// the new position.
    pub fn advance(&mut self, real: Duration) -> i64 {
        if !self.paused {
            let step = (real.as_nanos() as f64 * self.speed) as i64;
            self.position = self.position.saturating_add(step).min(self.duration);
        }
        self.position
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn play(&mut self) {
        self.paused = false;
    }

    /// Sets the speed, clamped to [`MIN_SPEED`]..=[`MAX_SPEED`]; returns it.
    pub fn set_speed(&mut self, speed: f64) -> f64 {
        self.speed = if speed.is_finite() {
            speed.clamp(MIN_SPEED, MAX_SPEED)
        } else {
            1.0
        };
        self.speed
    }

    /// Jumps to `ns`, clamped to the recording; returns the new position.
    pub fn seek(&mut self, ns: i64) -> i64 {
        self.position = ns.clamp(0, self.duration);
        self.position
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 1_000_000_000;

    #[tuff_core::watchdog]
    #[test]
    fn advances_scaled_and_stops_while_paused_or_at_the_end() {
        let mut head = Playhead::new(10 * SECOND);
        assert_eq!(head.advance(Duration::from_secs(1)), SECOND);
        head.set_speed(2.0);
        assert_eq!(head.advance(Duration::from_secs(1)), 3 * SECOND);
        head.pause();
        assert_eq!(head.advance(Duration::from_secs(5)), 3 * SECOND);
        head.play();
        assert_eq!(head.advance(Duration::from_secs(60)), 10 * SECOND);
        assert!(head.finished());
    }

    #[tuff_core::watchdog]
    #[test]
    fn seek_and_speed_are_clamped() {
        let mut head = Playhead::new(10 * SECOND);
        assert_eq!(head.seek(-5), 0);
        assert_eq!(head.seek(99 * SECOND), 10 * SECOND);
        assert_eq!(head.set_speed(1000.0), MAX_SPEED);
        assert_eq!(head.set_speed(0.0), MIN_SPEED);
        assert_eq!(head.set_speed(f64::NAN), 1.0);
    }
}
