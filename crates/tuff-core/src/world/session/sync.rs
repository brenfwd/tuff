use super::*;

/// Client movement sync: unconfirmed teleports, load gating and the
/// movement and input the client last reported.
#[derive(Default)]
pub struct MoveSync {
    pub(super) floating_ticks: u32,
    pub client_floating: bool,
    pub pending_teleport: Option<i32>,
    pub next_teleport_id: i32,
    /// Where the unconfirmed teleport put the player; the confirmation
    /// moves on from here.
    pub teleport_target: [f64; 3],
    /// The tick the unconfirmed teleport was last sent.
    pub teleport_sent: u64,
    /// A position move arrived since the client's last Client Tick End.
    pub position_this_tick: bool,
    /// Until this tick, or until the client says Player Loaded, moves are
    /// ignored (after joining and respawning); `None` once loaded.
    pub load_deadline: Option<u64>,
    /// How far the client last said it moved in one tick.
    pub known_movement: [f64; 3],
    /// The last Player Input bit flags: forward/back/left/right/jump/
    /// sneak/sprint key states, used while driving a server-steered mount
    /// (sneaking is also read out of this into `sneaking` below).
    pub input_flags: u8,
    pub pending_jump: Option<u8>,
    /// Feeds this player's ordered chat filtering worker, started on the first filtered message.
    pub chat_filter: Option<tokio::sync::mpsc::Sender<String>>,
    pub moved_this_tick: bool,
}

impl PlayerSession {
    pub fn teleport(&mut self, position: [f64; 3]) {
        self.pose.position = position;
        self.sync.teleport_target = position;
        self.sync.teleport_sent = self.tick;
        self.sync.next_teleport_id = self.sync.next_teleport_id.wrapping_add(1).max(1);
        self.sync.pending_teleport = Some(self.sync.next_teleport_id);
        let packet = play::PlayerPosition {
            teleport_id: self.sync.next_teleport_id,
            position,
            yaw: self.pose.yaw,
            pitch: self.pose.pitch,
        };
        self.send(&packet);
    }

    /// Ignores moves until the client has loaded the world (or 60 ticks
    /// passed), the way the game does after joining and respawning.
    pub fn await_client_load(&mut self) {
        self.sync.load_deadline = Some(self.tick + CLIENT_LOAD_TICKS);
    }

    pub fn client_loaded(&mut self) -> bool {
        match self.sync.load_deadline {
            Some(deadline) if self.tick < deadline => false,
            Some(_) => {
                self.sync.load_deadline = None;
                true
            }
            None => true,
        }
    }

    /// The client sends at most one position per tick and nothing it moves
    /// or does counts before it has loaded; a second position in one tick
    /// kicks. A move while a teleport is unconfirmed for over 20 ticks sends
    /// the teleport again.
    pub(super) fn admit_move(&mut self, packet: &Serverbound) -> bool {
        let admitted = self.admit(packet);
        if admitted
            && matches!(
                packet,
                Serverbound::Move(_) | Serverbound::MoveVehicle { .. }
            )
            && self.sync.pending_teleport.is_some()
            && self.tick.saturating_sub(self.sync.teleport_sent) > TELEPORT_RESEND_TICKS
        {
            self.teleport(self.sync.teleport_target);
        }
        admitted
    }

    pub(super) fn admit(&mut self, packet: &Serverbound) -> bool {
        match packet {
            Serverbound::Move(movement) => {
                if movement.position.is_some() {
                    if self.sync.position_this_tick {
                        self.kick(TextComponent::translate(
                            "multiplayer.disconnect.invalid_player_movement",
                            [],
                        ));
                        return false;
                    }
                    self.sync.position_this_tick = true;
                }
                self.client_loaded()
            }
            Serverbound::MoveVehicle { .. }
            | Serverbound::PlayerAction { .. }
            | Serverbound::PlayerCommand(_)
            | Serverbound::UseItemOn { .. }
            | Serverbound::UseItem { .. }
            | Serverbound::Attack(_)
            | Serverbound::Interact { .. } => self.client_loaded(),
            Serverbound::ClientTickEnd => {
                self.sync.position_this_tick = false;
                true
            }
            _ => true,
        }
    }

    /// The client confirms a teleport with the position it applied, which
    /// counts as a move from the teleport's target.
    pub(super) async fn accept_teleport(
        &mut self,
        id: i32,
        position: [f64; 3],
        rotation: (f32, f32),
    ) {
        if id != self.sync.next_teleport_id {
            return;
        }
        let invalid = position.iter().any(|c| c.is_nan())
            || !rotation.0.is_finite()
            || !rotation.1.is_finite();
        if self.sync.pending_teleport.is_none() || invalid {
            self.kick(TextComponent::translate(
                "multiplayer.disconnect.invalid_player_movement",
                [],
            ));
            return;
        }
        self.sync.pending_teleport = None;
        self.pose.position = self.sync.teleport_target;
        let movement = Serverbound::Move(Movement {
            position: Some(position),
            rotation: Some(rotation),
            on_ground: false,
            horizontal_collision: false,
        });
        let Some(packet) = self.intercept(movement).await else {
            return;
        };
        let Some(packet) = self.packet_hooks(packet).await else {
            return;
        };
        if let Serverbound::Move(movement) = packet
            && !self.hooks_suspended()
        {
            self.handle_move(movement).await;
        }
    }

