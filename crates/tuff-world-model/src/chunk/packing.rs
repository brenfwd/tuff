use super::*;

pub(super) fn palette<T: Copy + PartialEq>(values: impl Iterator<Item = T>) -> (Vec<T>, Vec<u16>) {
    let mut palette: Vec<T> = Vec::new();
    let mut last: Option<(T, u16)> = None;
    let indices = values
        .map(|value| {
            if let Some((prev, index)) = last
                && prev == value
            {
                return index;
            }
            let index = match palette.iter().position(|p| *p == value) {
                Some(index) => index as u16,
                None => {
                    palette.push(value);
                    (palette.len() - 1) as u16
                }
            };
            last = Some((value, index));
            index
        })
        .collect();
    (palette, indices)
}

pub(super) fn write_packed(out: &mut PacketWriter, bits: u32, values: impl Iterator<Item = u64>) {
    for word in pack(bits, values) {
        out.write_long(word as i64);
    }
}

/// Packs values into longs without splitting any value across two longs.
pub fn pack(bits: u32, values: impl Iterator<Item = u64>) -> Vec<u64> {
    let per_long = (64 / bits) as usize;
    let mut words = Vec::new();
    for (i, value) in values.enumerate() {
        let shift = (i % per_long) as u32 * bits;
        if shift == 0 {
            words.push(0);
        }
        *words.last_mut().unwrap() |= value << shift;
    }
    words
}

/// Inverse of [`pack`]. Returns `None` if `words` has the wrong length.
pub fn unpack(bits: u32, count: usize, words: &[i64]) -> Option<Vec<u64>> {
    let per_long = (64 / bits) as usize;
    if words.len() != count.div_ceil(per_long) {
        return None;
    }
    let mask = (1u64 << bits) - 1;
    Some(
        (0..count)
            .map(|i| (words[i / per_long] as u64 >> ((i % per_long) as u32 * bits)) & mask)
            .collect(),
    )
}
