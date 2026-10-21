//! A set of block positions that iterates in the order the game's hash set
//! does. Tree decorators draw random numbers per leaf and log in that order,
//! so any other order changes the generated blocks.
//!
//! Buckets are power-of-two sized, grow at a load factor of 0.75, and keep
//! insertion order within a bucket (a split on growth preserves it). A
//! bucket that would switch to a balanced tree (nine entries in a table of
//! 64 or more) keeps list order here, the one known divergence.

use tuff_world_model::BlockPos;

const INITIAL: usize = 16;
const TREEIFY: usize = 9;
const MIN_TREEIFY_CAPACITY: usize = 64;

#[derive(Clone, Default)]
pub struct PosSet {
    buckets: Vec<Vec<BlockPos>>,
    len: usize,
    first: usize,
}

pub fn pos_hash(pos: BlockPos) -> i32 {
    let h = pos
        .y
        .wrapping_add(pos.z.wrapping_mul(31))
        .wrapping_mul(31)
        .wrapping_add(pos.x);
    h ^ ((h as u32) >> 16) as i32
}

impl PosSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn index(&self, pos: BlockPos) -> usize {
        pos_hash(pos) as u32 as usize & (self.buckets.len() - 1)
    }

    pub fn contains(&self, pos: BlockPos) -> bool {
        !self.buckets.is_empty() && self.buckets[self.index(pos)].contains(&pos)
    }

    pub fn insert(&mut self, pos: BlockPos) -> bool {
        if self.buckets.is_empty() {
            self.buckets = vec![Vec::new(); INITIAL];
            self.first = INITIAL;
        }
        let index = self.index(pos);
        if self.buckets[index].contains(&pos) {
            return false;
        }
        self.buckets[index].push(pos);
        self.first = self.first.min(index);
        if self.buckets[index].len() >= TREEIFY && self.buckets.len() < MIN_TREEIFY_CAPACITY {
            self.grow();
        }
        self.len += 1;
        if self.len > self.buckets.len() / 4 * 3 {
            self.grow();
        }
        true
    }

    fn grow(&mut self) {
        let capacity = self.buckets.len() * 2;
        let old = std::mem::replace(&mut self.buckets, vec![Vec::new(); capacity]);
        for pos in old.into_iter().flatten() {
            let index = self.index(pos);
            self.buckets[index].push(pos);
        }
        self.first = self
            .buckets
            .iter()
            .position(|b| !b.is_empty())
            .unwrap_or(capacity);
    }

    /// Removes and returns the first position in iteration order.
    pub fn pop_first(&mut self) -> Option<BlockPos> {
        while self.first < self.buckets.len() {
            let bucket = &mut self.buckets[self.first];
            if !bucket.is_empty() {
                self.len -= 1;
                return Some(bucket.remove(0));
            }
            self.first += 1;
        }
        None
    }

    pub fn iter(&self) -> impl Iterator<Item = BlockPos> + '_ {
        self.buckets.iter().flatten().copied()
    }
}

/// The iteration order of the game's open-addressing set of longs (chunk
/// references use it): the zero key first, then table slots from the top
/// down, with linear probing and the table doubling at a load of 0.75.
pub fn long_set_order(keys: &[i64]) -> Vec<i64> {
    fn mix(x: i64) -> i64 {
        let h = x.wrapping_mul(0x9E37_79B9_7F4A_7C15_u64 as i64);
        let h = h ^ ((h as u64) >> 32) as i64;
        h ^ ((h as u64) >> 16) as i64
    }
    fn size_for(expected: usize) -> usize {
        ((expected as f64 / 0.75).ceil() as usize)
            .next_power_of_two()
            .max(2)
    }
    fn insert(table: &mut [i64], key: i64) {
        let mask = table.len() - 1;
        let mut pos = mix(key) as usize & mask;
        while table[pos] != 0 {
            pos = (pos + 1) & mask;
        }
        table[pos] = key;
    }
    let mut table = vec![0i64; size_for(16)];
    let mut has_zero = false;
    let mut size = 0usize;
    for &key in keys {
        if key == 0 {
            if has_zero {
                continue;
            }
            has_zero = true;
        } else if table.contains(&key) {
            continue;
        } else {
            insert(&mut table, key);
        }
        let max_fill = ((table.len() as f64 * 0.75).ceil() as usize).min(table.len() - 1);
        let grow = size >= max_fill;
        size += 1;
        if grow {
            let mut bigger = vec![0i64; size_for(size + 1)];
            for &old in table.iter().rev() {
                if old != 0 {
                    insert(&mut bigger, old);
                }
            }
            table = bigger;
        }
    }
    let mut out = Vec::with_capacity(size);
    if has_zero {
        out.push(0);
    }
    out.extend(table.iter().rev().copied().filter(|&k| k != 0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn iterates_in_bucket_order_across_growth() {
        let mut set = PosSet::new();
        let positions: Vec<BlockPos> = (0..40)
            .map(|i| BlockPos::new(i * 7 - 100, 64 + i % 5, -(i * 3)))
            .collect();
        for pos in &positions {
            assert!(set.insert(*pos));
        }
        assert!(!set.insert(positions[3]));
        assert_eq!(set.len(), 40);
        let capacity = 64u32;
        let mut expected = positions.clone();
        expected.sort_by_key(|p| pos_hash(*p) as u32 & (capacity - 1));
        assert_eq!(set.iter().collect::<Vec<_>>(), expected);
        assert_eq!(set.pop_first(), Some(expected[0]));
        assert!(!set.contains(expected[0]));
        assert_eq!(set.len(), 39);
    }
}
