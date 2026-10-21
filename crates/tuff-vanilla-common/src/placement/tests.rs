use std::collections::HashMap;

use super::*;

const GROUND: i32 = 64;

#[tuff_core::watchdog]
#[test]
fn vines_and_lichen_hang_on_leaves() {
    let mut world = World::new(Block::Stone);
    let leaves = BlockPos::new(0, GROUND + 5, 0);
    world.set(leaves, Block::OakLeaves.default_state());
    let pos = BlockPos::new(1, GROUND + 5, 0);
    let vine = Block::Vine.default_state().with("west", "true").unwrap();
    let lichen = Block::GlowLichen
        .default_state()
        .with("west", "true")
        .unwrap();
    for state in [vine, lichen] {
        assert!(can_survive(state, pos, &|p| world.get(p)), "{state:?}");
    }
}

#[tuff_core::watchdog]
#[test]
fn moss_carpet_sides_follow_neighbouring_walls() {
    let mut world = World::new(Block::Stone);
    let pos = BlockPos::new(0, GROUND + 1, 0);
    world.set(
        BlockPos::new(1, GROUND + 1, 0),
        Block::Stone.default_state(),
    );
    let carpet = Block::PaleMossCarpet.default_state();
    let placed = mossy_carpet_state(carpet, pos, &|p| world.get(p), true);
    assert_eq!(placed.get("east"), Some("low"));
    assert_eq!(placed.get("west"), Some("none"));
    let shaped = update_shape(
        carpet,
        pos,
        Facing::East,
        Block::Stone.default_state(),
        &|p| world.get(p),
    );
    assert_eq!(shaped.get("east"), Some("low"));
    let orphan = update_shape(carpet, pos, Facing::Down, AIR, &|p| {
        if p == pos { AIR } else { world.get(p) }
    });
    assert_eq!(orphan, AIR);
}

#[tuff_core::watchdog]
#[test]
fn snow_rests_on_full_collision_faces() {
    let snow = Block::Snow.default_state();
    let on = |ground: Block| {
        let world = World::new(ground);
        can_survive(snow, BlockPos::new(0, GROUND + 1, 0), &|p| world.get(p))
    };
    assert!(on(Block::SpruceLeaves), "leaves collide as a full cube");
    assert!(on(Block::Stone));
    assert!(!on(Block::Ice));
    assert!(!on(Block::OakSlab));
}

/// A world of `floor` at y = 64 and below, air above, plus `blocks`.
struct World {
    floor: Block,
    blocks: HashMap<BlockPos, BlockState>,
}

impl World {
    fn new(floor: Block) -> Self {
        Self {
            floor,
            blocks: HashMap::new(),
        }
    }

    fn get(&self, pos: BlockPos) -> BlockState {
        self.blocks
            .get(&pos)
            .copied()
            .unwrap_or(if pos.y <= GROUND {
                self.floor.default_state()
            } else {
                AIR
            })
    }

    fn set(&mut self, pos: BlockPos, state: impl Into<BlockState>) {
        self.blocks.insert(pos, state.into());
    }

    /// Uses `item` on `face` of `clicked`, looking along `yaw` and
    /// `pitch`, and places the result with its companions.
    fn use_item(
        &mut self,
        item: Item,
        clicked: BlockPos,
        face: Facing,
        view: (f32, f32),
        sneaking: bool,
    ) -> Option<BlockState> {
        let cursor = match face {
            Facing::Up => [0.5, 1.0, 0.5],
            Facing::Down => [0.5, 0.0, 0.5],
            _ => [0.5, 0.25, 0.5],
        };
        let context = {
            let level = |p| self.get(p);
            context(
                clicked,
                face,
                cursor,
                view.0,
                view.1,
                sneaking,
                item,
                ChunkShape::OVERWORLD,
                &level,
            )
        };
        let state = {
            let level = |p| self.get(p);
            if !can_place(&context, &level) {
                return None;
            }
            placement_state(&context, &level)?
        };
        self.set(context.pos, state);
        if let Some((pos, companion)) = companions(state, context.pos) {
            self.set(pos, companion);
        }
        Some(state)
    }

