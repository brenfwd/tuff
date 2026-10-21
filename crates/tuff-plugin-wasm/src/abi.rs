//! The plugin ABI's limits and name rules. The ABI itself is the WIT
//! world in `crates/tuff-plugin-wasm/wit/` (`plugin.wit`, which includes
//! `capabilities.wit`), run by [`crate::wit`]: a plugin is a core wasm
//! module importing the world's functions under the canonical ABI's
//! names (`tuff:plugin/host@0.1.0` `reply`, ...) and exporting
//! `memory`, `cabi_realloc` and the `guest` functions
//! (`tuff:plugin/guest@0.1.0#register`, ...), with a `tuff-plugin`
//! manifest section ([`crate::manifest`]).
//!
//! - Only `register` must be exported; a plugin that registers commands
//!   needs `run-command`, one that subscribes to events `on-event`.
//!   `enable`, `disable`, `on-timer` and `on-message` are optional.
//! - Registration calls answer `register-error.wrong-phase` outside
//!   `register`; the other calls refuse with their error value (or none)
//!   during it. Malformed input (bad pointers, invalid UTF-8, strings or
//!   lists over the limits in [`crate::Limits`]) traps, which faults the
//!   plugin and unloads it.
//! - Commands are trees ([`crate::wit::tree`]): the host matches a line
//!   against the tree and hands `run-command` the handler id and parsed
//!   arguments, and the whole line; a line the tree rejects never reaches
//!   the plugin.
//! - Permission nodes a plugin declares must start with its id and a dot
//!   (`greeter.admin`). Explicit grants and groups (`/tuff perm`) apply to
//!   declared nodes and to every server node a plugin checks.
//! - World reads load the chunks they touch and see the world as of the
//!   call; writes, messages, sends and timers are queued and carried out,
//!   in order, after the call returns and before the host handles anything
//!   else. Each call has read and write budgets.
//! - Plugins talk through interfaces: `provide` (register phase) declares
//!   an interface and its method keys; `request` and `notify` fail fast
//!   (no-provider, ambiguous, no-method, too-large) from the provider
//!   table, otherwise queue a delivery run as a fresh `on-message` call
//!   in the same host turn, FIFO per sender and target. A request is
//!   completed exactly once with an `io-event` reply. Deliveries chained
//!   more than [`MAX_HOPS`] deep, or past [`TURN_BUDGET`] per turn, wait
//!   for the next tick; an inbox holds [`MAX_INBOX`] (overflow answers
//!   `busy`).

/// The only ABI version: a manifest's `api` must equal it. The WIT
/// package version (`tuff:plugin@0.1.0`) in every import and export name
/// is what changes when the ABI breaks.
pub const API_VERSION: u32 = 0;

/// The `blocks-get` palette index of a block outside the world.
pub const UNAVAILABLE: u16 = u16::MAX;

/// Most blocks one `blocks-get` may cover.
pub const MAX_BOX_VOLUME: u64 = 1 << 15;

/// How deep messages sent from message handlers are followed.
pub const MAX_HOPS: usize = 8;
/// Interfaces one plugin may provide.
pub const MAX_PROVIDED: usize = 32;
/// Default message payload cap, in bytes.
pub const MESSAGE_BYTES: usize = 256 << 10;
/// Requests one plugin may have outstanding.
pub const MAX_OUTSTANDING_REQUESTS: usize = 256;
/// Request timeout when the caller passes 0, in ticks.
pub const DEFAULT_REQUEST_TICKS: u32 = 100;
pub const MAX_REQUEST_TICKS: u32 = 6000;
/// Queued deliveries one plugin may have waiting.
pub const MAX_INBOX: usize = 1024;
/// Deliveries one host turn runs before deferring the rest to the next tick.
pub const TURN_BUDGET: usize = 1024;

/// Whether `name` is an interface name: lowercase words and `.`, `-`, `_`, `:`.
pub fn valid_interface(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-:/".contains(&b))
}

/// Most entities one `entities-in-box` answers.
pub const MAX_ENTITIES: usize = 256;

/// Longest side of an `entities-in-box` box, in blocks.
pub const MAX_ENTITY_BOX: f64 = 256.0;

/// Permission nodes one plugin may declare.
pub const MAX_PERMISSIONS: usize = 1024;

/// Click filters one plugin may declare.
pub const MAX_CLICK_FILTERS: usize = 16;

/// Item filters one plugin may declare.
pub const MAX_ITEM_FILTERS: usize = 16;

/// Custom payload channels one plugin may register.
pub const MAX_PAYLOAD_CHANNELS: usize = 16;

