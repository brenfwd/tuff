use tuff_core::protocol::play::Serverbound;

use crate::BotState;

/// Something a bot does over many client ticks, such as flying somewhere.
pub trait Goal: Send {
    fn tick(&mut self, state: &BotState) -> Step;
}

/// What a goal wants this client tick: where to be, then which packets
/// to send after the move, in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Step {
    pub position: Option<[f64; 3]>,
    pub rotation: Option<(f32, f32)>,
    pub on_ground: bool,
    pub actions: Vec<Serverbound>,
    pub done: bool,
}

/// The yaw and pitch that look from `eye` straight at `target`.
pub fn look_at(eye: [f64; 3], target: [f64; 3]) -> (f32, f32) {
    let [dx, dy, dz] = [0, 1, 2].map(|i| target[i] - eye[i]);
    let yaw = (-dx).atan2(dz).to_degrees();
    let pitch = -dy.atan2(dx.hypot(dz)).to_degrees();
    (yaw as f32, pitch as f32)
}

/// A small deterministic generator, so a seeded run repeats its choices.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform in `0.0..1.0`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `low..high`.
    pub fn range(&mut self, low: u32, high: u32) -> u32 {
        low + (self.next_u64() % u64::from((high - low).max(1))) as u32
    }
}

/// Stays put for a number of ticks, still sending the client's tick end.
pub struct Idle {
    pub ticks: u64,
}

impl Goal for Idle {
    fn tick(&mut self, _: &BotState) -> Step {
        self.ticks = self.ticks.saturating_sub(1);
        Step {
            done: self.ticks == 0,
            ..Step::default()
        }
    }
}

/// Blocks a [`FlyLine`] rises or sinks per tick on its way to its height.
pub const CLIMB_PER_TICK: f64 = 0.5;

/// Flies in a straight horizontal line at a fixed height, as a spectator
/// or a creative flyer would, climbing to that height first at
/// [`CLIMB_PER_TICK`]. Each tick goes on from where the bot is, so a
/// server putting it back costs distance rather than a jump to catch up.
pub struct FlyLine {
    /// Horizontal direction; normalized on the first tick.
    pub direction: [f64; 2],
    pub height: f64,
    pub distance: f64,
    pub blocks_per_tick: f64,
    start: Option<[f64; 3]>,
    flown: f64,
}

/// How far short of its distance a [`FlyLine`] may stop, for rounding.
const ARRIVED: f64 = 1e-6;

impl FlyLine {
    pub fn new(direction: [f64; 2], height: f64, distance: f64, blocks_per_tick: f64) -> Self {
        Self {
            direction,
            height,
            distance,
            blocks_per_tick,
            start: None,
            flown: 0.0,
        }
    }

    /// Distance along the line, as of the last tick.
    pub fn flown(&self) -> f64 {
        self.flown
    }
}

impl Goal for FlyLine {
    fn tick(&mut self, state: &BotState) -> Step {
        let here = state.position;
        let start = *self.start.get_or_insert(here);
        let [dx, dz] = self.direction;
        let length = dx.hypot(dz).max(f64::EPSILON);
        let (dx, dz) = (dx / length, dz / length);
        let yaw = (-dx).atan2(dz).to_degrees() as f32;
        self.flown = ((here[0] - start[0]) * dx + (here[2] - start[2]) * dz).max(0.0);
        if self.flown >= self.distance - ARRIVED && here[1] == self.height {
            return Step {
                rotation: Some((yaw, 0.0)),
                done: true,
                ..Step::default()
            };
        }
        let rise = self.height - here[1];
        let y = if rise.abs() <= CLIMB_PER_TICK {
            self.height
        } else {
            here[1] + CLIMB_PER_TICK.copysign(rise)
        };
        let along = (self.flown + self.blocks_per_tick).min(self.distance);
        Step {
            position: Some([start[0] + dx * along, y, start[2] + dz * along]),
            rotation: Some((yaw, 0.0)),
            ..Step::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fly(goal: &mut FlyLine, state: &mut BotState) -> Step {
        let step = goal.tick(state);
        if let Some(position) = step.position {
            state.position = position;
        }
        step
    }

    #[tuff_core::watchdog]
    #[test]
    fn fly_line_reaches_its_distance_facing_its_direction() {
        let mut goal = FlyLine::new([1.0, 0.0], 65.0, 2.5, 1.0);
        let mut state = BotState {
            position: [10.0, 64.0, 5.0],
            ..BotState::default()
        };
        let steps: Vec<Step> = (0..4).map(|_| fly(&mut goal, &mut state)).collect();
        assert_eq!(steps[0].position, Some([11.0, 64.5, 5.0]));
        assert_eq!(steps[2].position, Some([12.5, 65.0, 5.0]));
        assert!(!steps[2].done && steps[3].done && steps[3].position.is_none());
        assert_eq!(steps[0].rotation, Some((-90.0, 0.0)));
        assert_eq!(goal.flown(), 2.5);
    }

    #[tuff_core::watchdog]
    #[test]
    fn fly_line_goes_on_from_a_setback() {
        let mut goal = FlyLine::new([0.0, 1.0], 100.0, f64::INFINITY, 2.0);
        let mut state = BotState {
            position: [0.0, 100.0, 0.0],
            ..BotState::default()
        };
        for _ in 0..5 {
            fly(&mut goal, &mut state);
        }
        assert_eq!(state.position, [0.0, 100.0, 10.0]);
        state.position = [0.0, 100.0, 4.0];
        let step = fly(&mut goal, &mut state);
        assert_eq!(step.position, Some([0.0, 100.0, 6.0]));
        assert_eq!(goal.flown(), 4.0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn look_at_matches_the_game_axes() {
        let eye = [0.0, 0.0, 0.0];
        assert_eq!(look_at(eye, [0.0, 0.0, 1.0]), (0.0, 0.0));
        assert_eq!(look_at(eye, [-1.0, 0.0, 0.0]), (90.0, 0.0));
        let (_, pitch) = look_at(eye, [0.0, -1.0, 1.0]);
        assert!((pitch - 45.0).abs() < 1e-4);
    }

    #[tuff_core::watchdog]
    #[test]
    fn rng_repeats_per_seed_and_stays_in_range() {
        let draws = |seed| {
            let mut rng = Rng::new(seed);
            (0..64).map(|_| rng.range(3, 9)).collect::<Vec<_>>()
        };
        assert_eq!(draws(7), draws(7));
        assert_ne!(draws(7), draws(8));
        assert!(draws(7).iter().all(|v| (3..9).contains(v)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn idle_finishes_after_its_ticks() {
        let mut goal = Idle { ticks: 2 };
        let state = BotState::default();
        assert!(!goal.tick(&state).done);
        assert!(goal.tick(&state).done);
    }
}