    /// Replaces the block at `pos` and applies the shape updates it
    /// causes, as the region does.
    fn replace(&mut self, pos: BlockPos, state: BlockState) {
        self.set(pos, state);
        let mut changed = vec![pos];
        while let Some(pos) = changed.pop() {
            let source = self.get(pos);
            for direction in UPDATE_ORDER {
                let target = step(pos, direction);
                let old = self.get(target);
                let new = {
                    let level = |p| self.get(p);
                    update_shape(old, target, direction.opposite(), source, &level)
                };
                if new != old {
                    self.set(target, new);
                    changed.push(target);
                }
            }
        }
    }
}

fn at(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, y, z)
}

const TOP: BlockPos = BlockPos::new(0, GROUND, 0);
const ABOVE: BlockPos = BlockPos::new(0, GROUND + 1, 0);
const LOOK_DOWN_SOUTH: (f32, f32) = (0.0, 60.0);

#[tuff_core::watchdog]
#[test]
fn plants_need_the_right_ground() {
    let mut world = World::new(Block::Stone);
    assert_eq!(
        world.use_item(Item::Poppy, TOP, Facing::Up, LOOK_DOWN_SOUTH, false),
        None
    );
    assert_eq!(
        world.use_item(Item::WheatSeeds, TOP, Facing::Up, LOOK_DOWN_SOUTH, false),
        None
    );

    let mut world = World::new(Block::GrassBlock);
    assert!(
        world
            .use_item(Item::Poppy, TOP, Facing::Up, LOOK_DOWN_SOUTH, false)
            .is_some()
    );
    assert_eq!(world.get(ABOVE).block(), Block::Poppy);
    assert_eq!(
        world.use_item(
            Item::WheatSeeds,
            at(1, GROUND, 0),
            Facing::Up,
            LOOK_DOWN_SOUTH,
            false
        ),
        None
    );

    let mut world = World::new(Block::Farmland);
    assert_eq!(
        world
            .use_item(Item::WheatSeeds, TOP, Facing::Up, LOOK_DOWN_SOUTH, false)
            .map(|s| s.block()),
        Some(Block::Wheat)
    );
}

#[tuff_core::watchdog]
#[test]
fn plants_break_when_their_ground_goes() {
    let mut world = World::new(Block::GrassBlock);
    world
        .use_item(Item::Poppy, TOP, Facing::Up, LOOK_DOWN_SOUTH, false)
        .unwrap();
    world
        .use_item(
            Item::Torch,
            at(1, GROUND, 0),
            Facing::Up,
            LOOK_DOWN_SOUTH,
            false,
        )
        .unwrap();
    world.replace(TOP, AIR);
    assert_eq!(world.get(ABOVE), AIR);
    world.replace(at(1, GROUND, 0), Block::Dirt.default_state());
    assert_eq!(world.get(at(1, GROUND + 1, 0)).block(), Block::Torch);
    world.replace(at(1, GROUND, 0), AIR);
    assert_eq!(world.get(at(1, GROUND + 1, 0)), AIR);
}

#[tuff_core::watchdog]
#[test]
fn leaf_litter_faces_the_player_and_stacks_unless_sneaking() {
    let mut world = World::new(Block::Stone);
    let placed = world
        .use_item(Item::LeafLitter, TOP, Facing::Up, LOOK_DOWN_SOUTH, false)
        .unwrap();
    assert_eq!(placed.facing(), Some(Facing::North));
    assert_eq!(placed.segment_amount(), Some(1));
    for amount in 2..=4 {
        let stacked = world
            .use_item(Item::LeafLitter, ABOVE, Facing::Up, LOOK_DOWN_SOUTH, false)
            .unwrap();
        assert_eq!(stacked.segment_amount(), Some(amount));
        assert_eq!(world.get(ABOVE).facing(), Some(Facing::North));
    }
    world.set(ABOVE, placed);
    world.use_item(Item::LeafLitter, ABOVE, Facing::Up, LOOK_DOWN_SOUTH, true);
    assert_eq!(world.get(ABOVE).segment_amount(), Some(1));

    let mut world = World::new(Block::Stone);
    world.set(
        TOP,
        "minecraft:oak_slab[type=bottom]"
            .parse::<BlockState>()
            .unwrap(),
    );
    assert_eq!(
        world.use_item(Item::LeafLitter, TOP, Facing::Up, LOOK_DOWN_SOUTH, false),
        None
    );
}

