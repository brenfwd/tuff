use tuff_core::protocol::play::Serverbound;

use super::{Check, Facts, Flag};

const NAME: &str = "inventory";
const PLAYER_MENU_SLOTS: i64 = 46;
const PLAYER_SLOTS_IN_MENU: i64 = 36;
const OUTSIDE: i16 = -999;
const MAX_CLICK_MODE: i32 = 6;

/// Slot, window, hand and index values that name something that cannot
/// exist, and clicks on a window the server does not have open.
pub struct InventoryValidity;

impl Check for InventoryValidity {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        match packet {
            Serverbound::SetCarriedItem(slot) if !(0..=8).contains(slot) => {
                Flag::cancel(NAME, 10.0)
            }
            Serverbound::UseItemOn { hand, .. } if !(0..=1).contains(hand) => {
                Flag::cancel(NAME, 10.0)
            }
            Serverbound::SelectTrade(index) if *index < 0 => Flag::cancel(NAME, 5.0),
            Serverbound::SelectBundleItem { slot, index } if *slot < 0 || *index < -1 => {
                Flag::cancel(NAME, 5.0)
            }
            Serverbound::ContainerClick {
                window_id,
                slot,
                mode,
                ..
            } => {
                if !(0..=MAX_CLICK_MODE).contains(mode) {
                    return Flag::resync(NAME, 5.0);
                }
                let slots = match facts.menu {
                    Some((window, container)) if window == *window_id => {
                        container as i64 + PLAYER_MENU_SLOTS
                    }
                    Some(_) => return Flag::resync(NAME, 0.5),
                    None if *window_id == 0 => PLAYER_MENU_SLOTS,
                    None => return Flag::resync(NAME, 1.0),
                };
                let valid = *slot == OUTSIDE || (0..slots).contains(&i64::from(*slot));
                if valid || slots < PLAYER_SLOTS_IN_MENU {
                    None
                } else {
                    Flag::resync(NAME, 5.0)
                }
            }
            _ => None,
        }
    }
}

/// Creative slot sets by players who are not in creative mode, and slots
/// outside the player menu.
pub struct CreativeSlots;

impl Check for CreativeSlots {
    fn examine(&mut self, facts: &Facts, packet: &Serverbound) -> Option<Flag> {
        let Serverbound::SetCreativeModeSlot { slot, .. } = packet else {
            return None;
        };
        if !facts.instabuild {
            return Flag::resync("creative_slot", 5.0);
        }
        if *slot != -1 && !(0..PLAYER_MENU_SLOTS as i16).contains(slot) {
            return Flag::resync("creative_slot", 5.0);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::super::facts;
    use super::*;

    fn click(window_id: i32, slot: i16, mode: i32) -> Serverbound {
        Serverbound::ContainerClick {
            window_id,
            state_id: 0,
            slot,
            button: 0,
            mode,
            changed: Vec::new(),
            carried: None,
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn valid_clicks_pass() {
        let mut check = InventoryValidity;
        let mut f = facts();
        assert_eq!(check.examine(&f, &click(0, 45, 0)), None);
        assert_eq!(check.examine(&f, &click(0, -999, 4)), None);
        f.menu = Some((3, 27));
        assert_eq!(check.examine(&f, &click(3, 62, 1)), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn nonexistent_slots_and_windows_are_flagged() {
        let mut check = InventoryValidity;
        let mut f = facts();
        assert!(check.examine(&f, &click(0, 46, 0)).is_some());
        assert!(check.examine(&f, &click(0, -5, 0)).is_some());
        assert!(check.examine(&f, &click(0, 5, 9)).is_some());
        assert!(check.examine(&f, &click(4, 5, 0)).is_some());
        f.menu = Some((3, 27));
        assert!(check.examine(&f, &click(3, 200, 0)).is_some());
        assert!(check.examine(&f, &click(2, 1, 0)).is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn hotbar_indices_are_bounded() {
        let mut check = InventoryValidity;
        let f = facts();
        assert_eq!(check.examine(&f, &Serverbound::SetCarriedItem(8)), None);
        assert!(check.examine(&f, &Serverbound::SetCarriedItem(9)).is_some());
        assert!(
            check
                .examine(&f, &Serverbound::SetCarriedItem(-1))
                .is_some()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn creative_sets_need_creative_mode() {
        let set = Serverbound::SetCreativeModeSlot {
            slot: 36,
            stack: tuff_core::protocol::codec::CreativeSlot::Empty,
        };
        let mut f = facts();
        assert!(CreativeSlots.examine(&f, &set).is_some());
        f.instabuild = true;
        assert_eq!(CreativeSlots.examine(&f, &set), None);
    }
}
