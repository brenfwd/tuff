use super::*;

impl Actor {
    /// Patches cached tiles with polled chunk changes.
    pub(super) fn apply(&mut self, updates: Vec<Update>) {
        let mut dirty: BTreeMap<TileKey, Rect> = BTreeMap::new();
        let mut orphaned: HashSet<TileKey> = HashSet::new();
        for update in updates {
            let Update {
                dim,
                pos,
                stamp,
                samples,
            } = update;
            match stamp {
                Some(stamp) => self.live.insert((dim, pos), stamp),
                None => self.live.remove(&(dim, pos)),
            };
            let base = TileKey::of_chunk(dim, pos);
            let first = base.first_chunk();
            let (cx, cz) = ((pos.x - first.x) as usize, (pos.z - first.z) as usize);
            let blank = samples.iter().all(|s| s.is_empty());
            match self.tiles.get_mut(&base).map(|e| &mut e.data) {
                Some(Data::Base(tile)) => {
                    let mut same = true;
                    for z in 0..16 {
                        let row = (cz * 16 + z) * SIDE + cx * 16;
                        if tile.samples[row..row + 16] != samples[z * 16..z * 16 + 16] {
                            same = false;
                            break;
                        }
                    }
                    if !same {
                        tile.place(cx, cz, &samples);
                        let rect = BaseTile::chunk_rect(cx, cz);
                        dirty
                            .entry(base)
                            .and_modify(|r| *r = r.union(rect))
                            .or_insert(rect);
                    }
                }
                Some(data @ Data::Empty) if !blank => {
                    let mut tile = Box::new(BaseTile::empty());
                    tile.place(cx, cz, &samples);
                    *data = Data::Base(tile);
                    self.bytes += BaseTile::BYTES;
                    dirty.insert(base, Rect::FULL);
                }
                Some(_) => {}
                None if !blank => {
                    orphaned.insert(base);
                }
                None => {}
            }
            if pos.z.rem_euclid(CHUNKS) == CHUNKS - 1 {
                let south = TileKey::of_chunk(dim, ChunkPos::new(pos.x, pos.z + 1));
                if let Some(Data::Base(tile)) = self.tiles.get_mut(&south).map(|e| &mut e.data) {
                    let row = &tile.north[cx * 16..cx * 16 + 16];
                    if row != &samples[15 * 16..] {
                        tile.place_north(cx, &samples);
                        let rect = Rect {
                            x0: cx * 16,
                            z0: 0,
                            x1: cx * 16 + 16,
                            z1: 1,
                        };
                        dirty
                            .entry(south)
                            .and_modify(|r| *r = r.union(rect))
                            .or_insert(rect);
                    }
                }
            }
        }
        for (base, rect) in dirty {
            let Some(entry) = self.tiles.get_mut(&base) else {
                continue;
            };
            if let Data::Base(tile) = &mut entry.data {
                tile.shade(rect, base.origin());
            }
            self.stats.patched += 1;
            self.series.patched.inc();
            self.touch(base);
            self.propagate(base, rect);
        }
        for base in orphaned {
            self.drop_ancestors(base);
        }
        self.series.cache_bytes.set(self.bytes as i64);
    }

    /// A tile's pixels changed: new version, no PNG until asked again.
    pub(super) fn touch(&mut self, key: TileKey) {
        let version = self.version();
        if let Some(entry) = self.tiles.get_mut(&key) {
            entry.version = version;
            self.bytes -= entry.png.as_ref().map_or(0, Bytes::len);
            entry.png = None;
            if entry.data.pixels().is_some() {
                self.disk.dirty.insert(key);
            }
        }
        self.changed.insert(key, version);
    }

    /// Downsamples `rect` of `child` into each cached ancestor in turn;
    /// past the first ancestor that cannot be patched, cached ones are
    /// dropped to be rebuilt.
    pub(super) fn propagate(&mut self, mut child: TileKey, mut rect: Rect) {
        while let Some(parent) = child.parent() {
            let patchable = matches!(
                self.tiles.get(&parent).map(|e| &e.data),
                Some(Data::Zoomed(_))
            ) && self
                .tiles
                .get(&child)
                .and_then(|e| e.data.pixels())
                .is_some();
            if !patchable {
                if matches!(self.tiles.get(&parent).map(|e| &e.data), Some(Data::Empty)) {
                    self.remove(parent);
                }
                self.drop_ancestors(parent);
                return;
            }
            let mut entry = self.tiles.remove(&parent).expect("checked above");
            if let Data::Zoomed(pixels) = &mut entry.data {
                let source = self.tiles[&child].data.pixels();
                downsample(source, child.quadrant(), rect, pixels);
            }
            self.tiles.insert(parent, entry);
            self.touch(parent);
            rect = rect.in_parent(child.quadrant());
            child = parent;
        }
    }

    pub(super) fn drop_ancestors(&mut self, mut key: TileKey) {
        while let Some(parent) = key.parent() {
            self.remove(parent);
            key = parent;
        }
    }

    pub(super) fn evict(&mut self) {
        let budget = self
            .shared
            .config
            .get_int(CACHE_KEY)
            .unwrap_or(128)
            .clamp(1, 1 << 20) as usize
            * 1024
            * 1024;
        let now = Instant::now();
        let idle: Vec<TileKey> = self
            .tiles
            .iter()
            .filter(|(k, e)| {
                now.saturating_duration_since(e.used) > IDLE
                    && !self.pending.contains_key(k)
                    && !self.rechecks.contains_key(k)
            })
            .map(|(k, _)| *k)
            .collect();
        for key in idle {
            self.evict_entry(key);
        }
        while self.bytes > budget {
            let Some(oldest) = self
                .tiles
                .iter()
                .filter(|(k, _)| !self.pending.contains_key(k) && !self.rechecks.contains_key(k))
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| *k)
            else {
                break;
            };
            self.evict_entry(oldest);
        }
        self.series.cache_bytes.set(self.bytes as i64);
    }
}
