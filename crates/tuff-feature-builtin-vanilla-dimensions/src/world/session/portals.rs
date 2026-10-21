//! Portal travel, gateways and the end credits.

use crate::world::travel::{self, Regions};

use crate::world::portal::{self, PortalContact, PortalKind};
use tuff_core::protocol::play::{self, GameMode};
use tuff_core::world::dimension::{DimensionId, THE_END};
use tuff_core::world::messages::{ManagerMessage, TimedTicket};
use tuff_core::world::player::Fall;
use tuff_core::world::session::PlayerSession;
use tuff_core::world::session::hooks::{HookFuture, SessionHooks, SessionTick};
use tuff_world_model::BlockPos;
use tuff_world_model::pos::{ChunkPos, RegionPos};

/// A player's portal travel, kept in the session's extensions.
#[derive(Default)]
pub struct PortalState {
    /// The portal the player stands in, counting up to the trip.
    process: Option<PortalProcess>,
    /// Watching the credits: out of the world until the client asks to
    /// respawn.
    won_game: bool,
}

struct PortalProcess {
    contact: PortalContact,
    time: u32,
    inside_this_tick: bool,
}

#[allow(async_fn_in_trait)]
pub trait PortalExt {
    /// The first exit from the end: the player leaves the world and the
    /// client rolls the credits, then asks to respawn.
    async fn show_end_credits(&mut self);
    /// The credits closed: back to the respawn point, keeping everything
    /// but what a respawn resets on the client.
    async fn leave_credits(&mut self);
    /// Records that the home region saw this player inside a portal block.
    fn inside_portal(&mut self, contact: PortalContact);
    /// Advances the portal timer; returns the portal to travel through once
    /// the player has stood in it long enough.
    fn tick_portal(&mut self) -> Option<PortalContact>;
    async fn travel(&mut self, contact: PortalContact);
    /// Comes out beside the exit of the gateway at `gateway` (built first
    /// if it has none), keeping its facing.
    async fn through_gateway(&mut self, gateway: BlockPos);
    /// The other side of the nether portal at `entry`: the closest portal
    /// near the scaled position, or a new one built there.
    async fn nether_destination(
        &mut self,
        entry: BlockPos,
    ) -> Option<(DimensionId, [f64; 3], f32, f32)>;
    /// Where an end portal leads: the end's platform, or from the end the
    /// respawn point (the credits are not shown).
    async fn end_destination(&mut self) -> Option<(DimensionId, [f64; 3], f32, f32)>;
}

impl PortalExt for PlayerSession {
    /// The first exit from the end: the player leaves the world and the
    /// client rolls the credits, then asks to respawn.
    async fn show_end_credits(&mut self) {
        self.ext.entry::<PortalState>().process = None;
        self.close_menu(true).await;
        self.finish_menu_close().await;
        self.published.seq += 1;
        self.detach_from_regions().await;
        let state = self.ext.entry::<PortalState>();
        if !state.won_game {
            state.won_game = true;
            self.send(&play::GameEvent {
                event: play::game_event::WIN_GAME,
                value: 0.0,
            });
            self.progress.seen_credits = true;
            self.conn.handle.set_seen_credits(true);
        }
    }

    /// The credits closed: back to the respawn point, keeping everything
    /// but what a respawn resets on the client.
    async fn leave_credits(&mut self) {
        self.ext.entry::<PortalState>().won_game = false;
        let (dimension, position, yaw, pitch) = self.spawn_block_point(false).await;
        self.pose.sneaking = false;
        self.ext.entry::<PortalState>().process = None;
        self.await_client_load();
        self.relocate(
            dimension,
            position,
            yaw,
            pitch,
            play::respawn::KEEP_ATTRIBUTES,
        )
        .await;
    }

    /// Records that the home region saw this player inside a portal block.
    fn inside_portal(&mut self, contact: PortalContact) {
        if contact.kind == PortalKind::Nether
            && !self.shared.rule_bool("allow_entering_nether_using_portals")
        {
            return;
        }
        if self.ride.riding.is_some() {
            return;
        }
        if self.actions.portal_cooldown > 0 {
            self.actions.portal_cooldown = portal::PLAYER_PORTAL_COOLDOWN;
            return;
        }
        let state = self.ext.entry::<PortalState>();
        match &mut state.process {
            Some(process) if process.contact.kind == contact.kind => {
                if !process.inside_this_tick {
                    process.contact = contact;
                    process.inside_this_tick = true;
                }
            }
            _ => {
                state.process = Some(PortalProcess {
                    contact,
                    time: 0,
                    inside_this_tick: true,
                });
            }
        }
    }

    /// Advances the portal timer; returns the portal to travel through once
    /// the player has stood in it long enough.
    fn tick_portal(&mut self) -> Option<PortalContact> {
        self.actions.portal_cooldown = self.actions.portal_cooldown.saturating_sub(1);
        let invulnerable = self.mode.abilities.invulnerable;
        let rule = if invulnerable {
            "players_nether_portal_creative_delay"
        } else {
            "players_nether_portal_default_delay"
        };
        let nether_delay = u32::try_from(self.shared.rule_int(rule)).unwrap_or(0);
        let alive = !self.is_dead();
        let state = self.ext.entry::<PortalState>();
        let process = state.process.as_mut()?;
        let delay = match process.contact.kind {
            PortalKind::Nether => nether_delay,
            PortalKind::End | PortalKind::Gateway => 0,
        };
        if process.inside_this_tick {
            process.inside_this_tick = false;
            if alive {
                let time = process.time;
                process.time += 1;
                if time >= delay {
                    return Some(process.contact);
                }
            }
            return None;
        }
        process.time = process.time.saturating_sub(4);
        if process.time == 0 {
            state.process = None;
        }
        None
    }

