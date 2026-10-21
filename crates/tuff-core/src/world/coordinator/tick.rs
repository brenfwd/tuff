use super::*;

impl Coordinator {
    /// Ticks, then either records the tick at once or, in lockstep, holds
    /// the clock until every barrier member has finished it. Returns when
    /// the next tick is due (see [`next_due`]).
    pub(super) fn start_tick(&mut self, due: tokio::time::Instant) -> tokio::time::Instant {
        let started = tokio::time::Instant::now();
        if let Some(ticks) = super::overloaded_by(
            due,
            started,
            self.tick_state().period(),
            self.last_overload_warning,
        ) {
            log::warn!(
                "Can't keep up! Is the server overloaded? Running {}ms or {ticks} ticks behind",
                started.saturating_duration_since(due).as_millis()
            );
            self.last_overload_warning = Some(started);
        }
        self.tick();
        let lockstep =
            self.sprint.is_some() || self.shared.tick_barrier.lockstep() == TickLockstep::Always;
        if lockstep {
            self.awaiting_barrier = Some((*self.clock.borrow(), started));
        } else {
            self.finish_tick_bookkeeping(started.elapsed());
        }
        next_due(due, started, self.tick_state().period())
    }

    pub(super) fn idle_paused(&self) -> bool {
        self.idle
            .paused(self.shared.live.pause_when_empty_seconds())
    }

    pub(super) fn count_empty_tick(&mut self) {
        self.idle.count(self.players.is_empty());
    }

    pub(super) fn tick(&mut self) {
        let _span = tuff_timings::span!("coordinator.tick");
        self.clock.send_modify(|tick| *tick += 1);
        let advance_time = self.game_rules.borrow().bool("advance_time");
        self.clocks.send_modify(|clocks| {
            clocks.set_advancing(advance_time);
            clocks.tick();
        });
        self.server_ticks.send_modify(|tick| *tick += 1);
        self.count_empty_tick();
        self.sync_time();
        self.sync_latency();
        let advance = self.game_rules.borrow().bool("advance_weather");
        let mut update = None;
        self.weather.send_modify(|weather| {
            update = weather.tick(advance, &mut self.rng);
        });
        if let Some(update) = update {
            for event in update.events {
                self.broadcast(&event);
            }
        }
        for border in &self.borders {
            border.send_if_modified(Border::tick);
        }
        self.tick_sleep();
        self.publish_index();
        self.tick_waypoints();
        self.autosave_level();
    }

    /// A tick while frozen: the game holds still, but players, the
    /// player index, sleeping and saving go on as they do in the game.
    pub(super) fn frozen_tick(&mut self) {
        let _span = tuff_timings::span!("coordinator.tick");
        self.server_ticks.send_modify(|tick| *tick += 1);
        self.count_empty_tick();
        self.sync_time();
        self.sync_latency();
        self.tick_sleep();
        self.publish_index();
        self.tick_waypoints();
        self.autosave_level();
    }

    /// Keeps every player's locator bar in step with the others, once
    /// something it depends on changed.
    pub(super) fn tick_waypoints(&mut self) {
        if self
            .server_ticks
            .borrow()
            .is_multiple_of(ENTITY_TRANSMITTER_POLL_TICKS)
        {
            self.poll_entity_transmitters();
        }
        let teams = self.shared.scoreboard.teams();
        let teams_changed = self
            .waypoint_teams
            .as_ref()
            .is_none_or(|last| !std::sync::Arc::ptr_eq(last, &teams));
        if teams_changed {
            self.waypoint_teams = Some(teams.clone());
            self.waypoints_stale |= !self.waypoints.is_empty();
        }
        if !std::mem::take(&mut self.waypoints_stale) {
            return;
        }
        let enabled = self.game_rules.borrow().bool("locator_bar");
        let mut candidates: Vec<crate::world::waypoints::Candidate> = self
            .players
            .iter()
            .filter(|(_, online)| online.entry.is_some())
            .filter_map(|(id, online)| {
                let state = online.state.as_ref()?;
                let mut icon = state.waypoint_icon.clone();
                if icon.color.is_none() {
                    icon.color = teams
                        .team_of(crate::world::scoreboard::Member::Player(
                            &online.profile.name,
                        ))
                        .and_then(|team| team.color)
                        .map(crate::world::scoreboard::TeamColor::rgb);
                }
                Some(crate::world::waypoints::Candidate {
                    id: *id,
                    uuid: online.profile.id,
                    dimension: state.dimension,
                    position: state.position,
                    spectator: state.game_mode == crate::protocol::play::GameMode::Spectator,
                    transmit_range: state.waypoint_transmit_range,
                    receive_range: state.waypoint_receive_range,
                    view_distance: state.view_distance,
                    icon,
                    indirect_passengers: Vec::new(),
                })
            })
            .collect();
        candidates.extend(self.entity_transmitters.iter().map(|entity| {
            crate::world::waypoints::Candidate {
                id: entity.id,
                uuid: entity.uuid,
                dimension: entity.dimension,
                position: entity.position,
                spectator: false,
                transmit_range: entity.range,
                receive_range: 0.0,
                view_distance: 0,
                icon: Default::default(),
                indirect_passengers: entity.indirect_passengers.clone(),
            }
        }));
        for (receiver, packet) in self.waypoints.step(&candidates, enabled) {
            if let Some(online) = self.players.get(&receiver) {
                let _ = online.outbound.send(framed(&packet));
            }
        }
    }

