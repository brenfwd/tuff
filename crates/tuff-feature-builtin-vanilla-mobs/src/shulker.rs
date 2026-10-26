//! Shulkers: they cling to a block face without moving, peek out of their
//! shell now and then, open up to fire homing bullets at players in range,
//! armor up while closed, teleport away when hurt badly or when their block
//! goes, and split in two when their own bullets hit them while open.

use rand::Rng;
use tuff_vanilla_data::block::{Axis, Facing};
use tuff_vanilla_data::item::ModifierOperation;
use tuff_vanilla_data::registry::{Attribute, EntityType, SoundEvent};

use super::goal::{Flags, Goal, GoalSelector, reduced_delay};
use super::goals::{LookAtPlayer, RandomLookAround};
use super::physics::{Aabb, block_pos};
use super::target::{HurtByTarget, NearestAttackableEntity, NearestAttackablePlayer};
use super::{Damage, Mob, MobCore, MobSounds, Species, data};
use crate::shulker_bullet::{self, Aim, ShulkerBullet};
use tuff_core::protocol::play::{self, MetadataValue};
use tuff_core::world::entity::{Culprit, Effect, NearbyPlayer, TickContext, take_number};
use tuff_core::world::game_event::GameEvent;
use tuff_core::world::physics;
use tuff_core::world::player::Difficulty;
use tuff_core::world::projectile::Owner;
use tuff_core::world::{framed, packets};
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;

const DATA_ATTACH_FACE: u8 = data::BABY;
const DATA_PEEK: u8 = data::BABY + 1;
const DATA_COLOR: u8 = data::BABY + 2;
const NO_COLOR: u8 = 16;
const XP: i32 = 5;
const PEEK_STEP: f32 = 0.05;
const TELEPORT_RANGE: i32 = 8;
const TELEPORT_ATTEMPTS: u32 = 5;
const SHOOT_RANGE_SQ: f64 = 400.0;
const COVERED_ARMOR: &str = "minecraft:covered";
const COVERED_ARMOR_AMOUNT: f64 = 20.0;
const TELEPORT_PARTICLES: i32 = 2016;
const NEARBY_SHULKERS: f64 = 8.0;
const SHULKER_LIMIT: f32 = 5.0;
const SEARCH_ACROSS: f64 = 4.0;
const DEFLATE: f64 = 1.0e-6;

/// A peek request from a goal: `0` asks nothing, `n + 1` asks for raw peek
/// `n`. Lives in [`MobCore::signal`].
fn request_peek(mob: &mut MobCore, amount: i32) {
    mob.signal = amount + 1;
}

/// How far the shell has slid open for a peek of `amount` (0 to 1).
pub fn physical_peek(amount: f32) -> f32 {
    0.5 - (((0.5 + amount) * std::f32::consts::PI).sin() * 0.5)
}

/// A unit box resting on its bottom centre, stretched `progress` blocks
/// toward `direction`.
pub fn progress_box(direction: Facing, progress: f32) -> Aabb {
    let mut aabb = [-0.5, 0.0, -0.5, 0.5, 1.0, 0.5];
    let (x, y, z) = direction.step();
    for (axis, step) in [x, y, z].into_iter().enumerate() {
        let reach = f64::from(step) * f64::from(progress);
        if reach > 0.0 {
            aabb[axis + 3] += reach;
        } else {
            aabb[axis] += reach;
        }
    }
    aabb
}

/// What the shell sweeps while its physical peek goes from `from` to `to`.
fn progress_delta_box(direction: Facing, from: f32, to: f32, position: [f64; 3]) -> Aabb {
    let (high, low) = (f64::from(from.max(to)), f64::from(from.min(to)));
    let mut aabb = [-0.5, 0.0, -0.5, 0.5, 1.0, 0.5];
    let (x, y, z) = direction.step();
    for (axis, step) in [x, y, z].into_iter().enumerate() {
        match step {
            1 => {
                aabb[axis + 3] += high;
                aabb[axis] += 1.0 + low;
            }
            -1 => {
                aabb[axis] -= high;
                aabb[axis + 3] -= 1.0 + low;
            }
            _ => {}
        }
    }
    moved(aabb, position)
}

fn moved(aabb: Aabb, by: [f64; 3]) -> Aabb {
    [
        aabb[0] + by[0],
        aabb[1] + by[1],
        aabb[2] + by[2],
        aabb[3] + by[0],
        aabb[4] + by[1],
        aabb[5] + by[2],
    ]
}

