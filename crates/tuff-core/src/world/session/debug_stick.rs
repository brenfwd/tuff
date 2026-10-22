//! The debug stick: selecting and cycling block state properties.

use super::*;
use crate::protocol::play;
use crate::world::messages::RegionRequest;
use tokio::sync::oneshot;
use tuff_item::item::ItemStack;
use tuff_nbt::Tag;
use tuff_storage::admin::PermissionLevel;
use tuff_vanilla_data::{BlockState, Item};
use tuff_world_model::BlockPos;

const STATE: &str = "minecraft:debug_stick_state";
const KEY: &str = "item.minecraft.debug_stick";

fn relative<T: PartialEq + Copy>(items: &[T], current: Option<T>, backward: bool) -> Option<T> {
    let last = items.len().checked_sub(1)?;
    let at = current.and_then(|c| items.iter().position(|i| *i == c));
    let next = match (at, backward) {
        (None, false) => 0,
        (None, true) => last,
        (Some(0), true) => last,
        (Some(i), true) => i - 1,
        (Some(i), false) if i == last => 0,
        (Some(i), false) => i + 1,
    };
    Some(items[next])
}

fn selected(stack: &ItemStack, state: BlockState) -> Option<String> {
    match stack
        .components
        .other
        .get(STATE)?
        .as_compound()?
        .get(state.block().name())?
    {
        Tag::String(name) => Some(name.clone()),
        _ => None,
    }
}

fn select_property(
    state: BlockState,
    current: Option<&str>,
    backward: bool,
) -> Option<&'static str> {
    let names: Vec<&'static str> = state.properties().map(|(name, _)| name).collect();
    let current = current.and_then(|c| names.iter().copied().find(|n| *n == c));
    relative(&names, current, backward)
}

fn cycle_value(
    state: BlockState,
    property: &str,
    backward: bool,
) -> Option<(BlockState, &'static str)> {
    let values = state.property_values(property)?;
    let value = relative(values, state.get(property), backward)?;
    Some((state.with(property, value)?, value))
}

fn store(stack: &mut ItemStack, state: BlockState, property: &str) {
    let other = &mut stack.components.other;
    if !matches!(other.get(STATE), Some(Tag::Compound(_))) {
        other.insert(STATE, tuff_nbt::Compound::new());
    }
    if let Some(map) = other.get_compound_mut(STATE) {
        map.insert(state.block().name(), property);
    }
}

impl PlayerSession {
    pub(super) fn holds_debug_stick(&self, off_hand: bool) -> bool {
        self.inventory
            .hand(off_hand)
            .is_some_and(|stack| stack.item() == Item::DebugStick)
    }

    fn overlay_translation(&mut self, key: String, args: impl IntoIterator<Item = TextComponent>) {
        let content = TextComponent::translate(key, args);
        self.send(&play::SystemChat {
            content: &content,
            overlay: true,
        });
    }

    /// A debug stick click on `pos`: a left click (`cycle` false) selects the
    /// next property, a right click cycles its value; sneaking reverses.
    pub(super) async fn debug_stick_interact(
        &mut self,
        off_hand: bool,
        pos: BlockPos,
        cycle: bool,
    ) {
        if self.mode.permission < PermissionLevel::Gamemasters {
            return;
        }
        let Some(region) = self.region_of(pos) else {
            return;
        };
        let (reply, answer) = oneshot::channel();
        region.request(RegionRequest::BlockAt { pos, reply }).await;
        let Ok(Some(state)) = answer.await else {
            return;
        };
        let backward = self.pose.sneaking;
        let name = state.block().name();
        if state.properties().len() == 0 {
            self.overlay_translation(
                format!("{KEY}.empty"),
                [TextComponent::new(name.to_owned())],
            );
            return;
        }
        let Some(stack) = self.inventory.hand(off_hand) else {
            return;
        };
        let current = selected(stack, state);
        if cycle {
            let property = match current.as_deref() {
                Some(property) if state.get(property).is_some() => property.to_owned(),
                _ => match select_property(state, None, false) {
                    Some(first) => first.to_owned(),
                    None => return,
                },
            };
            let Some((next, value)) = cycle_value(state, &property, backward) else {
                return;
            };
            region
                .request(RegionRequest::DebugStickSet { pos, state: next })
                .await;
            self.overlay_translation(
                format!("{KEY}.update"),
                [TextComponent::new(property), TextComponent::new(value)],
            );
        } else {
            let Some(property) = select_property(state, current.as_deref(), backward) else {
                return;
            };
            let value = state.get(property).unwrap_or_default();
            if let Some(stack) = self.inventory.hand_mut(off_hand) {
                store(stack, state, property);
            }
            self.send_inventory();
            self.overlay_translation(
                format!("{KEY}.select"),
                [TextComponent::new(property), TextComponent::new(value)],
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_item::item::MintReason;
    use tuff_vanilla_data::Block;

    #[tuff_core::watchdog]
    #[test]
    fn selection_wraps_both_ways_and_starts_at_the_ends() {
        let state = Block::OakStairs.default_state();
        let names: Vec<_> = state.properties().map(|(n, _)| n).collect();
        assert_eq!(select_property(state, None, false), names.first().copied());
        assert_eq!(select_property(state, None, true), names.last().copied());
        let last = *names.last().unwrap();
        assert_eq!(
            select_property(state, Some(last), false),
            names.first().copied()
        );
        assert_eq!(select_property(state, Some(names[0]), true), Some(last));
        assert_eq!(
            select_property(state, Some(names[0]), false),
            Some(names[1])
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn cycling_steps_the_value_forward_and_back() {
        let state = Block::OakStairs.default_state();
        let (forward, value) = cycle_value(state, "half", false).unwrap();
        assert_eq!(forward.get("half"), Some(value));
        assert_ne!(forward, state);
        let (back, _) = cycle_value(forward, "half", true).unwrap();
        assert_eq!(back, state);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_selection_is_stored_per_block_on_the_stack() {
        let mut stack = ItemStack::mint(Item::DebugStick, 1, MintReason::Test);
        let stairs = Block::OakStairs.default_state();
        let stone = Block::Stone.default_state();
        assert_eq!(selected(&stack, stairs), None);
        store(&mut stack, stairs, "half");
        assert_eq!(selected(&stack, stairs).as_deref(), Some("half"));
        assert_eq!(selected(&stack, stone), None);
        store(&mut stack, stairs, "shape");
        assert_eq!(selected(&stack, stairs).as_deref(), Some("shape"));
    }
}