    pub async fn handle_move(&mut self, movement: Movement) {
        if let Some((yaw, pitch)) = movement.rotation
            && yaw.is_finite()
            && pitch.is_finite()
        {
            self.pose.yaw = yaw;
            self.pose.pitch = pitch.clamp(-90.0, 90.0);
        }
        if self.sync.pending_teleport.is_some()
            || self.is_dead()
            || self.ride.riding.is_some()
            || self.mode.camera.is_some()
        {
            return;
        }
        if self.sleep.sleeping.is_some() {
            if let Some(new) = movement.position
                && distance_sq(new, self.pose.position) > 1.0
            {
                self.teleport(self.pose.position);
            }
            return;
        }
        let old = self.pose.position;
        let new = movement.position.unwrap_or(old);
        if new.iter().any(|c| !c.is_finite())
            || (self.mode.game_mode != GameMode::Spectator
                && self.dimension().border().blocks_move(
                    old,
                    new,
                    PLAYER_HALF_WIDTH,
                    BORDER_TOLERANCE,
                ))
        {
            self.teleport(old);
            return;
        }
        let (dx, dy, dz) = (new[0] - old[0], new[1] - old[1], new[2] - old[2]);
        self.sync.known_movement = [dx, dy, dz];
        self.sync.moved_this_tick = true;
        self.pose.position = new;
        let was_on_ground = self.pose.on_ground;
        self.pose.on_ground = movement.on_ground;
        let was_gliding = self.motion.glide.active;
        if was_gliding && movement.on_ground {
            self.motion.glide.stop();
            self.publish(true).await;
        }
        self.sync.client_floating = self.is_floating(dy);
        self.movement_stats(dx, dy, dz, movement.on_ground);
        if was_on_ground && !movement.on_ground && dy > 0.0 {
            self.progress.stats.award_custom(CustomStat::Jump, 1);
            let cost = if self.pose.sprinting {
                exhaustion::SPRINT_JUMP
            } else {
                exhaustion::JUMP
            };
            self.exhaust(cost);
        }
        if movement.on_ground && self.pose.sprinting {
            let centimeters = ((dx * dx + dz * dz).sqrt() * 100.0).round() as f32;
            self.exhaust(exhaustion::SPRINT_PER_METER * centimeters * 0.01);
        }
        if movement.on_ground && self.body.fall.distance >= 2.0 && !self.mode.abilities.may_fly {
            self.progress.stats.award_custom(
                CustomStat::FallOneCm,
                (self.body.fall.distance * 100.0).round() as i32,
            );
        }
        if was_gliding {
            let speed = dx.hypot(dz);
            let lost = (self.motion.glide_speed - speed) * 10.0 - 3.0;
            self.motion.glide_speed = speed;
            if movement.horizontal_collision && lost > 0.0 {
                self.hurt_by(lost as f32, &damage_type::FLY_INTO_WALL, None, None)
                    .await;
            }
            // No fall damage while gliding: vanilla caps the accumulated
            // fall distance to at most a block whenever the glide's lift
            // keeps the descent shallow, which covers ordinary flight; a
            // steep, fast dive into the ground is not modeled.
            self.body.fall.distance = 0.0;
            self.body.fall.try_reset_impulse();
        } else if self.body.fall.step(
            dy,
            movement.on_ground,
            self.published.seq + 1,
            self.pose.position[1],
        ) {
            self.publish(true).await;
        }
        if self.chunk() != self.view.center {
            self.update_view().await;
        }
    }

    pub(super) fn is_floating(&self, dy: f64) -> bool {
        if self.mode.abilities.may_fly
            || self.pose.on_ground
            || dy < -0.03125
            || self.is_gliding()
            || self.ride.riding.is_some()
            || self.effect(MobEffect::Levitation).is_some()
            || self.shared.live.network().allow_flight
        {
            return false;
        }
        let [x, y, z] = self.pose.position;
        let area = [
            x - 0.3625,
            y - 0.6125,
            z - 0.3625,
            x + 0.3625,
            y + 1.8625,
            z + 0.3625,
        ];
        let mirror = &self.view.mirror;
        !mirror.touches_unknown(area)
            && (area[0].floor() as i32..=area[3].floor() as i32).all(|bx| {
                (area[1].floor() as i32..=area[4].floor() as i32).all(|by| {
                    (area[2].floor() as i32..=area[5].floor() as i32).all(|bz| {
                        mirror
                            .block_at(tuff_world_model::BlockPos::new(bx, by, bz))
                            .is_none_or(tuff_world_model::chunk::is_air)
                    })
                })
            })
    }

    pub(super) fn tick_floating(&mut self) {
        let loading = self
            .sync
            .load_deadline
            .is_some_and(|deadline| self.tick < deadline);
        if !self.sync.client_floating || loading {
            self.sync.floating_ticks = 0;
            return;
        }
        self.sync.floating_ticks += 1;
        if self.sync.floating_ticks > 80 {
            self.kick(TextComponent::translate(
                "multiplayer.disconnect.flying",
                [],
            ));
        }
    }
}