fn offset(pos: BlockPos, face: Facing) -> BlockPos {
    let (x, y, z) = face.step();
    BlockPos::new(pos.x + x, pos.y + y, pos.z + z)
}

fn is_air(state: Option<tuff_vanilla_data::BlockState>) -> bool {
    state.is_some_and(tuff_world_model::chunk::is_air)
}

/// Whether other shulkers' shells take up any of `area`.
fn shulker_in(area: Aabb, own: i32, cx: &TickContext) -> bool {
    cx.entities.iter().any(|e| {
        e.id != own
            && e.kind == EntityType::Shulker
            && e.living.is_some_and(|l| l.alive)
            && physics::intersects(area, e.bounds)
    })
}

/// Whether a shulker could cling to `face` while sitting at `pos`: its own
/// cell is clear, the face it holds on to is full, and it would have room
/// to open all the way.
pub fn can_stay_at(mob: &MobCore, pos: BlockPos, face: Facing, cx: &TickContext) -> bool {
    let here = (cx.block)(pos);
    let own_piston = here.is_some_and(|s| s.block() == tuff_vanilla_data::Block::MovingPiston)
        && pos == block_pos(mob.base.position);
    if !is_air(here) && !own_piston {
        return false;
    }
    let opposite = face.opposite();
    if !(cx.block)(offset(pos, face)).is_some_and(|s| s.is_collision_face_full(opposite)) {
        return false;
    }
    let bottom = [
        f64::from(pos.x) + 0.5,
        f64::from(pos.y),
        f64::from(pos.z) + 0.5,
    ];
    let open = physics::inflate(moved(progress_box(opposite, 1.0), bottom), [-DEFLATE; 3]);
    physics::no_collision(open, cx.block) && !shulker_in(open, mob.base.id, cx)
}

/// The first face, in the game's direction order, a shulker at `pos` could
/// cling to.
pub fn attachable_face(mob: &MobCore, pos: BlockPos, cx: &TickContext) -> Option<Facing> {
    const ORDER: [Facing; 6] = [
        Facing::Down,
        Facing::Up,
        Facing::North,
        Facing::South,
        Facing::West,
        Facing::East,
    ];
    ORDER
        .into_iter()
        .find(|face| can_stay_at(mob, pos, *face, cx))
}

fn attach_axis(mob: &MobCore) -> Axis {
    mob.attached.unwrap_or(Facing::Down).axis()
}

/// Players it may pick: within its follow range, but only four blocks out
/// along the axis it clings on.
fn in_search_area(mob: &MobCore, player: &NearbyPlayer) -> bool {
    let range = mob.follow_range();
    let mut grow = [range; 3];
    grow[match attach_axis(mob) {
        Axis::X => 0,
        Axis::Y => 1,
        Axis::Z => 2,
    }] = SEARCH_ACROSS;
    physics::intersects(physics::inflate(mob.bounds(), grow), player.bounds())
}

fn search_grow(mob: &MobCore) -> [f64; 3] {
    let range = mob.follow_range();
    let mut grow = [range; 3];
    grow[match attach_axis(mob) {
        Axis::X => 0,
        Axis::Y => 1,
        Axis::Z => 2,
    }] = SEARCH_ACROSS;
    grow
}

fn on_a_team(mob: &MobCore, cx: &TickContext) -> bool {
    let teams = cx.shared.scoreboard.teams();
    teams
        .team_of(crate::target::team_member(cx, mob.base.uuid, None))
        .is_some()
}

/// Now and then, with nothing to fight, it opens up a little for one to
/// three seconds.
#[derive(Debug, Default)]
struct Peek {
    time: i32,
}

impl Goal for Peek {
    fn flags(&self) -> Flags {
        Flags::NONE
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        mob.target.is_none()
            && cx.rng.gen_range(0..reduced_delay(40)) == 0
            && can_stay_at(
                mob,
                block_pos(mob.base.position),
                mob.attached.unwrap_or(Facing::Down),
                cx,
            )
    }

    fn can_continue(&mut self, mob: &mut MobCore, _cx: &mut TickContext) -> bool {
        mob.target.is_none() && self.time > 0
    }

    fn start(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        self.time = self.adjusted_delay(20 * (1 + cx.rng.gen_range(0..3)));
        request_peek(mob, 30);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        if mob.target.is_none() {
            request_peek(mob, 0);
        }
    }

