use std::time::Instant;

use tuff_core::protocol::play::{PlayerActionStatus, Serverbound};

use super::budget::TickClock;
use super::{Check, Facts, Fix, Flag};
use crate::violations::Bucket;

type Cost = fn(&Serverbound) -> Option<f32>;

/// Events of one kind against a leaky bucket. An excess event is
/// cancelled; `resync` also resends the inventory.
struct Rate {
    name: &'static str,
    bucket: Bucket,
    cost: Cost,
    resync: bool,
    weak: f64,
    /// One click may use the main hand and then the off hand; the off
    /// hand's packet in the tick of a counted main-hand one is free.
    paired: bool,
    main_hand_at: Option<u64>,
}

impl Check for Rate {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let cost = (self.cost)(packet)?;
        if self.paired {
            match off_hand(packet) {
                Some(false) => self.main_hand_at = Some(facts.tick),
                Some(true) if self.main_hand_at == Some(facts.tick) => {
                    self.main_hand_at = None;
                    return None;
                }
                _ => {}
            }
        }
        if self.bucket.allow(cost, facts.tick) {
            if let Some(weight) = crate::improbable::weight_of(self.name) {
                self.weak = crate::improbable::weak_weight(self.bucket.fill(), 1.0, weight * 0.25);
            }
            return None;
        }
        Some(Flag {
            check: self.name,
            weight: 1.0,
            fix: if self.resync {
                Fix::ResyncInventory
            } else {
                Fix::Cancel
            },
        })
    }

    fn weak(&mut self) -> f64 {
        std::mem::take(&mut self.weak)
    }
}

const SPAM_COST: f32 = 20.0;
/// Right clicks a tick that placing and using may keep up: 12 a second,
/// above the 10 a fast clicker reaches.
const CLICK_DRAIN: f32 = 0.6;
const SPAM_CAP: f32 = 200.0;

fn rate(name: &'static str, cap: f32, drain: f32, cost: Cost, resync: bool) -> Box<dyn Check> {
    Box::new(Rate {
        name,
        bucket: Bucket::new(cap, drain),
        cost,
        resync,
        weak: 0.0,
        paired: false,
        main_hand_at: None,
    })
}

/// [`rate`] for packets a click sends once per hand.
fn per_click(name: &'static str, cap: f32, drain: f32, cost: Cost) -> Box<dyn Check> {
    Box::new(Rate {
        name,
        bucket: Bucket::new(cap, drain),
        cost,
        resync: false,
        weak: 0.0,
        paired: true,
        main_hand_at: None,
    })
}

fn off_hand(packet: &Serverbound) -> Option<bool> {
    match packet {
        Serverbound::UseItemOn { hand, .. } => Some(*hand == 1),
        Serverbound::UseItem { off_hand, .. } => Some(*off_hand),
        _ => None,
    }
}

fn action(packet: &Serverbound) -> Option<PlayerActionStatus> {
    match packet {
        Serverbound::PlayerAction { status, .. } => Some(*status),
        _ => None,
    }
}

/// Distinct entities attacked within one tick; a legitimate client hits
/// at most one thing per tick.
#[derive(Default)]
struct MultiTarget {
    tick: u64,
    seen: [i32; 4],
    count: usize,
}

impl Check for MultiTarget {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let Serverbound::Attack(entity) = packet else {
            return None;
        };
        if self.tick != facts.tick {
            self.tick = facts.tick;
            self.count = 0;
        }
        if self.seen[..self.count].contains(entity) {
            return None;
        }
        if self.count == self.seen.len() {
            return Flag::cancel("attack_rate", 2.0);
        }
        self.seen[self.count] = *entity;
        self.count += 1;
        if self.count > 2 {
            Flag::cancel("attack_rate", 2.0)
        } else {
            None
        }
    }
}

const SWINGS_PER_SECOND: f64 = 45.0;
const SWING_CREDIT: f64 = 90.0;

