//! The goals passive mobs use: floating, panicking, breeding, being
//! tempted by food, following a parent, wandering, looking around and
//! eating grass.

use rand::Rng;
use tuff_vanilla_data::entity::PathType;
use tuff_vanilla_data::registry::{Attribute, EntityType};
use tuff_vanilla_data::{Block, Item};

use super::control::{HEAD_ROT_SPEED, Operation};
use super::goal::{Flags, Goal, reduced_delay};
use super::physics::{self, block_pos};
use super::{MobCore, event, random_pos};
use tuff_core::protocol::play;
use tuff_core::world::entity::{Effect, EntitySummary, NearbyPlayer, TickContext};
use tuff_core::world::{framed, packets};
use tuff_world_model::BlockPos;

const LAST_DAMAGE_MEMORY: u64 = 40;

fn nearest_player<'a>(
    mob: &MobCore,
    from: [f64; 3],
    range: f64,
    players: &'a [NearbyPlayer<'a>],
    accept: impl Fn(&NearbyPlayer) -> bool,
) -> Option<&'a NearbyPlayer<'a>> {
    let _ = mob;
    players
        .iter()
        .filter(|p| accept(p))
        .map(|p| (tuff_core::world::distance_sq(from, p.position), p))
        .filter(|(d, _)| *d <= range * range)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, p)| p)
}

fn player(cx: &TickContext, id: i32) -> Option<NearbyPlayer<'static>> {
    cx.players
        .iter()
        .find(|p| p.id == id)
        .map(|p| NearbyPlayer { name: "", ..*p })
}

fn entity(cx: &TickContext, id: i32) -> Option<EntitySummary> {
    cx.entities.iter().find(|e| e.id == id).copied()
}

fn eye(position: [f64; 3], eye_height: f64) -> [f64; 3] {
    [position[0], position[1] + eye_height, position[2]]
}

/// Makes the mob swim up in water and lava.
#[derive(Debug, Default)]
pub struct Float;

impl Float {
    pub fn new(mob: &mut MobCore) -> Self {
        mob.navigation.can_float = true;
        Self
    }
}

impl Goal for Float {
    fn flags(&self) -> Flags {
        Flags::JUMP
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.body.water_height > mob.fluid_jump_threshold() || mob.in_lava()
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if cx.rng.r#gen::<f32>() < 0.8 {
            mob.jump.jump = true;
        }
    }
}

/// Keeps powder-snow walkers jumping so they stay on top of the snow.
#[derive(Debug, Default)]
pub struct ClimbOnTopOfPowderSnow;

impl Goal for ClimbOnTopOfPowderSnow {
    fn flags(&self) -> Flags {
        Flags::JUMP
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !mob.body.in_powder_snow
            || !tuff_datapack::tags::is(
                tuff_vanilla_data::tag::entity_type::key::POWDER_SNOW_WALKABLE_MOBS,
                mob.base.kind,
            )
        {
            return false;
        }
        let at = block_pos(mob.base.position);
        let above = BlockPos::new(at.x, at.y + 1, at.z);
        (cx.block)(above)
            .is_some_and(|s| s.block() == Block::PowderSnow || s.collision_boxes().is_empty())
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.jump.jump = true;
    }
}

/// Runs off after being hurt by something that scares it, towards water
/// when it burns.
#[derive(Debug)]
pub struct Panic {
    speed: f64,
    target: [f64; 3],
    /// Always runs for water this far away first, not only when burning
    /// (turtles).
    water_range: Option<i32>,
}

impl Panic {
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            target: [0.0; 3],
            water_range: None,
        }
    }

    /// Panics towards water within `range` blocks whenever there is some.
    pub fn to_water(speed: f64, range: i32) -> Self {
        Self {
            water_range: Some(range),
            ..Self::new(speed)
        }
    }

    fn look_for_water(mob: &MobCore, range: i32, cx: &TickContext) -> Option<BlockPos> {
        let here = block_pos(mob.base.position);
        let empty = (cx.block)(here).is_some_and(|s| s.collision_boxes().is_empty());
        if !empty {
            return None;
        }
        let water = |pos: BlockPos| {
            (cx.block)(pos).is_some_and(|s| {
                tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::fluid::key::WATER,
                    s.fluid_state().fluid(),
                )
            })
        };
        let mut found: Option<(i32, BlockPos)> = None;
        for dy in -1i32..=1 {
            for dx in -range..=range {
                for dz in -range..=range {
                    let distance = dx.abs() + dy.abs() + dz.abs();
                    let pos = BlockPos::new(here.x + dx, here.y + dy, here.z + dz);
                    if found.is_none_or(|(d, _)| distance < d) && water(pos) {
                        found = Some((distance, pos));
                    }
                }
            }
        }
        found.map(|(_, pos)| pos)
    }
}

