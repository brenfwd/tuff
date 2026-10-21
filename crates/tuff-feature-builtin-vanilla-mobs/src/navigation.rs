//! Ground navigation: finding paths on request, walking them node by node
//! through the move control, cutting corners, and giving up on paths that
//! get stuck or take too long. Paths are only computed while the region's
//! per-tick node budget lasts; a request that finds it spent is retried
//! the next time the goal asks.

use rand::Rng;
use tuff_vanilla_data::registry::Attribute;

use super::MobCore;
use super::path::{self, Path, PathMode, Walker};
use super::physics::block_pos;
use tuff_core::world::entity::{DebugPath, TickContext};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

const RECOMPUTE_TICKS: u64 = 20;
const STUCK_CHECK_INTERVAL: i32 = 100;
const STUCK_THRESHOLD_FACTOR: f32 = 0.25;
/// The least budget a search may start with; below it, it waits a tick.
const MIN_SEARCH_BUDGET: u32 = 64;

/// How a mob gets around: which node model its paths use and how it
/// follows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NavKind {
    #[default]
    Ground,
    /// Swims and never leaves the water (fish, guardians); `breaching`
    /// mobs may path through the air just above it and keep following
    /// their path out of water (dolphins).
    WaterBound { breaching: bool },
    /// Walks and swims (axolotls).
    Amphibious,
    /// Flies through open air in three dimensions.
    Fly,
}

#[derive(Debug, Clone)]
pub struct Navigation {
    pub kind: NavKind,
    pub path: Option<Path>,
    pub speed_modifier: f64,
    tick: i32,
    last_stuck_check: i32,
    last_stuck_check_position: [f64; 3],
    timeout_timer: u64,
    last_timeout_check: u64,
    timeout_limit: f64,
    timeout_node: Option<BlockPos>,
    delayed_recompute: bool,
    last_recompute: u64,
    target: Option<BlockPos>,
    reach: i32,
    stuck: bool,
    walk_cooldown: i32,
    walk_active: Option<BlockPos>,
    /// The game time a walk target first proved unreachable; cleared once
    /// it is reached or a path to it exists.
    pub cant_reach_since: Option<u64>,
    max_distance_to_waypoint: f32,
    pub can_float: bool,
    /// Uses the frog's amphibious node model (see `Walker::frog_nodes`).
    pub frog_nodes: bool,
    /// Which fluid `can_float` treats as a surface (see [`Walker::floats_on`]).
    pub floats_on: fn(Option<tuff_vanilla_data::BlockState>) -> bool,
    /// Which fluid it walks on top of whether or not it floats.
    pub stands_on_fluid: fn(Option<tuff_vanilla_data::BlockState>) -> bool,
    pub can_open_doors: bool,
    pub can_pass_doors: bool,
    pub can_walk_over_fences: bool,
    /// Paths stop short of open sky while the mob stands in shade.
    pub avoid_sun: bool,
    /// A wall climber heads straight for a target it finds no path to.
    pub climber: bool,
    pub climb_target: Option<BlockPos>,
    /// Only water makes a destination (a turtle out on a long swim).
    pub water_destinations: bool,
    required_path_length: f32,
    /// Keeps searches within a squared radius of a block (see [`Walker::bound`]).
    pub bound: Option<(BlockPos, f64)>,
}

impl Default for Navigation {
    fn default() -> Self {
        Self {
            kind: NavKind::Ground,
            path: None,
            speed_modifier: 0.0,
            tick: 0,
            last_stuck_check: 0,
            last_stuck_check_position: [0.0; 3],
            timeout_timer: 0,
            last_timeout_check: 0,
            timeout_limit: 0.0,
            timeout_node: None,
            delayed_recompute: false,
            last_recompute: 0,
            target: None,
            reach: 0,
            stuck: false,
            walk_cooldown: 0,
            walk_active: None,
            cant_reach_since: None,
            max_distance_to_waypoint: 0.5,
            can_float: false,
            frog_nodes: false,
            floats_on: path::is_floatable,
            stands_on_fluid: path::stands_on_nothing,
            can_open_doors: false,
            can_pass_doors: true,
            can_walk_over_fences: false,
            avoid_sun: false,
            climber: false,
            climb_target: None,
            water_destinations: false,
            required_path_length: 16.0,
            bound: None,
        }
    }
}

