//! Creaking: a melee hostile a creaking heart spawns as its protector. It
//! freezes while a player looks at it, wakes (turns active and hunts) when
//! one looks at it from up close, and gives up when nobody it could attack
//! is near. A heart-bound one keeps the heart's position (`home_pos`),
//! takes no damage, calls its heart when a player hits it and dies as soon
//! as the heart stops naming it; its heart tears it down, or, when a player
//! broke the heart, it dies and twitches for 45 ticks first. A heartless
//! (`/summon`ed) one is a plain one-hit hostile that still freezes.

use rand::rngs::StdRng;
use tuff_vanilla_data::entity::MobCategory;
use tuff_vanilla_data::registry::{EntityType, ParticleType, SoundEvent};
use tuff_vanilla_data::{Block, BlockState};

use super::attack::MeleeAttack;
use super::goal::GoalSelector;
use super::goals::{LookAtPlayer, RandomLookAround, RandomStroll};
use super::target::{TargetView, view};
use super::world::region::creaking_heart;
use super::{Damage, MobCore, MobSounds, Species, data};
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{Effect, NearbyPlayer, TickContext};
use tuff_core::world::game_event::GameEvent;
use tuff_core::world::messages::Packets;
use tuff_core::world::{framed, packets};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;

/// The entity data indices a creaking's own fields land at: it is not
/// ageable, so they start right after the shared mob flags byte.
const CAN_MOVE: u8 = data::BABY;
const IS_ACTIVE: u8 = data::BABY + 1;
const IS_TEARING_DOWN: u8 = data::BABY + 2;
const HOME_POS: u8 = data::BABY + 3;

const HOME_RADIUS_SQ: f64 = 1024.0;
pub const HOME_POS_KEY: &str = "home_pos";

const ATTACK_INTERVAL: i32 = 40;
const ACTIVATION_RANGE_SQ: f64 = 144.0;
const INVULNERABILITY_TICKS: i32 = 8;
const TEAR_DOWN_TICKS: i16 = 45;
const MAX_PLAYER_STUCK: i32 = 4;
const GAZE_CONE: f64 = 0.5;
const MAX_SIGHT: f64 = 128.0;
const ATTACK_EVENT: i8 = 4;
const INVULNERABLE_EVENT: i8 = 66;

#[derive(Debug)]
pub struct Creaking {
    home: Option<BlockPos>,
    can_move: bool,
    active: bool,
    tearing_down: bool,
    invulnerable_ticks: i32,
    player_stuck: i32,
    /// Shows the spawn poof on its first tick (a heart just made it).
    poof: bool,
    owed_death: Option<Damage>,
}

impl Default for Creaking {
    fn default() -> Self {
        Self {
            home: None,
            can_move: true,
            active: false,
            tearing_down: false,
            invulnerable_ticks: 0,
            player_stuck: 0,
            poof: false,
            owed_death: None,
        }
    }
}

impl Creaking {
    pub fn home(&self) -> Option<BlockPos> {
        self.home
    }

