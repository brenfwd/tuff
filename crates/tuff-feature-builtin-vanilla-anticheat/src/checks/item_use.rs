use tuff_core::protocol::play::{PlayerActionStatus, Serverbound};
use tuff_vanilla_data::Item;

use super::{Check, Facts, Flag};
use crate::violations::Bucket;

const BOW_MIN_DRAW: u32 = 3;
const TRIDENT_MIN_DRAW: u32 = 10;
const USE_ATTACK_GRACE: u32 = 3;
const MENU_ATTACK_GRACE: u64 = 10;
const MS_PER_TICK: i32 = 50;

/// The server's view of an item use in progress.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemUse {
    pub item: Item,
    /// Ticks since the use started.
    pub elapsed: u32,
    /// Ticks left, or `None` for an item held until released.
    pub remaining: Option<u32>,
}

/// The item-use timeline as the server counts it: releasing a bow or
/// trident before it can fire, releasing faster than uses can start, and
/// attacking while an item is in use or a menu is open. Consumption itself
/// is counted down by the server, so the client cannot shorten it.
pub struct ItemUseTimeline {
    short_draws: Bucket,
    releases: Bucket,
    menu_since: Option<u64>,
}

impl Default for ItemUseTimeline {
    fn default() -> Self {
        Self {
            short_draws: Bucket::new(3.0, 0.05),
            releases: Bucket::new(6.0, 0.35),
            menu_since: None,
        }
    }
}

fn min_draw(item: Item) -> Option<u32> {
    match item {
        Item::Bow => Some(BOW_MIN_DRAW),
        Item::Trident => Some(TRIDENT_MIN_DRAW),
        _ => None,
    }
}

impl Check for ItemUseTimeline {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        self.menu_since = match (facts.menu, self.menu_since) {
            (None, _) => None,
            (Some(_), None) => Some(facts.tick),
            (Some(_), since) => since,
        };
        match packet {
            Serverbound::PlayerAction {
                status: PlayerActionStatus::ReleaseUseItem,
                ..
            } => {
                if !self.releases.allow(1.0, facts.tick) {
                    return Flag::cancel("release_rate", 1.0);
                }
                let use_ = facts.item_use?;
                let min = min_draw(use_.item)?;
                if use_.elapsed < min && !self.short_draws.allow(1.0, facts.tick) {
                    return Flag::cancel("fast_bow", 1.0);
                }
                None
            }
            Serverbound::Attack(_) => {
                if let Some(since) = self.menu_since {
                    let lag = (facts.latency_ms.max(0) / MS_PER_TICK) as u64;
                    if facts.tick.saturating_sub(since) > MENU_ATTACK_GRACE + lag {
                        return Flag::cancel("container_attack", 1.0);
                    }
                }
                let use_ = facts.item_use?;
                let finishing = use_.remaining.is_some_and(|r| r <= USE_ATTACK_GRACE);
                if use_.elapsed >= USE_ATTACK_GRACE && !finishing {
                    return Flag::cancel("use_attack", 1.0);
                }
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;
    use tuff_world_model::BlockPos;

    fn release() -> Serverbound {
        Serverbound::PlayerAction {
            status: PlayerActionStatus::ReleaseUseItem,
            position: BlockPos::new(0, 0, 0),
            face: 0,
            sequence: 0,
        }
    }

    fn using(item: Item, elapsed: u32, remaining: Option<u32>) -> Facts {
        let mut f = facts();
        f.using = true;
        f.item_use = Some(ItemUse {
            item,
            elapsed,
            remaining,
        });
        f
    }

    #[tuff_core::watchdog]
    #[test]
    fn full_draws_never_flag_but_instant_bow_spam_does() {
        let mut check = ItemUseTimeline::default();
        for n in 0..50 {
            let mut f = using(Item::Bow, 20, None);
            f.tick = n * 25;
            assert_eq!(check.examine(&f, &release()), None);
        }
        let mut check = ItemUseTimeline::default();
        let flagged = (0..10)
            .filter(|n| {
                let mut f = using(Item::Bow, 0, None);
                f.tick = n * 4;
                check.examine(&f, &release()).is_some()
            })
            .count();
        assert!(flagged >= 5, "{flagged}");
        let mut taps = ItemUseTimeline::default();
        let mut f = using(Item::Bow, 1, None);
        f.tick = 100;
        assert_eq!(taps.examine(&f, &release()), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn short_trident_and_crossbow_releases() {
        let mut check = ItemUseTimeline::default();
        let flagged = (0..16)
            .filter(|n| {
                let mut f = using(Item::Trident, 6, None);
                f.tick = n * 8;
                check.examine(&f, &release()).is_some()
            })
            .count();
        assert!(flagged >= 4);
        let mut crossbow = ItemUseTimeline::default();
        assert!((0..8).all(|n| {
            let mut f = using(Item::Crossbow, 2, None);
            f.tick = n * 8;
            crossbow.examine(&f, &release()).is_none()
        }));
    }

    #[tuff_core::watchdog]
    #[test]
    fn release_floods_are_rate_limited() {
        let mut check = ItemUseTimeline::default();
        let f = facts();
        let flagged = (0..20)
            .filter_map(|_| check.examine(&f, &release()))
            .filter(|flag| flag.check == "release_rate")
            .count();
        assert!(flagged > 10);
    }

    #[tuff_core::watchdog]
    #[test]
    fn attacking_while_using_flags_except_at_the_edges() {
        let mut check = ItemUseTimeline::default();
        let attack = Serverbound::Attack(3);
        assert_eq!(
            check
                .examine(&using(Item::Shield, 10, None), &attack)
                .map(|f| f.check),
            Some("use_attack")
        );
        assert!(
            check
                .examine(&using(Item::CookedBeef, 20, Some(12)), &attack)
                .is_some()
        );
        assert_eq!(
            check.examine(&using(Item::CookedBeef, 1, Some(31)), &attack),
            None
        );
        assert_eq!(
            check.examine(&using(Item::CookedBeef, 30, Some(2)), &attack),
            None
        );
        assert_eq!(check.examine(&facts(), &attack), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn attacking_with_a_menu_open_flags_after_a_grace() {
        let mut check = ItemUseTimeline::default();
        let attack = Serverbound::Attack(3);
        let mut f = facts();
        f.menu = Some((1, 27));
        f.tick = 100;
        assert_eq!(check.examine(&f, &attack), None);
        f.tick = 105;
        assert_eq!(check.examine(&f, &attack), None);
        f.tick = 120;
        assert_eq!(
            check.examine(&f, &attack).map(|f| f.check),
            Some("container_attack")
        );
        f.menu = None;
        assert_eq!(check.examine(&f, &attack), None);
        f.menu = Some((2, 27));
        f.tick = 125;
        assert_eq!(check.examine(&f, &attack), None);
    }
}
