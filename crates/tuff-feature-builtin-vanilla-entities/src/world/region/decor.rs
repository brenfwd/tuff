//! Item frames and armor stands placed from items, by players and
//! dispensers.

use rand::Rng;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};
use tuff_vanilla_data::{BlockState, Item};

use crate::world::cushion::{self, Cushion};
use crate::world::item_frame::{self, ItemFrame};
use crate::world::painting::{self, Painting};
use tuff_core::world::entity::RegionEntity;
use tuff_core::world::game_event::{Context as GameContext, GameEvent};
use tuff_core::world::messages::{ActionOutcome, Place};
use tuff_core::world::region_api::{DispenseOutcome, RegionHooks, RegionWorld};
use tuff_core::world::{PLAYER_HALF_WIDTH, PLAYER_HEIGHT, physics};
use tuff_item::item::Components;
use tuff_nbt::Tag;
use tuff_world_model::pos::ChunkPos;
use tuff_world_model::{BlockPos, Face};

const STAND_PLACE_VOLUME: f32 = 0.75;
const STAND_PLACE_PITCH: f32 = 0.8;
const ENTITY_DATA: &str = "minecraft:entity_data";

pub struct DecorHooks;

fn facing_of_face(face: Face) -> Facing {
    match face {
        Face::Down => Facing::Down,
        Face::Up => Facing::Up,
        Face::North => Facing::North,
        Face::South => Facing::South,
        Face::West => Facing::West,
        Face::East => Facing::East,
    }
}

fn yaw_of(facing: Facing) -> f32 {
    match facing {
        Facing::South => 0.0,
        Facing::West => 90.0,
        Facing::North => 180.0,
        Facing::East => 270.0,
        Facing::Up | Facing::Down => 270.0,
    }
}

/// What the item's own data sets on the entity it places: its custom name
/// and the `entity_data` component merged over the new entity's saved form.
fn apply_stack(
    world: &mut dyn RegionWorld,
    entity: Box<dyn RegionEntity>,
    components: &Components,
) -> Box<dyn RegionEntity> {
    let mut entity = entity;
    if let Some(name) = &components.custom_name {
        entity.base_mut().custom_name = Some(name.to_nbt());
    }
    let Some(Tag::Compound(data)) = components.other.get(ENTITY_DATA) else {
        return entity;
    };
    let mut saved = tuff_core::world::entity::save(entity.as_ref());
    let id = entity.base().id;
    for (key, value) in data.iter() {
        if !matches!(key, "id" | "Pos" | "UUID") {
            saved.insert(key, value.clone());
        }
    }
    world
        .shared()
        .entity_types
        .load(&saved, id)
        .unwrap_or(entity)
}

fn overlaps(a: [f64; 6], b: [f64; 6]) -> bool {
    physics::intersects(a, b)
}

fn entity_in_the_way(world: &mut dyn RegionWorld, area: [f64; 6]) -> bool {
    let players = world.present_players().iter().any(|p| {
        let [x, y, z] = p.position;
        overlaps(
            area,
            [
                x - PLAYER_HALF_WIDTH,
                y,
                z - PLAYER_HALF_WIDTH,
                x + PLAYER_HALF_WIDTH,
                y + PLAYER_HEIGHT,
                z + PLAYER_HALF_WIDTH,
            ],
        )
    });
    players
        || world
            .entity_ids()
            .into_iter()
            .filter_map(|id| world.entity(id))
            .any(|t| overlaps(area, t.entity.bounds()))
}

fn stand_box(pos: BlockPos) -> [f64; 6] {
    let size = EntityType::ArmorStand.size();
    let half = f64::from(size.width) / 2.0;
    let (x, y, z) = (
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    );
    [
        x - half,
        y,
        z - half,
        x + half,
        y + f64::from(size.height),
        z + half,
    ]
}

fn spawn_stand(
    world: &mut dyn RegionWorld,
    pos: BlockPos,
    yaw: f32,
    components: &Components,
    placed_by_hand: bool,
) -> bool {
    let position = [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    ];
    if !world.owns_chunk(ChunkPos::of_position(position)) {
        return false;
    }
    let id = world.shared().next_entity_id();
    let Some(stand) = world
        .shared()
        .entity_types
        .create(EntityType::ArmorStand, id, position, yaw)
    else {
        return false;
    };
    let stand = apply_stack(world, stand, components);
    world.add_entity(stand);
    if !placed_by_hand {
        return true;
    }
    world.play_sound(
        SoundEvent::EntityArmorStandPlace,
        pos,
        STAND_PLACE_VOLUME,
        STAND_PLACE_PITCH,
        None,
    );
    true
}

fn place_stand(world: &mut dyn RegionWorld, place: &Place, state: BlockState) -> Option<()> {
    if place.face == Face::Down {
        return None;
    }
    let pos = if state.is_replaceable() {
        place.target
    } else {
        place.target.offset(place.face)
    };
    if !world.owns(pos) {
        return None;
    }
    let area = stand_box(pos);
    if !physics::no_collision(area, &|p| world.block(p)) || entity_in_the_way(world, area) {
        return None;
    }
    let yaw = (((wrap_degrees(place.yaw - 180.0) + 22.5) / 45.0).floor()) * 45.0;
    spawn_stand(world, pos, yaw, &place.components, true).then_some(())
}

fn wrap_degrees(degrees: f32) -> f32 {
    let wrapped = degrees.rem_euclid(360.0);
    if wrapped >= 180.0 {
        wrapped - 360.0
    } else {
        wrapped
    }
}

