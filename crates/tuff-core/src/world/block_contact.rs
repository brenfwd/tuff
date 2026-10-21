//! What blocks do to a living thing standing on or inside them: magma
//! burns the feet of anything not stepping carefully, lit campfires burn
//! whatever is inside them, grown sweet berry bushes prick whatever moves
//! through them, cactus pricks whatever touches it from any side, and a
//! suffocating block occupying the eyes chokes it.
//! Pure functions of a block lookup, shared by the player's home region
//! (which reports them to the session) and mobs.

use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, BlockKind, BlockState};

use crate::world::physics::{self, Aabb};
use tuff_world_model::BlockPos;

pub const HOT_FLOOR_DAMAGE: f32 = 1.0;
pub const BERRY_BUSH_DAMAGE: f32 = 1.0;
pub const CACTUS_DAMAGE: f32 = 1.0;
pub const SUFFOCATION_DAMAGE: f32 = 1.0;
/// Ticks of Wither a wither rose gives.
pub const WITHER_ROSE_TICKS: i64 = 40;
/// Ticks of Poison an eyeblossom gives a bee.
pub const BEE_POISON_TICKS: i64 = 25;
/// Half the height of the thin slab checked at eye height for suffocation.
const SUFFOCATION_HALF_HEIGHT: f64 = 5.0e-7;
/// The fraction of an entity's width the suffocation check box uses.
const SUFFOCATION_WIDTH_FRACTION: f64 = 0.8;
/// How far below the feet the block that is stepped on lies.
pub const STEP_OFFSET: f32 = 0.2;
/// How far a thing has to move along x or z in a tick for a bush to prick.
const BERRY_BUSH_MOVEMENT: f64 = 0.003_000_000_026_077_032;
/// A box counts as inside the blocks it overlaps once shrunk by this much.
const INSIDE_INSET: f64 = 9.999_999_747_378_752e-6;
/// How far a cactus's collision box sits inside its cell on the sides and top.
const CACTUS_INSET: f64 = 1.0 / 16.0;
const SUPPORT_EPSILON: f32 = 1.0e-5;

/// The block under the feet at `offset` below them: the supporting block
/// when there is one (at the feet's height unless it is a fence, wall or
/// gate tall enough to count), else the block straight below.
pub fn on_pos(
    position: [f64; 3],
    supporting: Option<BlockPos>,
    offset: f32,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> BlockPos {
    let y = (position[1] - offset as f64).floor() as i32;
    match supporting {
        Some(support) if offset > SUPPORT_EPSILON => {
            let keeps = block(support).is_some_and(|s| {
                let block = s.block();
                (offset <= 0.5
                    && tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::FENCES, block))
                    || tuff_datapack::tags::is(tuff_vanilla_data::tag::block::key::WALLS, block)
                    || block.kind() == BlockKind::FenceGate
            });
            if keeps {
                support
            } else {
                BlockPos::new(support.x, y, support.z)
            }
        }
        Some(support) => support,
        None => BlockPos::new(position[0].floor() as i32, y, position[2].floor() as i32),
    }
}

/// What the blocks around a living thing's box do to it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Contact {
    /// The block under its feet burns them (magma), when it stands on it
    /// without stepping carefully.
    pub hot_floor: bool,
    /// The damage of the strongest lit campfire it is inside.
    pub campfire: Option<f32>,
    /// It is inside a sweet berry bush grown enough to prick it.
    pub berry_bush: bool,
    /// It is touching a cactus block on any side.
    pub cactus: bool,
    /// It is inside a wither rose.
    pub wither_rose: bool,
    /// It is inside an eyeblossom that attracts bees, which poisons a bee.
    pub bee_poison: bool,
    /// The block under its feet is soul sand or soul soil (Soul Speed).
    pub soul_speed_block: bool,
    /// A suffocating block's collision shape fills the space at its eyes.
    pub suffocating: bool,
    /// Powder snow or something thawing (fire, a lava cauldron) inside it.
    pub freezing: crate::world::freezing::Inside,
    /// Cauldrons whose contents it is inside.
    pub cauldron: Cauldron,
    /// Rain falls on it, which puts out fire.
    pub in_rain: bool,
    /// How tight the space around a player is: 0 fits a standing player,
    /// 1 only a crouching one, 2 neither (it must crawl).
    pub cramped: u8,
    /// The block that holds it back on its next move, if any.
    pub stuck: Option<Stuck>,
    /// A honey block whose side it is pressed against below the top, where
    /// it slides down while falling.
    pub honey_side: Option<BlockState>,
}

/// What the cauldrons a box is inside of do to it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cauldron {
    /// It is inside a lava cauldron: it burns and takes lava damage.
    pub lava: bool,
    /// A water or powder snow cauldron that puts out its fire and loses a
    /// level when it burns.
    pub extinguishes: Option<BlockPos>,
}

impl Cauldron {
    pub fn merge(self, other: Self) -> Self {
        Self {
            lava: self.lava || other.lava,
            extinguishes: self.extinguishes.or(other.extinguishes),
        }
    }

