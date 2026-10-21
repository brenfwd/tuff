use super::*;

/// The chunks and entities this client sees, the send queue and the
/// regions it talks to.
pub struct ChunkView {
    pub center: ChunkPos,
    pub view_distance: i32,
    pub subscribed: HashSet<ChunkPos>,
    /// Chunks just outside the view that regions keep loaded and lit (but
    /// unsent) so the view's edge is sent with its final light.
    pub lit_ring: HashSet<ChunkPos>,
    pub sent: HashSet<ChunkPos>,
    /// Subscribed chunks asked for only if they already exist.
    pub peeked: HashSet<ChunkPos>,
    pub(super) mirror: Box<mirror::ChunkMirror>,
    pub queue: VecDeque<QueuedChunk>,
    pub unacked_batches: u32,
    pub max_unacked_batches: u32,
    pub chunks_per_tick: f32,
    pub batch_quota: f32,
    pub regions: HashMap<RegionKey, RegionHandle>,
    /// Regions of the current dimension that know where this player is.
    pub presence: HashSet<RegionPos>,
    pub entities: EntityView,
}

impl Default for ChunkView {
    fn default() -> Self {
        Self {
            center: ChunkPos::new(0, 0),
            view_distance: MIN_VIEW_DISTANCE,
            subscribed: HashSet::new(),
            lit_ring: HashSet::new(),
            sent: HashSet::new(),
            peeked: HashSet::new(),
            mirror: Box::default(),
            queue: VecDeque::new(),
            unacked_batches: 0,
            max_unacked_batches: INITIAL_UNACKED_BATCHES,
            chunks_per_tick: INITIAL_CHUNKS_PER_TICK,
            batch_quota: 0.0,
            regions: HashMap::new(),
            presence: HashSet::new(),
            entities: EntityView::default(),
        }
    }
}

impl PlayerSession {
    /// Everything a client needs after Login or Respawn.
    pub async fn send_world(&mut self) {
        let shared = self.shared.clone();
        let border = shared.dimension(self.dimension).border();
        self.teleport(self.pose.position);
        self.send(&border.initialize());
        let time = shared.time_packet();
        self.send(&time);
        let tick_state = *shared.tick_state.borrow();
        self.send(&play::SetTickingState {
            rate: tick_state.rate,
            frozen: tick_state.frozen,
        });
        let spawn = shared.spawn();
        self.send(&play::SetDefaultSpawnPosition {
            dimension: &spawn.dimension,
            location: spawn.pos,
            yaw: spawn.yaw,
            pitch: spawn.pitch,
        });
        self.send(&play::GameEvent {
            event: play::game_event::START_WAITING_FOR_CHUNKS,
            value: 0.0,
        });
        for event in shared.weather.borrow().join_events() {
            self.send(&event);
        }
        let rules = shared.game_rules.borrow().clone();
        self.send(&play::GameEvent {
            event: play::game_event::IMMEDIATE_RESPAWN,
            value: f32::from(u8::from(rules.bool("immediate_respawn"))),
        });
        self.send(&play::GameEvent {
            event: play::game_event::LIMITED_CRAFTING,
            value: f32::from(u8::from(rules.bool("limited_crafting"))),
        });
        self.view.center = self.chunk();
        self.send(&play::SetChunkCacheCenter {
            x: self.view.center.x,
            z: self.view.center.z,
        });
        self.send_inventory();
        self.send_recipe_book();
        self.published.last_sent_status = None;
        self.published.last_sent_experience = None;
        self.send_experience();
        self.send_effects();
        if !self.mode.post_effects.is_empty() {
            let list = self.mode.post_effects.clone();
            self.send(&play::PostEffects { effects: &list });
        }
        self.send_options_metadata();
        self.update_view().await;
        self.reclaim_vehicle().await;
        self.publish(true).await;
    }

