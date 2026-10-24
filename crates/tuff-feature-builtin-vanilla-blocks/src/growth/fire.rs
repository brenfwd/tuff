//! Fire: aging, burning out, burning the blocks around it away, spreading
//! to flammable blocks, going out in the rain, lighting TNT, and lava
//! setting fire to what it touches.

pub use tuff_core::world::contact::fire_contact;
use tuff_vanilla_data::block::Facing;
use tuff_vanilla_data::{Block, BlockState};

use super::{Behaviours, GrowthLevel, GrowthLevelExt, above, below, offset};
use tuff_core::world::redstone::{UPDATE_ALL, UPDATE_NONE};
use tuff_vanilla_common::placement::{self, step};
use tuff_world_model::BlockPos;
use tuff_world_model::chunk::is_air;

pub fn register(behaviours: &mut Behaviours) {
    behaviours.register_scheduled(Block::Fire, fire_tick);
    behaviours.register_placed(Block::Fire, fire_placed);
}

/// How long until a fire ticks again.
pub fn tick_delay(level: &mut dyn GrowthLevel) -> u32 {
    30 + level.random_int(10) as u32
}

fn fire_placed(level: &mut dyn GrowthLevel, pos: BlockPos, _state: BlockState) {
    if level.light_portal(pos) {
        return;
    }
    let delay = tick_delay(level);
    level.schedule_tick(pos, Block::Fire, delay);
}

/// Whether fire spreads near `pos`: the game rule's radius around a player,
/// or anywhere when it is -1.
pub fn can_spread_around(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    match level.rule_int("fire_spread_radius_around_player") {
        -1 => true,
        radius => level.player_within(pos, radius),
    }
}

/// Neighbours a fire tries to consume, in order, with the base odds divisor
/// for each (vertical neighbours burn faster).
const CONSUME: [(Facing, i32); 6] = [
    (Facing::East, 300),
    (Facing::West, 300),
    (Facing::Down, 250),
    (Facing::Up, 250),
    (Facing::North, 300),
    (Facing::South, 300),
];

const REACH: usize = 3 * 3 * 6 - 1;

/// Cells a fire may jump to, in scan order, with the roll ceiling for each:
/// jumping further up is harder.
const REACH_CELLS: [(i8, i8, i8, i16); REACH] = {
    let mut cells = [(0, 0, 0, 0); REACH];
    let mut n = 0;
    let mut i = 0;
    while i < 3 * 3 * 6 {
        let (x, z, y) = (
            (i / 18) as i8 - 1,
            ((i / 6) % 3) as i8 - 1,
            (i % 6) as i8 - 1,
        );
        if x != 0 || y != 0 || z != 0 {
            let ceiling = if y > 1 { y as i16 * 100 } else { 100 };
            cells[n] = (x, y, z, ceiling);
            n += 1;
        }
        i += 1;
    }
    cells
};

fn wet_around(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    std::iter::once(pos)
        .chain(super::HORIZONTAL.map(|side| step(pos, side)))
        .any(|cell| level.is_raining_at(cell))
}

fn douse(level: &mut dyn GrowthLevel, pos: BlockPos) {
    let leftover = placement::remains(level.state(pos));
    level.set(pos, leftover, UPDATE_ALL);
}

fn kindle(level: &mut dyn GrowthLevel, pos: BlockPos, age: u8) {
    let older = (age + level.random_int(5) as u8 / 4).min(15);
    let flame = placement::fire_state_with_age(pos, &|p| level.state(p), older);
    level.set(pos, flame, UPDATE_ALL);
}

/// The strongest pull an empty cell has on fire from its flammable
/// neighbours; zero when the cell is occupied.
fn pull(level: &dyn GrowthLevel, cell: BlockPos) -> i32 {
    if !level.is_empty(cell) {
        return 0;
    }
    let mut best = 0;
    for side in super::DIRECTIONS {
        best = best.max(placement::ignite_odds(level.state(step(cell, side))) as i32);
    }
    best
}

struct Blaze {
    at: BlockPos,
    age: u8,
    fast_burnout: bool,
}

