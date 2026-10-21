//! What right-clicking does to blocks, as pure functions of a [`Level`]
//! lookup: tools transforming blocks, honeycomb, lighting candles and
//! campfires, doors, trapdoors, fence gates, buttons, levers, note blocks
//! and bells. The region applies the results.

use tuff_vanilla_data::block::{Attachment, Facing, PickupKind};
use tuff_vanilla_data::block_transformer::Transform;
use tuff_vanilla_data::fluid::{Fluid, LiquidPlacement};
use tuff_vanilla_data::registry::SoundEvent;
use tuff_vanilla_data::tag::block as tags;
use tuff_vanilla_data::{Block, BlockKind, BlockState, Item};

use tuff_vanilla_common::placement::{self, Level};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

/// The highest note a note block plays; one more wraps to the lowest.
const MAX_NOTE: u8 = 24;
/// A bell rings only when struck below this height of its block.
const BELL_MAX_HIT_Y: f64 = 0.8123999834060669;

/// The first transform of the item's block transformer that applies to
/// the block at `pos` clicked on `face`, and the state it produces.
pub fn tool_transform(
    item: Item,
    face: Facing,
    pos: BlockPos,
    level: Level,
) -> Option<(&'static Transform, BlockState)> {
    let block_at = |[x, y, z]: [i32; 3]| level(BlockPos::new(pos.x + x, pos.y + y, pos.z + z));
    item.block_transformer()?
        .transforms
        .iter()
        .filter(|transform| !transform.disallowed_faces.contains(&face))
        .find_map(|transform| {
            let state = transform.apply(&block_at)?;
            let state = if transform.update_from_neighbors {
                placement::update_from_neighbours(state, pos, level)
            } else {
                state
            };
            Some((transform, state))
        })
}

/// `state` with its properties copied onto `block` wherever it has them.
pub fn with_properties_of(block: Block, state: BlockState) -> BlockState {
    state
        .properties()
        .fold(block.default_state(), |new, (name, value)| {
            new.with(name, value).unwrap_or(new)
        })
}

/// What honeycomb turns `state` into.
pub fn waxed(state: BlockState) -> Option<BlockState> {
    state
        .block()
        .waxed()
        .map(|block| with_properties_of(block, state))
}

/// Whether flint and steel or a fire charge lights `state`: an unlit,
/// dry campfire or candle, or an unlit candle cake.
pub fn can_light(state: BlockState) -> bool {
    let block = state.block();
    let unlit = state.lit() == Some(false);
    let dry = state.waterlogged() == Some(false);
    ((tuff_datapack::tags::is(tags::key::CAMPFIRES, block)
        || tuff_datapack::tags::is(tags::key::CANDLES, block))
        && unlit
        && dry)
        || (tuff_datapack::tags::is(tags::key::CANDLE_CAKES, block) && unlit)
}

/// A door, trapdoor or fence gate after a hand opens or closes it, with the
/// sound it makes. `looking` is the direction the player faces.
pub fn toggle(state: BlockState, looking: Facing) -> Option<(BlockState, SoundEvent)> {
    let toggle = state.block().toggle()?;
    if !toggle.opens_by_hand {
        return None;
    }
    let open = state.open()?;
    let mut next = state.with_open(!open)?;
    if state.block().kind() == BlockKind::FenceGate
        && !open
        && state.facing() == Some(looking.opposite())
    {
        next = next.with_facing(looking)?;
    }
    let sound = if open {
        toggle.close_sound
    } else {
        toggle.open_sound
    };
    Some((next, sound))
}

/// The note block one right click further along its notes.
pub fn next_note(state: BlockState) -> Option<BlockState> {
    let note = state.note()?;
    state.with("note", &((note + 1) % (MAX_NOTE + 1)).to_string())
}

/// Whether a note block sounds: head instruments always, others only with
/// air above.
pub fn plays_note(state: BlockState, above: BlockState) -> bool {
    state
        .instrument()
        .is_some_and(|instrument| instrument.works_above_note_block())
        || is_air(above)
}

/// The pitch a note block's note plays at: two octaves around 1.0.
pub fn note_pitch(note: u8) -> f32 {
    2f64.powf((note as f64 - 12.0) / 12.0) as f32
}

/// Whether clicking `face` at height `click_y` within the block strikes
/// a bell's body rather than its frame.
pub fn bell_struck(state: BlockState, face: Facing, click_y: f64) -> bool {
    if !face.is_horizontal() || click_y > BELL_MAX_HIT_Y {
        return false;
    }
    let (Some(facing), Some(attachment)) = (state.facing(), state.attachment()) else {
        return false;
    };
    match attachment {
        Attachment::Floor => facing.axis() == face.axis(),
        Attachment::SingleWall | Attachment::DoubleWall => facing.axis() != face.axis(),
        Attachment::Ceiling => true,
    }
}