impl Navigation {
    pub fn max_distance_to_waypoint(&self) -> f32 {
        self.max_distance_to_waypoint
    }

    pub fn is_done(&self) -> bool {
        self.path.as_ref().is_none_or(Path::is_done)
    }

    pub fn is_stuck(&self) -> bool {
        self.stuck
    }

    pub fn stop(&mut self) {
        self.path = None;
    }

    fn reset_stuck_timeout(&mut self) {
        self.timeout_node = None;
        self.timeout_timer = 0;
        self.timeout_limit = 0.0;
        self.stuck = false;
    }

    /// Whether a block change at `pos` lies close enough to the rest of the
    /// path to recompute it.
    pub fn should_recompute(&self, pos: BlockPos, mob: [f64; 3]) -> bool {
        if self.delayed_recompute {
            return false;
        }
        let Some(path) = &self.path else {
            return false;
        };
        let Some(end) = path.end().filter(|_| !path.is_done()) else {
            return false;
        };
        let middle = [
            (end.x as f64 + mob[0]) / 2.0,
            (end.y as f64 + mob[1]) / 2.0,
            (end.z as f64 + mob[2]) / 2.0,
        ];
        let center = pos.center();
        let distance = (path.nodes.len() - path.next) as f64;
        (0..3).map(|a| (center[a] - middle[a]).powi(2)).sum::<f64>() < distance * distance
    }
}

impl MobCore {
    fn max_path_length(&self) -> f32 {
        (self.attribute(Attribute::FollowRange) as f32).max(self.navigation.required_path_length)
    }

    fn max_visited_nodes(&self) -> u32 {
        (self.attribute_base(Attribute::FollowRange) * 16.0)
            .floor()
            .max(0.0) as u32
    }

    fn can_update_path(&self) -> bool {
        match self.navigation.kind {
            NavKind::Ground => self.base.on_ground || self.in_liquid(),
            NavKind::WaterBound { breaching } => breaching || self.in_liquid(),
            NavKind::Amphibious => true,
            NavKind::Fly => {
                (self.navigation.can_float && self.in_liquid()) || self.base.vehicle.is_none()
            }
        }
    }

    fn max_vertical_distance_to_waypoint(&self) -> f64 {
        match self.navigation.kind {
            NavKind::WaterBound { .. } => 0.5,
            NavKind::Ground | NavKind::Amphibious | NavKind::Fly => 1.0,
        }
    }

    /// Whether nothing solid lies between `from` and the middle of its box
    /// standing at `to`, for a swimmer cutting straight to its next node.
    fn can_move_directly(&self, from: [f64; 3], to: [f64; 3], cx: &TickContext) -> bool {
        let clear = || {
            let to = [to[0], to[1] + f64::from(self.size.height) * 0.5, to[2]];
            let level = |pos: BlockPos| {
                (cx.block)(pos).unwrap_or(tuff_vanilla_data::Block::Stone.default_state())
            };
            tuff_core::world::raycast::cast_colliders(from, to, &level).is_none()
        };
        match self.navigation.kind {
            NavKind::Ground => false,
            NavKind::WaterBound { .. } | NavKind::Fly => clear(),
            NavKind::Amphibious => self.in_liquid() && clear(),
        }
    }

