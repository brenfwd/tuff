use super::*;

impl FightActor {
    pub(super) async fn exit_portal_location(&mut self) -> Option<BlockPos> {
        if let Some(location) = self.data.exit_portal {
            return Some(location);
        }
        let handle = self.region(ChunkPos::of_block(ORIGIN).region()).await?;
        let (reply, answer) = oneshot::channel();
        handle
            .request(FightRequest::ExitPortalLocation { reply }.request())
            .await;
        let location = answer.await.ok().flatten()?;
        self.data.exit_portal = Some(location);
        self.save();
        Some(location)
    }

    pub(super) async fn spawn_exit_portal(&mut self, active: bool) {
        let Some(location) = self.exit_portal_location().await else {
            return;
        };
        let mut batch: Vec<(BlockPos, BlockState)> = Vec::new();
        let mut replacing = false;
        for (pos, state, replaces) in podium_blocks(location, active) {
            if replaces != replacing && !batch.is_empty() {
                self.set_blocks(std::mem::take(&mut batch), replacing).await;
            }
            replacing = replaces;
            batch.push((pos, state));
        }
        self.set_blocks(batch, replacing).await;
    }

    pub(super) async fn spawn_new_gateway(&mut self) {
        let Some(index) = self.data.gateways.pop() else {
            return;
        };
        let pos = gateway_position(index);
        if let Some(handle) = self.region(ChunkPos::of_block(pos).region()).await {
            handle
                .request(
                    FightRequest::LevelEvent {
                        event: play::level_event::ANIMATION_END_GATEWAY_SPAWN,
                        pos,
                        data: 0,
                    }
                    .request(),
                )
                .await;
        }
        self.set_blocks(crate::world::gateway::gateway_blocks(pos), false)
            .await;
        self.save();
    }

    pub(super) async fn place_egg(&self) {
        let Some(handle) = self.region(ChunkPos::of_block(ORIGIN).region()).await else {
            return;
        };
        let (reply, done) = oneshot::channel();
        handle
            .request(FightRequest::PlaceEgg { reply }.request())
            .await;
        let _ = done.await;
    }

    pub(super) async fn update_crystal_count(&mut self) {
        let mut alive = 0;
        let mut crystals = Vec::new();
        for entity in self.entities().await {
            if entity.kind != EntityType::EndCrystal {
                continue;
            }
            let on_spikes = self
                .spikes
                .iter()
                .filter(|spike| intersects(entity.bounds, spike_top(spike)))
                .count();
            if on_spikes > 0 {
                alive += on_spikes as i32;
                crystals.push((entity.uuid, entity.position));
            }
        }
        if alive != self.view.alive_crystals || crystals != self.view.crystals {
            self.view.alive_crystals = alive;
            self.view.crystals = crystals;
            self.publish();
        }
    }

    /// Four crystals on the exit portal, one on each side, summon a new
    /// dragon once the old one is dead.
    pub(super) async fn try_respawn(&mut self) {
        if !self.data.dragon_killed || self.data.respawn_stage.is_some() {
            return;
        }
        if self.data.exit_portal.is_none() && !self.find_exit_portal().await {
            self.spawn_exit_portal(true).await;
        }
        let Some(location) = self.data.exit_portal else {
            return;
        };
        let slots = [(0, -1), (1, 0), (0, 1), (-1, 0)].map(|(sx, sz)| {
            let min = [
                f64::from(location.x + sx * RESPAWN_CRYSTAL_DISTANCE),
                f64::from(location.y + 1),
                f64::from(location.z + sz * RESPAWN_CRYSTAL_DISTANCE),
            ];
            [
                min[0],
                min[1],
                min[2],
                min[0] + 1.0,
                min[1] + 1.0,
                min[2] + 1.0,
            ]
        });
        let entities = self.entities().await;
        let placed = |slot: [f64; 6]| {
            entities
                .iter()
                .filter(move |e| e.kind == EntityType::EndCrystal && intersects(e.bounds, slot))
                .map(|e| e.uuid)
        };
        if slots.iter().any(|&slot| placed(slot).next().is_none()) {
            return;
        }
        let crystals = slots.iter().flat_map(|&slot| placed(slot)).collect();
        self.respawn_dragon(crystals, location).await;
    }

    /// Turns the old exit portal to end stone, puts the inactive podium
    /// back and starts the summoning.
    pub(super) async fn respawn_dragon(&mut self, crystals: Vec<u128>, location: BlockPos) {
        let (min, max) = (
            BlockPos::new(location.x - 3, location.y - 1, location.z - 3),
            BlockPos::new(location.x + 3, location.y + 3, location.z + 3),
        );
        let mut old = self.find_blocks(Block::Bedrock, min, max).await;
        old.extend(self.find_blocks(Block::EndPortal, min, max).await);
        let end_stone = Block::EndStone.default_state();
        self.set_blocks(old.into_iter().map(|p| (p, end_stone)).collect(), false)
            .await;
        self.data.respawn_stage = Some(RespawnStage::Start);
        self.data.respawn_time = 0;
        self.spawn_exit_portal(false).await;
        self.data.respawn_crystals = crystals;
        self.save();
    }

