//! The server.properties spec table (types, defaults, effects, secrets) and
//! value parsing, shared by the web panel and `/tuff config minecraft.<key>`.

/// Longest text accepted for a property value.
pub const MAX_VALUE: usize = 4096;

/// How a server.properties value is typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyKind {
    Bool,
    Int { min: i64, max: i64 },
    Text,
    Choice(&'static [&'static str]),
}

/// What changing a property on a running server does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Applied as soon as it is set.
    Live,
    /// Read only when the server starts, for the reason given.
    Restart { reason: &'static str },
    /// Read only when a new world is created; an existing world keeps
    /// what it was made with.
    Creation,
    /// Kept in the file but not acted on yet.
    Unsupported {
        reason: &'static str,
        todo: &'static str,
    },
    /// Kept in the file and never acted on; not planned.
    Permanent { reason: &'static str },
}

impl Effect {
    pub fn name(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Restart { .. } => "restart",
            Self::Creation => "creation",
            Self::Unsupported { .. } => "unsupported",
            Self::Permanent { .. } => "permanent",
        }
    }

    /// One line for people: what setting the key does.
    pub fn describe(self) -> String {
        match self {
            Self::Live => "applies at once".to_owned(),
            Self::Restart { reason } => format!("needs a restart: {reason}"),
            Self::Creation => "only applies when a new world is created".to_owned(),
            Self::Unsupported { reason, .. } => format!("not supported yet: {reason}"),
            Self::Permanent { reason } => format!("not supported and not planned: {reason}"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Property {
    pub name: &'static str,
    pub kind: PropertyKind,
    pub default: &'static str,
    pub effect: Effect,
    pub secret: bool,
}

const fn prop(
    name: &'static str,
    kind: PropertyKind,
    default: &'static str,
    effect: Effect,
) -> Property {
    Property {
        name,
        kind,
        default,
        effect,
        secret: false,
    }
}

const fn int(min: i64, max: i64) -> PropertyKind {
    PropertyKind::Int { min, max }
}

const BOOL: PropertyKind = PropertyKind::Bool;
const TEXT: PropertyKind = PropertyKind::Text;
const U16: PropertyKind = int(0, 65535);
const ANY: PropertyKind = int(0, i32::MAX as i64);
const LIVE: Effect = Effect::Live;
const CREATION: Effect = Effect::Creation;
const fn restart(reason: &'static str) -> Effect {
    Effect::Restart { reason }
}
const fn permanent(reason: &'static str) -> Effect {
    Effect::Permanent { reason }
}
const fn secret(p: Property) -> Property {
    Property { secret: true, ..p }
}

const LISTENER: &str = "the game listener is bound when the server starts";
const RCON: Effect = permanent("remote console is not planned");
const QUERY: Effect = restart("the query socket is bound when the server starts");
const MANAGEMENT: Effect =
    permanent("the management server is not planned; the admin panel covers its uses");
const FILTERING: Effect = restart("the text filter is created when the server starts");

/// The properties the game defines, sorted by name, with their defaults.
pub const PROPERTIES: &[Property] = &[
    prop("accepts-transfers", BOOL, "false", LIVE),
    prop("allow-flight", BOOL, "false", LIVE),
    prop("broadcast-console-to-ops", BOOL, "true", LIVE),
    prop("broadcast-rcon-to-ops", BOOL, "true", RCON),
    prop("bug-report-link", TEXT, "", LIVE),
    prop("chat-spam-threshold-seconds", ANY, "10", LIVE),
    prop("command-spam-threshold-seconds", ANY, "10", LIVE),
    prop(
        "difficulty",
        PropertyKind::Choice(&["peaceful", "easy", "normal", "hard"]),
        "easy",
        LIVE,
    ),
    prop("enable-code-of-conduct", BOOL, "false", LIVE),
    prop(
        "enable-jmx-monitoring",
        BOOL,
        "false",
        permanent("there is no JVM to monitor"),
    ),
    prop("enable-query", BOOL, "false", QUERY),
    prop("enable-rcon", BOOL, "false", RCON),
    prop("enable-status", BOOL, "true", LIVE),
    prop(
        "enforce-secure-profile",
        BOOL,
        "true",
        restart("players negotiate signed chat when they log in"),
    ),
    prop("enforce-whitelist", BOOL, "false", LIVE),
    prop(
        "entity-broadcast-range-percentage",
        int(10, 1000),
        "100",
        LIVE,
    ),
    prop(
        "feature-packs",
        TEXT,
        "",
        restart("feature flags shape the registries every client receives at login"),
    ),
    prop("force-gamemode", BOOL, "false", LIVE),
    prop("function-permission-level", int(1, 4), "2", LIVE),
    prop(
        "gamemode",
        PropertyKind::Choice(&["survival", "creative", "adventure", "spectator"]),
        "survival",
        LIVE,
    ),
    prop("generate-structures", BOOL, "true", CREATION),
    prop("generator-settings", TEXT, "{}", CREATION),
    prop("hardcore", BOOL, "false", LIVE),
    prop("hide-online-players", BOOL, "false", LIVE),
    prop("initial-disabled-packs", TEXT, "", CREATION),
    prop("initial-enabled-packs", TEXT, "vanilla", CREATION),
    prop("level-name", TEXT, "world", CREATION),
    prop("level-seed", TEXT, "", CREATION),
    prop("level-type", TEXT, "minecraft:normal", CREATION),
    prop("log-ips", BOOL, "true", LIVE),
    prop("management-server-allowed-origins", TEXT, "", MANAGEMENT),
    prop("management-server-enabled", BOOL, "false", MANAGEMENT),
    prop("management-server-host", TEXT, "localhost", MANAGEMENT),
    prop("management-server-port", U16, "0", MANAGEMENT),
    secret(prop("management-server-secret", TEXT, "", MANAGEMENT)),
    prop("management-server-tls-enabled", BOOL, "true", MANAGEMENT),
    prop("management-server-tls-keystore", TEXT, "", MANAGEMENT),
    secret(prop(
        "management-server-tls-keystore-password",
        TEXT,
        "",
        MANAGEMENT,
    )),
    prop(
        "max-chained-neighbor-updates",
        int(i32::MIN as i64, i32::MAX as i64),
        "1000000",
        LIVE,
    ),
    prop("max-players", ANY, "20", LIVE),
    prop(
        "max-tick-time",
        int(-1, i64::MAX),
        "60000",
        permanent("no tick watchdog stops the server"),
    ),
    prop("max-world-size", int(1, 29_999_984), "29999984", LIVE),
    prop("motd", TEXT, "A Minecraft Server", LIVE),
    prop(
        "network-compression-threshold",
        int(-1, i32::MAX as i64),
        "256",
        LIVE,
    ),
    prop(
        "online-mode",
        BOOL,
        "true",
        restart("it changes the login handshake and how player ids are made"),
    ),
    prop("op-permission-level", int(0, 4), "4", LIVE),
    prop(
        "pause-when-empty-seconds",
        int(-1, i32::MAX as i64),
        "60",
        LIVE,
    ),
    prop("player-idle-timeout", ANY, "0", LIVE),
    prop(
        "pre-login-max-connections",
        ANY,
        "512",
        restart("admission limits are read when the server starts"),
    ),
    prop(
        "pre-login-max-connections-per-ip",
        ANY,
        "8",
        restart("admission limits are read when the server starts"),
    ),
    prop(
        "pre-login-timeout-seconds",
        ANY,
        "30",
        restart("admission limits are read when the server starts"),
    ),
    prop("prevent-proxy-connections", BOOL, "false", LIVE),
    prop("pvp", BOOL, "true", LIVE),
    prop("query.port", U16, "25565", QUERY),
    prop("rate-limit", ANY, "400", LIVE),
    secret(prop("rcon.password", TEXT, "", RCON)),
    prop("rcon.port", U16, "25575", RCON),
    prop(
        "region-file-compression",
        PropertyKind::Choice(&["deflate", "lz4", "none"]),
        "deflate",
        permanent("region files are always written with deflate"),
    ),
    prop("require-resource-pack", BOOL, "false", LIVE),
    prop("resource-pack", TEXT, "", LIVE),
    prop("resource-pack-id", TEXT, "", LIVE),
    prop("resource-pack-prompt", TEXT, "", LIVE),
    prop("resource-pack-sha1", TEXT, "", LIVE),
    prop("server-ip", TEXT, "", restart(LISTENER)),
    prop("server-port", U16, "25565", restart(LISTENER)),
    prop("simulation-distance", int(3, 32), "10", LIVE),
    prop("spawn-protection", ANY, "16", LIVE),
    prop("status-heartbeat-interval", ANY, "0", MANAGEMENT),
    prop(
        "sync-chunk-writes",
        BOOL,
        "true",
        permanent("the storage backends choose their own durability"),
    ),
    prop("text-filtering-config", TEXT, "", FILTERING),
    prop("text-filtering-version", ANY, "0", FILTERING),
    prop(
        "use-native-transport",
        BOOL,
        "true",
        permanent("tokio always uses the native transport"),
    ),
    prop("view-distance", int(3, 32), "10", LIVE),
    prop("white-list", BOOL, "true", LIVE),
];

/// What the world a creation-only key no longer affects was made with.
pub struct WorldFacts<'a> {
    pub seed: i64,
    pub level_name: &'a str,
}

/// What to tell someone who set a creation-only key while a world exists.
pub fn creation_note(name: &str, world: &WorldFacts) -> String {
    match name {
        "level-seed" => format!(
            "level-seed only applies when a new world is created; the current world keeps seed {}.",
            world.seed
        ),
        "level-name" => format!(
            "level-name only applies when the server starts, opening or creating that world; \
             the current world stays '{}'.",
            world.level_name
        ),
        _ => {
            let what = match name {
                "level-type" => "world type",
                "generator-settings" => "generator settings",
                "generate-structures" => "structure setting",
                "initial-enabled-packs" | "initial-disabled-packs" => {
                    "data packs (change them with /datapack)"
                }
                _ => "settings",
            };
            format!(
                "{name} only applies when a new world is created; the current world keeps its {what}."
            )
        }
    }
}

/// Notes for creation-only keys in the file that cannot affect `world`,
/// an existing world: each key whose value differs from what the world
/// has (the seed) or, where that is not known, from the default.
pub fn startup_creation_notes(
    entries: &std::collections::BTreeMap<String, String>,
    world: &WorldFacts,
) -> Vec<String> {
    PROPERTIES
        .iter()
        .filter(|p| p.effect == Effect::Creation && p.name != "level-name")
        .filter_map(|p| {
            let value = entries.get(p.name)?.trim();
            let differs = match p.name {
                "level-seed" => {
                    !value.is_empty() && tuff_storage::admin::parse_seed(value) != world.seed
                }
                _ => value != p.default && !value.is_empty(),
            };
            differs.then(|| creation_note(p.name, world))
        })
        .collect()
}

pub fn property(name: &str) -> Option<&'static Property> {
    PROPERTIES
        .binary_search_by(|p| p.name.cmp(name))
        .ok()
        .map(|i| &PROPERTIES[i])
}

/// Whether `text` may be written into a properties file line: no line
/// breaks or other control characters, and not too long.
pub fn valid_text(text: &str) -> bool {
    text.len() <= MAX_VALUE && !text.chars().any(char::is_control)
}

/// The canonical text for a property value, or why it is refused. A key
/// the game does not define (kept from the file) takes any text.
pub fn parse_property(spec: Option<&Property>, text: &str) -> Result<String, String> {
    if !valid_text(text) {
        return Err(format!(
            "must be one line of at most {MAX_VALUE} bytes without control characters"
        ));
    }
    let trimmed = text.trim();
    let Some(spec) = spec else {
        return Ok(trimmed.to_owned());
    };
    match spec.kind {
        PropertyKind::Bool => match trimmed {
            "true" | "false" => Ok(trimmed.to_owned()),
            _ => Err("must be true or false".to_owned()),
        },
        PropertyKind::Int { min, max } => match trimmed.parse::<i64>() {
            Ok(n) if (min..=max).contains(&n) => Ok(n.to_string()),
            _ => Err(format!("must be a whole number from {min} to {max}")),
        },
        PropertyKind::Choice(options) => {
            let lower = trimmed.to_ascii_lowercase();
            if options.contains(&lower.as_str()) {
                Ok(lower)
            } else {
                Err(format!("must be one of {}", options.join(", ")))
            }
        }
        PropertyKind::Text => Ok(trimmed.to_owned()),
    }
}

/// Splits `--config` pairs into server.properties overrides (canonical
/// key and value, spelled `minecraft.<key>` or the bare vanilla key) and
/// the rest, which are `tuff.*` settings.
pub type Pairs = Vec<(String, String)>;

pub fn split_overrides(pairs: &[(String, String)]) -> Result<(Pairs, Pairs), String> {
    let mut properties = Vec::new();
    let mut settings = Vec::new();
    for (name, text) in pairs {
        let prefixed = name.strip_prefix("minecraft.");
        let bare = prefixed.unwrap_or(name);
        let spec = if name.starts_with("tuff.") {
            None
        } else {
            property(bare)
        };
        let Some(spec) = spec else {
            if prefixed.is_some() {
                return Err(format!("unknown server.properties key {bare}"));
            }
            settings.push((name.clone(), text.clone()));
            continue;
        };
        match spec.effect {
            Effect::Unsupported { reason, .. } => {
                return Err(format!("{bare} is not supported yet: {reason}"));
            }
            Effect::Permanent { reason } => {
                return Err(format!("{bare} is not supported and not planned: {reason}"));
            }
            _ => {}
        }
        let value = parse_property(Some(spec), text).map_err(|err| format!("{bare}: {err}"))?;
        properties.push((bare.to_owned(), value));
    }
    Ok((properties, settings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn pair(k: &str, v: &str) -> Vec<(String, String)> {
        vec![(k.to_owned(), v.to_owned())]
    }

    #[tuff_core::watchdog]
    #[test]
    fn overrides_split_by_spelling() {
        let (props, settings) =
            split_overrides(&pair("minecraft.enable-code-of-conduct", "false")).unwrap();
        assert_eq!(props, pair("enable-code-of-conduct", "false"));
        assert!(settings.is_empty());
        let (props, _) = split_overrides(&pair("difficulty", "hard")).unwrap();
        assert_eq!(props, pair("difficulty", "hard"));
        let (props, settings) = split_overrides(&pair("tuff.http.port", "1")).unwrap();
        assert!(props.is_empty());
        assert_eq!(settings.len(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn pvp_override_is_a_known_bool_key() {
        let (props, settings) = split_overrides(&pair("minecraft.pvp", "false")).unwrap();
        assert_eq!(props, pair("pvp", "false"));
        assert!(settings.is_empty());
        assert!(split_overrides(&pair("pvp", "maybe")).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn bad_override_values_and_keys_are_refused() {
        assert!(split_overrides(&pair("minecraft.hardcore", "maybe")).is_err());
        assert!(split_overrides(&pair("minecraft.nope", "1")).is_err());
    }

    fn entries(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    const WORLD: WorldFacts = WorldFacts {
        seed: 42,
        level_name: "world",
    };

    #[tuff_core::watchdog]
    #[test]
    fn startup_notes_only_keys_that_cannot_apply() {
        let quiet = entries(&[
            ("level-seed", "42"),
            ("level-type", "minecraft:normal"),
            ("generate-structures", "true"),
            ("initial-enabled-packs", "vanilla"),
            ("level-name", "elsewhere"),
            ("motd", "hi"),
        ]);
        assert!(startup_creation_notes(&quiet, &WORLD).is_empty());
        let notes = startup_creation_notes(
            &entries(&[("level-seed", "7"), ("level-type", "minecraft:amplified")]),
            &WORLD,
        );
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].contains("level-seed") && notes[0].contains("keeps seed 42"));
        assert!(notes[1].contains("keeps its world type"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn effects_describe_themselves() {
        assert_eq!(property("motd").unwrap().effect, Effect::Live);
        let port = property("server-port").unwrap().effect;
        assert!(port.describe().starts_with("needs a restart: "));
        let rcon = property("enable-rcon").unwrap().effect.describe();
        assert!(rcon.contains("not planned"), "{rcon}");
        assert_eq!(property("level-seed").unwrap().effect.name(), "creation");
    }
}
