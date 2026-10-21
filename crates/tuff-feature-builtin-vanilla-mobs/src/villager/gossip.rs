//! What villagers say about others: per target, a value for each gossip
//! kind. Reputation is the weighted sum; gossip decays daily and spreads,
//! weakened, when villagers talk.

use std::collections::BTreeMap;

use rand::Rng;
use tuff_vanilla_data::villager::{GOSSIP_KINDS, GossipKind};

use tuff_nbt::{Compound, Tag};

/// Values below this are forgotten.
const DISCARD_THRESHOLD: i32 = 2;

pub fn kind(name: &str) -> &'static GossipKind {
    GossipKind::get(name).expect("gossip kinds come from the game")
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gossips {
    /// Target UUID to (kind index into `GOSSIP_KINDS`, value).
    entries: BTreeMap<u128, Vec<(usize, i32)>>,
}

impl Gossips {
    /// One `name: KIND: value` line per gossip, for the brains debug feed.
    pub fn debug_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (target, entries) in &self.entries {
            let name = tuff_core::world::debug_feeds::uuid_name(*target);
            for (kind, value) in entries {
                let kind = GOSSIP_KINDS[*kind].name.to_ascii_uppercase();
                lines.push(format!("{name}: {kind}: {value}"));
            }
        }
        lines
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn index(kind: &GossipKind) -> usize {
        GOSSIP_KINDS
            .iter()
            .position(|k| k.name == kind.name)
            .expect("known gossip kind")
    }

    pub fn value(&self, target: u128, kind: &GossipKind) -> i32 {
        let index = Self::index(kind);
        self.entries
            .get(&target)
            .and_then(|list| list.iter().find(|(k, _)| *k == index))
            .map_or(0, |(_, v)| *v)
    }

    /// The weighted sum of everything said about `target`.
    pub fn reputation(&self, target: u128) -> i32 {
        self.entries.get(&target).map_or(0, |list| {
            list.iter()
                .map(|(kind, value)| value * GOSSIP_KINDS[*kind].weight)
                .sum()
        })
    }

    /// Adds to a gossip value, up to the kind's maximum.
    pub fn add(&mut self, target: u128, kind: &GossipKind, amount: i32) {
        let index = Self::index(kind);
        let list = self.entries.entry(target).or_default();
        match list.iter_mut().find(|(k, _)| *k == index) {
            Some((_, old)) => {
                let sum = *old + amount;
                *old = if sum > kind.max {
                    kind.max.max(*old)
                } else {
                    sum
                };
            }
            None => list.push((index, amount)),
        }
        if let Some(position) = list.iter().position(|(k, _)| *k == index) {
            let value = list[position].1;
            if value > kind.max {
                list[position].1 = kind.max;
            }
            if value < DISCARD_THRESHOLD {
                list.remove(position);
            }
        }
        if list.is_empty() {
            self.entries.remove(&target);
        }
    }

    /// A day passed: every value drops by its kind's daily decay.
    pub fn decay(&mut self) {
        for list in self.entries.values_mut() {
            list.retain_mut(|(kind, value)| {
                *value -= GOSSIP_KINDS[*kind].decay_per_day;
                *value >= DISCARD_THRESHOLD
            });
        }
        self.entries.retain(|_, list| !list.is_empty());
    }

    fn flat(&self) -> Vec<(u128, usize, i32)> {
        self.entries
            .iter()
            .flat_map(|(target, list)| list.iter().map(|(k, v)| (*target, *k, *v)))
            .collect()
    }

    /// Hears up to `max` pieces of gossip from `source`, picked by weight,
    /// each weakened by its kind's transfer decay. Returns how many were
    /// picked.
    pub fn transfer_from(&mut self, source: &Gossips, rng: &mut impl Rng, max: usize) -> usize {
        let pool = source.flat();
        let mut bounds = Vec::with_capacity(pool.len());
        let mut weight_sum = 0;
        for &(_, kind, value) in &pool {
            weight_sum += (value * GOSSIP_KINDS[kind].weight).abs();
            bounds.push(weight_sum);
        }
        if weight_sum <= 0 {
            return 0;
        }
        let mut heard = vec![false; pool.len()];
        let mut order = Vec::with_capacity(max.min(pool.len()));
        for _ in 0..max {
            let roll = rng.gen_range(0..weight_sum);
            let slot = bounds.partition_point(|&upper| upper <= roll);
            if !std::mem::replace(&mut heard[slot], true) {
                order.push(slot);
            }
        }
        for &slot in &order {
            let (target, kind, value) = pool[slot];
            self.learn(target, kind, value - GOSSIP_KINDS[kind].decay_per_transfer);
        }
        order.len()
    }