/// Longest channel id, in bytes.
pub const MAX_CHANNEL_LEN: usize = 128;

/// Most bytes of one custom payload, in either direction.
pub const MAX_PAYLOAD_BYTES: usize = 32767;

/// Whether `channel` is a valid `namespace:path` id outside `minecraft:`.
pub fn valid_channel(channel: &str) -> bool {
    let Some((namespace, path)) = channel.split_once(':') else {
        return false;
    };
    channel.len() <= MAX_CHANNEL_LEN
        && namespace != "minecraft"
        && !namespace.is_empty()
        && !path.is_empty()
        && namespace
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
        && path
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'))
}

/// Inventory filters one plugin may declare.
pub const MAX_INVENTORY_FILTERS: usize = 16;

/// State filters one plugin may declare.
pub const MAX_STATE_FILTERS: usize = 16;

/// Blocks one plugin may watch with `watch-block`.
pub const MAX_WATCHED_BLOCKS: usize = 16;

/// Regions one plugin may watch with `watch-region`.
pub const MAX_WATCHED_REGIONS: usize = 16;

/// Advancement grants and revokes one call may queue.
pub const MAX_ADVANCEMENT_OPS: usize = 64;

/// Titles, action bars, sidebar and team changes one call may queue.
pub const MAX_DISPLAY_OPS: usize = 128;

/// Lines of a plugin sidebar.
pub const MAX_SIDEBAR_LINES: usize = 15;

/// Spans in one rich message.
pub const MAX_RICH_SPANS: usize = 32;

/// Longest title, line, prefix or suffix, in bytes.
pub const MAX_DISPLAY_TEXT: usize = 256;

/// Longest team name and most members of one team update.
pub const MAX_TEAM_NAME: usize = 32;
pub const MAX_TEAM_MEMBERS: usize = 256;

/// Player state changes one call may queue.
pub const MAX_PLAYER_OPS: usize = 256;
/// Particle effects one call may queue.
pub const MAX_PARTICLE_SPAWNS: usize = 256;

/// Item gives one call may queue.
pub const MAX_GIVES: usize = 16;

/// Most items one give moves.
pub const MAX_GIVE_COUNT: u32 = 2304;

/// Longest custom data SNBT or custom name a give accepts, in bytes.
pub const MAX_GIVE_TEXT: usize = 4096;

/// Inventory edits one call may queue.
pub const MAX_INVENTORY_OPS: usize = 64;

/// Longest stack SNBT a set-slot accepts, in bytes.
pub const MAX_STACK_SNBT: usize = 16384;

/// Menu calls (open, update, close) one call may queue.
pub const MAX_MENU_OPS: usize = 64;

/// Lore lines on one menu item.
pub const MAX_MENU_LORE: usize = 16;

/// Entity interaction filters one plugin may declare.
pub const MAX_ENTITY_FILTERS: usize = 16;

/// Command and chat filters one plugin may declare, each.
pub const MAX_LINE_FILTERS: usize = 32;

/// Route answers one call may queue.
pub const MAX_ROUTE_RESPONSES: usize = 256;

/// Bytes of route answer bodies one call may queue in all.
pub const MAX_RESPONSE_BYTES: usize = 8 << 20;

/// Block rule and team changes one call may queue.
pub const MAX_GUARD_OPS: usize = 1024;

/// Teleports one call may queue.
pub const MAX_TELEPORTS: usize = 64;

/// Entity spawns, removals and edits one call may queue, each.
pub const MAX_ENTITY_OPS: usize = 64;

/// Entities one smooth move or state read may name.
pub const MAX_ENTITY_GLIDES: usize = 64;

/// Longest SNBT or name an entity import accepts, in bytes.
pub const MAX_ENTITY_TEXT: usize = 16384;

/// Commands one plugin may register.
pub const MAX_COMMANDS: usize = 32;

/// `/tuff` subcommands one plugin may register.
pub const MAX_TUFF_SUBCOMMANDS: usize = 8;

/// Aliases one plugin may register.
pub const MAX_ALIASES: usize = 256;

/// Settings one plugin may declare.
pub const MAX_SETTINGS: usize = 64;

/// Longest setting key, after the plugin's prefix.
pub const MAX_SETTING_KEY: usize = 64;

/// Longest setting description.
pub const MAX_SETTING_DESCRIPTION: usize = 256;

/// Resource packs one plugin may offer.
pub const MAX_RESOURCE_PACKS: usize = 4;

