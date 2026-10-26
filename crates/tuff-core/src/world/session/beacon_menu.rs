//! Beacon effect selection: the Set Beacon packet and the payment it costs.

use super::*;
use crate::world::beacon;

impl PlayerSession {
    pub async fn set_beacon(&mut self, primary: Option<i32>, secondary: Option<i32>) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        let (MenuKind::Beacon, MenuBacking::Station(pos), Some(Station::Beacon { levels })) =
            (menu.kind, &menu.backing, &menu.station)
        else {
            return;
        };
        if menu.closing.is_some() {
            return;
        }
        let decode = |id: Option<i32>| match id {
            None => Some(None),
            Some(id) => beacon::decode_effect(id.saturating_add(1)).map(Some),
        };
        let effects = decode(primary).zip(decode(secondary));
        let valid = effects.filter(|(primary, secondary)| {
            menu.container[0].is_some() && beacon::validate_effects(*primary, *secondary, *levels)
        });
        let Some((primary, secondary)) = valid else {
            self.kick(TextComponent::translate(
                "multiplayer.disconnect.generic",
                [],
            ));
            return;
        };
        let pos = *pos;
        if let Some(payment) = menu.container[0].take() {
            payment.burn(tuff_item::item::BurnReason::Consumed);
        }
        let region = menu.region.clone();
        self.send_menu_slot(0);
        self.send_menu_data(1, beacon::encode_effect(primary) as i16);
        self.send_menu_data(2, beacon::encode_effect(secondary) as i16);
        region
            .request(RegionRequest::SetBeaconEffects {
                pos,
                primary,
                secondary,
            })
            .await;
    }
}
