use tuff_core::protocol::play::{GameMode, Serverbound};
use tuff_core::world::session::mirror::ChunkMirror;
use tuff_vanilla_data::{Block, BlockState};
use tuff_world_model::{BlockPos, Face};

use super::{Check, Facts, Flag, sight};
use crate::violations::Bucket;

const PLANE_SLACK: f64 = 0.4;
const CURSOR_SLACK: f64 = 0.01;
const RAY_MISS: f64 = 1.5;
const PLACES_PER_TICK: u32 = 4;
const SNAP_DEGREES: f32 = 45.0;
const RETURN_DEGREES: f32 = 10.0;
const BACKWARD_DEGREES: f64 = 100.0;
const MIN_SPRINT_STEP: f64 = 0.15;

/// What the client's world says about the block a `UseItemOn` targets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub eye: [f64; 3],
    /// Yaw and pitch the server last accepted.
    pub rotation: [f32; 2],
    /// The clicked block, `None` when its chunk was not sent.
    pub target: Option<BlockState>,
    /// A full block lies between the eyes and the clicked point.
    pub obstructed: bool,
    /// The clicking hand holds an item that places a block.
    pub holding_block: bool,
}

fn hit_point(position: BlockPos, cursor: [f32; 3]) -> [f64; 3] {
    [
        f64::from(position.x) + f64::from(cursor[0]),
        f64::from(position.y) + f64::from(cursor[1]),
        f64::from(position.z) + f64::from(cursor[2]),
    ]
}

fn is_full_cube(boxes: &[[f32; 6]]) -> bool {
    boxes == [[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]]
}

impl Placement {
    pub fn probe(
        mirror: &ChunkMirror,
        eye: [f64; 3],
        rotation: [f32; 2],
        holding_block: bool,
        packet: &Serverbound,
    ) -> Option<Placement> {
        let Serverbound::UseItemOn {
            position, cursor, ..
        } = packet
        else {
            return None;
        };
        let target = mirror.block_at(*position);
        Some(Placement {
            eye,
            rotation,
            target,
            obstructed: target.is_some()
                && sight::blocked(mirror, eye, hit_point(*position, *cursor), Some(*position))
                    .unwrap_or(false),
            holding_block,
        })
    }
}

fn look(rotation: [f32; 2]) -> [f64; 3] {
    let yaw = f64::from(rotation[0]).to_radians();
    let pitch = f64::from(rotation[1]).to_radians();
    [
        -yaw.sin() * pitch.cos(),
        -pitch.sin(),
        yaw.cos() * pitch.cos(),
    ]
}