fn place_frame(world: &mut dyn RegionWorld, place: &Place, item: Item) -> Option<()> {
    if !place.may_build {
        return None;
    }
    let kind = match item {
        Item::GlowItemFrame => EntityType::GlowItemFrame,
        _ => EntityType::ItemFrame,
    };
    let facing = facing_of_face(place.face);
    let pos = place.target.offset(place.face);
    let shape = world.dimension_info().shape;
    if pos.y < shape.min_y || pos.y > shape.max_y() || !world.owns(pos) {
        return None;
    }
    let fits = {
        let others: Vec<[f64; 6]> = world
            .entity_ids()
            .into_iter()
            .filter_map(|id| world.entity(id))
            .filter(|t| painting::is_hanging(t.entity.base().kind))
            .map(|t| t.entity.bounds())
            .collect();
        item_frame::survives(pos, facing, &|p| world.block(p), others.into_iter())
    };
    if !fits {
        return None;
    }
    let id = world.shared().next_entity_id();
    let uuid = world.rng().r#gen();
    let frame: Box<dyn RegionEntity> = Box::new(ItemFrame::new(kind, id, uuid, pos, facing));
    let frame = apply_stack(world, frame, &place.components);
    let sound_pos = pos;
    world.add_entity(frame);
    world.play_sound(SoundEvent::EntityItemFramePlace, sound_pos, 1.0, 1.0, None);
    Some(())
}

fn place_cushion(world: &mut dyn RegionWorld, place: &Place, item: Item) -> Option<()> {
    if !place.may_build || place.face != Face::Up {
        return None;
    }
    let color = cushion::color_of(item)?;
    let cell = place.target.offset(Face::Up);
    if !world.owns(cell) {
        return None;
    }
    let position = [
        f64::from(cell.x) + 0.5,
        f64::from(place.target.y) + f64::from(place.cursor[1]),
        f64::from(cell.z) + 0.5,
    ];
    let bounds = cushion::cushion_box(position);
    if !cushion::can_be_placed_at(bounds, &|p| world.block(p)) {
        return None;
    }
    let crowded = world
        .entity_ids()
        .into_iter()
        .filter_map(|id| world.entity(id))
        .any(|t| {
            t.entity.base().kind == EntityType::Cushion && overlaps(bounds, t.entity.bounds())
        });
    if crowded {
        return None;
    }
    let yaw = (((place.yaw / 90.0 + 0.5).floor() as i32) & 3) as f32 * 90.0;
    let id = world.shared().next_entity_id();
    let uuid = world.rng().r#gen();
    let entity: Box<dyn RegionEntity> = Box::new(Cushion::new(id, uuid, position, yaw, color));
    let entity = apply_stack(world, entity, &place.components);
    world.add_entity(entity);
    world.play_sound(SoundEvent::EntityCushionPlace, cell, 0.75, 0.8, None);
    world.game_event(GameEvent::EntityPlace, position, GameContext::default());
    Some(())
}

fn place_painting(world: &mut dyn RegionWorld, place: &Place) -> Option<()> {
    if !place.may_build
        || !matches!(
            place.face,
            Face::North | Face::South | Face::West | Face::East
        )
    {
        return None;
    }
    let facing = facing_of_face(place.face);
    let pos = place.target.offset(place.face);
    let shape = world.dimension_info().shape;
    if pos.y < shape.min_y || pos.y > shape.max_y() || !world.owns(pos) {
        return None;
    }
    let others: Vec<painting::Hanging> = world
        .entity_ids()
        .into_iter()
        .filter_map(|id| world.entity(id))
        .filter(|t| painting::is_hanging(t.entity.base().kind))
        .map(|t| (t.entity.base().kind, t.entity.bounds()))
        .collect();
    let fits = painting::fitting(pos, facing, &|p| world.block(p), &others);
    if fits.is_empty() {
        return None;
    }
    let variant = fits[world.rng().gen_range(0..fits.len())];
    let id = world.shared().next_entity_id();
    let uuid = world.rng().r#gen();
    let entity: Box<dyn RegionEntity> = Box::new(Painting::new(id, uuid, pos, facing, variant));
    let entity = apply_stack(world, entity, &place.components);
    world.add_entity(entity);
    world.play_sound(SoundEvent::EntityPaintingPlace, pos, 1.0, 1.0, None);
    Some(())
}

impl RegionHooks for DecorHooks {
    fn use_item_on(
        &self,
        world: &mut dyn RegionWorld,
        place: &Place,
        state: BlockState,
    ) -> Option<ActionOutcome> {
        let item = place.held?;
        let placed = match item {
            Item::ItemFrame | Item::GlowItemFrame => place_frame(world, place, item),
            Item::Painting => place_painting(world, place),
            other if cushion::color_of(other).is_some() => place_cushion(world, place, other),
            Item::ArmorStand => place_stand(world, place, state),
            _ => return None,
        };
        placed.map(|()| ActionOutcome::Placed {
            off_hand: place.off_hand,
            item,
            placement: None,
        })
    }

    fn dispense(
        &self,
        world: &mut dyn RegionWorld,
        pos: BlockPos,
        facing: Facing,
        _origin: [f64; 3],
        stack: &tuff_core::world::region_api::Fired,
    ) -> Option<DispenseOutcome> {
        if stack.item() != Item::ArmorStand {
            return None;
        }
        let (sx, sy, sz) = facing.step();
        let front = BlockPos::new(pos.x + sx, pos.y + sy, pos.z + sz);
        if !world.owns(front) {
            return Some(DispenseOutcome::Remote);
        }
        let placed = spawn_stand(world, front, yaw_of(facing), &stack.components, false);
        Some(if placed {
            DispenseOutcome::Dispensed
        } else {
            DispenseOutcome::Kept
        })
    }
}
