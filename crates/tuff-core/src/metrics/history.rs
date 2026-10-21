//! In-memory history: the last [`HISTORY_SECONDS`] samples of each series
//! and per-minute rollups of the last [`HISTORY_MINUTES`] minutes.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use serde::Serialize;

pub const HISTORY_SECONDS: usize = 600;
pub const HISTORY_MINUTES: usize = 1440;
const MINUTE_MS: u64 = 60_000;

/// One sample, or one minute's rollup: the mean and the largest value.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Point {
    pub unix_ms: u64,
    pub value: f64,
    pub max: f64,
}

#[derive(Default)]
struct Rollup {
    minute: u64,
    sum: f64,
    max: f64,
    count: u32,
}

impl Rollup {
    fn point(&self) -> Point {
        Point {
            unix_ms: self.minute * MINUTE_MS,
            value: self.sum / f64::from(self.count.max(1)),
            max: self.max,
        }
    }
}

#[derive(Default)]
struct Rings {
    seconds: VecDeque<Point>,
    minutes: VecDeque<Point>,
    open: Option<Rollup>,
}

impl Rings {
    fn push(&mut self, unix_ms: u64, value: f64) {
        if self.seconds.len() == HISTORY_SECONDS {
            self.seconds.pop_front();
        }
        self.seconds.push_back(Point {
            unix_ms,
            value,
            max: value,
        });
        let minute = unix_ms / MINUTE_MS;
        match &mut self.open {
            Some(open) if open.minute == minute => {
                open.sum += value;
                open.max = open.max.max(value);
                open.count += 1;
            }
            open => {
                if let Some(done) = open.take() {
                    if self.minutes.len() == HISTORY_MINUTES {
                        self.minutes.pop_front();
                    }
                    self.minutes.push_back(done.point());
                }
                *open = Some(Rollup {
                    minute,
                    sum: value,
                    max: value,
                    count: 1,
                });
            }
        }
    }

    fn range(&self, now_ms: u64, range: Duration) -> Vec<Point> {
        let since = now_ms.saturating_sub(range.as_millis() as u64);
        if range <= Duration::from_secs(HISTORY_SECONDS as u64) {
            return self
                .seconds
                .iter()
                .filter(|p| p.unix_ms >= since)
                .copied()
                .collect();
        }
        let since_minute = since / MINUTE_MS * MINUTE_MS;
        self.minutes
            .iter()
            .copied()
            .chain(self.open.as_ref().map(Rollup::point))
            .filter(|p| p.unix_ms >= since_minute)
            .collect()
    }
}

#[derive(Default)]
pub struct History(HashMap<String, Rings>);

impl History {
    pub fn push(&mut self, series: &str, unix_ms: u64, value: f64) {
        match self.0.get_mut(series) {
            Some(rings) => rings.push(unix_ms, value),
            None => {
                let mut rings = Rings::default();
                rings.push(unix_ms, value);
                self.0.insert(series.to_owned(), rings);
            }
        }
    }

    pub fn range(&self, series: &str, now_ms: u64, range: Duration) -> Option<Vec<Point>> {
        Some(self.0.get(series)?.range(now_ms, range))
    }
}
