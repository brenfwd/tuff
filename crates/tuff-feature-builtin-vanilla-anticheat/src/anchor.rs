//! Where a movement setback puts a player. The last accepted position is
//! fine while it stands on ground or in a safe medium; a player set back
//! in mid-air instead goes to the last accepted position that was safe,
//! so a setback never drops anyone into a pit, the void or a hazard, and
//! a height that was reached without a legitimate way up is undone.

use tuff_core::world::dimension::DimensionId;

use crate::checks::Terrain;

/// A safe anchor farther than this from the setback position is stale
/// (a long fall, a launch) and the plain setback is used instead.
const MAX_DISTANCE: f64 = 32.0;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Candidate {
    position: [f64; 3],
    safe: bool,
}

#[derive(Debug, Default)]
pub struct Anchors {
    dimension: Option<DimensionId>,
    /// The session's accepted position as of the last packet.
    seen: Option<[f64; 3]>,
    /// Whether `seen` itself is a safe place to stand.
    seen_safe: bool,
    /// The last accepted position on ground or in a safe medium, or the
    /// latest one in a flying-like state.
    safe: Option<[f64; 3]>,
    /// The last move the checks let through, kept until the session
    /// shows whether it accepted it.
    candidate: Option<Candidate>,
}

/// Whether a position with this footing is safe to put a player back on.
pub fn safe(terrain: Option<Terrain>, flying: bool) -> bool {
    flying
        || terrain.is_some_and(|t| t.known && !t.hazard && !t.phased && (t.underfoot || t.medium))
}

impl Anchors {
    /// Brings the anchors up to the session's accepted position: the last
    /// passed move becomes an anchor if the session took it; any other
    /// change (a teleport, a respawn, a push, another dimension) leaves no
    /// anchor until the player stands somewhere safe again.
    pub fn observe(&mut self, position: [f64; 3], dimension: DimensionId) {
        if self.dimension != Some(dimension) {
            *self = Anchors {
                dimension: Some(dimension),
                seen: Some(position),
                ..Anchors::default()
            };
            return;
        }
        match self.candidate.take() {
            Some(candidate) if candidate.position == position => {
                self.seen_safe = candidate.safe;
                if candidate.safe {
                    self.safe = Some(position);
                }
            }
            _ if self.seen != Some(position) => {
                self.seen_safe = false;
                self.safe = None;
            }
            _ => {}
        }
        self.seen = Some(position);
    }

    /// The checks let a move to `to` through.
    pub fn passed(&mut self, to: [f64; 3], safe: bool) {
        self.candidate = Some(Candidate { position: to, safe });
    }

    /// Where to put back a player whose last accepted position is `from`.
    pub fn setback(&self, from: [f64; 3], flying: bool) -> [f64; 3] {
        if flying || (self.seen == Some(from) && self.seen_safe) {
            return from;
        }
        match self.safe {
            Some(anchor) if distance(anchor, from) <= MAX_DISTANCE => anchor,
            _ => from,
        }
    }

    /// The player was put back at `to`.
    pub fn set_back_to(&mut self, to: [f64; 3]) {
        self.candidate = None;
        self.seen_safe = self.safe == Some(to) || (self.seen == Some(to) && self.seen_safe);
        self.seen = Some(to);
    }
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    const OVERWORLD: DimensionId = DimensionId::for_tests(0);

    fn ground() -> Option<Terrain> {
        Some(Terrain {
            known: true,
            supported: true,
            underfoot: true,
            ..Terrain::default()
        })
    }

    fn air() -> Option<Terrain> {
        Some(Terrain {
            known: true,
            ..Terrain::default()
        })
    }

