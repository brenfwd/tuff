//! The coordinator's item ledger check at each world save.

use super::*;

impl Coordinator {
    /// The per-save ledger check: items lost without a burn or operations
    /// refused since the last save are reported.
    pub(super) fn check_item_ledger(&self) {
        let totals = tuff_item::item::conservation::totals();
        let since = totals.since(self.ledger.replace(totals));
        if since.lost > 0 || since.violations > 0 {
            log::error!(
                "item ledger since the last save: {} minted, {} burned, {} loaded, {} lost \
                 without a burn, {} conservation violations",
                since.minted,
                since.burned,
                since.loaded,
                since.lost,
                since.violations
            );
        } else if cfg!(debug_assertions) {
            log::debug!(
                "item ledger since the last save: {} minted, {} burned, {} loaded",
                since.minted,
                since.burned,
                since.loaded
            );
        }
    }

    pub(super) fn conservation_checks(&self) -> bool {
        self.shared
            .config
            .get_bool(crate::config::ITEMS_CONSERVATION_CHECKS)
            .unwrap_or(true)
    }
}
