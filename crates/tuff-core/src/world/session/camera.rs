use super::*;
use crate::protocol::play::{GameMode, SetCamera};
use tuff_vanilla_data::registry::EntityType;

/// How far a spectator may be from an entity to start watching through it.
const CAMERA_RANGE: f64 = 3.0;

fn pickable(kind: EntityType) -> bool {
    !matches!(
        kind,
        EntityType::Item
            | EntityType::ExperienceOrb
            | EntityType::AreaEffectCloud
            | EntityType::Marker
            | EntityType::LightningBolt
            | EntityType::FishingBobber
    )
}

impl PlayerSession {
    pub fn camera(&self) -> Option<i32> {
        self.mode.camera
    }

    pub async fn spectate(&mut self, target: Option<i32>) {
        match target {
            None => self.reset_camera(),
            Some(target) => self.set_camera(target).await,
        }
    }

    /// The spectator menu: teleports to the player with `uuid` at any
    /// distance and in any dimension.
    pub async fn teleport_to_player(
        &mut self,
        index: &crate::world::nearby::PlayerIndex,
        uuid: u128,
    ) {
        if self.mode.game_mode != GameMode::Spectator {
            return;
        }
        let Some((dimension, target)) = index.find_by_uuid(uuid) else {
            return;
        };
        let teleport = crate::world::messages::Teleport {
            dimension,
            position: target.position,
            relative: [false; 3],
            rotation: Some([target.looks.yaw, target.looks.pitch]),
            relative_rotation: [false; 2],
            look_at: None,
        };
        self.reset_camera();
        self.teleport_by_command(teleport).await;
    }

    async fn set_camera(&mut self, target: i32) {
        self.point_camera(target, false).await;
    }

    pub async fn force_camera(&mut self, target: i32) {
        self.point_camera(target, true).await;
    }

    async fn point_camera(&mut self, target: i32, forced: bool) {
        if self.mode.game_mode != GameMode::Spectator || self.is_dead() || target == self.id {
            return;
        }
        let at = if let Some((at, _)) = self.view.entities.visible_player(target) {
            at
        } else if let Some((kind, _, at)) = self.view.entities.visible_entity(target) {
            if !forced && !pickable(kind) {
                return;
            }
            at
        } else {
            return;
        };
        let here = self.pose.position;
        let distance_sq: f64 = (0..3).map(|i| (at[i] - here[i]).powi(2)).sum();
        if !forced && distance_sq > CAMERA_RANGE * CAMERA_RANGE {
            return;
        }
        let block = BlockPos::new(
            at[0].floor() as i32,
            at[1].floor() as i32,
            at[2].floor() as i32,
        );
        if !self.dimension().block_within_border(block) {
            return;
        }
        if self.mode.camera == Some(target) {
            return;
        }
        self.mode.camera = Some(target);
        self.send(&SetCamera { entity_id: target });
    }

    pub fn reset_camera(&mut self) {
        if self.mode.camera.take().is_some() {
            self.send(&SetCamera { entity_id: self.id });
        }
    }

    pub(super) async fn tick_camera(&mut self) {
        let Some(target) = self.mode.camera else {
            return;
        };
        if self.mode.game_mode != GameMode::Spectator || self.is_dead() {
            self.reset_camera();
            return;
        }
        let at = self
            .view
            .entities
            .visible_player(target)
            .map(|(at, _)| at)
            .or_else(|| self.view.entities.visible_entity(target).map(|e| e.2));
        let Some(at) = at else {
            self.reset_camera();
            return;
        };
        if self.pose.position != at {
            self.pose.position = at;
            self.sync.teleport_target = at;
            if self.chunk() != self.view.center {
                self.update_view().await;
            }
            self.publish(true).await;
        }
    }
}