    fn surface_y(&self, cx: &TickContext) -> i32 {
        if !(self.navigation.can_float && self.in_floatable_fluid()) {
            return (self.base.position[1] + 0.5).floor() as i32;
        }
        let [x, y, z] = self.base.position;
        let mut surface = y.floor() as i32;
        let floatable = |y: i32| {
            (cx.block)(BlockPos::new(x.floor() as i32, y, z.floor() as i32)).is_some_and(|s| {
                tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::fluid::key::ENTITY_FLOATABLE,
                    s.fluid_state().fluid(),
                )
            })
        };
        let mut steps = 0;
        while floatable(surface) {
            surface += 1;
            steps += 1;
            if steps > 16 {
                return y.floor() as i32;
            }
        }
        surface
    }

    fn temp_position(&self, cx: &TickContext) -> [f64; 3] {
        let [x, y, z] = self.base.position;
        match self.navigation.kind {
            NavKind::Ground => [x, self.surface_y(cx) as f64, z],
            NavKind::Fly => [x, y, z],
            NavKind::WaterBound { .. } | NavKind::Amphibious => {
                [x, y + f64::from(self.size.height) * 0.5, z]
            }
        }
    }

    /// The walker a search starts from, keeping its sets for the debug
    /// feed while someone subscribes to entity paths.
    fn search_walker(&self, cx: &TickContext) -> Walker<'_> {
        let mut walker = self.walker();
        walker.capture_debug = cx
            .shared
            .debug_subscribers
            .borrow()
            .any(tuff_vanilla_data::registry::DebugSubscription::EntityPaths);
        walker.sea_level = cx.shared.dimensions.get(cx.dimension).sea_level();
        walker
    }

    pub fn walker(&self) -> Walker<'_> {
        Walker {
            position: self.base.position,
            bounds: self.bounds(),
            width: self.size.width,
            height: self.size.height,
            on_ground: self.base.on_ground,
            max_up_step: self.max_up_step(),
            max_fall_distance: self.max_fall_distance(),
            can_float: self.navigation.can_float,
            floats_on: self.navigation.floats_on,
            stands_on_fluid: self.navigation.stands_on_fluid,
            can_open_doors: self.navigation.can_open_doors,
            can_pass_doors: self.navigation.can_pass_doors,
            can_walk_over_fences: self.navigation.can_walk_over_fences,
            in_floatable_fluid: self.in_floatable_fluid(),
            malus: &self.path_malus,
            min_y: self.min_y,
            mode: match self.navigation.kind {
                NavKind::Ground => PathMode::Walk,
                NavKind::WaterBound { breaching } => PathMode::Swim { breaching },
                NavKind::Amphibious => PathMode::Amphibious,
                NavKind::Fly => PathMode::Fly,
            },
            in_water: self.body.in_water,
            bound: self.navigation.bound,
            capture_debug: false,
            frog_nodes: self.navigation.frog_nodes,
            sea_level: 63,
        }
    }

    /// Moves `pos` onto the surface of the column it is in, as ground
    /// navigation does before searching.
    fn surface_position(&self, mut pos: BlockPos, cx: &TickContext) -> BlockPos {
        let air = |p: BlockPos| (cx.block)(p).is_some_and(is_air);
        let solid = |p: BlockPos| (cx.block)(p).is_some_and(|s| s.is_solid());
        if air(pos) {
            let mut column = BlockPos::new(pos.x, pos.y - 1, pos.z);
            while column.y >= self.min_y && air(column) {
                column.y -= 1;
            }
            if column.y >= self.min_y {
                return BlockPos::new(pos.x, column.y + 1, pos.z);
            }
            column.y = pos.y + 1;
            while column.y <= self.max_y && air(column) {
                column.y += 1;
            }
            pos = column;
        }
        if solid(pos) {
            let mut column = BlockPos::new(pos.x, pos.y + 1, pos.z);
            while column.y <= self.max_y && solid(column) {
                column.y += 1;
            }
            return column;
        }
        pos
    }

    /// A path to `pos` (brought to the surface), or the current one if it
    /// already leads there.
    pub fn create_path(
        &mut self,
        pos: BlockPos,
        reach: i32,
        above: bool,
        cx: &mut TickContext,
    ) -> Option<Path> {
        if let Some(requests) = &mut self.steer_requests {
            requests.push(super::mount::PathRequest {
                pos,
                reach,
                above,
                speed: None,
            });
            return self.navigation.path.clone().filter(|p| !p.is_done());
        }
        if self.navigation.climber {
            self.navigation.climb_target = Some(pos);
        }
        (cx.block)(pos)?;
        let pos = if self.navigation.kind == NavKind::Ground {
            self.surface_position(pos, cx)
        } else {
            pos
        };
        if self.base.position[1] < self.min_y as f64 || !self.can_update_path() {
            return None;
        }
        if let Some(path) = &self.navigation.path
            && !path.is_done()
            && self.navigation.target == Some(pos)
        {
            return Some(path.clone());
        }
        if *cx.path_budget < MIN_SEARCH_BUDGET {
            self.navigation.delayed_recompute = true;
            return None;
        }
        let mut from = block_pos(self.base.position);
        if above {
            from.y += 1;
        }
        let _ = from;
        let max_length = self.max_path_length();
        let path = path::find_path(
            &self.search_walker(cx),
            &[pos],
            max_length,
            reach,
            self.max_visited_nodes(),
            cx.block,
            cx.path_budget,
        )?;
        self.navigation.target = Some(path.target);
        self.navigation.reach = reach;
        self.navigation.reset_stuck_timeout();
        Some(path)
    }

    /// A fresh ground search to `pos`, leaving the mob's own path alone.
    pub fn debug_path(&self, pos: BlockPos, cx: &mut TickContext) -> DebugPath {
        let walker = self.search_walker(cx);
        let mut budget = u32::MAX;
        match path::find_path(
            &walker,
            &[pos],
            self.max_path_length(),
            0,
            self.max_visited_nodes(),
            cx.block,
            &mut budget,
        ) {
            None => DebugPath::NoPath,
            Some(found) if found.reached => DebugPath::Made,
            Some(_) => DebugPath::NotComplete,
        }
    }

    pub fn set_required_path_length(&mut self, length: f32) {
        self.navigation.required_path_length = length;
    }

    /// A path to whichever of `targets` it reaches, stopping `reach` blocks
    /// short, without following it: `None` when none is reachable.
    pub fn path_to_any(
        &mut self,
        targets: &[BlockPos],
        reach: i32,
        cx: &mut TickContext,
    ) -> Option<Path> {
        if targets.is_empty() || *cx.path_budget < MIN_SEARCH_BUDGET {
            return None;
        }
        let max_length = self.max_path_length();
        let path = path::find_path(
            &self.search_walker(cx),
            targets,
            max_length,
            reach,
            self.max_visited_nodes(),
            cx.block,
            cx.path_budget,
        )?;
        path.reached.then_some(path)
    }

    /// While it steers what it rides, a walk is the vehicle's to search.
    fn steered_walk(&mut self, pos: BlockPos, reach: i32, above: bool, speed: f64) -> bool {
        let Some(requests) = &mut self.steer_requests else {
            return false;
        };
        requests.push(super::mount::PathRequest {
            pos,
            reach,
            above,
            speed: Some(speed),
        });
        true
    }

    /// Keeps a walk to within `close` blocks (Manhattan) of `pos` going the
    /// way a brain's walk target does: a walk that ended stuck short of the
    /// target waits a random 0 to 39 ticks before another try, the first
    /// tick no path reaches it is remembered, and with no path at all it
    /// takes a partial step toward it. True while walking or about to.
    pub fn walk_to_target(
        &mut self,
        pos: BlockPos,
        close: i32,
        speed: f64,
        cx: &mut TickContext,
    ) -> bool {
        let here = block_pos(self.base.position);
        let distance = (pos.x - here.x).abs() + (pos.y - here.y).abs() + (pos.z - here.z).abs();
        if distance <= close {
            self.navigation.walk_active = None;
            self.navigation.cant_reach_since = None;
            return false;
        }
        if !self.navigation.is_done() {
            self.navigation.walk_active = Some(pos);
            return true;
        }
        if self.navigation.walk_active.take().is_some() && self.navigation.is_stuck() {
            self.navigation.walk_cooldown = cx.rng.gen_range(0..40);
        }
        if self.navigation.walk_cooldown > 0 {
            self.navigation.walk_cooldown -= 1;
            return false;
        }
        let path = self.create_path(pos, 0, false, cx);
        if path.as_ref().is_some_and(|p| p.reached) {
            self.navigation.cant_reach_since = None;
        } else {
            self.navigation.cant_reach_since.get_or_insert(cx.tick);
        }
        let path = match path {
            Some(path) => Some(path),
            None => {
                let center = [
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y),
                    f64::from(pos.z) + 0.5,
                ];
                let step = super::random_pos::pos_towards_within(
                    self,
                    10,
                    7,
                    center,
                    std::f64::consts::FRAC_PI_2,
                    cx,
                );
                step.and_then(|step| self.create_path(block_pos(step), 0, false, cx))
            }
        };
        let moving = self.move_along(path, speed, cx);
        if moving {
            self.navigation.walk_active = Some(pos);
        }
        moving
    }

    /// Walks to within `reach` blocks of `pos`.
    pub fn move_to_block(
        &mut self,
        pos: BlockPos,
        reach: i32,
        speed: f64,
        cx: &mut TickContext,
    ) -> bool {
        if self.steered_walk(pos, reach, false, speed) {
            return true;
        }
        let path = self.create_path(pos, reach, false, cx);
        self.move_along(path, speed, cx)
    }

    pub fn move_to_position(&mut self, target: [f64; 3], speed: f64, cx: &mut TickContext) -> bool {
        let pos = block_pos(target);
        if self.steered_walk(pos, 1, false, speed) {
            return true;
        }
        let path = self.create_path(pos, 1, false, cx);
        self.move_along(path, speed, cx)
    }

    pub fn move_to_entity(&mut self, target: [f64; 3], speed: f64, cx: &mut TickContext) -> bool {
        let pos = block_pos(target);
        if self.steered_walk(pos, 1, true, speed) {
            return true;
        }
        match self.create_path(pos, 1, true, cx) {
            Some(path) => self.move_along(Some(path), speed, cx),
            None if self.navigation.climber => {
                self.navigation.speed_modifier = speed;
                true
            }
            None => false,
        }
    }

    /// Heads for an entity at `target`, stopping `reach` blocks short.
    pub fn move_to_entity_reach(
        &mut self,
        target: [f64; 3],
        reach: i32,
        speed: f64,
        cx: &mut TickContext,
    ) -> bool {
        let pos = block_pos(target);
        if self.steered_walk(pos, reach, true, speed) {
            return true;
        }
        match self.create_path(pos, reach, true, cx) {
            Some(path) => self.move_along(Some(path), speed, cx),
            None if self.navigation.climber => {
                self.navigation.speed_modifier = speed;
                true
            }
            None => {
                self.navigation.path = None;
                false
            }
        }
    }

    /// A climber without a path walks at its target, up walls if need be,
    /// until it stands there.
    fn tick_climb(&mut self) {
        let Some(target) = self.navigation.climb_target else {
            return;
        };
        let width = f64::from(self.size.width);
        let near =
            |pos: [f64; 3]| tuff_core::world::distance_sq(pos, self.base.position) < width * width;
        let center = |x: i32, y: f64, z: i32| [x as f64 + 0.5, y, z as f64 + 0.5];
        let [_, y, _] = self.base.position;
        let at_target = near(center(target.x, target.y as f64 + 0.5, target.z));
        let above = y > f64::from(target.y) && near(center(target.x, y.floor() + 0.5, target.z));
        if at_target || above {
            self.navigation.climb_target = None;
        } else {
            let speed = self.navigation.speed_modifier;
            self.movement.set_wanted(
                [
                    f64::from(target.x),
                    f64::from(target.y),
                    f64::from(target.z),
                ],
                speed,
            );
        }
    }

    pub fn move_along(&mut self, path: Option<Path>, speed: f64, cx: &mut TickContext) -> bool {
        let Some(path) = path else {
            self.navigation.path = None;
            return false;
        };
        let same = self
            .navigation
            .path
            .as_ref()
            .is_some_and(|current| current.same_nodes(&path));
        if !same {
            self.navigation.path = Some(path);
        }
        if self.navigation.is_done() {
            return false;
        }
        self.trim_path(cx);
        if self
            .navigation
            .path
            .as_ref()
            .is_none_or(|p| p.nodes.is_empty())
        {
            return false;
        }
        self.navigation.speed_modifier = speed;
        self.navigation.last_stuck_check = self.navigation.tick;
        self.navigation.last_stuck_check_position = self.temp_position(cx);
        true
    }

    fn trim_path(&mut self, cx: &TickContext) {
        if self.navigation.avoid_sun {
            let [x, y, z] = self.base.position;
            if !self.can_see_sky(block_pos([x, y + 0.5, z]), cx) {
                let cut = self.navigation.path.as_ref().and_then(|path| {
                    path.nodes
                        .iter()
                        .position(|node| self.can_see_sky(node.pos(), cx))
                });
                if let (Some(cut), Some(path)) = (cut, &mut self.navigation.path) {
                    path.nodes.truncate(cut);
                }
            }
        }
        let Some(path) = &mut self.navigation.path else {
            return;
        };
        for i in 0..path.nodes.len() {
            let node = path.nodes[i];
            let cauldron = (cx.block)(node.pos()).is_some_and(|s| {
                tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::CAULDRONS, s.block())
            });
            if cauldron {
                path.nodes[i].y += 1;
                if let Some(next) = path.nodes.get_mut(i + 1)
                    && node.y >= next.y
                {
                    next.y = node.y + 1;
                }
            }
        }
    }

    pub fn recompute_path(&mut self, cx: &mut TickContext) {
        if cx.tick.saturating_sub(self.navigation.last_recompute) > RECOMPUTE_TICKS
            && self.can_update_path()
        {
            if let Some(target) = self.navigation.target {
                self.navigation.path = None;
                let reach = self.navigation.reach;
                let path = self.find_again(target, reach, cx);
                self.navigation.path = path;
                self.navigation.last_recompute = cx.tick;
                self.navigation.delayed_recompute = false;
            }
            return;
        }
        self.navigation.delayed_recompute = true;
    }

    fn find_again(&mut self, target: BlockPos, reach: i32, cx: &mut TickContext) -> Option<Path> {
        if *cx.path_budget < MIN_SEARCH_BUDGET {
            return None;
        }
        let max_length = self.max_path_length();
        let path = path::find_path(
            &self.search_walker(cx),
            &[target],
            max_length,
            reach,
            self.max_visited_nodes(),
            cx.block,
            cx.path_budget,
        )?;
        self.navigation.reset_stuck_timeout();
        Some(path)
    }

    pub fn tick_navigation(&mut self, cx: &mut TickContext) {
        self.navigation.tick += 1;
        if self.navigation.delayed_recompute {
            self.recompute_path(cx);
        }
        if self.navigation.is_done() {
            if self.navigation.climber {
                self.tick_climb();
            }
            return;
        }
        let position = self.temp_position(cx);
        if self.can_update_path() {
            self.follow_path(cx);
        } else if let Some(path) = &mut self.navigation.path
            && !path.is_done()
        {
            let next = path.entity_position(path.next, self.size.width);
            if position[1] > next[1]
                && !self.base.on_ground
                && position[0].floor() == next[0].floor()
                && position[2].floor() == next[2].floor()
            {
                path.advance();
            }
        }
        if self.navigation.is_done() {
            return;
        }
        let path = self.navigation.path.as_ref().expect("not done");
        let target = path.entity_position(path.next, self.size.width);
        let below = block_pos(target);
        let ground = if self.navigation.kind != NavKind::Ground
            || (cx.block)(BlockPos::new(below.x, below.y - 1, below.z)).is_some_and(is_air)
        {
            target[1]
        } else {
            path::floor_level(below, cx.block)
        };
        let speed = self.navigation.speed_modifier;
        self.movement
            .set_wanted([target[0], ground, target[2]], speed);
    }

    fn follow_path(&mut self, cx: &mut TickContext) {
        let position = self.temp_position(cx);
        let width = self.size.width;
        self.navigation.max_distance_to_waypoint = if width > 0.75 {
            width / 2.0
        } else {
            0.75 - width / 2.0
        };
        let max = self.navigation.max_distance_to_waypoint as f64;
        let vertical = self.max_vertical_distance_to_waypoint();
        let path = self.navigation.path.as_ref().expect("not done");
        let node = path.next_node().pos();
        let close = (self.base.position[0] - (node.x as f64 + 0.5)).abs() < max
            && (self.base.position[2] - (node.z as f64 + 0.5)).abs() < max
            && (self.base.position[1] - node.y as f64).abs() < vertical;
        let kind = path.next_node().kind;
        let cut = !matches!(
            kind,
            tuff_vanilla_data::entity::PathType::FireInNeighbor
                | tuff_vanilla_data::entity::PathType::DamagingInNeighbor
                | tuff_vanilla_data::entity::PathType::WalkableDoor
        ) && !(self.navigation.frog_nodes
            && kind == tuff_vanilla_data::entity::PathType::WaterBorder)
            && next_in_direction(
                path,
                position,
                |to| self.can_move_directly(position, to, cx),
                self.size.width,
            );
        let path = self.navigation.path.as_mut().expect("not done");
        if close || cut {
            path.advance();
        }
        self.detect_stuck(position, cx);
    }

    fn detect_stuck(&mut self, position: [f64; 3], cx: &TickContext) {
        let speed = self.body.speed;
        let nav = &mut self.navigation;
        if nav.tick - nav.last_stuck_check > STUCK_CHECK_INTERVAL {
            let effective = if speed >= 1.0 { speed } else { speed * speed };
            let threshold = effective * 100.0 * STUCK_THRESHOLD_FACTOR;
            let moved = (0..3)
                .map(|a| (position[a] - nav.last_stuck_check_position[a]).powi(2))
                .sum::<f64>();
            if moved < (threshold * threshold) as f64 {
                nav.stuck = true;
                nav.path = None;
            } else {
                nav.stuck = false;
            }
            nav.last_stuck_check = nav.tick;
            nav.last_stuck_check_position = position;
        }
        let Some(path) = nav.path.as_ref().filter(|p| !p.is_done()) else {
            return;
        };
        let node = path.next_node().pos();
        if nav.timeout_node == Some(node) {
            nav.timeout_timer += cx.tick.saturating_sub(nav.last_timeout_check);
        } else {
            nav.timeout_node = Some(node);
            let bottom = [node.x as f64 + 0.5, node.y as f64, node.z as f64 + 0.5];
            let distance = (0..3)
                .map(|a| (position[a] - bottom[a]).powi(2))
                .sum::<f64>()
                .sqrt();
            nav.timeout_limit = if speed > 0.0 {
                distance / speed as f64 * 20.0
            } else {
                0.0
            };
        }
        if nav.timeout_limit > 0.0 && nav.timeout_timer as f64 > nav.timeout_limit * 3.0 {
            nav.reset_stuck_timeout();
            nav.path = None;
        }
        nav.last_timeout_check = cx.tick;
    }

    /// Whether a destination is one a wandering mob may choose: something
    /// solid to stand on, or for a swimmer, anything it can be inside.
    pub fn is_stable_destination(&self, pos: BlockPos, cx: &TickContext) -> bool {
        let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
        if self.navigation.water_destinations {
            return (cx.block)(pos).is_some_and(|s| s.block() == tuff_vanilla_data::Block::Water);
        }
        match self.navigation.kind {
            NavKind::Ground => (cx.block)(below).is_some_and(|s| s.is_solid_render()),
            NavKind::WaterBound { .. } => (cx.block)(pos).is_some_and(|s| !s.is_solid_render()),
            NavKind::Amphibious => (cx.block)(below).is_some_and(|s| !is_air(s)),
            NavKind::Fly => (cx.block)(pos).is_some_and(|s| s.is_solid_render()),
        }
    }
}