    pub fn can_move(&self) -> bool {
        self.can_move
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn is_tearing_down(&self) -> bool {
        self.tearing_down
    }

    /// Binds it to the heart at `home`, as a heart's new protector, and
    /// shows the spawn poof on its first tick.
    pub fn bind(&mut self, core: &mut MobCore, home: BlockPos) {
        self.set_home(core, home);
        self.poof = true;
    }

    fn set_home(&mut self, core: &mut MobCore, home: BlockPos) {
        use tuff_vanilla_data::entity::PathType;
        self.home = Some(home);
        for (kind, malus) in [
            (PathType::Damaging, 8.0),
            (PathType::PowderSnow, 8.0),
            (PathType::Lava, 8.0),
            (PathType::Fire, 0.0),
            (PathType::FireInNeighbor, 0.0),
        ] {
            core.set_path_malus(kind, malus);
        }
    }

    /// Its heart went because of `damage` (a player broke it): it dies of
    /// that on its next tick and twitches before it crumbles.
    pub fn owe_death(&mut self, damage: Damage) {
        self.owed_death = Some(damage);
    }

    /// The heart's periodic check: whether a player has had their eyes
    /// inside it for more than four checks in a row.
    pub fn player_stuck_in(&mut self, core: &MobCore, eyes: &[[f64; 3]]) -> bool {
        let range = core.follow_range();
        let bounds = core.bounds();
        let stuck = eyes
            .iter()
            .filter(|eye| core.distance_sq(**eye) <= range * range)
            .any(|eye| contains(bounds, *eye));
        if stuck {
            self.player_stuck += 1;
            return self.player_stuck > MAX_PLAYER_STUCK;
        }
        self.player_stuck = 0;
        false
    }

    fn set_can_move(&mut self, core: &mut MobCore, can_move: bool) {
        if self.can_move != can_move {
            self.can_move = can_move;
            core.changed(CAN_MOVE);
        }
        core.unpushable = !can_move;
        core.no_knockback = !can_move;
    }

    fn set_active(&mut self, core: &mut MobCore, active: bool) {
        if self.active != active {
            self.active = active;
            core.changed(IS_ACTIVE);
        }
    }

    fn activate(&mut self, core: &mut MobCore, player: i32, cx: &mut TickContext) {
        core.target = Some(player);
        game_event(core, cx);
        core.sound(SoundEvent::EntityCreakingActivate, 1.0, 1.0, cx);
        self.set_active(core, true);
    }

    fn deactivate(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        core.target = None;
        game_event(core, cx);
        core.sound(SoundEvent::EntityCreakingDeactivate, 1.0, 1.0, cx);
        self.set_active(core, false);
    }

    /// Whether it may move this tick: frozen while a player it could
    /// attack looks at it, and woken by a look from up close.
    fn check_can_move(&mut self, core: &mut MobCore, cx: &mut TickContext) -> bool {
        let range = core.follow_range();
        let mut players: Vec<NearbyPlayer<'static>> = cx
            .players
            .iter()
            .filter(|p| core.distance_sq(p.position) < range * range)
            .map(|p| NearbyPlayer { name: "", ..*p })
            .collect();
        players.sort_by(|a, b| {
            core.distance_sq(a.position)
                .total_cmp(&core.distance_sq(b.position))
        });
        if players.is_empty() {
            if self.active {
                self.deactivate(core, cx);
            }
            return true;
        }
        let mut potential_target = false;
        for player in &players {
            let target = TargetView::of_player(player);
            if !core.can_attack(&target, cx) || core.allied_to(&target, cx) {
                continue;
            }
            potential_target = true;
            if self.active && disguised(player) {
                continue;
            }
            if !watched_by(core, player, cx) {
                continue;
            }
            if self.active {
                return false;
            }
            if core.distance_sq(player.position) < ACTIVATION_RANGE_SQ {
                self.activate(core, player.id, cx);
                return false;
            }
        }
        if !potential_target && self.active {
            self.deactivate(core, cx);
        }
        true
    }

    /// Keeps its target to a player it can see and attack, picking the
    /// nearest such one while active.
    fn update_target(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let range = core.follow_range();
        let attackable = |core: &MobCore, target: &TargetView| {
            target.is_player() && core.targetable_within(target, range, 1.0, true, cx)
        };
        if let Some(target) = core.target
            && !view(cx, target).is_some_and(|t| attackable(core, &t))
        {
            core.target = None;
        }
        if core.target.is_none() && self.active {
            core.target = cx
                .players
                .iter()
                .map(TargetView::of_player)
                .filter(|t| attackable(core, t))
                .min_by(|a, b| {
                    core.distance_sq(a.position)
                        .total_cmp(&core.distance_sq(b.position))
                })
                .map(|t| t.id);
        }
    }

    fn heart_protects(&self, core: &MobCore, home: BlockPos, cx: &mut TickContext) -> bool {
        let foreign = tuff_world_model::pos::ChunkPos::of_block(home).region()
            != tuff_world_model::pos::ChunkPos::of_position(core.base.position).region();
        let answer = cx
            .state
            .with(|state| creaking_heart::protects(state, home, core.base.uuid, cx.tick));
        if foreign {
            return answer.unwrap_or(true);
        }
        match (cx.block)(home) {
            Some(state) if state.block() != Block::CreakingHeart => false,
            Some(_) => answer.unwrap_or(true),
            None => true,
        }
    }
}

fn contains(bounds: [f64; 6], point: [f64; 3]) -> bool {
    (0..3).all(|i| point[i] >= bounds[i] && point[i] < bounds[i + 3])
}

fn game_event(core: &MobCore, cx: &mut TickContext) {
    cx.effects.push(Effect::GameEvent {
        event: GameEvent::EntityAction,
        position: core.base.position,
        source: Some(core.base.id),
    });
}

fn disguised(player: &NearbyPlayer) -> bool {
    player.armor[3].is_some_and(|head| {
        tuff_datapack::tags::is(
            tuff_vanilla_data::tag::item::key::GAZE_DISGUISE_EQUIPMENT,
            head,
        )
    })
}

/// Whether `player` looks at it: within a wide cone towards its eyes, its
/// feet or its middle, with a clear view there.
pub fn watched_by(core: &MobCore, player: &NearbyPlayer, cx: &TickContext) -> bool {
    let look = tuff_core::world::raycast::view_vector(player.yaw, player.pitch);
    let eye = player.eye();
    let [x, y, z] = core.base.position;
    let eye_y = y + f64::from(core.size.eye_height);
    let level = |pos: BlockPos| (cx.block)(pos).unwrap_or(Block::Stone.default_state());
    for gaze in [
        eye_y,
        y + 0.5 * f64::from(core.size_scale),
        (eye_y + y) / 2.0,
    ] {
        let to = [x, gaze, z];
        let dir = [to[0] - eye[0], to[1] - eye[1], to[2] - eye[2]];
        let distance = dir.iter().map(|c| c * c).sum::<f64>().sqrt();
        if distance < 1.0e-9 {
            continue;
        }
        let dot = (0..3).map(|i| look[i] * dir[i] / distance).sum::<f64>();
        if dot > 1.0 - GAZE_CONE
            && distance <= MAX_SIGHT
            && tuff_core::world::raycast::cast_visual(eye, to, &level).is_none()
        {
            return true;
        }
    }
    false
}

/// Level Particles for a particle type carrying one block state, as many
/// as `count` spread over `spread`.
struct StateParticles {
    particle: ParticleType,
    state: BlockState,
    position: [f64; 3],
    spread: [f32; 3],
    count: i32,
}

impl tuff_core::protocol::Encode for StateParticles {
    const ID: i32 = tuff_vanilla_data::packet::play::Clientbound::LevelParticles.id();

