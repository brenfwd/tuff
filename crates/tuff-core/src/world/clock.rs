//! World clocks: the `minecraft:world_clock` registry's independent times
//! (the overworld's day cycle, the End's), each with its own rate and pause
//! state. Timelines give a clock's time meaning, including named time
//! markers such as `minecraft:noon`.

use tuff_vanilla_data::data_registry::WORLD_CLOCK;
use tuff_vanilla_data::timeline::{self, Timeline};

use crate::protocol::play::{self, ClockTime};
use crate::world::messages::ClockOutcome;
use tuff_nbt::{Compound, Tag};

/// Index of a clock in the `minecraft:world_clock` registry, which is also
/// its network ID.
pub type ClockId = usize;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockState {
    pub total_ticks: i64,
    /// Accumulated fraction of a tick, from rates that are not whole.
    pub partial_tick: f32,
    /// Clock ticks per game tick.
    pub rate: f32,
    pub paused: bool,
}

impl Default for ClockState {
    fn default() -> Self {
        Self {
            total_ticks: 0,
            partial_tick: 0.0,
            rate: 1.0,
            paused: false,
        }
    }
}

impl ClockState {
    fn tick(&mut self) {
        if self.paused {
            return;
        }
        self.partial_tick += self.rate;
        let whole = self.partial_tick.floor();
        self.partial_tick -= whole;
        self.total_ticks += whole as i64;
    }

    /// Moves to an absolute time, dropping any partial tick.
    pub fn set(&mut self, ticks: i64) {
        self.total_ticks = ticks;
        self.partial_tick = 0.0;
    }

    fn load(saved: &Compound, fallback_ticks: i64) -> Self {
        let default = Self::default();
        Self {
            total_ticks: saved.get_long("total_ticks").unwrap_or(fallback_ticks),
            partial_tick: saved.get_float("partial_tick").unwrap_or(0.0),
            rate: saved
                .get_float("rate")
                .filter(|rate| rate.is_finite() && *rate > 0.0)
                .unwrap_or(default.rate),
            paused: saved.get_bool("paused").unwrap_or(default.paused),
        }
    }

    /// Writes the state over `saved`, omitting fields at their default like
    /// the game does and keeping any fields this server does not know.
    fn save_into(&self, saved: &mut Compound) {
        let default = Self::default();
        saved.insert("total_ticks", self.total_ticks);
        let optional = [
            (
                "partial_tick",
                self.partial_tick != default.partial_tick,
                Tag::Float(self.partial_tick),
            ),
            ("rate", self.rate != default.rate, Tag::Float(self.rate)),
            (
                "paused",
                self.paused != default.paused,
                Tag::from(self.paused),
            ),
        ];
        for (key, set, value) in optional {
            if set {
                saved.insert(key, value);
            } else {
                saved.remove(key);
            }
        }
    }
}

/// A named point on a clock, repeating with its timeline's period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeMarker {
    ticks: i64,
    period: Option<i64>,
}

impl TimeMarker {
    /// The marker `name` on `clock`. A marker defined by several timelines
    /// of the same clock is taken from the last in registry order.
    pub fn find(clock: ClockId, name: &str) -> Option<Self> {
        let clock = WORLD_CLOCK.entries[clock];
        timelines_of(clock)
            .filter_map(|timeline| {
                let marker = timeline.time_markers.iter().find(|m| m.name == name)?;
                Some(Self {
                    ticks: marker.ticks.into(),
                    period: timeline.period_ticks.map(i64::from),
                })
            })
            .last()
    }

    pub fn occurs_at(self, ticks: i64) -> bool {
        match self.period {
            Some(period) => self.ticks == ticks % period,
            None => self.ticks == ticks,
        }
    }

    /// The next time at or after `ticks` that is this marker, never staying
    /// put: a clock already at the marker moves a whole period ahead.
    pub fn next_after(self, ticks: i64) -> i64 {
        let Some(period) = self.period else {
            return self.ticks;
        };
        let ahead = self.ticks - ticks % period;
        ticks + if ahead > 0 { ahead } else { period + ahead }
    }
}

