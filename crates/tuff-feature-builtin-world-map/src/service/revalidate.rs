//! Checking persisted tiles against the world. Chunk stamps do not
//! survive a restart, so a tile read from the persistent cache is served
//! at once but marked stale; once someone is served it, it is drawn again
//! in the background (a zoomed tile by checking its children first) after
//! all other work. A tile that turns out unchanged keeps its version, so
//! the page's ETag stays valid; a changed one gets a new version and its
//! cached ancestors are patched.

use super::{Actor, BaseResult, Data};
use crate::tile::{Rect, TileKey};

/// A tile being checked: how many of its children still are, and the
/// tiles waiting on it.
pub struct Recheck {
    missing: usize,
    parents: Vec<TileKey>,
    rounds: u8,
}

/// Times a zoomed tile goes back for children evicted before it could
/// be composed.
const MAX_ROUNDS: u8 = 3;

/// Level-0 tiles one served stale tile may queue for a recheck. The
/// rest stay stale and are taken up when the tile is served again.
pub const RECHECK_BASE_BUDGET: usize = 16;

impl Actor {
    fn fresh(&self, key: TileKey) -> bool {
        self.tiles.get(&key).is_some_and(|e| !e.stale)
    }

    fn recheck_cap(&self) -> usize {
        (self.max_jobs / 2).max(1)
    }

    pub(super) fn request_rechecks(&mut self) {
        for key in std::mem::take(&mut self.recheck_requests) {
            self.recheck_budget = RECHECK_BASE_BUDGET;
            self.recheck(key, None);
        }
    }

    fn recheck(&mut self, key: TileKey, parent: Option<TileKey>) {
        if let Some(recheck) = self.rechecks.get_mut(&key) {
            recheck.parents.extend(parent);
            return;
        }
        if self.fresh(key) {
            if let Some(parent) = parent {
                self.recheck_child_done(parent);
            }
            return;
        }
        if self.may_have_content(key) == Some(false) {
            let had = self
                .tiles
                .get(&key)
                .is_some_and(|e| e.data.pixels().is_some());
            self.install(key, Data::Empty, None);
            if had {
                self.stats.recheck_changed += 1;
                self.propagate(key, Rect::FULL);
            }
            self.stats.rechecked += 1;
            if let Some(parent) = parent {
                self.recheck_child_done(parent);
            }
            return;
        }
        if key.level == 0 && self.recheck_budget == 0 {
            self.stats.recheck_deferred += 1;
            if let Some(parent) = parent {
                self.recheck_child_done(parent);
            }
            return;
        }
        if key.level == 0 {
            self.recheck_budget -= 1;
        }
        self.rechecks.insert(
            key,
            Recheck {
                missing: 0,
                parents: parent.into_iter().collect(),
                rounds: 0,
            },
        );
        match key.children() {
            None => self.recheck_queue.push_back(key),
            Some(children) => {
                let todo: Vec<TileKey> = children.into_iter().filter(|c| !self.fresh(*c)).collect();
                self.check_children(key, todo);
            }
        }
    }

    fn check_children(&mut self, key: TileKey, todo: Vec<TileKey>) {
        if todo.is_empty() {
            self.finish_zoomed_recheck(key);
            return;
        }
        if let Some(recheck) = self.rechecks.get_mut(&key) {
            recheck.missing = todo.len();
        }
        for child in todo {
            self.recheck(child, Some(key));
        }
    }

    fn recheck_child_done(&mut self, parent: TileKey) {
        let Some(recheck) = self.rechecks.get_mut(&parent) else {
            return;
        };
        recheck.missing = recheck.missing.saturating_sub(1);
        if recheck.missing == 0 {
            self.finish_zoomed_recheck(parent);
        }
    }

    fn finish_zoomed_recheck(&mut self, key: TileKey) {
        let children = key.children().expect("zoomed tiles have children");
        let gone: Vec<TileKey> = children
            .into_iter()
            .filter(|c| !self.tiles.contains_key(c))
            .collect();
        if !gone.is_empty() {
            let retry = self.rechecks.get_mut(&key).is_some_and(|recheck| {
                recheck.rounds += 1;
                recheck.rounds <= MAX_ROUNDS
            });
            if retry {
                self.check_children(key, gone);
            } else {
                self.end_recheck(key);
            }
            return;
        }
        let (data, stale) = self.downsampled(children);
        let same = self
            .tiles
            .get(&key)
            .is_some_and(|e| e.data.pixels() == data.pixels());
        if same {
            if let Some(entry) = self.tiles.get_mut(&key) {
                entry.stale = stale;
            }
        } else {
            let had = self.tiles.contains_key(&key);
            self.install(key, data, None);
            if let Some(entry) = self.tiles.get_mut(&key) {
                entry.stale = stale;
            }
            if had {
                self.stats.recheck_changed += 1;
                self.propagate(key, Rect::FULL);
            }
        }
        self.end_recheck(key);
    }

    pub(super) fn end_recheck(&mut self, key: TileKey) {
        if let Some(recheck) = self.rechecks.remove(&key) {
            self.stats.rechecked += 1;
            for parent in recheck.parents {
                self.recheck_child_done(parent);
            }
        }
    }

    /// Starts drawing queued level-0 rechecks while the pool has room
    /// that no other work wants, using at most half of it.
    pub(super) fn pump_rechecks(&mut self) {
        while self.in_flight < self.max_jobs && self.rechecking < self.recheck_cap() {
            let Some(key) = self.recheck_queue.pop_front() else {
                return;
            };
            if self.fresh(key) {
                self.end_recheck(key);
                continue;
            }
            self.start_base(key, true);
        }
    }

    pub(super) fn finish_base_recheck(&mut self, key: TileKey, result: Option<BaseResult>) {
        if let Some(result) = result {
            self.stats.saved_reads += result.saved_reads;
            for (pos, stamp) in result.stamps {
                self.live.insert((key.dim, pos), stamp);
            }
            let data = match result.tile {
                Some(tile) => Data::Base(tile),
                None => Data::Empty,
            };
            let same = self
                .tiles
                .get(&key)
                .is_some_and(|e| e.data.pixels() == data.pixels());
            if same {
                let entry = self.tiles.get_mut(&key).expect("checked above");
                entry.stale = false;
                let resampled = match (&entry.data, &data) {
                    (Data::Base(old), Data::Base(new)) => {
                        old.samples != new.samples || old.north != new.north
                    }
                    _ => false,
                };
                if resampled {
                    entry.data = data;
                    self.disk.dirty.insert(key);
                }
                if entry.png.is_none()
                    && let Some(png) = result.png
                {
                    self.set_png(key, Some(png));
                }
            } else {
                let had = self.tiles.contains_key(&key);
                self.install(key, data, result.png);
                if had {
                    self.stats.recheck_changed += 1;
                    self.propagate(key, Rect::FULL);
                }
            }
        }
        self.end_recheck(key);
        self.evict();
    }
}