    fn encode(&self, w: &mut tuff_wire::PacketWriter) {
        w.write_var_int(self.particle.id() as i32);
        w.write_var_int(self.state.id() as i32);
        w.write_bool(false);
        w.write_bool(false);
        for c in self.position {
            w.write_double(c);
        }
        for d in self.spread {
            w.write_float(d);
        }
        for _ in 0..3 {
            w.write_float(0.0);
        }
        w.write_var_int(self.count);
        w.write_var_int(0);
    }
}

/// The crumble a creaking leaves when its heart tears it down: pale oak
/// splinters and a few awake heart pieces over its box.
pub fn tear_down_particles(bounds: [f64; 6]) -> Packets {
    let center = [
        (bounds[0] + bounds[3]) / 2.0,
        (bounds[1] + bounds[4]) / 2.0,
        (bounds[2] + bounds[5]) / 2.0,
    ];
    let spread = [0, 1, 2].map(|i| ((bounds[i + 3] - bounds[i]) * 0.3) as f32);
    let heart = Block::CreakingHeart
        .default_state()
        .with("creaking_heart_state", "awake")
        .unwrap_or(Block::CreakingHeart.default_state());
    packets([
        framed(&StateParticles {
            particle: ParticleType::BlockCrumble,
            state: Block::PaleOakWood.default_state(),
            position: center,
            spread,
            count: 100,
        }),
        framed(&StateParticles {
            particle: ParticleType::BlockCrumble,
            state: heart,
            position: center,
            spread,
            count: 10,
        }),
    ])
}

impl Species for Creaking {
    fn create(_rng: &mut StdRng) -> Self {
        Self::default()
    }

    fn load(&mut self, core: &mut MobCore, nbt: &mut Compound) {
        let home = nbt
            .get_int_array(HOME_POS_KEY)
            .and_then(|v| (v.len() == 3).then(|| BlockPos::new(v[0], v[1], v[2])));
        if let Some(home) = home {
            nbt.remove(HOME_POS_KEY);
            self.set_home(core, home);
        }
    }

    fn save(&self, _core: &MobCore, out: &mut Compound) {
        if let Some(home) = self.home {
            out.insert(HOME_POS_KEY, Tag::IntArray(vec![home.x, home.y, home.z]));
        }
    }

