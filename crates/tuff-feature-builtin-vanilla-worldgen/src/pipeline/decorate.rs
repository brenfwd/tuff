use super::*;

impl Shard {
    pub(super) fn is_decorated(&self, pos: ChunkPos) -> bool {
        if self.owns(pos) {
            self.slots.get(&pos).is_some_and(|s| s.decorated)
        } else {
            self.known_decorated.contains(&pos)
        }
    }

    /// Starts collecting the window of `center` (owned here) if something
    /// waits on its decoration.
    pub(super) fn try_decorate(&mut self, center: ChunkPos) {
        let Some(&priority) = self.needed.get(&center) else {
            return;
        };
        if self.is_decorated(center) || self.decorating.contains(&center) {
            return;
        }
        if self.decorating.len() >= self.max_decorating {
            if self
                .queued
                .get(&center)
                .is_none_or(|(queued, _)| priority < *queued)
            {
                self.queue_seq += 1;
                self.queued.insert(center, (priority, self.queue_seq));
                self.queue
                    .push((Reverse(priority), Reverse(self.queue_seq), center));
            }
            return;
        }
        self.queued.remove(&center);
        let mut groups: Vec<(usize, Vec<ChunkPos>)> = Vec::new();
        for pos in around(center) {
            let owner = self.routing.owner(pos);
            match groups.iter_mut().find(|(o, _)| *o == owner) {
                Some((_, positions)) => positions.push(pos),
                None => groups.push((owner, vec![pos])),
            }
        }
        groups.sort_by_key(|(owner, _)| *owner);
        let id = self.next_ticket;
        self.next_ticket += 1;
        self.decorating.insert(center);
        self.acquisitions.insert(
            id,
            Acquisition {
                center,
                window: (0..9).map(|_| None).collect(),
                groups: groups.into(),
            },
        );
        self.acquire_next(id);
    }

    /// Starts the decorations waiting in line while there is room.
    pub(super) fn admit(&mut self) {
        while self.decorating.len() < self.max_decorating {
            let Some((_, Reverse(seq), center)) = self.queue.pop() else {
                break;
            };
            if self
                .queued
                .get(&center)
                .is_some_and(|(_, latest)| *latest == seq)
            {
                self.queued.remove(&center);
                self.try_decorate(center);
            }
        }
    }

    pub(super) fn acquire_next(&mut self, id: u64) {
        let Some((owner, positions)) = self
            .acquisitions
            .get_mut(&id)
            .and_then(|a| a.groups.pop_front())
        else {
            return;
        };
        let ticket = Ticket {
            shard: self.index,
            id,
        };
        self.post(owner, ShardMessage::Acquire { ticket, positions });
    }

    /// Lends every waiting request whose chunks are all ready.
    pub(super) fn try_grants(&mut self) {
        let mut index = 0;
        while index < self.pending_grants.len() {
            let ready = self.pending_grants[index].positions.iter().all(|pos| {
                matches!(
                    self.slots.get(pos).map(|s| &s.state),
                    Some(SlotState::Ready(_))
                )
            });
            if !ready {
                index += 1;
                continue;
            }
            let grant = self.pending_grants.remove(index);
            let mut chunks = Vec::with_capacity(grant.positions.len());
            for pos in &grant.positions {
                let slot = self.slots.get_mut(pos).expect("checked ready");
                let SlotState::Ready(proto) = std::mem::replace(&mut slot.state, SlotState::Lent)
                else {
                    unreachable!("checked ready");
                };
                chunks.push(*proto);
            }
            self.lent += chunks.len();
            let Ticket { shard, id } = grant.ticket;
            self.post(shard, ShardMessage::Granted { id, chunks });
        }
    }