    /// The summoning crystals still in the world, in their saved order.
    pub(super) async fn respawn_crystals(&self) -> Vec<EntitySnapshot> {
        let entities = self.entities().await;
        self.data
            .respawn_crystals
            .iter()
            .filter_map(|uuid| {
                entities
                    .iter()
                    .find(|e| e.uuid == *uuid && e.kind == EntityType::EndCrystal)
                    .cloned()
            })
            .collect()
    }

    pub(super) async fn respawn_tick(
        &mut self,
        stage: RespawnStage,
        crystals: &[EntitySnapshot],
        time: i32,
    ) {
        let uuids: Vec<u128> = crystals.iter().map(|c| c.uuid).collect();
        match summoning_cue(stage, time, self.spikes.len()) {
            SummoningCue::Wait => {}
            SummoningCue::Roar => self.roar().await,
            SummoningCue::AimAtSummonPoint => {
                self.crystal_action(&uuids, CrystalAction::Beam(Some(SUMMON_POINT)))
                    .await;
            }
            SummoningCue::AimAtSpike(index) => {
                let spike = self.spikes[index];
                let top = BlockPos::new(spike.center_x, spike.height + 1, spike.center_z);
                self.crystal_action(&uuids, CrystalAction::Beam(Some(top)))
                    .await;
            }
            SummoningCue::RebuildSpike(index) => self.rebuild_spike(self.spikes[index]).await,
            SummoningCue::BeginBeaming => {
                self.crystal_action(&uuids, CrystalAction::Beam(Some(SUMMON_POINT)))
                    .await;
                self.set_respawn_stage(RespawnStage::PreparingToSummonPillars)
                    .await;
            }
            SummoningCue::Advance(next) => self.set_respawn_stage(next).await,
            SummoningCue::Finish => {
                self.set_respawn_stage(RespawnStage::End).await;
                self.reset_spike_crystals().await;
                self.crystal_action(&uuids, CrystalAction::Detonate).await;
            }
        }
    }

    /// Moves the summoning on; its end brings the new dragon.
    pub(super) async fn set_respawn_stage(&mut self, stage: RespawnStage) {
        self.data.respawn_time = 0;
        if stage == RespawnStage::End {
            self.data.respawn_stage = None;
            self.data.dragon_killed = false;
            if let Some(dragon) = self.create_dragon().await {
                self.notify_summoned(&dragon);
            }
        } else {
            self.data.respawn_stage = Some(stage);
        }
        self.save();
    }

    /// A summoning crystal was lost: the portal opens again.
    pub(super) async fn abort_respawn(&mut self) {
        self.data.respawn_stage = None;
        self.data.respawn_time = 0;
        self.reset_spike_crystals().await;
        self.spawn_exit_portal(true).await;
        self.save();
    }

    pub(super) async fn roar(&self) {
        let Some(handle) = self.region(ChunkPos::of_block(SUMMON_POINT).region()).await else {
            return;
        };
        handle
            .request(
                FightRequest::LevelEvent {
                    event: play::level_event::ANIMATION_DRAGON_SUMMON_ROAR,
                    pos: SUMMON_POINT,
                    data: 0,
                }
                .request(),
            )
            .await;
    }

    pub(super) async fn crystal_action(&self, uuids: &[u128], action: CrystalAction) {
        if uuids.is_empty() {
            return;
        }
        for region in self.arena().await {
            let (reply, done) = oneshot::channel();
            region
                .request(
                    FightRequest::Crystals {
                        uuids: uuids.to_vec(),
                        action,
                        reply,
                    }
                    .request(),
                )
                .await;
            let _ = done.await;
        }
    }

    /// The crystals on the spikes lose their protection and beams.
    pub(super) async fn reset_spike_crystals(&self) {
        let uuids: Vec<u128> = self
            .entities()
            .await
            .into_iter()
            .filter(|e| {
                e.kind == EntityType::EndCrystal
                    && self
                        .spikes
                        .iter()
                        .any(|spike| intersects(e.bounds, spike_top(spike)))
            })
            .map(|e| e.uuid)
            .collect();
        self.crystal_action(&uuids, CrystalAction::Release).await;
    }

