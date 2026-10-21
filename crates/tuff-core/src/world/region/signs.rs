//! Sign interactions: dyes, ink sacs and honeycomb used on signs, the sign
//! editor, and the text it sends back.

use tuff_vanilla_data::registry::{Attribute, BlockEntityType, EntityType, SoundEvent};
use tuff_vanilla_data::{BlockState, Item};

use super::{RegionActor, framed, packets};
use crate::protocol::play;
use crate::world::EYE_HEIGHT;
use crate::world::block_entity::raw;
use crate::world::block_entity::sign::{self, Side};
use crate::world::messages::{ActionOutcome, HandEffect, Place, PlayerEvent, PlayerHandle};
use tuff_item::text::TextComponent;
use tuff_nbt::Compound;
use tuff_world_model::BlockPos;
use tuff_world_model::pos::ChunkPos;

/// How much farther than their block reach an editing player may be.
const EDIT_RANGE_BUFFER: f64 = 4.0;

/// Whether `eye` is close enough to the block at `pos` to edit it.
fn within_edit_range(pos: BlockPos, eye: [f64; 3]) -> bool {
    let reach = EntityType::Player
        .base_attribute(Attribute::BlockInteractionRange)
        .unwrap_or(0.0)
        + EDIT_RANGE_BUFFER;
    let min = [pos.x as f64, pos.y as f64, pos.z as f64];
    let distance_sq: f64 = (0..3)
        .map(|axis| {
            let d = (min[axis] - eye[axis])
                .max(eye[axis] - (min[axis] + 1.0))
                .max(0.0);
            d * d
        })
        .sum();
    distance_sq < reach * reach
}

impl RegionActor {
    /// A click on a sign: an applicator item changes the side the player
    /// faces, else the editor opens (or a waxed sign refuses). `None` lets
    /// the click go on to the held item.
    pub(super) fn use_sign(&mut self, place: &Place, state: BlockState) -> Option<ActionOutcome> {
        let pos = place.target;
        let kind = self.block_entity_kind(pos).filter(|k| sign::is_sign(*k))?;
        let nbt = self.raw_block_entity(pos)?.clone();
        let side = sign::side_facing(state, pos, place.position);
        let waxed = sign::is_waxed(&nbt);
        let other_editor = self
            .menus
            .sign_editors
            .get(&pos)
            .is_some_and(|editor| editor.id != place.player.id);
        if let Some(item) = place.held {
            if self.chains_hanging_sign(place, state, kind, &nbt, side, item) {
                return None;
            }
            let applies = item.is_sign_applicator()
                && place.may_build
                && !waxed
                && !other_editor
                && (item == Item::Honeycomb || sign::has_message(&nbt, side));
            if applies && self.apply_to_sign(pos, kind, side, item) {
                if let Some(applied) = self.raw_block_entity(pos).cloned() {
                    self.run_sign_click_actions(place, pos, &applied, side);
                }
                return Some(ActionOutcome::Used {
                    off_hand: place.off_hand,
                    item,
                    effect: HandEffect::Consume(1),
                });
            }
        }
        if place.off_hand {
            return None;
        }
        let interacted = Some(ActionOutcome::Interacted {
            off_hand: place.off_hand,
        });
        let click_action = self.run_sign_click_actions(place, pos, &nbt, side);
        if waxed {
            let sound = match kind {
                BlockEntityType::HangingSign => SoundEvent::BlockHangingSignWaxedInteractFail,
                _ => SoundEvent::BlockSignWaxedInteractFail,
            };
            self.play_sound(sound, pos, 1.0, 1.0, None);
            return interacted;
        }
        if click_action {
            return interacted;
        }
        if !other_editor && place.may_build && sign::is_editable(&nbt, side) {
            self.open_sign_editor(pos, side, &place.player);
            return interacted;
        }
        None
    }

