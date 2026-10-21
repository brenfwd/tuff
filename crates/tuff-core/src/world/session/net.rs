use super::*;

/// Counts a repeated action up by a fixed step and down by one per tick.
#[derive(Clone, Copy, Debug)]
pub struct TickThrottler {
    step: u32,
    threshold: u32,
    count: u32,
    last_tick: u64,
}

impl TickThrottler {
    pub fn new(step: u32, threshold: u32) -> Self {
        Self {
            step,
            threshold,
            count: 0,
            last_tick: 0,
        }
    }

    pub fn set_threshold(&mut self, threshold: u32) {
        self.threshold = threshold;
    }

    /// Records one action at `tick`; true while the total stays under the
    /// threshold. A zero threshold never trips.
    pub fn record(&mut self, tick: u64) -> bool {
        let elapsed = tick.saturating_sub(self.last_tick);
        self.last_tick = tick;
        self.count = self
            .count
            .saturating_sub(u32::try_from(elapsed).unwrap_or(u32::MAX))
            .saturating_add(self.step);
        self.threshold == 0 || self.count < self.threshold
    }
}

/// Keep-alive, idle and packet-rate bookkeeping for the connection.
pub struct Net {
    pub keep_alive: Option<(i64, Instant)>,
    pub last_keep_alive: Instant,
    pub keep_alive_interval: Duration,
    pub keep_alive_timeout: Duration,
    /// Kick after this long without a deliberate action; `None` never.
    pub idle_timeout: Option<Duration>,
    pub last_action: Instant,
    /// Most packets accepted per second; 0 is unlimited.
    pub rate_limit: u32,
    pub(super) rate_meter: rate::RateMeter,
    pub latency_ms: i32,
    pub chat_spam: TickThrottler,
    pub command_spam: TickThrottler,
}

impl Net {
    pub(super) fn new(network: &crate::world::NetworkSettings, now: Instant) -> Self {
        Self {
            keep_alive: None,
            last_keep_alive: now,
            keep_alive_interval: KEEP_ALIVE_INTERVAL,
            keep_alive_timeout: KEEP_ALIVE_TIMEOUT,
            idle_timeout: network.idle_timeout,
            last_action: now,
            rate_limit: network.rate_limit,
            rate_meter: rate::RateMeter::new(now),
            latency_ms: 0,
            chat_spam: TickThrottler::new(20, 20 * network.chat_spam_threshold_seconds),
            command_spam: TickThrottler::new(20, 20 * network.command_spam_threshold_seconds),
        }
    }

    pub(super) fn idle_due(&self) -> Instant {
        match self.idle_timeout {
            Some(timeout) => self.last_action + timeout,
            None => Instant::now() + Duration::from_secs(3600),
        }
    }

    /// When the pending keep-alive times out, or the next one is due.
    pub(super) fn keep_alive_due(&self) -> Instant {
        match self.keep_alive {
            Some((_, sent)) => sent + self.keep_alive_timeout,
            None => self.last_keep_alive + self.keep_alive_interval,
        }
    }
}

impl PlayerSession {
    /// Takes up a server.properties value the coordinator changed.
    pub(super) async fn apply_live_change(&mut self, change: crate::world::live::LiveChange) {
        use crate::world::live::LiveChange;
        match change {
            LiveChange::Connection => {
                let network = self.shared.live.network();
                self.net.idle_timeout = network.idle_timeout;
                self.net.rate_limit = network.rate_limit;
                self.net
                    .chat_spam
                    .set_threshold(20 * network.chat_spam_threshold_seconds);
                self.net
                    .command_spam
                    .set_threshold(20 * network.command_spam_threshold_seconds);
            }
            LiveChange::ViewDistance => {
                self.send(&crate::protocol::play::SetChunkCacheRadius(
                    self.shared.live.view_distance(),
                ));
                self.update_view().await;
            }
            LiveChange::SimulationDistance => {
                self.send(&crate::protocol::play::SetSimulationDistance(
                    self.shared.live.simulation_distance(),
                ));
            }
            LiveChange::BroadcastRange => self.update_view().await,
            LiveChange::GameMode => {
                let forced = self
                    .shared
                    .force_game_mode
                    .load(std::sync::atomic::Ordering::Relaxed);
                if forced {
                    self.set_game_mode(self.shared.live.default_game_mode())
                        .await;
                }
            }
        }
    }

    pub(super) fn over_rate_limit(&mut self) -> bool {
        if self.net.rate_limit == 0 || self.disconnected {
            return false;
        }
        if !self
            .net
            .rate_meter
            .record(Instant::now(), self.net.rate_limit)
        {
            return false;
        }
        self.kick(TextComponent::translate(
            "disconnect.exceeded_packet_rate",
            [],
        ));
        true
    }

    /// Kicks a non-operator whose chat or commands outpace the threshold.
    pub(super) fn detect_spam(&mut self, command: bool) {
        let tick = self.tick;
        let within = if command {
            self.net.command_spam.record(tick)
        } else {
            self.net.chat_spam.record(tick)
        };
        if !within && self.mode.permission < PermissionLevel::Gamemasters {
            self.kick(TextComponent::translate("disconnect.spam", []));
        }
    }

    pub(super) fn note_activity(&mut self, packet: &Serverbound) {
        let deliberate = match packet {
            Serverbound::KeepAlive(_)
            | Serverbound::Pong(_)
            | Serverbound::ClientTickEnd
            | Serverbound::ChunkBatchReceived(_)
            | Serverbound::AcceptTeleportation { .. } => false,
            Serverbound::Move(movement) => {
                movement.position.is_some_and(|p| p != self.pose.position)
                    || movement.rotation.is_some_and(|(yaw, pitch)| {
                        yaw != self.pose.yaw || pitch != self.pose.pitch
                    })
            }
            _ => true,
        };
        if deliberate {
            self.net.last_action = Instant::now();
        }
    }

    pub(super) fn check_idle(&mut self) {
        if let Some(timeout) = self.net.idle_timeout
            && self.net.last_action.elapsed() >= timeout
        {
            self.kick(TextComponent::translate(
                "multiplayer.disconnect.idling",
                [],
            ));
        }
    }

    /// Kicks a client that left the last keep-alive unanswered, or sends
    /// the next one. Driven by its own timer, never by the game tick, so
    /// a frozen or stalled clock cannot time clients out.
    pub(super) fn check_keep_alive(&mut self) {
        let now = Instant::now();
        if now < self.net.keep_alive_due() {
            return;
        }
        if self.net.keep_alive.is_some() {
            log::info!("{} timed out: no keep-alive reply", self.profile.name);
            self.kick(TextComponent::translate("disconnect.timeout", []));
            return;
        }
        let value = self.rng.r#gen::<i64>();
        self.net.keep_alive = Some((value, now));
        self.net.last_keep_alive = now;
        self.send(&play::KeepAlive(value));
    }
}

#[cfg(test)]
mod tests {
    use super::TickThrottler;

    #[tuff_core::watchdog]
    #[test]
    fn the_throttler_trips_on_a_burst_and_decays_with_ticks() {
        let mut throttler = TickThrottler::new(20, 40);
        assert!(throttler.record(0));
        assert!(!throttler.record(0));
        assert!(throttler.record(30));
        assert!(!throttler.record(30));
        let mut off = TickThrottler::new(20, 0);
        assert!((0..10).all(|_| off.record(0)));
    }
}