impl Blaze {
    fn consume(&self, level: &mut dyn GrowthLevel, side: Facing, divisor: i32) {
        let cell = step(self.at, side);
        let victim = level.state(cell);
        let divisor = if self.fast_burnout {
            divisor - 50
        } else {
            divisor
        };
        if level.random_int(divisor) >= placement::burn_odds(victim) as i32 {
            return;
        }
        let relight = level.random_int(self.age as i32 + 10) < 5 && !level.is_raining_at(cell);
        if victim.block() == Block::Tnt {
            level.prime_tnt(cell);
        }
        if relight {
            kindle(level, cell, self.age);
        } else {
            douse(level, cell);
        }
    }

    fn jump(&self, level: &mut dyn GrowthLevel) {
        let hardship = level.difficulty() as i32 * 7 + 40;
        let damping = self.age as i32 + 30;
        for (x, y, z, ceiling) in REACH_CELLS {
            let cell = offset(self.at, x.into(), y.into(), z.into());
            let draw = pull(level, cell);
            if draw <= 0 {
                continue;
            }
            let chance = ((draw + hardship) / damping) >> u8::from(self.fast_burnout);
            if chance <= 0 || level.random_int(ceiling.into()) > chance {
                continue;
            }
            if level.is_raining() && wet_around(level, cell) {
                continue;
            }
            kindle(level, cell, self.age);
        }
    }
}

/// Whether the fire at `pos` dies before spreading this tick.
fn gutters(
    level: &mut dyn GrowthLevel,
    pos: BlockPos,
    state: BlockState,
    base: BlockState,
) -> bool {
    let age = state.age().unwrap_or(0);
    let rained_out = level.is_raining()
        && wet_around(level, pos)
        && level.random_float() < 0.2 + age as f32 * 0.03;
    if rained_out {
        return true;
    }
    let grown = (age + level.random_int(3) as u8 / 2).min(15);
    if grown != age {
        level.set(pos, super::aged(state, grown), UPDATE_NONE);
    }
    if !placement::valid_fire_location(pos, &|p| level.state(p)) {
        return age > 3 || !base.is_face_sturdy(Facing::Up, tuff_vanilla_data::Support::Full);
    }
    age == 15 && level.random_int(4) == 0 && !placement::can_burn(base)
}

fn fire_tick(level: &mut dyn GrowthLevel, pos: BlockPos, state: BlockState) {
    let delay = tick_delay(level);
    level.schedule_tick(pos, Block::Fire, delay);
    if !can_spread_around(level, pos) {
        return;
    }
    if !super::can_survive(level, state, pos) {
        douse(level, pos);
    }
    let base = level.state(below(pos));
    let eternal =
        tuff_vanilla_data::growth::block_tag(level.dimension().infiniburn.trim_start_matches('#'))
            .contains(&base.block());
    let blaze = Blaze {
        at: pos,
        age: state.age().unwrap_or(0),
        fast_burnout: level
            .biome(pos)
            .and_then(|biome| level.biome_climate(biome).increased_fire_burnout)
            .unwrap_or(level.environment().increased_fire_burnout),
    };
    if eternal {
        let grown = (blaze.age + level.random_int(3) as u8 / 2).min(15);
        if grown != blaze.age {
            level.set(pos, super::aged(state, grown), UPDATE_NONE);
        }
    } else if gutters(level, pos, state, base) {
        douse(level, pos);
        return;
    }
    for (side, divisor) in CONSUME {
        blaze.consume(level, side, divisor);
    }
    blaze.jump(level);
}

fn ignited_by_lava(level: &dyn GrowthLevel, pos: BlockPos) -> bool {
    level.block(pos).is_some_and(BlockState::is_ignited_by_lava)
}