    /// Takes the player out of the world: its entity disappears, nothing
    /// is subscribed and every region ticket is released. `seq` must have
    /// been advanced so viewers drop the entity for good.
    pub async fn detach_from_regions(&mut self) {
        self.leave_vehicle().await;
        let home = (self.dimension, self.chunk().region());
        if let Some(region) = self.view.regions.get(&home) {
            region
                .request(RegionRequest::PlayerRemoved {
                    player: self.id,
                    seq: self.published.seq,
                })
                .await;
        }
        let dimension = self.dimension;
        for region in std::mem::take(&mut self.view.presence) {
            if let Some(handle) = self.view.regions.get(&(dimension, region)) {
                handle
                    .request(RegionRequest::PresenceGone { player: self.id })
                    .await;
            }
        }
        self.published.published_home = None;
        let mut by_region: HashMap<RegionPos, Vec<ChunkPos>> = HashMap::new();
        for chunk in self
            .view
            .subscribed
            .drain()
            .chain(self.view.lit_ring.drain())
        {
            by_region.entry(chunk.region()).or_default().push(chunk);
        }
        for (region, chunks) in by_region {
            if let Some(handle) = self.view.regions.get(&(dimension, region)) {
                handle
                    .request(RegionRequest::Unsubscribe {
                        player: self.id,
                        chunks,
                    })
                    .await;
            }
        }
        self.view.sent.clear();
        self.view.mirror.clear();
        self.view.queue.clear();
        self.view.entities = EntityView::default();
        for ((dimension, region), _) in self.view.regions.drain() {
            let _ = self
                .shared
                .manager
                .send(ManagerMessage::Release { dimension, region })
                .await;
        }
    }

    pub async fn region(&mut self, region: RegionPos) -> Option<RegionHandle> {
        self.region_in(self.dimension, region).await
    }

    pub async fn region_in(
        &mut self,
        dimension: DimensionId,
        region: RegionPos,
    ) -> Option<RegionHandle> {
        if let Some(handle) = self.view.regions.get(&(dimension, region)) {
            return Some(handle.clone());
        }
        let (reply, handle) = oneshot::channel();
        self.shared
            .manager
            .send(ManagerMessage::Acquire {
                dimension,
                region,
                reply,
            })
            .await
            .ok()?;
        let handle = handle.await.ok()?;
        self.view
            .regions
            .insert((dimension, region), handle.clone());
        Some(handle)
    }

    /// Loads (or generates) the chunk at `position` in `dimension` and
    /// waits for it before returning, so a teleport destination is never
    /// released to a player before its terrain exists. A no-op (resolves
    /// within the same call) when the chunk is already loaded.
    pub async fn await_chunk_loaded(&mut self, dimension: DimensionId, position: [f64; 3]) {
        let pos = BlockPos::new(
            position[0].floor() as i32,
            position[1].floor() as i32,
            position[2].floor() as i32,
        );
        let region = ChunkPos::of_block(pos).region();
        let _ = self
            .shared
            .manager
            .send(ManagerMessage::Hold {
                dimension,
                center: ChunkPos::of_block(pos),
                ticket: crate::world::messages::TimedTicket::PlayerSpawn,
            })
            .await;
        let Some(handle) = self.region_in(dimension, region).await else {
            return;
        };
        let (reply, answer) = oneshot::channel();
        handle
            .request(RegionRequest::AwaitChunk { pos, reply })
            .await;
        let _ = answer.await;
    }

    pub fn region_of(&self, pos: BlockPos) -> Option<RegionHandle> {
        self.view
            .regions
            .get(&(self.dimension, ChunkPos::of_block(pos).region()))
            .cloned()
    }