    fn poll_entity_transmitters(&self) {
        let manager = self.shared.manager.clone();
        let coordinator = self.shared.coordinator.clone();
        tokio::spawn(async move {
            let (reply, answer) = tokio::sync::oneshot::channel();
            if manager
                .send(crate::world::messages::ManagerMessage::Regions { reply })
                .await
                .is_err()
            {
                return;
            }
            let mut found = Vec::new();
            for region in answer.await.unwrap_or_default() {
                let (reply, answer) = tokio::sync::oneshot::channel();
                if region
                    .request(crate::world::messages::RegionRequest::Transmitters { reply })
                    .await
                    && let Ok(mut transmitters) = answer.await
                {
                    found.append(&mut transmitters);
                }
            }
            found.sort_by_key(|entity| entity.id);
            let _ = coordinator
                .send(crate::world::messages::CoordinatorMessage::WaypointTransmitters(found))
                .await;
        });
    }

    pub(super) fn sync_latency(&mut self) {
        if !self
            .server_ticks
            .borrow()
            .is_multiple_of(LATENCY_SYNC_TICKS)
        {
            return;
        }
        let mut entries = Vec::new();
        for online in self.players.values_mut() {
            if online.latency_dirty
                && let Some(entry) = &online.entry
            {
                online.latency_dirty = false;
                entries.push(entry.clone());
            }
        }
        if !entries.is_empty() {
            self.broadcast(&play::PlayerInfoUpdate {
                actions: play::player_info::UPDATE_LATENCY,
                entries: &entries,
            });
        }
    }

    pub(super) fn sync_time(&mut self) {
        if self.server_ticks.borrow().is_multiple_of(TIME_SYNC_TICKS) {
            let time = framed(&self.shared.time_packet());
            self.broadcast_raw(&time);
        }
    }

    pub(super) fn autosave_level(&mut self) {
        if self.server_ticks.borrow().is_multiple_of(AUTOSAVE_TICKS) {
            self.save_level();
            if self.shared.saves().autosaves() {
                drop(self.save_at_barrier(false));
            }
        }
    }

    /// Records this tick's processing time and advances any in-progress
    /// `/tick step` or `/tick sprint`.
    pub(super) fn finish_tick_bookkeeping(&mut self, busy: Duration) {
        tuff_timings::record!(tuff_timings::SERVER_TICK, busy);
        self.tick_samples.push(busy);
        self.send_tick_time(busy);
        self.publish_debug_subscribers();
        let tick = *self.clock.borrow();
        let _ = self.tick_times.send(TickTime { tick, busy });
        let (state, sprinting, players) = (self.tick_state(), self.sprint.is_some(), &self.players);
        self.tick_metrics
            .record(&self.shared.metrics, tick, busy, state, sprinting, || {
                player_pings(players)
            });
        if self.step_remaining > 0 {
            self.step_remaining -= 1;
            if self.step_remaining == 0 {
                self.broadcast(&play::StepTick(0));
            }
        }
        if let Some(mut sprint) = self.sprint.take() {
            if sprint.record(busy) {
                self.finish_sprint(sprint);
            } else {
                self.sprint = Some(sprint);
            }
        }
    }

