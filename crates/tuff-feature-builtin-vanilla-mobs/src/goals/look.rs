use super::*;

/// Now and then looks at a nearby player for a while.
#[derive(Debug)]
pub struct LookAtPlayer {
    pub(super) distance: f32,
    pub(super) player: Option<i32>,
    pub(super) look_time: i32,
    pub(super) probability: f32,
    pub(super) flags: Flags,
}

impl LookAtPlayer {
    pub fn new(distance: f32) -> Self {
        Self {
            distance,
            player: None,
            look_time: 0,
            probability: 0.02,
            flags: Flags::LOOK,
        }
    }

    /// The game's interact goal: always looks at a player in range, and
    /// holds still (look and move flags) while doing so.
    pub fn interact(distance: f32) -> Self {
        Self {
            probability: 1.0,
            flags: Flags::LOOK | Flags::MOVE,
            ..Self::new(distance)
        }
    }
}

impl Goal for LookAtPlayer {
    fn flags(&self) -> Flags {
        self.flags
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if cx.rng.r#gen::<f32>() >= self.probability {
            return false;
        }
        self.player = nearest_player(
            mob,
            mob.eye_position(),
            self.distance as f64,
            cx.players,
            |p| !rides(mob.base.id, p.vehicle, cx),
        )
        .map(|p| p.id);
        self.player.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(player) = self.player.and_then(|id| player(cx, id)) else {
            return false;
        };
        mob.distance_sq(player.position) <= (self.distance * self.distance) as f64
            && self.look_time > 0
    }

    fn start(&mut self, _mob: &mut MobCore, cx: &mut TickContext) {
        self.look_time = self.adjusted_delay(40 + cx.rng.gen_range(0..40));
    }

    fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.player = None;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(player) = self.player.and_then(|id| player(cx, id)) {
            mob.look_at_point(eye(player.position, player.eye_height));
            self.look_time -= 1;
        }
    }
}

/// Now and then looks at the nearest other mob for a while.
#[derive(Debug)]
pub struct LookAtMob {
    pub(super) distance: f32,
    pub(super) target: Option<i32>,
    pub(super) look_time: i32,
}

impl LookAtMob {
    pub fn new(distance: f32) -> Self {
        Self {
            distance,
            target: None,
            look_time: 0,
        }
    }
}

impl Goal for LookAtMob {
    fn flags(&self) -> Flags {
        Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        if cx.rng.r#gen::<f32>() >= 0.02 {
            return false;
        }
        let from = mob.eye_position();
        let range = f64::from(self.distance);
        self.target = cx
            .entities
            .iter()
            .filter(|e| e.id != mob.base.id && e.kind != EntityType::Player && e.living.is_some())
            .map(|e| (tuff_core::world::distance_sq(from, e.position), e.id))
            .filter(|(d, _)| *d <= range * range)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| id);
        self.target.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(target) = self.target.and_then(|id| entity(cx, id)) else {
            return false;
        };
        mob.distance_sq(target.position) <= f64::from(self.distance * self.distance)
            && self.look_time > 0
    }

    fn start(&mut self, _mob: &mut MobCore, cx: &mut TickContext) {
        self.look_time = 40 + cx.rng.gen_range(0..40);
    }

    fn stop(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.target = None;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(target) = self.target.and_then(|id| entity(cx, id)) {
            mob.look_at_point(target.position);
            self.look_time -= 1;
        }
    }
}

/// Looks at a nearby player holding a bone or the species' food, with
/// its head tilted.
#[derive(Debug)]
pub struct Beg {
    pub(super) distance: f32,
    pub(super) food: fn(tuff_vanilla_data::Item) -> bool,
    pub(super) player: Option<i32>,
    pub(super) look_time: i32,
}

impl Beg {
    pub fn new(distance: f32, food: fn(tuff_vanilla_data::Item) -> bool) -> Self {
        Self {
            distance,
            food,
            player: None,
            look_time: 0,
        }
    }

    pub(super) fn holds_interesting(&self, player: &NearbyPlayer) -> bool {
        player
            .held
            .iter()
            .flatten()
            .any(|item| *item == tuff_vanilla_data::Item::Bone || (self.food)(*item))
    }
}

impl Goal for Beg {
    fn flags(&self) -> Flags {
        Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let nearest = nearest_player(
            mob,
            mob.base.position,
            self.distance as f64,
            cx.players,
            |_| true,
        );
        match nearest.filter(|p| self.holds_interesting(p)) {
            Some(p) => {
                self.player = Some(p.id);
                true
            }
            None => false,
        }
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(player) = self.player.and_then(|id| player(cx, id)) else {
            return false;
        };
        mob.distance_sq(player.position) <= (self.distance * self.distance) as f64
            && self.look_time > 0
            && self.holds_interesting(&player)
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(tame) = &mut mob.tame {
            tame.begging = true;
        }
        self.look_time = self.adjusted_delay(40 + cx.rng.gen_range(0..40));
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        if let Some(tame) = &mut mob.tame {
            tame.begging = false;
        }
        self.player = None;
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(player) = self.player.and_then(|id| player(cx, id)) {
            mob.look_at_point(eye(player.position, player.eye_height));
            self.look_time -= 1;
        }
    }
}

/// Now and then looks in a random direction.
#[derive(Debug, Default)]
pub struct RandomLookAround {
    pub(super) relative: [f64; 2],
    pub(super) look_time: i32,
}

impl Goal for RandomLookAround {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, _mob: &mut MobCore, cx: &mut TickContext) -> bool {
        cx.rng.r#gen::<f32>() < 0.02
    }

    fn can_continue(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        self.look_time >= 0
    }

    fn start(&mut self, _mob: &mut MobCore, cx: &mut TickContext) {
        let angle = std::f64::consts::TAU * cx.rng.r#gen::<f64>();
        self.relative = [angle.cos(), angle.sin()];
        self.look_time = 20 + cx.rng.gen_range(0..20);
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.look_time -= 1;
        let [x, _, z] = mob.base.position;
        let eye = mob.eye_position();
        mob.look.set_look_at(
            [x + self.relative[0], eye[1], z + self.relative[1]],
            HEAD_ROT_SPEED,
            mob.max_head_x_rot,
        );
    }
}
