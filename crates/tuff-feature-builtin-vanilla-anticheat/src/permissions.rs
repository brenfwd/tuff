//! The anti-cheat's permission nodes and the exemptions they grant.
//! Exemptions come only from explicit grants (the nodes have no operator
//! bypass; operators are exempted by `tuff.anticheat.exemptOperators`),
//! so a player without settings costs nothing to resolve.

use tuff_core::permission::{Node, NodeRegistry, Subject, View};
use tuff_item::load::LoadError;
use tuff_storage::admin::PermissionLevel;

use crate::checks;

pub const ALERTS: &str = "tuff.anticheat.alerts";
pub const BYPASS: &str = "tuff.anticheat.bypass";
pub const LOGIN: &str = "tuff.anticheat.bypass.login";
pub const VIOLATIONS: &str = "tuff.anticheat.command.violations";
pub const RESET: &str = "tuff.anticheat.command.reset";
pub const DEBUG: &str = "tuff.anticheat.command.debug";
const EXEMPT_PREFIX: &str = "tuff.anticheat.exempt.";

pub fn exempt_node(check: &str) -> String {
    format!("{EXEMPT_PREFIX}{check}")
}

pub fn register(registry: &mut NodeRegistry, feature: &'static str) -> Result<(), LoadError> {
    let mut nodes = vec![
        Node::new(
            ALERTS,
            "Hear anti-cheat setbacks and kicks in chat, and toggle them with /anticheat alerts",
        )
        .op_level(PermissionLevel::Moderators),
        Node::new(BYPASS, "Skip every anti-cheat check").op_bypass(false),
        Node::new(LOGIN, "Skip the login and relog throttles").op_bypass(false),
        Node::new(
            VIOLATIONS,
            "List players' anti-cheat violations with /anticheat violations",
        )
        .op_level(PermissionLevel::Gamemasters),
        Node::new(
            RESET,
            "Clear a player's anti-cheat violation levels with /anticheat reset",
        )
        .op_level(PermissionLevel::Gamemasters),
        Node::new(
            DEBUG,
            "Trace a player's anti-cheat checks (allowed and actual values) with /anticheat debug",
        )
        .op_level(PermissionLevel::Gamemasters),
    ];
    for info in checks::INFOS {
        nodes.push(
            Node::new(
                exempt_node(info.name),
                format!("Skip the {} anti-cheat check", info.name),
            )
            .op_bypass(false),
        );
    }
    for node in nodes {
        registry.register(feature, node)?;
    }
    Ok(())
}

/// The checks one player is exempt from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Exemptions {
    pub all: bool,
    pub checks: Vec<&'static str>,
}

impl Exemptions {
    pub fn of(view: &View, uuid: u128, level: PermissionLevel) -> Exemptions {
        if view
            .player(uuid)
            .is_none_or(|grants| grants.nodes.is_empty())
        {
            return Exemptions::default();
        }
        let subject = Subject::Player { uuid, level };
        if view.allows(subject, BYPASS) {
            return Exemptions {
                all: true,
                checks: Vec::new(),
            };
        }
        Exemptions {
            all: false,
            checks: checks::INFOS
                .iter()
                .filter(|info| view.allows(subject, &exempt_node(info.name)))
                .map(|info| info.name)
                .collect(),
        }
    }

    pub fn covers(&self, check: &str) -> bool {
        self.all || self.checks.contains(&check)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tuff_core::permission::{Grants, PlayerGrants};

    fn view(grants: &[(&str, bool)]) -> View {
        let mut registry = NodeRegistry::new();
        register(&mut registry, "test").unwrap();
        let mut player = PlayerGrants::default();
        for (pattern, held) in grants {
            player.nodes.insert((*pattern).to_owned(), *held);
        }
        let mut all = Grants::default();
        all.players.insert(1, player);
        View::new(registry.nodes(), Arc::new(all))
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_explicit_grants_exempt_even_owners() {
        let owner = Exemptions::of(&view(&[]), 1, PermissionLevel::Owners);
        assert_eq!(owner, Exemptions::default());
        let one = Exemptions::of(
            &view(&[("tuff.anticheat.exempt.reach", true)]),
            1,
            PermissionLevel::All,
        );
        assert!(one.covers("reach") && !one.covers("sight"));
        let wild = Exemptions::of(
            &view(&[("tuff.anticheat.exempt.*", true)]),
            1,
            PermissionLevel::All,
        );
        assert!(wild.covers("reach") && wild.covers("sight") && !wild.all);
        let bypass = Exemptions::of(&view(&[(BYPASS, true)]), 1, PermissionLevel::All);
        assert!(bypass.all);
    }

    #[tuff_core::watchdog]
    #[test]
    fn alerts_reach_operators_by_default() {
        let view = view(&[]);
        let op = Subject::Player {
            uuid: 2,
            level: PermissionLevel::Moderators,
        };
        let player = Subject::Player {
            uuid: 2,
            level: PermissionLevel::All,
        };
        assert!(view.allows(op, ALERTS));
        assert!(!view.allows(player, ALERTS));
        assert!(!view.allows(op, VIOLATIONS));
    }
}