    /// The cauldron at `pos` when `bounds` overlap what fills it: a lava
    /// cauldron entirely, a water or powder snow one up to its contents.
    pub fn at(state: BlockState, pos: BlockPos, bounds: Aabb) -> Self {
        let height = match state.block() {
            Block::LavaCauldron => 1.0,
            Block::WaterCauldron | Block::PowderSnowCauldron => {
                (6.0 + f64::from(state.level().unwrap_or(1)) * 3.0) / 16.0
            }
            _ => return Self::default(),
        };
        let [x, y, z] = [pos.x, pos.y, pos.z].map(f64::from);
        let wall = 2.0 / 16.0;
        let parts = [
            [x, y, z, x + 1.0, y + height, z + 1.0],
            [x, y, z, x + wall, y + 1.0, z + 1.0],
            [x + 1.0 - wall, y, z, x + 1.0, y + 1.0, z + 1.0],
            [x, y, z, x + 1.0, y + 1.0, z + wall],
            [x, y, z + 1.0 - wall, x + 1.0, y + 1.0, z + 1.0],
        ];
        if !parts.iter().any(|part| physics::intersects(*part, bounds)) {
            return Self::default();
        }
        Self {
            lava: state.block() == Block::LavaCauldron,
            extinguishes: (state.block() != Block::LavaCauldron).then_some(pos),
        }
    }
}

/// The cauldrons `bounds` are inside.
pub fn cauldron_inside(bounds: Aabb, block: &dyn Fn(BlockPos) -> Option<BlockState>) -> Cauldron {
    let inner = [
        bounds[0] + INSIDE_INSET,
        bounds[1] + INSIDE_INSET,
        bounds[2] + INSIDE_INSET,
        bounds[3] - INSIDE_INSET,
        bounds[4] - INSIDE_INSET,
        bounds[5] - INSIDE_INSET,
    ];
    let mut found = Cauldron::default();
    for x in inner[0].floor() as i32..=inner[3].floor() as i32 {
        for y in inner[1].floor() as i32..=inner[4].floor() as i32 {
            for z in inner[2].floor() as i32..=inner[5].floor() as i32 {
                let pos = BlockPos::new(x, y, z);
                if let Some(state) = block(pos) {
                    found = found.merge(Cauldron::at(state, pos, inner));
                }
            }
        }
    }
    found
}

/// A block that slows whatever is inside it on its next move: the move is
/// scaled per axis and the velocity it had is lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stuck {
    Cobweb,
    BerryBush,
    PowderSnow,
}

impl Stuck {
    /// The per-axis scale of the next move; `weaving` for a thing under
    /// the Weaving effect, which cobwebs hold back less.
    pub fn multiplier(self, weaving: bool) -> [f64; 3] {
        match self {
            Stuck::Cobweb if weaving => [0.5, 0.25, 0.5],
            Stuck::Cobweb => [0.25, 0.050_000_000_745_058_06, 0.25],
            Stuck::BerryBush => [0.800_000_011_920_929, 0.75, 0.800_000_011_920_929],
            Stuck::PowderSnow => [0.899_999_976_158_142_1, 1.5, 0.899_999_976_158_142_1],
        }
    }
}

impl Contact {
    /// Nothing here hurts.
    pub fn is_none(&self) -> bool {
        Self {
            in_rain: false,
            cramped: 0,
            stuck: None,
            honey_side: None,
            ..*self
        } == Self::default()
    }

    /// Whether a bush pricks a thing that moved by `movement` this tick.
    pub fn pricks(&self, movement: [f64; 3]) -> bool {
        self.berry_bush
            && (movement[0].abs() >= BERRY_BUSH_MOVEMENT
                || movement[2].abs() >= BERRY_BUSH_MOVEMENT)
    }
}

/// The contact of a living `kind` whose box is `bounds`, standing on
/// `ground` (the block at [`STEP_OFFSET`] under its feet).
pub fn contact(
    kind: EntityType,
    bounds: Aabb,
    ground: Option<BlockState>,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> Contact {
    swept_contact(kind, bounds, bounds, ground, block)
}

/// The union of two boxes, which is what a thing swept through when it
/// moved from one to the other.
fn swept_union(a: Aabb, b: Aabb) -> Aabb {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].min(b[2]),
        a[3].max(b[3]),
        a[4].max(b[4]),
        a[5].max(b[5]),
    ]
}

