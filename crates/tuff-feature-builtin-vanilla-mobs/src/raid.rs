//! The raids a region knows of, as its mobs see them: where each is
//! centred, how far along it is, and which region runs it. The region that
//! owns a raid's centre runs it (`world::region::raids`) and copies this
//! view into itself and its neighbours, since raiders and villagers there
//! read it too.

use tuff_world_model::BlockPos;
use tuff_world_model::pos::RegionPos;

/// Blocks (squared) within which a raid reaches a mob.
pub const REACH_SQ: i64 = 96 * 96;

/// Ticks a copied raid lasts without a refresh from its home region.
const MIRROR_TICKS: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Preparing,
    BetweenWaves,
    Active,
    Victory,
    Loss,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Raid {
    pub id: u32,
    pub center: BlockPos,
    pub phase: Phase,
    /// The region running it, for reports from raiders elsewhere.
    pub home: Option<RegionPos>,
    pub omen_level: i32,
    pub groups_spawned: i32,
    /// Bit `n` is set while wave `n` has a living leader.
    pub leaders: u64,
    expires: Option<u64>,
}

impl Raid {
    pub fn new(id: u32, center: BlockPos, phase: Phase) -> Self {
        Self {
            id,
            center,
            phase,
            home: None,
            omen_level: 0,
            groups_spawned: 0,
            leaders: 0,
            expires: None,
        }
    }

    pub fn is_over(&self) -> bool {
        matches!(self.phase, Phase::Victory | Phase::Loss)
    }

    pub fn is_active(&self) -> bool {
        self.phase != Phase::Stopped
    }

    pub fn has_leader(&self, wave: i32) -> bool {
        (0..64).contains(&wave) && self.leaders & (1 << wave) != 0
    }
}

#[derive(Debug, Default)]
pub struct Raids {
    raids: Vec<Raid>,
    next: u32,
}

impl Raids {
    /// A raid known only to this region, which nothing refreshes.
    pub fn start(&mut self, center: BlockPos) -> u32 {
        self.next += 1;
        self.raids
            .push(Raid::new(self.next, center, Phase::Preparing));
        self.next
    }

    pub fn set_phase(&mut self, id: u32, phase: Phase) {
        if let Some(raid) = self.raids.iter_mut().find(|r| r.id == id) {
            raid.phase = phase;
        }
    }

    pub fn remove(&mut self, id: u32) {
        self.raids.retain(|r| r.id != id);
    }

    pub fn get(&self, id: u32) -> Option<Raid> {
        self.raids.iter().find(|r| r.id == id).copied()
    }

    /// Records the home region's latest view of a raid, kept until `now`
    /// plus a grace without a refresh.
    pub fn mirror(&mut self, raid: Raid, now: u64) {
        let raid = Raid {
            expires: Some(now + MIRROR_TICKS),
            ..raid
        };
        match self.raids.iter_mut().find(|r| r.id == raid.id) {
            Some(known) => *known = raid,
            None => self.raids.push(raid),
        }
    }

    /// Forgets copies their home has stopped refreshing.
    pub fn expire(&mut self, now: u64) {
        self.raids.retain(|r| r.expires.is_none_or(|at| at > now));
    }

    /// The closest active raid whose centre is within 96 blocks of `pos`.
    pub fn at(&self, pos: BlockPos) -> Option<Raid> {
        let distance = |r: &Raid| distance_sq(r.center, pos);
        self.raids
            .iter()
            .filter(|r| r.is_active() && distance(r) < REACH_SQ)
            .min_by_key(|r| distance(r))
            .copied()
    }
}

pub fn distance_sq(a: BlockPos, b: BlockPos) -> i64 {
    let dx = (a.x - b.x) as i64;
    let dy = (a.y - b.y) as i64;
    let dz = (a.z - b.z) as i64;
    dx * dx + dy * dy + dz * dz
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_raid_reaches_ninety_six_blocks_and_picks_the_closest() {
        let mut raids = Raids::default();
        let far = raids.start(BlockPos::new(0, 64, 0));
        let near = raids.start(BlockPos::new(50, 64, 0));
        assert_eq!(raids.at(BlockPos::new(40, 64, 0)).unwrap().id, near);
        assert_eq!(raids.at(BlockPos::new(-90, 64, 0)).unwrap().id, far);
        assert!(raids.at(BlockPos::new(-100, 64, 0)).is_none());
        raids.set_phase(near, Phase::Victory);
        assert_eq!(
            raids.at(BlockPos::new(40, 64, 0)).unwrap().phase,
            Phase::Victory
        );
        raids.remove(near);
        assert_eq!(raids.at(BlockPos::new(40, 64, 0)).unwrap().id, far);
    }

    #[tuff_core::watchdog]
    #[test]
    fn mirrored_raids_expire_without_a_refresh_and_stopped_ones_reach_no_one() {
        let mut raids = Raids::default();
        let mut raid = Raid::new(77, BlockPos::new(0, 64, 0), Phase::Active);
        raids.mirror(raid, 10);
        raids.expire(109);
        assert_eq!(raids.at(BlockPos::new(1, 64, 1)).unwrap().id, 77);
        raid.phase = Phase::Stopped;
        raids.mirror(raid, 50);
        assert!(raids.at(BlockPos::new(1, 64, 1)).is_none());
        assert!(raids.get(77).is_some());
        raids.expire(150);
        assert!(raids.get(77).is_none());
    }
}