    fn walk(anchors: &mut Anchors, to: [f64; 3], terrain: Option<Terrain>) {
        anchors.passed(to, safe(terrain, false));
        anchors.observe(to, OVERWORLD);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_setback_in_mid_air_returns_to_the_last_ground() {
        let mut anchors = Anchors::default();
        anchors.observe([0.0, 64.0, 0.0], OVERWORLD);
        walk(&mut anchors, [0.5, 64.0, 0.0], ground());
        walk(&mut anchors, [1.0, 64.0, 0.0], ground());
        walk(&mut anchors, [1.5, 64.4, 0.0], air());
        walk(&mut anchors, [2.0, 64.6, 0.0], air());
        assert_eq!(anchors.setback([2.0, 64.6, 0.0], false), [1.0, 64.0, 0.0]);
        assert_eq!(anchors.setback([2.0, 64.6, 0.0], true), [2.0, 64.6, 0.0]);
        walk(&mut anchors, [2.5, 64.0, 0.0], ground());
        assert_eq!(anchors.setback([2.5, 64.0, 0.0], false), [2.5, 64.0, 0.0]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn water_and_climbables_are_anchors_but_hazards_and_rejected_moves_are_not() {
        let mut anchors = Anchors::default();
        anchors.observe([0.0, 64.0, 0.0], OVERWORLD);
        let water = Some(Terrain {
            medium: true,
            ..air().unwrap()
        });
        walk(&mut anchors, [0.0, 63.0, 0.0], water);
        let lava = Some(Terrain {
            hazard: true,
            ..ground().unwrap()
        });
        walk(&mut anchors, [0.0, 62.0, 0.0], lava);
        walk(&mut anchors, [0.0, 61.0, 0.0], air());
        assert_eq!(anchors.setback([0.0, 61.0, 0.0], false), [0.0, 63.0, 0.0]);
        anchors.passed([0.0, 60.0, 0.0], true);
        anchors.observe([0.0, 61.0, 0.0], OVERWORLD);
        assert_eq!(anchors.setback([0.0, 61.0, 0.0], false), [0.0, 63.0, 0.0]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn teleports_dimensions_and_distant_anchors_fall_back_to_the_plain_setback() {
        let mut anchors = Anchors::default();
        anchors.observe([0.0, 64.0, 0.0], OVERWORLD);
        walk(&mut anchors, [0.0, 64.0, 0.0], ground());
        walk(&mut anchors, [0.0, 64.0, 0.5], ground());
        anchors.observe([500.0, 90.0, 0.0], OVERWORLD);
        assert_eq!(
            anchors.setback([500.0, 90.0, 0.0], false),
            [500.0, 90.0, 0.0]
        );

        let mut anchors = Anchors::default();
        anchors.observe([0.0, 64.0, 0.0], OVERWORLD);
        walk(&mut anchors, [0.0, 64.0, 0.5], ground());
        anchors.observe([0.0, 64.0, 0.5], DimensionId::for_tests(1));
        assert_eq!(anchors.setback([0.0, 64.0, 0.5], false), [0.0, 64.0, 0.5]);

        let mut anchors = Anchors::default();
        anchors.observe([0.0, 64.0, 0.0], OVERWORLD);
        walk(&mut anchors, [0.0, 64.0, 0.5], ground());
        walk(&mut anchors, [0.0, 20.0, 0.5], air());
        assert_eq!(anchors.setback([0.0, 20.0, 0.5], false), [0.0, 20.0, 0.5]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn flying_like_states_anchor_at_the_latest_position() {
        let mut anchors = Anchors::default();
        anchors.observe([0.0, 64.0, 0.0], OVERWORLD);
        walk(&mut anchors, [0.0, 64.0, 0.0], ground());
        anchors.passed([0.0, 80.0, 0.0], safe(air(), true));
        anchors.observe([0.0, 80.0, 0.0], OVERWORLD);
        anchors.passed([0.0, 81.0, 0.0], safe(air(), true));
        anchors.observe([0.0, 81.0, 0.0], OVERWORLD);
        walk(&mut anchors, [0.0, 80.5, 0.0], air());
        assert_eq!(anchors.setback([0.0, 80.5, 0.0], false), [0.0, 81.0, 0.0]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_setback_target_is_remembered() {
        let mut anchors = Anchors::default();
        anchors.observe([0.0, 64.0, 0.0], OVERWORLD);
        walk(&mut anchors, [0.0, 64.0, 0.5], ground());
        walk(&mut anchors, [0.0, 65.0, 0.5], air());
        let to = anchors.setback([0.0, 65.0, 0.5], false);
        anchors.set_back_to(to);
        anchors.observe(to, OVERWORLD);
        assert_eq!(anchors.setback(to, false), to);
        assert_eq!(anchors.safe, Some([0.0, 64.0, 0.5]));
    }
}