/// Like [`contact`] for a thing that moved from `previous` to `bounds`
/// this tick: the blocks it passed through count, not only the ones it
/// ended in (a cobweb or berry bush it crossed still holds it, a cactus
/// pricks it). What depends on where it ended up still uses `bounds`.
pub fn swept_contact(
    kind: EntityType,
    previous: Aabb,
    bounds: Aabb,
    ground: Option<BlockState>,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> Contact {
    let mut contact = Contact {
        hot_floor: ground.is_some_and(|s| s.block() == Block::MagmaBlock),
        soul_speed_block: ground.is_some_and(|s| {
            tuff_datapack::tags::is(
                tuff_vanilla_data::tag::block::key::SOUL_SPEED_BLOCKS,
                s.block(),
            )
        }),
        ..Contact::default()
    };
    let swept = swept_union(previous, bounds);
    let inner = [
        swept[0] + INSIDE_INSET,
        swept[1] + INSIDE_INSET,
        swept[2] + INSIDE_INSET,
        swept[3] - INSIDE_INSET,
        swept[4] - INSIDE_INSET,
        swept[5] - INSIDE_INSET,
    ];
    let pricked = !matches!(kind, EntityType::Fox | EntityType::Bee);
    let webbed = !matches!(kind, EntityType::Spider | EntityType::CaveSpider);
    let feet = physics::block_pos([
        (bounds[0] + bounds[3]) / 2.0,
        bounds[1],
        (bounds[2] + bounds[5]) / 2.0,
    ]);
    let heading = [(0, 3), (2, 5)]
        .map(|(lo, hi)| (bounds[lo] + bounds[hi]) / 2.0 - (previous[lo] + previous[hi]) / 2.0);
    let mut latest = f64::NEG_INFINITY;
    let in_powder_snow = block(feet).is_some_and(|s| s.block() == Block::PowderSnow);
    for x in inner[0].floor() as i32..=inner[3].floor() as i32 {
        for y in inner[1].floor() as i32..=inner[4].floor() as i32 {
            for z in inner[2].floor() as i32..=inner[5].floor() as i32 {
                let Some(state) = block(BlockPos::new(x, y, z)) else {
                    continue;
                };
                if let Some(damage) = state.block().campfire_damage()
                    && state.get("lit") == Some("true")
                {
                    contact.campfire = Some(contact.campfire.map_or(damage, |d| d.max(damage)));
                }
                let along = (f64::from(x) + 0.5) * heading[0] + (f64::from(z) + 0.5) * heading[1];
                let mut hold = |stuck| {
                    if along >= latest {
                        latest = along;
                        contact.stuck = Some(stuck);
                    }
                };
                match state.block() {
                    Block::Cobweb if webbed => hold(Stuck::Cobweb),
                    Block::SweetBerryBush if pricked => {
                        hold(Stuck::BerryBush);
                        if state.get("age").is_some_and(|age| age != "0") {
                            contact.berry_bush = true;
                        }
                    }
                    Block::PowderSnow if in_powder_snow => hold(Stuck::PowderSnow),
                    _ => {}
                }
                if state.block() == Block::Cactus {
                    contact.cactus = true;
                }
                if state.block() == Block::WitherRose {
                    contact.wither_rose = true;
                }
                if matches!(
                    state.block(),
                    Block::OpenEyeblossom | Block::ClosedEyeblossom
                ) && tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::block::key::BEE_ATTRACTIVE,
                    state.block(),
                ) {
                    contact.bee_poison = true;
                }
                if state.block() == Block::HoneyBlock
                    && against_honey_side(bounds, BlockPos::new(x, y, z))
                {
                    contact.honey_side = Some(state);
                }
                contact.cauldron =
                    contact
                        .cauldron
                        .merge(Cauldron::at(state, BlockPos::new(x, y, z), inner));
                contact.freezing = contact.freezing.merge(crate::world::freezing::Inside::at(
                    state,
                    BlockPos::new(x, y, z),
                ));
            }
        }
    }
    contact
}

/// Whether a box inside a honey block's cell is below its top and sticks
/// out past the block's inset side, so it rubs against that side.
fn against_honey_side(bounds: Aabb, pos: BlockPos) -> bool {
    const TOP: f64 = 0.9375;
    const SIDE: f64 = 0.4375;
    const EPSILON: f64 = 1.0e-7;
    if bounds[1] > f64::from(pos.y) + TOP - EPSILON {
        return false;
    }
    let half_width = (bounds[3] - bounds[0]) / 2.0;
    let dx = (f64::from(pos.x) + 0.5 - (bounds[0] + bounds[3]) / 2.0).abs();
    let dz = (f64::from(pos.z) + 0.5 - (bounds[2] + bounds[5]) / 2.0).abs();
    let overlap = SIDE + half_width;
    dx + EPSILON > overlap || dz + EPSILON > overlap
}

/// Whether a box, shrunk by the inside inset, overlaps a cactus block's
/// whole cell (cactus counts contact against the full cube, not its
/// narrower collision box, so resting on top or against a side touches).
pub fn touches_cactus(bounds: Aabb, block: &dyn Fn(BlockPos) -> Option<BlockState>) -> bool {
    let lo = [0, 1, 2].map(|axis| (bounds[axis] + INSIDE_INSET).floor() as i32);
    let hi = [3, 4, 5].map(|axis| (bounds[axis] - INSIDE_INSET).floor() as i32);
    (lo[0]..=hi[0]).any(|x| {
        (lo[1]..=hi[1]).any(|y| {
            (lo[2]..=hi[2])
                .any(|z| block(BlockPos::new(x, y, z)).is_some_and(|s| s.block() == Block::Cactus))
        })
    })
}

