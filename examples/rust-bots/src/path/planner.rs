// SPDX-License-Identifier: MIT
use tuff_bots_api::control::Goal;

use super::{Limits, MoveKind, Params, Plan, Pos, Search, Status, default_moves, reached};
use crate::world::Blocks;

const REPLAN_AHEAD: usize = 8;
const VALIDATE_AHEAD: usize = 12;
const VALIDATE_EVERY_TICKS: u32 = 60;

#[derive(Debug, PartialEq)]
pub enum Advance {
    /// Head for this block next.
    Waypoint(Pos),
    /// A first path is still being searched for.
    Searching,
    Arrived,
    Failed(String),
}

/// Follows a goal in segments: it keeps the current path, plans the next
/// segment from its end before it is reached, and replans when the blocks
/// ahead change or the bot is knocked off the path.
pub struct Planner {
    goal: Goal,
    params: Params,
    moves: Vec<Box<dyn MoveKind>>,
    limits: Limits,
    path: Vec<Pos>,
    /// Throwaway blocks placed on the way to each node of `path`.
    placed: Vec<u32>,
    complete: bool,
    exhausted: bool,
    search: Option<Search>,
    dirty: bool,
    ticks_since_validate: u32,
}

impl Planner {
    pub fn new(goal: Goal, params: Params) -> Self {
        Self {
            goal,
            params,
            moves: default_moves(),
            limits: Limits::default(),
            path: Vec::new(),
            placed: Vec::new(),
            complete: false,
            exhausted: false,
            search: None,
            dirty: false,
            ticks_since_validate: 0,
        }
    }

    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    pub fn path(&self) -> &[Pos] {
        &self.path
    }

    /// The throwaway blocks the bot carries now; later searches plan with
    /// what is left after the placements still ahead.
    pub fn set_throwaway(&mut self, count: u32) {
        self.params.throwaway = count;
    }

    fn search_from(&self, start: Pos, ahead: u32) -> Search {
        let mut params = self.params.clone();
        params.throwaway = params.throwaway.saturating_sub(ahead);
        Search::new(start, self.goal.clone(), params)
    }

    /// A block event touched the world: check the path ahead next tick.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Keeps later searches off this block, for a step that did not work.
    pub fn penalise(&mut self, at: Pos) {
        self.params.avoid.push(crate::exec::unit_box(at));
        self.drop_plan();
    }

    fn drop_plan(&mut self) {
        self.path.clear();
        self.placed.clear();
        self.complete = false;
        self.exhausted = false;
        self.search = None;
    }

    fn edge_exists(&self, world: &mut dyn Blocks, from: Pos, to: Pos) -> bool {
        let mut edges = Vec::new();
        for kind in &self.moves {
            if kind.expand(world, &self.params, from, &mut edges).is_err() {
                return true;
            }
        }
        edges.iter().any(|e| e.to == to)
    }

    fn still_valid(&self, world: &mut dyn Blocks, upto: usize) -> bool {
        let end = (upto + VALIDATE_AHEAD).min(self.path.len() - 1);
        (upto..end).all(|i| self.edge_exists(world, self.path[i], self.path[i + 1]))
    }

    fn absorb(&mut self, plan: Plan) {
        let extends = self.search.as_ref().map(Search::start) == self.path.last().copied()
            && !self.path.is_empty();
        if extends {
            if plan.path.len() > 1 {
                let base = self.placed.last().copied().unwrap_or(0);
                self.path.extend_from_slice(&plan.path[1..]);
                self.placed
                    .extend(plan.placed[1..].iter().map(|p| p + base));
            }
        } else {
            self.path = plan.path;
            self.placed = plan.placed;
        }
        self.complete = plan.complete;
        self.search = None;
    }

    pub fn tick(&mut self, at: Pos, world: &mut dyn Blocks) -> Advance {
        if reached(&self.goal, at) {
            return Advance::Arrived;
        }
        self.ticks_since_validate += 1;
        let index = self.path.iter().position(|p| *p == at);
        if index.is_none() && !self.path.is_empty() {
            self.drop_plan();
        }
        if let Some(i) = index
            && (self.dirty || self.ticks_since_validate >= VALIDATE_EVERY_TICKS)
        {
            self.dirty = false;
            self.ticks_since_validate = 0;
            let ahead: Vec<Pos> = self.path[i..]
                .iter()
                .take(VALIDATE_AHEAD + 1)
                .copied()
                .collect();
            world.refresh(&ahead);
            if !self.still_valid(world, i) {
                self.drop_plan();
            }
        }
        let index = self.path.iter().position(|p| *p == at);
        if self.search.is_none() {
            let remaining = index.map_or(0, |i| self.path.len() - 1 - i);
            if self.path.is_empty() {
                self.search = Some(self.search_from(at, 0));
            } else if !self.complete && remaining <= REPLAN_AHEAD {
                let end = *self.path.last().unwrap_or(&at);
                let ahead = match (index, self.placed.last()) {
                    (Some(i), Some(last)) => last - self.placed[i],
                    _ => 0,
                };
                self.search = Some(self.search_from(end, ahead));
            }
        }
        if let Some(search) = self.search.as_mut()
            && let Status::Done(plan) = search.step(world, &self.moves, self.limits)
        {
            let stuck = plan.path.len() <= 1 && !plan.complete;
            if stuck {
                let at_end = index.is_none_or(|i| i + 1 == self.path.len());
                self.search = None;
                if at_end {
                    self.drop_plan();
                    return Advance::Failed("no path".into());
                }
                self.complete = true;
                self.exhausted = true;
            } else {
                self.absorb(plan);
            }
        }
        match self.path.iter().position(|p| *p == at) {
            Some(i) if i + 1 < self.path.len() => Advance::Waypoint(self.path[i + 1]),
            Some(_) if self.exhausted => {
                self.drop_plan();
                Advance::Failed("no path".into())
            }
            _ => Advance::Searching,
        }
    }
}