    /// Runs the click events of the side's lines for the clicking player
    /// when the sign allows operator features, else tells the player they
    /// are disabled; true if the side has any. Commands go to the player's
    /// session, inline dialogs open on the player's client; custom actions
    /// have no server-side handler and are consumed without effect.
    fn run_sign_click_actions(
        &self,
        place: &Place,
        pos: BlockPos,
        nbt: &Compound,
        side: Side,
    ) -> bool {
        if !sign::has_click_action(nbt, side) {
            return false;
        }
        if !nbt.get_bool("allow_op_features").unwrap_or(false) {
            place.player.send(PlayerEvent::Overlay(
                TextComponent::translate("sign.click_actions_disabled", []).color("red"),
            ));
            return true;
        }
        let commands = sign::click_commands(nbt, side);
        if !commands.is_empty() {
            place.player.send(PlayerEvent::SignCommands {
                dimension: self.dimension,
                position: [pos.x as f64 + 0.5, pos.y as f64 + 0.5, pos.z as f64 + 0.5],
                commands,
            });
        }
        let dialog = match sign::click_dialogs(nbt, side).into_iter().next() {
            Some(sign::ClickDialog::Inline(dialog)) => crate::dialog::validate(&dialog)
                .is_ok()
                .then(|| framed(&play::ShowDialog { dialog })),
            Some(sign::ClickDialog::Registry(name)) => {
                tuff_vanilla_data::data_registry::DataRegistry::get("minecraft:dialog")
                    .and_then(|registry| registry.entries.binary_search(&name.as_str()).ok())
                    .map(|id| framed(&play::ShowRegistryDialog { id: id as i32 }))
            }
            None => None,
        };
        if let Some(packet) = dialog {
            place.player.send(PlayerEvent::ChunkPackets {
                dimension: self.dimension,
                pos: ChunkPos::of_block(pos),
                packets: packets([packet]),
            });
        }
        true
    }

    /// Whether a hanging sign held against a hanging sign is meant to hang
    /// another below (or beside) it rather than change this one.
    fn chains_hanging_sign(
        &self,
        place: &Place,
        state: BlockState,
        kind: BlockEntityType,
        nbt: &Compound,
        side: Side,
        item: Item,
    ) -> bool {
        let holds_hanging_sign = item
            .block()
            .and_then(|block| block.block_entity())
            .is_some_and(|held| held == BlockEntityType::HangingSign);
        let commands = sign::is_waxed(nbt) && sign::has_click_action(nbt, side);
        if kind != BlockEntityType::HangingSign || !holds_hanging_sign || commands {
            return false;
        }
        let face = crate::world::placement::facing(place.face);
        match state.block().kind() {
            tuff_vanilla_data::BlockKind::CeilingHangingSign => {
                face == tuff_vanilla_data::block::Facing::Down
            }
            tuff_vanilla_data::BlockKind::WallHangingSign => state
                .facing()
                .is_some_and(|facing| facing.axis() != face.axis()),
            _ => false,
        }
    }

    /// Dyes, lights, darkens or waxes one side of a sign; false if it
    /// already was that way.
    fn apply_to_sign(
        &mut self,
        pos: BlockPos,
        kind: BlockEntityType,
        side: Side,
        item: Item,
    ) -> bool {
        let Some(nbt) = self.raw_block_entity(pos).cloned() else {
            return false;
        };
        let mut changed = nbt.clone();
        let (applied, sound) = match item {
            Item::Honeycomb => (sign::wax(&mut changed), SoundEvent::ItemHoneycombWaxOn),
            Item::GlowInkSac => (
                sign::set_glowing(kind, &mut changed, side, true),
                SoundEvent::ItemGlowInkSacUse,
            ),
            Item::InkSac => (
                sign::set_glowing(kind, &mut changed, side, false),
                SoundEvent::ItemInkSacUse,
            ),
            _ => match item.dye() {
                Some(color) => (
                    sign::set_color(kind, &mut changed, side, color.name()),
                    SoundEvent::ItemDyeUse,
                ),
                None => (false, SoundEvent::ItemDyeUse),
            },
        };
        if !applied {
            return false;
        }
        if let Some(stored) = self.raw_block_entity_mut(pos) {
            *stored = changed;
        }
        if item == Item::Honeycomb {
            self.level_event(play::level_event::PARTICLES_WAX_ON, pos, 0, false);
        }
        self.play_sound(sound, pos, 1.0, 1.0, None);
        self.send_block_entity_data(pos);
        true
    }

