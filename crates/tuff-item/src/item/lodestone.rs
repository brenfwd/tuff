//! The `lodestone_tracker` component: the lodestone a compass points to.

use super::hash::Canon;
use tuff_nbt::{Compound, Tag};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LodestoneTracker {
    /// Dimension key and block position of the lodestone.
    pub target: Option<(String, [i32; 3])>,
    /// Whether the compass drops its target once the lodestone is gone.
    pub tracked: bool,
}

impl LodestoneTracker {
    pub fn locked(dimension: &str, pos: [i32; 3]) -> Self {
        Self {
            target: Some((dimension.to_owned(), pos)),
            tracked: true,
        }
    }

    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        let nbt = tag.as_compound()?;
        let target = match nbt.get("target") {
            None => None,
            Some(target) => {
                let target = target.as_compound()?;
                let pos = target.get_int_array("pos").filter(|p| p.len() == 3)?;
                Some((
                    target.get_str("dimension")?.to_owned(),
                    [pos[0], pos[1], pos[2]],
                ))
            }
        };
        let tracked = match nbt.get("tracked") {
            None => true,
            Some(tracked) => tracked.as_bool()?,
        };
        Some(Self { target, tracked })
    }

    pub fn to_nbt(&self) -> Tag {
        let mut nbt = Compound::new();
        if let Some((dimension, pos)) = &self.target {
            nbt.insert(
                "target",
                Compound::new()
                    .with("dimension", dimension.as_str())
                    .with("pos", pos.to_vec()),
            );
        }
        if !self.tracked {
            nbt.insert("tracked", false);
        }
        Tag::Compound(nbt)
    }

    pub fn canon(&self) -> Canon {
        let mut fields = Vec::new();
        if let Some((dimension, pos)) = &self.target {
            fields.push((
                "target".to_owned(),
                Canon::Map(vec![
                    ("dimension".to_owned(), Canon::str(dimension.as_str())),
                    ("pos".to_owned(), Canon::Ints(pos.to_vec())),
                ]),
            ));
        }
        if !self.tracked {
            fields.push(("tracked".to_owned(), Canon::Bool(false)));
        }
        Canon::Map(fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn tracker_round_trips_through_nbt_and_omits_the_default() {
        let tracker = LodestoneTracker::locked("minecraft:the_nether", [1, -2, 3]);
        let nbt = tracker.to_nbt();
        assert!(nbt.as_compound().unwrap().get("tracked").is_none());
        assert_eq!(LodestoneTracker::from_nbt(&nbt), Some(tracker));
        let lost = LodestoneTracker {
            target: None,
            tracked: false,
        };
        assert_eq!(LodestoneTracker::from_nbt(&lost.to_nbt()), Some(lost));
    }
}