#[tuff_core::watchdog]
#[test]
fn torches_go_on_walls_or_floors_by_where_the_player_looks() {
    let mut world = World::new(Block::Stone);
    let wall = at(0, GROUND + 1, 1);
    world.set(wall, Block::Stone);
    let placed = world
        .use_item(Item::Torch, wall, Facing::North, (0.0, 0.0), false)
        .unwrap();
    assert_eq!(placed.block(), Block::WallTorch);
    assert_eq!(placed.facing(), Some(Facing::North));
    let floor = at(1, GROUND, 0);
    let placed = world
        .use_item(Item::Torch, floor, Facing::Up, LOOK_DOWN_SOUTH, false)
        .unwrap();
    assert_eq!(placed.block(), Block::Torch);

    world.replace(wall, AIR);
    assert_eq!(world.get(step(floor, Facing::Up)).block(), Block::Torch);
    assert_eq!(
        world.get(step(wall, Facing::North)),
        AIR,
        "the wall torch fell off"
    );
}

#[tuff_core::watchdog]
#[test]
fn logs_follow_the_clicked_face() {
    let mut world = World::new(Block::Stone);
    let placed = world
        .use_item(Item::OakLog, TOP, Facing::Up, LOOK_DOWN_SOUTH, false)
        .unwrap();
    assert_eq!(placed.axis(), Some(Axis::Y));
    let placed = world
        .use_item(Item::OakLog, ABOVE, Facing::East, (90.0, 0.0), false)
        .unwrap();
    assert_eq!(placed.axis(), Some(Axis::X));
}

