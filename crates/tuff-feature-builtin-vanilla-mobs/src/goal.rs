//! Goal-driven AI: every mob has a list of goals with priorities, each
//! claiming some control flags (moving, looking, jumping, targeting). Each
//! AI step stops goals that no longer apply, starts idle goals that may
//! take their flags from lower-priority ones, and ticks the running ones.

use std::fmt::Debug;

use super::MobCore;
use tuff_core::world::entity::TickContext;

/// The controls a goal takes over while it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags(u8);

impl Flags {
    pub const NONE: Self = Self(0);
    pub const MOVE: Self = Self(1);
    pub const LOOK: Self = Self(2);
    pub const JUMP: Self = Self(4);
    pub const TARGET: Self = Self(8);
    const COUNT: usize = 4;

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    fn indices(self) -> impl Iterator<Item = usize> {
        (0..Self::COUNT).filter(move |i| self.0 & (1 << i) != 0)
    }
}

impl std::ops::BitOr for Flags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

/// One behaviour a mob may pursue.
pub trait Goal: std::any::Any + Send + Debug {
    fn flags(&self) -> Flags;

    /// The name the goal selector debug feed shows: the type's name,
    /// ending in `Goal`.
    fn debug_name(&self) -> String {
        let full = std::any::type_name::<Self>();
        let path = full.split('<').next().unwrap_or(full);
        let name = path.rsplit("::").next().unwrap_or(path);
        if name.ends_with("Goal") {
            name.to_owned()
        } else {
            format!("{name}Goal")
        }
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool;

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.can_use(mob, cx)
    }

    fn interruptable(&self) -> bool {
        true
    }

    fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {}

    fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {}

    /// Whether it ticks on every AI step rather than every other one.
    fn every_tick(&self) -> bool {
        false
    }

    fn tick(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {}

    /// A delay in ticks for a goal that only ticks every other step.
    fn adjusted_delay(&self, ticks: i32) -> i32 {
        if self.every_tick() {
            ticks
        } else {
            reduced_delay(ticks)
        }
    }
}

/// Half of `ticks`, rounded up: goals ticked every other step count time
/// in steps.
pub fn reduced_delay(ticks: i32) -> i32 {
    (ticks + 1).div_euclid(2)
}

#[derive(Debug)]
struct Entry {
    priority: u8,
    goal: Box<dyn Goal>,
    running: bool,
}

#[derive(Debug, Default)]
pub struct GoalSelector {
    goals: Vec<Entry>,
    /// The goal holding each flag, by index into `goals`.
    locked: [Option<usize>; Flags::COUNT],
    disabled: Flags,
}

impl GoalSelector {
    pub fn add(&mut self, priority: u8, goal: impl Goal + 'static) {
        self.goals.push(Entry {
            priority,
            goal: Box::new(goal),
            running: false,
        });
    }

    pub fn set_disabled(&mut self, flags: Flags, disabled: bool) {
        if disabled {
            self.disabled = self.disabled | flags;
        } else {
            self.disabled = Flags(self.disabled.0 & !flags.0);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.goals.is_empty()
    }

    /// The first goal of type `G`.
    pub fn find<G: Goal>(&self) -> Option<&G> {
        self.goals.iter().find_map(|entry| {
            let goal: &dyn std::any::Any = &*entry.goal;
            goal.downcast_ref::<G>()
        })
    }

    pub fn find_mut<G: Goal>(&mut self) -> Option<&mut G> {
        self.goals.iter_mut().find_map(|entry| {
            let goal: &mut dyn std::any::Any = &mut *entry.goal;
            goal.downcast_mut::<G>()
        })
    }

    /// Each goal's priority, whether it runs and its name, for the goal
    /// selector debug feed.
    pub fn debug_goals(&self) -> Vec<tuff_core::protocol::play::DebugGoal> {
        self.goals
            .iter()
            .map(|entry| tuff_core::protocol::play::DebugGoal {
                priority: i32::from(entry.priority),
                running: entry.running,
                name: entry.goal.debug_name(),
            })
            .collect()
    }

    /// Whether the goal at `index` runs; for tests and debugging.
    pub fn running(&self) -> Vec<usize> {
        (0..self.goals.len())
            .filter(|i| self.goals[*i].running)
            .collect()
    }

    fn replaceable_by(&self, index: usize) -> bool {
        let entry = &self.goals[index];
        entry
            .goal
            .flags()
            .indices()
            .all(|flag| match self.locked[flag] {
                None => true,
                Some(holder) => {
                    let holder = &self.goals[holder];
                    !holder.running
                        || (holder.goal.interruptable() && entry.priority < holder.priority)
                }
            })
    }

    fn stop(&mut self, index: usize, mob: &mut MobCore, cx: &mut TickContext) {
        let entry = &mut self.goals[index];
        if entry.running {
            entry.running = false;
            entry.goal.stop(mob, cx);
        }
    }

    /// A full AI step: cleanup, then starting goals, then ticking all the
    /// running ones.
    pub fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        for index in 0..self.goals.len() {
            let entry = &mut self.goals[index];
            if entry.running
                && (entry.goal.flags().intersects(self.disabled)
                    || !entry.goal.can_continue(mob, cx))
            {
                self.stop(index, mob, cx);
            }
        }
        for slot in &mut self.locked {
            if slot.is_some_and(|holder| !self.goals[holder].running) {
                *slot = None;
            }
        }
        for index in 0..self.goals.len() {
            let entry = &self.goals[index];
            if entry.running || entry.goal.flags().intersects(self.disabled) {
                continue;
            }
            if !self.replaceable_by(index) || !self.goals[index].goal.can_use(mob, cx) {
                continue;
            }
            for flag in self.goals[index].goal.flags().indices() {
                if let Some(holder) = self.locked[flag] {
                    self.stop(holder, mob, cx);
                }
                self.locked[flag] = Some(index);
            }
            let entry = &mut self.goals[index];
            entry.running = true;
            entry.goal.start(mob, cx);
        }
        self.tick_running(true, mob, cx);
    }

