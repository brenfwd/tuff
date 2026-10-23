//! Built-in features: optional, non-vanilla behaviour compiled into the
//! binary and switched on only by `tuff.features.<id>.enabled` settings.

use tuff_core::config::{Key, Value};
use tuff_core::feature::{Feature, LoadContext};
use tuff_item::load::{LoadError, Phase};

const PREFIX: &str = "tuff.features.";
const SUFFIX: &str = ".enabled";
const VANILLA_PREFIX: &str = "tuff.vanilla.";

pub struct OptionalFeature {
    pub id: &'static str,
    pub summary: &'static str,
    make: fn() -> Box<dyn Feature>,
}

impl OptionalFeature {
    pub fn make(&self) -> Box<dyn Feature> {
        (self.make)()
    }

    pub fn key(&self) -> String {
        enabled_key(self.id)
    }
}

pub fn enabled_key(id: &str) -> String {
    format!("{PREFIX}{id}{SUFFIX}")
}

/// The short name a vanilla feature goes by in settings: its id without the
/// `vanilla-` prefix.
pub fn vanilla_name(id: &str) -> &str {
    id.strip_prefix("vanilla-").unwrap_or(id)
}

pub fn vanilla_enabled_key(id: &str) -> String {
    format!("{VANILLA_PREFIX}{}{SUFFIX}", vanilla_name(id))
}

/// The vanilla feature ids switched off by `overrides`. The last override of
/// a key wins; unknown names and non-boolean values are refused.
pub fn disabled_vanilla_from(overrides: &[(String, String)]) -> Result<Vec<&'static str>, String> {
    let all: Vec<&'static str> = crate::vanilla_features::all()
        .iter()
        .map(|p| p.id())
        .collect();
    let mut off = vec![false; all.len()];
    for (key, value) in overrides {
        let Some(name) = key
            .strip_prefix(VANILLA_PREFIX)
            .and_then(|rest| rest.strip_suffix(SUFFIX))
        else {
            continue;
        };
        let index = all
            .iter()
            .position(|id| vanilla_name(id) == name)
            .ok_or_else(|| {
                let known: Vec<&str> = all.iter().map(|id| vanilla_name(id)).collect();
                format!(
                    "unknown vanilla feature {name:?}; expected one of {}",
                    known.join(", ")
                )
            })?;
        off[index] = !value
            .parse::<bool>()
            .map_err(|_| format!("{key} must be true or false, not {value:?}"))?;
    }
    Ok(all
        .into_iter()
        .zip(off)
        .filter(|(_, o)| *o)
        .map(|(id, _)| id)
        .collect())
}

fn tuff_feature_builtin_moving_block_entities() -> Box<dyn Feature> {
    Box::new(tuff_feature_builtin_moving_block_entities::MovingBlockEntities)
}

fn world_map() -> Box<dyn Feature> {
    Box::new(tuff_feature_builtin_world_map::WorldMapFeature)
}

fn example() -> Box<dyn Feature> {
    Box::new(tuff_feature_builtin_example::ExampleFeature)
}

fn audit_log() -> Box<dyn Feature> {
    Box::new(tuff_feature_builtin_audit_log::AuditLogFeature::default())
}

fn serverpack_command() -> Box<dyn Feature> {
    Box::new(tuff_feature_builtin_vanilla_commands::serverpack::ServerPackFeature)
}

fn chase_command() -> Box<dyn Feature> {
    Box::new(tuff_feature_builtin_vanilla_commands::chase::ChaseFeature)
}

fn raid_command() -> Box<dyn Feature> {
    Box::new(tuff_feature_builtin_vanilla_mobs::raid_command::RaidFeature)
}

fn debugconfig_command() -> Box<dyn Feature> {
    Box::new(tuff_feature_builtin_vanilla_commands::debugconfig::DebugConfigFeature)
}

fn debugpath_command() -> Box<dyn Feature> {
    Box::new(tuff_feature_builtin_vanilla_commands::debugpath::DebugPathFeature)
}

fn debugmobspawning_command() -> Box<dyn Feature> {
    Box::new(tuff_feature_builtin_vanilla_mobs::debugmobspawning_command::DebugMobSpawningFeature)
}

fn warden_spawn_tracker_command() -> Box<dyn Feature> {
    Box::new(tuff_feature_builtin_vanilla_commands::warden_spawn_tracker::WardenSpawnTrackerFeature)
}