    fn tick(&mut self, _mob: &mut MobCore, _cx: &mut TickContext) {
        self.time -= 1;
    }
}

/// Opens wide and fires a bullet every one to five and a half seconds at a
/// target within 20 blocks; farther away it gives the target up.
#[derive(Debug, Default)]
struct Attack {
    time: i32,
}

impl Goal for Attack {
    fn flags(&self) -> Flags {
        Flags::MOVE | Flags::LOOK
    }

    fn can_use(&mut self, mob: &mut MobCore, cx: &mut TickContext) -> bool {
        *cx.shared.difficulty.borrow() != Difficulty::Peaceful
            && mob.target_view(cx).is_some_and(|t| t.alive)
    }

    fn start(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        self.time = 20;
        request_peek(mob, 100);
    }

    fn stop(&mut self, mob: &mut MobCore, _cx: &mut TickContext) {
        request_peek(mob, 0);
    }

    fn every_tick(&self) -> bool {
        true
    }

    fn tick(&mut self, mob: &mut MobCore, cx: &mut TickContext) {
        if *cx.shared.difficulty.borrow() == Difficulty::Peaceful {
            return;
        }
        self.time -= 1;
        let Some(target) = mob.target_view(cx) else {
            return;
        };
        mob.look.set_look_at(target.eye, 180.0, 180.0);
        if mob.distance_sq(target.position) >= SHOOT_RANGE_SQ {
            mob.target = None;
            return;
        }
        if self.time > 0 {
            return;
        }
        self.time = 20 + cx.rng.gen_range(0..10) * 20 / 2;
        let aim = Aim {
            position: target.position,
            height: target.bounds[4] - target.bounds[1],
        };
        let owner = Owner::of(Culprit::entity(&mob.base), None);
        let id = cx.shared.next_entity_id();
        let bullet = ShulkerBullet::new(
            id,
            mob.bounds(),
            owner,
            (target.uuid, aim),
            Some(attach_axis(mob)),
            cx,
        );
        cx.effects.push(Effect::Spawn(Box::new(bullet)));
        let pitch = (cx.rng.r#gen::<f32>() - cx.rng.r#gen::<f32>()) * 0.2 + 1.0;
        mob.sound(SoundEvent::EntityShulkerShoot, 2.0, pitch, cx);
    }
}

#[derive(Debug)]
pub struct Shulker {
    face: Facing,
    /// The raw peek (0 closed to 100 wide open) clients are told.
    peek: i32,
    current: f32,
    color: u8,
    block: Option<BlockPos>,
    box_pushed: bool,
}

impl Shulker {
    pub fn color(&self) -> Option<u8> {
        (self.color < NO_COLOR).then_some(self.color)
    }

    pub fn set_color(&mut self, color: Option<u8>) {
        self.color = color.filter(|c| *c < NO_COLOR).unwrap_or(NO_COLOR);
    }

    pub fn attach_face(&self) -> Facing {
        self.face
    }

    pub fn peek(&self) -> i32 {
        self.peek
    }

    fn closed(&self) -> bool {
        self.peek == 0
    }

    fn set_face(&mut self, core: &mut MobCore, face: Facing) {
        if self.face != face {
            self.face = face;
            core.changed(DATA_ATTACH_FACE);
        }
        core.attached = Some(face);
        self.refresh_shape(core);
    }

    /// Sets the peek clients see without the shell's sounds (a teleport or
    /// a move snapping it shut).
    fn reset_peek(&mut self, core: &mut MobCore) {
        if self.peek != 0 {
            self.peek = 0;
            core.changed(DATA_PEEK);
        }
    }

    /// A goal opening or closing it: the shell sounds, and closed it wears
    /// its armor bonus.
    fn set_raw_peek(&mut self, core: &mut MobCore, amount: i32, cx: &mut TickContext) {
        let armor = (amount == 0).then_some((COVERED_ARMOR_AMOUNT, ModifierOperation::AddValue));
        core.set_permanent_modifier(Attribute::Armor, COVERED_ARMOR, armor);
        let (sound, event) = if amount == 0 {
            (SoundEvent::EntityShulkerClose, GameEvent::ContainerClose)
        } else {
            (SoundEvent::EntityShulkerOpen, GameEvent::ContainerOpen)
        };
        core.sound(sound, 1.0, 1.0, cx);
        cx.effects.push(Effect::GameEvent {
            event,
            position: core.base.position,
            source: Some(core.base.id),
        });
        if self.peek != amount {
            self.peek = amount;
            core.changed(DATA_PEEK);
        }
    }

