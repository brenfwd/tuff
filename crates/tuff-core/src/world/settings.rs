use super::*;

/// Connection policy from `server.properties`; the coordinator keeps the
/// live part current in [`live::LiveSettings`].
#[derive(Clone, Debug)]
pub struct NetworkSettings {
    /// Negative disables compression.
    pub compression_threshold: i32,
    pub enable_status: bool,
    pub accepts_transfers: bool,
    pub allow_flight: bool,
    pub log_ips: bool,
    pub idle_timeout: Option<std::time::Duration>,
    /// Packets per second a client may send; 0 is unlimited.
    pub rate_limit: u32,
    /// Unauthenticated connections allowed at once; 0 is unlimited.
    pub pre_login_max_connections: usize,
    /// Unauthenticated connections allowed per address (IPv6 by /64); 0 is unlimited.
    pub pre_login_max_per_ip: usize,
    pub pre_login_timeout: std::time::Duration,
    pub online_mode: bool,
    pub port: u16,
    pub ip: String,
    /// The pack from `resource-pack`, offered to every joining player.
    pub resource_pack: Option<crate::resource_pack::PackOffer>,
    /// Seconds of chat or command spam a player may build up before being
    /// kicked; 0 disables the check.
    pub chat_spam_threshold_seconds: u32,
    pub command_spam_threshold_seconds: u32,
    /// The url `bug-report-link` offers clients in the server links.
    pub bug_report_link: Option<String>,
    /// Every link offered to clients in the server links packet.
    pub server_links: Vec<crate::protocol::configuration::ServerLink>,
    /// `broadcast-console-to-ops`.
    pub broadcast_console_to_ops: bool,
    /// `enable-code-of-conduct`.
    pub code_of_conduct_enabled: bool,
    /// Code of conduct texts by language; empty when it is not enabled.
    pub code_of_conduct: Vec<(String, String)>,
    /// The border may not reach further than this from the origin.
    pub max_world_size: i32,
    /// Sends the client address to the session server at login.
    pub prevent_proxy_connections: bool,
    /// Where `server.properties` and the code of conduct folder live;
    /// `None` for worlds with no instance directory (tests).
    pub instance_dir: Option<PathBuf>,
    /// `enable-query` and `query.port`, read once at startup.
    pub query_port: Option<u16>,
    /// `level-name`, reported as the map by the query protocol.
    pub level_name: String,
}

impl NetworkSettings {
    /// Takes every value `properties` may change while the server runs;
    /// the listener, online mode and code of conduct texts stay as they
    /// are.
    pub fn apply(&mut self, properties: &tuff_storage::admin::ServerProperties) {
        self.compression_threshold = properties.network_compression_threshold();
        self.enable_status = properties.enable_status();
        self.accepts_transfers = properties.accepts_transfers();
        self.allow_flight = properties.allow_flight();
        self.log_ips = properties.log_ips();
        self.idle_timeout = match properties.player_idle_timeout_minutes() {
            0 => None,
            minutes => Some(std::time::Duration::from_secs(minutes as u64 * 60)),
        };
        self.rate_limit = properties.rate_limit() as u32;
        self.pre_login_max_connections = properties.pre_login_max_connections();
        self.pre_login_max_per_ip = properties.pre_login_max_connections_per_ip();
        self.pre_login_timeout =
            std::time::Duration::from_secs(properties.pre_login_timeout_seconds());
        self.resource_pack = properties.resource_pack_offer();
        self.chat_spam_threshold_seconds = properties.chat_spam_threshold_seconds() as u32;
        self.command_spam_threshold_seconds = properties.command_spam_threshold_seconds() as u32;
        self.bug_report_link = properties.bug_report_link();
        self.server_links = self
            .bug_report_link
            .iter()
            .map(|url| {
                crate::protocol::configuration::ServerLink::known(
                    crate::protocol::configuration::KnownLinkType::BugReport,
                    url.clone(),
                )
            })
            .collect();
        self.broadcast_console_to_ops = properties.broadcast_console_to_ops();
        self.code_of_conduct_enabled = properties.enable_code_of_conduct();
        if !self.code_of_conduct_enabled {
            self.code_of_conduct.clear();
        }
        self.max_world_size = properties.max_world_size();
        self.prevent_proxy_connections = properties.prevent_proxy_connections();
    }
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            compression_threshold: COMPRESSION_THRESHOLD as i32,
            enable_status: true,
            accepts_transfers: false,
            allow_flight: false,
            log_ips: true,
            idle_timeout: None,
            rate_limit: 0,
            pre_login_max_connections: 512,
            pre_login_max_per_ip: 8,
            pre_login_timeout: std::time::Duration::from_secs(30),
            online_mode: true,
            port: 25565,
            ip: String::new(),
            resource_pack: None,
            chat_spam_threshold_seconds: 10,
            command_spam_threshold_seconds: 10,
            bug_report_link: None,
            server_links: Vec::new(),
            broadcast_console_to_ops: true,
            code_of_conduct_enabled: false,
            code_of_conduct: Vec::new(),
            max_world_size: border::ABSOLUTE_MAX,
            prevent_proxy_connections: false,
            instance_dir: None,
            query_port: None,
            level_name: String::new(),
        }
    }
}

/// Simulation policy read from `server.properties` at startup; the
/// coordinator keeps it current in [`live::LiveSettings`].
#[derive(Clone, Copy, Debug)]
pub struct SimulationSettings {
    /// Chunks around a player (Chebyshev) that tick; beyond the view
    /// distance this changes nothing.
    pub simulation_distance: i32,
    /// Seconds with no players before the game stops ticking; negative
    /// never pauses.
    pub pause_when_empty_seconds: i32,
    /// `entity-broadcast-range-percentage`: scales how far away other
    /// entities are shown to a player.
    pub broadcast_range_percent: i32,
}

impl Default for SimulationSettings {
    fn default() -> Self {
        Self {
            simulation_distance: DEFAULT_VIEW_DISTANCE,
            pause_when_empty_seconds: 60,
            broadcast_range_percent: 100,
        }
    }
}
