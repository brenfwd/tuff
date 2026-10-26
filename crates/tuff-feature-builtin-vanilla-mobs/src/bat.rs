//! Bats: they hang resting on cave ceilings by day and drift about at
//! night, with no other behaviour (no combat, no breeding, no own air
//! rules -- see project notes for why).

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::registry::{EntityType, SoundEvent};

use super::physics::block_pos;
use super::{MobCore, MobSounds, Species};
use tuff_core::protocol::play::{self as play, MetadataValue};
use tuff_core::world::entity::{Effect, TickContext};
use tuff_world_model::BlockPos;

/// The byte flags index a bat's own data starts at: bats are not ageable,
/// so unlike most species their own data begins right after the shared
/// mob flags at 16 rather than at [`super::data::SPECIES`] (18) -- there
/// is no baby/age-locked pair to skip. Verified against the dumped layout
/// (17 entries total, the last one a lone byte).
const BAT_FLAGS: u8 = 16;
const FLAG_RESTING: i8 = 0x01;

/// Within this range of a resting bat, a player wakes it.
const WAKE_RANGE: f64 = 4.0;
const LIFTOFF_EVENT: i32 = 1025;
const HEIGHT: f64 = 0.9;

#[derive(Debug)]
pub struct Bat {
    resting: bool,
    /// The block it is flying toward.
    target: Option<BlockPos>,
}

impl Bat {
    fn conductor(cx: &TickContext, pos: BlockPos) -> bool {
        (cx.block)(pos).is_some_and(|s| s.is_redstone_conductor())
    }

    fn player_nearby(core: &MobCore, cx: &TickContext) -> bool {
        cx.players
            .iter()
            .any(|p| core.distance_sq(p.position) < WAKE_RANGE * WAKE_RANGE)
    }

    fn set_resting(&mut self, core: &mut MobCore, resting: bool) {
        if resting != self.resting {
            self.resting = resting;
            core.changed(BAT_FLAGS);
        }
    }

    fn lift_off(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        self.set_resting(core, false);
        if core.base.silent {
            return;
        }
        let packet = tuff_core::world::framed(&play::LevelEvent {
            event: LIFTOFF_EVENT,
            position: block_pos(core.base.position),
            data: 0,
            global: false,
        });
        cx.effects
            .push(Effect::ChunkPackets(tuff_core::world::packets([packet])));
    }

    /// Hangs on until the ceiling goes or a player comes close; in flight,
    /// steers its velocity toward a random nearby air block, now and then
    /// picking another, and may settle under a ceiling it passes.
    fn steer(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        let pos = block_pos(core.base.position);
        let above = BlockPos::new(pos.x, pos.y + 1, pos.z);
        if self.resting {
            if !Self::conductor(cx, above) {
                self.lift_off(core, cx);
                return;
            }
            if cx.rng.gen_range(0..200) == 0 {
                core.head_yaw = cx.rng.gen_range(0..360) as f32;
            }
            if Self::player_nearby(core, cx) {
                self.lift_off(core, cx);
            }
            return;
        }
        if let Some(target) = self.target
            && ((cx.block)(target).is_none_or(|s| !tuff_world_model::chunk::is_air(s))
                || target.y <= cx.min_y)
        {
            self.target = None;
        }
        let [x, y, z] = core.base.position;
        let near = |t: BlockPos| {
            let d = [
                t.x as f64 + 0.5 - x,
                t.y as f64 + 0.5 - y,
                t.z as f64 + 0.5 - z,
            ];
            d.iter().map(|v| v * v).sum::<f64>() < 4.0
        };
        let target = match self.target {
            Some(t) if cx.rng.gen_range(0..30) != 0 && !near(t) => t,
            _ => {
                let mut draw = |n: i32| f64::from(cx.rng.gen_range(0..n));
                let tx = x + draw(7) - draw(7);
                let ty = y + draw(6) - 2.0;
                let tz = z + draw(7) - draw(7);
                BlockPos::new(tx.floor() as i32, ty.floor() as i32, tz.floor() as i32)
            }
        };
        self.target = Some(target);
        let d = [
            target.x as f64 + 0.5 - x,
            target.y as f64 + 0.1 - y,
            target.z as f64 + 0.5 - z,
        ];
        let v = core.base.velocity;
        let pull = [0.5, 0.699_999_988_079_071, 0.5];
        let v =
            [0, 1, 2].map(|i| v[i] + (d[i].signum() * pull[i] - v[i]) * 0.100_000_001_490_116_12);
        core.base.velocity = v;
        let yaw = (v[2].atan2(v[0]) * 57.295_776_367_187_5) as f32 - 90.0;
        core.base.yaw = yaw;
        core.body.zza = 0.5;
        if cx.rng.gen_range(0..100) == 0 && Self::conductor(cx, above) {
            self.set_resting(core, true);
        }
    }
}