    fn refresh_shape(&self, core: &mut MobCore) {
        core.box_offset = Some(progress_box(
            self.face.opposite(),
            physical_peek(self.current),
        ));
        let size = core.base.kind.size();
        let stretch = if self.face == Facing::Down && self.current > 0.0 {
            1.0 + self.current
        } else {
            1.0
        };
        core.size.height = size.height * stretch;
        core.size.eye_height = size.eye_height * stretch;
    }

    /// Slides the shell a step toward the raw peek.
    fn update_peek(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let wanted = self.peek as f32 * 0.01;
        if self.current == wanted {
            return;
        }
        let before = physical_peek(self.current);
        self.current = if self.current > wanted {
            (self.current - PEEK_STEP).clamp(wanted, 1.0)
        } else {
            (self.current + PEEK_STEP).clamp(0.0, wanted)
        };
        self.refresh_shape(core);
        let push = physical_peek(self.current) - before;
        if push > 0.0 && core.base.vehicle.is_none() {
            let direction = self.face.opposite();
            cx.effects.push(Effect::ShellPush {
                area: progress_delta_box(
                    direction,
                    before,
                    physical_peek(self.current),
                    core.base.position,
                ),
                direction,
                amount: f64::from(push),
            });
        }
    }

    /// Puts it in the middle of its block, as it always sits; a move to
    /// another block snaps it shut.
    fn snap(&mut self, core: &mut MobCore) {
        if core.base.vehicle.is_some() {
            return;
        }
        let [x, y, z] = core.base.position;
        core.base.position = [x.floor() + 0.5, (y + 0.5).floor(), z.floor() + 0.5];
        let pos = block_pos(core.base.position);
        if self.block.is_some_and(|old| old != pos) {
            self.reset_peek(core);
            core.signal = 0;
        }
        self.block = Some(pos);
    }

    /// Jumps to a random free spot within eight blocks that it can cling
    /// to.
    pub fn teleport_somewhere(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        if core.no_ai || !core.is_alive() {
            return false;
        }
        let old = block_pos(core.base.position);
        for _ in 0..TELEPORT_ATTEMPTS {
            let mut roll = || cx.rng.gen_range(-TELEPORT_RANGE..=TELEPORT_RANGE);
            let (dx, dy, dz) = (roll(), roll(), roll());
            let target = BlockPos::new(old.x + dx, old.y + dy, old.z + dz);
            let in_border = cx
                .shared
                .dimension(cx.dimension)
                .border()
                .contains_block(target);
            if target.y <= cx.min_y || !in_border || !is_air((cx.block)(target)) {
                continue;
            }
            let cell = [
                f64::from(target.x),
                f64::from(target.y),
                f64::from(target.z),
                f64::from(target.x) + 1.0,
                f64::from(target.y) + 1.0,
                f64::from(target.z) + 1.0,
            ];
            let cell = physics::inflate(cell, [-DEFLATE; 3]);
            if !physics::no_collision(cell, cx.block) || shulker_in(cell, core.base.id, cx) {
                continue;
            }
            let Some(face) = attachable_face(core, target, cx) else {
                continue;
            };
            let shunned = (cx.block)(offset(target, face)).is_some_and(|s| {
                tuff_datapack::tags::is(
                    tuff_vanilla_data::tag::block::key::SHULKER_DOES_NOT_TELEPORT_TO,
                    s.block(),
                )
            });
            if shunned {
                continue;
            }
            if core.base.vehicle.is_some() {
                cx.effects.push(Effect::StopRiding);
            }
            self.set_face(core, face);
            core.sound(SoundEvent::EntityShulkerTeleport, 1.0, 1.0, cx);
            core.base.position = [
                f64::from(target.x) + 0.5,
                f64::from(target.y),
                f64::from(target.z) + 0.5,
            ];
            self.block = Some(target);
            cx.effects.push(Effect::GameEvent {
                event: GameEvent::Teleport,
                position: [
                    f64::from(old.x) + 0.5,
                    f64::from(old.y) + 0.5,
                    f64::from(old.z) + 0.5,
                ],
                source: Some(core.base.id),
            });
            let packed = ((dx + TELEPORT_RANGE) & 255) << 16
                | ((dy + TELEPORT_RANGE) & 255) << 8
                | ((dz + TELEPORT_RANGE) & 255);
            let event = framed(&play::LevelEvent {
                event: TELEPORT_PARTICLES,
                position: old,
                data: packed,
                global: false,
            });
            cx.effects.push(Effect::ChunkPackets(packets([event])));
            self.reset_peek(core);
            core.signal = 0;
            core.target = None;
            return true;
        }
        false
    }