impl Goal for Panic {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let scared = mob.last_damage.as_ref().is_some_and(|(damage, at)| {
            cx.tick.saturating_sub(*at) <= LAST_DAMAGE_MEMORY
                && tuff_core::world::damage_type_in_tag(damage.kind, "minecraft:panic_causes")
        });
        if !scared {
            return false;
        }
        let range = self.water_range.unwrap_or(5);
        if (self.water_range.is_some() || mob.base.on_fire())
            && let Some(water) = Self::look_for_water(mob, range, cx)
        {
            self.target = [water.x as f64, water.y as f64, water.z as f64];
            return true;
        }
        match random_pos::default_pos(mob, 5, 4, cx) {
            Some(target) => {
                self.target = target;
                true
            }
            None => false,
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.move_to_position(self.target, self.speed, cx);
        mob.panicking = true;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.panicking = false;
    }
}

/// Walks to a partner in love and breeds with it.
#[derive(Debug)]
pub struct Breed {
    speed: f64,
    partner: Option<i32>,
    love_time: i32,
    brain: bool,
    duration: i32,
    unless: Option<fn(&MobCore) -> bool>,
    partners: Option<fn(EntityType, EntityType) -> bool>,
}

impl Breed {
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            partner: None,
            love_time: 0,
            brain: false,
            duration: 60,
            unless: None,
            partners: None,
        }
    }

    /// Brain-driven animals court for 60 plus up to 49 random ticks.
    pub fn brain(mut self) -> Self {
        self.brain = true;
        self
    }

    /// Never starts while `unless` holds (a turtle already carrying eggs).
    pub fn unless(mut self, unless: fn(&MobCore) -> bool) -> Self {
        self.unless = Some(unless);
        self
    }

    /// Courts any kind `partners(own, other)` accepts, not just its own
    /// (a horse and a donkey).
    pub fn with_partners(mut self, partners: fn(EntityType, EntityType) -> bool) -> Self {
        self.partners = Some(partners);
        self
    }

    fn free_partner(&self, mob: &MobCore, cx: &TickContext) -> Option<i32> {
        let area = physics::inflate(mob.bounds(), [8.0; 3]);
        let kind = mob.base.kind;
        cx.entities
            .iter()
            .filter(|e| {
                e.id != mob.base.id
                    && self
                        .partners
                        .map_or(e.kind == kind, |partners| partners(kind, e.kind))
                    && physics::intersects(e.bounds, area)
                    && e.living
                        .is_some_and(|l| l.alive && l.in_love && !l.panicking)
            })
            .map(|e| (mob.distance_sq(e.position), e.id))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id)
    }
}

impl Goal for Breed {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !mob.love.as_ref().is_some_and(|l| l.is_in_love())
            || self.unless.is_some_and(|unless| unless(mob))
        {
            return false;
        }
        self.partner = self.free_partner(mob, cx);
        if self.partner.is_some() && self.brain {
            self.duration = 60 + cx.rng.gen_range(0..50);
        }
        self.partner.is_some()
    }

    fn can_continue(&mut self, _mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(partner) = self.partner.and_then(|id| entity(cx, id)) else {
            return false;
        };
        partner
            .living
            .is_some_and(|l| l.alive && l.in_love && !l.panicking)
            && self.love_time < self.duration
    }

    fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.partner = None;
        self.love_time = 0;
        self.duration = 60;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(partner) = self.partner.and_then(|id| entity(cx, id)) else {
            return;
        };
        let eye_height = partner.living.map_or(0.0, |l| l.eye_height) as f64;
        mob.look
            .set_look_at(eye(partner.position, eye_height), 10.0, mob.max_head_x_rot);
        mob.move_to_entity(partner.position, self.speed, cx);
        self.love_time += 1;
        if self.love_time >= self.adjusted_delay(self.duration)
            && mob.distance_sq(partner.position) < 9.0
        {
            cx.effects.push(Effect::Breed {
                partner: partner.id,
            });
        }
    }
}

/// Follows a player holding food it likes.
#[derive(Debug)]
pub struct Tempt {
    speed: f64,
    land_speed: Option<f64>,
    items: fn(Item) -> bool,
    mob_items: Option<fn(&MobCore, Item) -> bool>,
    baby_speed: Option<f64>,
    player: Option<i32>,
    calm_down: i32,
    stop_distance: f64,
    flying: bool,
}

impl Tempt {
    pub fn new(speed: f64, items: fn(Item) -> bool) -> Self {
        Self {
            speed,
            land_speed: None,
            items,
            mob_items: None,
            baby_speed: None,
            player: None,
            calm_down: 0,
            stop_distance: 2.5,
            flying: false,
        }
    }

