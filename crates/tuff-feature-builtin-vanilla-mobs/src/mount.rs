//! A mob riding another mob steers it: the rider's goals walk the
//! vehicle's navigation and move control, as if the two were one body.
//!
//! Each runs its own tick, the vehicle first. The vehicle lends a copy of
//! its navigation and move control through the region's [`Mounts`]; its
//! controlling passenger's goals work on that copy, and any path they ask
//! for is left as a request the vehicle searches with its own size and
//! position. The vehicle takes the copy back on its next tick. Its own
//! movement, jump and look goals stand down while it is steered.

use std::collections::HashMap;

use tuff_core::world::entity::TickContext;
use tuff_world_model::BlockPos;

use super::MobCore;
use super::control::MoveControl;
use super::navigation::Navigation;
use tuff_vanilla_data::registry::EntityType;

/// A path a steering passenger asked its vehicle to find.
#[derive(Debug, Clone, Copy)]
pub struct PathRequest {
    pub pos: BlockPos,
    pub reach: i32,
    pub above: bool,
    pub speed: Option<f64>,
}

#[derive(Debug)]
struct Lent {
    tick: u64,
    navigation: Navigation,
    movement: MoveControl,
}

#[derive(Debug)]
struct Returned {
    tick: u64,
    navigation: Navigation,
    movement: MoveControl,
    requests: Vec<PathRequest>,
}

/// Steering handed between vehicles and the mobs riding them, by vehicle id.
#[derive(Debug, Default)]
pub struct Mounts {
    lent: HashMap<i32, Lent>,
    returned: HashMap<i32, Returned>,
    /// Mobs able to steer what they ride, by id, with the tick they said so
    /// and their kind.
    controllers: HashMap<i32, (u64, EntityType)>,
}

/// What a steering passenger borrowed for its goals this tick.
pub(super) struct Borrowed {
    vehicle: i32,
    navigation: Navigation,
    movement: MoveControl,
}

fn first_passenger(core: &MobCore) -> Option<i32> {
    let base = &core.base;
    let first = tuff_core::world::entity::all_passengers(base)
        .first()
        .copied()?;
    base.entity_riders.contains(&first).then_some(first)
}

/// Whether a mob of `kind` can steer what it rides.
fn can_control(kind: EntityType) -> bool {
    !tuff_datapack::tags::is(
        tuff_vanilla_data::tag::entity_type::key::NON_CONTROLLING_RIDER,
        kind,
    )
}

/// Whether a controlling `rider` takes over a `vehicle`'s movement: a
/// ravager carrying a fellow raider keeps walking and targeting on its own.
fn steers(vehicle: EntityType, rider: EntityType) -> bool {
    !(vehicle == EntityType::Ravager && super::raider::is_raider(rider))
}

impl MobCore {
    /// Start of a vehicle's AI: whether a mob riding it steers it now, and
    /// if so its navigation and move control come back from that rider with
    /// the paths it asked for searched.
    pub(super) fn take_back_steering(&mut self, cx: &mut TickContext) {
        let id = self.base.id;
        let controller = first_passenger(self).filter(|rider| {
            !self.no_ai
                && cx
                    .state
                    .get::<Mounts>()
                    .controllers
                    .get(rider)
                    .is_some_and(|(at, kind)| {
                        cx.tick.saturating_sub(*at) <= 1 && steers(self.base.kind, *kind)
                    })
        });
        let mut mounts = cx.state.get::<Mounts>();
        mounts.lent.remove(&id);
        let returned = mounts.returned.remove(&id);
        drop(mounts);
        self.steered = controller.is_some();
        if self.steered != self.goals_steered {
            self.update_control_flags();
        }
        if !self.steered {
            return;
        }
        let Some(returned) = returned.filter(|r| cx.tick.saturating_sub(r.tick) <= 1) else {
            return;
        };
        self.navigation = returned.navigation;
        self.movement = returned.movement;
        if let Some(request) = returned.requests.last().copied() {
            let path = self.create_path(request.pos, request.reach, request.above, cx);
            let speed = request.speed.unwrap_or(self.navigation.speed_modifier);
            self.move_along(path, speed, cx);
        }
    }

    /// End of a steered vehicle's AI: a copy of its steering for its rider.
    pub(super) fn lend_steering(&self, cx: &mut TickContext) {
        if !self.steered {
            return;
        }
        cx.state.get::<Mounts>().lent.insert(
            self.base.id,
            Lent {
                tick: cx.tick,
                navigation: self.navigation.clone(),
                movement: self.movement.clone(),
            },
        );
    }

    /// Before a passenger's goals: if it is the first passenger of a mob
    /// that lent its steering this tick, its goals work on that.
    pub(super) fn borrow_steering(&mut self, cx: &mut TickContext) -> Option<Borrowed> {
        let mount = self.base.vehicle.as_ref()?;
        let vehicle = mount.id;
        if self.not_a_mob || !can_control(self.base.kind) {
            return None;
        }
        if mount.passengers.first() != Some(&self.base.id) && !mount.passengers.is_empty() {
            return None;
        }
        let mut mounts = cx.state.get::<Mounts>();
        if cx.tick.is_multiple_of(100) {
            let tick = cx.tick;
            mounts
                .controllers
                .retain(|_, (at, _)| tick.saturating_sub(*at) <= 1);
            mounts.lent.retain(|_, l| tick.saturating_sub(l.tick) <= 1);
            mounts
                .returned
                .retain(|_, r| tick.saturating_sub(r.tick) <= 1);
        }
        mounts
            .controllers
            .insert(self.base.id, (cx.tick, self.base.kind));
        let lent = mounts.lent.remove(&vehicle).filter(|l| l.tick == cx.tick)?;
        let navigation = std::mem::replace(&mut self.navigation, lent.navigation);
        let movement = std::mem::replace(&mut self.movement, lent.movement);
        self.steer_requests = Some(Vec::new());
        Some(Borrowed {
            vehicle,
            navigation,
            movement,
        })
    }

    /// After a passenger's goals: the vehicle's steering goes back to it.
    pub(super) fn give_back_steering(&mut self, borrowed: Borrowed, cx: &mut TickContext) {
        let navigation = std::mem::replace(&mut self.navigation, borrowed.navigation);
        let movement = std::mem::replace(&mut self.movement, borrowed.movement);
        let requests = self.steer_requests.take().unwrap_or_default();
        cx.state.get::<Mounts>().returned.insert(
            borrowed.vehicle,
            Returned {
                tick: cx.tick,
                navigation,
                movement,
                requests,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_ravager_drives_itself_under_a_raider_but_not_under_other_mobs() {
        assert!(!steers(EntityType::Ravager, EntityType::Pillager));
        assert!(!steers(EntityType::Ravager, EntityType::Evoker));
        assert!(steers(EntityType::Ravager, EntityType::Zombie));
        assert!(steers(EntityType::Chicken, EntityType::Zombie));
        assert!(steers(EntityType::Spider, EntityType::Pillager));
    }
}