    /// Sends opped subscribers this tick's timings: the whole period since
    /// the last tick, the tick's own work and the idle rest.
    pub(super) fn send_tick_time(&mut self, busy: Duration) {
        let now = std::time::Instant::now();
        let full = self.last_tick_end.replace(now).map(|last| now - last);
        let Some(full) = full else {
            return;
        };
        let tick_bit = crate::world::debug_feeds::bit(
            tuff_vanilla_data::registry::DebugSubscription::DedicatedServerTickTime,
        );
        if !self.players.values().any(|p| p.debug & tick_bit != 0) {
            return;
        }
        let nanos = |d: Duration| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX);
        let packet = framed(&play::DebugSample {
            sample: [
                nanos(full),
                nanos(busy),
                0,
                nanos(full.saturating_sub(busy)),
            ],
        });
        let permissions = self.shared.permissions.view();
        for online in self.players.values().filter(|p| p.debug & tick_bit != 0) {
            let subject = crate::permission::Subject::Player {
                uuid: online.profile.id,
                level: online.permission,
            };
            if permissions.allows(subject, crate::permission::DEBUG_TICK_TIME) {
                let _ = online.outbound.send(packet.clone());
            }
        }
    }

    /// Publishes who may receive world debug feeds, when that changed.
    fn publish_debug_subscribers(&mut self) {
        use crate::world::debug_feeds::{DebugSubscribers, bit};
        let world_feeds =
            !bit(tuff_vanilla_data::registry::DebugSubscription::DedicatedServerTickTime);
        if !self.players.values().any(|p| p.debug & world_feeds != 0)
            && *self.debug_subscribers.borrow() == Default::default()
        {
            return;
        }
        let permissions = self.shared.permissions.view();
        let players = self
            .players
            .iter()
            .filter(|(_, p)| p.debug & world_feeds != 0)
            .filter(|(_, p)| {
                permissions.allows(
                    crate::permission::Subject::Player {
                        uuid: p.profile.id,
                        level: p.permission,
                    },
                    crate::permission::DEBUG_SUBSCRIPTIONS,
                )
            })
            .map(|(id, p)| (*id, p.debug & world_feeds))
            .collect();
        let next = DebugSubscribers::new(players);
        self.debug_subscribers.send_if_modified(|current| {
            let changed = **current != next;
            if changed {
                *current = std::sync::Arc::new(next);
            }
            changed
        });
    }

    pub(super) fn tick_state(&self) -> TickState {
        *self.tick_state.borrow()
    }

    /// Publishes a new tick state and tells clients, as the game does
    /// whenever the rate or freeze state changes.
    pub(super) fn set_tick_state(&mut self, state: TickState) {
        self.tick_state.send_replace(state);
        self.broadcast(&play::SetTickingState {
            rate: state.rate,
            frozen: state.frozen,
        });
    }

    /// Ends `sprint` (naturally or interrupted), reporting the achieved
    /// rate and resuming whatever freeze state was active when it started.
    pub(super) fn finish_sprint(&mut self, sprint: Sprint) {
        let rate = self.tick_state().rate;
        let report = sprint_report(sprint, self.tick_state().period());
        self.set_tick_state(TickState {
            rate,
            frozen: sprint.resume_frozen,
        });
        if let Some(reply) = self.sprint_report.take() {
            let _ = reply.send(report);
        }
    }

    pub(super) fn run_tick_command(
        &mut self,
        action: TickAction,
        report: Option<oneshot::Sender<SprintReport>>,
    ) -> TickOutcome {
        match action {
            TickAction::Query => TickOutcome::Query {
                state: self.tick_state(),
                sprinting: self.sprint.is_some(),
                average: self.tick_samples.average(),
                samples: self.tick_samples.clone(),
            },
            TickAction::Rate(rate) => {
                let frozen = self.tick_state().frozen;
                self.set_tick_state(TickState { rate, frozen });
                TickOutcome::Rate(rate)
            }
            TickAction::Freeze => {
                self.stop_sprint_and_step();
                let rate = self.tick_state().rate;
                self.set_tick_state(TickState { rate, frozen: true });
                TickOutcome::Frozen(true)
            }
            TickAction::Unfreeze => {
                let rate = self.tick_state().rate;
                self.set_tick_state(TickState {
                    rate,
                    frozen: false,
                });
                TickOutcome::Frozen(false)
            }
            TickAction::Step(ticks) => {
                if !self.tick_state().frozen {
                    return TickOutcome::NotFrozen;
                }
                self.step_remaining = ticks;
                self.broadcast(&play::StepTick(ticks as i32));
                TickOutcome::Stepping(ticks)
            }
            TickAction::StepStop => {
                if self.step_remaining == 0 {
                    return TickOutcome::NotStepping;
                }
                self.step_remaining = 0;
                self.broadcast(&play::StepTick(0));
                TickOutcome::StepStopped
            }
            TickAction::Sprint(ticks) => {
                let interrupted = self.sprint.is_some();
                let resume_frozen = self.tick_state().frozen;
                self.sprint = Some(
                    Sprint::new(u64::from(ticks), resume_frozen)
                        .skipping_in_flight(self.awaiting_barrier.is_some()),
                );
                self.sprint_report = report;
                let rate = self.tick_state().rate;
                self.set_tick_state(TickState {
                    rate,
                    frozen: false,
                });
                TickOutcome::Sprinting { interrupted }
            }
            TickAction::SprintStop => {
                let Some(sprint) = self.sprint.take() else {
                    return TickOutcome::NotSprinting;
                };
                self.finish_sprint(sprint);
                TickOutcome::SprintStopped
            }
        }
    }

    /// Silently ends an in-progress sprint or step, as `/tick freeze` does
    /// before it freezes.
    pub(super) fn stop_sprint_and_step(&mut self) {
        if let Some(sprint) = self.sprint.take() {
            self.finish_sprint(sprint);
        }
        if self.step_remaining > 0 {
            self.step_remaining = 0;
            self.broadcast(&play::StepTick(0));
        }
    }

    /// Announces who sleeps in each dimension when that changes, and skips
    /// the night once enough players (by `players_sleeping_percentage`)
    /// have slept long enough.
    pub(super) fn tick_sleep(&mut self) {
        let percentage = self.game_rules.borrow().int("players_sleeping_percentage");
        let mut dimensions: BTreeMap<crate::world::dimension::DimensionId, (usize, usize, usize)> =
            BTreeMap::new();
        for state in self.players.values().filter_map(|p| p.state.as_ref()) {
            let counts = dimensions.entry(state.dimension).or_default();
            if state.game_mode != play::GameMode::Spectator {
                counts.0 += 1;
                counts.1 += usize::from(state.sleeping);
                counts.2 += usize::from(state.deep_sleeping);
            }
        }
        let known: Vec<_> = self.sleep_status.keys().copied().collect();
        for dimension in known {
            dimensions.entry(dimension).or_default();
        }
        for (dimension, (active, sleeping, deep)) in dimensions {
            let (old_active, old_sleeping) = self
                .sleep_status
                .insert(dimension, (active, sleeping))
                .unwrap_or((0, 0));
            let needed = sleepers_needed(active, percentage);
            let changed = (old_sleeping > 0 || sleeping > 0)
                && (old_active, old_sleeping) != (active, sleeping);
            if changed && percentage <= 100 {
                let message = if sleeping >= needed {
                    TextComponent::translate("sleep.skipping_night", [])
                } else {
                    TextComponent::translate(
                        "sleep.players_sleeping",
                        [text(sleeping), text(needed)],
                    )
                };
                let packet = framed(&play::SystemChat {
                    content: &message,
                    overlay: true,
                });
                for online in self.players.values() {
                    if online.entry.is_some()
                        && online
                            .state
                            .as_ref()
                            .is_some_and(|s| s.dimension == dimension)
                    {
                        let _ = online.outbound.send(packet.clone());
                    }
                }
            }
            if sleeping >= needed && deep >= needed && sleeping > 0 {
                self.skip_night(dimension);
            }
        }
    }

    pub(super) fn skip_night(&mut self, dimension: crate::world::dimension::DimensionId) {
        let clock = self.shared.dimension(dimension).default_clock;
        if self.game_rules.borrow().bool("advance_time")
            && let Some(clock) = clock
            && let Some(marker) = crate::world::clock::TimeMarker::find(
                clock,
                tuff_vanilla_data::world::WAKE_UP_MARKER,
            )
        {
            self.clocks.send_modify(|clocks| {
                let state = clocks.get_mut(clock);
                let next = marker.next_after(state.total_ticks);
                state.set(next);
            });
            let world_age = *self.clock.borrow() as i64;
            let packet = self.clocks.borrow().packet(world_age, Some(clock));
            self.broadcast(&packet);
        }
        for online in self.players.values_mut() {
            if let Some(state) = online.state.as_mut()
                && state.dimension == dimension
                && state.sleeping
            {
                state.sleeping = false;
                state.deep_sleeping = false;
                online.handle.send(PlayerEvent::WakeUp);
            }
        }
        self.sleep_status.insert(dimension, (0, 0));
        if self.game_rules.borrow().bool("advance_weather") && self.weather.borrow().raining {
            self.weather.send_modify(|weather| weather.reset_cycle());
        }
    }
}
