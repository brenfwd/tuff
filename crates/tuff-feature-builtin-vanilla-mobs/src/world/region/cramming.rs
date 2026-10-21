//! Players squeezed among more pushable things than the
//! `max_entity_cramming` rule allows take cramming damage, as mobs do.
//! Passengers push but are not counted, nor are climbers (who are not
//! pushable); nothing sharing the player's vehicle stack pushes it.

use rand::Rng;
use tuff_vanilla_data::damage_type;
use tuff_vanilla_data::registry::EntityType;

use tuff_core::world::messages::PlayerEvent;
use tuff_core::world::physics;
use tuff_core::world::player::on_climbable;
use tuff_core::world::region_api::{RegionWorld, SeenPlayer};
use tuff_core::world::scoreboard::Member;
use tuff_world_model::BlockPos;

const DAMAGE: f32 = 6.0;

fn team_of(players: &[SeenPlayer], uuid: u128) -> Member<'_> {
    match players.iter().find(|p| p.uuid == uuid) {
        Some(player) => Member::Player(&player.name),
        None => Member::Entity(uuid),
    }
}

fn pushable(kind: EntityType, living: Option<tuff_core::world::entity::LivingSummary>) -> bool {
    living.is_some_and(|l| l.alive && !l.unpushable)
        || tuff_vanilla_data::Item::of_boat(kind).is_some()
        || kind.name().contains("minecart")
}

fn root(world: &dyn RegionWorld, mut id: i32) -> i32 {
    for _ in 0..16 {
        match world
            .entity(id)
            .and_then(|t| t.entity.base().vehicle.as_ref().map(|m| m.id))
        {
            Some(vehicle) => id = vehicle,
            None => break,
        }
    }
    id
}

pub fn tick(world: &mut dyn RegionWorld) {
    let max = world.shared().rule_int("max_entity_cramming");
    if max <= 0 {
        return;
    }
    let players = world.seen_player_details();
    if players.is_empty() {
        return;
    }
    let ids = world.entity_ids();
    let mut boxes = Vec::new();
    for id in &ids {
        let Some(tracked) = world.entity(*id) else {
            continue;
        };
        let base = tracked.entity.base();
        if !pushable(base.kind, tracked.entity.living()) {
            continue;
        }
        let rides = base.vehicle.is_some();
        let riders: Vec<i32> = base.riders.iter().map(|r| r.id).collect();
        let owner = tracked.entity.living().and_then(|l| l.owner);
        boxes.push((
            *id,
            owner.unwrap_or(base.uuid),
            tracked.entity.bounds(),
            rides,
            riders,
        ));
    }
    let teams = world.shared().scoreboard.teams();
    let mut crushed = Vec::new();
    for player in &players {
        if !world.owns(BlockPos::containing(player.position)) {
            continue;
        }
        let [x, y, z] = player.position;
        let own = [
            x - 0.3,
            y,
            z - 0.3,
            x + 0.3,
            y + player.looks.height,
            z + 0.3,
        ];
        let vehicle = boxes
            .iter()
            .find(|(_, _, _, _, riders)| riders.contains(&player.handle.id))
            .map(|(id, ..)| root(world, *id));
        let member = Member::Player(&player.name);
        let mut total = 0;
        let mut counted = 0;
        for (id, uuid, bounds, rides, _) in &boxes {
            if !physics::intersects(*bounds, own)
                || vehicle.is_some_and(|v| root(world, *id) == v)
                || !teams.pushes(member, team_of(&players, *uuid))
            {
                continue;
            }
            total += 1;
            counted += i32::from(!rides);
        }
        for other in &players {
            if other.handle.id == player.handle.id {
                continue;
            }
            let [ox, oy, oz] = other.position;
            let theirs = [
                ox - 0.3,
                oy,
                oz - 0.3,
                ox + 0.3,
                oy + other.looks.height,
                oz + 0.3,
            ];
            if !physics::intersects(theirs, own)
                || on_climbable(other.position, |pos| world.block(pos))
                || !teams.pushes(member, Member::Player(&other.name))
            {
                continue;
            }
            let rides = boxes
                .iter()
                .any(|(_, _, _, _, riders)| riders.contains(&other.handle.id));
            total += 1;
            counted += i32::from(!rides);
        }
        if total > max - 1 && world.rng().gen_range(0..4) == 0 && counted > max - 1 {
            crushed.push(player.handle.clone());
        }
    }
    for handle in crushed {
        handle.send(PlayerEvent::Hurt {
            amount: DAMAGE,
            kind: &damage_type::CRAMMING,
            attacker: None,
            position: None,
        });
    }
}
