//! Encodings for protocol data types shared by the configuration and play
//! states (https://minecraft.wiki/w/Java_Edition_protocol/Data_types).

use tuff_vanilla_data::Item;
use tuff_vanilla_data::data_registry::{BANNER_PATTERN, TRIM_MATERIAL, TRIM_PATTERN};
use tuff_vanilla_data::item::{
    DyeColor, EquipmentSlotGroup, FireworkShape, ModifierOperation, Weapon,
};
use tuff_vanilla_data::registry::{Attribute, Block, DataComponentType, MobEffect, Potion};
use tuff_world_model::BlockPos;

use tuff_item::item::bundle::BundleContents;
use tuff_item::item::decor::{
    ArmorTrim, BannerLayer, Filterable, FireworkExplosion, Fireworks, InlineTrimMaterial,
    InlineTrimPattern, MAX_BANNER_LAYERS, MAX_BOOK_PAGES, MAX_EXPLOSIONS, MAX_PAGE_CHARS,
    MAX_TITLE_CHARS, WrittenBook,
};
use tuff_item::item::gear::{
    AttributeModifier, BlockSet, MAX_MODIFIERS, MAX_TOOL_RULES, ModifierDisplay, Tool, ToolRule,
};
use tuff_item::item::{
    Components, EffectDetails, Enchantments, ItemPicture, ItemView, Mode, NetworkComponent,
    Picture, PotionContents, PotionEffect, SuspiciousEffect, typed_entity_name,
};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_wire::{BufferError, PacketReader, PacketWriter, Result};

const MAX_HASHED_COMPONENTS: usize = 256;
const MAX_SLOT_COMPONENTS: i32 = 256;
const MAX_ENCHANTMENTS: i32 = 256;
const MAX_LORE_LINES: i32 = 256;
const MAX_POTION_EFFECTS: i32 = 256;
const MAX_SUSPICIOUS_STEW_EFFECTS: i32 = 256;
const MAX_BLOCK_STATE_PROPERTIES: i32 = 64;
const MAX_HIDDEN_EFFECT_DEPTH: u32 = 16;
const MAX_CONTAINER_DEPTH: u32 = 4;
const MAX_ENCHANTMENT_LEVEL: i32 = 255;
const MAX_IDENTIFIER_BYTES: usize = 32767;
const LP_VEC3_ABS_MAX: f64 = 1.7179869183e10;

/// The vector a client decodes from `v` written as an `lp_vec3`.
pub fn lp_vec3_received(v: [f64; 3]) -> [f64; 3] {
    let v = v.map(|c| {
        if c.is_finite() {
            c.clamp(-LP_VEC3_ABS_MAX, LP_VEC3_ABS_MAX)
        } else {
            0.0
        }
    });
    let max = v.iter().fold(0.0f64, |m, c| m.max(c.abs()));
    if max < 3.051944088384301e-5 {
        return [0.0; 3];
    }
    let scale = max.ceil() as u64;
    let decoded = if scale > 3 {
        (scale & 3) | (u64::from((scale >> 2) as i32 as u32) << 2)
    } else {
        scale
    };
    let wire = |c: f64| ((c / scale as f64 * 0.5 + 0.5) * 32766.0).round() as u64 & 0x7FFF;
    let unpack = |w: u64| (w as f64).min(32766.0) * 2.0 / 32766.0 - 1.0;
    v.map(|c| unpack(wire(c)) * decoded as f64)
}

/// The client's view of a slot in Click Container: component values are
/// sent as checksums of their serialized form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashedSlot {
    pub item: u32,
    pub count: i32,
    /// `(component type id, checksum)`.
    pub added: Vec<(u32, i32)>,
    pub removed: Vec<u32>,
}

impl HashedSlot {
    /// Whether the client's view agrees with a stack the server holds.
    pub fn matches(slot: Option<&Self>, stack: Option<ItemView<'_>>) -> bool {
        match (slot, stack) {
            (None, None) => true,
            (Some(slot), Some(stack)) => slot.matches_stack(stack),
            _ => false,
        }
    }

    fn matches_stack(&self, stack: ItemView<'_>) -> bool {
        if self.item != stack.item().id() || self.count != stack.count() as i32 {
            return false;
        }
        let mut removed: Vec<u32> = stack
            .components
            .network_removed()
            .into_iter()
            .map(|kind| kind.id())
            .collect();
        removed.sort_unstable();
        let mut claimed = self.removed.clone();
        claimed.sort_unstable();
        claimed.dedup();
        if removed != claimed {
            return false;
        }
        let added = stack.components.network_added();
        added.len() == self.added.len()
            && added.iter().all(|component| {
                let id = component.kind().id();
                self.added
                    .iter()
                    .any(|(kind, hash)| *kind == id && *hash == component.hash())
            })
    }
}