fn yaw_delta(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

fn turn(a: [f32; 2], b: [f32; 2]) -> f32 {
    yaw_delta(a[0], b[0]).max((a[1] - b[1]).abs())
}

/// Whether a ray from `eye` along `rotation` passes within `RAY_MISS` of
/// `hit`, in front of the eyes.
fn aims_at(eye: [f64; 3], rotation: [f32; 2], hit: [f64; 3]) -> bool {
    let dir = look(rotation);
    let to = [hit[0] - eye[0], hit[1] - eye[1], hit[2] - eye[2]];
    let along = to[0] * dir[0] + to[1] * dir[1] + to[2] * dir[2];
    let length2 = to[0] * to[0] + to[1] * to[1] + to[2] * to[2];
    if length2 < RAY_MISS * RAY_MISS {
        return true;
    }
    along > 0.0 && (length2 - along * along).max(0.0).sqrt() <= RAY_MISS
}

/// The face and cursor of a click disagree with the clicked block, or the
/// eyes are behind the clicked face.
fn bad_face(
    target: BlockState,
    face: Face,
    cursor: [f32; 3],
    eye: [f64; 3],
    pos: BlockPos,
) -> bool {
    let (nx, ny, nz) = face.normal();
    let normal = [f64::from(nx), f64::from(ny), f64::from(nz)];
    let hit = hit_point(pos, cursor);
    let outward = (0..3)
        .map(|axis| (eye[axis] - hit[axis]) * normal[axis])
        .sum::<f64>();
    if outward < -PLANE_SLACK {
        return true;
    }
    if is_full_cube(target.outline_boxes()) {
        let axis = normal.iter().position(|n| *n != 0.0).unwrap_or(0);
        let expected = if normal[axis] > 0.0 { 1.0 } else { 0.0 };
        return (f64::from(cursor[axis]) - expected).abs() > CURSOR_SLACK;
    }
    false
}

fn unclickable(target: BlockState) -> bool {
    target.outline_boxes().is_empty()
        && !matches!(
            target.block(),
            Block::MovingPiston | Block::Light | Block::StructureVoid
        )
}

#[derive(Clone, Copy)]
struct Pending {
    rotation: [f32; 2],
    hit: [f64; 3],
    eye: [f64; 3],
}

/// Where a click lands: against air or liquid, on a face that cannot be
/// seen or does not match the cursor, through a wall, or away from where
/// the player looks.
pub struct PlaceValidity {
    rotation: Option<[f32; 2]>,
    pending: Option<Pending>,
    aim_misses: Bucket,
}

impl Default for PlaceValidity {
    fn default() -> Self {
        Self {
            rotation: None,
            pending: None,
            aim_misses: Bucket::new(3.0, 0.1),
        }
    }
}

impl Check for PlaceValidity {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        match packet {
            Serverbound::Move(movement) => {
                if let Some((yaw, pitch)) = movement.rotation {
                    self.rotation = Some([yaw, pitch]);
                }
                None
            }
            Serverbound::ClientTickEnd => {
                let pending = self.pending.take()?;
                let rotation = self.rotation?;
                let aimed = aims_at(pending.eye, rotation, pending.hit)
                    || aims_at(pending.eye, pending.rotation, pending.hit);
                if aimed || self.aim_misses.allow(1.0, facts.tick) {
                    return None;
                }
                Flag::cancel("place_aim", 1.0)
            }
            Serverbound::UseItemOn {
                position,
                face,
                cursor,
                ..
            } => {
                let place = facts.placement?;
                if facts.game_mode == GameMode::Spectator || facts.teleport_pending {
                    return None;
                }
                self.rotation = self.rotation.or(Some(place.rotation));
                let target = place.target?;
                if unclickable(target) && !facts.instabuild {
                    return Flag::cancel("place_air", 2.0);
                }
                if bad_face(target, *face, *cursor, place.eye, *position) {
                    return Flag::cancel("place_face", 2.0);
                }
                if place.obstructed {
                    return Flag::cancel("place_wall", 1.5);
                }
                self.pending = Some(Pending {
                    rotation: place.rotation,
                    hit: hit_point(*position, *cursor),
                    eye: place.eye,
                });
                None
            }
            _ => None,
        }
    }
}

/// A swing already under way suppresses the packet of the next click.
const SWING_ANIMATION_TICKS: u64 = 6;

/// A block that would fill the body of the placer is refused on the client
/// without a swing.
fn blocked_by_body(facts: &Facts, face: Face, position: BlockPos) -> bool {
    fills_body(facts.position, face, position)
}

pub fn fills_body(p: [f64; 3], face: Face, position: BlockPos) -> bool {
    let (nx, ny, nz) = face.normal();
    let cell = [
        f64::from(position.x + nx),
        f64::from(position.y + ny),
        f64::from(position.z + nz),
    ];
    let height = 1.8;
    p[0] + 0.3 > cell[0]
        && p[0] - 0.3 < cell[0] + 1.0
        && p[1] + height > cell[1]
        && p[1] < cell[1] + 1.0
        && p[2] + 0.3 > cell[2]
        && p[2] - 0.3 < cell[2] + 1.0
}

/// Bridging patterns: placing without a swing, sprinting backwards while
/// bridging, more placements per tick than clicks allow, snapping the view
/// to the block and back, and switching to a block and away around one
/// placement.
pub struct Scaffold {
    swing_misses: Bucket,
    swaps: Bucket,
    snaps: Bucket,
    placed: u32,
    awaiting_swing: bool,
    switched: bool,
    switched_then_placed: bool,
    last_step: [f64; 2],
    tick_rotation: Option<[f32; 2]>,
    before: Option<[f32; 2]>,
    snapped: Option<([f32; 2], [f32; 2])>,
    placed_this_tick: bool,
    last_swing: Option<u64>,
}

impl Default for Scaffold {
    fn default() -> Self {
        Self {
            swing_misses: Bucket::new(4.0, 0.1),
            swaps: Bucket::new(4.0, 0.1),
            snaps: Bucket::new(3.0, 0.1),
            placed: 0,
            awaiting_swing: false,
            switched: false,
            switched_then_placed: false,
            last_step: [0.0; 2],
            tick_rotation: None,
            before: None,
            snapped: None,
            placed_this_tick: false,
            last_swing: None,
        }
    }
}

