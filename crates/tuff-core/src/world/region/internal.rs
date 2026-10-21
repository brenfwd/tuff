use super::*;

impl RegionActor {
    pub(super) fn on_internal(&mut self, message: RegionInternal) {
        let _span = span!("region.internal", self.timings_tag());
        match message {
            RegionInternal::NeighborUp(handle) => {
                let pos = handle.pos;
                for (chunk, column) in &self.columns {
                    if pos.is_adjacent(*chunk) {
                        handle.internal(RegionInternal::Halo {
                            pos: *chunk,
                            chunk: Some(column.chunk.clone()),
                        });
                    }
                }
                self.mirror_pois_to(&handle);
                self.border.neighbors.insert(pos, handle);
            }
            RegionInternal::Footprints { from, boxes } => {
                if boxes.is_empty() {
                    self.sim.redstone.foreign.remove(&from);
                } else {
                    self.sim.redstone.foreign.insert(from, boxes);
                }
            }
            RegionInternal::NeighborDown(pos) => {
                self.border.neighbors.remove(&pos);
                self.border
                    .halo_pending
                    .retain(|chunk, _| chunk.region() != pos);
                self.chunk_io.pois.drop_mirror_of(pos);
                self.sim.redstone.foreign.remove(&pos);
                let gone: Vec<ChunkPos> = self
                    .border
                    .halo
                    .keys()
                    .copied()
                    .filter(|chunk| chunk.region() == pos)
                    .collect();
                for chunk in gone {
                    self.border.halo.remove(&chunk);
                    self.border.halo_pending.remove(&chunk);
                    self.border.neighbourhood_cache = None;
                    self.light.light_ops.push(LightOp::Remove(chunk));
                }
            }
            RegionInternal::Halo { pos, chunk } => match chunk {
                Some(chunk) => match self.border.halo.insert(pos, chunk.clone()) {
                    Some(old) => {
                        for (x, y, z, state) in old.changed_blocks(&chunk) {
                            self.light.light_ops.push(LightOp::SetBlock {
                                chunk: pos,
                                x,
                                y,
                                z,
                                state,
                            });
                        }
                        self.anti_xray_halo_changed(pos, &old, &chunk);
                    }
                    None => {
                        self.border.neighbourhood_cache = None;
                        self.border.halo_pending.insert(pos, self.sched.server_tick);
                        self.light.light_ops.push(LightOp::Add {
                            pos,
                            chunk,
                            owned: false,
                        });
                    }
                },
                None => {
                    let was_pending = self.border.halo_pending.remove(&pos).is_some();
                    if self.border.halo.remove(&pos).is_some() {
                        self.border.neighbourhood_cache = None;
                        self.light.light_ops.push(LightOp::Remove(pos));
                    }
                    if was_pending {
                        self.request_neighbour_packets(pos);
                    }
                }
            },
            RegionInternal::HaloLoading { pos } => {
                if !self.border.halo.contains_key(&pos) {
                    self.border
                        .halo_pending
                        .entry(pos)
                        .or_insert(self.sched.server_tick);
                }
            }
            RegionInternal::Entity { entity, seq } => self.receive_entity(entity, seq),
            RegionInternal::EntityViewers { entity, viewers } => {
                self.receive_entity_viewers(entity, viewers)
            }
            RegionInternal::PoiMirror { pos, records } => {
                self.chunk_io.pois.set_mirror(pos, records.as_deref())
            }
            RegionInternal::PoiTickets(changes) => self.poi_tickets(changes),
            RegionInternal::BorderMerge(step) => self.border_merge(step),
            RegionInternal::CommandBlock(remote) => self.command_block_remote(remote),
            RegionInternal::ItemTaken {
                item,
                offer,
                player,
                taken,
            } => self.item_taken(item, offer, &player, taken),
            RegionInternal::ShoulderTaken {
                entity,
                offer,
                player,
                accepted,
            } => self.shoulder_taken(entity, offer, &player, accepted),
            RegionInternal::Place(place) => self.place(place),
            RegionInternal::OpenHalf {
                place,
                pos,
                from,
                spectator,
                reply,
            } => self.open_half(place, pos, from, spectator, reply),
            RegionInternal::HalfOpened {
                place,
                spectator,
                half,
            } => {
                if let Some(half) = half {
                    self.half_opened(place, spectator, half);
                }
            }
            RegionInternal::HalfSnapshot { pos, items } => self.half_snapshot(pos, items),
            RegionInternal::HalfRequest { pos, reply } => self.half_requested(pos, reply),
            RegionInternal::HalfInsert {
                pos,
                face,
                stack,
                cause,
            } => self.with_cause(cause, |region| region.half_inserted(pos, face, stack)),
            message @ (RegionInternal::HalfDrain { .. } | RegionInternal::HalfDrained { .. }) => {
                self.golem_drain_message(message)
            }
            RegionInternal::HalfPull {
                pos,
                slot,
                hopper,
                reply,
            } => self.with_cause(BlockCause::Block(Block::Hopper), |region| {
                region.half_pulled_from(pos, slot, hopper, reply)
            }),
            RegionInternal::HalfPulled { hopper, stack } => self
                .with_cause(BlockCause::Block(Block::Hopper), |region| {
                    region.half_arrived(hopper, stack)
                }),
            RegionInternal::ReleaseHalf { player, pos } => self.remove_viewer(pos, player),
            RegionInternal::Blast(blast) => {
                self.apply_blast(&blast, false);
            }
            RegionInternal::BlockUpdates { tick, updates } => {
                self.border.pending_updates.push((tick, updates));
            }
            RegionInternal::Run(run) => run.run(self),
            RegionInternal::Area(deferred) => self.border.area.push(deferred),
            RegionInternal::ForceChunks { chunks, on } => {
                self.border.neighbourhood_cache = None;
                for chunk in chunks {
                    if on {
                        self.chunk_io.forced.insert(chunk);
                        if !self.columns.contains_key(&chunk) && self.sched.closing.is_none() {
                            self.load(chunk);
                        }
                    } else {
                        self.chunk_io.forced.remove(&chunk);
                    }
                }
            }
            RegionInternal::SaveAll { done, snapshotted } => {
                if self.shared.saves().saves() {
                    let dirty: Vec<ChunkPos> = self
                        .columns
                        .iter()
                        .filter(|(_, c)| c.needs_save())
                        .map(|(pos, _)| *pos)
                        .collect();
                    for pos in dirty {
                        self.start_save(pos);
                    }
                }
                if let Some(snapshotted) = snapshotted {
                    let _ = snapshotted.send(());
                }
                self.sched.save_waiters.extend(done);
            }
            RegionInternal::Generated { pos, finished } => self.generated(pos, *finished),
            RegionInternal::Shutdown { done } => self.begin_close(done),
        }
    }
}
