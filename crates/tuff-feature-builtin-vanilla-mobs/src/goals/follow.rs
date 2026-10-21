use super::*;

/// Stays put while ordered to sit, ahead of any movement goal.
#[derive(Debug, Default)]
pub struct Sit;

impl Goal for Sit {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::JUMP
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(tame) = &mob.tame else { return false };
        if !tame.sitting || mob.body.in_water || !mob.base.on_ground {
            return false;
        }
        let owner = tame
            .owner
            .and_then(|uuid| cx.players.iter().find(|p| p.uuid == uuid));
        !owner.is_some_and(|p| mob.distance_sq(p.position) < 144.0 && p.last_hurt_by.is_some())
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.tame.as_ref().is_some_and(|t| t.sitting)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
        mob.set_sitting_pose(true);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.set_sitting_pose(false);
    }
}

/// Follows any nearby mob of another kind, backing off when too close.
#[derive(Debug)]
pub struct FollowMob {
    pub(super) speed: f64,
    pub(super) stop_distance: f64,
    pub(super) area: f64,
    pub(super) following: Option<i32>,
    pub(super) recalculate: i32,
}

impl FollowMob {
    pub fn new(speed: f64, stop_distance: f64, area: f64) -> Self {
        Self {
            speed,
            stop_distance,
            area,
            following: None,
            recalculate: 0,
        }
    }
}

impl Goal for FollowMob {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let area = physics::inflate(mob.bounds(), [self.area; 3]);
        let found = cx.entities.iter().find(|e| {
            e.kind != mob.base.kind
                && e.id != mob.base.id
                && e.living.is_some_and(|l| l.alive)
                && physics::intersects(e.bounds, area)
        });
        self.following = found.map(|e| e.id);
        self.following.is_some()
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(other) = self
            .following
            .and_then(|id| cx.entities.iter().find(|e| e.id == id))
        else {
            return false;
        };
        !mob.navigation.is_done()
            && mob.distance_sq(other.position) > self.stop_distance * self.stop_distance
    }

    fn start(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.recalculate = 0;
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.following = None;
        mob.navigation.stop();
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        let Some(other) = self
            .following
            .and_then(|id| cx.entities.iter().find(|e| e.id == id))
            .copied()
        else {
            return;
        };
        if mob.leash.is_some() {
            return;
        }
        let leader_eye = eye(
            other.position,
            f64::from(other.living.map_or(0.0, |l| l.eye_height)),
        );
        mob.look.set_look_at(leader_eye, 10.0, mob.max_head_x_rot);
        self.recalculate -= 1;
        if self.recalculate > 0 {
            return;
        }
        self.recalculate = self.adjusted_delay(10);
        let gap_sq = mob.distance_sq(other.position);
        let comfort = self.stop_distance;
        if gap_sq > comfort * comfort {
            mob.move_to_entity(other.position, self.speed, cx);
            return;
        }
        mob.navigation.stop();
        if gap_sq <= comfort {
            let retreat = mirrored_away(mob.base.position, other.position);
            mob.move_to_position(retreat, self.speed, cx);
        }
    }
}

/// The point as far past `own` as `other` is before it, at `own`'s height.
fn mirrored_away(own: [f64; 3], other: [f64; 3]) -> [f64; 3] {
    let reflect = |axis: usize| 2.0 * own[axis] - other[axis];
    [reflect(0), own[1], reflect(2)]
}

/// A tamed animal walks back to its owner once it falls far behind, like
/// [`FollowParent`].
#[derive(Debug)]
pub struct FollowOwner {
    pub(super) speed: f64,
    pub(super) recalculate: i32,
    pub(super) flies: bool,
    pub(super) start_distance: f64,
    pub(super) stop_distance: f64,
    pub(super) old_water_cost: f32,
}

impl FollowOwner {
    /// Wolf distances: starts 10 blocks out, stops 2 away.
    pub fn new(speed: f64) -> Self {
        Self::with_distances(speed, 10.0, 2.0)
    }

    pub fn with_distances(speed: f64, start_distance: f64, stop_distance: f64) -> Self {
        Self {
            speed,
            recalculate: 0,
            flies: false,
            start_distance,
            stop_distance,
            old_water_cost: 0.0,
        }
    }

    /// Like [`FollowOwner::new`] for a flyer, which may land on leaves and
    /// needs no footing.
    pub fn flying(speed: f64) -> Self {
        Self {
            flies: true,
            ..Self::with_distances(speed, 5.0, 1.0)
        }
    }