    /// Steers with the move control toward a random point along the line to
    /// the player's eyes instead of pathfinding, stopping within
    /// `stop_distance`.
    pub fn without_pathfinding(mut self, stop_distance: f64) -> Self {
        self.flying = true;
        self.stop_distance = stop_distance;
        self
    }

    /// Goes at `speed` instead while a baby.
    pub fn baby_speed(mut self, speed: f64) -> Self {
        self.baby_speed = Some(speed);
        self
    }

    /// Tempted by items that depend on the mob's own state.
    pub fn for_mob(speed: f64, items: fn(&MobCore, Item) -> bool) -> Self {
        Self {
            mob_items: Some(items),
            ..Self::new(speed, |_| false)
        }
    }

    /// Goes at `speed` instead while out of water.
    pub fn on_land(mut self, speed: f64) -> Self {
        self.land_speed = Some(speed);
        self
    }

    fn stop_moving(&self, mob: &mut MobCore) {
        if self.flying {
            mob.movement.operation = Operation::Wait;
        } else {
            mob.navigation.stop();
        }
    }
}

impl Goal for Tempt {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.calm_down > 0 {
            self.calm_down -= 1;
            return false;
        }
        let range = mob.attribute(Attribute::TemptRange);
        let items = self.items;
        let mob_items = self.mob_items;
        let wants = |item: Item| match mob_items {
            Some(f) => f(mob, item),
            None => items(item),
        };
        self.player = nearest_player(mob, mob.base.position, range, cx.players, |p| {
            p.held.iter().flatten().any(|item| wants(*item))
        })
        .map(|p| p.id);
        self.player.is_some()
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.player = None;
        self.stop_moving(mob);
        self.calm_down = reduced_delay(100);
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(player) = self.player.and_then(|id| player(cx, id)) else {
            return;
        };
        mob.look.set_look_at(
            eye(player.position, player.eye_height),
            mob.max_head_y_rot + 20.0,
            mob.max_head_x_rot,
        );
        if mob.distance_sq(player.position) < self.stop_distance * self.stop_distance {
            self.stop_moving(mob);
        } else if self.flying {
            let eyes = eye(player.position, player.eye_height);
            let from = mob.base.position;
            let scale: f64 = cx.rng.r#gen();
            let target = [
                from[0] + (eyes[0] - from[0]) * scale,
                from[1] + (eyes[1] - from[1]) * scale,
                from[2] + (eyes[2] - from[2]) * scale,
            ];
            let speed = match self.baby_speed {
                Some(baby) if mob.is_baby() => baby,
                _ => self.speed,
            };
            mob.movement.set_wanted(target, speed);
        } else {
            let speed = match self.baby_speed {
                Some(baby) if mob.is_baby() => baby,
                _ => land_or(self.speed, self.land_speed, mob),
            };
            mob.move_to_entity(player.position, speed, cx);
        }
    }
}

fn land_or(speed: f64, land_speed: Option<f64>, mob: &MobCore) -> f64 {
    match land_speed {
        Some(land) if !mob.body.in_water => land,
        _ => speed,
    }
}

/// Keeps a baby near an adult of its kind.
#[derive(Debug)]
pub struct FollowParent {
    speed: f64,
    land_speed: Option<f64>,
    parent: Option<i32>,
    recalculate: i32,
}

impl FollowParent {
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            land_speed: None,
            parent: None,
            recalculate: 0,
        }
    }

    /// Goes at `speed` instead while out of water.
    pub fn on_land(mut self, speed: f64) -> Self {
        self.land_speed = Some(speed);
        self
    }
}

impl Goal for FollowParent {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !mob.is_baby() {
            return false;
        }
        let area = physics::inflate(mob.bounds(), [8.0, 4.0, 8.0]);
        let mut closest: Option<(f64, i32)> = None;
        for other in cx.entities {
            if other.kind != mob.base.kind
                || !physics::intersects(other.bounds, area)
                || !other.living.is_some_and(|l| l.age >= 0)
            {
                continue;
            }
            let distance = mob.distance_sq(other.position);
            if closest.is_none_or(|(d, _)| distance <= d) {
                closest = Some((distance, other.id));
            }
        }
        match closest {
            Some((distance, id)) if distance >= 9.0 => {
                self.parent = Some(id);
                true
            }
            _ => false,
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if !mob.is_baby() {
            return false;
        }
        let Some(parent) = self.parent.and_then(|id| entity(cx, id)) else {
            return false;
        };
        let distance = mob.distance_sq(parent.position);
        parent.living.is_some_and(|l| l.alive) && (9.0..=256.0).contains(&distance)
    }

    fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.recalculate = 0;
    }

    fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.parent = None;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.recalculate -= 1;
        if self.recalculate > 0 {
            return;
        }
        self.recalculate = self.adjusted_delay(10);
        if let Some(parent) = self.parent.and_then(|id| entity(cx, id)) {
            let speed = land_or(self.speed, self.land_speed, mob);
            mob.move_to_entity(parent.position, speed, cx);
        }
    }
}

/// Where a wandering mob looks for somewhere to go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wander {
    /// Anywhere it can stand.
    Anywhere,
    /// Onto land, and out of water when in it.
    Land,
    /// Anywhere water mobs may path through.
    Swimmable,
    /// Hovering above ground ahead of where it looks, else anywhere in the
    /// air or water there.
    Flying,
    /// Anywhere in the air or water ahead of where it looks, a little below
    /// its height.
    AirAndWater,
}

/// Heads back toward its home whenever it stands outside it.
#[derive(Debug)]
pub struct MoveTowardsHome {
    speed: f64,
    flags: Flags,
    target: [f64; 3],
}

impl MoveTowardsHome {
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            flags: Flags::MOVE,
            target: [0.0; 3],
        }
    }

    /// Also holds the look while it goes (guardians).
    pub fn looking(mut self) -> Self {
        self.flags = Flags::MOVE | Flags::LOOK;
        self
    }
}

impl Goal for MoveTowardsHome {
    fn flags(&self) -> Flags {
        self.flags
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(home) = mob.home else {
            return false;
        };
        if mob.within_home(block_pos(mob.base.position)) {
            return false;
        }
        let towards = [
            f64::from(home.pos.x) + 0.5,
            f64::from(home.pos.y),
            f64::from(home.pos.z) + 0.5,
        ];
        let angle = std::f64::consts::FRAC_PI_2;
        match random_pos::pos_towards_within(mob, 16, 7, towards, angle, cx) {
            Some(target) => {
                self.target = target;
                true
            }
            None => false,
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.move_to_position(self.target, self.speed, cx);
    }
}

type Heading = fn(&MobCore, &TickContext) -> Option<[f64; 3]>;

/// Wanders to random nearby spots every so often.
#[derive(Debug)]
pub struct RandomStroll {
    speed: f64,
    interval: i32,
    wander: Wander,
    flags: Flags,
    target: [f64; 3],
    unless: Option<fn(&MobCore, &mut TickContext) -> bool>,
    heading: Option<Heading>,
}

impl RandomStroll {
    pub const INTERVAL: i32 = 120;

    fn new(speed: f64, interval: i32, wander: Wander) -> Self {
        Self {
            speed,
            interval,
            wander,
            flags: Flags::MOVE,
            target: [0.0; 3],
            unless: None,
            heading: None,
        }
    }

    /// Flies about, hovering above ground where it can, trying about every
    /// `interval` ticks.
    pub fn flying(speed: f64, interval: i32) -> Self {
        Self::new(speed, interval, Wander::Flying)
    }

    /// Flies about through air and water, trying about every `interval`
    /// ticks.
    pub fn air_and_water(speed: f64, interval: i32) -> Self {
        Self::new(speed, interval, Wander::AirAndWater)
    }

    /// A flier heads this way instead of where it looks, when it says so.
    pub fn heading(mut self, heading: Heading) -> Self {
        self.heading = Some(heading);
        self
    }

    /// Never starts while `unless` holds (a fish following its school).
    pub fn unless(mut self, unless: fn(&MobCore, &mut TickContext) -> bool) -> Self {
        self.unless = Some(unless);
        self
    }

    pub fn water_avoiding(speed: f64) -> Self {
        Self::new(speed, Self::INTERVAL, Wander::Land)
    }

    /// Wanders anywhere, water too, trying about every `interval` ticks.
    pub fn with_interval(speed: f64, interval: i32) -> Self {
        Self::new(speed, interval, Wander::Anywhere)
    }

    /// Swims somewhere, trying about every `interval` ticks.
    pub fn swimming(speed: f64, interval: i32) -> Self {
        Self::new(speed, interval, Wander::Swimmable)
    }

    /// Also claims the look control while it runs.
    pub fn looking(mut self) -> Self {
        self.flags = self.flags | Flags::LOOK;
        self
    }

