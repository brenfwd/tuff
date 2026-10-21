//! Rain and thunder: the game's weather cycle with its timers, the
//! clear/rain/thunder durations commands and the cycle pick, and the
//! `weather` saved data. Weather has no gameplay effect here yet; clients
//! are told the state through Game Event packets.

use rand::Rng;

use crate::protocol::play::{self, game_event};
use tuff_nbt::Compound;

pub const RAIN_DELAY: (i32, i32) = (12_000, 180_000);
pub const RAIN_DURATION: (i32, i32) = (12_000, 24_000);
pub const THUNDER_DELAY: (i32, i32) = (12_000, 180_000);
pub const THUNDER_DURATION: (i32, i32) = (3_600, 15_600);
const LEVEL_STEP: f32 = 0.01;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Weather {
    pub clear_time: i32,
    pub rain_time: i32,
    pub thunder_time: i32,
    pub raining: bool,
    pub thundering: bool,
    /// The strength clients are shown, rising and falling 0.01 per tick.
    pub rain_level: f32,
    pub thunder_level: f32,
}

impl Default for Weather {
    fn default() -> Self {
        Self {
            clear_time: 0,
            rain_time: 0,
            thunder_time: 0,
            raining: false,
            thundering: false,
            rain_level: 0.0,
            thunder_level: 0.0,
        }
    }
}

/// Packets that carry a weather change to clients.
#[derive(Debug, Clone, PartialEq)]
pub struct WeatherUpdate {
    pub events: Vec<play::GameEvent>,
}

fn sample(range: (i32, i32), rng: &mut impl Rng) -> i32 {
    rng.gen_range(range.0..=range.1)
}

impl Weather {
    pub fn load(saved: &Compound) -> Self {
        let raining = saved.get_bool("raining").unwrap_or(false);
        let thundering = saved.get_bool("thundering").unwrap_or(false);
        Self {
            clear_time: saved.get_int("clear_weather_time").unwrap_or(0),
            rain_time: saved.get_int("rain_time").unwrap_or(0),
            thunder_time: saved.get_int("thunder_time").unwrap_or(0),
            raining,
            thundering,
            rain_level: if raining { 1.0 } else { 0.0 },
            thunder_level: if raining && thundering { 1.0 } else { 0.0 },
        }
    }

    pub fn save(&self) -> Compound {
        Compound::new()
            .with("clear_weather_time", self.clear_time)
            .with("rain_time", self.rain_time)
            .with("thunder_time", self.thunder_time)
            .with("raining", self.raining)
            .with("thundering", self.thundering)
    }

    /// What `/weather clear|rain|thunder` sets; `duration` is `None` for
    /// the game's random one.
    pub fn set(&mut self, kind: WeatherKind, duration: Option<i32>, rng: &mut impl Rng) {
        let (clear, rain, raining, thundering) = match kind {
            WeatherKind::Clear => (
                duration.unwrap_or_else(|| sample(RAIN_DELAY, rng)),
                0,
                false,
                false,
            ),
            WeatherKind::Rain => (
                0,
                duration.unwrap_or_else(|| sample(RAIN_DURATION, rng)),
                true,
                false,
            ),
            WeatherKind::Thunder => (
                0,
                duration.unwrap_or_else(|| sample(THUNDER_DURATION, rng)),
                true,
                true,
            ),
        };
        self.clear_time = clear;
        self.rain_time = rain;
        self.thunder_time = rain;
        self.raining = raining;
        self.thundering = thundering;
    }

    /// Advances one tick; `advance` is the `advance_weather` rule. Returns
    /// the packets everyone should get, if anything visible changed.
    pub fn tick(&mut self, advance: bool, rng: &mut impl Rng) -> Option<WeatherUpdate> {
        let was_raining = self.is_raining();
        if advance {
            if self.clear_time > 0 {
                self.clear_time -= 1;
                self.thunder_time = if self.thundering { 0 } else { 1 };
                self.rain_time = if self.raining { 0 } else { 1 };
                self.thundering = false;
                self.raining = false;
            } else {
                if self.thunder_time > 0 {
                    self.thunder_time -= 1;
                    if self.thunder_time == 0 {
                        self.thundering = !self.thundering;
                    }
                } else {
                    self.thunder_time = sample(
                        if self.thundering {
                            THUNDER_DURATION
                        } else {
                            THUNDER_DELAY
                        },
                        rng,
                    );
                }
                if self.rain_time > 0 {
                    self.rain_time -= 1;
                    if self.rain_time == 0 {
                        self.raining = !self.raining;
                    }
                } else {
                    self.rain_time = sample(
                        if self.raining {
                            RAIN_DURATION
                        } else {
                            RAIN_DELAY
                        },
                        rng,
                    );
                }
            }
        }
        let old_thunder = self.thunder_level;
        let old_rain = self.rain_level;
        let step = |level: f32, up: bool| {
            (level + if up { LEVEL_STEP } else { -LEVEL_STEP }).clamp(0.0, 1.0)
        };
        self.thunder_level = step(self.thunder_level, self.thundering);
        self.rain_level = step(self.rain_level, self.raining);
        let mut events = Vec::new();
        if old_rain != self.rain_level {
            events.push(play::GameEvent {
                event: game_event::RAIN_LEVEL_CHANGE,
                value: self.rain_level,
            });
        }
        if old_thunder != self.thunder_level {
            events.push(play::GameEvent {
                event: game_event::THUNDER_LEVEL_CHANGE,
                value: self.thunder_level,
            });
        }
        if was_raining != self.is_raining() {
            events.push(play::GameEvent {
                event: if was_raining {
                    game_event::STOP_RAINING
                } else {
                    game_event::START_RAINING
                },
                value: 0.0,
            });
            events.push(play::GameEvent {
                event: game_event::RAIN_LEVEL_CHANGE,
                value: self.rain_level,
            });
            events.push(play::GameEvent {
                event: game_event::THUNDER_LEVEL_CHANGE,
                value: self.thunder_level,
            });
        }
        (!events.is_empty()).then_some(WeatherUpdate { events })
    }