    pub(super) fn granted(&mut self, id: u64, chunks: Vec<ProtoChunk>) {
        let Some(acquisition) = self.acquisitions.get_mut(&id) else {
            return;
        };
        let center = acquisition.center;
        for chunk in chunks {
            let index = (chunk.pos.z - center.z + 1) * 3 + chunk.pos.x - center.x + 1;
            acquisition.window[index as usize] = Some(chunk);
        }
        if !acquisition.groups.is_empty() {
            self.acquire_next(id);
            return;
        }
        let acquisition = self.acquisitions.remove(&id).expect("present above");
        let window: Vec<ProtoChunk> = acquisition
            .window
            .into_iter()
            .map(|chunk| chunk.expect("every part of the window was granted"))
            .collect();
        if self.is_decorated(center) {
            self.decorating.remove(&center);
            self.release(window);
            self.admit();
            return;
        }
        let generator = self.generator.clone();
        let positions: Vec<ChunkPos> = around(center).collect();
        self.spawn_job(
            move || {
                let _span = tuff_timings::span!("worldgen.decorate");
                let staged = &*generator;
                JobResult::Decorated(center, staged.decorate(center, window))
            },
            JobResult::DecorationPanicked(center, positions),
        );
    }

    /// Hands chunks back to their owners.
    pub(super) fn release(&mut self, chunks: Vec<ProtoChunk>) {
        let mut by_owner: Vec<(usize, Vec<ProtoChunk>)> = Vec::new();
        for chunk in chunks {
            let owner = self.routing.owner(chunk.pos);
            match by_owner.iter_mut().find(|(o, _)| *o == owner) {
                Some((_, list)) => list.push(chunk),
                None => by_owner.push((owner, vec![chunk])),
            }
        }
        for (owner, chunks) in by_owner {
            self.post(owner, ShardMessage::Release { chunks });
        }
    }

    pub(super) fn decorated(&mut self, center: ChunkPos, chunks: Vec<ProtoChunk>) {
        self.decorating.remove(&center);
        if let Some(slot) = self.slots.get_mut(&center) {
            slot.decorated = true;
        }
        self.release(chunks);
        self.became_decorated(center);
        self.admit();
    }