    pub fn area(&self) -> ViewArea<'_> {
        ViewArea {
            center: self.view.center,
            view_distance: self.view.view_distance,
            subscribed: &self.view.subscribed,
            spectator: self.mode.game_mode == GameMode::Spectator,
            position: self.pose.position,
            range_percent: self.shared.live.broadcast_range_percent(),
        }
    }

    /// Shows and hides entities by tracking range once this player has
    /// moved into another section.
    pub(super) fn follow_entities(&mut self) {
        let mut out = Vec::new();
        let mut entities = std::mem::take(&mut self.view.entities);
        entities.follow(&self.area(), self.tick, &mut out);
        self.view.entities = entities;
        self.send_all(out);
    }

    pub fn effective_view_distance(&self) -> i32 {
        (self.info.view_distance as i32).clamp(MIN_VIEW_DISTANCE, self.shared.live.view_distance())
    }

    /// Subscribes to newly visible chunks, unloads ones out of view, and
    /// takes or releases region tickets to match.
    pub async fn update_view(&mut self) {
        let center = self.chunk();
        if center != self.view.center {
            self.view.center = center;
            self.send(&play::SetChunkCacheCenter {
                x: center.x,
                z: center.z,
            });
        }
        self.view.view_distance = self.effective_view_distance();
        let wanted = center.view(self.view.view_distance);
        let wanted_set: HashSet<ChunkPos> = wanted.iter().copied().collect();
        let peek = self.mode.game_mode == GameMode::Spectator
            && !self.shared.rule_bool("spectators_generate_chunks");
        let ring = match peek {
            true => HashSet::new(),
            false => lit_ring(&wanted_set),
        };
        let upgraded: Vec<ChunkPos> = match peek {
            true => Vec::new(),
            false => std::mem::take(&mut self.view.peeked)
                .into_iter()
                .filter(|c| wanted_set.contains(c) && !self.view.sent.contains(c))
                .collect(),
        };
        let mut needed: HashSet<RegionPos> = wanted.iter().map(|c| c.region()).collect();
        needed.extend(ring.iter().map(|c| c.region()));
        needed.extend(regions_near(self.pose.position, PRESENCE_MARGIN));

        let mut added: Vec<(RegionPos, Vec<ChunkPos>)> = Vec::new();
        for chunk in wanted.iter().filter(|c| !self.view.subscribed.contains(c)) {
            match added.iter_mut().find(|(r, _)| *r == chunk.region()) {
                Some((_, chunks)) => chunks.push(*chunk),
                None => added.push((chunk.region(), vec![*chunk])),
            }
        }
        let held: Vec<ChunkPos> = ring
            .iter()
            .filter(|c| !self.view.lit_ring.contains(c))
            .copied()
            .collect();
        let dropped: Vec<ChunkPos> = self
            .view
            .subscribed
            .iter()
            .chain(&self.view.lit_ring)
            .filter(|c| !wanted_set.contains(c) && !ring.contains(c))
            .copied()
            .collect();
        let forgotten: Vec<ChunkPos> = self
            .view
            .subscribed
            .iter()
            .filter(|c| !wanted_set.contains(c))
            .copied()
            .collect();
        for chunk in forgotten {
            self.view.subscribed.remove(&chunk);
            self.view.peeked.remove(&chunk);
            self.view.mirror.remove(chunk);
            if self.view.sent.remove(&chunk) {
                self.send(&play::ForgetLevelChunk {
                    x: chunk.x,
                    z: chunk.z,
                });
            }
        }
        self.view.lit_ring = ring;
        for (region, chunks) in by_region(held) {
            if let Some(handle) = self.region(region).await {
                handle
                    .request(RegionRequest::HoldLit {
                        player: self.id,
                        chunks,
                    })
                    .await;
            }
        }
        for (region, chunks) in by_region(upgraded) {
            if let Some(handle) = self.region(region).await {
                handle
                    .request(RegionRequest::Subscribe {
                        player: self.conn.handle.clone(),
                        chunks,
                    })
                    .await;
            }
        }
        for (region, chunks) in &added {
            self.view.subscribed.extend(chunks.iter().copied());
            if peek {
                self.view.peeked.extend(chunks.iter().copied());
            }
            if let Some(handle) = self.region(*region).await {
                let player = self.conn.handle.clone();
                let chunks = chunks.clone();
                handle
                    .request(match peek {
                        true => RegionRequest::Peek { player, chunks },
                        false => RegionRequest::Subscribe { player, chunks },
                    })
                    .await;
            }
        }
        for (region, chunks) in by_region(dropped) {
            if let Some(handle) = self.view.regions.get(&(self.dimension, region)) {
                handle
                    .request(RegionRequest::Unsubscribe {
                        player: self.id,
                        chunks,
                    })
                    .await;
            }
        }
        self.view.queue.retain(|q| wanted_set.contains(&q.pos));
        for region in needed.iter() {
            self.region(*region).await;
        }
        let dimension = self.dimension;
        let unneeded: Vec<RegionKey> = self
            .view
            .regions
            .keys()
            .filter(|(d, r)| {
                *d != dimension || (!needed.contains(r) && !self.view.presence.contains(r))
            })
            .copied()
            .collect();
        for key in unneeded {
            self.view.regions.remove(&key);
            let (dimension, region) = key;
            let _ = self
                .shared
                .manager
                .send(ManagerMessage::Release { dimension, region })
                .await;
        }
        let mut out = Vec::new();
        let tick = self.tick;
        let mut entities = std::mem::take(&mut self.view.entities);
        entities.refresh(&self.area(), tick, &mut out);
        self.view.entities = entities;
        self.send_all(out);
    }

    /// What this client has been sent of the world.
    pub fn client_world(&self) -> &mirror::ChunkMirror {
        &self.view.mirror
    }

    pub fn client_world_mut(&mut self) -> &mut mirror::ChunkMirror {
        &mut self.view.mirror
    }

    pub fn predict_client_block(&mut self, pos: BlockPos, state: tuff_vanilla_data::BlockState) {
        self.view.mirror.predict(pos, state);
    }
}
