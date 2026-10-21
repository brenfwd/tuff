use super::ServerProperties;

/// A seed as a number, or the string hash of the text when it is not one.
pub fn parse_seed(text: &str) -> i64 {
    text.parse::<i64>().unwrap_or_else(|_| {
        text.encode_utf16()
            .fold(0i32, |h, unit| h.wrapping_mul(31).wrapping_add(unit as i32)) as i64
    })
}

fn parse_bool(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case("true")
}

impl ServerProperties {
    pub fn text(&self, key: &str) -> Option<String> {
        self.raw(key).map(|text| text.trim().to_owned())
    }

    pub fn flag(&self, key: &str, default: bool) -> bool {
        self.text(key).map_or(default, |text| parse_bool(&text))
    }

    pub fn number(&self, key: &str, default: i64) -> i64 {
        self.text(key)
            .and_then(|text| text.parse().ok())
            .unwrap_or(default)
    }

    pub fn motd(&self) -> String {
        self.raw("motd")
            .map_or_else(|| "A Minecraft Server".to_owned(), |text| text.into_owned())
    }

    pub fn max_players(&self) -> u32 {
        self.number("max-players", 20).clamp(0, i32::MAX as i64) as u32
    }

    pub fn view_distance(&self) -> i32 {
        self.number("view-distance", 10).clamp(3, 32) as i32
    }

    pub fn simulation_distance(&self) -> i32 {
        self.number("simulation-distance", 10).clamp(3, 32) as i32
    }

    pub fn spawn_protection(&self) -> i32 {
        self.number("spawn-protection", 16)
            .clamp(0, i32::MAX as i64) as i32
    }

    pub fn entity_broadcast_range_percentage(&self) -> i32 {
        self.number("entity-broadcast-range-percentage", 100)
            .clamp(10, 1000) as i32
    }

    pub fn network_compression_threshold(&self) -> i32 {
        self.number("network-compression-threshold", 256)
            .clamp(-1, i32::MAX as i64) as i32
    }

    pub fn player_idle_timeout_minutes(&self) -> i32 {
        self.number("player-idle-timeout", 0)
            .clamp(0, i32::MAX as i64) as i32
    }

    pub fn pause_when_empty_seconds(&self) -> i32 {
        self.number("pause-when-empty-seconds", 60)
            .clamp(-1, i32::MAX as i64) as i32
    }

    pub fn pre_login_max_connections(&self) -> usize {
        self.number("pre-login-max-connections", 512)
            .clamp(0, 1 << 20) as usize
    }

    pub fn pre_login_max_connections_per_ip(&self) -> usize {
        self.number("pre-login-max-connections-per-ip", 8)
            .clamp(0, 1 << 20) as usize
    }

    pub fn pre_login_timeout_seconds(&self) -> u64 {
        self.number("pre-login-timeout-seconds", 30).clamp(1, 600) as u64
    }

    pub fn rate_limit(&self) -> i32 {
        self.number("rate-limit", 0).clamp(0, i32::MAX as i64) as i32
    }

    pub fn chat_spam_threshold_seconds(&self) -> i32 {
        self.number("chat-spam-threshold-seconds", 10)
            .clamp(0, i32::MAX as i64) as i32
    }

    pub fn command_spam_threshold_seconds(&self) -> i32 {
        self.number("command-spam-threshold-seconds", 10)
            .clamp(0, i32::MAX as i64) as i32
    }

    /// The `bug-report-link` when it is an http or https url.
    pub fn bug_report_link(&self) -> Option<String> {
        let link = self.text("bug-report-link")?;
        let lower = link.to_ascii_lowercase();
        let rest = lower
            .strip_prefix("https://")
            .or_else(|| lower.strip_prefix("http://"))?;
        (!rest.is_empty() && !link.chars().any(|c| c.is_whitespace() || c.is_control()))
            .then_some(link)
    }

    pub fn enable_code_of_conduct(&self) -> bool {
        self.flag("enable-code-of-conduct", false)
    }

    /// `level-type` without the `minecraft:` namespace, lowercased.
    pub fn level_type(&self) -> String {
        let text = self
            .text("level-type")
            .unwrap_or_default()
            .to_ascii_lowercase();
        match text.strip_prefix("minecraft:") {
            Some(name) => name.to_owned(),
            None if text.is_empty() => "normal".to_owned(),
            None => text,
        }
    }

    pub fn generator_settings(&self) -> String {
        self.text("generator-settings")
            .unwrap_or_else(|| "{}".to_owned())
    }

    pub fn max_chained_neighbor_updates(&self) -> i32 {
        self.number("max-chained-neighbor-updates", 1_000_000)
            .clamp(i32::MIN as i64, i32::MAX as i64) as i32
    }

    pub fn max_world_size(&self) -> i32 {
        self.number("max-world-size", 29_999_984)
            .clamp(1, 29_999_984) as i32
    }

    pub fn server_port(&self) -> u16 {
        u16::try_from(self.number("server-port", 25565)).unwrap_or(25565)
    }

    pub fn server_ip(&self) -> String {
        self.text("server-ip").unwrap_or_default()
    }

    pub fn level_name(&self) -> String {
        self.text("level-name")
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "world".to_owned())
    }

    pub fn level_seed(&self) -> String {
        self.text("level-seed").unwrap_or_default()
    }

    pub fn broadcast_console_to_ops(&self) -> bool {
        self.flag("broadcast-console-to-ops", true)
    }

    pub fn prevent_proxy_connections(&self) -> bool {
        self.flag("prevent-proxy-connections", false)
    }

    pub fn online_mode(&self) -> bool {
        self.flag("online-mode", true)
    }

    pub fn allow_flight(&self) -> bool {
        self.flag("allow-flight", false)
    }

    pub fn force_gamemode(&self) -> bool {
        self.flag("force-gamemode", false)
    }

    pub fn hardcore(&self) -> bool {
        self.flag("hardcore", false)
    }

    pub fn pvp(&self) -> bool {
        self.flag("pvp", true)
    }

    pub fn enable_query(&self) -> bool {
        self.flag("enable-query", false)
    }

    pub fn query_port(&self) -> u16 {
        u16::try_from(self.number("query.port", 25565)).unwrap_or(25565)
    }

    pub fn enable_status(&self) -> bool {
        self.flag("enable-status", true)
    }

    pub fn hide_online_players(&self) -> bool {
        self.flag("hide-online-players", false)
    }

    pub fn log_ips(&self) -> bool {
        self.flag("log-ips", true)
    }

    pub fn accepts_transfers(&self) -> bool {
        self.flag("accepts-transfers", false)
    }

    pub fn text_filtering(&self) -> (String, i64) {
        (
            self.text("text-filtering-config").unwrap_or_default(),
            self.number("text-filtering-version", 0),
        )
    }

    pub fn enforce_secure_profile(&self) -> bool {
        self.flag("enforce-secure-profile", true)
    }

    /// The `level-seed` as a number, or its string hash when it is not one;
    /// `None` when empty.
    pub fn level_seed_value(&self) -> Option<i64> {
        let text = self.level_seed();
        (!text.is_empty()).then(|| parse_seed(&text))
    }
}