    fn configure(core: &mut MobCore) {
        core.make_monster();
        core.xp_reward = 0;
        core.navigation.can_float = true;
        core.navigation.can_pass_doors = true;
        let mut goals = GoalSelector::default();
        goals.add(
            2,
            MeleeAttack::new(1.0, true)
                .with_interval(|_| ATTACK_INTERVAL)
                .with_on_hit(|core| core.signal = 1),
        );
        goals.add(5, RandomStroll::water_avoiding(0.3));
        goals.add(6, LookAtPlayer::new(8.0));
        goals.add(6, RandomLookAround::default());
        core.goals = goals;
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if self.invulnerable_ticks > 0 {
            self.invulnerable_ticks -= 1;
        }
        if std::mem::take(&mut self.poof) {
            core.entity_event(super::event::POOF, cx);
        }
        core.navigation.bound = self.home.map(|home| (home, HOME_RADIUS_SQ));
        if let Some(home) = self.home
            && core.is_alive()
            && self.owed_death.is_none()
            && !self.heart_protects(core, home, cx)
        {
            core.health = 0.0;
            core.changed(data::HEALTH);
        }
        if !core.is_alive() {
            return;
        }
        let could_move = self.can_move;
        let can_move = self.check_can_move(core, cx);
        if can_move != could_move {
            game_event(core, cx);
            if can_move {
                core.sound(SoundEvent::EntityCreakingUnfreeze, 1.0, 1.0, cx);
            } else {
                core.navigation.stop();
                core.body.xxa = 0.0;
                core.body.yya = 0.0;
                core.body.zza = 0.0;
                core.body.speed = 0.0;
                core.sound(SoundEvent::EntityCreakingFreeze, 1.0, 1.0, cx);
            }
        }
        self.set_can_move(core, can_move);
        self.update_target(core, cx);
    }

    fn ai_step(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if std::mem::take(&mut core.signal) != 0 {
            core.entity_event(ATTACK_EVENT, cx);
        }
    }

    fn immobile(&self, _core: &MobCore) -> bool {
        !self.can_move
    }

    fn take_death(&mut self, core: &mut MobCore) -> Option<Damage> {
        let damage = self.owed_death.take()?;
        if !core.is_alive() {
            return None;
        }
        self.tearing_down = true;
        core.changed(IS_TEARING_DOWN);
        Some(damage)
    }

    fn died(&mut self, core: &mut MobCore, _damage: &Damage, cx: &mut TickContext) {
        if self.tearing_down {
            core.sound(SoundEvent::EntityCreakingTwitch, 1.0, 1.0, cx);
        }
    }

    fn tick_death(&mut self, core: &mut MobCore, cx: &mut TickContext) -> Option<bool> {
        if self.home.is_none() || !self.tearing_down {
            return None;
        }
        core.death_time += 1;
        if core.death_time <= TEAR_DOWN_TICKS {
            return Some(false);
        }
        cx.effects
            .push(Effect::ChunkPackets(tear_down_particles(core.bounds())));
        core.sound(SoundEvent::EntityCreakingDeath, 1.0, 1.0, cx);
        Some(true)
    }

    fn before_hurt(&mut self, core: &mut MobCore, damage: &Damage, cx: &mut TickContext) -> bool {
        let Some(home) = self.home else {
            return true;
        };
        if damage.is("bypasses_invulnerability") {
            return true;
        }
        if self.invulnerable_ticks > 0 || !core.is_alive() {
            return false;
        }
        let living_attacker = damage.attacker.is_some_and(|(_, kind)| {
            kind == EntityType::Player || kind.category() != MobCategory::Misc
        });
        if damage.player.is_none() && damage.projectile.is_none() && !living_attacker {
            return false;
        }
        self.invulnerable_ticks = INVULNERABILITY_TICKS;
        core.entity_event(INVULNERABLE_EVENT, cx);
        game_event(core, cx);
        if self.heart_protects(core, home, cx) {
            if damage.player.is_some() {
                cx.state
                    .with(|state| creaking_heart::call(state, home, core.base.uuid));
            }
            let pitch = core.voice_pitch(cx.rng);
            core.sound(SoundEvent::EntityCreakingSway, 1.0, pitch, cx);
        }
        false
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        vec![
            (CAN_MOVE, MetadataValue::Boolean(self.can_move)),
            (IS_ACTIVE, MetadataValue::Boolean(self.active)),
            (IS_TEARING_DOWN, MetadataValue::Boolean(self.tearing_down)),
            (HOME_POS, MetadataValue::OptionalPosition(self.home)),
        ]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: (!self.active).then_some(SoundEvent::EntityCreakingAmbient),
            hurt: Some(if self.home.is_some() {
                SoundEvent::EntityCreakingSway
            } else {
                SoundEvent::EntityHostileHurt
            }),
            death: Some(SoundEvent::EntityCreakingDeath),
            step: Some(SoundEvent::EntityCreakingStep),
            volume: 1.0,
        }
    }
}