impl Species for Bat {
    fn create(_rng: &mut StdRng) -> Self {
        Self {
            resting: true,
            target: None,
        }
    }

    fn configure(core: &mut MobCore) {
        core.goals = super::goal::GoalSelector::default();
        core.unpushable = true;
    }

    fn load(&mut self, _core: &mut MobCore, nbt: &mut tuff_nbt::Compound) {
        if let Some(flags) = tuff_core::world::entity::take_number(nbt, "BatFlags") {
            self.resting = (flags as i8 & FLAG_RESTING) != 0;
        }
    }

    fn save(&self, _core: &MobCore, out: &mut tuff_nbt::Compound) {
        let flags: i8 = if self.resting { FLAG_RESTING } else { 0 };
        out.insert("BatFlags", flags);
    }

    fn metadata(&self, _core: &MobCore) -> Vec<(u8, MetadataValue)> {
        let flags: i8 = if self.resting { FLAG_RESTING } else { 0 };
        vec![(BAT_FLAGS, MetadataValue::Byte(flags))]
    }

    fn sounds(&self, _core: &MobCore) -> MobSounds {
        MobSounds {
            ambient: Some(SoundEvent::EntityBatAmbient),
            hurt: Some(SoundEvent::EntityBatHurt),
            death: Some(SoundEvent::EntityBatDeath),
            step: None,
            volume: 1.0,
        }
    }

    fn tick(&mut self, core: &mut MobCore, cx: &mut TickContext) {
        if core.is_alive() && !core.no_ai {
            self.steer(core, cx);
        }
    }

    fn ai_step(&mut self, core: &mut MobCore, _cx: &mut TickContext) {
        if self.resting {
            core.base.velocity = [0.0; 3];
            let y = core.base.position[1];
            core.base.position[1] = y.floor() + 1.0 - HEIGHT;
        } else {
            core.base.velocity[1] *= 0.6;
        }
    }

    fn before_hurt(
        &mut self,
        core: &mut MobCore,
        damage: &super::Damage,
        _cx: &mut TickContext,
    ) -> bool {
        if damage.kind == &tuff_vanilla_data::damage_type::FALL {
            return false;
        }
        self.set_resting(core, false);
        true
    }

    fn remove_when_far_away(&self, _core: &MobCore) -> bool {
        true
    }
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    registry.register(EntityType::Bat, super::load::<Bat>);
    registry.register_spawn_rule(EntityType::Bat, super::spawning::bat_rule);
}

#[cfg(test)]
mod tests {
    use tuff_vanilla_data::Block;

    use super::super::Damage;
    use super::*;
    use tuff_core::protocol::play::MetadataValue;
    use tuff_core::world::entity::{EntityBase, NearbyPlayer};

