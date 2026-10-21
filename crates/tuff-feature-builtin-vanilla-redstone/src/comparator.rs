//! Comparator readings of containers and minecarts.

use tuff_item::item::ItemStack;
use tuff_vanilla_data::BlockState;
use tuff_world_model::BlockPos;

use crate::Reader;

/// How full slots are: 0 when empty, 1 to 15 otherwise.
pub fn fill_signal<'a>(slots: impl IntoIterator<Item = &'a Option<ItemStack>>) -> u8 {
    let mut count = 0usize;
    let mut fullness = 0.0f32;
    for slot in slots {
        count += 1;
        if let Some(stack) = slot {
            fullness += f32::from(stack.count()) / f32::from(stack.max_stack_size().max(1));
        }
    }
    if count == 0 {
        return 0;
    }
    let share = fullness / count as f32;
    (share * 14.0).floor() as u8 + u8::from(share > 0.0)
}

/// A pressed detector rail's reading: the success count of a command
/// block minecart on it, else how full a container minecart on it is.
pub fn detector_rail_signal(level: &dyn Reader, pos: BlockPos, state: BlockState) -> u8 {
    if state.powered() != Some(true) {
        return 0;
    }
    let area = [
        f64::from(pos.x) + 0.2,
        f64::from(pos.y),
        f64::from(pos.z) + 0.2,
        f64::from(pos.x + 1) - 0.2,
        f64::from(pos.y + 1) - 0.2,
        f64::from(pos.z + 1) - 0.2,
    ];
    let carts = level.minecarts_in(area);
    if let Some(count) = carts.iter().find_map(|cart| cart.command_success) {
        return count.clamp(0, 15) as u8;
    }
    carts
        .iter()
        .find_map(|cart| cart.slots)
        .map_or(0, |items| fill_signal(items.iter()))
}
