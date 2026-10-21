//! A variant plus a matching sound variant, picked into a data-driven
//! registry pair (`cow_variant`/`cow_sound_variant`, `wolf_variant`/
//! `wolf_sound_variant`, ...) the same way for every voiced animal:
//! biome-based spawn selection, index-based save/load, and metadata as two
//! consecutive registry holders.

use rand::Rng;
use rand::rngs::StdRng;
use tuff_vanilla_data::variant::{self, FarmVariant, SoundVariant};

use super::MobCore;
use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{TickContext, take};
use tuff_nbt::Compound;

/// A variant and sound variant by index into their generated lists.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Variants {
    pub variant: usize,
    pub sound: usize,
}

impl Variants {
    pub fn default_for(variants: &'static [FarmVariant], sounds: &'static [SoundVariant]) -> Self {
        Self {
            variant: variant::index(variants, "minecraft:temperate").unwrap_or(0),
            sound: sounds
                .iter()
                .position(|s| s.name == "minecraft:classic")
                .unwrap_or(0),
        }
    }

    pub fn load(
        &mut self,
        nbt: &mut Compound,
        variants: &'static [FarmVariant],
        sounds: &'static [SoundVariant],
    ) {
        if let Some(index) = take(nbt, "variant", |t| variant::index(variants, t.as_str()?)) {
            self.variant = index;
        }
        if let Some(index) = take(nbt, "sound_variant", |t| {
            let name = t.as_str()?;
            sounds.iter().position(|s| s.name == name)
        }) {
            self.sound = index;
        }
    }

    pub fn save(
        &self,
        out: &mut Compound,
        variants: &'static [FarmVariant],
        sounds: &'static [SoundVariant],
    ) {
        out.insert("variant", variants[self.variant].name);
        out.insert("sound_variant", sounds[self.sound].name);
    }

    fn holder(core: &MobCore, index: u8, id: usize) -> (u8, MetadataValue) {
        (
            index,
            MetadataValue::Holder {
                serializer: core.base.kind.data_serializer(index).unwrap_or(0),
                id: id as i32,
            },
        )
    }

    /// The variant and its sound variant at consecutive indices, as most
    /// voiced animals send them (cow, pig, chicken, wolf).
    pub fn metadata(&self, core: &MobCore, first: u8) -> Vec<(u8, MetadataValue)> {
        vec![
            Self::holder(core, first, self.variant),
            Self::holder(core, first + 1, self.sound),
        ]
    }

    /// The variant alone, for a layout that separates it from its sound
    /// variant (the cat).
    pub fn variant_metadata(&self, core: &MobCore, index: u8) -> (u8, MetadataValue) {
        Self::holder(core, index, self.variant)
    }

    /// The sound variant alone; pairs with `variant_metadata`.
    pub fn sound_metadata(&self, core: &MobCore, index: u8) -> (u8, MetadataValue) {
        Self::holder(core, index, self.sound)
    }

    pub fn finalize(
        &mut self,
        core: &MobCore,
        variants: &'static [FarmVariant],
        sounds: &'static [SoundVariant],
        cx: &mut TickContext,
    ) {
        self.finalize_at(core, variants, sounds, None, cx);
    }

    pub fn finalize_at(
        &mut self,
        core: &MobCore,
        variants: &'static [FarmVariant],
        sounds: &'static [SoundVariant],
        structure: Option<&str>,
        cx: &mut TickContext,
    ) {
        let pos = super::physics::block_pos(core.base.position);
        let biome = (cx.biome)(pos).unwrap_or("minecraft:plains");
        let moon_brightness = tuff_core::world::moon_brightness(cx.shared, cx.dimension);
        let candidates = variant::candidates_at(
            variants,
            &variant::SpawnSite {
                biome,
                structure,
                moon_brightness,
            },
        );
        if !candidates.is_empty() {
            self.variant = candidates[cx.rng.gen_range(0..candidates.len())];
        }
        self.sound = cx.rng.gen_range(0..sounds.len());
    }

    pub fn inherit(&self, other: &Self, defaults: Self, rng: &mut StdRng) -> Self {
        Self {
            variant: if rng.r#gen() {
                self.variant
            } else {
                other.variant
            },
            sound: defaults.sound,
        }
    }

    pub fn sounds(
        &self,
        sounds: &'static [SoundVariant],
        baby: bool,
    ) -> &'static variant::SoundSet {
        sounds[self.sound].sounds(baby)
    }
}