    /// Hands `pos` (owned here) to whoever asked once it and its
    /// neighbours are decorated and it is not in a decoration window.
    pub(super) fn try_deliver(&mut self, pos: ChunkPos) {
        if !self.wanted.contains_key(&pos)
            || !around(pos).all(|n| self.is_decorated(n))
            || !self.ensure_terrain(pos)
        {
            return;
        }
        let Some(slot) = self.slots.get_mut(&pos) else {
            return;
        };
        let SlotState::Ready(proto) = std::mem::replace(&mut slot.state, SlotState::Delivering)
        else {
            unreachable!("checked above");
        };
        let requesters = self.wanted.remove(&pos).unwrap_or_default();
        self.priorities.remove(&pos);
        self.drop_candidate(pos);
        self.committed.remove(&pos);
        let generator = self.generator.clone();
        let shape = self.shape;
        let tx = self.jobs_tx.clone();
        let tags = self.tags.current();
        self.in_flight += 1;
        rayon::spawn(move || {
            let _span = tuff_timings::span!("worldgen.finish");
            let finished = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let staged = &*generator;
                tuff_datapack::tags::sync_scope(tags.clone(), || staged.finish(*proto))
            }));
            let finished = finished.unwrap_or_else(|_| {
                tuff_core::log::error!("finishing chunk {pos:?} panicked; it stays empty");
                Finished::new(Chunk::empty(shape))
            });
            let _ = tx.send(match send_finished(pos, finished, &requesters) {
                None => JobResult::Delivered(pos),
                Some(finished) => JobResult::Undelivered(
                    pos,
                    Box::new(tuff_datapack::tags::sync_scope(tags, || {
                        saved::unfinished(pos, finished)
                    })),
                ),
            });
        });
    }

    /// Saves and drops chunks in progress that nothing needed for a while
    /// (just drops them without storage).
    pub(super) fn evict(&mut self) {
        let tick = self.tick;
        self.compact_queue();
        let mut protected: HashSet<ChunkPos> = HashSet::new();
        for pos in self.wanted.keys() {
            protected.extend(around(*pos));
        }
        for pos in self.needed.keys() {
            protected.extend(around(*pos));
        }
        for grant in &self.pending_grants {
            protected.extend(grant.positions.iter().copied());
        }
        let keep_saved = self.storage.is_some();
        let mut to_save = Vec::new();
        self.slots.retain(|pos, slot| {
            let idle = tick.saturating_sub(slot.last_used);
            if protected.contains(pos) || idle < SAVE_IDLE_TICKS {
                return true;
            }
            let saved = keep_saved && slot.persist;
            if idle < EVICT_TICKS && !(saved && matches!(slot.state, SlotState::Ready(_))) {
                return true;
            }
            match &slot.state {
                SlotState::Ready(_) if saved => {
                    let SlotState::Ready(proto) =
                        std::mem::replace(&mut slot.state, SlotState::Lent)
                    else {
                        unreachable!("matched above");
                    };
                    to_save.push((*pos, (*proto).clone(), slot.decorated));
                    slot.state = SlotState::Saving(proto);
                    true
                }
                SlotState::Ready(_) | SlotState::Delivered => false,
                _ => true,
            }
        });
        self.starts.retain(|_, slot| {
            !matches!(slot.state, StartsState::Ready(_))
                || !slot.waiting.is_empty()
                || tick.saturating_sub(slot.last_used) < EVICT_TICKS
        });
        let wanted = &self.wanted;
        self.known_decorated
            .retain(|pos| around(*pos).any(|n| wanted.contains_key(&n)));
        self.save(to_save);
    }

    /// Encodes chunks for storage on the rayon pool.
    pub(super) fn save(&mut self, chunks: Vec<(ChunkPos, ProtoChunk, bool)>) {
        let Some(storage) = self.storage.clone() else {
            return;
        };
        if chunks.is_empty() {
            return;
        }
        let tx = self.jobs_tx.clone();
        self.in_flight += 1;
        rayon::spawn(move || {
            let _span = tuff_timings::span!("worldgen.save");
            let blobs = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                chunks
                    .into_iter()
                    .filter_map(|(pos, proto, decorated)| {
                        match storage
                            .codec()
                            .encode_root(saved::to_nbt(&proto, decorated))
                        {
                            Ok(blob) => Some((pos, blob)),
                            Err(error) => {
                                tuff_core::log::error!(
                                    "cannot save chunk {pos:?} in progress: {error}"
                                );
                                None
                            }
                        }
                    })
                    .collect()
            }))
            .unwrap_or_default();
            let _ = tx.send(JobResult::Encoded(blobs));
        });
    }

    /// Writes encoded chunks that are still to be saved.
    pub(super) fn store(&mut self, blobs: Vec<(ChunkPos, Vec<u8>)>) {
        let Some(storage) = &self.storage else {
            return;
        };
        let blobs: Vec<(ChunkPos, Vec<u8>)> = blobs
            .into_iter()
            .filter(|(pos, _)| {
                matches!(
                    self.slots.get(pos).map(|s| &s.state),
                    Some(SlotState::Saving(_))
                )
            })
            .collect();
        if blobs.is_empty() {
            return;
        }
        let positions: Vec<ChunkPos> = blobs.iter().map(|(pos, _)| *pos).collect();
        let answer = storage.write(blobs);
        let tx = self.jobs_tx.clone();
        self.in_flight += 1;
        tokio::spawn(async move {
            let _ = answer.await;
            let _ = tx.send(JobResult::Stored(positions));
        });
    }

    /// Moves a requested save along: it writes every chunk in progress the
    /// pipeline may write, and answers once they are stored.
    pub(super) fn continue_save(&mut self) {
        if self.saving.is_empty() {
            return;
        }
        if !self.save_queued {
            self.save_queued = true;
            let mut to_save = Vec::new();
            if self.storage.is_some() {
                for (pos, slot) in &mut self.slots {
                    if !slot.persist || !matches!(slot.state, SlotState::Ready(_)) {
                        continue;
                    }
                    let SlotState::Ready(proto) =
                        std::mem::replace(&mut slot.state, SlotState::Lent)
                    else {
                        unreachable!("matched above");
                    };
                    to_save.push((*pos, (*proto).clone(), slot.decorated));
                    slot.state = SlotState::Saving(proto);
                }
            }
            self.save(to_save);
        }
        if self.in_flight > 0 {
            return;
        }
        self.save_queued = false;
        for done in self.saving.drain(..) {
            let _ = done.send(());
        }
        if self.wanted.is_empty() {
            self.shrink_tables();
        }
    }
}
