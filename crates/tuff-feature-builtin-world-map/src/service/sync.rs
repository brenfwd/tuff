use super::*;

impl Actor {
    pub(super) fn publish_changes(&mut self) {
        if self.changed.is_empty() {
            return;
        }
        let changed = std::mem::take(&mut self.changed);
        let body = if changed.len() > MAX_EVENT_TILES {
            r#"{"resync":true}"#.to_owned()
        } else {
            let tiles: Vec<serde_json::Value> = changed
                .iter()
                .map(|(k, v)| serde_json::json!([k.dim, k.level, k.x, k.z, v]))
                .collect();
            serde_json::json!({ "tiles": tiles }).to_string()
        };
        self.shared.web.events.publish(TILES_TOPIC, "changed", body);
    }

    pub(super) fn fetch_entities(&mut self) {
        if self.entities_busy {
            return;
        }
        let now = Instant::now();
        let mut by_dim: BTreeMap<u16, Vec<crate::entities::Area>> = BTreeMap::new();
        for lease in self.leases.values().filter(|l| l.until > now) {
            by_dim
                .entry(lease.view.dim)
                .or_default()
                .push(crate::entities::Area {
                    min: lease.view.min,
                    max: lease.view.max,
                });
        }
        self.entities_busy = true;
        let shared = self.shared.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let mut rows = Vec::new();
            for (dim, areas) in by_dim {
                for mut row in crate::entities::collect(&shared, dim, areas).await {
                    row["dim"] = dim.into();
                    rows.push(row);
                }
            }
            rows.truncate(crate::entities::MAX_MARKERS);
            let _ = tx.send(Internal::Entities(
                serde_json::json!({ "entities": rows }).to_string(),
            ));
        });
    }

    pub(super) fn fetch_players(&self) {
        let now = Instant::now();
        let viewed: HashSet<usize> = self
            .leases
            .values()
            .filter(|lease| lease.until > now)
            .map(|lease| usize::from(lease.view.dim))
            .collect();
        let shared = self.shared.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let (reply, answer) = oneshot::channel();
            if shared
                .coordinator
                .send(CoordinatorMessage::Players { reply })
                .await
                .is_err()
            {
                return;
            }
            let Ok(players) = answer.await else {
                return;
            };
            let index: HashMap<DimensionId, usize> = shared
                .dimensions
                .iter()
                .enumerate()
                .map(|(i, d)| (d.id, i))
                .collect();
            let mut rows: Vec<serde_json::Value> = players
                .iter()
                .filter(|p| p.state.alive)
                .filter_map(|p| {
                    let dim = *index.get(&p.state.dimension)?;
                    if !viewed.contains(&dim) {
                        return None;
                    }
                    let [x, y, z] = p.state.position;
                    let round = |v: f64| (v * 10.0).round() / 10.0;
                    Some(serde_json::json!({
                        "uuid": format_uuid(p.uuid),
                        "name": p.name,
                        "dim": dim,
                        "x": round(x),
                        "y": round(y),
                        "z": round(z),
                        "yaw": p.state.rotation[0].round(),
                        "mode": format!("{:?}", p.state.game_mode).to_ascii_lowercase(),
                    }))
                })
                .collect();
            rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            let _ = tx.send(Internal::Players(
                serde_json::json!({ "players": rows }).to_string(),
            ));
        });
    }

    /// A pre-generation job saved `batch`: the storage listing learns of
    /// its file regions, and chunks cached tiles show are read back and
    /// patched in, since they were loaded too briefly for a poll to see.
    pub(super) fn on_generated(&mut self, batch: &Generated) {
        let Some(dim) = self
            .shared
            .dimensions
            .iter()
            .position(|d| d.id == batch.dimension)
        else {
            return;
        };
        let dim = dim as u16;
        if let Some(listing) = self.listings.get_mut(&dim) {
            let missing: Vec<FileRegionPos> = batch
                .chunks
                .iter()
                .map(|pos| pos.file_region())
                .filter(|file| !listing.regions.contains(file))
                .collect();
            if !missing.is_empty() {
                Arc::make_mut(&mut listing.regions).extend(missing);
            }
        }
        let covered: Vec<ChunkPos> = batch
            .chunks
            .iter()
            .copied()
            .filter(|pos| self.covered(dim, *pos))
            .collect();
        if covered.is_empty() {
            return;
        }
        let ceiling = self.shared.dimensions.get(batch.dimension).kind.has_ceiling;
        let id = batch.dimension;
        let shared = self.shared.clone();
        let pool = self.pool.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let (key, shape) = {
                let dimension = shared.dimensions.get(id);
                (dimension.key.clone(), dimension.shape)
            };
            let saved_reads = covered.len() as u64;
            let blobs = gather::read_saved(&shared, &key, covered).await;
            let (reply, answer) = oneshot::channel();
            let render_shared = shared.clone();
            pool.spawn(move || {
                let updates: Vec<Update> = blobs
                    .into_par_iter()
                    .filter_map(|(pos, blob)| {
                        let chunk =
                            gather::decode(&render_shared, pos, shape, Source::Saved(blob))?;
                        Some(Update {
                            dim,
                            pos,
                            stamp: None,
                            samples: Box::new(sample_chunk(&chunk, ceiling)),
                        })
                    })
                    .collect();
                let _ = reply.send(updates);
            });
            let updates = answer.await.unwrap_or_default();
            let _ = tx.send(Internal::Refreshed {
                updates,
                saved_reads,
            });
        });
    }

    /// Missed pre-generation batches: drops every cached tile and listing
    /// so they are drawn again from storage.
    pub(super) fn resync(&mut self) {
        let keys: Vec<TileKey> = self
            .tiles
            .keys()
            .copied()
            .filter(|k| !self.pending.contains_key(k) && !self.rechecks.contains_key(k))
            .collect();
        for key in keys {
            self.remove(key);
            self.disk.dirty.remove(&key);
        }
        self.listings.clear();
        self.live.clear();
        self.series.cache_bytes.set(self.bytes as i64);
    }

    /// Whether a cached tile shows `chunk` (directly, through an
    /// ancestor, or as the row north of a level-0 tile).
    pub(super) fn covered(&self, dim: u16, chunk: ChunkPos) -> bool {
        let mut key = TileKey::of_chunk(dim, chunk);
        if chunk.z.rem_euclid(CHUNKS) == CHUNKS - 1 {
            let south = TileKey::of_chunk(dim, ChunkPos::new(chunk.x, chunk.z + 1));
            if matches!(self.tiles.get(&south).map(|e| &e.data), Some(Data::Base(_))) {
                return true;
            }
        }
        loop {
            if self.tiles.contains_key(&key) {
                return true;
            }
            match key.parent() {
                Some(parent) => key = parent,
                None => return false,
            }
        }
    }

    pub(super) fn prune_live(&mut self) {
        let keys: Vec<(u16, ChunkPos)> = self.live.keys().copied().collect();
        for key in keys {
            if !self.covered(key.0, key.1) {
                self.live.remove(&key);
            }
        }
    }

    pub(super) fn start_poll(&mut self) {
        if self.polling {
            return;
        }
        let dims: Vec<u16> = {
            let mut dims: Vec<u16> = self.tiles.keys().map(|k| k.dim).collect();
            dims.sort_unstable();
            dims.dedup();
            dims
        };
        if dims.is_empty() {
            for waiter in self.poll_waiters.drain(..) {
                let _ = waiter.send(());
            }
            return;
        }
        self.polling = true;
        let side = tuff_world_model::pos::REGION_CHUNKS;
        let mut ids: HashMap<u16, DimensionId> = HashMap::new();
        let mut regions: HashSet<(u16, RegionPos)> = HashSet::new();
        for &dim in &dims {
            let Some(id) = dimension_id(&self.shared, dim) else {
                continue;
            };
            ids.insert(dim, id);
            let running = self.running.get(&dim).map(Vec::as_slice).unwrap_or(&[]);
            for key in self.tiles.keys().filter(|k| k.dim == dim) {
                let files = key.file_regions();
                for region in running {
                    if overlaps(region_files(*region), files) {
                        regions.insert((dim, *region));
                    }
                }
            }
        }
        let mut wanted: HashMap<(u16, RegionPos), gather::Wanted> = HashMap::new();
        for (dim, region) in regions {
            let mut list = Vec::new();
            for dz in 0..side {
                for dx in 0..side {
                    let pos = ChunkPos::new(region.x * side + dx, region.z * side + dz);
                    if self.covered(dim, pos) {
                        list.push((pos, self.live.get(&(dim, pos)).copied()));
                    }
                }
            }
            if !list.is_empty() {
                wanted.insert((dim, region), list);
            }
        }
        let stored: HashMap<u16, Arc<HashSet<FileRegionPos>>> = self
            .listings
            .iter()
            .map(|(d, l)| (*d, l.regions.clone()))
            .collect();
        let ceilings: HashMap<u16, bool> = ids
            .iter()
            .map(|(dim, id)| (*dim, self.shared.dimensions.get(*id).kind.has_ceiling))
            .collect();
        let shared = self.shared.clone();
        let pool = self.pool.clone();
        let tx = self.internal_tx.clone();
        tokio::spawn(async move {
            let mut running = Vec::new();
            let mut handles = HashMap::new();
            for (&dim, &id) in &ids {
                let list = gather::running(&shared, id).await;
                running.push((dim, list.iter().map(|h| h.pos).collect()));
                handles.insert(dim, list);
            }
            let mut found: Vec<(u16, ChunkPos, Source)> = Vec::new();
            let mut forgotten = Vec::new();
            let mut reread: HashMap<u16, Vec<ChunkPos>> = HashMap::new();
            for ((dim, region), list) in wanted {
                let Some(handle) = handles
                    .get(&dim)
                    .and_then(|hs| hs.iter().find(|h| h.pos == region))
                else {
                    for (pos, known) in list {
                        if known.is_some() {
                            reread.entry(dim).or_default().push(pos);
                        }
                    }
                    continue;
                };
                let known: HashMap<ChunkPos, Option<u64>> = list.iter().copied().collect();
                let Some(answer) = gather::ask_region(handle, list).await else {
                    continue;
                };
                for (pos, stamp, chunk) in answer.changed {
                    found.push((dim, pos, Source::Live(stamp, chunk)));
                }
                for pos in answer.absent {
                    if known.get(&pos).copied().flatten().is_some() {
                        reread.entry(dim).or_default().push(pos);
                    }
                }
            }
            let mut saved_reads = 0;
            for (dim, positions) in reread {
                let Some(&id) = ids.get(&dim) else { continue };
                let key = shared.dimensions.get(id).key.clone();
                let empty = Arc::new(HashSet::new());
                let files = stored.get(&dim).unwrap_or(&empty);
                let (keep, gone): (Vec<ChunkPos>, Vec<ChunkPos>) = positions
                    .into_iter()
                    .partition(|p| files.contains(&p.file_region()));
                forgotten.extend(gone.iter().map(|p| (dim, *p)));
                let blobs = gather::read_saved(&shared, &key, keep.clone()).await;
                saved_reads += keep.len() as u64;
                for pos in keep {
                    forgotten.push((dim, pos));
                    if let Some(blob) = blobs.get(&pos) {
                        found.push((dim, pos, Source::Saved(blob.clone())));
                    }
                }
            }
            let (reply, answer) = oneshot::channel();
            let render_shared = shared.clone();
            pool.spawn(move || {
                let updates: Vec<Update> = found
                    .into_par_iter()
                    .filter_map(|(dim, pos, source)| {
                        let id = *ids.get(&dim)?;
                        let shape = render_shared.dimensions.get(id).shape;
                        let stamp = match &source {
                            Source::Live(stamp, _) => Some(*stamp),
                            Source::Saved(_) => None,
                        };
                        let chunk = gather::decode(&render_shared, pos, shape, source)?;
                        let ceiling = ceilings.get(&dim).copied().unwrap_or(false);
                        Some(Update {
                            dim,
                            pos,
                            stamp,
                            samples: Box::new(sample_chunk(&chunk, ceiling)),
                        })
                    })
                    .collect();
                let _ = reply.send(updates);
            });
            let updates = answer.await.unwrap_or_default();
            let _ = tx.send(Internal::Polled {
                running,
                updates,
                forgotten,
                saved_reads,
            });
        });
    }
}
