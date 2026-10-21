//! Operators, the whitelist, bans and `server.properties` as the server
//! holds them in memory. Where they are kept is up to a
//! [`ConfigStore`](crate::config::ConfigStore).

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::{SystemTime, UNIX_EPOCH};

/// Vanilla's command permission levels, lowest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PermissionLevel {
    All,
    Moderators,
    Gamemasters,
    Admins,
    Owners,
}

impl PermissionLevel {
    pub fn id(self) -> i8 {
        self as i8
    }

    pub fn from_id(id: i64) -> Option<Self> {
        Some(match id {
            0 => Self::All,
            1 => Self::Moderators,
            2 => Self::Gamemasters,
            3 => Self::Admins,
            4 => Self::Owners,
            _ => return None,
        })
    }

    pub fn from_id_clamped(id: i64) -> Self {
        Self::from_id(id.clamp(0, 4)).unwrap_or(Self::All)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "all" => Self::All,
            "moderators" => Self::Moderators,
            "gamemasters" => Self::Gamemasters,
            "admins" => Self::Admins,
            "owners" => Self::Owners,
            _ => return None,
        })
    }

    /// Whether this level may use target selectors.
    pub fn allows_selectors(self) -> bool {
        self >= Self::Gamemasters
    }
}

/// The `chat/*` permission atoms. A dedicated server grants every one of
/// them at every level, so they only gate chat for a level that withholds
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatPermission {
    SendMessages,
    SendCommands,
    ReceivePlayerMessages,
    ReceiveSystemMessages,
}

impl ChatPermission {
    pub const ALL: [Self; 4] = [
        Self::SendMessages,
        Self::SendCommands,
        Self::ReceivePlayerMessages,
        Self::ReceiveSystemMessages,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::SendMessages => "chat/send_messages",
            Self::SendCommands => "chat/send_commands",
            Self::ReceivePlayerMessages => "chat/receive_player_messages",
            Self::ReceiveSystemMessages => "chat/receive_system_messages",
        }
    }
}

impl PermissionLevel {
    /// Whether this level holds the chat permission `atom`.
    pub fn grants_chat(self, _atom: ChatPermission) -> bool {
        true
    }
}

