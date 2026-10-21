use super::*;

/// Health, food, experience, breath, burning, effects and attributes.
pub struct Body {
    pub health: Health,
    pub food: Food,
    pub experience: Experience,
    pub fall: Fall,
    /// Ticks of breath left under water.
    pub air: i32,
    pub ticks_frozen: i32,
    /// Experience points collected over the player's lives.
    pub score: i32,
    /// Where the player was when powder snow last checked for wading.
    pub wade_position: [f64; 3],
    pub stuck_arrows: crate::world::entity::StuckArrows,
    /// Ticks left burning; zero or less when not on fire.
    pub fire_ticks: i32,
    pub effects: ActiveEffects,
    pub attributes: attributes::AttributeOverrides,
    pub absorption: f32,
    pub combat: CombatTracker,
    /// The player who last hurt this one and the tick, for kill credit.
    pub kill_credit: Option<(String, u128, u64)>,
    /// The profile key the player signs chat with, once they send it.
    pub chat_session: Option<crate::protocol::chat_session::ChatSession>,
    pub shoulders: super::shoulders::Shoulders,
    pub chat_chain: Option<crate::protocol::chat_session::ChatChain>,
    pub last_seen: crate::protocol::chat_session::LastSeenValidator,
}

/// Whether the player is fighting, as the combat tracker decides: entered
/// when a living entity hurts them, left once no damage lands for 300
/// ticks (100 while not yet in combat).
#[derive(Debug, Default, Clone)]
pub struct CombatTracker {
    entries: Vec<crate::world::death::CombatEntry>,
    last_damage: u64,
    start: u64,
    end: u64,
    in_combat: bool,
    taking_damage: bool,
    last_attacker: Option<(i32, u64)>,
    retaliable: bool,
    last_victim: Option<(i32, u64)>,
}

impl CombatTracker {
    /// Remembers who hurt the player last and whether wolves answer it.
    pub fn record_attacker(&mut self, now: u64, attacker: Option<i32>, retaliable: bool) {
        if let Some(id) = attacker {
            self.last_attacker = Some((id, now));
        }
        self.retaliable = retaliable;
    }

    pub fn record_victim(&mut self, now: u64, victim: i32) {
        self.last_victim = Some((victim, now));
    }

    /// The attacker a tamed pet defends against: recent damage that wolves
    /// answer.
    pub fn defend_target(&self, now: u64) -> Option<(i32, u64)> {
        (self.retaliable && now.saturating_sub(self.last_damage) <= 100)
            .then_some(self.last_attacker)
            .flatten()
    }

    pub fn push(&mut self, entry: crate::world::death::CombatEntry) {
        self.entries.push(entry);
    }

    pub fn entries(&self) -> &[crate::world::death::CombatEntry] {
        &self.entries
    }

    pub fn clear_entries(&mut self) {
        self.entries.clear();
    }

    pub fn victim(&self) -> Option<(i32, u64)> {
        self.last_victim
    }

    pub fn in_combat(&self) -> bool {
        self.in_combat
    }

    /// Records a landed hit at `now` (after a `recheck`); true when it
    /// starts a fight.
    pub fn record_damage(&mut self, now: u64, alive: bool, by_living: bool) -> bool {
        self.last_damage = now;
        self.taking_damage = true;
        if !self.in_combat && alive && by_living {
            self.in_combat = true;
            self.start = now;
            self.end = now;
            return true;
        }
        false
    }

    /// Leaves combat once the reset window passed; the fight's length in
    /// ticks when it just ended.
    pub fn recheck(&mut self, now: u64, alive: bool) -> Option<u64> {
        let reset = if self.in_combat { 300 } else { 100 };
        if !self.taking_damage || (alive && now.saturating_sub(self.last_damage) <= reset) {
            return None;
        }
        let was_in_combat = self.in_combat;
        self.taking_damage = false;
        self.in_combat = false;
        self.end = now;
        self.entries.clear();
        was_in_combat.then(|| self.end.saturating_sub(self.start))
    }
}

impl Default for Body {
    fn default() -> Self {
        Self {
            health: Health::default(),
            food: Food::default(),
            experience: Experience::default(),
            fall: Fall::default(),
            air: MAX_AIR_SUPPLY,
            ticks_frozen: 0,
            score: 0,
            wade_position: [0.0; 3],
            stuck_arrows: Default::default(),
            fire_ticks: 0,
            effects: ActiveEffects::default(),
            attributes: Default::default(),
            absorption: 0.0,
            combat: CombatTracker::default(),
            kill_credit: None,
            chat_session: None,
            shoulders: Default::default(),
            chat_chain: None,
            last_seen: Default::default(),
        }
    }
}

#[cfg(test)]
mod combat_tests {
    use super::CombatTracker;

    #[tuff_core::watchdog]
    #[test]
    fn a_living_attacker_starts_a_fight_that_ends_after_three_hundred_quiet_ticks() {
        let mut tracker = CombatTracker::default();
        assert!(!tracker.record_damage(10, true, false));
        assert!(tracker.record_damage(20, true, true));
        assert!(tracker.in_combat());
        assert_eq!(tracker.recheck(320, true), None);
        assert_eq!(tracker.recheck(321, true), Some(301));
        assert!(!tracker.in_combat());
    }

    #[tuff_core::watchdog]
    #[test]
    fn death_ends_the_fight_at_once_and_non_living_damage_never_starts_one() {
        let mut tracker = CombatTracker::default();
        assert!(tracker.record_damage(0, true, true));
        assert_eq!(tracker.recheck(5, false), Some(5));
        assert!(!tracker.record_damage(6, true, false));
        assert!(!tracker.in_combat());
        assert_eq!(tracker.recheck(107, true), None);
    }
}