    /// Clears the space around a spike, blasts it and builds it again with
    /// a protected crystal aiming at the summoning point.
    pub(super) async fn rebuild_spike(&mut self, spike: EndSpike) {
        let air = Block::Air.default_state();
        let (cx, h, cz) = (spike.center_x, spike.height, spike.center_z);
        let r = SPIKE_CLEAR_RADIUS;
        let mut clear = Vec::new();
        for z in cz - r..=cz + r {
            for y in h - r..=h + r {
                for x in cx - r..=cx + r {
                    clear.push((BlockPos::new(x, y, z), air));
                }
            }
        }
        self.set_blocks(clear, false).await;
        let blocks = if self.shared.rule_bool("block_explosion_drop_decay") {
            BlockInteraction::DestroyWithDecay
        } else {
            BlockInteraction::Destroy
        };
        let center = [f64::from(cx) + 0.5, f64::from(h), f64::from(cz) + 0.5];
        if let Some(handle) = self.region(ChunkPos::of_position(center).region()).await {
            let (reply, done) = oneshot::channel();
            handle
                .request(
                    FightRequest::Explode {
                        explosion: Explosion {
                            center,
                            radius: SPIKE_BLAST_POWER,
                            fire: false,
                            blocks,
                            damage_type: &tuff_vanilla_data::damage_type::EXPLOSION,
                            igniter: None,
                            culprit: None,
                            dangerous_wither_skull: false,
                            spares_nether_portals: false,
                        },
                        reply,
                    }
                    .request(),
                )
                .await;
            let _ = done.await;
        }
        let min_y = self.shared.dimension(self.dimension).shape.min_y;
        let (blocks, fire) = spike_blocks(&spike, min_y);
        self.set_blocks(blocks, false).await;
        let crystal = spike.crystal();
        if let Some(handle) = self.region(ChunkPos::of_block(crystal).region()).await {
            let yaw = self.rng.r#gen::<f32>() * 360.0;
            let summon_point = [SUMMON_POINT.x, SUMMON_POINT.y, SUMMON_POINT.z];
            let nbt = spike_crystal(&spike, yaw, true, Some(summon_point));
            let (reply, done) = oneshot::channel();
            handle
                .request(RegionRequest::Summon {
                    position: [
                        f64::from(crystal.x) + 0.5,
                        f64::from(crystal.y),
                        f64::from(crystal.z) + 0.5,
                    ],
                    nbt,
                    fresh: false,
                    reply,
                })
                .await;
            let _ = done.await;
        }
        self.set_blocks(fire.to_vec(), false).await;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SummoningCue {
    Wait,
    Roar,
    AimAtSummonPoint,
    AimAtSpike(usize),
    RebuildSpike(usize),
    BeginBeaming,
    Advance(RespawnStage),
    Finish,
}

/// Inclusive tick windows of a timed stage, checked in order; the first
/// window holding the stage clock decides the cue.
type Timeline = [(i32, i32, SummoningCue); 4];

const PREPARING: Timeline = [
    (
        100,
        i32::MAX,
        SummoningCue::Advance(RespawnStage::SummoningPillars),
    ),
    (95, 99, SummoningCue::Roar),
    (50, 52, SummoningCue::Roar),
    (0, 0, SummoningCue::Roar),
];

const AWAKENING: Timeline = [
    (100, i32::MAX, SummoningCue::Finish),
    (80, 99, SummoningCue::Roar),
    (0, 0, SummoningCue::AimAtSummonPoint),
    (i32::MIN, 4, SummoningCue::Roar),
];

fn on_timeline(timeline: &Timeline, clock: i32) -> SummoningCue {
    timeline
        .iter()
        .find(|&&(from, to, _)| (from..=to).contains(&clock))
        .map_or(SummoningCue::Wait, |&(_, _, cue)| cue)
}

/// What the summoning does on tick `clock` of `stage`, with `spikes`
/// pillars to raise one per beam window.
pub(super) fn summoning_cue(stage: RespawnStage, clock: i32, spikes: usize) -> SummoningCue {
    match stage {
        RespawnStage::Start => SummoningCue::BeginBeaming,
        RespawnStage::PreparingToSummonPillars => on_timeline(&PREPARING, clock),
        RespawnStage::SummoningPillars => {
            let window = (clock / PILLAR_BEAM_TICKS) as usize;
            match (clock % PILLAR_BEAM_TICKS, window < spikes) {
                (0, true) => SummoningCue::AimAtSpike(window),
                (0, false) => SummoningCue::Advance(RespawnStage::SummoningDragon),
                (phase, true) if phase == PILLAR_BEAM_TICKS - 1 => {
                    SummoningCue::RebuildSpike(window)
                }
                _ => SummoningCue::Wait,
            }
        }
        RespawnStage::SummoningDragon => on_timeline(&AWAKENING, clock),
        RespawnStage::End => SummoningCue::Wait,
    }
}