/// Punches against a real-time leaky bucket. The game's client punches
/// once a client tick while attack is held on a block and once per
/// click, so a click on a block punches twice in its tick; held attack
/// plus fast clicking stays under the rate. The credit covers clicking
/// bursts and the catch-up after a stall, and only the first overrun of
/// a server tick weighs, so one bunched burst cannot climb the ladder.
#[derive(Default)]
struct SwingRate {
    level: f64,
    at: Option<Instant>,
    clock: TickClock,
    flagged_at: Option<u64>,
}

impl Check for SwingRate {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        if !matches!(packet, Serverbound::Punch) {
            return None;
        }
        let now = self.clock.now(facts.now, facts.tick);
        let at = self.at.map_or(now, |last| last.max(now));
        let elapsed = self.at.map_or(0.0, |last| (at - last).as_secs_f64());
        self.at = Some(at);
        self.level = elapsed.mul_add(-SWINGS_PER_SECOND, self.level).max(0.0);
        if self.level + 1.0 <= SWING_CREDIT {
            self.level += 1.0;
            return None;
        }
        let first = self.flagged_at != Some(facts.tick);
        self.flagged_at = Some(facts.tick);
        Flag::cancel("swing_rate", if first { 1.0 } else { 0.0 })
    }
}

const CLICKS_PER_TICK: u32 = 12;

/// Container clicks within one server tick; a client sends a handful at
/// most (shift-click runs, number keys), never dozens. Creative is exempt.
#[derive(Default)]
struct ClickBurst {
    tick: u64,
    count: u32,
}

impl Check for ClickBurst {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        if facts.instabuild || !matches!(packet, Serverbound::ContainerClick { .. }) {
            return None;
        }
        if self.tick != facts.tick {
            self.tick = facts.tick;
            self.count = 0;
        }
        self.count += 1;
        (self.count > CLICKS_PER_TICK).then(|| Flag {
            check: "click_rate",
            weight: 1.0,
            fix: Fix::ResyncInventory,
        })
    }
}

