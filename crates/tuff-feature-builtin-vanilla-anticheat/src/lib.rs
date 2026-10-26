//! Server-side checks on what players do. Every serverbound packet passes
//! the interceptor, which asks each [`checks::Check`] about it; a flagged
//! packet is set back (cancelled or corrected, with the true state resent)
//! and adds to that check's violation level. A kick follows only when a
//! level climbs past its limit before decaying; nothing here bans.
//! Operators are exempt unless the configuration says otherwise; anyone
//! else through the [`permissions`] nodes. Staff inspect and reset
//! violations with `/anticheat`, and other features subscribe to them on
//! the world's [`service::Board`].

#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "test code is exempt from the door gate"
    )
)]

pub mod anchor;
pub mod checks;
pub mod clock;
pub mod command;
pub mod config;
pub mod history;
pub mod improbable;
pub mod interceptor;
pub mod login;
pub mod panel;
pub mod permissions;
pub mod service;
pub mod trace;
pub mod violations;

use std::sync::Arc;

use tuff_core::feature::{Feature, LoadContext};
use tuff_core::world::session::hooks::Interceptor;
use tuff_item::load::{LoadError, Phase};
use tuff_web_ui::panel::MetricCard;

const ID: &str = "vanilla-anticheat";
const ORDER: i32 = -1000;

pub struct AnticheatFeature;

impl Feature for AnticheatFeature {
    fn id(&self) -> &'static str {
        ID
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            let registries = cx.registries();
            registries.config_keys.extend(config::keys());
            registries.config_keys.extend(login::keys());
            registries.login_gates.push(Arc::new(login::ThrottleGates));
            permissions::register(&mut registries.permissions, ID)?;
            registries
                .commands
                .register(command::root(), command::Anticheat);
            registries.services.push(Arc::new(service::BoardFactory));
            registries.panel.register_card(
                ID,
                MetricCard::new(service::FLAGS_SERIES, "Anticheat flags"),
            )?;
            registries.panel.register_card(
                ID,
                MetricCard::new(service::KICKS_SERIES, "Anticheat kicks"),
            )?;
            panel::register(registries, ID)?;
            registries.packet_interceptors.push(Interceptor {
                order: ORDER,
                hook: Arc::new(interceptor::ActionInterceptor),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn the_dashboard_shows_flag_and_kick_totals() {
        let features: Vec<Box<dyn Feature>> = vec![Box::new(AnticheatFeature)];
        let mut registries = tuff_core::registry::Registries::new();
        tuff_core::feature::load_all(&features, &mut registries).unwrap();
        let cards = registries.panel.installed().cards;
        assert!(cards.contains_key(service::FLAGS_SERIES));
        assert!(cards.contains_key(service::KICKS_SERIES));
    }
}