/// Longest resource pack prompt.
pub const MAX_PACK_PROMPT: usize = 256;

/// The longest timer delay, in server ticks (a day of real time).
pub const MAX_DELAY: u64 = 20 * 60 * 60 * 24;

/// How deep guest calls may nest.
pub const MAX_RECURSION: usize = 1024;

/// Whether `name` can be a command a plugin registers: 1 to 32 of
/// `a-z 0-9 - _`, optionally behind exactly one leading `/` (the client
/// sends `//set` as command text `/set`).
pub fn valid_command(name: &str) -> bool {
    valid_subcommand(name.strip_prefix('/').unwrap_or(name))
}

/// Whether `name` can be a `/tuff` subcommand: like a command without the
/// leading slash.
pub fn valid_subcommand(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Whether `target` is a line an alias can rewrite to: words of
/// [`valid_subcommand`] names, at most four.
pub fn valid_alias_target(target: &str) -> bool {
    let words: Vec<&str> = target.split(' ').collect();
    words.len() <= 4 && words.iter().all(|word| valid_subcommand(word))
}

/// Whether `node` can be a permission node plugin `plugin` declares: its
/// id, a dot, then 1 to 64 of `a-z 0-9 . - _`.
pub fn valid_node(plugin: &str, node: &str) -> bool {
    node.strip_prefix(plugin)
        .and_then(|rest| rest.strip_prefix('.'))
        .is_some_and(|rest| {
            !rest.is_empty()
                && rest.len() <= 64
                && rest.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_')
                })
        })
}

/// Whether `key` can be a setting a plugin declares: dot-separated words
/// of `a-z A-Z 0-9 - _`, at most [`MAX_SETTING_KEY`] bytes.
pub fn valid_setting(key: &str) -> bool {
    key.len() <= MAX_SETTING_KEY
        && !key.is_empty()
        && key.split('.').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

/// The server setting a plugin's `key` is stored as.
pub fn setting_name(plugin: &str, key: &str) -> String {
    format!("plugins.{plugin}.{key}")
}

/// Panel pages, APIs and dashboard cards one plugin may register, each.
pub const MAX_PANEL_PAGES: usize = 8;
pub const MAX_PANEL_APIS: usize = 4;
pub const MAX_PANEL_CARDS: usize = 8;

/// Most files one panel page ships, and the most bytes in all of them.
pub const MAX_PANEL_ASSETS: usize = 64;
pub const MAX_PANEL_BYTES: usize = 4 << 20;

#[cfg(test)]
mod tests {
    #[tuff_core::watchdog]
    #[test]
    fn channel_names_are_validated() {
        for ok in ["worldedit:cui", "a.b-c_d:x/y"] {
            assert!(super::valid_channel(ok), "{ok}");
        }
        let long = format!("tuff:{}", "a".repeat(super::MAX_CHANNEL_LEN));
        for bad in [
            "",
            "cui",
            ":x",
            "x:",
            "minecraft:brand",
            "Tuff:x",
            "tuff:X",
            "tuff:a b",
            "tu ff:x",
            "tuff:\u{e9}",
            &long,
        ] {
            assert!(!super::valid_channel(bad), "{bad}");
        }
    }

    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn commands_allow_one_leading_slash() {
        assert!(valid_command("set"));
        assert!(valid_command("/set"));
        assert!(!valid_command("//set"));
        assert!(!valid_command("/"));
        assert!(!valid_command(""));
        assert!(!valid_command("Set"));
        assert!(!valid_command("/a b"));
        assert!(!valid_command(&format!("/{}", "a".repeat(33))));
    }

    #[tuff_core::watchdog]
    #[test]
    fn subcommands_and_alias_targets_have_no_slash() {
        assert!(valid_subcommand("we"));
        assert!(!valid_subcommand("/we"));
        assert!(valid_alias_target("tuff we set"));
        assert!(!valid_alias_target("tuff  we"));
        assert!(!valid_alias_target("/tuff we"));
        assert!(!valid_alias_target("a b c d e"));
        assert!(!valid_alias_target(""));
    }

    #[tuff_core::watchdog]
    #[test]
    fn setting_keys_are_dotted_words() {
        assert!(valid_setting("speed"));
        assert!(valid_setting("relay.Channel-id_2"));
        assert!(!valid_setting(""));
        assert!(!valid_setting("a..b"));
        assert!(!valid_setting(".a"));
        assert!(!valid_setting("a b"));
        assert!(!valid_setting(&"a".repeat(65)));
        assert_eq!(setting_name("relay", "token"), "plugins.relay.token");
    }
}