pub fn timelines_of(clock: &str) -> impl Iterator<Item = &'static Timeline> + '_ {
    timeline::ALL
        .iter()
        .map(|(_, timeline)| *timeline)
        .filter(move |timeline| timeline.clock == clock)
}

/// Markers `/time set` suggests for `clock`.
pub fn command_markers(clock: ClockId) -> impl Iterator<Item = &'static str> {
    timelines_of(WORLD_CLOCK.entries[clock])
        .flat_map(|timeline| timeline.time_markers)
        .filter(|marker| marker.show_in_commands)
        .map(|marker| marker.name)
}

pub fn clock_id(name: &str) -> Option<ClockId> {
    WORLD_CLOCK.entries.binary_search(&name).ok()
}

pub fn clock_name(clock: ClockId) -> &'static str {
    WORLD_CLOCK.entries[clock]
}

/// The state of every registered clock, indexed by [`ClockId`].
#[derive(Debug, Clone, PartialEq)]
pub struct WorldClocks(Vec<ClockState>, bool);

impl Default for WorldClocks {
    fn default() -> Self {
        Self(vec![ClockState::default(); WORLD_CLOCK.entries.len()], true)
    }
}

impl WorldClocks {
    /// Reads the `world_clocks` saved data. A clock missing from it starts
    /// at `fallback_ticks`.
    pub fn load(saved: &Compound, fallback_ticks: i64) -> Self {
        Self(
            WORLD_CLOCK
                .entries
                .iter()
                .map(|name| match saved.get_compound(name) {
                    Some(clock) => ClockState::load(clock, fallback_ticks),
                    None => ClockState {
                        total_ticks: fallback_ticks,
                        ..ClockState::default()
                    },
                })
                .collect(),
            true,
        )
    }

    /// Mirrors the `advance_time` rule: while off, clocks stand still and
    /// sync with a zero rate.
    pub fn set_advancing(&mut self, advance: bool) {
        self.1 = advance;
    }

    /// Writes every clock into the `world_clocks` saved data, preserving
    /// entries and fields this server does not know.
    pub fn save_into(&self, saved: &mut Compound) {
        for (name, state) in WORLD_CLOCK.entries.iter().zip(&self.0) {
            if let Some(clock) = saved.get_compound_mut(name) {
                state.save_into(clock);
            } else {
                let mut clock = Compound::new();
                state.save_into(&mut clock);
                saved.insert(*name, clock);
            }
        }
    }

    pub fn get(&self, clock: ClockId) -> &ClockState {
        &self.0[clock]
    }

    pub fn get_mut(&mut self, clock: ClockId) -> &mut ClockState {
        &mut self.0[clock]
    }

    pub fn tick(&mut self) {
        if !self.1 {
            return;
        }
        for clock in &mut self.0 {
            clock.tick();
        }
    }