/// Whether the mob, near its next node, should aim past it already.
fn next_in_direction(
    path: &Path,
    position: [f64; 3],
    can_move_directly: impl Fn([f64; 3]) -> bool,
    width: f32,
) -> bool {
    if path.next + 1 >= path.nodes.len() {
        return false;
    }
    let bottom = |node: &path::PathNode| [node.x as f64 + 0.5, node.y as f64, node.z as f64 + 0.5];
    let current = bottom(&path.nodes[path.next]);
    let to_current = [
        current[0] - position[0],
        current[1] - position[1],
        current[2] - position[2],
    ];
    let current_sq = to_current.iter().map(|v| v * v).sum::<f64>();
    if current_sq >= 4.0 {
        return false;
    }
    if can_move_directly(path.entity_position(path.next, width)) {
        return true;
    }
    let next = bottom(&path.nodes[path.next + 1]);
    let to_next = [
        next[0] - position[0],
        next[1] - position[1],
        next[2] - position[2],
    ];
    let next_sq = to_next.iter().map(|v| v * v).sum::<f64>();
    if next_sq < current_sq || current_sq < 0.5 {
        let normalize = |v: [f64; 3], length: f64| {
            if length < 1.0e-5 {
                [0.0; 3]
            } else {
                v.map(|c| c / length)
            }
        };
        let a = normalize(to_current, current_sq.sqrt());
        let b = normalize(to_next, next_sq.sqrt());
        return a[0] * b[0] + a[1] * b[1] + a[2] * b[2] < 0.0;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_walk_that_ended_stuck_waits_before_trying_again() {
        crate::tests::with_context(|mob, cx| {
            mob.base.position = [0.5, 64.0, 0.5];
            let target = BlockPos::new(6, 64, 0);
            mob.navigation.walk_active = Some(target);
            mob.navigation.stuck = true;
            mob.walk_to_target(target, 0, 1.0, cx);
            assert!(mob.navigation.walk_active.is_none() || !mob.navigation.is_done());
            assert!(
                (0..40).contains(&mob.navigation.walk_cooldown)
                    || mob.navigation.walk_cooldown == 0
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_unreachable_walk_target_is_remembered_and_cleared_on_arrival() {
        let block = |pos: BlockPos| {
            (pos.x < 20).then(|| {
                if pos.y < 64 {
                    tuff_vanilla_data::Block::GrassBlock.default_state()
                } else {
                    tuff_vanilla_data::Block::Air.default_state()
                }
            })
        };
        crate::tests::with_world(&block, &[], |mob, cx| {
            mob.base.position = [0.5, 64.0, 0.5];
            cx.tick = 77;
            mob.walk_to_target(BlockPos::new(60, 64, 0), 0, 1.0, cx);
            assert_eq!(mob.navigation.cant_reach_since, Some(77));
            assert!(!mob.walk_to_target(BlockPos::new(0, 64, 0), 0, 1.0, cx));
            assert_eq!(mob.navigation.cant_reach_since, None);
        });
    }
}