    /// Its own kind's bullet hit it while open: it jumps away and, unless
    /// crowded by other shulkers, leaves a copy of itself behind.
    fn hit_by_bullet(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let old_position = core.base.position;
        let old_box = core.bounds();
        if self.closed() || !self.teleport_somewhere(core, cx) {
            return;
        }
        let area = physics::inflate(old_box, [NEARBY_SHULKERS; 3]);
        let others = cx
            .entities
            .iter()
            .filter(|e| {
                e.id != core.base.id
                    && e.kind == EntityType::Shulker
                    && e.living.is_some_and(|l| l.alive)
                    && physics::intersects(area, e.bounds)
            })
            .count();
        let failure = others as f32 / SHULKER_LIMIT;
        if cx.rng.r#gen::<f32>() < failure {
            return;
        }
        let id = cx.shared.next_entity_id();
        let Some(mut baby) =
            cx.shared
                .entity_types
                .create(EntityType::Shulker, id, old_position, 0.0)
        else {
            return;
        };
        if let Some(mob) = baby.downcast_mut::<Mob<Shulker>>() {
            mob.species.set_color(self.color());
        }
        cx.effects.push(Effect::Spawn(baby));
    }

    /// Clings to another face of its block, or teleports, once the one it
    /// holds is gone.
    fn check_attachment(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if core.base.vehicle.is_some() {
            if self.face != Facing::Down {
                self.set_face(core, Facing::Down);
            }
            return;
        }
        let pos = block_pos(core.base.position);
        if can_stay_at(core, pos, self.face, cx) {
            return;
        }
        match attachable_face(core, pos, cx) {
            Some(face) => self.set_face(core, face),
            None => {
                self.teleport_somewhere(core, cx);
            }
        }
    }
}

impl Species for Shulker {
    fn track_shape(&self, _core: &MobCore) -> Option<tuff_core::world::messages::Shape> {
        let target = physical_peek(self.peek as f32 * 0.01);
        Some(tuff_core::world::messages::Shape {
            non_solid: false,
            offset: Some(progress_box(self.face.opposite(), self.current.max(target))),
        })
    }

    fn create(_rng: &mut rand::rngs::StdRng) -> Self {
        Self {
            face: Facing::Down,
            peek: 0,
            current: 0.0,
            color: NO_COLOR,
            block: None,
            box_pushed: false,
        }
    }