    fn position(&self, mob: &MobCore, cx: &mut TickContext) -> Option<[f64; 3]> {
        match self.wander {
            Wander::Anywhere => random_pos::default_pos(mob, 10, 7, cx),
            Wander::Swimmable => random_pos::swimmable_pos(mob, 10, 7, cx),
            Wander::Land if mob.body.in_water => random_pos::land_pos(mob, 15, 7, cx)
                .or_else(|| random_pos::default_pos(mob, 10, 7, cx)),
            Wander::Land if cx.rng.r#gen::<f32>() >= 0.001 => random_pos::land_pos(mob, 10, 7, cx),
            Wander::Land => random_pos::default_pos(mob, 10, 7, cx),
            Wander::Flying => random_pos::flying_pos(mob, self.view(mob, cx), cx),
            Wander::AirAndWater => {
                random_pos::air_and_water_pos(mob, 10, 7, -2, self.view(mob, cx), cx)
            }
        }
    }

    fn view(&self, mob: &MobCore, cx: &TickContext) -> [f64; 3] {
        self.heading
            .and_then(|heading| heading(mob, cx))
            .unwrap_or_else(|| super::ghast::view_vector(mob))
    }
}

impl Goal for RandomStroll {
    fn flags(&self) -> Flags {
        self.flags
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.unless.is_some_and(|unless| unless(mob, cx)) {
            return false;
        }
        if mob.no_action_time >= 100 || cx.rng.gen_range(0..reduced_delay(self.interval)) != 0 {
            return false;
        }
        match self.position(mob, cx) {
            Some(target) => {
                self.target = target;
                true
            }
            None => false,
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.move_to_position(self.target, self.speed, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }
}

/// A swimming mob's idle wander as its brain drives it: whenever it has
/// nowhere to go and is in water, it picks somewhere further along the
/// water and heads there; an amphibious one also strolls on land.
#[derive(Debug)]
pub struct SwimStroll {
    speed: f64,
    land_speed: Option<f64>,
}

impl SwimStroll {
    pub fn new(speed: f64) -> Self {
        Self {
            speed,
            land_speed: None,
        }
    }

    pub fn amphibious(speed: f64, land_speed: f64) -> Self {
        Self {
            speed,
            land_speed: Some(land_speed),
        }
    }
}

impl Goal for SwimStroll {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        (mob.body.in_water || self.land_speed.is_some()) && mob.navigation.is_done()
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let (target, speed) = match self.land_speed {
            Some(land_speed) if !mob.body.in_water => {
                (random_pos::land_pos(mob, 10, 7, cx), land_speed)
            }
            _ => (random_pos::swim_target(mob, cx), self.speed),
        };
        let Some(target) = target else {
            return;
        };
        let pos = block_pos(target);
        if block_pos(mob.base.position) == pos {
            return;
        }
        let path = mob.create_path(pos, 0, false, cx).or_else(|| {
            let bottom = [
                f64::from(pos.x) + 0.5,
                f64::from(pos.y),
                f64::from(pos.z) + 0.5,
            ];
            let step = random_pos::pos_towards(mob, 10, 7, bottom, cx)?;
            mob.create_path(block_pos(step), 0, false, cx)
        });
        mob.move_along(path, speed, cx);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
    }
}

/// Keeps away from players (or mobs of some types) that come near,
/// speeding up once one is within seven blocks.
#[derive(Debug)]
pub struct AvoidPlayer {
    distance: f64,
    walk_speed: f64,
    sprint_speed: f64,
    kinds: Option<&'static [tuff_vanilla_data::registry::EntityType]>,
    player: Option<i32>,
    path: Option<super::path::Path>,
    disabled: bool,
    only_if: Option<fn(&MobCore) -> bool>,
    monsters: bool,
}

impl AvoidPlayer {
    /// Turns the goal off or on (an ocelot stops fleeing once it trusts).
    pub fn set_disabled(&mut self, disabled: bool) {
        self.disabled = disabled;
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// Only flees while `only_if(mob)` holds.
    pub fn only_if(mut self, only_if: fn(&MobCore) -> bool) -> Self {
        self.only_if = Some(only_if);
        self
    }

    /// Keeps away from hostile mobs instead of players.
    pub fn monsters(distance: f64, walk_speed: f64, sprint_speed: f64) -> Self {
        Self {
            monsters: true,
            ..Self::new(distance, walk_speed, sprint_speed)
        }
    }

    pub fn new(distance: f64, walk_speed: f64, sprint_speed: f64) -> Self {
        Self {
            distance,
            walk_speed,
            sprint_speed,
            kinds: None,
            player: None,
            path: None,
            disabled: false,
            only_if: None,
            monsters: false,
        }
    }

    /// Keeps away from mobs of `kinds` instead of players.
    pub fn entities(
        kinds: &'static [tuff_vanilla_data::registry::EntityType],
        distance: f64,
        walk_speed: f64,
        sprint_speed: f64,
    ) -> Self {
        Self {
            kinds: Some(kinds),
            ..Self::new(distance, walk_speed, sprint_speed)
        }
    }
}

impl Goal for AvoidPlayer {
    fn flags(&self) -> Flags {
        Flags::MOVE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.disabled || self.only_if.is_some_and(|ok| !ok(mob)) {
            return false;
        }
        let area = physics::inflate(mob.bounds(), [self.distance, 3.0, self.distance]);
        let position = mob.base.position;
        let candidates: Vec<(super::target::TargetView, f64)> = match self.kinds {
            None if self.monsters => cx
                .entities
                .iter()
                .filter(|e| {
                    e.living.as_ref().is_some_and(|l| l.enemy)
                        && physics::intersects(area, e.bounds)
                })
                .filter_map(|e| Some((super::target::view(cx, e.id)?, 1.0)))
                .collect(),
            None => cx
                .players
                .iter()
                .filter(|p| physics::intersects(area, p.bounds()))
                .map(|p| {
                    (
                        super::target::TargetView::of_player(p),
                        p.visibility(mob.kind()),
                    )
                })
                .collect(),
            Some(kinds) => cx
                .entities
                .iter()
                .filter(|e| kinds.contains(&e.kind) && physics::intersects(area, e.bounds))
                .filter_map(|e| Some((super::target::view(cx, e.id)?, 1.0)))
                .collect(),
        };
        let nearest = candidates
            .into_iter()
            .filter(|(view, visibility)| {
                mob.targetable_within(view, self.distance, *visibility, true, cx)
            })
            .map(|(view, _)| {
                (
                    tuff_core::world::distance_sq(position, view.position),
                    view.id,
                    view.position,
                )
            })
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((distance_sq, id, from)) = nearest else {
            return false;
        };
        let Some(away) = random_pos::pos_away(mob, 16, 7, from, cx) else {
            return false;
        };
        if tuff_core::world::distance_sq(from, away) < distance_sq {
            return false;
        }
        self.player = Some(id);
        self.path = mob.create_path(block_pos(away), 0, false, cx);
        self.path.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        !self.disabled && !mob.navigation.is_done()
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        mob.move_along(self.path.take(), self.walk_speed, cx);
    }

    fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.player = None;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let near = self
            .player
            .and_then(|id| super::target::view(cx, id))
            .is_some_and(|p| mob.distance_sq(p.position) < 49.0);
        mob.navigation.speed_modifier = if near {
            self.sprint_speed
        } else {
            self.walk_speed
        };
    }
}

/// Faces the way it is drifting, for mobs with no other look goal.
#[derive(Debug, Default)]
pub struct FaceVelocity;

impl Goal for FaceVelocity {
    fn flags(&self) -> Flags {
        Flags::LOOK
    }

    fn can_use(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        true
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        let [vx, _, vz] = mob.base.velocity;
        if vx * vx + vz * vz > 1.0e-4 {
            mob.base.yaw = -(vx.atan2(vz) as f32).to_degrees();
        }
        mob.body_yaw = mob.base.yaw;
    }
}

/// Whether something riding `vehicle` is carried, however deep, by `mob`.
fn rides(mob: i32, mut vehicle: Option<i32>, cx: &TickContext) -> bool {
    for _ in 0..8 {
        let Some(id) = vehicle else {
            return false;
        };
        if id == mob {
            return true;
        }
        vehicle = cx
            .entities
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.vehicle);
    }
    false
}

/// Grazes: eats the grass it stands in, or turns the grass block under it
/// into dirt.
#[derive(Debug, Default)]
pub struct EatBlock {
    pub eat_tick: i32,
}

fn edible(state: Option<tuff_vanilla_data::BlockState>) -> bool {
    state.is_some_and(|s| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::block::key::EDIBLE_FOR_SHEEP,
            s.block(),
        )
    })
}