    async fn travel(&mut self, contact: PortalContact) {
        if contact.kind == PortalKind::Gateway {
            self.through_gateway(contact.pos).await;
            return;
        }
        if contact.kind == PortalKind::End
            && portal::end_exit(self.dimension().key == THE_END, self.progress.seen_credits)
                == portal::EndExit::Credits
        {
            self.show_end_credits().await;
            return;
        }
        self.actions.portal_cooldown = portal::PLAYER_PORTAL_COOLDOWN;
        let destination = match contact.kind {
            PortalKind::Nether => self.nether_destination(contact.pos).await,
            PortalKind::End => self.end_destination().await,
            PortalKind::Gateway => None,
        };
        let Some((dimension, position, yaw, pitch)) = destination else {
            return;
        };
        self.ext.entry::<PortalState>().process = None;
        let _ = self
            .shared
            .manager
            .send(ManagerMessage::Hold {
                dimension,
                center: ChunkPos::of_position(position),
                ticket: TimedTicket::Portal,
            })
            .await;
        self.relocate(
            dimension,
            position,
            yaw,
            pitch,
            play::respawn::KEEP_ALL_DATA,
        )
        .await;
        self.send(&play::LevelEvent {
            event: play::level_event::SOUND_PORTAL_TRAVEL,
            position: BlockPos::default(),
            data: 0,
            global: false,
        });
        self.actions.portal_cooldown = portal::PLAYER_PORTAL_COOLDOWN;
    }

    /// Comes out beside the exit of the gateway at `gateway` (built first
    /// if it has none), keeping its facing.
    async fn through_gateway(&mut self, gateway: BlockPos) {
        self.actions.portal_cooldown = portal::PLAYER_PORTAL_COOLDOWN;
        let destination =
            crate::world::gateway::destination(&self.shared, self.dimension, gateway).await;
        let Some(position) = destination else {
            return;
        };
        self.ext.entry::<PortalState>().process = None;
        self.body.fall = Fall::default();
        self.teleport(position);
        self.update_view().await;
        self.publish(true).await;
    }

    /// The other side of the nether portal at `entry`: the closest portal
    /// near the scaled position, or a new one built there.
    async fn nether_destination(
        &mut self,
        entry: BlockPos,
    ) -> Option<(DimensionId, [f64; 3], f32, f32)> {
        let shared = self.shared.clone();
        let (from, position) = (self.dimension, self.pose.position);
        let size = [portal::PLAYER_WIDTH, tuff_core::world::PLAYER_HEIGHT];
        let create = self.mode.game_mode != GameMode::Spectator;
        let exit = travel::nether_exit(self, &shared, from, entry, position, size, create).await?;
        Some((
            exit.dimension,
            exit.position,
            self.pose.yaw + exit.rotation,
            self.pose.pitch,
        ))
    }

    /// Where an end portal leads: the end's platform, or from the end the
    /// respawn point (the credits are not shown).
    async fn end_destination(&mut self) -> Option<(DimensionId, [f64; 3], f32, f32)> {
        if self.dimension().key == THE_END {
            return Some(self.spawn_block_point(false).await);
        }
        let shared = self.shared.clone();
        let end = travel::end_platform(self, &shared).await?;
        let spawn = portal::END_SPAWN;
        let position = [
            spawn.x as f64 + 0.5,
            (spawn.y - 1) as f64,
            spawn.z as f64 + 0.5,
        ];
        Some((end, position, 90.0, self.pose.pitch))
    }
}

impl Regions for PlayerSession {
    async fn region_in(
        &mut self,
        dimension: DimensionId,
        region: RegionPos,
    ) -> Option<tuff_core::world::messages::RegionHandle> {
        PlayerSession::region_in(self, dimension, region).await
    }
}

/// Portal travel as session hooks.
pub struct PortalHooks;

impl SessionHooks for PortalHooks {
    fn tick<'a>(&'a self, session: &'a mut PlayerSession, phase: SessionTick) -> HookFuture<'a> {
        Box::pin(async move {
            if phase != SessionTick::Late {
                return;
            }
            if let Some(contact) = session.tick_portal() {
                session.travel(contact).await;
            }
        })
    }

    fn surroundings<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        portal: Option<tuff_core::world::contact::PortalContact>,
    ) -> HookFuture<'a> {
        if let Some(contact) = portal.and_then(portal::PortalContact::of) {
            session.inside_portal(contact);
        }
        Box::pin(async {})
    }

    fn use_item<'a>(&'a self, session: &'a mut PlayerSession, off_hand: bool) -> HookFuture<'a> {
        Box::pin(crate::dragon::bottle::use_bottle(session, off_hand))
    }

    fn teleported(&self, session: &mut PlayerSession) {
        session.ext.entry::<PortalState>().process = None;
    }

    fn wants_reports(&self, session: &PlayerSession) -> bool {
        session
            .ext
            .get::<PortalState>()
            .is_some_and(|state| state.process.is_some())
    }

    fn client_command<'a>(
        &'a self,
        session: &'a mut PlayerSession,
        action: i32,
        taken: &'a mut bool,
    ) -> HookFuture<'a> {
        Box::pin(async move {
            if action == 0 && self.suspended(session) {
                *taken = true;
                session.leave_credits().await;
            }
        })
    }

    fn suspended(&self, session: &PlayerSession) -> bool {
        session
            .ext
            .get::<PortalState>()
            .is_some_and(|state| state.won_game)
    }
}
