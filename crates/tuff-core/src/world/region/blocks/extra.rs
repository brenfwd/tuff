use super::*;

pub(super) fn section_block(chunk: ChunkPos, section_y: i32, index: u16) -> BlockPos {
    BlockPos::new(
        chunk.x * 16 + i32::from(index >> 8),
        section_y * 16 + i32::from(index & 15),
        chunk.z * 16 + i32::from((index >> 4) & 15),
    )
}

pub(super) fn block_experience_effects(tool: Option<&ItemStack>, amount: i32) -> i32 {
    let registry = tuff_item::enchantment::current();
    let mut rng = rand::thread_rng();
    let mut value = amount as f32;
    let on_tool = tool.and_then(|tool| tool.components.enchantments.as_ref());
    for (_, def, level) in tuff_item::enchantment::leveled(&registry, on_tool) {
        for entry in &def.effects.block_experience {
            value = entry.effect.process(level, &mut rng, value);
        }
    }
    value.max(0.0) as i32
}

pub(super) fn stand_in_place_sound(state: BlockState, pos: BlockPos, seed: i64) -> Packets {
    let (sound, volume, pitch) = state.place_sound();
    packets([framed(&play::Sound {
        sound_event: sound.id(),
        category: play::sound_category::BLOCKS,
        position: pos.center(),
        volume: (volume + 1.0) / 2.0,
        pitch: pitch * 0.8,
        seed,
    })])
}

impl RegionActor {
    /// The shelves around an enchanting table that lend it power.
    pub(super) fn enchanting_bookcases(&self, table: BlockPos) -> i32 {
        use tuff_vanilla_data::tag::block::key::{
            ENCHANTMENT_POWER_PROVIDER, ENCHANTMENT_POWER_TRANSMITTER,
        };
        let mut count = 0;
        for dx in -2i32..=2 {
            for dz in -2i32..=2 {
                if dx.abs() != 2 && dz.abs() != 2 {
                    continue;
                }
                for dy in 0..=1 {
                    let shelf = BlockPos::new(table.x + dx, table.y + dy, table.z + dz);
                    let between = BlockPos::new(table.x + dx / 2, table.y + dy, table.z + dz / 2);
                    let is = |pos, key| {
                        self.block(pos)
                            .is_some_and(|s| tuff_datapack::tags::is(key, s.block()))
                    };
                    if is(shelf, ENCHANTMENT_POWER_PROVIDER)
                        && is(between, ENCHANTMENT_POWER_TRANSMITTER)
                    {
                        count += 1;
                    }
                }
            }
        }
        count
    }
}
