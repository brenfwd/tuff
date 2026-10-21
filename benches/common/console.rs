//! Reads the plain English text both servers answer console commands
//! with: tuff renders its components against `en_us`, vanilla answers RCON
//! in the same words, so one parser serves both.

// Shared by several bench targets, each of which uses only part of it.
#![allow(dead_code)]

use serde_json::{Value, json};

/// What `/tick query` reports about the last 100 ticks.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TickQuery {
    pub sprinting: bool,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub sample: u64,
}

impl TickQuery {
    pub fn to_json(self) -> Value {
        json!({
            "mean_ms": self.mean_ms,
            "p50_ms": self.p50_ms,
            "p95_ms": self.p95_ms,
            "p99_ms": self.p99_ms,
            "sample": self.sample,
        })
    }
}

pub fn sprinting(text: &str) -> bool {
    text.contains("The game is sprinting")
}

pub fn tick_query(text: &str) -> Option<TickQuery> {
    let ms = |label: &str| -> Option<f64> {
        let rest = &text[text.find(label)? + label.len()..];
        rest.trim_start().split("ms").next()?.trim().parse().ok()
    };
    let sample = text[text.find("Sample:")? + "Sample:".len()..]
        .split(|c: char| !c.is_ascii_digit() && c != ' ')
        .next()?
        .trim()
        .parse()
        .ok()?;
    Some(TickQuery {
        sprinting: sprinting(text),
        mean_ms: ms("Average time per tick:")?,
        p50_ms: ms("P50:")?,
        p95_ms: ms("P95:")?,
        p99_ms: ms("P99:")?,
        sample,
    })
}

/// The x and z of a `/locate` answer: `... is at [X, ~, Z] ...`.
pub fn located(text: &str) -> Option<(i32, i32)> {
    let rest = &text[text.find("is at [")? + "is at [".len()..];
    let inner = rest.split(']').next()?;
    let mut parts = inner.split(',').map(str::trim);
    let x = parts.next()?.parse().ok()?;
    parts.next()?;
    let z = parts.next()?.parse().ok()?;
    Some((x, z))
}

/// The count from `/execute if entity ...`: `Test passed. Count: N`, or 0
/// for `Test failed`.
pub fn entity_count(text: &str) -> Option<u64> {
    if text.contains("Test failed") {
        return Some(0);
    }
    let lower = text.to_ascii_lowercase();
    if let Some(at) = lower.find("count:") {
        return lower[at + "count:".len()..]
            .split(|c: char| !c.is_ascii_digit() && c != ' ')
            .next()?
            .trim()
            .parse()
            .ok();
    }
    text.contains("Test passed").then_some(1)
}

/// The names in a `/list` answer.
pub fn player_list(text: &str) -> Option<Vec<String>> {
    let rest = &text[text.find("players online:")? + "players online:".len()..];
    Some(
        rest.split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

pub fn summoned(text: &str) -> bool {
    text.contains("Summoned new")
}

pub fn player_missing(text: &str) -> bool {
    text.contains("No player was found") || text.contains("No entity was found")
}

pub fn opped(text: &str) -> bool {
    text.contains("a server operator") || text.contains("already is an operator")
}

/// Chunk holders in a performance profile's per-level `chunks.csv`, and
/// how many of them hold a fully generated, accessible chunk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChunkRows {
    pub holders: u64,
    pub full: u64,
}

impl ChunkRows {
    pub fn add(&mut self, csv: &str) {
        let mut lines = csv.lines();
        let Some(header) = lines.next() else {
            return;
        };
        let column = header.split(',').position(|name| name == "full_status");
        for line in lines.filter(|line| !line.trim().is_empty()) {
            self.holders += 1;
            let status = column.and_then(|at| line.split(',').nth(at));
            if matches!(status, Some("FULL" | "BLOCK_TICKING" | "ENTITY_TICKING")) {
                self.full += 1;
            }
        }
    }
}