impl Goal for EatBlock {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK | Flags::JUMP
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let chance = self.adjusted_delay(if mob.is_baby() { 50 } else { 1000 });
        if cx.rng.gen_range(0..chance) != 0 {
            return false;
        }
        let pos = block_pos(mob.base.position);
        edible((cx.block)(pos))
            || (cx.block)(BlockPos::new(pos.x, pos.y - 1, pos.z))
                .is_some_and(|s| s.block() == Block::GrassBlock)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.eat_tick = self.adjusted_delay(40);
        mob.entity_event(event::EAT_GRASS, cx);
        mob.navigation.stop();
    }

    fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.eat_tick = 0;
    }

    fn can_continue(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        self.eat_tick > 0
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.eat_tick = (self.eat_tick - 1).max(0);
        if self.eat_tick != self.adjusted_delay(4) {
            return;
        }
        let griefing = cx.shared.game_rules.borrow().bool("mob_griefing");
        let pos = block_pos(mob.base.position);
        if edible((cx.block)(pos)) {
            if griefing {
                cx.effects.push(Effect::DestroyBlock { pos, drop: false });
            }
            mob.ate = true;
            return;
        }
        let below = BlockPos::new(pos.x, pos.y - 1, pos.z);
        if (cx.block)(below).is_some_and(|s| s.block() == Block::GrassBlock) {
            if griefing {
                let event = framed(&play::LevelEvent {
                    event: play::level_event::BLOCK_BREAK,
                    position: below,
                    data: Block::GrassBlock.default_state().id() as i32,
                    global: false,
                });
                cx.effects.push(Effect::ChunkPackets(packets([event])));
                cx.effects.push(Effect::SetBlock {
                    pos: below,
                    state: Block::Dirt.default_state(),
                });
            }
            mob.ate = true;
        }
    }
}

fn is_water(cx: &TickContext, pos: BlockPos) -> bool {
    (cx.block)(pos).is_some_and(|s| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::fluid::key::WATER,
            s.fluid_state().fluid(),
        )
    })
}

/// Heads back into water when stranded on land.
#[derive(Debug)]
pub struct FindWater {
    /// How far a walking search looks (by Manhattan distance) and how fast
    /// it goes; `None` steers straight at water within two blocks instead.
    walk: Option<(i32, f64)>,
    next_search: u64,
}

impl FindWater {
    /// Walks to water within `range` blocks, looking every two seconds.
    pub fn walking(range: i32, speed: f64) -> Self {
        Self {
            walk: Some((range, speed)),
            next_search: 0,
        }
    }

    /// Steers its move control at water right next to it.
    pub fn steering() -> Self {
        Self {
            walk: None,
            next_search: 0,
        }
    }

    fn nearest_water(&self, mob: &MobCore, range: i32, cx: &TickContext) -> Option<BlockPos> {
        let here = block_pos(mob.base.position);
        let mut fallback = None;
        for distance in 0..=range * 3 {
            for dy in -range..=range {
                for dx in -range..=range {
                    let rest = distance - dx.abs() - dy.abs();
                    if rest < 0 || rest > range {
                        continue;
                    }
                    for dz in if rest == 0 {
                        vec![0]
                    } else {
                        vec![-rest, rest]
                    } {
                        let pos = BlockPos::new(here.x + dx, here.y + dy, here.z + dz);
                        if (dx == 0 && dz == 0) || !is_water(cx, pos) {
                            continue;
                        }
                        let liquid = (cx.block)(pos).is_some_and(|s| s.is_liquid());
                        if !liquid {
                            continue;
                        }
                        let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
                        if (cx.block)(above).is_some_and(tuff_world_model::chunk::is_air) {
                            return Some(pos);
                        }
                        if fallback.is_none() && mob.distance_sq(pos.center()) >= 1.5 * 1.5 {
                            fallback = Some(pos);
                        }
                    }
                }
            }
        }
        fallback
    }
}

impl Goal for FindWater {
    fn flags(&self) -> Flags {
        match self.walk {
            Some(_) => Flags::MOVE,
            None => Flags::NONE,
        }
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let here = block_pos(mob.base.position);
        match self.walk {
            Some(_) => {
                mob.target.is_none()
                    && mob.navigation.is_done()
                    && !is_water(cx, here)
                    && cx.tick >= self.next_search
            }
            None => mob.base.on_ground && !is_water(cx, here),
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        match self.walk {
            Some(_) => !mob.navigation.is_done(),
            None => self.can_use(mob, cx),
        }
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some((range, speed)) = self.walk else {
            let [x, y, z] = mob.base.position;
            let (min, max) = (
                block_pos([x - 2.0, y - 2.0, z - 2.0]),
                block_pos([x + 2.0, y, z + 2.0]),
            );
            let found = (min.z..=max.z)
                .flat_map(|z| {
                    (min.y..=max.y)
                        .flat_map(move |y| (min.x..=max.x).map(move |x| BlockPos::new(x, y, z)))
                })
                .find(|pos| is_water(cx, *pos));
            if let Some(pos) = found {
                mob.movement
                    .set_wanted([pos.x as f64, pos.y as f64, pos.z as f64], 1.0);
            }
            return;
        };
        self.next_search = cx.tick + 40;
        if let Some(pos) = self.nearest_water(mob, range, cx) {
            mob.move_to_position(
                [pos.x as f64 + 0.5, pos.y as f64, pos.z as f64 + 0.5],
                speed,
                cx,
            );
        }
    }
}

/// Whether it is day where the mob is: a dimension with a moving sun and
/// the sky barely darkened.
pub fn bright_outside(cx: &TickContext) -> bool {
    !cx.shared
        .dimension(cx.dimension)
        .environment()
        .has_fixed_time
        && cx.sky_darken < 4
}

/// Heads for the nearest block a test accepts (searched outward from its
/// feet, a level below them first), then stays on it for a while.
#[derive(Debug)]
pub struct MoveToBlock {
    speed: f64,
    range: i32,
    vertical: i32,
    /// The first search level, relative to the block under its feet.
    pub vertical_start: i32,
    valid: fn(&MobCore, BlockPos, &TickContext) -> bool,
    /// Ticks between path refreshes while on its way.
    pub recalculate_every: i32,
    next_start: i32,
    pub try_ticks: i32,
    max_stay: i32,
    /// The block it heads for.
    pub pos: BlockPos,
    reached: bool,
    /// How close (squared) to the spot above the block counts as there.
    pub accepted_sq: f64,
    /// A fixed ticks-between-searches, instead of a random 200..400.
    pub search_every: Option<i32>,
}

impl MoveToBlock {
    pub const GIVE_UP_TICKS: i32 = 1200;

