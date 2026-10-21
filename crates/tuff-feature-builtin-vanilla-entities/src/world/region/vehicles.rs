//! Boats and minecarts placed from items, by players and dispensers.

use tuff_vanilla_data::BlockState;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::tag;

use crate::world::boat::{self, Boat};
use crate::world::minecart::{self, Minecart};
use tuff_core::world::game_event::{Context as GameContext, GameEvent};
use tuff_core::world::messages::{ActionOutcome, BoatPlace, HandEffect, Place, PlayerEvent};
use tuff_core::world::physics;
use tuff_core::world::raycast::{self, FluidMode};
use tuff_core::world::region_api::{DispenseOutcome, RegionHooks, RegionWorld};
use tuff_item::item::ItemStack;
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;
use tuff_world_model::pos::ChunkPos;

/// Placing vehicles as region hooks.
pub struct VehicleHooks;

impl RegionHooks for VehicleHooks {
    fn use_item_on(
        &self,
        world: &mut dyn RegionWorld,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        let item = place.held?;
        let kind = minecart::of_minecart_item(item)?;
        if !minecart::is_rail_block(state) {
            return None;
        }
        if !place_minecart(world, kind, place.target, state) {
            return Some(ActionOutcome::Rejected(world.corrections(&[place.target])));
        }
        Some(ActionOutcome::Placed {
            off_hand: place.off_hand,
            item,
            placement: None,
        })
    }

    fn place_boat(&self, world: &mut dyn RegionWorld, request: &BoatPlace) {
        place_boat(world, request);
    }

    fn dispense(
        &self,
        world: &mut dyn RegionWorld,
        pos: BlockPos,
        facing: Facing,
        _origin: [f64; 3],
        stack: &ItemStack,
    ) -> Option<DispenseOutcome> {
        if let Some(kind) = stack.item.boat() {
            return Some(dispense_boat(world, pos, facing, kind, stack));
        }
        let kind = minecart::of_minecart_item(stack.item)?;
        Some(dispense_minecart(world, pos, facing, kind, stack))
    }
}

/// The yaw an entity facing `facing` has.
fn yaw_of(facing: Facing) -> f32 {
    match facing {
        Facing::South => 0.0,
        Facing::West => 90.0,
        Facing::North => 180.0,
        Facing::East => 270.0,
        _ => 0.0,
    }
}

fn is_water(state: Option<BlockState>) -> bool {
    state.is_some_and(|s| tuff_datapack::tags::is(tag::fluid::key::WATER, s.fluid_state().fluid()))
}

/// A dispenser puts a boat on the water in front of it (or on the water
/// just below that), else drops the item.
fn dispense_boat(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    facing: Facing,
    kind: EntityType,
    stack: &ItemStack,
) -> DispenseOutcome {
    let (sx, sy, sz) = facing.step();
    let front = BlockPos::new(pos.x + sx, pos.y + sy, pos.z + sz);
    let below = BlockPos::new(front.x, front.y - 1, front.z);
    let lift = if is_water(world.block(front)) {
        1.0
    } else if world.block(front).is_some_and(is_air) && is_water(world.block(below)) {
        0.0
    } else {
        return DispenseOutcome::Dropped;
    };
    let reach = 0.5625 + f64::from(kind.size().width) / 2.0;
    let center = pos.center();
    let position = [
        center[0] + f64::from(sx) * reach,
        center[1] + f64::from(sy as f32 * 1.125) + lift,
        center[2] + f64::from(sz) * reach,
    ];
    if !world.owns_chunk(ChunkPos::of_position(position)) {
        return DispenseOutcome::Dropped;
    }
    let id = world.shared().next_entity_id();
    let mut placed = Boat::new(kind, id, position, yaw_of(facing), world.rng());
    placed.base.custom_name = stack.components.custom_name.as_ref().map(|n| n.to_nbt());
    world.add_entity(Box::new(placed));
    entity_placed(world, position);
    DispenseOutcome::Dispensed
}

fn pickable_kind(kind: EntityType) -> bool {
    !matches!(
        kind,
        EntityType::Item
            | EntityType::ExperienceOrb
            | EntityType::AreaEffectCloud
            | EntityType::Marker
            | EntityType::LightningBolt
            | EntityType::FishingBobber
    )
}

fn entity_placed(world: &mut dyn RegionWorld, position: [f64; 3]) {
    world.game_event(GameEvent::EntityPlace, position, GameContext::default());
}

fn is_slope(state: BlockState) -> bool {
    state
        .get("shape")
        .is_some_and(|s| s.starts_with("ascending"))
}