/// Registers the creaking's loader. It has no natural spawn rule: only a
/// creaking heart, a summon or a spawn egg creates one.
pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Creaking, super::load::<Creaking>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mob;
    use tuff_core::world::entity::{EntityBase, RegionEntity, Tick};
    use tuff_core::world::region_api::RegionState;
    use tuff_vanilla_data::damage_type;

    fn ground(pos: BlockPos) -> Option<BlockState> {
        Some(if pos.y < 64 {
            Block::GrassBlock.default_state()
        } else {
            Block::Air.default_state()
        })
    }

    fn player(position: [f64; 3], yaw: f32) -> NearbyPlayer<'static> {
        NearbyPlayer {
            id: 7,
            uuid: 7,
            name: "",
            position,
            eye_height: 1.62,
            yaw,
            pitch: 0.0,
            held: [None, None],
            creative: false,
            height: 1.8,
            sneaking: false,
            invisible: false,
            invulnerable: false,
            armor: [None, None, None, None],
            effects: 0,
            movement: [0.0; 3],
            vehicle: None,
            sleeping: None,
            last_hurt_by: None,
            last_hurt_mob: None,
            perchable: false,
            sprinting: false,
        }
    }

    fn creaking() -> Mob<Creaking> {
        let base = EntityBase::new(
            EntityType::Creaking,
            1,
            0xc0ffee,
            [0.5, 64.0, 0.5],
            [0.0; 3],
        );
        let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
        Mob::<Creaking>::new(base, &mut rng)
    }

    fn with_cx(
        block: &dyn Fn(BlockPos) -> Option<BlockState>,
        players: &[NearbyPlayer],
        state: &mut RegionState,
        f: impl FnOnce(&mut TickContext),
    ) {
        let shared = tuff_test_support::bare_shared();
        let light = |_: BlockPos| Some((15, 0));
        let biome = |_: BlockPos| Some("minecraft:plains");
        let mut rng = <StdRng as rand::SeedableRng>::seed_from_u64(1);
        let mut effects = Vec::new();
        let mut budget = 10_000;
        let pois = crate::world::poi::PoiStore::default();
        let lock = tuff_core::world::region_api::RegionStateLock::new(std::mem::take(state));
        let mut cx = TickContext {
            tick: 1,
            dimension: tuff_core::world::dimension::DimensionId::for_tests(0),
            block,
            light: &light,
            sky_darken: 0,
            ambient_light: 0.0,
            monsters_burn: true,
            raining: false,
            biome: &biome,
            container: &|_| None,
            container_info: &|_| Default::default(),
            inhabited_time: &|_| 0,
            min_y: -64,
            max_y: 319,
            players,
            players_in_dimension: players.len(),
            entities: &[],
            entity_grid: None,
            pistons: &[],
            parts: &[],
            shared: &shared,
            rng: &mut rng,
            effects: &mut effects,
            path_budget: &mut budget,
            pois: &pois,
            state: &lock,
        };
        f(&mut cx);
        *state = lock.into_inner();
    }

    fn played(cx: &TickContext, sound: SoundEvent) -> bool {
        cx.effects.iter().any(|e| match e {
            Effect::ChunkPackets(p) | Effect::NearbyPackets { packets: p, .. } => {
                p.iter().any(|frame| {
                    tuff_test_support::decode_sound(frame)
                        .is_some_and(|s| s.sound_event == sound.id())
                })
            }
            _ => false,
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_look_from_up_close_freezes_and_wakes_it() {
        let mut mob = creaking();
        let watcher = player([0.5, 64.0, -5.5], 0.0);
        with_cx(&ground, &[watcher], &mut RegionState::default(), |cx| {
            mob.tick(cx);
            assert!(!mob.species.can_move(), "frozen while watched");
            assert!(mob.species.is_active(), "woken by the close look");
            assert_eq!(mob.core.target, Some(7));
            assert!(played(cx, SoundEvent::EntityCreakingActivate));
            let changed: Vec<u8> = mob
                .take_changed_data()
                .into_iter()
                .map(|(i, _)| i)
                .collect();
            assert!(changed.contains(&CAN_MOVE) && changed.contains(&IS_ACTIVE));
        });
        let away = player([0.5, 64.0, -5.5], 180.0);
        with_cx(&ground, &[away], &mut RegionState::default(), |cx| {
            mob.tick(cx);
            assert!(mob.species.can_move(), "moves once nobody looks");
            assert!(mob.species.is_active(), "still hunting the player");
            assert!(played(cx, SoundEvent::EntityCreakingUnfreeze));
        });
        with_cx(&ground, &[], &mut RegionState::default(), |cx| {
            mob.tick(cx);
            assert!(!mob.species.is_active(), "nobody near: it gives up");
            assert_eq!(mob.core.target, None);
            assert!(played(cx, SoundEvent::EntityCreakingDeactivate));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_distant_look_neither_freezes_nor_wakes_it() {
        let mut mob = creaking();
        let watcher = player([0.5, 64.0, -20.5], 0.0);
        with_cx(&ground, &[watcher], &mut RegionState::default(), |cx| {
            mob.tick(cx);
            assert!(mob.species.can_move());
            assert!(!mob.species.is_active());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn walls_hide_it_but_glass_does_not() {
        let walled = |glass: bool| {
            move |pos: BlockPos| {
                if pos.z == -2 && (64..70).contains(&pos.y) {
                    return Some(if glass {
                        Block::Glass.default_state()
                    } else {
                        Block::Stone.default_state()
                    });
                }
                ground(pos)
            }
        };
        for (glass, frozen) in [(true, true), (false, false)] {
            let mut mob = creaking();
            let watcher = player([0.5, 64.0, -5.5], 0.0);
            let block = walled(glass);
            with_cx(&block, &[watcher], &mut RegionState::default(), |cx| {
                mob.tick(cx);
                assert_eq!(!mob.species.can_move(), frozen, "glass {glass}");
            });
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn panes_and_bars_do_not_hide_it_but_stone_slabs_do() {
        for (block, frozen) in [
            (Block::GlassPane, true),
            (Block::IronBars, true),
            (Block::Stone, false),
        ] {
            let mut mob = creaking();
            let watcher = player([0.5, 64.0, -5.5], 0.0);
            let level = move |pos: BlockPos| {
                if pos.z == -2 && pos.y >= 63 && pos.y <= 66 {
                    Some(block.default_state())
                } else {
                    ground(pos)
                }
            };
            with_cx(&level, &[watcher], &mut RegionState::default(), |cx| {
                mob.tick(cx);
                assert_eq!(!mob.species.can_move(), frozen, "{block:?}");
            });
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_carved_pumpkin_hides_the_gaze_of_whoever_it_hunts() {
        let mut mob = creaking();
        let mut watcher = player([0.5, 64.0, -5.5], 0.0);
        with_cx(&ground, &[watcher], &mut RegionState::default(), |cx| {
            mob.tick(cx);
            assert!(mob.species.is_active());
        });
        watcher.armor[3] = Some(tuff_vanilla_data::Item::CarvedPumpkin);
        with_cx(&ground, &[watcher], &mut RegionState::default(), |cx| {
            mob.tick(cx);
            assert!(mob.species.can_move(), "a disguised gaze does not hold it");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn frozen_it_takes_no_knockback() {
        let mut mob = creaking();
        with_cx(&ground, &[], &mut RegionState::default(), |cx| {
            mob.species.set_can_move(&mut mob.core, false);
            mob.core.knockback(0.4, 1.0, 0.0, cx.rng);
            assert_eq!(mob.core.base.velocity, [0.0; 3]);
            mob.species.set_can_move(&mut mob.core, true);
            mob.core.knockback(0.4, 1.0, 0.0, cx.rng);
            assert!(mob.core.base.velocity[0] < 0.0);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_heartless_creaking_dies_to_one_hit() {
        let mut mob = creaking();
        with_cx(&ground, &[], &mut RegionState::default(), |cx| {
            assert!(mob.hurt_by(&Damage::of(&damage_type::GENERIC), 1.0, cx));
            assert!(!mob.core.is_alive());
        });
    }

    fn player_hit() -> Damage {
        Damage {
            player: Some((7, 7)),
            attacker: Some((7, EntityType::Player)),
            position: Some([0.5, 64.0, -2.5]),
            ..Damage::of(&damage_type::PLAYER_ATTACK)
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_heart_bound_creaking_flinches_instead_of_taking_damage() {
        let home = BlockPos::new(3, 64, 3);
        let heart = move |pos: BlockPos| {
            if pos == home {
                return Some(Block::CreakingHeart.default_state());
            }
            ground(pos)
        };
        let mut mob = creaking();
        mob.species.bind(&mut mob.core, home);
        with_cx(&heart, &[], &mut RegionState::default(), |cx| {
            mob.tick(cx);
            assert!(!mob.hurt_by(&player_hit(), 100.0, cx));
            assert!(mob.core.is_alive());
            assert!(played(cx, SoundEvent::EntityCreakingSway));
            cx.effects.clear();
            assert!(!mob.hurt_by(&player_hit(), 100.0, cx));
            assert!(
                !played(cx, SoundEvent::EntityCreakingSway),
                "a second hit inside the flinch does nothing"
            );
            assert!(!mob.hurt_by(&Damage::of(&damage_type::FALL), 100.0, cx));
            assert!(
                mob.hurt_by(&Damage::of(&damage_type::GENERIC_KILL), 100.0, cx),
                "/kill still works"
            );
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_creaking_whose_heart_is_gone_dies() {
        let mut mob = creaking();
        mob.species.bind(&mut mob.core, BlockPos::new(3, 64, 3));
        with_cx(&ground, &[], &mut RegionState::default(), |cx| {
            mob.tick(cx);
            assert!(!mob.core.is_alive(), "no heart at home: its health drops");
            assert!(
                !cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::KillCredit { .. }))
            );
            let mut removed = false;
            for _ in 0..25 {
                removed |= matches!(mob.tick(cx), Tick::Discard);
            }
            assert!(removed, "it fades like any dead mob");
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_creaking_owed_a_death_twitches_then_crumbles() {
        let mut mob = creaking();
        mob.species.bind(&mut mob.core, BlockPos::new(3, 64, 3));
        with_cx(&ground, &[], &mut RegionState::default(), |cx| {
            mob.species.owe_death(player_hit());
            assert!(matches!(mob.tick(cx), Tick::Alive));
            assert!(!mob.core.is_alive());
            assert!(mob.species.is_tearing_down());
            assert!(played(cx, SoundEvent::EntityCreakingTwitch));
            assert!(
                cx.effects
                    .iter()
                    .any(|e| matches!(e, Effect::KillCredit { .. }))
            );
            let mut ticks = 1;
            loop {
                ticks += 1;
                if matches!(mob.tick(cx), Tick::Discard) {
                    break;
                }
                assert!(ticks < 100);
            }
            assert_eq!(ticks, i32::from(TEAR_DOWN_TICKS) + 1);
            assert!(played(cx, SoundEvent::EntityCreakingDeath));
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_player_stuck_inside_counts_after_four_checks() {
        let mob = creaking();
        let mut species = Creaking::default();
        let eye = [[0.5, 65.0, 0.5]];
        for _ in 0..4 {
            assert!(!species.player_stuck_in(&mob.core, &eye));
        }
        assert!(species.player_stuck_in(&mob.core, &eye));
        assert!(!species.player_stuck_in(&mob.core, &[[5.0, 65.0, 5.0]]));
        assert!(
            !species.player_stuck_in(&mob.core, &eye),
            "the count starts over"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn creaking_data_indices_match_the_dumped_layout() {
        let boolean = MetadataValue::Boolean(false).type_id() as u8;
        let optional_block_pos = MetadataValue::OptionalPosition(None).type_id() as u8;
        let layout = EntityType::Creaking.data_layout();
        assert_eq!(layout.len(), HOME_POS as usize + 1);
        assert_eq!(
            layout[CAN_MOVE as usize..],
            [boolean, boolean, boolean, optional_block_pos]
        );
    }
}