    /// A tick context like `super::super::tests::with_context`'s, but with
    /// caller-chosen block lookup and nearby players: that helper's fields
    /// are borrowed for its own call scope, so a test cannot reassign
    /// `cx.block`/`cx.players` afterwards to something scoped to its own
    /// closure body.
    fn with_context_over(
        block: &dyn Fn(BlockPos) -> Option<tuff_vanilla_data::BlockState>,
        players: &[NearbyPlayer],
        f: impl FnOnce(&mut MobCore, &mut TickContext),
    ) {
        let shared = tuff_test_support::bare_shared();
        let light = |_: BlockPos| Some((15, 0));
        let biome = |_: BlockPos| Some("minecraft:plains");
        let mut rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(1);
        let mut effects = Vec::new();
        let mut budget = 10_000;
        let pois = crate::world::poi::PoiStore::default();
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
            players_in_dimension: 0,
            entities: &[],
            entity_grid: None,
            pistons: &[],
            parts: &[],
            shared: &shared,
            rng: &mut rng,
            effects: &mut effects,
            path_budget: &mut budget,
            pois: &pois,
            state: &tuff_core::world::region_api::RegionStateLock::default(),
        };
        let base = EntityBase::new(EntityType::Bat, 1, 1, [0.5, 64.0, 0.5], [0.0; 3]);
        let mut core = MobCore::new(base);
        f(&mut core, &mut cx);
    }

    #[tuff_core::watchdog]
    #[test]
    fn bat_data_index_matches_the_dumped_layout() {
        let layout = EntityType::Bat.data_layout();
        assert_eq!(layout.len(), BAT_FLAGS as usize + 1);
        assert_eq!(
            layout[BAT_FLAGS as usize],
            MetadataValue::Byte(0).type_id() as u8
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_awake_bat_flies_about_instead_of_sitting_still() {
        let open = |_: BlockPos| Some(Block::Air.default_state());
        with_context_over(&open, &[], |core, cx| {
            Bat::configure(core);
            let mut bat = Bat::create(cx.rng);
            assert!(bat.resting);
            bat.tick(core, cx);
            assert!(!bat.resting, "with no ceiling it lets go at once");
            let mut path = 0.0;
            for _ in 0..40 {
                bat.tick(core, cx);
                for axis in 0..3 {
                    core.base.position[axis] += core.base.velocity[axis];
                }
                path += core.base.velocity.iter().map(|v| v * v).sum::<f64>().sqrt();
                bat.ai_step(core, cx);
            }
            assert!(path > 4.0, "it flew {path} blocks in two seconds");
            assert!(bat.target.is_some());
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_solid_ceiling_and_no_player_keeps_a_bat_resting() {
        let ceiling = |pos: BlockPos| {
            Some(if pos.y == 65 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        with_context_over(&ceiling, &[], |core, cx| {
            Bat::configure(core);
            let mut bat = Bat::create(cx.rng);
            bat.tick(core, cx);
            assert!(bat.resting);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_nearby_player_wakes_a_resting_bat() {
        let ceiling = |pos: BlockPos| {
            Some(if pos.y == 65 {
                Block::Stone.default_state()
            } else {
                Block::Air.default_state()
            })
        };
        let player = NearbyPlayer {
            id: 1,
            uuid: 1,
            name: "",
            position: [1.0, 64.0, 0.5],
            eye_height: 1.62,
            yaw: 0.0,
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
        };
        with_context_over(&ceiling, &[player], |core, cx| {
            Bat::configure(core);
            let mut bat = Bat::create(cx.rng);
            bat.tick(core, cx);
            assert!(!bat.resting);
        });
    }

    #[tuff_core::watchdog]
    #[test]
    fn hurting_a_resting_bat_wakes_it() {
        super::super::tests::with_context(|core, cx| {
            Bat::configure(core);
            let mut bat = Bat::create(cx.rng);
            assert!(bat.before_hurt(
                core,
                &Damage::of(&tuff_vanilla_data::damage_type::GENERIC),
                cx
            ));
            assert!(!bat.resting);
        });
    }
}
