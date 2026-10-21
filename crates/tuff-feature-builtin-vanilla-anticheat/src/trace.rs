//! Debug traces: what a check allowed, what the packet did and which
//! explanations widened the allowance. Checks keep their last trace in a
//! plain `Copy` value, so nothing allocates unless a trace is shown.

use std::fmt;

/// The explanations a check applied, as a small bit set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tags(u64);

const NAMES: &[&str] = &[
    "jump_envelope",
    "jump_boost",
    "bounce",
    "push",
    "grace",
    "slippery",
    "unknown_terrain",
    "speed_effect",
    "sneak",
    "item_resync",
    "use_momentum",
    "use_flat",
    "phase",
    "lost_ground",
    "predicted",
    "hidden_ticks",
    "edge_backoff",
    "step_up",
    "rebased",
    "sprint_resync",
    "sneak_resync",
    "climb",
    "stuck",
    "seeded",
    "crowded",
    "fluid",
    "glide",
    "impulse",
    "boost",
    "levitation",
    "entity_push",
    "swim",
    "jump_delay",
    "pushed_out",
];

impl Tags {
    pub const JUMP_ENVELOPE: Tags = Tags(1);
    pub const JUMP_BOOST: Tags = Tags(1 << 1);
    pub const BOUNCE: Tags = Tags(1 << 2);
    pub const PUSH: Tags = Tags(1 << 3);
    pub const GRACE: Tags = Tags(1 << 4);
    pub const SLIPPERY: Tags = Tags(1 << 5);
    pub const UNKNOWN_TERRAIN: Tags = Tags(1 << 6);
    pub const SPEED_EFFECT: Tags = Tags(1 << 7);
    pub const SNEAK: Tags = Tags(1 << 8);
    pub const ITEM_RESYNC: Tags = Tags(1 << 9);
    pub const USE_MOMENTUM: Tags = Tags(1 << 10);
    pub const USE_FLAT: Tags = Tags(1 << 11);
    pub const PHASE: Tags = Tags(1 << 12);
    pub const LOST_GROUND: Tags = Tags(1 << 13);
    pub const PREDICTED: Tags = Tags(1 << 14);
    pub const HIDDEN_TICKS: Tags = Tags(1 << 15);
    pub const EDGE_BACKOFF: Tags = Tags(1 << 16);
    pub const STEP_UP: Tags = Tags(1 << 17);
    pub const REBASED: Tags = Tags(1 << 18);
    pub const SPRINT_RESYNC: Tags = Tags(1 << 19);
    pub const SNEAK_RESYNC: Tags = Tags(1 << 20);
    pub const CLIMB: Tags = Tags(1 << 21);
    pub const STUCK: Tags = Tags(1 << 22);
    pub const SEEDED: Tags = Tags(1 << 23);
    pub const CROWDED: Tags = Tags(1 << 24);
    pub const FLUID: Tags = Tags(1 << 25);
    pub const GLIDE: Tags = Tags(1 << 26);
    pub const IMPULSE: Tags = Tags(1 << 27);
    pub const BOOST: Tags = Tags(1 << 28);
    pub const LEVITATION: Tags = Tags(1 << 29);
    pub const ENTITY_PUSH: Tags = Tags(1 << 30);
    pub const SWIM: Tags = Tags(1 << 31);
    pub const JUMP_DELAY: Tags = Tags(1 << 32);
    pub const PUSHED_OUT: Tags = Tags(1 << 33);

    pub const fn empty() -> Tags {
        Tags(0)
    }

    pub fn contains(self, other: Tags) -> bool {
        self.0 & other.0 == other.0
    }

    /// `other` added when `on`.
    #[must_use]
    pub fn with(self, other: Tags, on: bool) -> Tags {
        if on { Tags(self.0 | other.0) } else { self }
    }

    #[must_use]
    pub fn union(self, other: Tags) -> Tags {
        Tags(self.0 | other.0)
    }

    pub fn names(self) -> impl Iterator<Item = &'static str> {
        NAMES
            .iter()
            .enumerate()
            .filter(move |(bit, _)| self.0 & (1u64 << bit) != 0)
            .map(|(_, name)| *name)
    }
}

impl fmt::Display for Tags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (n, name) in self.names().enumerate() {
            if n > 0 {
                f.write_str(",")?;
            }
            f.write_str(name)?;
        }
        Ok(())
    }
}

/// One check's verdict on one packet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Trace {
    pub check: &'static str,
    pub allowed: f64,
    pub actual: f64,
    pub tags: Tags,
}

impl Trace {
    pub fn new(check: &'static str, allowed: f64, actual: f64, tags: Tags) -> Option<Trace> {
        Some(Trace {
            check,
            allowed,
            actual,
            tags,
        })
    }
}

impl fmt::Display for Trace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} allowed {:.4} actual {:.4} [{}]",
            self.check, self.allowed, self.actual, self.tags
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn tags_name_every_bit_and_format() {
        assert_eq!(NAMES.len(), 34);
        assert_eq!(Tags::REBASED.names().collect::<Vec<_>>(), ["rebased"]);
        assert_eq!(Tags::CROWDED.names().collect::<Vec<_>>(), ["crowded"]);
        assert_eq!(Tags::FLUID.names().collect::<Vec<_>>(), ["fluid"]);
        assert_eq!(
            Tags::ENTITY_PUSH.names().collect::<Vec<_>>(),
            ["entity_push"]
        );
        let tags = Tags::empty()
            .with(Tags::JUMP_ENVELOPE, true)
            .with(Tags::PUSH, true)
            .with(Tags::GRACE, false);
        assert!(tags.contains(Tags::PUSH));
        assert!(!tags.contains(Tags::GRACE));
        assert_eq!(tags.to_string(), "jump_envelope,push");
        let trace = Trace::new("fly", 0.45, 0.5, tags).unwrap();
        assert_eq!(
            trace.to_string(),
            "fly allowed 0.4500 actual 0.5000 [jump_envelope,push]"
        );
        assert_eq!(
            Tags::LOST_GROUND.names().collect::<Vec<_>>(),
            ["lost_ground"]
        );
    }
}