impl Scaffold {
    fn end_tick(&mut self, facts: &Facts) -> Option<Flag> {
        let missed = std::mem::take(&mut self.awaiting_swing);
        let now = self.tick_rotation;
        let mut flag = None;
        if let (Some((before, at)), Some(now)) = (self.snapped.take(), now)
            && turn(at, now) > SNAP_DEGREES
            && turn(before, now) < RETURN_DEGREES
            && !self.snaps.allow(1.0, facts.tick)
        {
            flag = Flag::cancel("scaffold", 1.0);
        }
        if self.placed_this_tick
            && let (Some(before), Some(at)) = (self.before, now)
            && turn(before, at) > SNAP_DEGREES
        {
            self.snapped = Some((before, at));
        }
        self.before = now;
        self.placed = 0;
        self.placed_this_tick = false;
        self.switched = false;
        self.switched_then_placed = false;
        if missed && !self.swing_misses.allow(1.0, facts.tick) {
            flag = Flag::cancel("scaffold", 1.0);
        }
        flag
    }

    fn sprinting_backwards(&self, facts: &Facts, face: Face, position: BlockPos) -> bool {
        let Some(place) = facts.placement else {
            return false;
        };
        let (nx, ny, nz) = face.normal();
        let below = f64::from(position.y) < facts.position[1].floor();
        let step = self.last_step;
        let speed = step[0].hypot(step[1]);
        if !facts.sprinting || ny != 0 || !below || speed < MIN_SPRINT_STEP {
            return false;
        }
        let extends = step[0] * f64::from(nx) + step[1] * f64::from(nz) > 0.0;
        let dir = look(place.rotation);
        let facing = dir[0].hypot(dir[2]);
        if facing < 1e-6 {
            return extends;
        }
        let cos = (step[0] * dir[0] + step[1] * dir[2]) / (speed * facing);
        extends && cos.clamp(-1.0, 1.0).acos().to_degrees() > BACKWARD_DEGREES
    }
}

impl Check for Scaffold {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        match packet {
            Serverbound::Move(movement) => {
                if let Some((yaw, pitch)) = movement.rotation {
                    self.tick_rotation = Some([yaw, pitch]);
                }
                if let Some(p) = movement.position {
                    self.last_step = [p[0] - facts.position[0], p[2] - facts.position[2]];
                }
                None
            }
            Serverbound::Punch => {
                self.awaiting_swing = false;
                self.last_swing = Some(facts.tick);
                None
            }
            Serverbound::SetCarriedItem(_) => {
                if self.switched_then_placed {
                    self.switched_then_placed = false;
                    if !self.swaps.allow(1.0, facts.tick) {
                        return Flag::cancel("scaffold", 1.0);
                    }
                }
                self.switched = true;
                None
            }
            Serverbound::ClientTickEnd => self.end_tick(facts),
            Serverbound::UseItemOn { position, face, .. } => {
                let place = facts.placement?;
                if !place.holding_block
                    || facts.instabuild
                    || facts.game_mode == GameMode::Spectator
                {
                    return None;
                }
                self.tick_rotation = self.tick_rotation.or(Some(place.rotation));
                let swung_recently = self
                    .last_swing
                    .is_some_and(|t| facts.tick.saturating_sub(t) <= SWING_ANIMATION_TICKS);
                self.awaiting_swing = !swung_recently && !blocked_by_body(facts, *face, *position);
                self.placed_this_tick = true;
                self.switched_then_placed |= self.switched;
                self.placed += 1;
                if self.placed > PLACES_PER_TICK {
                    return Flag::cancel("scaffold", 1.0);
                }
                if self.sprinting_backwards(facts, *face, *position) {
                    return Flag::cancel("scaffold", 1.5);
                }
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;
    use tuff_core::protocol::play::Movement;

    fn stone() -> BlockState {
        Block::Stone.default_state()
    }

    fn place(eye: [f64; 3], rotation: [f32; 2], target: Option<BlockState>) -> Placement {
        Placement {
            eye,
            rotation,
            target,
            obstructed: false,
            holding_block: true,
        }
    }

    fn click(pos: BlockPos, face: Face, cursor: [f32; 3]) -> Serverbound {
        Serverbound::UseItemOn {
            hand: 0,
            position: pos,
            face,
            cursor,
            sequence: 0,
        }
    }

    fn rotate(yaw: f32, pitch: f32) -> Serverbound {
        Serverbound::Move(Movement {
            horizontal_collision: false,
            position: None,
            rotation: Some((yaw, pitch)),
            on_ground: true,
        })
    }

    fn top_click() -> Serverbound {
        click(BlockPos::new(0, 63, 2), Face::Up, [0.5, 1.0, 0.5])
    }

    fn facing_down_ahead() -> Facts {
        let mut f = facts();
        f.placement = Some(place([0.5, 65.62, 0.5], [0.0, 45.0], Some(stone())));
        f
    }

    fn name(flag: Option<Flag>) -> Option<&'static str> {
        flag.map(|f| f.check)
    }