/// A dispenser puts a minecart on the rail in front of it (or on the rail
/// just below that), else drops the item.
fn dispense_minecart(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    facing: Facing,
    kind: EntityType,
    stack: &ItemStack,
) -> DispenseOutcome {
    let (sx, sy, sz) = facing.step();
    let front = BlockPos::new(pos.x + sx, pos.y + sy, pos.z + sz);
    let air = tuff_vanilla_data::Block::Air.default_state();
    let ahead = world.block(front).unwrap_or(air);
    let lift = if minecart::is_rail_block(ahead) {
        if is_slope(ahead) { 0.6 } else { 0.1 }
    } else if is_air(ahead) {
        let below = world
            .block(BlockPos::new(front.x, front.y - 1, front.z))
            .unwrap_or(air);
        if !minecart::is_rail_block(below) {
            return DispenseOutcome::Dropped;
        }
        if facing == Facing::Down || !is_slope(below) {
            -0.9
        } else {
            -0.4
        }
    } else {
        return DispenseOutcome::Dropped;
    };
    let center = pos.center();
    let position = [
        center[0] + f64::from(sx) * 1.125,
        center[1].floor() + f64::from(sy) + lift,
        center[2] + f64::from(sz) * 1.125,
    ];
    if !world.owns_chunk(ChunkPos::of_position(position)) {
        return DispenseOutcome::Dropped;
    }
    let id = world.shared().next_entity_id();
    let mut cart = Minecart::new(kind, id, position, 0.0, world.rng());
    cart.base.custom_name = stack.components.custom_name.as_ref().map(|n| n.to_nbt());
    if improved(world) {
        cart.place_on_rails(&|pos| world.block(pos));
    }
    world.add_entity(Box::new(cart));
    entity_placed(world, position);
    DispenseOutcome::Dispensed
}

/// Whether the world moves minecarts the newer way.
fn improved(world: &dyn RegionWorld) -> bool {
    world
        .shared()
        .datapacks
        .has_feature(minecart::track::FEATURE)
}

/// A boat item used on a block or fluid: the boat goes where the view ray
/// hits, facing the player's way, if it fits there.
fn place_boat(world: &mut dyn RegionWorld, request: &BoatPlace) {
    let reject = |world: &dyn RegionWorld, at: &[BlockPos]| {
        request.player.send(PlayerEvent::ActionResult {
            sequence: request.sequence,
            outcome: ActionOutcome::Rejected(world.corrections(at)),
        });
    };
    let to = [0, 1, 2].map(|i| request.eye[i] + request.look[i] * request.reach);
    let hit = {
        let level = |p: BlockPos| {
            world
                .block(p)
                .unwrap_or(tuff_vanilla_data::Block::Air.default_state())
        };
        raycast::cast(request.eye, to, FluidMode::Any, &level)
    };
    let Some(hit) = hit else {
        return reject(world, &[]);
    };
    let chunk = ChunkPos::of_position(hit.location);
    if !world.owns_chunk(chunk) || world.chunk(chunk).is_none() {
        return reject(world, &[hit.pos]);
    }
    let id = world.shared().next_entity_id();
    let mut placed = Boat::new(request.kind, id, hit.location, request.yaw, world.rng());
    placed.base.custom_name = request.name.clone();
    let bounds = placed.base.bounds();
    let fits = physics::no_collision(bounds, &|pos| world.block(pos));
    let look_box = physics::inflate(
        physics::expand_towards(
            [
                request.eye[0],
                request.eye[1],
                request.eye[2],
                request.eye[0],
                request.eye[1],
                request.eye[2],
            ],
            [
                request.look[0] * request.reach,
                request.look[1] * request.reach,
                request.look[2] * request.reach,
            ],
        ),
        [1.0; 3],
    );
    let eye_inside = world
        .entity_ids()
        .into_iter()
        .filter_map(|id| world.entity(id))
        .any(|t| {
            let b = t.entity.bounds();
            physics::intersects(b, look_box)
                && pickable_kind(t.entity.base().kind)
                && b[0] <= request.eye[0]
                && request.eye[0] <= b[3]
                && b[1] <= request.eye[1]
                && request.eye[1] <= b[4]
                && b[2] <= request.eye[2]
                && request.eye[2] <= b[5]
        });
    if eye_inside {
        return reject(world, &[]);
    }
    let crowded = world
        .entity_ids()
        .into_iter()
        .filter_map(|id| world.entity(id))
        .any(|t| {
            boat::is_boat(t.entity.base().kind) && physics::intersects(t.entity.bounds(), bounds)
        });
    if !fits || crowded {
        return reject(world, &[hit.pos]);
    }
    world.add_entity(Box::new(placed));
    request.player.send(PlayerEvent::ActionResult {
        sequence: request.sequence,
        outcome: ActionOutcome::Used {
            off_hand: request.off_hand,
            item: request.item,
            effect: HandEffect::Consume(1),
        },
    });
}

/// A minecart item used on a rail block: the cart appears on that block,
/// halfway up a slope. With the newer movement it sits on the rail at once
/// and is refused (false) where it would overlap another cart.
fn place_minecart(
    world: &mut dyn RegionWorld,
    kind: EntityType,
    pos: BlockPos,
    rail: BlockState,
) -> bool {
    let lift = if rail.shape().is_some_and(minecart::is_slope) {
        0.5
    } else {
        0.0
    };
    let position = [
        pos.x as f64 + 0.5,
        pos.y as f64 + minecart::RAIL_RIDE_HEIGHT + lift,
        pos.z as f64 + 0.5,
    ];
    let id = world.shared().next_entity_id();
    let mut cart = Minecart::new(kind, id, position, 0.0, world.rng());
    if improved(world) {
        cart.place_on_rails(&|pos| world.block(pos));
        let bounds = cart.base.bounds();
        let crowded = world
            .entity_ids()
            .into_iter()
            .filter_map(|id| world.entity(id))
            .any(|t| {
                minecart::is_minecart(t.entity.base().kind)
                    && physics::intersects(t.entity.bounds(), bounds)
            });
        if crowded {
            return false;
        }
    }
    world.add_entity(Box::new(cart));
    true
}