    fn configure(core: &mut MobCore) {
        core.hostile = true;
        core.xp_reward = XP;
        core.no_physics = true;
        core.unpushable = true;
        core.no_knockback = true;
        core.attached = Some(Facing::Down);
        core.box_offset = Some(progress_box(Facing::Up, 0.0));
        core.max_head_x_rot = 180.0;
        core.max_head_y_rot = 180.0;
        let mut goals = GoalSelector::default();
        goals.add(1, LookAtPlayer::new(8.0));
        goals.add(4, Attack::default());
        goals.add(7, Peek::default());
        goals.add(8, RandomLookAround::default());
        goals.add(
            1,
            HurtByTarget::new(&[EntityType::Shulker]).alerting(&[EntityType::Shulker]),
        );
        goals.add(
            2,
            NearestAttackablePlayer::with_interval(10, true, Some(in_search_area)),
        );
        goals.add(
            3,
            NearestAttackableEntity::any_living(true)
                .only(|e| e.living.is_some_and(|l| l.enemy))
                .when(on_a_team)
                .searching(search_grow),
        );
        core.goals = goals;
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        let face = take_number(nbt, "AttachFace")
            .and_then(|id| shulker_bullet::facing_from_id(id as i64))
            .unwrap_or(Facing::Down);
        self.peek = take_number(nbt, "Peek").map_or(0, |p| i32::from(p as i8));
        self.color = take_number(nbt, "Color").map_or(NO_COLOR, |c| c as i8 as u8);
        self.face = face;
        self.set_face(core, face);
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        out.insert("AttachFace", self.face.id() as i8);
        out.insert("Peek", self.peek as i8);
        out.insert("Color", self.color as i8);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (
                DATA_ATTACH_FACE,
                MetadataValue::Direction(self.face.id() as i32),
            ),
            (DATA_PEEK, MetadataValue::Byte(self.peek as i8)),
            (DATA_COLOR, MetadataValue::Byte(self.color as i8)),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: (!self.closed()).then_some(SoundEvent::EntityShulkerAmbient),
            hurt: Some(if self.closed() {
                SoundEvent::EntityShulkerHurtClosed
            } else {
                SoundEvent::EntityShulkerHurt
            }),
            death: Some(SoundEvent::EntityShulkerDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn tick(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        core.base.velocity = [0.0; 3];
        self.snap(core);
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let request = std::mem::take(&mut core.signal);
        if request > 0 {
            self.set_raw_peek(core, request - 1, cx);
        }
        if std::mem::take(&mut self.box_pushed) {
            self.teleport_somewhere(core, cx);
        }
        self.check_attachment(core, cx);
        self.update_peek(core, cx);
    }

    fn finalize_spawn(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        core.base.yaw = 0.0;
        core.head_yaw = 0.0;
        core.body_yaw = 0.0;
    }

    fn before_hurt(&mut self, _core: &mut MobCore, damage: &Damage, _cx: &mut TickContext) -> bool {
        let arrow = matches!(
            damage.projectile,
            Some(EntityType::Arrow | EntityType::SpectralArrow | EntityType::Trident)
        );
        !(self.closed() && arrow)
    }

    fn after_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) {
        if core.health < core.max_health() * 0.5 && cx.rng.gen_range(0..4) == 0 {
            self.teleport_somewhere(core, cx);
            return;
        }
        if damage.is("is_projectile") && damage.projectile == Some(EntityType::ShulkerBullet) {
            self.hit_by_bullet(core, cx);
        }
    }

    fn knockback(
        &mut self,
        _core: &mut MobCore,
        _push: &crate::Knockback,
        _damage: &Damage,
        _cx: &mut TickContext,
    ) -> bool {
        true
    }

    fn remove_when_far_away(&self, _core: &MobCore) -> bool {
        false
    }

    fn shulker_box_pushed(&mut self, _core: &mut MobCore) {
        self.box_pushed = true;
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Shulker, super::load::<Shulker>);
    registry.register(EntityType::ShulkerBullet, crate::shulker_bullet::load);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn a_peeking_shulker_reports_its_grown_box() {
        let mut shulker = Shulker::create(&mut rand::SeedableRng::seed_from_u64(1));
        shulker.peek = 100;
        let core = MobCore::new(tuff_core::world::entity::EntityBase::new(
            EntityType::Shulker,
            1,
            1,
            [0.0; 3],
            [0.0; 3],
        ));
        let shape = shulker.track_shape(&core).unwrap();
        let offset = shape.offset.unwrap();
        assert!((offset[4] - 2.0).abs() < 1e-6);
        assert!(!shape.non_solid);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_shell_opens_toward_the_side_away_from_its_block() {
        assert_eq!(physical_peek(0.0), 0.0);
        assert!((physical_peek(1.0) - 1.0).abs() < 1e-6);
        assert!((physical_peek(0.3) - 0.20610738).abs() < 1e-5);
        assert_eq!(
            progress_box(Facing::Up, 0.5),
            [-0.5, 0.0, -0.5, 0.5, 1.5, 0.5]
        );
        assert_eq!(
            progress_box(Facing::Down, 1.0),
            [-0.5, -1.0, -0.5, 0.5, 1.0, 0.5]
        );
        assert_eq!(
            progress_box(Facing::West, 0.25),
            [-0.75, 0.0, -0.5, 0.5, 1.0, 0.5]
        );
    }
}

#[cfg(test)]
mod push_tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_swept_box_spans_the_lid_travel() {
        let aabb = progress_delta_box(Facing::Up, 0.2, 0.5, [0.0; 3]);
        assert!((aabb[1] - 1.2).abs() < 1e-6 && (aabb[4] - 1.5).abs() < 1e-6);
        let aabb = progress_delta_box(Facing::Down, 0.2, 0.5, [0.0; 3]);
        assert!((aabb[1] + 0.5).abs() < 1e-6 && (aabb[4] + 0.2).abs() < 1e-6);
    }
}
