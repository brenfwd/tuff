//! The parts of the server's access lists and `server.properties` that
//! need core types: login checks, ban messages and the typed accessors
//! returning game types. The records themselves live in `tuff_storage::admin`.

use std::net::IpAddr;

use tuff_storage::admin::{
    BanEntry, NameAndId, ServerProperties, UserLists, address_text, format_message_date,
};

use tuff_item::text::TextComponent;

fn name_uuid(text: &str) -> u128 {
    use md5::{Digest, Md5};
    let mut hash: [u8; 16] = Md5::digest(text.as_bytes()).into();
    hash[6] = (hash[6] & 0x0F) | 0x30;
    hash[8] = (hash[8] & 0x3F) | 0x80;
    u128::from_be_bytes(hash)
}

pub fn reason_message<K>(ban: &BanEntry<K>) -> TextComponent {
    match &ban.reason {
        Some(reason) => TextComponent::new(reason.clone()),
        None => TextComponent::translate("multiplayer.disconnect.banned.reason.default", []),
    }
}

/// The reason a player may not join, checked in the game's order.
pub fn can_login(
    lists: &mut UserLists,
    user: &NameAndId,
    address: IpAddr,
    whitelist_on: bool,
    online: usize,
    max_players: usize,
    ops_trusted: bool,
) -> Result<(), TextComponent> {
    lists.expire();
    if let Some(ban) = lists.ban(user.uuid) {
        let mut reason = TextComponent::translate(
            "multiplayer.disconnect.banned.reason",
            [reason_message(ban)],
        );
        if let Some(expires) = ban.expires {
            reason = reason.append(TextComponent::translate(
                "multiplayer.disconnect.banned.expiration",
                [TextComponent::new(format_message_date(expires))],
            ));
        }
        return Err(reason);
    }
    if whitelist_on && !(ops_trusted && lists.is_op(user.uuid)) && !lists.is_whitelisted(user.uuid)
    {
        return Err(TextComponent::translate(
            "multiplayer.disconnect.not_whitelisted",
            [],
        ));
    }
    if let Some(ban) = lists.ip_ban(&address_text(address)) {
        let mut reason = TextComponent::translate(
            "multiplayer.disconnect.banned_ip.reason",
            [reason_message(ban)],
        );
        if let Some(expires) = ban.expires {
            reason = reason.append(TextComponent::translate(
                "multiplayer.disconnect.banned_ip.expiration",
                [TextComponent::new(format_message_date(expires))],
            ));
        }
        return Err(reason);
    }
    let bypasses = ops_trusted
        && lists
            .op(user.uuid)
            .is_some_and(|op| op.bypasses_player_limit);
    if online >= max_players && !bypasses {
        return Err(TextComponent::translate(
            "multiplayer.disconnect.server_full",
            [],
        ));
    }
    Ok(())
}

/// Typed accessors of `server.properties` that return game types.
pub trait ServerPropertiesExt {
    fn resource_pack_offer(&self) -> Option<crate::resource_pack::PackOffer>;
    fn gamemode(&self) -> crate::protocol::play::GameMode;
    fn difficulty(&self) -> crate::world::player::Difficulty;
    fn initial_data_packs(&self) -> crate::datapack::DataPackConfig;
}

impl ServerPropertiesExt for ServerProperties {
    /// The pack `resource-pack` points at, offered to every joining
    /// player; `None` when the url is empty. The id is `resource-pack-id`
    /// or, when absent, the name-based UUID of the url.
    fn resource_pack_offer(&self) -> Option<crate::resource_pack::PackOffer> {
        let url = self.text("resource-pack").filter(|url| !url.is_empty())?;
        let id = self
            .text("resource-pack-id")
            .and_then(|text| crate::resource_pack::parse_uuid(&text))
            .unwrap_or_else(|| name_uuid(&url));
        let sha1 = self
            .text("resource-pack-sha1")
            .filter(|hash| !hash.is_empty())
            .or_else(|| self.text("resource-pack-hash"))
            .unwrap_or_default();
        let prompt = self
            .text("resource-pack-prompt")
            .filter(|text| !text.is_empty())
            .and_then(|text| tuff_item::text::TextComponent::from_json_str(&text));
        Some(crate::resource_pack::PackOffer {
            id,
            url,
            sha1,
            required: self.flag("require-resource-pack", false),
            prompt,
        })
    }

    /// `gamemode` by name or by number, `survival` when absent or unknown.
    fn gamemode(&self) -> crate::protocol::play::GameMode {
        use crate::protocol::play::GameMode;
        match self
            .text("gamemode")
            .as_deref()
            .map(str::to_ascii_lowercase)
        {
            Some(t) if t == "creative" || t == "1" => GameMode::Creative,
            Some(t) if t == "adventure" || t == "2" => GameMode::Adventure,
            Some(t) if t == "spectator" || t == "3" => GameMode::Spectator,
            _ => GameMode::Survival,
        }
    }