/// A full Slot sent by a creative-mode client. Unlike a clientbound Slot,
/// each added component's payload is prefixed with its byte length, so a
/// component type this server cannot decode is skipped rather than making
/// the rest of the packet unreadable.
/// The stack is a picture of what the client asks for; the session makes
/// it.
#[derive(Clone, Debug, PartialEq)]
pub enum CreativeSlot {
    Empty,
    Stack(tuff_item::item::ItemSnapshot),
    /// A stack this server cannot hold: a component type it cannot decode.
    Unsupported,
    /// A stack no client holds: an unknown item, or a count above the
    /// item's stack limit.
    Invalid,
}

pub trait WriteExt {
    fn write_position(&mut self, pos: BlockPos);
    fn write_angle(&mut self, degrees: f32);
    fn write_lp_vec3(&mut self, v: [f64; 3]);
    fn write_slot<M: Mode>(&mut self, stack: Option<ItemView<'_, M>>);
    fn write_any_slot(&mut self, stack: Option<tuff_item::item::AnyItem<'_>>);
    /// A slot as players other than the holder see it: container, bundle
    /// and book contents and any oversized component are left out.
    fn write_viewer_slot<M: Mode>(&mut self, stack: Option<ItemView<'_, M>>);
    fn write_viewer_any_slot(&mut self, stack: Option<tuff_item::item::AnyItem<'_>>);
    /// A viewer slot that also drops durability, enchantments and custom
    /// data, keeping only the enchantment glint.
    fn write_obfuscated_viewer_slot<M: Mode>(&mut self, stack: Option<ItemView<'_, M>>);
    fn write_obfuscated_viewer_any_slot(&mut self, stack: Option<tuff_item::item::AnyItem<'_>>);
    /// A non-empty stack as item, count and component patch, with no Slot
    /// count prefix (recipe book stack displays, container contents).
    fn write_item_template<M: Mode>(&mut self, stack: ItemView<'_, M>);
    fn write_component<M: Mode>(&mut self, component: &NetworkComponent<'_, M>);
    fn write_text(&mut self, text: &TextComponent);
    fn write_effect_details(&mut self, details: &EffectDetails);
    fn write_bitset(&mut self, words: &[u64]);
    fn write_explosion(&mut self, explosion: &FireworkExplosion);
    fn write_filterable_string(&mut self, value: &Filterable<String>);
    /// An ID Set given inline: every id listed, no tag reference.
    fn write_id_list(&mut self, ids: &[u32]);
}

pub trait ReadExt {
    fn read_position(&mut self) -> Result<BlockPos>;
    fn read_lp_vec3(&mut self) -> Result<[f64; 3]>;
    fn read_hashed_slot(&mut self) -> Result<Option<HashedSlot>>;
    fn read_creative_slot(&mut self) -> Result<CreativeSlot>;
    /// Reads one component's payload into `components`; `Ok(false)` for a
    /// type this server cannot decode, after which the reader's position is
    /// meaningless.
    fn read_component(
        &mut self,
        kind: DataComponentType,
        components: &mut Components<Picture>,
    ) -> Result<bool>;
    /// Reads a component nested `depth` container items deep.
    fn read_component_at(
        &mut self,
        kind: DataComponentType,
        components: &mut Components<Picture>,
        depth: u32,
    ) -> Result<bool>;
    /// A `container` component's stacks; `None` if one of them holds a
    /// component this server cannot decode.
    fn read_container(&mut self, depth: u32) -> Result<Option<Vec<Option<ItemPicture>>>>;
    /// A `charged_projectiles` component's stacks; `None` if one of them
    /// holds a component this server cannot keep.
    fn read_charged_projectiles(&mut self, depth: u32) -> Result<Option<Vec<ItemPicture>>>;
    /// One item template (id, count, patch); `None` for one this server
    /// cannot keep.
    /// A `bundle_contents` component's stacks; `None` if one of them holds
    /// a component this server cannot decode.
    fn read_bundle(&mut self, depth: u32) -> Result<Option<BundleContents<Picture>>>;
    /// An item, count and component patch with no Slot count prefix.
    fn read_item_template(&mut self, depth: u32) -> Result<Option<ItemPicture>>;
    fn read_effect_details(&mut self, depth: u32) -> Result<EffectDetails>;
    fn read_explosion(&mut self) -> Result<FireworkExplosion>;
    fn read_filterable_string(&mut self, max_chars: usize) -> Result<Filterable<String>>;
}

mod read;
#[cfg(test)]
mod tests;
mod write;