/// A lever after a pull, and the pitch of its click.
pub fn pull_lever(state: BlockState) -> Option<(BlockState, f32)> {
    let powered = !state.powered()?;
    Some((
        state.with_powered(powered)?,
        if powered { 0.6 } else { 0.5 },
    ))
}

/// What a bucket a dispenser holds takes from `state`: the state left
/// behind, the filled bucket, and the sound. A waterlogged block that
/// cannot stand dry must be broken afterwards.
pub fn bucket_pickup(state: BlockState) -> Option<(BlockState, Item, Option<SoundEvent>)> {
    player_bucket_pickup(state, false)
}

/// [`bucket_pickup`] for a player's bucket: only a creative player drains
/// a barrier.
pub fn player_bucket_pickup(
    state: BlockState,
    creative: bool,
) -> Option<(BlockState, Item, Option<SoundEvent>)> {
    if state.block() == Block::Barrier && !creative {
        return None;
    }
    let pickup = state.block().bucket_pickup()?;
    let left = match pickup.kind {
        PickupKind::Liquid if state.level() == Some(0) => Block::Air.default_state(),
        PickupKind::Liquid => return None,
        PickupKind::Waterlogged if state.waterlogged() == Some(true) => {
            state.with_waterlogged(false)?
        }
        PickupKind::Waterlogged => return None,
        PickupKind::Whole => Block::Air.default_state(),
    };
    Some((left, pickup.item, pickup.sound))
}

/// What pouring a bucket does at the position it lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pour {
    /// The water boils away (the nether).
    Evaporate,
    /// A block that holds liquids takes the water (see
    /// [`BlockState::place_liquid`]); if it refuses, nothing changes but the
    /// bucket still empties.
    Contain,
    /// The fluid's source block goes in, breaking what was there when
    /// `break_old`.
    Replace { break_old: bool },
}

/// Where and how a bucket of `fluid` pours when a player aims at `hit`.
/// `creative` players also waterlog barriers.
pub fn pour_target(
    hit: &tuff_core::world::raycast::Hit,
    fluid: Block,
    sneaking: bool,
    creative: bool,
    evaporates: bool,
    level: Level,
) -> Option<(BlockPos, Pour)> {
    let water = fluid == Block::Water;
    let clicked = level(hit.pos);
    let first = if clicked.is_liquid_container() && water {
        hit.pos
    } else {
        placement::step(hit.pos, hit.face)
    };
    let pourer = Pourer {
        sneaking,
        creative,
        evaporates,
    };
    pour_at(first, fluid, pourer, Some(hit), level)
}

#[derive(Clone, Copy)]
struct Pourer {
    sneaking: bool,
    creative: bool,
    evaporates: bool,
}

/// The waterlogging a creative player's water bucket gives a dry barrier,
/// which refuses every other source of water.
pub fn barrier_placement(state: BlockState) -> Option<&'static LiquidPlacement> {
    static PLACEMENT: std::sync::OnceLock<LiquidPlacement> = std::sync::OnceLock::new();
    (state.block() == Block::Barrier && state.waterlogged() == Some(false)).then(|| {
        PLACEMENT.get_or_init(|| LiquidPlacement {
            result: Block::Barrier
                .default_state()
                .with_waterlogged(true)
                .expect("barriers waterlog"),
            tick: Some(Fluid::Water),
            sounds: &[],
        })
    })
}

/// Where and how a bucket of `fluid` a dispenser empties at `pos` pours.
pub fn dispense_pour(
    pos: BlockPos,
    fluid: Block,
    evaporates: bool,
    level: Level,
) -> Option<(BlockPos, Pour)> {
    let pourer = Pourer {
        sneaking: false,
        creative: false,
        evaporates,
    };
    pour_at(pos, fluid, pourer, None, level)
}