    /// `difficulty` by name or by number, `easy` when absent or unknown.
    fn difficulty(&self) -> crate::world::player::Difficulty {
        use crate::world::player::Difficulty;
        match self
            .text("difficulty")
            .as_deref()
            .map(str::to_ascii_lowercase)
        {
            Some(t) if t == "peaceful" || t == "0" => Difficulty::Peaceful,
            Some(t) if t == "normal" || t == "2" => Difficulty::Normal,
            Some(t) if t == "hard" || t == "3" => Difficulty::Hard,
            _ => Difficulty::Easy,
        }
    }

    fn initial_data_packs(&self) -> crate::datapack::DataPackConfig {
        let list = |key: &str| -> Option<Vec<String>> {
            self.raw(key).map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
        };
        let default = crate::datapack::DataPackConfig::default();
        crate::datapack::DataPackConfig {
            enabled: list("initial-enabled-packs").unwrap_or(default.enabled),
            disabled: list("initial-disabled-packs").unwrap_or(default.disabled),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_storage::admin::{OpEntry, PermissionLevel};

    fn user(name: &str) -> NameAndId {
        NameAndId {
            uuid: crate::auth::offline_uuid(name),
            name: name.into(),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn initial_packs_come_from_the_properties() {
        let default = ServerProperties::parse("");
        assert_eq!(
            default.initial_data_packs(),
            crate::datapack::DataPackConfig::default()
        );
        let set = ServerProperties::parse(
            "initial-enabled-packs=vanilla, trade_rebalance\ninitial-disabled-packs=file/old\n",
        );
        let config = set.initial_data_packs();
        assert_eq!(config.enabled, ["vanilla", "trade_rebalance"]);
        assert_eq!(config.disabled, ["file/old"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn untrusted_ops_get_no_whitelist_or_player_limit_bypass() {
        let mut lists = UserLists::default();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        lists.ops.push(OpEntry {
            user: user("A"),
            level: PermissionLevel::Owners,
            bypasses_player_limit: true,
        });
        assert!(can_login(&mut lists, &user("A"), ip, true, 0, 20, true).is_ok());
        assert!(can_login(&mut lists, &user("A"), ip, true, 20, 20, true).is_ok());
        assert_eq!(
            can_login(&mut lists, &user("A"), ip, true, 0, 20, false)
                .unwrap_err()
                .translate
                .unwrap()
                .into_string(),
            "multiplayer.disconnect.not_whitelisted"
        );
        lists.whitelist.push(user("A"));
        assert_eq!(
            can_login(&mut lists, &user("A"), ip, true, 20, 20, false)
                .unwrap_err()
                .translate
                .unwrap()
                .into_string(),
            "multiplayer.disconnect.server_full"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn login_checks_follow_the_games_order() {
        let mut lists = UserLists::default();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let key = |result: Result<(), TextComponent>| {
            result.unwrap_err().translate.unwrap().into_string()
        };
        assert_eq!(
            key(can_login(&mut lists, &user("A"), ip, true, 0, 20, true)),
            "multiplayer.disconnect.not_whitelisted"
        );
        assert!(can_login(&mut lists, &user("A"), ip, false, 0, 20, true).is_ok());
        lists.whitelist.push(user("A"));
        assert!(can_login(&mut lists, &user("A"), ip, true, 0, 20, true).is_ok());
        assert_eq!(
            key(can_login(&mut lists, &user("A"), ip, true, 20, 20, true)),
            "multiplayer.disconnect.server_full"
        );
        lists.ops.push(OpEntry {
            user: user("A"),
            level: PermissionLevel::Owners,
            bypasses_player_limit: true,
        });
        assert!(can_login(&mut lists, &user("A"), ip, true, 20, 20, true).is_ok());
        assert_eq!(lists.permission(user("A").uuid), PermissionLevel::Owners);
        assert_eq!(lists.permission(user("B").uuid), PermissionLevel::All);
        let mut ban = BanEntry::new(user("A"), "Server".into(), Some("bye".into()));
        ban.expires = Some(1_700_000_000);
        lists.bans.push(ban);
        assert!(
            can_login(&mut lists, &user("A"), ip, true, 0, 20, true).is_ok(),
            "expired bans are dropped"
        );
        assert!(lists.bans.is_empty());
        let mut ban = BanEntry::new(user("A"), "Server".into(), Some("bye".into()));
        ban.expires = Some(4_102_444_800);
        lists.bans.push(ban);
        let reason = can_login(&mut lists, &user("A"), ip, true, 0, 20, true).unwrap_err();
        assert_eq!(
            reason.translate.as_deref(),
            Some("multiplayer.disconnect.banned.reason")
        );
        assert_eq!(reason.with[0].text, "bye");
        assert_eq!(
            reason.extra[0].translate.as_deref(),
            Some("multiplayer.disconnect.banned.expiration")
        );
        assert_eq!(reason.extra[0].with[0].text, "2100-01-01 at 00:00:00 UTC");
        lists.bans.clear();
        lists
            .ip_bans
            .push(BanEntry::new("127.0.0.1".to_owned(), "Server".into(), None));
        let reason = can_login(&mut lists, &user("A"), ip, true, 0, 20, true).unwrap_err();
        assert_eq!(
            reason.translate.as_deref(),
            Some("multiplayer.disconnect.banned_ip.reason")
        );
        assert_eq!(
            reason.with[0].translate.as_deref(),
            Some("multiplayer.disconnect.banned.reason.default")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn defaults_match_the_game() {
        let p = ServerProperties::parse("");
        assert_eq!(p.max_players(), 20);
        assert_eq!(p.view_distance(), 10);
        assert_eq!(p.spawn_protection(), 16);
        assert_eq!(p.network_compression_threshold(), 256);
        assert_eq!(p.motd(), "A Minecraft Server");
        assert!(p.online_mode() && p.enable_status() && p.log_ips() && p.pvp());
        assert!(!p.hardcore() && !p.allow_flight() && !p.accepts_transfers());
        assert_eq!(p.level_name(), "world");
        assert_eq!(p.server_port(), 25565);
    }

    #[tuff_core::watchdog]
    #[test]
    fn values_are_parsed_and_clamped_like_the_game() {
        let p = ServerProperties::parse(
            "max-players=5\nview-distance=99\nsimulation-distance=1\nhardcore=TRUE\n\
             entity-broadcast-range-percentage=5\nspawn-protection=oops\n\
             difficulty=hard\nserver-port=70000\nmotd=hi there\nnetwork-compression-threshold=-5\n",
        );
        assert_eq!(p.max_players(), 5);
        assert_eq!(p.view_distance(), 32);
        assert_eq!(p.simulation_distance(), 3);
        assert!(p.hardcore());
        assert_eq!(p.entity_broadcast_range_percentage(), 10);
        assert_eq!(p.spawn_protection(), 16);
        assert_eq!(p.difficulty(), crate::world::player::Difficulty::Hard);
        assert_eq!(p.server_port(), 25565);
        assert_eq!(p.motd(), "hi there");
        assert_eq!(p.network_compression_threshold(), -1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn gamemode_and_seed_are_parsed() {
        use crate::protocol::play::GameMode;
        let p = ServerProperties::parse("gamemode=Creative\nlevel-seed=-42\n");
        assert_eq!(p.gamemode(), GameMode::Creative);
        assert_eq!(p.level_seed_value(), Some(-42));
        let p = ServerProperties::parse("gamemode=3\nlevel-seed=abc\n");
        assert_eq!(p.gamemode(), GameMode::Spectator);
        assert_eq!(p.level_seed_value(), Some(96354));
        let p = ServerProperties::parse("");
        assert_eq!(p.gamemode(), GameMode::Survival);
        assert_eq!(p.level_seed_value(), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn spam_links_conduct_and_level_type_properties_parse() {
        let p = ServerProperties::parse("");
        assert_eq!(p.chat_spam_threshold_seconds(), 10);
        assert_eq!(p.command_spam_threshold_seconds(), 10);
        assert_eq!(p.bug_report_link(), None);
        assert!(!p.enable_code_of_conduct());
        assert_eq!(p.level_type(), "normal");
        assert_eq!(p.generator_settings(), "{}");
        let p = ServerProperties::parse(
            "chat-spam-threshold-seconds=3\ncommand-spam-threshold-seconds=-4\n\
             bug-report-link=https://example.com/bugs\nenable-code-of-conduct=true\n\
             level-type=minecraft:LARGE_BIOMES\n",
        );
        assert_eq!(p.chat_spam_threshold_seconds(), 3);
        assert_eq!(p.command_spam_threshold_seconds(), 0);
        assert_eq!(
            p.bug_report_link().as_deref(),
            Some("https://example.com/bugs")
        );
        assert!(p.enable_code_of_conduct());
        assert_eq!(p.level_type(), "large_biomes");
        let p = ServerProperties::parse("bug-report-link=ftp://example.com\nlevel-name=lobby\n");
        assert_eq!(p.bug_report_link(), None);
        assert_eq!(p.level_name(), "lobby");
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_properties_resource_pack_is_offered() {
        assert!(ServerProperties::parse("").resource_pack_offer().is_none());
        let p = ServerProperties::parse(
            "resource-pack=http://x/p.zip\nresource-pack-sha1=abc\nrequire-resource-pack=true\n\
             resource-pack-prompt={\"text\":\"hi\"}\n",
        );
        let offer = p.resource_pack_offer().unwrap();
        assert_eq!(offer.url, "http://x/p.zip");
        assert_eq!(offer.sha1, "abc");
        assert!(offer.required && offer.prompt.is_some());
        assert_eq!(offer.id, name_uuid("http://x/p.zip"));
        let p = ServerProperties::parse(
            "resource-pack=u\nresource-pack-id=00000000-0000-0000-0000-000000000007\n",
        );
        assert_eq!(p.resource_pack_offer().unwrap().id, 7);
    }

    #[tuff_core::watchdog]
    #[test]
    fn unsupported_keys_are_kept() {
        let p = ServerProperties::parse("enable-rcon=true\n");
        assert!(p.to_text().contains("enable-rcon=true"));
    }
}