    /// Update Time for every clock, or just `only`.
    pub fn packet(&self, world_age: i64, only: Option<ClockId>) -> play::SetTime {
        let clocks = self
            .0
            .iter()
            .enumerate()
            .filter(|(id, _)| only.is_none_or(|only| only == *id))
            .map(|(id, state)| ClockTime {
                clock: id as i32,
                total_ticks: state.total_ticks,
                partial_tick: state.partial_tick,
                rate: if state.paused || !self.1 {
                    0.0
                } else {
                    state.rate
                },
            })
            .collect();
        play::SetTime { world_age, clocks }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overworld() -> ClockId {
        clock_id("minecraft:overworld").unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn fractional_rates_accumulate_partial_ticks() {
        let mut clock = ClockState {
            rate: 0.25,
            ..ClockState::default()
        };
        for _ in 0..3 {
            clock.tick();
        }
        assert_eq!((clock.total_ticks, clock.partial_tick), (0, 0.75));
        clock.tick();
        assert_eq!((clock.total_ticks, clock.partial_tick), (1, 0.0));
        clock.rate = 2.5;
        clock.tick();
        assert_eq!((clock.total_ticks, clock.partial_tick), (3, 0.5));
        clock.paused = true;
        clock.tick();
        assert_eq!(clock.total_ticks, 3);
    }

    #[tuff_core::watchdog]
    #[test]
    fn day_markers_repeat_every_day() {
        let noon = TimeMarker::find(overworld(), "minecraft:noon").unwrap();
        assert!(noon.occurs_at(6000));
        assert!(noon.occurs_at(30_000));
        assert!(!noon.occurs_at(6001));
        assert_eq!(noon.next_after(0), 6000);
        assert_eq!(noon.next_after(5999), 6000);
        assert_eq!(noon.next_after(6000), 30_000);
        assert_eq!(noon.next_after(6001), 30_000);
        assert_eq!(noon.next_after(25_000), 30_000);
        assert!(TimeMarker::find(overworld(), "minecraft:wake_up_from_sleep").is_some());
        assert_eq!(TimeMarker::find(overworld(), "minecraft:nope"), None);
        let end = clock_id("minecraft:the_end").unwrap();
        assert_eq!(TimeMarker::find(end, "minecraft:noon"), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_command_markers_are_suggested() {
        let mut markers: Vec<_> = command_markers(overworld()).collect();
        markers.sort();
        assert_eq!(
            markers,
            [
                "minecraft:day",
                "minecraft:midnight",
                "minecraft:night",
                "minecraft:noon"
            ]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn saved_state_round_trips_and_omits_defaults() {
        let mut clocks = WorldClocks::default();
        *clocks.get_mut(overworld()) = ClockState {
            total_ticks: 12_345,
            partial_tick: 0.5,
            rate: 3.0,
            paused: true,
        };
        let mut saved = Compound::new().with(
            "minecraft:overworld",
            Compound::new().with("extra", 1i32).with("rate", 9.0f32),
        );
        clocks.save_into(&mut saved);
        assert_eq!(WorldClocks::load(&saved, 0), clocks);
        let overworld_saved = saved.get_compound("minecraft:overworld").unwrap();
        assert_eq!(overworld_saved.get_int("extra"), Some(1));

        clocks.get_mut(overworld()).rate = 1.0;
        clocks.get_mut(overworld()).paused = false;
        clocks.save_into(&mut saved);
        let overworld_saved = saved.get_compound("minecraft:overworld").unwrap();
        assert!(!overworld_saved.contains_key("rate"));
        assert!(!overworld_saved.contains_key("paused"));
        let end = saved.get_compound("minecraft:the_end").unwrap();
        assert_eq!(end.keys().collect::<Vec<_>>(), ["total_ticks"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn missing_clocks_start_at_the_fallback_and_bad_rates_reset() {
        let saved = Compound::new().with(
            "minecraft:overworld",
            Compound::new()
                .with("total_ticks", 5i64)
                .with("rate", -1.0f32),
        );
        let clocks = WorldClocks::load(&saved, 77);
        assert_eq!(clocks.get(overworld()).total_ticks, 5);
        assert_eq!(clocks.get(overworld()).rate, 1.0);
        let end = clock_id("minecraft:the_end").unwrap();
        assert_eq!(clocks.get(end).total_ticks, 77);
    }

    #[tuff_core::watchdog]
    #[test]
    fn advance_time_off_freezes_and_zeroes_rate() {
        let mut clocks = WorldClocks::default();
        clocks.set_advancing(false);
        clocks.tick();
        assert_eq!(clocks.get(overworld()).total_ticks, 0);
        assert_eq!(clocks.packet(0, Some(overworld())).clocks[0].rate, 0.0);
        clocks.set_advancing(true);
        clocks.tick();
        assert_eq!(clocks.get(overworld()).total_ticks, 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn paused_clocks_sync_with_zero_rate() {
        let mut clocks = WorldClocks::default();
        clocks.get_mut(overworld()).paused = true;
        clocks.get_mut(overworld()).rate = 4.0;
        let packet = clocks.packet(9, Some(overworld()));
        assert_eq!(packet.world_age, 9);
        assert_eq!(packet.clocks.len(), 1);
        assert_eq!(packet.clocks[0].clock, overworld() as i32);
        assert_eq!(packet.clocks[0].rate, 0.0);
        assert_eq!(
            clocks.packet(0, None).clocks.len(),
            WORLD_CLOCK.entries.len()
        );
    }
}

/// A `/time` subcommand on one clock, run by the coordinator.
#[derive(Debug, Clone, PartialEq)]
pub enum ClockAction {
    Query,
    QueryTimeline {
        timeline: &'static Timeline,
        repetitions: bool,
    },
    Add(i32),
    Set(i32),
    /// A time marker's resource location.
    SetMarker(String),
    Pause,
    Resume,
    Rate(f32),
}

/// Runs a clock subcommand, returning what it did and whether the clock
/// changed.
pub fn run_clock(
    clocks: &mut WorldClocks,
    clock: ClockId,
    action: &ClockAction,
) -> (ClockOutcome, bool) {
    let name = clock_name(clock);
    let state = clocks.get_mut(clock);
    match action {
        ClockAction::Query => (
            ClockOutcome::Query {
                name,
                total_ticks: state.total_ticks,
            },
            false,
        ),
        ClockAction::QueryTimeline {
            timeline,
            repetitions,
        } => {
            if timeline.clock != name {
                return (
                    ClockOutcome::WrongTimeline {
                        timeline: timeline.name,
                        name,
                    },
                    false,
                );
            }
            let period = timeline.period_ticks.map(i64::from);
            let value = if *repetitions {
                period.map_or(0, |period| i64::from((state.total_ticks / period) as i32))
            } else {
                period.map_or(state.total_ticks, |period| state.total_ticks % period)
            };
            (
                ClockOutcome::QueryTimeline {
                    timeline: timeline.name,
                    value,
                    repetitions: *repetitions,
                },
                false,
            )
        }
        ClockAction::Add(ticks) => {
            state.total_ticks = (state.total_ticks + i64::from(*ticks)).max(0);
            (
                ClockOutcome::Set {
                    name,
                    total_ticks: state.total_ticks,
                },
                true,
            )
        }
        ClockAction::Set(ticks) => {
            if state.total_ticks == i64::from(*ticks) {
                return (
                    ClockOutcome::AlreadyAtTime {
                        name,
                        ticks: *ticks,
                    },
                    false,
                );
            }
            state.set((*ticks).into());
            (
                ClockOutcome::Set {
                    name,
                    total_ticks: i64::from(*ticks),
                },
                true,
            )
        }
        ClockAction::SetMarker(marker) => {
            let Some(found) = TimeMarker::find(clock, marker) else {
                return (
                    ClockOutcome::NoTimeMarker {
                        marker: marker.clone(),
                        name,
                    },
                    false,
                );
            };
            if found.occurs_at(state.total_ticks) {
                return (
                    ClockOutcome::AlreadyAtMarker {
                        name,
                        marker: marker.clone(),
                    },
                    false,
                );
            }
            state.set(found.next_after(state.total_ticks));
            (
                ClockOutcome::SetMarker {
                    name,
                    marker: marker.clone(),
                },
                true,
            )
        }
        ClockAction::Pause | ClockAction::Resume => {
            let paused = *action == ClockAction::Pause;
            if state.paused == paused {
                return (ClockOutcome::AlreadyPaused { name, paused }, false);
            }
            state.paused = paused;
            (ClockOutcome::Paused { name, paused }, true)
        }
        ClockAction::Rate(rate) => {
            if state.rate == *rate {
                return (ClockOutcome::AlreadyRate { name, rate: *rate }, false);
            }
            state.rate = *rate;
            (ClockOutcome::Rate { name, rate: *rate }, true)
        }
    }
}
