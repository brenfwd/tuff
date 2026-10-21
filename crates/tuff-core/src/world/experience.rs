//! Experience vocabulary: the game's split of an amount into orb values,
//! award formulas, and the orb capability the region merges and collects by.

use rand::Rng;
use tuff_vanilla_data::registry::EntityType;

use tuff_nbt::{Compound, Tag};

pub const LIFETIME: u32 = 6000;
pub const MERGE_SCAN_PERIOD: u32 = 20;
/// Orbs whose entity ids are congruent modulo this merge.
pub const MERGE_GROUPS: i32 = 40;
pub const MERGE_DISTANCE: f64 = 0.5;
/// A player collects at most one orb every this many ticks.
pub const PICKUP_DELAY: u64 = 2;
/// The orb sizes the game splits an amount into, largest first.
const VALUES: [i32; 11] = [2477, 1237, 617, 307, 149, 73, 37, 17, 7, 3, 1];

/// The largest orb value not above `amount`.
pub fn orb_value(amount: i32) -> i32 {
    VALUES
        .into_iter()
        .find(|value| amount >= *value)
        .unwrap_or(1)
}

/// The orb values `amount` splits into, in spawn order.
pub fn split(mut amount: i32) -> Vec<i32> {
    let mut values = Vec::new();
    while amount > 0 {
        let value = orb_value(amount);
        amount -= value;
        values.push(value);
    }
    values
}

/// A bottle o' enchanting's award on breaking: 3 plus two rolls of 0..5,
/// so 3 to 11 with the middle more likely than the ends.
pub fn bottle_amount(rng: &mut impl Rng) -> i32 {
    3 + rng.gen_range(0..5) + rng.gen_range(0..5)
}

/// A cooking recipe's experience for `count` results: the whole part plus
/// one more with the fractional part's probability.
pub fn cooking_reward(count: i32, value: f32, rng: &mut impl Rng) -> i32 {
    let total = count as f32 * value;
    let mut reward = total.floor() as i32;
    let fraction = total - total.floor();
    if fraction != 0.0 && rng.r#gen::<f32>() < fraction {
        reward += 1;
    }
    reward
}

/// What the region needs of an experience orb to merge and collect it.
pub trait Orb {
    fn value(&self) -> i32;
    fn count(&self) -> i32;
    fn age(&self) -> u32;
    fn merges_with(&self, id: i32, value: i32) -> bool;
    fn merge_due(&self) -> bool;
    fn absorb(&mut self, count: i32, age: u32);
    fn touches(&self, feet: [f64; 3]) -> bool;
    /// Hands out one of its orbs; whether any are left.
    fn take_one(&mut self) -> bool;
}

/// The saved form of a freshly awarded orb with the game's random initial
/// velocity.
pub fn spawn_nbt(position: [f64; 3], value: i32, rng: &mut impl Rng) -> Compound {
    let velocity = [
        (rng.r#gen::<f64>() * 0.2 - 0.1) * 2.0,
        rng.r#gen::<f64>() * 0.2 * 2.0,
        (rng.r#gen::<f64>() * 0.2 - 0.1) * 2.0,
    ];
    Compound::new()
        .with("id", EntityType::ExperienceOrb.name())
        .with("Pos", Tag::List(position.map(Tag::Double).to_vec()))
        .with("Motion", Tag::List(velocity.map(Tag::Double).to_vec()))
        .with("Value", value.clamp(0, i16::MAX as i32) as i16)
}

pub fn intersects(a: [f64; 6], b: [f64; 6]) -> bool {
    (0..3).all(|axis| a[axis] < b[axis + 3] && a[axis + 3] > b[axis])
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    #[tuff_core::watchdog]
    #[test]
    fn amounts_split_into_the_standard_orb_values() {
        assert_eq!(split(0), Vec::<i32>::new());
        assert_eq!(split(1), vec![1]);
        assert_eq!(split(2), vec![1, 1]);
        assert_eq!(split(7), vec![7]);
        assert_eq!(split(100), vec![73, 17, 7, 3]);
        assert_eq!(split(2477), vec![2477]);
        assert_eq!(split(5000), vec![2477, 2477, 37, 7, 1, 1]);
        assert_eq!(orb_value(36), 17);
    }

    #[tuff_core::watchdog]
    #[test]
    fn bottle_amounts_land_in_range_with_a_triangular_spread() {
        let mut rng = StdRng::seed_from_u64(7);
        let samples: Vec<i32> = (0..500).map(|_| bottle_amount(&mut rng)).collect();
        assert!(samples.iter().all(|a| (3..=11).contains(a)), "{samples:?}");
        assert!(samples.contains(&3));
        assert!(samples.contains(&11));
        let middling = samples
            .iter()
            .copied()
            .filter(|a| (6..=8).contains(a))
            .count();
        let extreme = samples.iter().filter(|a| **a == 3 || **a == 11).count();
        assert!(
            middling > extreme,
            "the middle of the range should be far more common: {middling} vs {extreme}"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn cooking_rewards_round_by_the_fraction() {
        let mut rng = StdRng::seed_from_u64(3);
        assert_eq!(cooking_reward(10, 0.7, &mut rng), 7);
        assert_eq!(cooking_reward(3, 1.0, &mut rng), 3);
        let rewards: Vec<i32> = (0..200)
            .map(|_| cooking_reward(1, 0.35, &mut rng))
            .collect();
        assert!(rewards.iter().all(|r| *r == 0 || *r == 1));
        let ones = rewards.iter().filter(|r| **r == 1).count();
        assert!((40..=100).contains(&ones), "{ones} of 200");
    }
}