fn pour_at(
    pos: BlockPos,
    fluid: Block,
    pourer: Pourer,
    hit: Option<&tuff_core::world::raycast::Hit>,
    level: Level,
) -> Option<(BlockPos, Pour)> {
    let water = fluid == Block::Water;
    let state = level(pos);
    let may_replace = state.is_replaceable_by_fluid(!water);
    let kind = if water { Fluid::Water } else { Fluid::Lava };
    let contains = state.is_liquid_container()
        && (state.can_place_liquid(kind)
            || (pourer.creative && water && state.block() == Block::Barrier));
    let place_liquid = may_replace || contains;
    let inside = is_air(state) || (place_liquid && (!pourer.sneaking || hit.is_none()));
    if !inside {
        let hit = hit?;
        return pour_at(
            placement::step(hit.pos, hit.face),
            fluid,
            pourer,
            None,
            level,
        );
    }
    if pourer.evaporates && water {
        return Some((pos, Pour::Evaporate));
    }
    if state.is_liquid_container() && water {
        return Some((pos, Pour::Contain));
    }
    Some((
        pos,
        Pour::Replace {
            break_old: may_replace && !state.is_liquid() && !is_air(state),
        },
    ))
}

pub use tuff_vanilla_common::dismount::*;

/// How high above the bed's head block a sleeper lies: the top of the
/// block's shape (a straw bed's foot for a straw bed).
pub fn sleep_height(head: BlockPos, state: BlockState, level: Level) -> Option<f64> {
    let bed = state.block().bed()?;
    let lying_on = if bed.explodes {
        state
    } else {
        let foot = placement::step(head, state.facing()?.opposite());
        let foot_state = level(foot);
        if foot_state.block() != state.block() {
            return None;
        }
        foot_state
    };
    top(lying_on.outline_boxes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_vanilla_data::block::Instrument;

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[tuff_core::watchdog]
    #[test]
    fn axes_strip_logs_only_from_allowed_faces_and_hoes_need_air() {
        let log = Block::OakLog.default_state().with("axis", "z").unwrap();
        let level = |p: BlockPos| {
            if p == at(0, 0, 0) {
                log
            } else {
                Block::Air.default_state()
            }
        };
        let (transform, stripped) =
            tool_transform(Item::DiamondAxe, Facing::Down, at(0, 0, 0), &level).unwrap();
        assert_eq!(stripped.block(), Block::StrippedOakLog);
        assert_eq!(stripped.get("axis"), Some("z"));
        assert_eq!(transform.sound, Some(SoundEvent::ItemAxeStrip));
        assert_eq!(transform.item_damage_per_use, 1);

        let dirt = Block::Dirt.default_state();
        let level = |p: BlockPos| {
            if p.y <= 0 {
                dirt
            } else {
                Block::Air.default_state()
            }
        };
        assert_eq!(
            tool_transform(Item::IronHoe, Facing::Up, at(0, 0, 0), &level).map(|(_, s)| s.block()),
            Some(Block::Farmland)
        );
        assert!(tool_transform(Item::IronHoe, Facing::Down, at(0, 0, 0), &level).is_none());
        assert!(tool_transform(Item::IronHoe, Facing::Up, at(0, -1, 0), &level).is_none());
        let rooted = Block::RootedDirt.default_state();
        let level = |_: BlockPos| rooted;
        let (transform, tilled) =
            tool_transform(Item::IronHoe, Facing::Down, at(0, 0, 0), &level).unwrap();
        assert_eq!(tilled, dirt);
        assert_eq!(transform.loot, Some("minecraft:till/rooted_dirt"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn honeycomb_and_axes_move_along_the_wax_chain() {
        let door = Block::ExposedCopperDoor
            .default_state()
            .with("hinge", "right")
            .unwrap();
        let waxed = waxed(door).unwrap();
        assert_eq!(waxed.block(), Block::WaxedExposedCopperDoor);
        assert_eq!(waxed.get("hinge"), Some("right"));
        assert!(super::waxed(waxed).is_none());
        let level = |_: BlockPos| waxed;
        let (transform, unwaxed) =
            tool_transform(Item::StoneAxe, Facing::North, at(0, 0, 0), &level).unwrap();
        assert_eq!(unwaxed, door);
        assert!(!transform.update_from_neighbors);
    }

    #[tuff_core::watchdog]
    #[test]
    fn candles_and_campfires_light_when_dry_and_unlit() {
        let campfire = Block::Campfire.default_state().with_lit(false).unwrap();
        assert!(can_light(campfire));
        assert!(!can_light(campfire.with_waterlogged(true).unwrap()));
        assert!(!can_light(Block::Campfire.default_state()));
        assert!(can_light(Block::RedCandle.default_state()));
        assert!(can_light(Block::CandleCake.default_state()));
        assert!(!can_light(Block::Stone.default_state()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn doors_gates_and_trapdoors_toggle_by_hand_with_their_sounds() {
        let door = Block::OakDoor.default_state();
        let (open, sound) = toggle(door, Facing::North).unwrap();
        assert_eq!(open.open(), Some(true));
        assert_eq!(sound, SoundEvent::BlockWoodenDoorOpen);
        assert!(toggle(Block::IronDoor.default_state(), Facing::North).is_none());
        let (closed, sound) = toggle(open, Facing::North).unwrap();
        assert_eq!(closed, door);
        assert_eq!(sound, SoundEvent::BlockWoodenDoorClose);
        let gate = Block::OakFenceGate
            .default_state()
            .with_facing(Facing::North)
            .unwrap();
        let (opened, _) = toggle(gate, Facing::South).unwrap();
        assert_eq!(opened.facing(), Some(Facing::South), "gates open away");
        let (opened, _) = toggle(gate, Facing::East).unwrap();
        assert_eq!(opened.facing(), Some(Facing::North));
        let (trapdoor, sound) =
            toggle(Block::SpruceTrapdoor.default_state(), Facing::East).unwrap();
        assert_eq!(trapdoor.open(), Some(true));
        assert_eq!(sound, SoundEvent::BlockWoodenTrapdoorOpen);
    }

    #[tuff_core::watchdog]
    #[test]
    fn note_blocks_take_instruments_from_neighbours_and_cycle_notes() {
        let below = |block: Block, above: Block| {
            move |p: BlockPos| match p.y {
                -1 => block.default_state(),
                1 => above.default_state(),
                _ => Block::NoteBlock.default_state(),
            }
        };
        assert_eq!(
            placement::note_instrument(at(0, 0, 0), &below(Block::Stone, Block::Air)),
            Instrument::Basedrum
        );
        assert_eq!(
            placement::note_instrument(at(0, 0, 0), &below(Block::GoldBlock, Block::Air)),
            Instrument::Bell
        );
        assert_eq!(
            placement::note_instrument(at(0, 0, 0), &below(Block::Stone, Block::ZombieHead)),
            Instrument::Zombie
        );
        assert_eq!(
            placement::note_instrument(at(0, 0, 0), &below(Block::CreeperHead, Block::Air)),
            Instrument::Harp
        );
        let note = Block::NoteBlock.default_state().with("note", "24").unwrap();
        assert_eq!(next_note(note).unwrap().note(), Some(0));
        assert_eq!(note_pitch(12), 1.0);
        assert!((note_pitch(24) - 2.0).abs() < 1e-6);
        assert!(!plays_note(note, Block::Stone.default_state()));
        assert!(plays_note(note, Block::Air.default_state()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn buckets_take_sources_and_drain_waterlogged_blocks() {
        let water = Block::Water.default_state();
        let (left, taken, sound) = bucket_pickup(water).unwrap();
        assert_eq!(
            (left, taken),
            (Block::Air.default_state(), Item::WaterBucket)
        );
        assert_eq!(sound, Some(SoundEvent::ItemBucketFill));
        assert!(bucket_pickup(water.with("level", "3").unwrap()).is_none());
        assert_eq!(
            bucket_pickup(Block::Lava.default_state()).unwrap().1,
            Item::LavaBucket
        );
        let stairs = Block::OakStairs
            .default_state()
            .with_waterlogged(true)
            .unwrap();
        let (left, taken, _) = bucket_pickup(stairs).unwrap();
        assert_eq!(left.waterlogged(), Some(false));
        assert_eq!(taken, Item::WaterBucket);
        assert!(bucket_pickup(Block::OakStairs.default_state()).is_none());
        assert_eq!(
            bucket_pickup(Block::PowderSnow.default_state()).unwrap().1,
            Item::PowderSnowBucket
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn buckets_pour_next_to_the_hit_or_into_waterloggable_blocks() {
        use tuff_core::world::raycast::Hit;
        let slab = Block::OakSlab.default_state();
        let level = |p: BlockPos| match (p.x, p.y) {
            (0, 0) => slab,
            (1, 0) => Block::ShortGrass.default_state(),
            (_, y) if y < 0 => Block::Stone.default_state(),
            _ => Block::Air.default_state(),
        };
        let hit = |pos, face| Hit {
            pos,
            face,
            location: [0.5, 0.5, 0.5],
        };
        let into_slab = pour_target(
            &hit(at(0, 0, 0), Facing::Up),
            Block::Water,
            false,
            false,
            false,
            &level,
        );
        assert_eq!(into_slab, Some((at(0, 0, 0), Pour::Contain)));
        let lava = pour_target(
            &hit(at(0, 0, 0), Facing::Up),
            Block::Lava,
            false,
            false,
            false,
            &level,
        );
        assert_eq!(
            lava,
            Some((at(0, 1, 0), Pour::Replace { break_old: false }))
        );
        let grass = pour_target(
            &hit(at(1, -1, 0), Facing::Up),
            Block::Water,
            false,
            false,
            false,
            &level,
        );
        assert_eq!(
            grass,
            Some((at(1, 0, 0), Pour::Replace { break_old: true }))
        );
        let nether = pour_target(
            &hit(at(1, -1, 0), Facing::Up),
            Block::Water,
            false,
            false,
            true,
            &level,
        );
        assert_eq!(nether, Some((at(1, 0, 0), Pour::Evaporate)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_creative_players_waterlog_and_drain_barriers() {
        use tuff_core::world::raycast::Hit;
        let barrier = Block::Barrier.default_state();
        let level = |p: BlockPos| match (p.x, p.y) {
            (0, 0) => barrier,
            _ => Block::Air.default_state(),
        };
        let hit = Hit {
            pos: at(0, 0, 0),
            face: Facing::Up,
            location: [0.5, 1.0, 0.5],
        };
        let survival = pour_target(&hit, Block::Water, false, false, false, &level);
        assert_eq!(
            survival,
            Some((at(0, 1, 0), Pour::Replace { break_old: false }))
        );
        let creative = pour_target(&hit, Block::Water, false, true, false, &level);
        assert_eq!(creative, Some((at(0, 0, 0), Pour::Contain)));
        let lava = pour_target(&hit, Block::Lava, false, true, false, &level);
        assert_eq!(
            lava,
            Some((at(0, 1, 0), Pour::Replace { break_old: false }))
        );
        assert!(dispense_pour(at(0, 0, 0), Block::Water, false, &level).is_none());

        let placed = barrier_placement(barrier).unwrap();
        assert_eq!(placed.result.waterlogged(), Some(true));
        assert!(barrier_placement(placed.result).is_none());
        assert!(barrier.place_liquid(Fluid::Water).is_none());

        let wet = placed.result;
        assert!(
            bucket_pickup(wet).is_none(),
            "dispensers never drain a barrier"
        );
        assert!(player_bucket_pickup(wet, false).is_none());
        let (left, taken, _) = player_bucket_pickup(wet, true).unwrap();
        assert_eq!((left, taken), (barrier, Item::WaterBucket));
    }

    #[tuff_core::watchdog]
    #[test]
    fn players_get_out_of_bed_beside_it() {
        let head = at(0, 64, 0);
        let bed_head = Block::RedBed
            .default_state()
            .with_facing(Facing::South)
            .unwrap()
            .with_part(tuff_vanilla_data::block::Part::Head)
            .unwrap();
        let level = move |p: BlockPos| {
            if p == head {
                bed_head
            } else if p.y < 64 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            }
        };
        let standing = stand_up_position(head, Facing::South, 0.0, &level).unwrap();
        assert_eq!(standing[1], 64.0);
        assert!((standing[0] - 0.5).abs() == 1.0 || (standing[2] - 0.5).abs() >= 1.0);
        assert!((sleep_height(head, bed_head, &level).unwrap() - 0.5625).abs() < 1e-6);
        let walled = move |p: BlockPos| {
            if p == head {
                bed_head
            } else if p.y < 64 || p.y <= 66 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            }
        };
        assert_eq!(stand_up_position(head, Facing::South, 0.0, &walled), None);
        assert!((yaw_toward([0.5, 64.0, -1.5], head) - 0.0).abs() < 1e-4);
        assert!(safe_dismount(at(3, 64, 3), true, &level).is_some());
        let lava = |p: BlockPos| {
            if p.y < 64 {
                Block::Lava.default_state()
            } else {
                Block::Air.default_state()
            }
        };
        assert!(safe_dismount(at(3, 64, 3), true, &lava).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn bells_ring_only_when_struck_on_their_body() {
        let floor = Block::Bell
            .default_state()
            .with("attachment", "floor")
            .unwrap()
            .with_facing(Facing::North)
            .unwrap();
        assert!(bell_struck(floor, Facing::South, 0.5));
        assert!(!bell_struck(floor, Facing::East, 0.5));
        assert!(!bell_struck(floor, Facing::South, 0.9));
        assert!(!bell_struck(floor, Facing::Up, 0.5));
        let (on, pitch) = pull_lever(Block::Lever.default_state()).unwrap();
        assert_eq!((on.powered(), pitch), (Some(true), 0.6));
    }
}