/// Lava's random tick: it may set fire to flammable blocks above and
/// around it.
pub fn lava_tick(level: &mut dyn GrowthLevel, pos: BlockPos, _state: BlockState) {
    if !can_spread_around(level, pos) {
        return;
    }
    let passes = level.random_int(3);
    if passes > 0 {
        let mut target = pos;
        for _ in 0..passes {
            let dx = level.random_int(3) - 1;
            let dz = level.random_int(3) - 1;
            target = offset(target, dx, 1, dz);
            let Some(state) = level.block(target) else {
                return;
            };
            if is_air(state) {
                let flammable_near = super::DIRECTIONS
                    .into_iter()
                    .any(|direction| ignited_by_lava(level, step(target, direction)));
                if flammable_near {
                    let fire = placement::fire_state(target, &|p| level.state(p));
                    level.set(target, fire, UPDATE_ALL);
                    return;
                }
            } else if tuff_datapack::tags::is(
                tuff_vanilla_data::tag::block::key::BLOCKS_LAVA_FIRE_SPREAD,
                state.block(),
            ) {
                return;
            }
        }
        return;
    }
    for _ in 0..3 {
        let dx = level.random_int(3) - 1;
        let dz = level.random_int(3) - 1;
        let target = offset(pos, dx, 0, dz);
        if level.block(target).is_none() {
            return;
        }
        if level.is_empty(above(target)) && ignited_by_lava(level, target) {
            let fire = placement::fire_state(target, &|p| level.state(p));
            level.set(above(target), fire, UPDATE_ALL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::TestLevel;
    use super::*;

    fn burning_platform() -> (TestLevel, BlockPos) {
        let mut level = TestLevel::new();
        level.rules.insert("fire_spread_radius_around_player", -1);
        level.fill(
            BlockPos::new(-3, 64, -3),
            BlockPos::new(3, 64, 3),
            Block::OakPlanks.default_state(),
        );
        let fire = BlockPos::new(0, 65, 0);
        level.put(fire, Block::Fire.default_state());
        (level, fire)
    }

    fn run_fires(level: &mut TestLevel, rounds: usize) {
        for _ in 0..rounds {
            let mut fires: Vec<BlockPos> = level
                .blocks
                .iter()
                .filter(|(_, state)| state.block() == Block::Fire)
                .map(|(pos, _)| *pos)
                .collect();
            fires.sort_by_key(|p| (p.x, p.y, p.z));
            for pos in fires {
                if level.state(pos).block() == Block::Fire {
                    level.run(fire_tick, pos);
                }
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn fire_spreads_over_planks_and_burns_them_away() {
        let (mut level, _) = burning_platform();
        run_fires(&mut level, 400);
        assert!(
            level.count(Block::OakPlanks) < 35,
            "{} planks left",
            level.count(Block::OakPlanks)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn fire_stays_put_away_from_players_by_default() {
        let (mut level, fire) = burning_platform();
        level.rules.remove("fire_spread_radius_around_player");
        run_fires(&mut level, 100);
        assert_eq!(level.count(Block::OakPlanks), 49);
        assert_eq!(level.state(fire), Block::Fire.default_state());
        level.player = Some(BlockPos::new(10, 64, 0));
        run_fires(&mut level, 100);
        assert!(level.count(Block::OakPlanks) < 49);
    }

    #[tuff_core::watchdog]
    #[test]
    fn rain_needs_a_clear_view_of_the_sky() {
        let mut level = TestLevel::new();
        level.raining = true;
        let pos = BlockPos::new(0, 64, 0);
        assert!(level.is_raining_at(pos));
        level.put(BlockPos::new(0, 70, 0), Block::Torch.default_state());
        assert!(!level.is_raining_at(pos));
    }

    #[tuff_core::watchdog]
    #[test]
    fn rain_puts_fire_out_but_not_on_infiniburn_blocks() {
        let (mut level, fire) = burning_platform();
        level.raining = true;
        for _ in 0..200 {
            if level.state(fire).block() != Block::Fire {
                break;
            }
            level.run(fire_tick, fire);
        }
        assert!(is_air(level.state(fire)));

        let mut level = TestLevel::new();
        level.rules.insert("fire_spread_radius_around_player", -1);
        level.raining = true;
        level.put(BlockPos::new(0, 64, 0), Block::Netherrack.default_state());
        let fire = BlockPos::new(0, 65, 0);
        level.put(fire, Block::Fire.default_state());
        for _ in 0..200 {
            level.run(fire_tick, fire);
        }
        assert_eq!(level.state(fire).block(), Block::Fire);
    }

    #[tuff_core::watchdog]
    #[test]
    fn burning_tnt_is_primed() {
        let mut level = TestLevel::new();
        level.rules.insert("fire_spread_radius_around_player", -1);
        level.put(BlockPos::new(0, 64, 0), Block::Stone.default_state());
        let fire = BlockPos::new(0, 65, 0);
        level.put(BlockPos::new(1, 65, 0), Block::Tnt.default_state());
        for _ in 0..500 {
            level.put(fire, Block::Fire.default_state());
            level.run(fire_tick, fire);
            if !level.primed.is_empty() {
                break;
            }
        }
        assert_eq!(level.primed, vec![BlockPos::new(1, 65, 0)]);
        assert_ne!(level.state(BlockPos::new(1, 65, 0)).block(), Block::Tnt);
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_the_bottom_of_a_fire_block_touches() {
        let mut level = TestLevel::new();
        level.put(BlockPos::new(0, 64, 0), Block::SoulFire.default_state());
        let lookup = |p: BlockPos| level.block(p);
        let standing = [0.2, 64.0, 0.2, 0.8, 65.8, 0.8];
        assert_eq!(fire_contact(standing, lookup), Some(2.0));
        let above = [0.2, 64.1, 0.2, 0.8, 65.9, 0.8];
        assert_eq!(fire_contact(above, lookup), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn lava_sets_fire_to_wood_above_it() {
        let mut level = TestLevel::new();
        level.rules.insert("fire_spread_radius_around_player", -1);
        let lava = BlockPos::new(0, 64, 0);
        level.put(lava, Block::Lava.default_state());
        level.fill(
            BlockPos::new(-1, 66, -1),
            BlockPos::new(1, 66, 1),
            Block::OakPlanks.default_state(),
        );
        for _ in 0..200 {
            level.run(lava_tick, lava);
        }
        assert!(level.count(Block::Fire) > 0);
    }
    fn fingerprint(level: &TestLevel) -> u64 {
        use std::hash::{Hash, Hasher};
        struct Fnv(u64);
        impl Hasher for Fnv {
            fn finish(&self) -> u64 {
                self.0
            }
            fn write(&mut self, bytes: &[u8]) {
                for b in bytes {
                    self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3);
                }
            }
        }
        let mut cells: Vec<_> = level
            .blocks
            .iter()
            .map(|(p, s)| (p.x, p.y, p.z, s.id()))
            .collect();
        cells.sort_unstable();
        let mut h = Fnv(0xcbf2_9ce4_8422_2325);
        cells.hash(&mut h);
        for p in &level.primed {
            (p.x, p.y, p.z).hash(&mut h);
        }
        level.ticks.len().hash(&mut h);
        h.finish()
    }

    #[tuff_core::watchdog]
    #[test]
    fn fire_tick_outcome_is_pinned_for_a_fixed_seed() {
        let (mut level, _) = burning_platform();
        level.put(BlockPos::new(2, 65, 0), Block::Tnt.default_state());
        level.put(BlockPos::new(0, 66, 1), Block::OakLeaves.default_state());
        level.fill(
            BlockPos::new(-2, 68, -2),
            BlockPos::new(2, 68, 2),
            Block::WhiteWool.default_state(),
        );
        let mut dry = 0u64;
        for _ in 0..12 {
            run_fires(&mut level, 1);
            dry = dry.rotate_left(7) ^ fingerprint(&level);
        }
        level.raining = true;
        let mut wet = 0u64;
        for _ in 0..12 {
            run_fires(&mut level, 1);
            wet = wet.rotate_left(7) ^ fingerprint(&level);
        }
        let (mut lava, _) = burning_platform();
        lava.put(BlockPos::new(0, 62, 0), Block::Lava.default_state());
        for _ in 0..100 {
            lava.run(lava_tick, BlockPos::new(0, 62, 0));
        }
        assert_eq!(
            (dry, wet, fingerprint(&lava)),
            (
                7547219352763063991,
                18079736635820111555,
                5711889758464386253
            )
        );
    }
}