pub mod user_cache;
pub use user_cache::{UserCache, UserCacheEntry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameAndId {
    pub uuid: u128,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpEntry {
    pub user: NameAndId,
    pub level: PermissionLevel,
    pub bypasses_player_limit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BanEntry<K> {
    pub key: K,
    /// Seconds since the epoch.
    pub created: i64,
    pub source: String,
    pub expires: Option<i64>,
    pub reason: Option<String>,
}

impl<K> BanEntry<K> {
    pub fn new(key: K, source: String, reason: Option<String>) -> Self {
        Self {
            key,
            created: now(),
            source,
            expires: None,
            reason,
        }
    }

    pub fn expired(&self, at: i64) -> bool {
        self.expires.is_some_and(|expires| expires < at)
    }
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `(year, month, day)` of a day count since 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `yyyy-MM-dd HH:mm:ss Z` in UTC, as the ban files store dates.
pub fn format_file_date(seconds: i64) -> String {
    let (y, m, d) = civil_from_days(seconds.div_euclid(86_400));
    let secs = seconds.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} +0000",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// `yyyy-MM-dd 'at' HH:mm:ss z`, as the ban disconnect message shows expiry.
pub fn format_message_date(seconds: i64) -> String {
    let (y, m, d) = civil_from_days(seconds.div_euclid(86_400));
    let secs = seconds.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} at {:02}:{:02}:{:02} UTC",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Parses `yyyy-MM-dd HH:mm:ss Z` (any numeric zone) to epoch seconds.
pub fn parse_file_date(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once(' ')?;
    let (time, zone) = rest.split_once(' ')?;
    let mut date_parts = date.split('-').map(|p| p.parse::<i64>());
    let (y, m, d) = (
        date_parts.next()?.ok()?,
        date_parts.next()?.ok()?,
        date_parts.next()?.ok()?,
    );
    let mut time_parts = time.split(':').map(|p| p.parse::<i64>());
    let (h, min, s) = (
        time_parts.next()?.ok()?,
        time_parts.next()?.ok()?,
        time_parts.next()?.ok()?,
    );
    let (sign, digits) = zone.split_at(1);
    let offset: i64 = digits.parse().ok()?;
    let offset = (offset / 100) * 3600 + (offset % 100) * 60;
    let offset = if sign == "-" { -offset } else { offset };
    Some(days_from_civil(y, m as u32, d as u32) * 86_400 + h * 3600 + min * 60 + s - offset)
}

/// Every stored list, in memory.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserLists {
    pub ops: Vec<OpEntry>,
    pub whitelist: Vec<NameAndId>,
    pub bans: Vec<BanEntry<NameAndId>>,
    pub ip_bans: Vec<BanEntry<String>>,
}

impl UserLists {
    pub fn op(&self, uuid: u128) -> Option<&OpEntry> {
        self.ops.iter().find(|op| op.user.uuid == uuid)
    }

    pub fn is_op(&self, uuid: u128) -> bool {
        self.op(uuid).is_some()
    }

    pub fn is_whitelisted(&self, uuid: u128) -> bool {
        self.whitelist.iter().any(|user| user.uuid == uuid)
    }

    /// Drops expired bans, as the game does before every lookup.
    pub fn expire(&mut self) {
        let at = now();
        self.bans.retain(|ban| !ban.expired(at));
        self.ip_bans.retain(|ban| !ban.expired(at));
    }

    pub fn ban(&mut self, uuid: u128) -> Option<&BanEntry<NameAndId>> {
        self.bans.iter().find(|ban| ban.key.uuid == uuid)
    }

    pub fn ip_ban(&mut self, ip: &str) -> Option<&BanEntry<String>> {
        self.ip_bans.iter().find(|ban| ban.key == ip)
    }

    /// The permission level a player gets on joining.
    pub fn permission(&self, uuid: u128) -> PermissionLevel {
        self.op(uuid).map_or(PermissionLevel::All, |op| op.level)
    }
}

/// An address as the ban list keys it: the bare IP text.
pub fn address_text(address: IpAddr) -> String {
    address.to_string()
}

mod typed;
pub use typed::parse_seed;

/// The `server.properties` keys this server reads, with vanilla's
/// defaults. Other keys in an existing file are preserved.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerProperties {
    pub white_list: bool,
    pub enforce_whitelist: bool,
    pub op_permission_level: PermissionLevel,
    pub function_permission_level: PermissionLevel,
    others: BTreeMap<String, String>,
}

impl Default for ServerProperties {
    fn default() -> Self {
        Self {
            white_list: true,
            enforce_whitelist: false,
            op_permission_level: PermissionLevel::Owners,
            function_permission_level: PermissionLevel::Gamemasters,
            others: BTreeMap::new(),
        }
    }
}

impl ServerProperties {
    pub fn parse(text: &str) -> Self {
        Self::from_entries(text.lines().filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_owned(), value.trim().to_owned()))
        }))
    }

    pub fn from_entries(entries: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut properties = Self::default();
        for (key, value) in entries {
            match key.as_str() {
                "white-list" => properties.white_list = value.eq_ignore_ascii_case("true"),
                "enforce-whitelist" => {
                    properties.enforce_whitelist = value.eq_ignore_ascii_case("true")
                }
                "op-permission-level" => {
                    if let Some(level) = value.parse().ok().and_then(PermissionLevel::from_id) {
                        properties.op_permission_level = level;
                    }
                }
                "function-permission-level" => {
                    if let Some(level) = value.parse().ok().and_then(PermissionLevel::from_id) {
                        properties.function_permission_level = level;
                    }
                }
                _ => {
                    properties.others.insert(key, value);
                }
            }
        }
        properties
    }

    /// The raw text of any key, modeled or not.
    pub fn raw(&self, key: &str) -> Option<std::borrow::Cow<'_, str>> {
        match key {
            "white-list" => Some(self.white_list.to_string().into()),
            "enforce-whitelist" => Some(self.enforce_whitelist.to_string().into()),
            "op-permission-level" => Some(self.op_permission_level.id().to_string().into()),
            "function-permission-level" => {
                Some(self.function_permission_level.id().to_string().into())
            }
            _ => self.others.get(key).map(|value| value.as_str().into()),
        }
    }

    /// The `generate-structures` key, true when absent or unparsable.
    pub fn generate_structures(&self) -> bool {
        self.others
            .get("generate-structures")
            .is_none_or(|value| value != "false")
    }

    /// The feature packs forced on at every startup (`feature-packs`, a
    /// comma list such as `minecart_improvements`); `None` leaves a saved
    /// world's flags as they are.
    pub fn startup_feature_packs(&self) -> Option<Vec<String>> {
        self.others.get("feature-packs").map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .collect()
        })
    }

    /// Every key and value, modeled or not, sorted by key.
    pub fn entries(&self) -> BTreeMap<String, String> {
        let mut entries = self.others.clone();
        entries.insert("white-list".into(), self.white_list.to_string());
        entries.insert(
            "enforce-whitelist".into(),
            self.enforce_whitelist.to_string(),
        );
        entries.insert(
            "op-permission-level".into(),
            self.op_permission_level.id().to_string(),
        );
        entries.insert(
            "function-permission-level".into(),
            self.function_permission_level.id().to_string(),
        );
        entries
    }

    pub fn to_text(&self) -> String {
        let mut out = String::from("#Minecraft server properties\n");
        for (key, value) in self.entries() {
            out.push_str(&key);
            out.push('=');
            out.push_str(&value);
            out.push('\n');
        }
        out
    }
}

