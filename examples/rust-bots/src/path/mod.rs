// SPDX-License-Identifier: MIT
//! Baritone-style pathing over block positions: costs in ticks, pluggable
//! move kinds, goals with admissible heuristics and a node-budgeted A*
//! that resumes across ticks.

mod goal;
mod moves;
mod planner;
mod search;
#[cfg(test)]
mod tests;

pub use goal::{heuristic, reached};
pub use moves::{
    Ascend, Climb, Descend, Diagonal, Parkour, Pillar, Traverse, break_cost, default_moves,
    fall_ticks, parkour_gap,
};
pub use planner::{Advance, Planner};
pub use search::{Limits, Plan, Search, Status};

use std::cell::RefCell;
use std::rc::Rc;

use tuff_bots_api::control::{Aabb, Policy};

use crate::world::{Blocks, Pending, name_of};

pub type Pos = (i32, i32, i32);

pub const WALK_TICKS: f64 = 20.0 / 4.317;
pub const SPRINT_TICKS: f64 = 20.0 / 5.612;
pub const SWIM_TICKS: f64 = 20.0 / 2.2;
/// Per step with the head under water, steering plans toward the surface.
pub const SUBMERGED_TICKS: f64 = 6.0;
pub const CLIMB_UP_TICKS: f64 = 20.0 / 2.35;
pub const CLIMB_DOWN_TICKS: f64 = 20.0 / 3.0;
pub const ASCEND_TICKS: f64 = 7.0;
pub const PARKOUR_JUMP_TICKS: f64 = 2.0;
pub const DANGER_NEIGHBOR_TICKS: f64 = 8.0;
pub const FALL_DAMAGE_TICKS_PER_BLOCK: f64 = 10.0;
pub const SAFE_FALL: u32 = 3;
pub const MAX_WATER_FALL: u32 = 32;
/// Blocking blocks one step may break.
pub const MAX_BREAKS_PER_STEP: usize = 2;
/// Aiming and swapping to the tool before a block starts to crack.
pub const BREAK_OVERHEAD_TICKS: f64 = 4.0;
/// Swapping to the block, aiming and clicking.
pub const PLACE_TICKS: f64 = 5.0;
/// Walking to the edge while sneaking, at sneak speed.
pub const BRIDGE_TICKS: f64 = WALK_TICKS / 0.3;
/// A jump up to the height a block can be placed under the feet.
pub const PILLAR_TICKS: f64 = 10.0;

/// How a bot breaks a block with what its hotbar holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BreakTool {
    pub ticks: u32,
    /// The hotbar slot (0 to 8) to select.
    pub slot: u8,
}

/// What breaking a block costs a bot: ticks with its best hotbar tool.
pub trait Tools {
    /// `None` when it cannot break the state at all.
    fn break_tool(&mut self, state: &str) -> Option<BreakTool>;
}

/// The policy's say on breaking, with the bot's tools.
#[derive(Clone)]
pub struct Breaking {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub tools: Rc<RefCell<dyn Tools>>,
}

impl std::fmt::Debug for Breaking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Breaking")
            .field("allow", &self.allow)
            .field("deny", &self.deny)
            .finish_non_exhaustive()
    }
}

impl Breaking {
    pub fn new(policy: &Policy, tools: Rc<RefCell<dyn Tools>>) -> Self {
        Self {
            allow: policy.break_allow.clone(),
            deny: policy.break_deny.clone(),
            tools,
        }
    }

    /// Ticks to break `state`, `None` when the policy or the tools forbid it.
    pub fn ticks(&self, state: &str) -> Option<u32> {
        let name = name_of(state);
        let listed = |list: &[String]| list.iter().any(|n| name_of(n) == name);
        if listed(&self.deny) || (!self.allow.is_empty() && !listed(&self.allow)) {
            return None;
        }
        self.tools.borrow_mut().break_tool(state).map(|t| t.ticks)
    }
}

#[derive(Clone, Debug)]
pub struct Params {
    pub max_fall: u32,
    pub allow_sprint: bool,
    pub allow_parkour: bool,
    pub avoid: Vec<Aabb>,
    /// Set when the policy allows breaking.
    pub breaking: Option<Breaking>,
    /// Throwaway blocks a path may place; zero when placing is not allowed.
    pub throwaway: u32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            max_fall: SAFE_FALL,
            allow_sprint: true,
            allow_parkour: false,
            avoid: Vec::new(),
            breaking: None,
            throwaway: 0,
        }
    }
}

impl Params {
    pub fn from_policy(policy: &Policy) -> Self {
        Self {
            max_fall: policy.max_fall,
            allow_sprint: policy.allow_sprint,
            allow_parkour: policy.allow_parkour,
            avoid: policy.avoid.clone(),
            breaking: None,
            throwaway: 0,
        }
    }

    /// Adds what the bot carries: its tools when the policy allows breaking
    /// and its throwaway blocks when it allows placing.
    pub fn equipped(
        mut self,
        policy: &Policy,
        tools: Option<Rc<RefCell<dyn Tools>>>,
        throwaway: u32,
    ) -> Self {
        self.breaking = tools
            .filter(|_| policy.allow_break)
            .map(|t| Breaking::new(policy, t));
        self.throwaway = if policy.allow_place { throwaway } else { 0 };
        self
    }

    pub fn walk_ticks(&self) -> f64 {
        if self.allow_sprint {
            SPRINT_TICKS
        } else {
            WALK_TICKS
        }
    }

    /// Whether the block (as a unit cube) overlaps a box to stay out of.
    pub fn avoids(&self, (x, y, z): Pos) -> bool {
        let (x, y, z) = (f64::from(x), f64::from(y), f64::from(z));
        self.avoid.iter().any(|a| {
            a.min.x < x + 1.0
                && a.max.x > x
                && a.min.y < y + 2.0
                && a.max.y > y
                && a.min.z < z + 1.0
                && a.max.z > z
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    pub to: Pos,
    pub cost: f64,
    /// Throwaway blocks the step places.
    pub place: u32,
}

/// One kind of step. Adding a kind (breaking, placing, parkour) is adding
/// an implementation to the list the search is given.
pub trait MoveKind {
    fn expand(
        &self,
        world: &mut dyn Blocks,
        params: &Params,
        from: Pos,
        out: &mut Vec<Edge>,
    ) -> Result<(), Pending>;
}