    pub fn new(
        speed: f64,
        range: i32,
        vertical: i32,
        valid: fn(&MobCore, BlockPos, &TickContext) -> bool,
    ) -> Self {
        Self {
            speed,
            range,
            vertical,
            vertical_start: 0,
            valid,
            recalculate_every: 40,
            next_start: 0,
            try_ticks: 0,
            max_stay: 0,
            pos: BlockPos::new(0, 0, 0),
            reached: false,
            accepted_sq: 1.0,
            search_every: None,
        }
    }

    pub fn reached(&self) -> bool {
        self.reached
    }

    pub fn is_valid(&self, mob: &MobCore, cx: &TickContext) -> bool {
        (self.valid)(mob, self.pos, cx)
    }

    fn find_nearest(&mut self, mob: &MobCore, cx: &TickContext) -> bool {
        let here = block_pos(mob.base.position);
        let step = |v: i32| if v > 0 { -v } else { 1 - v };
        let mut y = self.vertical_start;
        while y <= self.vertical {
            for r in 0..self.range {
                let mut x = 0;
                while x <= r {
                    let mut z = if x < r && x > -r { r } else { 0 };
                    while z <= r {
                        let pos = BlockPos::new(here.x + x, here.y + y - 1, here.z + z);
                        if (self.valid)(mob, pos, cx) {
                            self.pos = pos;
                            return true;
                        }
                        z = step(z);
                    }
                    x = step(x);
                }
            }
            y = step(y);
        }
        false
    }

    fn move_to(&self, mob: &mut MobCore, cx: &mut TickContext) {
        let target = [
            f64::from(self.pos.x) + 0.5,
            f64::from(self.pos.y + 1),
            f64::from(self.pos.z) + 0.5,
        ];
        mob.move_to_position(target, self.speed, cx);
    }
}

impl Goal for MoveToBlock {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::JUMP
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if self.next_start > 0 {
            self.next_start -= 1;
            return false;
        }
        self.next_start = reduced_delay(
            self.search_every
                .unwrap_or_else(|| 200 + cx.rng.gen_range(0..200)),
        );
        self.find_nearest(mob, cx)
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        self.try_ticks >= -self.max_stay
            && self.try_ticks <= Self::GIVE_UP_TICKS
            && self.is_valid(mob, cx)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.move_to(mob, cx);
        self.try_ticks = 0;
        let spread = cx.rng.gen_range(0..1200) + 1200;
        self.max_stay = cx.rng.gen_range(0..spread) + 1200;
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let above = BlockPos::new(self.pos.x, self.pos.y + 1, self.pos.z);
        if mob.distance_sq(above.center()) >= self.accepted_sq {
            self.reached = false;
            self.try_ticks += 1;
            if self.try_ticks % self.recalculate_every == 0 {
                let target = [
                    f64::from(above.x) + 0.5,
                    f64::from(above.y),
                    f64::from(above.z) + 0.5,
                ];
                mob.move_to_position(target, self.speed, cx);
            }
            return;
        }
        self.reached = true;
        self.try_ticks -= 1;
    }
}

#[cfg(test)]
#[path = "goals_look_tests.rs"]
mod look_tests;

#[cfg(test)]
#[path = "goals_flying_tempt_tests.rs"]
mod flying_tempt_tests;

mod follow;
pub use follow::*;

mod look;
pub use look::*;

#[cfg(test)]
#[path = "goals_powder_snow_tests.rs"]
mod powder_snow_tests;