    /// Stops rain and thunder with their timers run out, as sleeping
    /// through a storm does; the next tick picks new durations.
    pub fn reset_cycle(&mut self) {
        self.rain_time = 0;
        self.raining = false;
        self.thunder_time = 0;
        self.thundering = false;
    }

    /// Whether clients see rain: the shown level is above zero.
    pub fn is_raining(&self) -> bool {
        self.rain_level > 0.2
    }

    pub fn is_thundering(&self) -> bool {
        self.is_raining() && self.thunder_level > 0.9
    }

    /// The packets a joining client needs to show the current weather.
    pub fn join_events(&self) -> Vec<play::GameEvent> {
        let mut events = Vec::new();
        if self.is_raining() {
            events.push(play::GameEvent {
                event: game_event::START_RAINING,
                value: 0.0,
            });
            events.push(play::GameEvent {
                event: game_event::RAIN_LEVEL_CHANGE,
                value: self.rain_level,
            });
            events.push(play::GameEvent {
                event: game_event::THUNDER_LEVEL_CHANGE,
                value: self.thunder_level,
            });
        }
        events
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeatherKind {
    Clear,
    Rain,
    Thunder,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[tuff_core::watchdog]
    #[test]
    fn set_and_tick_follow_the_games_cycle() {
        let mut rng = StdRng::seed_from_u64(3);
        let mut weather = Weather::default();
        weather.set(WeatherKind::Rain, Some(30), &mut rng);
        assert!(weather.raining && !weather.thundering);
        assert_eq!(
            (weather.rain_time, weather.thunder_time, weather.clear_time),
            (30, 30, 0)
        );
        let first = weather.tick(true, &mut rng).unwrap();
        assert_eq!(
            first.events.len(),
            1,
            "the level rises before rain is shown"
        );
        assert_eq!(first.events[0].event, game_event::RAIN_LEVEL_CHANGE);
        let mut started = None;
        for tick in 2..=29 {
            if let Some(update) = weather.tick(true, &mut rng)
                && update
                    .events
                    .iter()
                    .any(|e| e.event == game_event::START_RAINING)
            {
                started = Some(tick);
            }
        }
        assert_eq!(started, Some(20), "rain shows once the level passes 0.2");
        assert!(weather.raining);
        weather.tick(true, &mut rng);
        assert!(!weather.raining, "the timer flips the state at zero");
        assert_eq!(weather.rain_time, 0);
        weather.tick(true, &mut rng);
        assert!(weather.rain_time >= RAIN_DELAY.0 - 1 && weather.rain_time <= RAIN_DELAY.1);
        let mut stopped = false;
        for _ in 0..120 {
            if let Some(update) = weather.tick(true, &mut rng) {
                stopped |= update
                    .events
                    .iter()
                    .any(|e| e.event == game_event::STOP_RAINING);
            }
        }
        assert!(stopped);
        assert_eq!(weather.rain_level, 0.0);
        assert!(weather.tick(true, &mut rng).is_none());

        weather.set(WeatherKind::Thunder, None, &mut rng);
        assert!(weather.raining && weather.thundering);
        assert!(weather.rain_time >= THUNDER_DURATION.0 && weather.rain_time <= THUNDER_DURATION.1);
        weather.set(WeatherKind::Clear, Some(100), &mut rng);
        assert_eq!(weather.clear_time, 100);
        assert!(!weather.raining);
        let mut frozen = weather;
        frozen.tick(false, &mut rng);
        assert_eq!(
            frozen.clear_time, 100,
            "advance_weather off keeps the timers"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn saved_data_round_trips_and_sets_levels() {
        let saved = Compound::new()
            .with("clear_weather_time", 0)
            .with("rain_time", 500)
            .with("thunder_time", 700)
            .with("raining", true)
            .with("thundering", true);
        let weather = Weather::load(&saved);
        assert_eq!(weather.rain_level, 1.0);
        assert_eq!(weather.thunder_level, 1.0);
        assert!(weather.is_thundering());
        assert_eq!(weather.save(), saved);
        assert_eq!(weather.join_events().len(), 3);
        assert!(Weather::default().join_events().is_empty());
    }
}
