use super::*;

/// The Update Light body (everything after the packet ID), carrying only the
/// sections whose bits are set in the masks.
pub fn light_update_body(
    chunk_x: i32,
    chunk_z: i32,
    light: &ChunkLight,
    sky_mask: u64,
    block_mask: u64,
) -> Bytes {
    let only = |layers: &[SectionLight], mask: u64| -> Vec<SectionLight> {
        layers
            .iter()
            .enumerate()
            .map(|(i, section)| {
                if mask >> i & 1 == 1 {
                    section.clone()
                } else {
                    SectionLight::Missing
                }
            })
            .collect()
    };
    let mut w = PacketWriter::new();
    w.write_var_int(chunk_x);
    w.write_var_int(chunk_z);
    write_light(
        &mut w,
        &only(&light.sky, sky_mask),
        &only(&light.block, block_mask),
    );
    w.into_bytes()
}

pub(super) fn write_light(w: &mut PacketWriter, sky: &[SectionLight], block: &[SectionLight]) {
    let masks = |layers: &[SectionLight]| {
        let mut present = 0u64;
        let mut empty = 0u64;
        for (i, layer) in layers.iter().enumerate() {
            match layer {
                SectionLight::Data(_) => present |= 1 << i,
                SectionLight::Empty => empty |= 1 << i,
                SectionLight::Missing => {}
            }
        }
        (present, empty)
    };
    let (sky_mask, empty_sky) = masks(sky);
    let (block_mask, empty_block) = masks(block);
    // Light masks are serialized as byte arrays (Java's BitSet.toByteArray()),
    // unlike standard protocol BitSets which use long arrays.
    write_bitset_bytes(w, sky_mask);
    write_bitset_bytes(w, block_mask);
    write_bitset_bytes(w, empty_sky);
    write_bitset_bytes(w, empty_block);
    for layers in [sky, block] {
        let data: Vec<&[u8; LIGHT_BYTES]> = layers
            .iter()
            .filter_map(|layer| match layer {
                SectionLight::Data(data) => Some(&**data),
                _ => None,
            })
            .collect();
        w.write_var_int(data.len() as i32);
        for data in data {
            w.write_prefixed_bytes(&data[..]);
        }
    }
}

pub(super) fn write_bitset_bytes(w: &mut PacketWriter, word: u64) {
    let mut bytes = BytesMut::from(&word.to_le_bytes()[..]);
    while bytes.last() == Some(&0) {
        bytes.truncate(bytes.len() - 1);
    }
    w.write_prefixed_bytes(&bytes);
}