fn chunk_progress() -> Box<dyn Feature> {
    Box::new(tuff_feature_builtin_chunk_progress::ExampleChunkFeature)
}

pub const FEATURES: &[OptionalFeature] = &[
    OptionalFeature {
        id: tuff_feature_builtin_moving_block_entities::ID,
        summary: "pistons move blocks with block entities (chests, furnaces, ...) and their data",
        make: tuff_feature_builtin_moving_block_entities,
    },
    OptionalFeature {
        id: tuff_feature_builtin_world_map::ID,
        summary: "a live, zoomable world map page in the web panel",
        make: world_map,
    },
    OptionalFeature {
        id: tuff_feature_builtin_example::ID,
        summary: "the feature API example: /gold, extra recipes, a custom item, block and entity, and a Gold panel page",
        make: example,
    },
    OptionalFeature {
        id: tuff_feature_builtin_chunk_progress::ID,
        summary: "a bossbar showing each player how many chunks in view have been sent",
        make: chunk_progress,
    },
    OptionalFeature {
        id: tuff_feature_builtin_audit_log::ID,
        summary: "a world audit log in SQLite: who broke, placed and said what, with lookup, rollback and restore under /tuff audit",
        make: audit_log,
    },
    OptionalFeature {
        id: tuff_feature_builtin_vanilla_commands::serverpack::ID,
        summary: "the game's developer /serverpack push|pop command, gamemasters only",
        make: serverpack_command,
    },
    OptionalFeature {
        id: tuff_feature_builtin_vanilla_commands::chase::ID,
        summary: "the game's developer /chase command (lead/follow/stop over TCP), owners only",
        make: chase_command,
    },
    OptionalFeature {
        id: tuff_feature_builtin_vanilla_mobs::raid_command::ID,
        summary: "the game's developer /raid command (start, stop, check, setomen, glow), admins only",
        make: raid_command,
    },
    OptionalFeature {
        id: tuff_feature_builtin_vanilla_commands::debugconfig::ID,
        summary: "the game's developer /debugconfig command (send a player back to configuration and return them), admins only",
        make: debugconfig_command,
    },
    OptionalFeature {
        id: tuff_feature_builtin_vanilla_commands::debugpath::ID,
        summary: "the game's developer /debugpath command (ground path from the source mob), gamemasters",
        make: debugpath_command,
    },
    OptionalFeature {
        id: tuff_feature_builtin_vanilla_mobs::debugmobspawning_command::ID,
        summary: "the game's developer /debugmobspawning command (one natural spawn attempt for a category), gamemasters",
        make: debugmobspawning_command,
    },
    OptionalFeature {
        id: tuff_feature_builtin_vanilla_commands::warden_spawn_tracker::ID,
        summary: "the game's developer /warden_spawn_tracker clear|set command, gamemasters",
        make: warden_spawn_tracker_command,
    },
];

const IDS: &[&str] = &[
    tuff_feature_builtin_moving_block_entities::ID,
    tuff_feature_builtin_world_map::ID,
    tuff_feature_builtin_example::ID,
    tuff_feature_builtin_chunk_progress::ID,
    tuff_feature_builtin_audit_log::ID,
    tuff_feature_builtin_vanilla_commands::serverpack::ID,
    tuff_feature_builtin_vanilla_commands::chase::ID,
    tuff_feature_builtin_vanilla_mobs::raid_command::ID,
    tuff_feature_builtin_vanilla_commands::debugconfig::ID,
    tuff_feature_builtin_vanilla_commands::debugpath::ID,
    tuff_feature_builtin_vanilla_mobs::debugmobspawning_command::ID,
    tuff_feature_builtin_vanilla_commands::warden_spawn_tracker::ID,
];

const FEATURES_ID: &str = "tuff-features";

pub fn is_feature(id: &str) -> bool {
    IDS.contains(&id)
}

/// The feature id in a `tuff.features.<id>.enabled` key, if it has that shape.
fn id_of_key(key: &str) -> Option<&str> {
    key.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)
}

fn unknown(id: &str) -> String {
    format!("unknown feature {id:?}; expected one of {}", IDS.join(", "))
}

