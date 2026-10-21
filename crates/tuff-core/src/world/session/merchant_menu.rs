//! Merchant trading: offers, updates and trade selection.

use super::*;
use crate::world::menu;

impl PlayerSession {
    pub fn send_merchant_offers(&mut self) {
        let Some(menu) = &self.menus.menu else {
            return;
        };
        let Some(Station::Merchant { view, .. }) = &menu.station else {
            return;
        };
        if view.offers.is_empty() {
            return;
        }
        let packet = framed(&play::MerchantOffers {
            window_id: menu.window_id,
            offers: &view.offers,
            level: view.level,
            xp: view.xp,
            regular: view.regular,
            can_restock: view.can_restock,
        });
        self.send_raw(packet);
    }

    /// The merchant stopped trading: its screen closes if it is open.
    pub async fn merchant_closed(&mut self, merchant: i32) {
        let open = self
            .menus
            .menu
            .as_ref()
            .is_some_and(|menu| menu.backing == MenuBacking::Merchant(merchant));
        if open {
            self.close_menu(true).await;
        }
    }

    /// The merchant's offers or level changed while its screen is open.
    pub fn merchant_updated(&mut self, update: MerchantView) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        if menu.backing != MenuBacking::Merchant(update.merchant) || menu.pending.is_some() {
            return;
        }
        if let Some(Station::Merchant { view, .. }) = &mut menu.station {
            *view = update;
        }
        self.send_merchant_offers();
        self.refresh_station();
    }

    /// Select Trade: the payment slots are refilled for the chosen offer.
    pub fn select_trade(&mut self, index: i32) {
        let Some(menu) = self.menus.menu.as_mut() else {
            return;
        };
        if menu.closing.is_some() || menu.pending.is_some() {
            return;
        }
        let Some(Station::Merchant { view, hint, .. }) = &mut menu.station else {
            return;
        };
        let Some(index) = usize::try_from(index).ok() else {
            return;
        };
        *hint = index;
        if let Some(offer) = view.offers.get(index) {
            menu::merchant_move_items(&mut menu.container, &mut self.inventory, offer);
        }
        self.refresh_station();
        self.send_menu_content();
    }

    pub async fn stop_trading(&mut self, merchant: i32) {
        let Some((_, chunk, _)) = self.view.entities.visible_entity(merchant) else {
            return;
        };
        if let Some(region) = self.region(chunk.region()).await {
            region
                .request(RegionRequest::StopTrading {
                    merchant,
                    player: self.profile.id,
                })
                .await;
        }
    }
}