pub fn parse_uuid_text(text: &str) -> Option<u128> {
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() != 5 {
        return None;
    }
    let mut value: u128 = 0;
    for (part, bits) in parts.iter().zip([32u32, 16, 16, 16, 48]) {
        if part.is_empty() || part.len() > 16 {
            return None;
        }
        let piece = u64::from_str_radix(part, 16).ok()?;
        if bits < 64 && piece >> bits != 0 {
            return None;
        }
        value = (value << bits) | u128::from(piece);
    }
    Some(value)
}

pub fn format_uuid(uuid: u128) -> String {
    let hex = format!("{uuid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn every_level_holds_every_chat_permission() {
        let levels = [
            PermissionLevel::All,
            PermissionLevel::Moderators,
            PermissionLevel::Gamemasters,
            PermissionLevel::Admins,
            PermissionLevel::Owners,
        ];
        for level in levels {
            for atom in ChatPermission::ALL {
                assert!(level.grants_chat(atom), "{level:?} lacks {}", atom.name());
            }
        }
        assert_eq!(ChatPermission::SendCommands.name(), "chat/send_commands");
    }

    #[tuff_core::watchdog]
    #[test]
    fn dates_format_and_parse_like_the_game() {
        assert_eq!(format_file_date(0), "1970-01-01 00:00:00 +0000");
        assert_eq!(format_file_date(1_700_000_000), "2023-11-14 22:13:20 +0000");
        assert_eq!(
            format_message_date(1_700_000_000),
            "2023-11-14 at 22:13:20 UTC"
        );
        assert_eq!(
            parse_file_date("2023-11-14 22:13:20 +0000"),
            Some(1_700_000_000)
        );
        assert_eq!(
            parse_file_date("2023-11-14 23:13:20 +0100"),
            Some(1_700_000_000)
        );
        assert_eq!(parse_file_date("forever"), None);
        for seconds in [86_399, 951_782_400, 4_102_444_800] {
            assert_eq!(parse_file_date(&format_file_date(seconds)), Some(seconds));
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn properties_default_to_vanilla_and_keep_other_keys() {
        let properties = ServerProperties::default();
        assert!(properties.white_list);
        let text = properties.to_text();
        assert!(text.starts_with("#Minecraft server properties\n"));
        assert!(text.contains("white-list=true\n"));
        assert!(text.contains("op-permission-level=4\n"));
        assert!(text.contains("function-permission-level=2\n"));
        assert!(text.contains("enforce-whitelist=false\n"));
        let parsed = ServerProperties::parse(
            "# comment\nmotd=Hi\nwhite-list=false\nop-permission-level=2\nfunction-permission-level=4\n",
        );
        assert!(!parsed.white_list);
        assert_eq!(parsed.op_permission_level, PermissionLevel::Gamemasters);
        assert_eq!(parsed.function_permission_level, PermissionLevel::Owners);
        assert!(parsed.to_text().contains("motd=Hi\n"));
        assert_eq!(ServerProperties::parse(&parsed.to_text()), parsed);
        assert_eq!(ServerProperties::from_entries(parsed.entries()), parsed);
    }
}