    /// Lets `player` edit one side of the sign at `pos` and opens their
    /// editor.
    pub(super) fn open_sign_editor(&mut self, pos: BlockPos, side: Side, player: &PlayerHandle) {
        let Some(state) = self.block(pos) else {
            return;
        };
        self.menus.sign_editors.insert(pos, player.clone());
        player.send(PlayerEvent::ChunkPackets {
            dimension: self.dimension,
            pos: ChunkPos::of_block(pos),
            packets: packets([
                framed(&play::BlockUpdate {
                    position: pos,
                    state: state.id(),
                }),
                framed(&play::OpenSignEditor {
                    position: pos,
                    front: side == Side::Front,
                }),
            ]),
        });
    }

    /// Opens the editor on a just placed sign's front, as placing one does.
    pub(super) fn edit_placed_sign(&mut self, pos: BlockPos, player: &PlayerHandle) {
        let editable = self.raw_block_entity(pos).is_some_and(|nbt| {
            raw::kind(nbt).is_some_and(sign::is_sign)
                && !sign::is_waxed(nbt)
                && sign::is_editable(nbt, Side::Front)
        });
        if editable {
            self.open_sign_editor(pos, Side::Front, player);
        }
    }

    /// The lines the editor sent, applied if `player` may still edit the
    /// sign.
    pub(super) fn update_sign(
        &mut self,
        player: &PlayerHandle,
        pos: BlockPos,
        front: bool,
        lines: &[String],
        filtered: Option<&[String]>,
        eye: [f64; 3],
    ) {
        if self
            .menus
            .sign_editors
            .get(&pos)
            .is_none_or(|editor| editor.id != player.id)
        {
            return;
        }
        let Some(kind) = self.block_entity_kind(pos).filter(|k| sign::is_sign(*k)) else {
            return;
        };
        if !within_edit_range(pos, eye) || self.raw_block_entity(pos).is_none_or(sign::is_waxed) {
            self.menus.sign_editors.remove(&pos);
            return;
        }
        let side = if front { Side::Front } else { Side::Back };
        let lines: Vec<String> = lines
            .iter()
            .map(|line| sign::strip_formatting(line))
            .collect();
        let old = self.raw_block_entity(pos).cloned();
        if let Some(nbt) = self.raw_block_entity_mut(pos) {
            sign::set_lines(kind, nbt, side, &lines);
            if let Some(filtered) = filtered {
                sign::set_filtered_lines(kind, nbt, side, filtered);
            }
        }
        if let (Some(old), Some(new), Some(state)) =
            (old, self.raw_block_entity(pos).cloned(), self.block(pos))
        {
            self.sign_edited(crate::world::region_api::SignEdit {
                player: player.id,
                pos,
                state,
                front,
                old,
                new,
            });
        }
        self.menus.sign_editors.remove(&pos);
        self.send_block_entity_data(pos);
    }

    /// Forgets editors who left or walked out of range, as the game does
    /// every tick.
    pub(super) fn tick_sign_editors(&mut self) {
        let _span = tuff_timings::span!("region.sign_editors");
        if self.menus.sign_editors.is_empty() {
            return;
        }
        let presence = &self.ents.presence;
        self.menus.sign_editors.retain(|pos, editor| {
            if !editor.is_connected() {
                return false;
            }
            presence.get(&editor.id).is_none_or(|p| {
                let eye = [p.position[0], p.position[1] + EYE_HEIGHT, p.position[2]];
                within_edit_range(*pos, eye)
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn editors_stay_within_reach_plus_a_buffer() {
        let pos = BlockPos::new(0, 64, 0);
        assert!(within_edit_range(pos, [0.5, 65.6, 8.0]));
        assert!(!within_edit_range(pos, [0.5, 65.6, 9.6]));
    }
}