/// The features switched on by `overrides`, in table order. The last override
/// of a key wins; unknown feature ids and non-boolean values are refused.
pub fn enabled_from(overrides: &[(String, String)]) -> Result<Vec<String>, String> {
    let mut on = vec![false; FEATURES.len()];
    for (key, value) in overrides {
        let Some(id) = id_of_key(key) else { continue };
        let index = FEATURES
            .iter()
            .position(|feature| feature.id == id)
            .ok_or_else(|| unknown(id))?;
        on[index] = value
            .parse::<bool>()
            .map_err(|_| format!("{key} must be true or false, not {value:?}"))?;
    }
    Ok(FEATURES
        .iter()
        .zip(on)
        .filter(|(_, on)| *on)
        .map(|(feature, _)| feature.id.to_owned())
        .collect())
}

/// The `tuff.features.*` entries of a `tuff.properties` text, in file order.
pub fn file_overrides(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#') && !line.starts_with('!'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
        .filter(|(key, _)| key.starts_with(PREFIX) || key.starts_with(VANILLA_PREFIX))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// The saved `tuff.features.*` settings of the instance, read from the
/// configuration store of the chosen backend before the server starts.
pub fn saved_overrides(
    backend: &str,
    instance_dir: &std::path::Path,
    server_file: &std::path::Path,
    locking: tuff_storage::sqlite::Locking,
    no_save: bool,
) -> Vec<(String, String)> {
    use tuff_storage::config::ConfigStore;
    use tuff_storage::sqlite;
    use tuff_storage::vanilla;
    let read = if backend == sqlite::NAME {
        if !server_file.is_file() {
            return Vec::new();
        }
        let open = if no_save {
            sqlite::SqliteBackend::open_read_only(server_file)
        } else {
            sqlite::SqliteBackend::open_with(server_file, locking)
        };
        open.and_then(|backend| {
            tuff_storage::backend::WorldBackend::config(&backend)
                .map_or(Ok(Vec::new()), |c| c.read_settings())
        })
    } else {
        vanilla::config::VanillaConfig::new(instance_dir).read_settings()
    };
    read.unwrap_or_default()
        .into_iter()
        .filter(|(key, _)| key.starts_with(PREFIX) || key.starts_with(VANILLA_PREFIX))
        .collect()
}

/// The features named by `ids`, in that order.
pub fn make(ids: &[String]) -> Result<Vec<Box<dyn Feature>>, String> {
    ids.iter()
        .map(|id| {
            FEATURES
                .iter()
                .find(|feature| feature.id == id)
                .map(OptionalFeature::make)
                .ok_or_else(|| unknown(id))
        })
        .collect()
}

/// What the `tuff` binary runs: the vanilla set, the feature settings, then
/// the features named by `enabled`.
pub fn shipped(enabled: &[String]) -> Result<Vec<Box<dyn Feature>>, String> {
    shipped_without(enabled, &[])
}

/// [`shipped`] minus the vanilla features named in `disabled`. Refuses a
/// set where a feature that stays needs one that was left out.
pub fn shipped_without(
    enabled: &[String],
    disabled: &[&str],
) -> Result<Vec<Box<dyn Feature>>, String> {
    let mut features = crate::vanilla_features::all();
    if !disabled.is_empty() {
        features.retain(|feature| !disabled.contains(&feature.id()));
        let mut registries = tuff_core::registry::Registries::new();
        if let Err(err) = tuff_core::feature::load_all(&features, &mut registries) {
            return Err(format!(
                "{} cannot load with the vanilla features {} disabled: {err}",
                err.feature,
                disabled.join(", ")
            ));
        }
    }
    features.push(Box::new(FeatureSettings));
    features.push(Box::new(tuff_plugin_wasm::WasmPluginsFeature::default()));
    features.extend(make(enabled)?);
    Ok(features)
}

struct FeatureSettings;

impl Feature for FeatureSettings {
    fn id(&self) -> &'static str {
        FEATURES_ID
    }

    fn load(&self, cx: &mut LoadContext) -> Result<(), LoadError> {
        if cx.phase() == Phase::Content {
            for feature in crate::vanilla_features::all() {
                cx.registries().config_keys.push(Key::new(
                    &vanilla_enabled_key(feature.id()),
                    Value::Bool(true),
                    &format!(
                        "Loads the {} vanilla feature at startup; false leaves it out, and a change applies after a restart",
                        feature.id()
                    ),
                ));
            }
            for feature in FEATURES {
                cx.registries().config_keys.push(Key::new(
                    &feature.key(),
                    Value::Bool(false),
                    &format!(
                        "Turns on the {} feature at startup: {}; a change applies after a restart",
                        feature.id, feature.summary
                    ),
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(features: &[Box<dyn Feature>]) -> Vec<&'static str> {
        features.iter().map(|feature| feature.id()).collect()
    }

    fn overrides(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn load(features: &[Box<dyn Feature>]) -> tuff_core::registry::Registries {
        let mut registries = tuff_core::registry::Registries::new();
        tuff_core::feature::load_all(features, &mut registries).unwrap();
        registries
    }

    #[tuff_core::watchdog]
    #[test]
    fn features_are_off_by_default() {
        let shipped = shipped(&[]).unwrap();
        let mut expected = ids(&crate::vanilla_features::all());
        expected.push(FEATURES_ID);
        expected.push(tuff_plugin_wasm::ID);
        assert_eq!(ids(&shipped), expected);
        assert!(enabled_from(&[]).unwrap().is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn serverpack_is_registered_only_when_its_flag_is_on() {
        let key = enabled_key(tuff_feature_builtin_vanilla_commands::serverpack::ID);
        assert_eq!(key, "tuff.features.serverpack-command.enabled");
        let off = load(&shipped(&[]).unwrap());
        assert!(off.commands.roots().command("serverpack").is_none());
        let enabled = enabled_from(&overrides(&[(&key, "true")])).unwrap();
        let on = load(&shipped(&enabled).unwrap());
        assert!(on.commands.roots().command("serverpack").is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn chase_is_registered_only_when_its_flag_is_on() {
        let key = enabled_key(tuff_feature_builtin_vanilla_commands::chase::ID);
        assert_eq!(key, "tuff.features.chase-command.enabled");
        let off = load(&shipped(&[]).unwrap());
        assert!(off.commands.roots().command("chase").is_none());
        let enabled = enabled_from(&overrides(&[(&key, "true")])).unwrap();
        let on = load(&shipped(&enabled).unwrap());
        assert!(on.commands.roots().command("chase").is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn debugconfig_is_registered_only_when_its_flag_is_on() {
        let key = enabled_key(tuff_feature_builtin_vanilla_commands::debugconfig::ID);
        assert_eq!(key, "tuff.features.debugconfig-command.enabled");
        let off = load(&shipped(&[]).unwrap());
        assert!(off.commands.roots().command("debugconfig").is_none());
        let enabled = enabled_from(&overrides(&[(&key, "true")])).unwrap();
        let on = load(&shipped(&enabled).unwrap());
        assert!(on.commands.roots().command("debugconfig").is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn raid_is_registered_only_when_its_flag_is_on() {
        let key = enabled_key(tuff_feature_builtin_vanilla_mobs::raid_command::ID);
        assert_eq!(key, "tuff.features.raid-command.enabled");
        let off = load(&shipped(&[]).unwrap());
        assert!(off.commands.roots().command("raid").is_none());
        let enabled = enabled_from(&overrides(&[(&key, "true")])).unwrap();
        let on = load(&shipped(&enabled).unwrap());
        assert!(on.commands.roots().command("raid").is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn debugpath_is_registered_only_when_its_flag_is_on() {
        let key = enabled_key(tuff_feature_builtin_vanilla_commands::debugpath::ID);
        assert_eq!(key, "tuff.features.debugpath-command.enabled");
        let off = load(&shipped(&[]).unwrap());
        assert!(off.commands.roots().command("debugpath").is_none());
        let enabled = enabled_from(&overrides(&[(&key, "true")])).unwrap();
        let on = load(&shipped(&enabled).unwrap());
        assert!(on.commands.roots().command("debugpath").is_some());
    }

    #[tuff_core::watchdog]
    #[test]
    fn debug_spawn_and_warden_commands_are_registered_only_when_flagged() {
        for (id, key, name) in [
            (
                tuff_feature_builtin_vanilla_mobs::debugmobspawning_command::ID,
                "tuff.features.debugmobspawning-command.enabled",
                "debugmobspawning",
            ),
            (
                tuff_feature_builtin_vanilla_commands::warden_spawn_tracker::ID,
                "tuff.features.warden-spawn-tracker-command.enabled",
                "warden_spawn_tracker",
            ),
        ] {
            assert_eq!(enabled_key(id), key);
            let off = load(&shipped(&[]).unwrap());
            assert!(off.commands.roots().command(name).is_none());
            let enabled = enabled_from(&overrides(&[(key, "true")])).unwrap();
            let on = load(&shipped(&enabled).unwrap());
            assert!(on.commands.roots().command(name).is_some());
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn vanilla_features_are_all_on_by_default() {
        assert!(disabled_vanilla_from(&[]).unwrap().is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_vanilla_feature_can_be_excluded() {
        let key = vanilla_enabled_key("vanilla-survival");
        assert_eq!(key, "tuff.vanilla.survival.enabled");
        let off = disabled_vanilla_from(&overrides(&[(&key, "false")])).unwrap();
        assert_eq!(off, ["vanilla-survival"]);
        let shipped = shipped_without(&[], &off).unwrap();
        assert!(!ids(&shipped).contains(&"vanilla-survival"));
        assert_eq!(shipped.len(), crate::vanilla_features::all().len() + 1);
        let back = overrides(&[(&key, "false"), (&key, "true")]);
        assert!(disabled_vanilla_from(&back).unwrap().is_empty());
        assert!(
            disabled_vanilla_from(&overrides(&[("tuff.vanilla.nope.enabled", "false")])).is_err()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_missing_vanilla_dependency_is_refused_by_name() {
        let all: Vec<&'static str> = crate::vanilla_features::all()
            .iter()
            .map(|p| p.id())
            .collect();
        let broken = all.iter().find(|id| {
            let off = [**id];
            shipped_without(&[], &off).is_err()
        });
        if let Some(id) = broken {
            let err = shipped_without(&[], &[id]).err().unwrap();
            assert!(err.contains(id), "{err}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_enabled_keys_select_features_and_the_last_override_wins() {
        let key = enabled_key(tuff_feature_builtin_audit_log::ID);
        let world = enabled_key(tuff_feature_builtin_world_map::ID);
        let on = enabled_from(&overrides(&[
            (&key, "false"),
            (&world, "true"),
            (&key, "true"),
        ]))
        .unwrap();
        assert_eq!(
            on,
            [
                tuff_feature_builtin_world_map::ID,
                tuff_feature_builtin_audit_log::ID
            ]
        );
        let off = enabled_from(&overrides(&[(&key, "true"), (&key, "false")])).unwrap();
        assert!(off.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_properties_file_selects_features_and_overrides_win() {
        let key = enabled_key(tuff_feature_builtin_audit_log::ID);
        let text = format!("# c\ntuff.http.port=1\n{key} = true\n");
        let mut all = file_overrides(&text);
        assert_eq!(all, overrides(&[(&key, "true")]));
        assert_eq!(
            enabled_from(&all).unwrap(),
            [tuff_feature_builtin_audit_log::ID]
        );
        all.extend(overrides(&[(&key, "false")]));
        assert!(enabled_from(&all).unwrap().is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn unknown_features_and_bad_values_are_refused() {
        let err = enabled_from(&overrides(&[("tuff.features.nope.enabled", "true")])).unwrap_err();
        assert!(
            err.contains("\"nope\"")
                && err.contains(tuff_feature_builtin_moving_block_entities::ID),
            "{err}"
        );
        let key = enabled_key(tuff_feature_builtin_moving_block_entities::ID);
        assert!(enabled_from(&overrides(&[(&key, "maybe")])).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_old_plugin_list_key_selects_nothing() {
        let on = enabled_from(&overrides(&[("tuff.plugins.enabled", "audit-log")])).unwrap();
        assert!(on.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn enabling_appends_the_feature_after_the_rest() {
        let shipped =
            shipped(&[tuff_feature_builtin_moving_block_entities::ID.to_owned()]).unwrap();
        assert_eq!(
            ids(&shipped).last(),
            Some(&tuff_feature_builtin_moving_block_entities::ID)
        );
        assert_eq!(shipped.len(), crate::vanilla_features::all().len() + 3);
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_runtime_plugin_host_ships_by_default() {
        let registries = load(&shipped(&[]).unwrap());
        assert!(
            registries
                .config_keys
                .iter()
                .any(|key| key.name == tuff_plugin_wasm::ENABLED_KEY)
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn feature_ids_match_the_table() {
        assert_eq!(FEATURES.iter().map(|f| f.id).collect::<Vec<_>>(), IDS);
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_feature_loads_beside_the_shipped_set() {
        let ids: Vec<String> = IDS.iter().map(|id| (*id).to_owned()).collect();
        load(&shipped(&ids).unwrap());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_example_feature_loads_only_when_enabled() {
        let gold = |features: &[Box<dyn Feature>]| {
            load(features)
                .panel
                .installed()
                .pages
                .contains_key(tuff_feature_builtin_example::GOLD_PAGE)
        };
        assert!(!gold(&shipped(&[]).unwrap()));
        assert!(gold(
            &shipped(&[tuff_feature_builtin_example::ID.to_owned()]).unwrap()
        ));
    }

    fn tuff_children(features: &[Box<dyn Feature>]) -> Vec<&'static str> {
        let registries = load(features);
        let tuff = registries
            .commands
            .roots()
            .commands()
            .iter()
            .find(|command| command.name == "tuff")
            .unwrap()
            .node;
        tuff.children.iter().map(|child| child.name).collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_audit_log_adds_its_tuff_subcommand_only_when_enabled() {
        let off = tuff_children(&shipped(&[]).unwrap());
        assert!(!off.contains(&"audit"), "{off:?}");
        let on = tuff_children(&shipped(&[tuff_feature_builtin_audit_log::ID.to_owned()]).unwrap());
        assert!(on.contains(&"audit") && on.is_sorted(), "{on:?}");
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_chunk_progress_bar_loads_only_when_enabled() {
        let hooks = |features: &[Box<dyn Feature>]| load(features).session_hooks.len();
        assert_eq!(
            hooks(&shipped(&[tuff_feature_builtin_chunk_progress::ID.to_owned()]).unwrap()),
            hooks(&shipped(&[]).unwrap()) + 1
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_settings_are_saved_booleans() {
        let registries =
            load(&shipped(&[tuff_feature_builtin_moving_block_entities::ID.to_owned()]).unwrap());
        let settings = tuff_core::config::ConfigStore::new();
        for key in &registries.config_keys {
            settings.register(key.clone()).unwrap();
        }
        let key = enabled_key(tuff_feature_builtin_moving_block_entities::ID);
        settings.set_text_for_run(&key, "true").unwrap();
        assert!(settings.set_text_for_run(&key, "nope").is_err());
        assert!(settings.definition(&key).unwrap().saved);
        assert!(registries.block_entity_carrier.get().is_some());
    }

    fn restart_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tuff-features-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn settings_with_sink(
        store: std::sync::Arc<dyn tuff_storage::config::ConfigStore>,
    ) -> tuff_core::config::ConfigStore {
        let registries = load(&shipped(&[]).unwrap());
        let settings = tuff_core::config::ConfigStore::new();
        for key in &registries.config_keys {
            settings.register(key.clone()).unwrap();
        }
        settings.set_sink(Box::new(move |pairs| store.write_settings(&pairs).unwrap()));
        settings
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_feature_set_at_runtime_survives_a_vanilla_restart() {
        use tuff_storage::sqlite::Locking;
        let dir = restart_dir("vanilla");
        let store = std::sync::Arc::new(tuff_storage::vanilla::config::VanillaConfig::new(&dir));
        let key = enabled_key(tuff_feature_builtin_moving_block_entities::ID);
        settings_with_sink(store).set_text(&key, "true").unwrap();
        let saved = saved_overrides("vanilla", &dir, &dir, Locking::Exclusive, false);
        assert_eq!(
            enabled_from(&saved).unwrap(),
            [tuff_feature_builtin_moving_block_entities::ID]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_feature_set_in_the_properties_file_survives_a_restart() {
        use tuff_storage::sqlite::Locking;
        let dir = restart_dir("file");
        let key = enabled_key(tuff_feature_builtin_moving_block_entities::ID);
        std::fs::write(dir.join("tuff.properties"), format!("{key}=true\n")).unwrap();
        let saved = saved_overrides("vanilla", &dir, &dir, Locking::Exclusive, false);
        assert_eq!(
            enabled_from(&saved).unwrap(),
            [tuff_feature_builtin_moving_block_entities::ID]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_feature_set_at_runtime_survives_a_sqlite_restart() {
        use tuff_storage::sqlite::{Locking, SqliteBackend};
        let dir = restart_dir("sqlite");
        let file = dir.join("world.tuff");
        let key = enabled_key(tuff_feature_builtin_moving_block_entities::ID);
        {
            let backend = SqliteBackend::open_with(&file, Locking::Exclusive).unwrap();
            let store = tuff_storage::backend::WorldBackend::config(&backend).unwrap();
            settings_with_sink(store).set_text(&key, "true").unwrap();
        }
        let saved = saved_overrides("sqlite", &dir, &file, Locking::Exclusive, false);
        assert_eq!(
            enabled_from(&saved).unwrap(),
            [tuff_feature_builtin_moving_block_entities::ID]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
