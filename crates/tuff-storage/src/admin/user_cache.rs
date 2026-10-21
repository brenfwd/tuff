//! Names and UUIDs of players seen before, so name-based commands and the
//! panel know offline players across restarts. An entry expires a month
//! after the player was last added; at most [`MAX_ENTRIES`] are kept, the
//! least recently used going first.

use std::collections::HashMap;

use super::{NameAndId, civil_from_days, days_from_civil};

pub const MAX_ENTRIES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserCacheEntry {
    pub user: NameAndId,
    /// Seconds since the epoch.
    pub expires: i64,
}

/// One calendar month after `seconds`, with the day clamped to the
/// month's length.
pub fn month_after(seconds: i64) -> i64 {
    let days = seconds.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let length = (days_from_civil(if m == 12 { y + 1 } else { y }, m % 12 + 1, 1)
        - days_from_civil(y, m, 1)) as u32;
    days_from_civil(y, m, d.min(length)) * 86_400 + seconds.rem_euclid(86_400)
}

#[derive(Debug)]
struct Slot {
    user: NameAndId,
    expires: i64,
    used: u64,
}

#[derive(Debug, Default)]
pub struct UserCache {
    by_uuid: HashMap<u128, Slot>,
    by_name: HashMap<String, u128>,
    clock: u64,
}

impl UserCache {
    /// Builds the cache from stored entries, most recently used first;
    /// expired entries and repeats are dropped.
    pub fn load(entries: Vec<UserCacheEntry>, now: i64) -> Self {
        let mut cache = Self::default();
        for entry in entries.into_iter().take(MAX_ENTRIES).rev() {
            if entry.expires >= now {
                cache.put(entry.user, entry.expires);
            }
        }
        cache
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn put(&mut self, user: NameAndId, expires: i64) {
        self.remove(user.uuid);
        if let Some(&old) = self.by_name.get(&user.name.to_lowercase()) {
            self.remove(old);
        }
        if self.by_uuid.len() >= MAX_ENTRIES
            && let Some(oldest) = self
                .by_uuid
                .iter()
                .min_by_key(|(_, slot)| slot.used)
                .map(|(&uuid, _)| uuid)
        {
            self.remove(oldest);
        }
        let used = self.tick();
        self.by_name.insert(user.name.to_lowercase(), user.uuid);
        self.by_uuid.insert(
            user.uuid,
            Slot {
                user,
                expires,
                used,
            },
        );
    }

    fn remove(&mut self, uuid: u128) {
        if let Some(slot) = self.by_uuid.remove(&uuid) {
            self.by_name.remove(&slot.user.name.to_lowercase());
        }
    }

    /// Records `user` as seen now, replacing any entry with its name or
    /// UUID.
    pub fn add(&mut self, user: NameAndId, now: i64) {
        self.put(user, month_after(now));
    }

    /// Whether a live entry has this name, ignoring case.
    pub fn contains(&self, name: &str, now: i64) -> bool {
        self.by_name
            .get(&name.to_lowercase())
            .and_then(|uuid| self.by_uuid.get(uuid))
            .is_some_and(|slot| slot.expires >= now)
    }

    /// The live entry with this name, ignoring case; a lookup counts as a
    /// use, and an expired entry is dropped.
    pub fn get(&mut self, name: &str, now: i64) -> Option<NameAndId> {
        let uuid = *self.by_name.get(&name.to_lowercase())?;
        if self.by_uuid.get(&uuid)?.expires < now {
            self.remove(uuid);
            return None;
        }
        let used = self.tick();
        let slot = self.by_uuid.get_mut(&uuid)?;
        slot.used = used;
        Some(slot.user.clone())
    }

    /// Every live entry.
    pub fn users(&self, now: i64) -> impl Iterator<Item = &NameAndId> {
        self.by_uuid
            .values()
            .filter(move |slot| slot.expires >= now)
            .map(|slot| &slot.user)
    }

    /// The entries to store, most recently used first.
    pub fn entries(&self) -> Vec<UserCacheEntry> {
        let mut slots: Vec<&Slot> = self.by_uuid.values().collect();
        slots.sort_by_key(|slot| std::cmp::Reverse(slot.used));
        slots
            .into_iter()
            .map(|slot| UserCacheEntry {
                user: slot.user.clone(),
                expires: slot.expires,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::parse_file_date;

    fn user(uuid: u128, name: &str) -> NameAndId {
        NameAndId {
            uuid,
            name: name.to_owned(),
        }
    }

    fn date(text: &str) -> i64 {
        parse_file_date(text).unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_month_is_a_calendar_month_clamped_to_its_length() {
        assert_eq!(
            month_after(date("2026-01-15 10:20:30 +0000")),
            date("2026-02-15 10:20:30 +0000")
        );
        assert_eq!(
            month_after(date("2026-01-31 00:00:00 +0000")),
            date("2026-02-28 00:00:00 +0000")
        );
        assert_eq!(
            month_after(date("2028-01-31 00:00:00 +0000")),
            date("2028-02-29 00:00:00 +0000")
        );
        assert_eq!(
            month_after(date("2026-12-31 23:59:59 +0000")),
            date("2027-01-31 23:59:59 +0000")
        );
        assert_eq!(
            month_after(date("2026-11-30 00:00:00 +0000")),
            date("2026-12-30 00:00:00 +0000")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn names_are_case_insensitive_and_follow_renames() {
        let mut cache = UserCache::default();
        cache.add(user(1, "Alice"), 0);
        assert_eq!(cache.get("alice", 0), Some(user(1, "Alice")));
        cache.add(user(1, "Alicia"), 10);
        assert_eq!(cache.get("alice", 10), None);
        assert_eq!(cache.get("ALICIA", 10), Some(user(1, "Alicia")));
        cache.add(user(2, "alicia"), 20);
        assert_eq!(cache.get("Alicia", 20), Some(user(2, "alicia")));
        assert_eq!(cache.users(20).count(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn entries_expire_a_month_after_they_were_added() {
        let start = date("2026-03-01 00:00:00 +0000");
        let mut cache = UserCache::default();
        cache.add(user(1, "Alice"), start);
        let expiry = date("2026-04-01 00:00:00 +0000");
        assert!(cache.contains("Alice", expiry));
        assert_eq!(cache.users(expiry + 1).count(), 0);
        assert_eq!(cache.get("Alice", expiry + 1), None);
        assert!(cache.entries().is_empty(), "an expired lookup drops it");
        let stored = vec![UserCacheEntry {
            user: user(2, "Bob"),
            expires: start - 1,
        }];
        assert!(UserCache::load(stored, start).entries().is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_least_recently_used_entry_goes_first_and_order_survives_a_reload() {
        let mut cache = UserCache::default();
        for i in 0..MAX_ENTRIES as u128 {
            cache.add(user(i, &format!("p{i}")), 0);
        }
        assert!(cache.get("p0", 0).is_some());
        cache.add(user(5000, "new"), 0);
        let entries = cache.entries();
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(entries[0].user, user(5000, "new"));
        assert_eq!(entries[1].user, user(0, "p0"));
        assert!(!cache.contains("p1", 0), "p1 was least recently used");
        let reloaded = UserCache::load(entries.clone(), 0);
        assert_eq!(reloaded.entries(), entries);
    }
}