pub fn rate_checks() -> Vec<Box<dyn Check>> {
    vec![
        rate(
            "attack_rate",
            10.0,
            0.75,
            |p| matches!(p, Serverbound::Attack(_)).then_some(1.0),
            false,
        ),
        Box::new(MultiTarget::default()),
        Box::new(SwingRate::default()),
        Box::new(ClickBurst::default()),
        rate(
            "click_rate",
            60.0,
            1.5,
            |p| matches!(p, Serverbound::ContainerClick { .. }).then_some(1.0),
            true,
        ),
        rate(
            "drop_rate",
            30.0,
            1.0,
            |p| {
                matches!(
                    action(p),
                    Some(PlayerActionStatus::DropItem | PlayerActionStatus::DropAllItems)
                )
                .then_some(1.0)
            },
            true,
        ),
        rate(
            "drop_rate",
            10.0,
            0.25,
            |p| matches!(action(p), Some(PlayerActionStatus::SwapItemWithOffhand)).then_some(1.0),
            true,
        ),
        rate(
            "hotbar_rate",
            24.0,
            1.0,
            |p| matches!(p, Serverbound::SetCarriedItem(_)).then_some(1.0),
            false,
        ),
        per_click("place_rate", 8.0, CLICK_DRAIN, |p| {
            matches!(p, Serverbound::UseItemOn { .. }).then_some(1.0)
        }),
        per_click("use_rate", 12.0, CLICK_DRAIN, |p| {
            matches!(p, Serverbound::UseItem { .. }).then_some(1.0)
        }),
        rate(
            "chat_rate",
            SPAM_CAP,
            1.0,
            |p| matches!(p, Serverbound::Chat(_)).then_some(SPAM_COST),
            false,
        ),
        rate(
            "command_rate",
            SPAM_CAP,
            1.0,
            |p| matches!(p, Serverbound::ChatCommand(_)).then_some(SPAM_COST),
            false,
        ),
        rate("flood", 500.0, 50.0, |_| Some(1.0), false),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;

    fn run(packet: &Serverbound, per_tick: &[(u64, usize)]) -> Vec<Flag> {
        let mut checks = rate_checks();
        let mut facts = facts();
        let mut flags = Vec::new();
        for (tick, count) in per_tick {
            facts.tick = *tick;
            for _ in 0..*count {
                for check in &mut checks {
                    flags.extend(check.examine(&facts, packet));
                }
            }
        }
        flags
    }

    fn names(flags: &[Flag]) -> Vec<&'static str> {
        flags.iter().map(|f| f.check).collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_normal_click_rate_and_a_lag_burst_pass() {
        let attack = Serverbound::Attack(7);
        let steady: Vec<(u64, usize)> = (0..200).map(|t| (t, usize::from(t % 3 == 0))).collect();
        assert!(run(&attack, &steady).is_empty());
        let burst = [(0, 0), (40, 6), (41, 1)];
        assert!(run(&attack, &burst).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_autoclicker_is_cancelled() {
        let attack = Serverbound::Attack(7);
        let every_tick: Vec<(u64, usize)> = (0..200).map(|t| (t, 2)).collect();
        let flags = run(&attack, &every_tick);
        assert!(!flags.is_empty());
        assert!(names(&flags).iter().all(|n| *n == "attack_rate"));
        assert!(flags.iter().all(|f| f.fix == Fix::Cancel));
    }

    fn weight(flags: &[Flag]) -> f64 {
        flags.iter().map(|f| f.weight).sum()
    }

    #[tuff_core::watchdog]
    #[test]
    fn held_attack_with_fast_clicks_on_a_block_never_flags() {
        let punches: Vec<(u64, usize)> = (0..2400)
            .map(|t| (t, if t % 400 < 60 { 3 } else { 2 }))
            .collect();
        assert!(run(&Serverbound::Punch, &punches).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_stall_catch_up_burst_weighs_at_most_one() {
        let mut punches: Vec<(u64, usize)> = (0..100).map(|t| (t, 2)).collect();
        punches.push((140, 80));
        punches.extend((141..200).map(|t| (t, 2)));
        assert!(run(&Serverbound::Punch, &punches).is_empty());
        punches.push((300, 200));
        punches.extend((301..400).map(|t| (t, 2)));
        let flags = run(&Serverbound::Punch, &punches);
        assert!(!flags.is_empty());
        assert!(names(&flags).iter().all(|n| *n == "swing_rate"));
        assert!(weight(&flags) <= 1.0, "{}", weight(&flags));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_swing_autoclicker_weighs_once_a_tick_after_its_credit() {
        let punches: Vec<(u64, usize)> = (0..200).map(|t| (t, 4)).collect();
        let flags = run(&Serverbound::Punch, &punches);
        assert!(flags.iter().all(|f| f.fix == Fix::Cancel));
        assert!(names(&flags).iter().all(|n| *n == "swing_rate"));
        let weighed = weight(&flags);
        assert!((130.0..=150.0).contains(&weighed), "{weighed}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn several_targets_in_one_tick_are_flagged() {
        let mut checks = rate_checks();
        let facts = facts();
        let mut flagged = 0;
        for entity in 1..=5 {
            for check in &mut checks {
                flagged += usize::from(
                    check
                        .examine(&facts, &Serverbound::Attack(entity))
                        .is_some(),
                );
            }
        }
        assert!(flagged >= 3);
    }

    #[tuff_core::watchdog]
    #[test]
    fn container_clicks_beyond_the_rate_resync_the_inventory() {
        let click = Serverbound::ContainerClick {
            window_id: 0,
            state_id: 0,
            slot: 9,
            button: 0,
            mode: 0,
            changed: Vec::new(),
            carried: None,
        };
        let flags = run(&click, &[(0, 100)]);
        assert!(!flags.is_empty());
        assert!(flags.iter().all(|f| f.fix == Fix::ResyncInventory));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_click_burst_in_one_tick_flags_unless_creative() {
        let click = Serverbound::ContainerClick {
            window_id: 0,
            state_id: 0,
            slot: 9,
            button: 0,
            mode: 1,
            changed: Vec::new(),
            carried: None,
        };
        let mut burst = ClickBurst::default();
        let mut facts = facts();
        let flagged = |burst: &mut ClickBurst, facts: &Facts, n: usize| {
            (0..n)
                .filter(|_| burst.examine(facts, &click).is_some())
                .count()
        };
        assert_eq!(flagged(&mut burst, &facts, 12), 0);
        assert_eq!(flagged(&mut burst, &facts, 3), 3);
        facts.tick = 1;
        assert_eq!(flagged(&mut burst, &facts, 12), 0);
        facts.tick = 2;
        facts.instabuild = true;
        assert_eq!(flagged(&mut burst, &facts, 50), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn chat_allows_the_vanilla_burst_then_one_a_second() {
        let chat = Serverbound::Chat("hi".into());
        assert!(run(&chat, &[(0, 10)]).is_empty());
        assert_eq!(run(&chat, &[(0, 11)]).len(), 1);
        let sustained: Vec<(u64, usize)> = (0..100).map(|t| (t * 20, 1)).collect();
        assert!(run(&chat, &sustained).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_packet_flood_is_cancelled() {
        let keep = Serverbound::KeepAlive(1);
        assert!(run(&keep, &[(0, 300), (1, 40)]).is_empty());
        assert!(!run(&keep, &[(0, 2000)]).is_empty());
    }

    fn place(off_hand: bool) -> Serverbound {
        Serverbound::UseItemOn {
            hand: i32::from(off_hand),
            position: tuff_world_model::BlockPos::new(0, 64, 0),
            face: tuff_world_model::Face::Up,
            cursor: [0.5, 1.0, 0.5],
            sequence: 0,
        }
    }

    fn use_item(off_hand: bool) -> Serverbound {
        Serverbound::UseItem {
            off_hand,
            sequence: 0,
        }
    }

    fn run_clicks(clicks: &[(u64, Vec<Serverbound>)]) -> Vec<Flag> {
        let mut checks = rate_checks();
        let mut facts = facts();
        let mut flags = Vec::new();
        for (tick, packets) in clicks {
            facts.tick = *tick;
            for packet in packets {
                for check in &mut checks {
                    flags.extend(check.examine(&facts, packet));
                }
            }
        }
        flags
    }

    /// A click every other client tick for a minute, each a placement or
    /// a miss, arriving in bunches of up to four ticks late.
    fn ten_cps(click: impl Fn(u64) -> Vec<Serverbound>) -> Vec<(u64, Vec<Serverbound>)> {
        (0..600u64)
            .filter(|t| t % 2 == 0)
            .map(|t| {
                let late = (t * 7 / 2) % 5;
                (t + late - late % 2, click(t))
            })
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_ten_cps_bridger_is_not_flagged_for_place_or_use_rate() {
        let clicks = ten_cps(|t| {
            if (t / 2) % 5 == 3 {
                vec![use_item(false)]
            } else {
                vec![place(false)]
            }
        });
        assert!(names(&run_clicks(&clicks)).is_empty());
        let placing = ten_cps(|_| vec![place(false)]);
        assert!(names(&run_clicks(&placing)).is_empty());
        let missing = ten_cps(|_| vec![use_item(false)]);
        assert!(names(&run_clicks(&missing)).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn missed_clicks_that_use_both_hands_are_not_flagged_for_use_rate() {
        let clicks = ten_cps(|_| vec![use_item(false), use_item(true)]);
        assert!(names(&run_clicks(&clicks)).is_empty());
        let failed = ten_cps(|_| vec![place(false), use_item(false), place(true), use_item(true)]);
        assert!(names(&run_clicks(&failed)).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn placing_or_using_every_tick_is_still_flagged() {
        let placer: Vec<(u64, Vec<Serverbound>)> =
            (0..200).map(|t| (t, vec![place(false)])).collect();
        let flags = names(&run_clicks(&placer));
        assert!(!flags.is_empty() && flags.iter().all(|n| *n == "place_rate"));
        let off_hands: Vec<(u64, Vec<Serverbound>)> =
            (0..200).map(|t| (t, vec![use_item(true)])).collect();
        assert!(names(&run_clicks(&off_hands)).contains(&"use_rate"));
        let doubled: Vec<(u64, Vec<Serverbound>)> = (0..200)
            .filter(|t| t % 2 == 0)
            .map(|t| (t, vec![use_item(false), use_item(true), use_item(true)]))
            .collect();
        assert!(names(&run_clicks(&doubled)).contains(&"use_rate"));
    }
}