    fn learn(&mut self, target: u128, kind: usize, value: i32) {
        if value < DISCARD_THRESHOLD {
            return;
        }
        let known = self.entries.entry(target).or_default();
        if let Some(slot) = known.iter_mut().find(|(k, _)| *k == kind) {
            slot.1 = slot.1.max(value);
        } else {
            known.push((kind, value));
        }
    }

    /// The saved list: `[{Target, Type, Value}]`.
    pub fn to_nbt(&self) -> Tag {
        Tag::List(
            self.flat()
                .into_iter()
                .map(|(target, kind, value)| {
                    Tag::Compound(
                        Compound::new()
                            .with("Target", Tag::uuid(target))
                            .with("Type", GOSSIP_KINDS[kind].name)
                            .with("Value", value),
                    )
                })
                .collect(),
        )
    }

    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        let mut gossips = Self::default();
        for entry in tag.as_list()? {
            let entry = entry.as_compound()?;
            let target = entry.get_uuid("Target")?;
            let kind = GossipKind::get(entry.get_str("Type")?)?;
            let value = entry.get_int("Value").filter(|v| *v > 0)?;
            let index = Self::index(kind);
            let list = gossips.entries.entry(target).or_default();
            list.retain(|(k, _)| *k != index);
            list.push((index, value));
        }
        Some(gossips)
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn reputation_is_the_weighted_sum() {
        let mut gossips = Gossips::default();
        gossips.add(1, kind("trading"), 2);
        gossips.add(1, kind("major_positive"), 20);
        gossips.add(1, kind("minor_negative"), 25);
        assert_eq!(gossips.reputation(1), 2 + 5 * 20 - 25);
        assert_eq!(gossips.reputation(2), 0);
        gossips.add(1, kind("trading"), 100);
        assert_eq!(
            gossips.value(1, kind("trading")),
            25,
            "capped at the maximum"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn small_values_are_forgotten_and_decay() {
        let mut gossips = Gossips::default();
        gossips.add(1, kind("trading"), 1);
        assert!(gossips.is_empty());
        gossips.add(1, kind("trading"), 5);
        gossips.decay();
        assert_eq!(gossips.value(1, kind("trading")), 3);
        gossips.decay();
        assert!(gossips.is_empty());
        gossips.add(1, kind("major_positive"), 20);
        gossips.decay();
        assert_eq!(
            gossips.value(1, kind("major_positive")),
            20,
            "no daily decay"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn gossip_spreads_weakened_and_round_trips() {
        let mut source = Gossips::default();
        source.add(7, kind("minor_negative"), 100);
        let mut listener = Gossips::default();
        let picked = listener.transfer_from(&source, &mut StdRng::seed_from_u64(1), 10);
        assert_eq!(picked, 1);
        assert_eq!(listener.value(7, kind("minor_negative")), 80);
        let saved = source.to_nbt();
        assert_eq!(Gossips::from_nbt(&saved), Some(source));
    }

    fn mixed_source() -> Gossips {
        let mut source = Gossips::default();
        for target in 1..=6u128 {
            source.add(target, kind("minor_negative"), 10 * target as i32 + 3);
            source.add(target, kind("trading"), target as i32 * 3);
            source.add(target * 100, kind("major_positive"), 5 + target as i32);
            source.add(target * 100, kind("minor_positive"), 40);
        }
        source
    }

    #[tuff_core::watchdog]
    #[test]
    fn transfer_picks_are_pinned_for_fixed_seeds() {
        let source = mixed_source();
        let mut digest = Vec::new();
        for seed in 0..40u64 {
            let mut listener = Gossips::default();
            listener.add(3, kind("minor_negative"), 7);
            let mut rng = StdRng::seed_from_u64(seed);
            let picked = listener.transfer_from(&source, &mut rng, 10);
            let tail: u32 = rng.r#gen();
            digest.push(format!("{picked}:{tail}:{:?}", listener.debug_lines()));
        }
        let joined = digest.join("|");
        let hash = joined.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
        });
        assert_eq!(hash, 2_495_429_426_645_714_610, "{joined}");
    }

    #[tuff_core::watchdog]
    #[test]
    #[ignore = "timing"]
    fn transfer_timing() {
        let source = mixed_source();
        let mut rng = StdRng::seed_from_u64(9);
        let start = std::time::Instant::now();
        let mut total = 0;
        for _ in 0..200_000 {
            let mut listener = Gossips::default();
            total += listener.transfer_from(&source, &mut rng, 10);
        }
        eprintln!("TIMING {:?} {total}", start.elapsed());
    }
}
