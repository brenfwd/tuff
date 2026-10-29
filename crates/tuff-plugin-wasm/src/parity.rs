//! What a built-in plugin can register through `LoadContext` and how a
//! WASM plugin reaches the same surface. Every registration kind core
//! offers (each `Registries` field, and each registration method of the
//! registries that have several) has an entry: the WIT items that cover it,
//! a waiver saying why WASM plugins deliberately do without it, or the
//! plan that adds it. The test reads core's sources, so a new
//! registration kind fails it until it is mapped here.

/// How WASM plugins reach one registration kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// Reached through these WIT items: `interface.function` for imports
    /// and exports, `event.<kind>` for event kinds.
    Bound(&'static [&'static str]),
    /// Deliberately not offered to WASM plugins, and why.
    Waived(&'static str),
    /// Not offered yet; the plan that adds it and what is missing.
    Planned {
        plan: &'static str,
        gap: &'static str,
    },
}

use Coverage::{Bound, Waived};

const SINGLE_OWNER: &str = "a single-owner hook slot the vanilla plugins fill once; a second \
     owner would replace vanilla behaviour wholesale, and its calls run synchronously on region \
     ticks, where guest code must never run";

/// Every registration kind, by `Registries` field or `field.method`.
pub const SURFACES: &[(&str, Coverage)] = &[
    ("block_behaviors", Waived(SINGLE_OWNER)),
    ("growth_behaviors", Waived(SINGLE_OWNER)),
    ("block_loot", Waived(SINGLE_OWNER)),
    ("tnt", Waived(SINGLE_OWNER)),
    ("dispensers", Waived(SINGLE_OWNER)),
    ("block_entity_carrier", Waived(SINGLE_OWNER)),
    ("block_entity_tickers", Waived(SINGLE_OWNER)),
    ("item_ports", Waived(SINGLE_OWNER)),
    ("fluid_behaviors", Waived(SINGLE_OWNER)),
    ("data_recipes", Waived(SINGLE_OWNER)),
    (
        "generators",
        Waived(
            "the single-owner generator of the vanilla dimensions; plugin dimensions generate \
             through dimension_providers",
        ),
    ),
    ("player_damage", Waived(SINGLE_OWNER)),
    ("fake_motion", Waived(SINGLE_OWNER)),
    ("anti_xray", Waived(SINGLE_OWNER)),
    ("advancements", Waived(SINGLE_OWNER)),
    ("station_rules", Waived(SINGLE_OWNER)),
    ("explosion_rules", Waived(SINGLE_OWNER)),
    ("block_rules", Waived(SINGLE_OWNER)),
    (
        "region_hooks",
        Bound(&[
            "host.subscribe",
            "guest.on-event",
            "event.block-break",
            "event.block-place",
            "event.entity-spawn",
            "event.entity-death",
            "event.entity-damage",
            "event.inside-block",
            "host.watch-block",
            "host.watch-region",
        ]),
    ),
    (
        "session_hooks",
        Bound(&[
            "host.subscribe",
            "guest.on-event",
            "event.join",
            "event.leave",
            "event.chat",
            "event.player-hurt",
            "event.player-death",
            "event.fake-hurt",
        ]),
    ),
    (
        "packet_interceptors",
        Bound(&[
            "host.intercept-click",
            "host.intercept-item",
            "host.register-channel",
            "event.payload",
            "host.intercept-inventory",
            "host.intercept-state",
            "event.player-state",
            "event.inventory-click",
            "event.inventory-open",
            "event.inventory-close",
            "event.item-drop",
            "event.item-pickup",
            "event.item-swap",
            "event.item-craft",
            "event.items-taken",
            "event.left-click",
            "event.right-click",
            "host.intercept-entity-interact",
            "host.intercept-command",
            "host.intercept-chat",
            "host.intercept-attack",
            "host.intercept-damage",
            "event.player-attack",
            "event.damage-filtered",
            "host.open-menu",
            "host.update-menu-slot",
            "host.close-menu",
            "event.menu-click",
            "event.menu-close",
            "event.entity-interact",
        ]),
    ),
    ("login_gates", Bound(&["host.refuse-login", "event.login"])),
    (
        "save_normalisers",
        Waived(
            "normalisers run synchronously on the region's save path, where guest code must never \
             run; WASM custom content is core custom content, whose saved identities core keeps \
             frozen when the plugin is gone",
        ),
    ),
    ("recipe_entries", Bound(&["content.declare-content"])),
    (
        "recipe_entries.register",
        Bound(&["content.declare-content"]),
    ),
    (
        "recipe_entries.register_shared",
        Bound(&["content.declare-content"]),
    ),
    (
        "recipe_entries.replace",
        Waived("replacing another plugin's recipe is a vanilla-plugin layering tool"),
    ),
    (
        "recipe_entries.remove",
        Waived("removing another plugin's recipe is a vanilla-plugin layering tool"),
    ),
    (
        "services",
        Bound(&[
            "host.schedule",
            "guest.on-timer",
            "guest.enable",
            "guest.disable",
            "host.provide",
            "host.request",
            "host.notify",
            "host.respond",
            "host.plugin-loaded",
            "guest.on-message",
        ]),
    ),
    ("content_providers", Bound(&["content.declare-content"])),
    (
        "dimension_providers",
        Bound(&["worldgen.declare-dimensions", "worldgen.generate-chunk"]),
    ),
    (
        "commands",
        Bound(&[
            "host.register-command",
            "guest.run-command",
            "guest.suggest",
        ]),
    ),
    (
        "commands.register",
        Bound(&["host.register-command", "guest.run-command"]),
    ),
    (
        "commands.add_tuff_subcommand",
        Bound(&["host.register-tuff-subcommand"]),
    ),
    ("commands.register_alias", Bound(&["host.register-alias"])),
    (
        "commands.add_line_claim",
        Waived(
            "line claims hand raw command lines to a synchronous claimer on the session; plugin \
             commands are trees the host parses, and aliases cover rewriting",
        ),
    ),
    (
        "commands.vanilla",
        Waived("installs the vanilla command set; only the vanilla commands plugin calls it"),
    ),
    (
        "commands.set_text_resolver",
        Waived(
            "the single resolver for text components in commands, filled once by the vanilla \
             commands plugin; it runs synchronously inside command parsing",
        ),
    ),
    ("commands.add_resource", Bound(&["content.declare-content"])),
    (
        "commands.add_custom_item",
        Bound(&["content.declare-content"]),
    ),
    (
        "entity_types",
        Waived(
            "entity loaders are Rust code run on region ticks, where guest code must never run; \
             WASM plugins declare custom entity ids with content.declare-content and drive them \
             with spawn-entity and entity events",
        ),
    ),
    (
        "entity_types.register",
        Waived("binds a vanilla entity type to its Rust loader; vanilla plugins only"),
    ),
    (
        "entity_types.register_as",
        Waived(
            "entity loaders are Rust code run on region ticks, where guest code must never run; \
             WASM plugins declare custom entity ids with content.declare-content and drive them \
             with spawn-entity and entity events",
        ),
    ),
    (
        "entity_types.register_spawn_rule",
        Waived(
            "spawn rules are evaluated synchronously during region spawning, where guest code \
             must never run",
        ),
    ),
    (
        "entity_types.register_bucket_release",
        Waived(
            "bucket releases build Rust entities synchronously while a region pours a bucket, \
             where guest code must never run",
        ),
    ),
    (
        "entity_types.register_launcher",
        Waived(
            "launchers build Rust projectiles synchronously inside entity ticks and sessions; \
             guests spawn projectiles as entities with their saved data instead",
        ),
    ),
    (
        "advancement_triggers",
        Bound(&["host.advancement-grant", "host.advancement-revoke"]),
    ),
    (
        "advancement_packs",
        Waived("whole trees compiled in as data; guests declare advancements individually"),
    ),
    (
        "advancement_definitions",
        Bound(&["content.declare-content"]),
    ),
    (
        "player_saved",
        Waived("runs on join and save under the session; a WASM binding is a follow-up"),
    ),
    (
        "config_keys",
        Bound(&[
            "host.declare-config",
            "host.config-get",
            "host.config-set",
            "event.config-change",
        ]),
    ),
    (
        "http_routes",
        Bound(&["capability-events.on-io", "net.respond"]),
    ),
    ("resource_packs", Bound(&["host.register-resource-pack"])),
    (
        "public_ip_endpoints",
        Waived("server operations setting, not plugin behaviour"),
    ),
    (
        "panel",
        Bound(&[
            "host.register-panel-page",
            "host.register-panel-api",
            "host.register-panel-card",
        ]),
    ),
    ("panel.register_page", Bound(&["host.register-panel-page"])),
    (
        "panel.register_api",
        Bound(&["host.register-panel-api", "capability-events.on-io"]),
    ),
    (
        "panel.register_card",
        Bound(&["host.register-panel-card", "host.panel-card-set"]),
    ),
    (
        "permissions",
        Bound(&["host.declare-permission", "host.has-permission"]),
    ),
    ("permissions.register", Bound(&["host.declare-permission"])),
    (
        "permissions.register_commands",
        Waived("core derives command nodes from the command tree at freeze"),
    ),
    ("custom", Bound(&["content.declare-content"])),
    ("custom.register_item", Bound(&["content.declare-content"])),
    (
        "custom.register_entity",
        Bound(&["content.declare-content"]),
    ),
    (
        "custom.register_assets",
        Bound(&["content.declare-content"]),
    ),
    (
        "custom.resolve",
        Waived("core resolves custom content itself after loading"),
    ),
    (
        "ext",
        Waived(
            "type-keyed registries Rust plugins share by Rust type; nothing to name across the \
             WASM boundary",
        ),
    ),
    (
        "hook_owners",
        Waived("bookkeeping of which plugin filled each slot, not a registration"),
    ),
];