    /// Ticks running goals; on the steps between full ones only those that
    /// want every tick.
    pub fn tick_running(&mut self, all: bool, mob: &mut MobCore, cx: &mut TickContext) {
        for entry in &mut self.goals {
            if entry.running && (all || entry.goal.every_tick()) {
                entry.goal.tick(mob, cx);
            }
        }
    }

    /// Stops every running goal.
    pub fn stop_all(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        for index in 0..self.goals.len() {
            self.stop(index, mob, cx);
        }
        self.locked = [None; Flags::COUNT];
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::tests::with_context;

    /// A goal that records its life and wants to run while `wanted`.
    #[derive(Debug)]
    struct Probe {
        name: &'static str,
        flags: Flags,
        wanted: Arc<Mutex<bool>>,
        log: Arc<Mutex<Vec<String>>>,
        interruptable: bool,
    }

    impl Goal for Probe {
        fn flags(&self) -> Flags {
            self.flags
        }

        fn can_use(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
            *self.wanted.lock().unwrap()
        }

        fn interruptable(&self) -> bool {
            self.interruptable
        }

        fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
            self.log
                .lock()
                .unwrap()
                .push(format!("start {}", self.name));
        }

        fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
            self.log.lock().unwrap().push(format!("stop {}", self.name));
        }
    }

    fn probe(
        name: &'static str,
        flags: Flags,
        log: &Arc<Mutex<Vec<String>>>,
        interruptable: bool,
    ) -> (Probe, Arc<Mutex<bool>>) {
        let wanted = Arc::new(Mutex::new(false));
        (
            Probe {
                name,
                flags,
                wanted: wanted.clone(),
                log: log.clone(),
                interruptable,
            },
            wanted,
        )
    }

    #[tuff_core::watchdog]
    #[test]
    fn higher_priorities_take_flags_and_disjoint_goals_run_together() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (wander, wander_on) = probe("wander", Flags::MOVE, &log, true);
        let (panic, panic_on) = probe("panic", Flags::MOVE, &log, true);
        let (look, look_on) = probe("look", Flags::LOOK, &log, true);
        let mut goals = GoalSelector::default();
        goals.add(1, panic);
        goals.add(5, wander);
        goals.add(6, look);
        with_context(|mob, cx| {
            *wander_on.lock().unwrap() = true;
            *look_on.lock().unwrap() = true;
            goals.tick(mob, cx);
            assert_eq!(goals.running(), [1, 2]);
            *panic_on.lock().unwrap() = true;
            goals.tick(mob, cx);
            assert_eq!(goals.running(), [0, 2], "panic took the move flag");
            *panic_on.lock().unwrap() = false;
            goals.tick(mob, cx);
            assert_eq!(goals.running(), [1, 2], "wandering resumes");
            goals.set_disabled(Flags::LOOK, true);
            goals.tick(mob, cx);
            assert_eq!(goals.running(), [1], "disabled flags stop goals");
        });
        assert_eq!(
            *log.lock().unwrap(),
            [
                "start wander",
                "start look",
                "stop wander",
                "start panic",
                "stop panic",
                "start wander",
                "stop look"
            ]
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn uninterruptable_goals_keep_their_flags() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (eat, eat_on) = probe("eat", Flags::MOVE | Flags::JUMP, &log, false);
        let (flee, flee_on) = probe("flee", Flags::MOVE, &log, true);
        let mut goals = GoalSelector::default();
        goals.add(5, eat);
        goals.add(1, flee);
        with_context(|mob, cx| {
            *eat_on.lock().unwrap() = true;
            goals.tick(mob, cx);
            *flee_on.lock().unwrap() = true;
            goals.tick(mob, cx);
            assert_eq!(goals.running(), [0]);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn delays_halve_for_goals_ticked_every_other_step() {
        assert_eq!(reduced_delay(100), 50);
        assert_eq!(reduced_delay(1), 1);
        assert_eq!(reduced_delay(0), 0);
    }
}