#[tuff_core::watchdog]
#[test]
fn slabs_pick_a_half_and_combine() {
    let mut world = World::new(Block::Stone);
    let side = at(0, GROUND + 1, 1);
    world.set(side, Block::Stone);
    let clicked_high = |world: &mut World| {
        let level = |p| world.get(p);
        let context = context(
            side,
            Facing::North,
            [0.5, 0.75, 0.0],
            0.0,
            0.0,
            false,
            Item::OakSlab,
            ChunkShape::OVERWORLD,
            &level,
        );
        placement_state(&context, &level)
    };
    assert_eq!(
        clicked_high(&mut world).and_then(|s| s.r#type()),
        Some(Type::Top)
    );

    let bottom = world
        .use_item(Item::OakSlab, TOP, Facing::Up, LOOK_DOWN_SOUTH, false)
        .unwrap();
    assert_eq!(bottom.r#type(), Some(Type::Bottom));
    let double = world
        .use_item(Item::OakSlab, ABOVE, Facing::Up, LOOK_DOWN_SOUTH, false)
        .unwrap();
    assert_eq!(double.r#type(), Some(Type::Double));
    assert_eq!(world.get(ABOVE), double);
}

#[tuff_core::watchdog]
#[test]
fn a_waterlogged_slab_doubles_dry_and_never_holds_water_again() {
    let mut world = World::new(Block::Stone);
    let wet = Block::OakSlab
        .default_state()
        .with_waterlogged(true)
        .unwrap();
    world.set(ABOVE, wet);
    let double = world
        .use_item(Item::OakSlab, ABOVE, Facing::Up, LOOK_DOWN_SOUTH, false)
        .unwrap();
    assert_eq!(double.r#type(), Some(Type::Double));
    assert_eq!(double.waterlogged(), Some(false));
    assert!(!double.can_place_liquid(tuff_vanilla_data::fluid::Fluid::Water));
}

#[tuff_core::watchdog]
#[test]
fn stairs_face_away_and_turn_corners() {
    let mut world = World::new(Block::Stone);
    let first = world
        .use_item(Item::OakStairs, TOP, Facing::Up, (0.0, 30.0), false)
        .unwrap();
    assert_eq!(first.facing(), Some(Facing::South));
    assert_eq!(first.half(), Some(Half::Bottom));
    assert_eq!(first.shape(), Some(Shape::Straight));
    let corner_pos = at(0, GROUND + 1, 1);
    world
        .use_item(
            Item::OakStairs,
            at(0, GROUND, 1),
            Facing::Up,
            (90.0, 30.0),
            false,
        )
        .unwrap();
    world.replace(corner_pos, world.get(corner_pos));
    let corner = world.get(corner_pos);
    assert_eq!(corner.facing(), Some(Facing::West));
    assert_eq!(corner.shape(), Some(Shape::Straight));
    assert_ne!(world.get(ABOVE).shape(), Some(Shape::Straight));
}

#[tuff_core::watchdog]
#[test]
fn doors_place_both_halves_and_break_together() {
    let mut world = World::new(Block::Stone);
    let lower = world
        .use_item(Item::OakDoor, TOP, Facing::Up, LOOK_DOWN_SOUTH, false)
        .unwrap();
    assert_eq!(lower.half(), Some(Half::Lower));
    assert_eq!(lower.facing(), Some(Facing::South));
    let upper_pos = at(0, GROUND + 2, 0);
    assert_eq!(world.get(upper_pos).half(), Some(Half::Upper));
    world.replace(upper_pos, AIR);
    assert_eq!(world.get(ABOVE), AIR);

    world.set(upper_pos, Block::Stone);
    assert_eq!(
        world.use_item(Item::OakDoor, TOP, Facing::Up, LOOK_DOWN_SOUTH, false),
        None
    );
}

#[tuff_core::watchdog]
#[test]
fn fences_connect_to_solid_blocks_and_each_other() {
    let mut world = World::new(Block::Stone);
    world.set(at(1, GROUND + 1, 0), Block::Stone);
    let fence = world
        .use_item(Item::OakFence, TOP, Facing::Up, LOOK_DOWN_SOUTH, false)
        .unwrap();
    assert_eq!(fence.east(), Some(true));
    assert_eq!(fence.west(), Some(false));
    world
        .use_item(
            Item::OakFence,
            at(-1, GROUND, 0),
            Facing::Up,
            LOOK_DOWN_SOUTH,
            false,
        )
        .unwrap();
    world.replace(at(-1, GROUND + 1, 0), world.get(at(-1, GROUND + 1, 0)));
    assert_eq!(world.get(ABOVE).west(), Some(true));
    world.replace(at(1, GROUND + 1, 0), AIR);
    assert_eq!(world.get(ABOVE).east(), Some(false));
}

#[tuff_core::watchdog]
#[test]
fn beds_need_room_for_the_head() {
    let mut world = World::new(Block::Stone);
    let foot = world
        .use_item(Item::RedBed, TOP, Facing::Up, LOOK_DOWN_SOUTH, false)
        .unwrap();
    assert_eq!(foot.part(), Some(Part::Foot));
    assert_eq!(world.get(at(0, GROUND + 1, 1)).part(), Some(Part::Head));
    world.replace(at(0, GROUND + 1, 1), AIR);
    assert_eq!(world.get(ABOVE), AIR);
}

#[tuff_core::watchdog]
#[test]
fn collision_uses_the_placed_shape() {
    let player = [0.2, 65.0, 0.2, 0.8, 66.8, 0.8];
    assert!(obstructs(Block::Stone.default_state(), ABOVE, player));
    assert!(!obstructs(Block::Torch.default_state(), ABOVE, player));
    assert!(!obstructs(Block::Poppy.default_state(), ABOVE, player));
}

#[tuff_core::watchdog]
#[test]
fn chests_pair_into_double_chests_and_split_when_broken() {
    let mut world = World::new(Block::Stone);
    let first = world
        .use_item(Item::Chest, TOP, Facing::Up, LOOK_DOWN_SOUTH, false)
        .unwrap();
    assert_eq!(first.facing(), Some(Facing::North));
    assert_eq!(first.r#type(), Some(Type::Single));
    let east = at(1, GROUND + 1, 0);
    let second = world
        .use_item(
            Item::Chest,
            at(1, GROUND, 0),
            Facing::Up,
            LOOK_DOWN_SOUTH,
            false,
        )
        .unwrap();
    assert_eq!(second.r#type(), Some(Type::Right));
    world.replace(east, second);
    assert_eq!(world.get(ABOVE).r#type(), Some(Type::Left));
    assert_eq!(chest_partner(world.get(ABOVE), ABOVE), Some(east));
    assert_eq!(chest_partner(second, east), Some(ABOVE));
    let third = world
        .use_item(
            Item::Chest,
            at(2, GROUND, 0),
            Facing::Up,
            LOOK_DOWN_SOUTH,
            false,
        )
        .unwrap();
    assert_eq!(third.r#type(), Some(Type::Single));
    assert!(
        world
            .use_item(
                Item::TrappedChest,
                at(-1, GROUND, 0),
                Facing::Up,
                LOOK_DOWN_SOUTH,
                false
            )
            .unwrap()
            .r#type()
            == Some(Type::Single)
    );
    world.replace(east, AIR);
    assert_eq!(world.get(ABOVE).r#type(), Some(Type::Single));
    let sneaked = world
        .use_item(
            Item::Chest,
            at(0, GROUND + 1, 1),
            Facing::North,
            LOOK_DOWN_SOUTH,
            true,
        )
        .unwrap();
    assert_eq!(sneaked.r#type(), Some(Type::Single));
    let sneaked = world
        .use_item(Item::Chest, ABOVE, Facing::East, (0.0, 0.0), true)
        .unwrap();
    assert_eq!(sneaked.facing(), Some(Facing::North));
    assert_eq!(sneaked.r#type(), Some(Type::Right));
}

#[tuff_core::watchdog]
#[test]
fn covered_paths_and_farmland_turn_back_into_their_base() {
    let dirt = Block::Dirt.default_state();
    for (block, item) in [
        (Block::DirtPath, Item::DirtPath),
        (Block::Farmland, Item::Farmland),
    ] {
        let mut world = World::new(block);
        let state = world.get(TOP);
        let tick = |world: &World, direction| {
            let level = |p| world.get(p);
            shape_tick(state, TOP, direction, &level)
        };
        let reverted = |world: &World| {
            let level = |p| world.get(p);
            tick_state(state, TOP, &level)
        };
        assert_eq!(tick(&world, Facing::Up), None, "{block:?} under air");

        world.replace(ABOVE, Block::Stone.default_state());
        assert_eq!(world.get(TOP), state, "{block:?} waits for its tick");
        assert_eq!(tick(&world, Facing::Up), Some(1), "{block:?} under stone");
        assert_eq!(
            tick(&world, Facing::North),
            None,
            "only the block above counts"
        );
        assert_eq!(reverted(&world), Some(dirt), "{block:?} ticks into dirt");

        world.replace(ABOVE, Block::Torch.default_state());
        assert_eq!(tick(&world, Facing::Up), None, "{block:?} under a torch");

        let placed = |world: &mut World| {
            world
                .use_item(
                    item,
                    at(0, GROUND - 1, 0),
                    Facing::Up,
                    LOOK_DOWN_SOUTH,
                    false,
                )
                .map(BlockState::block)
        };
        world.set(TOP, AIR);
        world.set(ABOVE, Block::Stone.default_state());
        world.set(at(0, GROUND - 1, 0), Block::Stone.default_state());
        assert_eq!(
            placed(&mut world),
            Some(Block::Dirt),
            "{item:?} under stone"
        );
        world.set(TOP, AIR);
        world.set(ABOVE, AIR);
        assert_eq!(placed(&mut world), Some(block), "{item:?} in the open");
    }

    let mut world = World::new(Block::DirtPath);
    let path = world.get(TOP);
    assert!(Block::OakFenceGate.default_state().is_solid());
    world.replace(ABOVE, Block::OakFenceGate.default_state());
    let level = |p| world.get(p);
    assert_eq!(shape_tick(path, TOP, Facing::Up, &level), None);
    assert_eq!(
        tick_state(path, TOP, &level),
        Some(Block::Dirt.default_state())
    );

    let mut world = World::new(Block::Farmland);
    let farmland = world.get(TOP);
    world.replace(ABOVE, Block::Stone.default_state());
    world.replace(ABOVE, AIR);
    let level = |p| world.get(p);
    assert_eq!(
        tick_state(farmland, TOP, &level),
        None,
        "uncovered before its tick"
    );
}

#[tuff_core::watchdog]
#[test]
fn concrete_from_powder_derives_the_matching_color_by_name() {
    assert_eq!(
        concrete_from_powder(Block::WhiteConcretePowder),
        Some(Block::WhiteConcrete)
    );
    assert_eq!(
        concrete_from_powder(Block::BlackConcretePowder),
        Some(Block::BlackConcrete)
    );
    assert_eq!(concrete_from_powder(Block::Sand), None);
    assert_eq!(concrete_from_powder(Block::WhiteConcrete), None);
}

#[tuff_core::watchdog]
#[test]
fn concrete_powder_solidifies_when_placed_touching_water() {
    let mut world = World::new(Block::Stone);
    world.set(at(1, GROUND + 1, 0), Block::Water.default_state());
    let placed = world
        .use_item(
            Item::WhiteConcretePowder,
            TOP,
            Facing::Up,
            LOOK_DOWN_SOUTH,
            false,
        )
        .map(BlockState::block);
    assert_eq!(placed, Some(Block::WhiteConcrete));
}

#[tuff_core::watchdog]
#[test]
fn concrete_powder_placed_away_from_water_stays_powder() {
    let mut world = World::new(Block::Stone);
    let placed = world
        .use_item(
            Item::WhiteConcretePowder,
            TOP,
            Facing::Up,
            LOOK_DOWN_SOUTH,
            false,
        )
        .map(BlockState::block);
    assert_eq!(placed, Some(Block::WhiteConcretePowder));
}

#[tuff_core::watchdog]
#[test]
fn water_flowing_next_to_placed_concrete_powder_solidifies_it() {
    let mut world = World::new(Block::Stone);
    world
        .use_item(
            Item::WhiteConcretePowder,
            TOP,
            Facing::Up,
            LOOK_DOWN_SOUTH,
            false,
        )
        .unwrap();
    assert_eq!(world.get(ABOVE).block(), Block::WhiteConcretePowder);
    world.replace(at(1, GROUND + 1, 0), Block::Water.default_state());
    assert_eq!(world.get(ABOVE).block(), Block::WhiteConcrete);
}

#[tuff_core::watchdog]
#[test]
fn concrete_powder_ignores_water_below_unless_inside_it_and_sturdy_sides() {
    let pos = BlockPos::new(0, GROUND + 3, 0);
    let mut world = World::new(Block::Stone);
    world.set(step(pos, Facing::Down), Block::Water.default_state());
    assert!(!touches_water(pos, &|p| world.get(p)), "water below only");
    world.set(step(pos, Facing::Up), Block::Water.default_state());
    assert!(touches_water(pos, &|p| world.get(p)), "water above");
    world.set(step(pos, Facing::Up), AIR);
    world.set(pos, Block::Water.default_state());
    assert!(touches_water(pos, &|p| world.get(p)), "inside water");
    world.set(pos, AIR);
    world.set(step(pos, Facing::East), Block::Water.default_state());
    assert!(touches_water(pos, &|p| world.get(p)), "water beside");
}

#[tuff_core::watchdog]
#[test]
fn campfire_signal_fire_follows_the_block_below() {
    let pos = BlockPos::new(0, GROUND + 1, 0);
    let world = World::new(Block::Stone);
    let fire = Block::Campfire.default_state();
    let level = |p| world.get(p);
    let on_hay = update_shape(
        fire,
        pos,
        Facing::Down,
        Block::HayBlock.default_state(),
        &level,
    );
    assert_eq!(on_hay.get("signal_fire"), Some("true"));
    let on_stone = update_shape(
        on_hay,
        pos,
        Facing::Down,
        Block::Stone.default_state(),
        &level,
    );
    assert_eq!(on_stone.get("signal_fire"), Some("false"));
}

#[tuff_core::watchdog]
#[test]
fn cutting_a_plant_stalk_turns_the_body_into_a_head() {
    let pos = BlockPos::new(0, GROUND + 1, 0);
    let mut world = World::new(Block::Stone);
    world.set(pos, Block::Water.default_state());
    let body = Block::KelpPlant.default_state();
    let level = |p| world.get(p);
    let head = update_shape(body, pos, Facing::Up, Block::Water.default_state(), &level);
    assert_eq!(head.block(), Block::Kelp);
    let kept = update_shape(body, pos, Facing::Up, Block::Kelp.default_state(), &level);
    assert_eq!(kept.block(), Block::KelpPlant);
}

#[tuff_core::watchdog]
#[test]
fn seagrass_needs_a_full_water_block() {
    let pos = BlockPos::new(0, GROUND + 1, 0);
    let mut world = World::new(Block::Sand);
    world.set(pos, Block::Water.default_state());
    assert!(
        world
            .use_item(
                Item::Seagrass,
                step(pos, Facing::Down),
                Facing::Up,
                (0.0, 0.0),
                false
            )
            .is_some()
    );
}

#[tuff_core::watchdog]
#[test]
fn a_hanging_sign_chained_below_an_aligned_sign_hangs_from_its_chain() {
    let top = BlockPos::new(0, GROUND + 5, 0);
    let mut world = World::new(Block::Stone);
    world.set(step(top, Facing::Up), Block::Stone.default_state());
    let sign = Block::OakHangingSign
        .default_state()
        .with("rotation", "0")
        .unwrap();
    world.set(top, sign);
    let aligned = world
        .use_item(Item::OakHangingSign, top, Facing::Down, (0.0, 0.0), false)
        .expect("placed");
    assert_eq!(aligned.get("attached"), Some("false"));
    let mut world2 = World::new(Block::Stone);
    world2.set(step(top, Facing::Up), Block::Stone.default_state());
    world2.set(top, sign);
    let sideways = world2
        .use_item(Item::OakHangingSign, top, Facing::Down, (90.0, 0.0), false)
        .expect("placed");
    assert_eq!(sideways.get("attached"), Some("true"));
}

#[tuff_core::watchdog]
#[test]
fn wall_hanging_sign_needs_a_side_support() {
    let up = BlockPos::new(0, GROUND, 0);
    let mut world = World::new(Block::Stone);
    assert!(
        world
            .use_item(Item::OakHangingSign, up, Facing::Up, (0.0, 0.0), false)
            .is_none()
    );
    let pos = BlockPos::new(0, GROUND + 1, 0);
    for d in Facing::HORIZONTAL {
        world.set(step(pos, d), Block::Stone.default_state());
    }
    let placed = world.use_item(Item::OakHangingSign, up, Facing::Up, (0.0, 0.0), false);
    assert_eq!(
        placed.map(|s| s.block().kind()),
        Some(BlockKind::WallHangingSign)
    );
}

#[tuff_core::watchdog]
#[test]
fn kelp_cannot_stand_on_magma() {
    let pos = BlockPos::new(0, GROUND + 1, 0);
    let mut world = World::new(Block::Stone);
    world.set(pos, Block::Water.default_state());
    let kelp = Block::Kelp.default_state();
    assert!(can_survive(kelp, pos, &|p| world.get(p)));
    world.set(step(pos, Facing::Down), Block::MagmaBlock.default_state());
    assert!(!can_survive(kelp, pos, &|p| world.get(p)));
    assert!(!can_survive(Block::KelpPlant.default_state(), pos, &|p| {
        world.get(p)
    }));
}

#[tuff_core::watchdog]
#[test]
fn spore_blossom_pops_in_water() {
    let pos = BlockPos::new(0, GROUND + 3, 0);
    let mut world = World::new(Block::Stone);
    world.set(step(pos, Facing::Up), Block::Stone.default_state());
    let blossom = Block::SporeBlossom.default_state();
    assert!(can_survive(blossom, pos, &|p| world.get(p)));
    world.set(pos, Block::Water.default_state());
    assert!(!can_survive(blossom, pos, &|p| world.get(p)));
}

#[tuff_core::watchdog]
#[test]
fn joined_copper_chests_take_the_least_oxidized_block() {
    let mut world = World::new(Block::Stone);
    let pos = BlockPos::new(0, GROUND + 1, 0);
    let other = step(pos, Facing::East);
    let weathered = Block::WeatheredCopperChest
        .default_state()
        .with_facing(Facing::South)
        .unwrap();
    world.set(other, weathered);
    let placed = chest_state_for_facing(Block::OxidizedCopperChest, pos, Facing::South, &|p| {
        world.get(p)
    })
    .unwrap();
    assert_ne!(placed.r#type(), Some(Type::Single));
    assert_eq!(placed.block(), Block::WeatheredCopperChest);

    let waxed = chest_state_for_facing(Block::WaxedCopperChest, pos, Facing::South, &|p| {
        world.get(p)
    })
    .unwrap();
    assert_eq!(waxed.block(), Block::CopperChest);

    let shaped = update_shape(
        placed
            .block()
            .default_state()
            .with_facing(Facing::South)
            .unwrap()
            .with_type(placed.r#type().unwrap())
            .unwrap(),
        pos,
        Facing::East,
        Block::CopperChest.default_state(),
        &|p| world.get(p),
    );
    assert_eq!(shaped.block(), Block::CopperChest);
}

#[tuff_core::watchdog]
#[test]
fn bamboo_places_as_sapling_and_copies_age() {
    let mut world = World::new(Block::Stone);
    world.set(BlockPos::new(0, GROUND, 0), Block::Dirt.default_state());
    let ground = BlockPos::new(0, GROUND, 0);
    let placed = world.use_item(Item::Bamboo, ground, Facing::Up, (0.0, 0.0), false);
    assert_eq!(placed.map(|s| s.block()), Some(Block::BambooSapling));

    let stalk = BlockPos::new(0, GROUND + 1, 0);
    let sapling_pos = stalk;
    world.set(
        stalk,
        Block::Bamboo.default_state().with("age", "1").unwrap(),
    );
    let on_stalk = world.use_item(Item::Bamboo, stalk, Facing::Up, (0.0, 0.0), false);
    assert_eq!(
        on_stalk.and_then(|s| s.get("age").map(str::to_owned)),
        Some("1".into())
    );

    let sapling = Block::BambooSapling.default_state();
    let level = |p| world.get(p);
    let grown = update_shape(
        sapling,
        sapling_pos,
        Facing::Up,
        Block::Bamboo.default_state(),
        &level,
    );
    assert_eq!(grown.block(), Block::Bamboo);
    let low = Block::Bamboo.default_state();
    let cycled = update_shape(
        low,
        sapling_pos,
        Facing::Up,
        Block::Bamboo.default_state().with("age", "1").unwrap(),
        &level,
    );
    assert_eq!(cycled.get("age"), Some("1"));
}

#[tuff_core::watchdog]
#[test]
fn attached_stem_reverts_to_a_ripe_stem_when_its_fruit_is_removed() {
    let pos = BlockPos::new(0, GROUND + 1, 0);
    let mut world = World::new(Block::Stone);
    world.set(BlockPos::new(0, GROUND, 0), Block::Farmland.default_state());
    let level = |p| world.get(p);
    let attached = Block::AttachedPumpkinStem
        .default_state()
        .with("facing", "east")
        .unwrap();
    let kept = update_shape(
        attached,
        pos,
        Facing::East,
        Block::Pumpkin.default_state(),
        &level,
    );
    assert_eq!(kept, attached);
    let other_side = update_shape(attached, pos, Facing::West, AIR, &level);
    assert_eq!(other_side, attached);
    let reverted = update_shape(attached, pos, Facing::East, AIR, &level);
    assert_eq!(reverted.block(), Block::PumpkinStem);
    assert_eq!(reverted.get("age"), Some("7"));
}