/// Hurts a non-living entity (dropped item, experience orb) touching a
/// cactus after its move, as the block does every tick to anything inside
/// its cell. Returns whether that destroyed it. Living things take cactus
/// damage through [`Contact`] instead.
///
/// Items and orbs collide with every block as a full cube, so they come to
/// rest on the cell's faces rather than on the cactus's collision box a
/// sixteenth inside them; their box is grown by that sixteenth sideways and
/// downwards so resting where the game would rest still counts.
pub fn pricked_by_cactus(
    entity: &mut dyn crate::world::entity::RegionEntity,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let b = entity.base().bounds();
    let grown = [
        b[0] - CACTUS_INSET,
        b[1] - CACTUS_INSET,
        b[2] - CACTUS_INSET,
        b[3] + CACTUS_INSET,
        b[4],
        b[5] + CACTUS_INSET,
    ];
    !entity.base().invulnerable
        && touches_cactus(grown, block)
        && entity.hurt(CACTUS_DAMAGE, &tuff_vanilla_data::damage_type::CACTUS)
}

/// The contact of a player whose box is `bounds`: the block it would stand
/// on is the one supporting that box, as the game tracks for players on
/// the ground. `eye_height` is the eyes' height above `bounds`' bottom,
/// which follows the player's pose (sneaking, swimming, ...).
pub fn player_contact(
    bounds: Aabb,
    eye_height: f64,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> Contact {
    player_contact_swept(bounds, bounds, eye_height, block)
}

/// [`player_contact`] for a player that moved from `previous` to `bounds`.
pub fn player_contact_swept(
    previous: Aabb,
    bounds: Aabb,
    eye_height: f64,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> Contact {
    let position = [
        (bounds[0] + bounds[3]) / 2.0,
        bounds[1],
        (bounds[2] + bounds[5]) / 2.0,
    ];
    let supporting = physics::supporting_block(bounds, block);
    let ground = block(on_pos(position, supporting, STEP_OFFSET, block));
    let mut result = swept_contact(EntityType::Player, previous, bounds, ground, block);
    let eye = [position[0], bounds[1] + eye_height, position[2]];
    let width = (bounds[3] - bounds[0]) as f32;
    result.suffocating = suffocates(eye, width, block);
    result.cramped = cramped_pose(position, width as f64, block);
    result
}

const STANDING_HEIGHT: f64 = 1.8;
const CROUCHING_HEIGHT: f64 = 1.5;
const SWIMMING_HEIGHT: f64 = 0.6;
const FIT_TOLERANCE: f64 = 1.0e-7;

fn fits_height(
    feet: [f64; 3],
    width: f64,
    height: f64,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    !blocked_by(feet, width, height, block, |_| true)
}

fn blocked_by(
    feet: [f64; 3],
    width: f64,
    height: f64,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
    counts: impl Fn(BlockState) -> bool,
) -> bool {
    let half = width / 2.0 - FIT_TOLERANCE;
    let area = [
        feet[0] - half,
        feet[1] + FIT_TOLERANCE,
        feet[2] - half,
        feet[0] + half,
        feet[1] + height - FIT_TOLERANCE,
        feet[2] + half,
    ];
    let min = physics::block_pos([area[0], area[1], area[2]]);
    let max = physics::block_pos([area[3], area[4], area[5]]);
    for x in min.x..=max.x {
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                let pos = BlockPos::new(x, y, z);
                if let Some(state) = block(pos)
                    && counts(state)
                    && physics::block_boxes(Some(state), pos).any(|b| physics::intersects(b, area))
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether a player's box at `feet`, `width` wide, fits standing (0),
/// only crouching (1) or neither (2).
pub fn cramped(feet: [f64; 3], width: f64, block: &dyn Fn(BlockPos) -> Option<BlockState>) -> u8 {
    let fits = |height: f64| fits_height(feet, width, height, block);
    if fits(STANDING_HEIGHT) {
        0
    } else if fits(CROUCHING_HEIGHT) {
        1
    } else {
        2
    }
}

/// Like `cramped`, but a box that does not even fit swimming (a door closed
/// on the player; a suffocating block does not count) leaves the pose alone (0).
pub fn cramped_pose(
    feet: [f64; 3],
    width: f64,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> u8 {
    let result = cramped(feet, width, block);
    if result == 2
        && !blocked_by(
            feet,
            width,
            SWIMMING_HEIGHT,
            block,
            BlockState::is_suffocating,
        )
        && !fits_height(feet, width, SWIMMING_HEIGHT, block)
    {
        0
    } else {
        result
    }
}

/// Whether a thing whose eyes are at `eye` and whose body is `width` wide
/// chokes: a thin slab centred on the eyes, `width * 0.8` wide, overlaps
/// the collision shape of a block marked as suffocating there. Checked
/// every tick, with no gate for sneaking or anything else (a sleeping
/// player is exempt, but that is handled by the caller).
pub fn suffocates(
    eye: [f64; 3],
    width: f32,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let half = width as f64 * SUFFOCATION_WIDTH_FRACTION / 2.0;
    let area = [
        eye[0] - half,
        eye[1] - SUFFOCATION_HALF_HEIGHT,
        eye[2] - half,
        eye[0] + half,
        eye[1] + SUFFOCATION_HALF_HEIGHT,
        eye[2] + half,
    ];
    let min = physics::block_pos([area[0], area[1], area[2]]);
    let max = physics::block_pos([area[3], area[4], area[5]]);
    for x in min.x..=max.x {
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                let pos = BlockPos::new(x, y, z);
                let Some(state) = block(pos) else {
                    continue;
                };
                if state.is_suffocating()
                    && physics::block_boxes(Some(state), pos).any(|b| physics::intersects(b, area))
                {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const EYE_HEIGHT: f64 = 1.62;

    fn player_box(position: [f64; 3]) -> Aabb {
        [
            position[0] - 0.3,
            position[1],
            position[2] - 0.3,
            position[0] + 0.3,
            position[1] + 1.8,
            position[2] + 0.3,
        ]
    }

    fn world(blocks: Vec<(BlockPos, BlockState)>) -> impl Fn(BlockPos) -> Option<BlockState> {
        move |pos| {
            Some(
                blocks
                    .iter()
                    .find(|(p, _)| *p == pos)
                    .map_or(Block::Air.default_state(), |(_, s)| *s),
            )
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_door_closed_on_the_player_does_not_force_a_pose() {
        let door = Block::OakDoor.default_state();
        let at = world(vec![
            (BlockPos::new(0, 64, 0), door),
            (BlockPos::new(0, 65, 0), door),
        ]);
        assert_eq!(cramped_pose([0.5, 64.0, 0.9], 0.6, &at), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn real_gaps_still_force_crouching_and_crawling() {
        let stone = Block::Stone.default_state();
        let gap = |y: i32| world(vec![(BlockPos::new(0, y, 0), stone)]);
        assert_eq!(cramped_pose([0.5, 64.5, 0.5], 0.6, &gap(66)), 1, "1.5 gap");
        assert_eq!(cramped_pose([0.5, 64.0, 0.5], 0.6, &gap(65)), 2, "1 gap");
        assert_eq!(cramped_pose([0.5, 64.0, 0.5], 0.6, &gap(67)), 0, "2 gap");
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_a_box_pressed_against_a_honey_side_below_its_top_slides() {
        let honey = Block::HoneyBlock.default_state();
        let at = world(vec![(BlockPos::new(0, 64, 0), honey)]);
        let side =
            |position| contact(EntityType::Player, player_box(position), None, &at).honey_side;
        assert_eq!(
            side([1.29, 64.0, 0.5]),
            Some(honey),
            "against the east side"
        );
        assert_eq!(side([1.29, 64.95, 0.5]), None, "above the top");
        assert_eq!(side([0.5, 63.0, 0.5]), None, "under it, not beside it");
        assert_eq!(side([1.35, 64.0, 0.5]), None, "clear of its cell");
        assert!(
            Contact {
                honey_side: Some(honey),
                ..Contact::default()
            }
            .is_none(),
            "honey hurts nothing"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_stuck_block_follows_the_path() {
        let both = world(vec![
            (BlockPos::new(0, 64, 0), Block::Cobweb.default_state()),
            (
                BlockPos::new(2, 64, 0),
                Block::SweetBerryBush.default_state(),
            ),
        ]);
        let (a, b) = (player_box([0.5, 64.0, 0.5]), player_box([2.5, 64.0, 0.5]));
        let east = swept_contact(EntityType::Pig, a, b, None, &both);
        assert_eq!(east.stuck, Some(Stuck::BerryBush));
        let west = swept_contact(EntityType::Pig, b, a, None, &both);
        assert_eq!(west.stuck, Some(Stuck::Cobweb));
    }

    #[tuff_core::watchdog]
    #[test]
    fn blocks_crossed_during_the_move_still_count() {
        let web = world(vec![(
            BlockPos::new(0, 64, 0),
            Block::Cobweb.default_state(),
        )]);
        let from = player_box([0.5, 64.0, 0.5]);
        let to = player_box([3.5, 64.0, 0.5]);
        let swept = swept_contact(EntityType::Player, from, to, None, &web);
        assert_eq!(swept.stuck, Some(Stuck::Cobweb));
        assert_eq!(contact(EntityType::Player, to, None, &web).stuck, None);
        let cactus = world(vec![(
            BlockPos::new(1, 64, 0),
            Block::Cactus.default_state(),
        )]);
        assert!(swept_contact(EntityType::Player, from, to, None, &cactus).cactus);
        assert!(player_contact_swept(from, to, EYE_HEIGHT, &cactus).cactus);
        assert!(!player_contact(to, EYE_HEIGHT, &cactus).cactus);
    }

    #[tuff_core::watchdog]
    #[test]
    fn cobwebs_bushes_and_powder_snow_at_the_feet_hold_things_back() {
        let at = |block: Block| world(vec![(BlockPos::new(0, 64, 0), block.default_state())]);
        let stuck = |kind, position, block: Block| {
            contact(kind, player_box(position), None, &at(block)).stuck
        };
        let pig = EntityType::Pig;
        assert_eq!(
            stuck(pig, [0.5, 64.5, 0.5], Block::Cobweb),
            Some(Stuck::Cobweb)
        );
        assert_eq!(
            stuck(EntityType::Spider, [0.5, 64.5, 0.5], Block::Cobweb),
            None
        );
        assert_eq!(
            stuck(EntityType::CaveSpider, [0.5, 64.5, 0.5], Block::Cobweb),
            None
        );
        assert_eq!(
            stuck(pig, [0.5, 63.0, 0.5], Block::Cobweb),
            Some(Stuck::Cobweb),
            "a head in a cobweb is enough"
        );
        assert_eq!(
            stuck(pig, [0.5, 64.0, 0.5], Block::SweetBerryBush),
            Some(Stuck::BerryBush)
        );
        assert_eq!(
            stuck(EntityType::Fox, [0.5, 64.0, 0.5], Block::SweetBerryBush),
            None
        );
        assert_eq!(
            stuck(pig, [0.5, 64.2, 0.5], Block::PowderSnow),
            Some(Stuck::PowderSnow)
        );
        assert_eq!(
            stuck(pig, [0.5, 63.0, 0.5], Block::PowderSnow),
            None,
            "powder snow holds only what has its feet in it"
        );
        assert_eq!(stuck(pig, [2.5, 64.0, 0.5], Block::Cobweb), None);
        assert_eq!(Stuck::Cobweb.multiplier(true), [0.5, 0.25, 0.5]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn magma_under_the_feet_is_a_hot_floor() {
        let block = world(vec![(
            BlockPos::new(0, 63, 0),
            Block::MagmaBlock.default_state(),
        )]);
        assert!(player_contact(player_box([0.5, 64.0, 0.5]), EYE_HEIGHT, &block).hot_floor);
        assert!(player_contact(player_box([0.9, 64.0, 0.5]), EYE_HEIGHT, &block).hot_floor);
        assert!(!player_contact(player_box([2.5, 64.0, 0.5]), EYE_HEIGHT, &block).hot_floor);
        let stone = world(vec![(
            BlockPos::new(0, 63, 0),
            Block::Stone.default_state(),
        )]);
        assert!(!player_contact(player_box([0.5, 64.0, 0.5]), EYE_HEIGHT, &stone).hot_floor);
    }

    #[tuff_core::watchdog]
    #[test]
    fn lit_campfires_burn_by_their_own_damage() {
        let pos = BlockPos::new(0, 64, 0);
        let lit = |block: Block| world(vec![(pos, block.default_state())]);
        let on_top = player_box([0.5, 64.4375, 0.5]);
        assert_eq!(
            player_contact(on_top, EYE_HEIGHT, &lit(Block::Campfire)).campfire,
            Some(1.0)
        );
        assert_eq!(
            player_contact(on_top, EYE_HEIGHT, &lit(Block::SoulCampfire)).campfire,
            Some(2.0)
        );
        let unlit = world(vec![(
            pos,
            Block::Campfire
                .default_state()
                .with("lit", "false")
                .unwrap(),
        )]);
        assert_eq!(player_contact(on_top, EYE_HEIGHT, &unlit).campfire, None);
        let beside = player_box([1.3 + 1.0e-4, 64.0, 0.5]);
        assert_eq!(
            player_contact(beside, EYE_HEIGHT, &lit(Block::Campfire)).campfire,
            None
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn grown_berry_bushes_prick_what_moves_through_them() {
        let pos = BlockPos::new(0, 64, 0);
        let bush = |age: &str| {
            world(vec![(
                pos,
                Block::SweetBerryBush
                    .default_state()
                    .with("age", age)
                    .unwrap(),
            )])
        };
        let inside = player_box([0.5, 64.0, 0.5]);
        let grown = player_contact(inside, EYE_HEIGHT, &bush("1"));
        assert!(grown.pricks([0.01, 0.0, 0.0]));
        assert!(!grown.pricks([0.001, 0.5, 0.002]));
        assert!(!player_contact(inside, EYE_HEIGHT, &bush("0")).berry_bush);
        let fox = contact(EntityType::Fox, inside, None, &bush("3"));
        assert!(!fox.berry_bush);
    }

    #[tuff_core::watchdog]
    #[test]
    fn cactus_pricks_whatever_touches_any_face() {
        let pos = BlockPos::new(0, 64, 0);
        let cactus = world(vec![(pos, Block::Cactus.default_state())]);
        assert!(player_contact(player_box([0.5, 64.0, 0.5]), EYE_HEIGHT, &cactus).cactus);
        let beside = player_box([1.3 - 1.0e-4, 64.0, 0.5]);
        assert!(player_contact(beside, EYE_HEIGHT, &cactus).cactus);
        let far = player_box([3.0, 64.0, 0.5]);
        assert!(!player_contact(far, EYE_HEIGHT, &cactus).cactus);
        let fox = contact(EntityType::Fox, player_box([0.5, 64.0, 0.5]), None, &cactus);
        assert!(fox.cactus);
    }

    #[tuff_core::watchdog]
    #[test]
    fn wither_rose_is_felt_by_whatever_is_inside() {
        let pos = BlockPos::new(0, 64, 0);
        let rose = world(vec![(pos, Block::WitherRose.default_state())]);
        assert!(player_contact(player_box([0.5, 64.0, 0.5]), EYE_HEIGHT, &rose).wither_rose);
        assert!(!player_contact(player_box([3.0, 64.0, 0.5]), EYE_HEIGHT, &rose).wither_rose);
    }

    #[tuff_core::watchdog]
    #[test]
    fn eyeblossoms_mark_what_is_inside_for_bee_poison() {
        let pos = BlockPos::new(0, 64, 0);
        let open = world(vec![(pos, Block::OpenEyeblossom.default_state())]);
        let inside = contact(EntityType::Bee, player_box([0.5, 64.0, 0.5]), None, &open);
        assert!(inside.bee_poison);
        let beside = contact(EntityType::Bee, player_box([3.0, 64.0, 0.5]), None, &open);
        assert!(!beside.bee_poison);
        let closed = world(vec![(pos, Block::ClosedEyeblossom.default_state())]);
        let inside = contact(EntityType::Bee, player_box([0.5, 64.0, 0.5]), None, &closed);
        assert!(!inside.bee_poison);
    }

    #[tuff_core::watchdog]
    #[test]
    fn cactus_wears_down_and_destroys_resting_items() {
        use crate::world::entity::RegionEntity;
        use crate::world::items::ItemEntity;
        use tuff_item::item::{ItemStack, MintReason};
        use tuff_vanilla_data::Item;
        let cactus = world(vec![(
            BlockPos::new(0, 64, 0),
            Block::Cactus.default_state(),
        )]);
        let destroyed_after =
            |entity: &mut dyn RegionEntity| (1..=10).find(|_| pricked_by_cactus(entity, &cactus));
        for (item, at) in [
            (Item::Dirt, [0.5, 65.0, 0.5]),
            (Item::NetheriteIngot, [0.5, 65.0, 0.5]),
            (Item::Dirt, [1.125, 64.0, 0.5]),
        ] {
            let mut entity = ItemEntity::new(
                1,
                1,
                ItemStack::mint(item, 1, MintReason::Test),
                at,
                [0.0; 3],
                0,
            );
            assert_eq!(destroyed_after(&mut entity), Some(5), "{item:?} at {at:?}");
        }
        let mut away = ItemEntity::new(
            1,
            1,
            ItemStack::mint(Item::Dirt, 1, MintReason::Test),
            [1.3, 64.0, 0.5],
            [0.0; 3],
            0,
        );
        assert_eq!(destroyed_after(&mut away), None);
        let mut invulnerable = ItemEntity::new(
            1,
            1,
            ItemStack::mint(Item::Dirt, 1, MintReason::Test),
            [0.5, 65.0, 0.5],
            [0.0; 3],
            0,
        );
        invulnerable.base.invulnerable = true;
        assert_eq!(destroyed_after(&mut invulnerable), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_suffocating_block_at_eye_height_chokes_but_glass_does_not() {
        let bounds = player_box([0.5, 64.0, 0.5]);
        let stone = world(vec![(
            BlockPos::new(0, 65, 0),
            Block::Stone.default_state(),
        )]);
        assert!(player_contact(bounds, EYE_HEIGHT, &stone).suffocating);
        let glass = world(vec![(
            BlockPos::new(0, 65, 0),
            Block::Glass.default_state(),
        )]);
        assert!(
            !player_contact(bounds, EYE_HEIGHT, &glass).suffocating,
            "glass has a full collision shape but is never suffocating"
        );
        let feet_only = world(vec![(
            BlockPos::new(0, 64, 0),
            Block::Stone.default_state(),
        )]);
        assert!(
            !player_contact(bounds, EYE_HEIGHT, &feet_only).suffocating,
            "only the block at the eyes matters, not the rest of the body"
        );
        let clear = world(vec![]);
        assert!(!player_contact(bounds, EYE_HEIGHT, &clear).suffocating);
    }

    #[tuff_core::watchdog]
    #[test]
    fn bubble_columns_push_by_their_drag_and_harder_at_the_top() {
        let column = |drag: &str| {
            Block::BubbleColumn
                .default_state()
                .with("drag", drag)
                .unwrap()
        };
        let water = Block::Water.default_state();
        let item = |y: f64| [0.4, y, 0.4, 0.6, y + 0.25, 0.6];
        let up = world(vec![
            (BlockPos::new(0, 63, 0), column("false")),
            (BlockPos::new(0, 64, 0), column("false")),
            (BlockPos::new(0, 65, 0), water),
        ]);
        let mut velocity = [0.0; 3];
        assert!(bubble_column_push(item(63.2), &mut velocity, &up));
        assert!((velocity[1] - 0.06).abs() < 1e-9);
        velocity[1] = 0.68;
        bubble_column_push(item(63.2), &mut velocity, &up);
        assert_eq!(velocity[1], 0.7);

        let top = world(vec![(BlockPos::new(0, 63, 0), column("false"))]);
        let mut velocity = [0.0; 3];
        assert!(!bubble_column_push(item(63.2), &mut velocity, &top));
        assert!((velocity[1] - 0.1).abs() < 1e-9);

        let down = world(vec![
            (BlockPos::new(0, 63, 0), column("true")),
            (BlockPos::new(0, 64, 0), water),
        ]);
        let mut velocity = [0.0, -0.29, 0.0];
        bubble_column_push(item(63.2), &mut velocity, &down);
        assert_eq!(velocity[1], -0.3);
        let mut velocity = [0.0; 3];
        bubble_column_push(item(62.0), &mut velocity, &down);
        assert_eq!(velocity[1], 0.0, "a box below the column is untouched");
    }

    #[tuff_core::watchdog]
    #[test]
    fn projectiles_get_uncapped_pushes_from_a_column() {
        let column = |drag: &str| {
            Block::BubbleColumn
                .default_state()
                .with("drag", drag)
                .unwrap()
        };
        let water = Block::Water.default_state();
        let arrow = |y: f64| [0.4, y, 0.4, 0.6, y + 0.25, 0.6];
        let up = world(vec![
            (BlockPos::new(0, 63, 0), column("false")),
            (BlockPos::new(0, 64, 0), water),
        ]);
        let mut velocity = [0.0, 0.69, 0.0];
        assert!(projectile_bubble_column_push(
            arrow(63.2),
            &mut velocity,
            &up
        ));
        assert!((velocity[1] - 0.75).abs() < 1e-9, "no 0.7 cap inside");
        let down = world(vec![
            (BlockPos::new(0, 63, 0), column("true")),
            (BlockPos::new(0, 64, 0), water),
        ]);
        let mut velocity = [0.0, -0.29, 0.0];
        projectile_bubble_column_push(arrow(63.2), &mut velocity, &down);
        assert!((velocity[1] + 0.32).abs() < 1e-9, "no -0.3 cap inside");
        let top = world(vec![(BlockPos::new(0, 63, 0), column("false"))]);
        let mut velocity = [0.0, 1.75, 0.0];
        assert!(!projectile_bubble_column_push(
            arrow(63.2),
            &mut velocity,
            &top
        ));
        assert!((velocity[1] - 1.85).abs() < 1e-9, "no 1.8 cap at the top");
    }
}

/// Pushes a body whose box is `bounds` along the bubble columns it is
/// inside: up over soul sand, down over magma, harder in the cell at the
/// top of a column. Each column cell overlapped pushes once. Returns
/// whether any cell below the top was overlapped (which forgets a fall).
pub fn bubble_column_push(
    bounds: Aabb,
    velocity: &mut [f64; 3],
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let mut inside = false;
    for_each_bubble_column(bounds, block, |cell| {
        velocity[1] = cell.capped_push(velocity[1]);
        inside |= !cell.open_above;
    });
    inside
}

/// Like `bubble_column_push`, but for projectiles, whose pushes are
/// uncapped. Returns whether any cell below the top was overlapped.
pub fn projectile_bubble_column_push(
    bounds: Aabb,
    velocity: &mut [f64; 3],
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let mut inside = false;
    for_each_bubble_column(bounds, block, |cell| {
        velocity[1] += match (cell.open_above, cell.drag) {
            (_, true) => -0.03,
            (true, false) => 0.1,
            (false, false) => 0.06,
        };
        inside |= !cell.open_above;
    });
    inside
}

/// One bubble column cell a box overlaps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BubbleCell {
    /// Pulls down (over magma) rather than lifting (over soul sand).
    pub drag: bool,
    /// Nothing solid and no fluid above: the top of the column.
    pub open_above: bool,
}

impl BubbleCell {
    /// The common push this cell gives a vertical velocity.
    pub fn capped_push(self, y: f64) -> f64 {
        match (self.open_above, self.drag) {
            (true, true) => (y - 0.03).max(-0.9),
            (true, false) => (y + 0.1).min(1.8),
            (false, true) => (y - 0.03).max(-0.3),
            (false, false) => (y + 0.06).min(0.7),
        }
    }
}

/// Calls `visit` for every bubble column cell `bounds` overlaps.
pub fn for_each_bubble_column(
    bounds: Aabb,
    block: &dyn Fn(BlockPos) -> Option<BlockState>,
    mut visit: impl FnMut(BubbleCell),
) {
    let min = [0, 1, 2].map(|i| (bounds[i] + INSIDE_INSET).floor() as i32);
    let max = [0, 1, 2].map(|i| (bounds[i + 3] - INSIDE_INSET).floor() as i32);
    for x in min[0]..=max[0] {
        for y in min[1]..=max[1] {
            for z in min[2]..=max[2] {
                let Some(state) = block(BlockPos::new(x, y, z)) else {
                    continue;
                };
                if state.block() != Block::BubbleColumn {
                    continue;
                }
                let drag = state.get("drag") == Some("true");
                let open_above = block(BlockPos::new(x, y + 1, z)).is_some_and(|above| {
                    above.collision_boxes().is_empty() && above.fluid_state().is_empty()
                });
                visit(BubbleCell { drag, open_above });
            }
        }
    }
}