/// Registries whose registration methods are listed one by one, as
/// `(field, source file, impl header)`.
#[cfg(test)]
const METHODS: &[(&str, &str, &str)] = &[
    (
        "commands",
        include_str!("../../tuff-core/src/world/commands/registry.rs"),
        "impl Registry {",
    ),
    (
        "custom",
        include_str!("../../tuff-core/src/custom.rs"),
        "impl Definitions {",
    ),
    (
        "panel",
        include_str!("../../tuff-core/src/panel_host/panel.rs"),
        "impl Panel {",
    ),
    (
        "permissions",
        include_str!("../../tuff-core/src/permission/mod.rs"),
        "impl NodeRegistry {",
    ),
    (
        "entity_types",
        include_str!("../../tuff-core/src/world/entity/registry.rs"),
        "impl Registry {",
    ),
    (
        "recipe_entries",
        include_str!("../../tuff-core/src/world/crafting/mod.rs"),
        "impl Registry {",
    ),
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::wit::bindings::{EXPORTS, IMPORTS};

    /// The `pub` fields of `struct Registries`.
    fn registries_fields() -> Vec<String> {
        let source = include_str!("../../tuff-core/src/registry.rs");
        let start = source
            .find("pub struct Registries {")
            .expect("Registries is defined in registry.rs");
        let body = &source[start..start + source[start..].find("\n}\n").unwrap()];
        body.lines()
            .filter_map(|line| line.trim().strip_prefix("pub "))
            .filter_map(|rest| rest.split_once(':'))
            .map(|(name, _)| name.trim().to_owned())
            .filter(|name| !name.contains(' '))
            .collect()
    }

    /// The `pub fn` taking `&mut self` in the impl block opened by
    /// `header`, except `freeze`.
    fn registration_methods(source: &str, header: &str) -> Vec<String> {
        let start = source
            .find(header)
            .unwrap_or_else(|| panic!("{header} not found"));
        let body = &source[start..start + source[start..].find("\n}\n").unwrap()];
        let mut names = Vec::new();
        let mut rest = body;
        while let Some(at) = rest.find("\n    pub fn ") {
            rest = &rest[at + "\n    pub fn ".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let signature = &rest[..rest.find(')').unwrap_or(rest.len())];
            if signature.contains("&mut self") && name != "freeze" {
                names.push(name);
            }
        }
        names
    }

    fn registration_kinds() -> BTreeSet<String> {
        let mut kinds: BTreeSet<String> = registries_fields().into_iter().collect();
        for (field, source, header) in METHODS {
            assert!(kinds.contains(*field), "{field} is not a Registries field");
            for method in registration_methods(source, header) {
                kinds.insert(format!("{field}.{method}"));
            }
        }
        kinds
    }

    /// Every WIT name a binding may cite.
    fn wit_names() -> BTreeSet<String> {
        let interface = |module: &str| {
            let tail = module.rsplit('/').next().unwrap_or(module);
            tail.split('@').next().unwrap_or(tail).to_owned()
        };
        let mut names = BTreeSet::new();
        for (module, field) in IMPORTS {
            names.insert(format!("{}.{field}", interface(module)));
        }
        for export in EXPORTS {
            if let Some((module, field)) = export.split_once('#') {
                names.insert(format!("{}.{field}", interface(module)));
            }
        }
        let wit = include_str!("../wit/plugin.wit");
        let start = wit
            .find("flags event-kinds {")
            .expect("event-kinds is defined");
        let block = &wit[start..start + wit[start..].find('}').unwrap()];
        for line in block.lines().skip(1) {
            let kind = line.trim().trim_end_matches(',');
            if !kind.is_empty() {
                names.insert(format!("event.{kind}"));
            }
        }
        names
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_registration_kind_is_mapped() {
        let kinds = registration_kinds();
        assert!(kinds.contains("commands") && kinds.contains("custom.register_item"));
        let mapped: BTreeSet<String> = SURFACES.iter().map(|(k, _)| (*k).to_owned()).collect();
        assert_eq!(mapped.len(), SURFACES.len(), "a kind is mapped twice");
        let unmapped: Vec<&String> = kinds.difference(&mapped).collect();
        assert!(
            unmapped.is_empty(),
            "registration kinds with no WASM binding or waiver in parity.rs: {unmapped:?}"
        );
        let stale: Vec<&String> = mapped.difference(&kinds).collect();
        assert!(
            stale.is_empty(),
            "parity.rs maps kinds core no longer has: {stale:?}"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn bindings_name_real_wit_items_and_waivers_say_why() {
        let names = wit_names();
        assert!(names.contains("host.log") && names.contains("guest.register"));
        assert!(names.contains("event.join") && names.contains("worldgen.generate-chunk"));
        for (kind, coverage) in SURFACES {
            match coverage {
                Bound(items) => {
                    assert!(!items.is_empty(), "{kind} is bound to nothing");
                    for item in *items {
                        assert!(
                            names.contains(*item),
                            "{kind} cites unknown WIT item {item}"
                        );
                    }
                }
                Waived(why) => assert!(why.len() > 20, "{kind} needs a real reason"),
                Coverage::Planned { plan, gap } => {
                    assert!(
                        !plan.is_empty() && !gap.is_empty(),
                        "{kind} needs a plan and a gap"
                    )
                }
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_import_serves_some_surface_or_is_general_purpose() {
        let bound: BTreeSet<&str> = SURFACES
            .iter()
            .filter_map(|(_, coverage)| match coverage {
                Bound(items) => Some(items.iter().copied()),
                _ => None,
            })
            .flatten()
            .collect();
        let registering: Vec<String> = wit_names()
            .into_iter()
            .filter(|name| {
                name.starts_with("host.register-")
                    || name.starts_with("host.declare-")
                    || name == "host.subscribe"
                    || name == "host.intercept-click"
                    || name == "host.intercept-item"
                    || name == "host.register-channel"
                    || name == "host.intercept-inventory"
                    || name == "host.intercept-state"
                    || name == "host.intercept-entity-interact"
                    || name == "host.intercept-command"
                    || name == "host.intercept-chat"
                    || name == "host.intercept-attack"
                    || name == "host.intercept-damage"
                    || name == "host.watch-block"
                    || name == "host.watch-region"
            })
            .filter(|name| !bound.contains(name.as_str()))
            .collect();
        assert!(
            registering.is_empty(),
            "registration imports missing from the parity table: {registering:?}"
        );
    }
}
