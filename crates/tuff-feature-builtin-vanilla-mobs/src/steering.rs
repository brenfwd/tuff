//! Steering a saddled mount with a lure on a stick: the boost a rider's
//! click starts, and the ridden speed it scales.

const MIN_BOOST_TIME: u32 = 140;
const BOOST_RANGE: u32 = 841;
const BOOST: f32 = 1.15;
/// The most a boost multiplies the ridden speed by.
pub const MAX_FACTOR: f32 = 1.0 + BOOST;

#[derive(Debug, Default)]
pub struct ItemSteering {
    boosting: bool,
    time: i32,
    total: i32,
}

impl ItemSteering {
    pub fn total(&self) -> i32 {
        self.total
    }

    /// Starts a boost lasting 140 to 980 ticks from `roll`; false while
    /// one is running.
    pub fn boost(&mut self, roll: u32) -> bool {
        if self.boosting {
            return false;
        }
        self.boosting = true;
        self.time = 0;
        self.total = (roll % BOOST_RANGE + MIN_BOOST_TIME) as i32;
        true
    }

    pub fn tick(&mut self) {
        if self.boosting {
            let before = self.time;
            self.time += 1;
            if before > self.total {
                self.boosting = false;
            }
        }
    }

    pub fn factor(&self) -> f32 {
        if self.boosting {
            1.0 + BOOST * (self.time as f32 / self.total as f32 * std::f32::consts::PI).sin()
        } else {
            1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_boost_peaks_midway_and_ends() {
        let mut steering = ItemSteering::default();
        assert_eq!(steering.factor(), 1.0);
        assert!(steering.boost(0));
        assert!(!steering.boost(0), "one boost at a time");
        assert_eq!(steering.total(), 140);
        for _ in 0..70 {
            steering.tick();
        }
        assert!((steering.factor() - 2.15).abs() < 0.01);
        for _ in 0..80 {
            steering.tick();
        }
        assert_eq!(steering.factor(), 1.0);
        assert!(steering.boost(1));
    }
}