    pub(super) fn try_teleport_to(&self, mob: &mut MobCore, owner: [f64; 3], cx: &mut TickContext) {
        let base = [
            owner[0].floor() as i32,
            owner[1].floor() as i32,
            owner[2].floor() as i32,
        ];
        for _ in 0..10 {
            let xd: i32 = cx.rng.gen_range(-3..=3);
            let zd: i32 = cx.rng.gen_range(-3..=3);
            if xd.abs() < 2 && zd.abs() < 2 {
                continue;
            }
            let yd = cx.rng.gen_range(-1..=1);
            let pos = BlockPos::new(base[0] + xd, base[1] + yd, base[2] + zd);
            if self.can_teleport_to(mob, pos, cx) {
                mob.base.position = [
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y),
                    f64::from(pos.z) + 0.5,
                ];
                mob.navigation.stop();
                mob.impulse = true;
                return;
            }
        }
    }

    pub(super) fn can_teleport_to(&self, mob: &MobCore, pos: BlockPos, cx: &TickContext) -> bool {
        let half = f64::from(mob.size.width) / 2.0;
        let height = f64::from(mob.size.height);
        let (x, y, z) = (
            f64::from(pos.x) + 0.5,
            f64::from(pos.y),
            f64::from(pos.z) + 0.5,
        );
        let area_at = |y: f64| [x - half, y, z - half, x + half, y + height, z + half];
        if !super::super::physics::no_collision(area_at(y), cx.block) {
            return false;
        }
        let Some(below) = (cx.block)(BlockPos::new(pos.x, pos.y - 1, pos.z)) else {
            return false;
        };
        if self.flies {
            return true;
        }
        if tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::LEAVES, below.block()) {
            return false;
        }
        !super::super::physics::no_collision(area_at(y - 0.5), cx.block)
    }
}

impl Goal for FollowOwner {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(tame) = &mob.tame else { return false };
        if tame.sitting {
            return false;
        }
        let Some(owner) = tame.owner else {
            return false;
        };
        let Some(player) = cx.players.iter().find(|p| p.uuid == owner) else {
            return false;
        };
        mob.distance_sq(player.position) >= self.start_distance * self.start_distance
    }

    fn can_continue(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        let Some(tame) = &mob.tame else { return false };
        if tame.sitting {
            return false;
        }
        let Some(owner) = tame.owner else {
            return false;
        };
        let Some(player) = cx.players.iter().find(|p| p.uuid == owner) else {
            return false;
        };
        !mob.navigation.is_done()
            && mob.distance_sq(player.position) > self.stop_distance * self.stop_distance
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.recalculate = 0;
        self.old_water_cost = mob.path_malus(PathType::Water);
        mob.set_path_malus(PathType::Water, 0.0);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        mob.navigation.stop();
        mob.set_path_malus(PathType::Water, self.old_water_cost);
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if let Some(player) = mob
            .tame
            .as_ref()
            .and_then(|t| t.owner)
            .and_then(|o| cx.players.iter().find(|p| p.uuid == o))
            && mob.distance_sq(player.position) < 144.0
        {
            let (position, eye_height) = (player.position, player.eye_height);
            mob.look
                .set_look_at(eye(position, eye_height), 10.0, mob.max_head_x_rot);
        }
        self.recalculate -= 1;
        if self.recalculate > 0 {
            return;
        }
        self.recalculate = self.adjusted_delay(10);
        let Some(owner) = mob.tame.as_ref().and_then(|t| t.owner) else {
            return;
        };
        if let Some(player) = cx.players.iter().find(|p| p.uuid == owner) {
            let target = player.position;
            if mob.distance_sq(target) >= 144.0 {
                self.try_teleport_to(mob, target, cx);
            } else {
                mob.move_to_entity(target, self.speed, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(other_at: [f64; 3]) -> Vec<(i32, Option<BlockPos>, [i64; 3])> {
        let mut other = crate::attack::tests::villager(7);
        other.kind = tuff_vanilla_data::registry::EntityType::Pig;
        other.position = other_at;
        other.bounds = [
            other_at[0] - 0.3,
            other_at[1],
            other_at[2] - 0.3,
            other_at[0] + 0.3,
            other_at[1] + 1.9,
            other_at[2] + 0.3,
        ];
        let mut out = Vec::new();
        crate::tests::with_entities(&[other], |mob, cx| {
            mob.base.position = [0.5, 64.0, 0.5];
            mob.navigation.climber = true;
            let mut follow = FollowMob::new(1.0, 3.0, 7.0);
            assert!(follow.can_use(mob, cx));
            follow.start(mob, cx);
            for _ in 0..13 {
                follow.tick(mob, cx);
                let target = mob.navigation.climb_target.take();
                let wanted = mob.look.wanted.map(|v| (v * 1000.0) as i64);
                out.push((follow.recalculate, target, wanted));
            }
        });
        out
    }

    #[tuff_core::watchdog]
    #[test]
    fn following_repaths_every_ten_ticks_and_backs_off_when_close() {
        let cases = [
            ([5.5, 64.0, 2.5], Some(BlockPos::new(5, 64, 2))),
            ([2.5, 64.0, 0.5], None),
            ([3.0, 64.0, 2.5], Some(BlockPos::new(3, 64, 2))),
            ([1.5, 64.0, 1.0], Some(BlockPos::new(-1, 64, 0))),
        ];
        for (at, goes) in cases {
            for (tick, (recalculate, target, wanted)) in trace(at).into_iter().enumerate() {
                assert_eq!(recalculate, 5 - (tick % 5) as i32);
                assert_eq!(target, goes.filter(|_| tick % 5 == 0));
                assert_eq!(
                    wanted,
                    [(at[0] * 1000.0) as i64, 65620, (at[2] * 1000.0) as i64]
                );
            }
        }
    }
}