    #[tuff_core::watchdog]
    #[test]
    fn legit_clicks_pass() {
        let f = facing_down_ahead();
        let mut check = PlaceValidity::default();
        assert_eq!(check.examine(&f, &rotate(0.0, 45.0)), None);
        assert_eq!(check.examine(&f, &top_click()), None);
        assert_eq!(check.examine(&f, &Serverbound::ClientTickEnd), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn air_and_liquid_targets_are_flagged() {
        let mut check = PlaceValidity::default();
        for target in [Block::Air, Block::Water, Block::Lava] {
            let mut f = facts();
            f.placement = Some(place(
                [0.5, 65.62, 0.5],
                [0.0, 45.0],
                Some(target.default_state()),
            ));
            assert_eq!(name(check.examine(&f, &top_click())), Some("place_air"));
        }
        let mut unknown = facts();
        unknown.placement = Some(place([0.5, 65.62, 0.5], [0.0, 45.0], None));
        assert_eq!(check.examine(&unknown, &top_click()), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn hidden_or_mismatched_faces_are_flagged() {
        let f = facing_down_ahead();
        let mut check = PlaceValidity::default();
        let bottom = click(BlockPos::new(0, 63, 2), Face::Down, [0.5, 0.0, 0.5]);
        assert_eq!(name(check.examine(&f, &bottom)), Some("place_face"));
        let floating = click(BlockPos::new(0, 63, 2), Face::Up, [0.5, 0.4, 0.5]);
        assert_eq!(name(check.examine(&f, &floating)), Some("place_face"));
        let mut slab = facts();
        slab.placement = Some(place(
            [0.5, 65.62, 0.5],
            [0.0, 45.0],
            Some(Block::StoneSlab.default_state()),
        ));
        let half = click(BlockPos::new(0, 63, 2), Face::Up, [0.5, 0.5, 0.5]);
        assert_eq!(check.examine(&slab, &half), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn walls_and_looking_away_are_flagged() {
        let mut f = facing_down_ahead();
        let mut walled = f.placement.unwrap();
        walled.obstructed = true;
        f.placement = Some(walled);
        let mut check = PlaceValidity::default();
        assert_eq!(name(check.examine(&f, &top_click())), Some("place_wall"));

        let f = facing_down_ahead();
        let mut check = PlaceValidity::default();
        let mut flags = Vec::new();
        let mut away = facts();
        away.placement = Some(place([0.5, 65.62, 0.5], [180.0, -30.0], Some(stone())));
        for _ in 0..5 {
            check.examine(&away, &rotate(180.0, -30.0));
            check.examine(&away, &top_click());
            flags.extend(name(check.examine(&away, &Serverbound::ClientTickEnd)));
        }
        assert_eq!(flags, ["place_aim", "place_aim"]);
        let mut late_turn = PlaceValidity::default();
        for _ in 0..10 {
            late_turn.examine(&f, &top_click());
            late_turn.examine(&f, &rotate(180.0, -30.0));
            assert_eq!(late_turn.examine(&f, &Serverbound::ClientTickEnd), None);
        }
    }

    fn scaffold_tick(check: &mut Scaffold, f: &Facts, swing: bool) -> Vec<&'static str> {
        let mut flags = Vec::new();
        flags.extend(name(check.examine(f, &top_click())));
        if swing {
            flags.extend(name(check.examine(f, &Serverbound::Punch)));
        }
        flags.extend(name(check.examine(f, &Serverbound::ClientTickEnd)));
        flags
    }

    #[tuff_core::watchdog]
    #[test]
    fn swinging_placements_pass_and_silent_ones_flag() {
        let mut f = facing_down_ahead();
        let mut check = Scaffold::default();
        for tick in 0..200 {
            f.tick = tick * 5;
            assert!(scaffold_tick(&mut check, &f, true).is_empty());
        }
        let flagged: usize = (0..40)
            .map(|i| {
                f.tick = 1010 + i * 7;
                scaffold_tick(&mut check, &f, false).len()
            })
            .sum();
        assert!(flagged >= 5);
    }

    #[tuff_core::watchdog]
    #[test]
    fn clicks_inside_a_running_swing_need_no_swing() {
        let mut f = facing_down_ahead();
        let mut check = Scaffold::default();
        f.tick = 100;
        assert!(scaffold_tick(&mut check, &f, true).is_empty());
        for i in 1..=6 {
            f.tick = 100 + i;
            assert!(scaffold_tick(&mut check, &f, false).is_empty());
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_block_filling_the_body_is_not_expected_to_swing() {
        let f = facing_down_ahead();
        let at = BlockPos::new(
            f.position[0].floor() as i32,
            f.position[1].floor() as i32,
            f.position[2].floor() as i32,
        );
        assert!(blocked_by_body(
            &f,
            Face::Up,
            BlockPos::new(at.x, at.y - 1, at.z)
        ));
        assert!(!blocked_by_body(
            &f,
            Face::Up,
            BlockPos::new(at.x, at.y - 2, at.z)
        ));
    }

    #[tuff_core::watchdog]
    #[test]
    fn too_many_placements_in_a_tick_flag() {
        let f = facing_down_ahead();
        let mut check = Scaffold::default();
        for _ in 0..4 {
            assert_eq!(check.examine(&f, &top_click()), None);
        }
        assert_eq!(name(check.examine(&f, &top_click())), Some("scaffold"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn snapping_to_place_and_back_flags() {
        let f = facing_down_ahead();
        let mut check = Scaffold::default();
        let mut run = |rotation: (f32, f32), place: bool| {
            check.examine(&f, &rotate(rotation.0, rotation.1));
            if place {
                check.examine(&f, &top_click());
                check.examine(&f, &Serverbound::Punch);
            }
            name(check.examine(&f, &Serverbound::ClientTickEnd))
        };
        assert_eq!(run((0.0, 0.0), false), None);
        assert_eq!(run((180.0, 80.0), true), None);
        assert_eq!(run((2.0, 1.0), false), None);
        let flagged = (0..6)
            .filter(|_| {
                run((180.0, 80.0), true);
                run((2.0, 1.0), false).is_some()
            })
            .count();
        assert!(flagged >= 2);
        assert_eq!(run((60.0, 0.0), false), None);
        assert_eq!(run((70.0, 0.0), true), None);
        assert_eq!(run((72.0, 0.0), false), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn sprinting_backwards_while_bridging_flags() {
        let mut f = facts();
        f.sprinting = true;
        f.position = [0.5, 64.0, 0.5];
        let side = click(BlockPos::new(0, 63, 0), Face::South, [0.5, 0.5, 1.0]);
        let step = Serverbound::Move(Movement {
            horizontal_collision: false,
            position: Some([0.5, 64.0, 0.8]),
            rotation: None,
            on_ground: true,
        });
        f.placement = Some(place([0.5, 65.62, 0.8], [180.0, 80.0], Some(stone())));
        let mut check = Scaffold::default();
        check.examine(&f, &step);
        assert_eq!(name(check.examine(&f, &side)), Some("scaffold"));
        f.sprinting = false;
        let mut walking = Scaffold::default();
        walking.examine(&f, &step);
        assert_eq!(walking.examine(&f, &side), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn switching_around_each_placement_flags() {
        let f = facing_down_ahead();
        let mut check = Scaffold::default();
        let flagged = (0..12)
            .filter(|_| {
                check.examine(&f, &Serverbound::SetCarriedItem(1));
                check.examine(&f, &top_click());
                check.examine(&f, &Serverbound::Punch);
                let back = check.examine(&f, &Serverbound::SetCarriedItem(0));
                check.examine(&f, &Serverbound::ClientTickEnd);
                back.is_some()
            })
            .count();
        assert!(flagged >= 3);
        let mut once = Scaffold::default();
        once.examine(&f, &Serverbound::SetCarriedItem(1));
        once.examine(&f, &Serverbound::ClientTickEnd);
        assert_eq!(once.examine(&f, &top_click()), None);
        once.examine(&f, &Serverbound::Punch);
        assert_eq!(once.examine(&f, &Serverbound::ClientTickEnd), None);
    }
}
