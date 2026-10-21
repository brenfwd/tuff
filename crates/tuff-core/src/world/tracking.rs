//! What one player's client knows about other entities. Regions report every
//! entity in chunks the player subscribes to; this decides what the client
//! actually sees: only entities in chunks within entity-tracking range, so
//! players far apart learn nothing about each other.
//!
//! Reports can come from different regions and arrive out of order, so each
//! carries the entity's sequence number and older reports are ignored, with
//! tombstones remembering the sequence of recently removed entities.

use std::collections::{HashMap, HashSet};

use bytes::Bytes;

use crate::protocol::play;
use crate::world::framed;
use crate::world::messages::{Packets, PlayerHandle, Shape, Track};
use tuff_world_model::pos::ChunkPos;

/// How long, in ticks, a removed entity's sequence number is remembered.
const TOMBSTONE_TICKS: u64 = 200;

/// An entity's tracking range in blocks after applying the broadcast
/// range percentage.
pub fn scaled_range(range: i32, percent: i32) -> i32 {
    (i64::from(range) * i64::from(percent) / 100) as i32
}

/// The 16-block cube holding `position`.
fn section_of(position: [f64; 3]) -> [i32; 3] {
    position.map(|v| (v.floor() as i32) >> 4)
}

/// The chunks a player's client has in view.
pub struct ViewArea<'a> {
    pub center: ChunkPos,
    pub view_distance: i32,
    pub subscribed: &'a HashSet<ChunkPos>,
    /// Whether the viewer spectates, and so also sees other spectators.
    pub spectator: bool,
    /// The viewer's own position.
    pub position: [f64; 3],
    /// `entity-broadcast-range-percentage`.
    pub range_percent: i32,
}

impl ViewArea<'_> {
    fn shows(&self, chunk: ChunkPos, spectator: bool) -> bool {
        self.subscribed.contains(&chunk)
            && chunk.is_tracked_from(self.center, self.view_distance)
            && (!spectator || self.spectator)
    }

    /// Whether an entity at `position` with a tracking `range` (blocks)
    /// is close enough horizontally to be seen.
    fn in_range(&self, position: [f64; 3], range: i32) -> bool {
        let visible =
            f64::from(scaled_range(range, self.range_percent).min(self.view_distance * 16));
        let (dx, dz) = (
            self.position[0] - position[0],
            self.position[2] - position[2],
        );
        dx * dx + dz * dz <= visible * visible
    }
}

/// A visible non-player entity and its last movement.
#[derive(Debug, Clone, Copy)]
pub struct Collider {
    pub kind: tuff_vanilla_data::registry::EntityType,
    pub position: [f64; 3],
    pub previous: [f64; 3],
    pub moved_at: u64,
    pub shape: Option<Shape>,
}

struct Seen {
    kind: tuff_vanilla_data::registry::EntityType,
    chunk: ChunkPos,
    seq: u64,
    position: [f64; 3],
    /// Where the previous report had it, and the tick this one arrived.
    previous: [f64; 3],
    moved_at: u64,
    shape: Option<Shape>,
    range: i32,
    /// The entity's section when its range was last checked, and the verdict:
    /// it is rechecked only when it or the viewer changes section.
    section: [i32; 3],
    in_range: bool,
    spawn: Packets,
    update: Packets,
    player: Option<PlayerHandle>,
    spectator: bool,
    spawned: bool,
    /// The latest Debug Entity Value packet per feed, replayed on spawn.
    debug: Vec<(u8, Bytes)>,
    /// The client applied every report up to `seq`, so it can follow
    /// the next one's changes.
    synced: bool,
}

#[derive(Default)]
pub struct EntityView {
    seen: HashMap<i32, Seen>,
    tombstones: HashMap<i32, (u64, u64)>,
    /// The viewer's section at the last refresh.
    viewer_section: Option<[i32; 3]>,
}

impl EntityView {
    fn is_stale(&self, entity: i32, seq: u64) -> bool {
        let last = self
            .seen
            .get(&entity)
            .map(|s| s.seq)
            .or_else(|| self.tombstones.get(&entity).map(|t| t.0));
        last.is_some_and(|last| seq < last)
    }

    fn forget(&mut self, entity: i32, seq: u64, tick: u64, removed: &mut Vec<i32>) {
        if let Some(seen) = self.seen.remove(&entity) {
            if seen.spawned {
                removed.push(entity);
            }
            self.tombstones.insert(entity, (seen.seq.max(seq), tick));
        }
    }

    /// `driving` marks the vehicle this player itself drives: its client
    /// simulates that entity, so echoing the server's stale position back
    /// would rubber-band it (and kill momentum, e.g. sliding on ice).
    pub fn track(
        &mut self,
        track: Track,
        area: &ViewArea,
        tick: u64,
        driving: bool,
        out: &mut Vec<Bytes>,
    ) {
        if self.is_stale(track.entity, track.seq) {
            return;
        }
        if !area.subscribed.contains(&track.chunk) {
            let mut removed = Vec::new();
            self.forget(track.entity, track.seq, tick, &mut removed);
            remove_packet(&removed, out);
            return;
        }
        let section = section_of(track.position);
        let seen = self.seen.get(&track.entity);
        let in_range = match seen {
            Some(seen) if seen.section == section => seen.in_range,
            _ => area.in_range(track.position, track.range),
        };
        let visible = in_range && area.shows(track.chunk, track.spectator);
        let was_spawned = seen.is_some_and(|s| s.spawned);
        let follows = seen.is_some_and(|s| s.synced && s.seq + 1 == track.seq);
        let mut synced = visible;
        match (was_spawned, visible) {
            (true, true) if driving => synced = false,
            (true, true) => match &track.moves {
                Some(moves) if follows => out.extend(moves.iter().cloned()),
                _ => {
                    out.extend(track.update.iter().cloned());
                    out.extend(track.head.iter().cloned());
                }
            },
            (false, true) => {
                out.extend(track.spawn.iter().cloned());
                out.extend(track.update.iter().cloned());
                if let Some(seen) = seen {
                    out.extend(seen.debug.iter().map(|(_, p)| p.clone()));
                }
            }
            (true, false) => remove_packet(&[track.entity], out),
            (false, false) => {}
        }
        self.tombstones.remove(&track.entity);
        let debug = self
            .seen
            .get_mut(&track.entity)
            .map(|old| std::mem::take(&mut old.debug))
            .unwrap_or_default();
        let (previous, moved_at) = match self.seen.get(&track.entity) {
            Some(old) if old.position != track.position => (old.position, tick),
            Some(old) => (old.previous, old.moved_at),
            None => (track.position, 0),
        };
        self.seen.insert(
            track.entity,
            Seen {
                kind: track.kind,
                chunk: track.chunk,
                seq: track.seq,
                position: track.position,
                previous,
                moved_at,
                shape: track.shape,
                range: track.range,
                section,
                in_range,
                spawn: track.spawn,
                update: track.update,
                player: track.player,
                spectator: track.spectator,
                spawned: visible,
                debug,
                synced,
            },
        );
    }

    pub fn untrack(&mut self, entity: i32, seq: u64, tick: u64, out: &mut Vec<Bytes>) {
        if self.is_stale(entity, seq) {
            return;
        }
        let mut removed = Vec::new();
        match self.seen.contains_key(&entity) {
            true => self.forget(entity, seq, tick, &mut removed),
            false => {
                self.tombstones.insert(entity, (seq, tick));
            }
        }
        remove_packet(&removed, out);
    }

    /// Forwards packets about an entity the client can currently see.
    pub fn entity_packets(&self, entity: i32, packets: &Packets, out: &mut Vec<Bytes>) {
        if self.seen.get(&entity).is_some_and(|s| s.spawned) {
            out.extend(packets.iter().cloned());
        }
    }

    /// Keeps `packet` as `entity`'s latest value of debug feed `feed`,
    /// sending it now if the entity is shown.
    pub fn debug_value(&mut self, entity: i32, feed: u8, packet: Bytes, out: &mut Vec<Bytes>) {
        let Some(seen) = self.seen.get_mut(&entity) else {
            return;
        };
        if seen.spawned {
            out.push(packet.clone());
        }
        match seen.debug.iter_mut().find(|(f, _)| *f == feed) {
            Some(slot) => slot.1 = packet,
            None => seen.debug.push((feed, packet)),
        }
    }

    /// Re-evaluates what is in range once the viewer enters another section.
    pub fn follow(&mut self, area: &ViewArea, tick: u64, out: &mut Vec<Bytes>) {
        if self.viewer_section != Some(section_of(area.position)) {
            self.refresh(area, tick, out);
        }
    }

    /// Re-evaluates everything after the player's own view changed.
    pub fn refresh(&mut self, area: &ViewArea, tick: u64, out: &mut Vec<Bytes>) {
        self.viewer_section = Some(section_of(area.position));
        let mut removed = Vec::new();
        let gone: Vec<i32> = self
            .seen
            .iter()
            .filter(|(_, seen)| !area.subscribed.contains(&seen.chunk))
            .map(|(id, _)| *id)
            .collect();
        for entity in gone {
            self.forget(entity, 0, tick, &mut removed);
        }
        for (id, seen) in &mut self.seen {
            seen.in_range = area.in_range(seen.position, seen.range);
            let visible = seen.in_range && area.shows(seen.chunk, seen.spectator);
            if visible && !seen.spawned {
                out.extend(seen.spawn.iter().cloned());
                out.extend(seen.update.iter().cloned());
                out.extend(seen.debug.iter().map(|(_, p)| p.clone()));
                seen.synced = true;
            } else if !visible && seen.spawned {
                removed.push(*id);
            }
            seen.spawned = visible;
        }
        self.tombstones
            .retain(|_, (_, at)| tick.saturating_sub(*at) < TOMBSTONE_TICKS);
        remove_packet(&removed, out);
    }

    /// A visible player's last known position and handle, for attacks.
    pub fn visible_player(&self, entity: i32) -> Option<([f64; 3], &PlayerHandle)> {
        let seen = self.seen.get(&entity).filter(|s| s.spawned)?;
        Some((seen.position, seen.player.as_ref()?))
    }

    /// Every visible player: its entity id, last known position and handle.
    pub fn visible_players(&self) -> impl Iterator<Item = (i32, [f64; 3], &PlayerHandle)> {
        self.seen.iter().filter_map(|(id, seen)| {
            let player = seen.player.as_ref().filter(|_| seen.spawned)?;
            Some((*id, seen.position, player))
        })
    }

    /// How far a visible entity moved in the last tick, zero once it
    /// stopped.
    pub fn visible_velocity(&self, entity: i32, tick: u64) -> [f64; 3] {
        match self.seen.get(&entity) {
            Some(s) if s.spawned && s.moved_at + 1 >= tick => {
                [0, 1, 2].map(|i| s.position[i] - s.previous[i])
            }
            _ => [0.0; 3],
        }
    }

    /// A visible entity that is not a player: its chunk and last known
    /// position.
    pub fn visible_entity(
        &self,
        entity: i32,
    ) -> Option<(tuff_vanilla_data::registry::EntityType, ChunkPos, [f64; 3])> {
        let seen = self
            .seen
            .get(&entity)
            .filter(|s| s.spawned && s.player.is_none())?;
        Some((seen.kind, seen.chunk, seen.position))
    }

    /// Every visible entity that is not a player: its id, chunk and last
    /// known position.
    pub fn visible_entities(
        &self,
    ) -> impl Iterator<
        Item = (
            i32,
            tuff_vanilla_data::registry::EntityType,
            ChunkPos,
            [f64; 3],
        ),
    > + '_ {
        self.seen
            .iter()
            .filter(|(_, s)| s.spawned && s.player.is_none())
            .map(|(id, s)| (*id, s.kind, s.chunk, s.position))
    }

    /// Every visible entity that is not a player, with where the previous
    /// report had it and the tick its position last changed, so callers can
    /// bound the box a client still interpolating shows.
    pub fn visible_colliders(&self) -> impl Iterator<Item = Collider> + '_ {
        self.seen
            .values()
            .filter(|s| s.spawned && s.player.is_none())
            .map(|s| Collider {
                kind: s.kind,
                position: s.position,
                previous: s.previous,
                moved_at: s.moved_at,
                shape: s.shape,
            })
    }

    pub fn is_visible(&self, entity: i32) -> bool {
        self.seen.get(&entity).is_some_and(|s| s.spawned)
    }
}

fn remove_packet(ids: &[i32], out: &mut Vec<Bytes>) {
    if !ids.is_empty() {
        out.push(framed(&play::RemoveEntities(ids)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn track(entity: i32, chunk: ChunkPos, seq: u64) -> Track {
        Track {
            entity,
            kind: tuff_vanilla_data::registry::EntityType::Tnt,
            dimension: crate::world::dimension::Dimensions::from_settings(
                &crate::storage::level::DimensionSettings::vanilla(),
                0,
                &crate::world::generator::NoGenerators,
            )
            .default_id(),
            chunk,
            seq,
            position: [
                f64::from(chunk.x * 16 + 8),
                64.0,
                f64::from(chunk.z * 16 + 8),
            ],
            range: 160,
            spawn: Arc::from(vec![Bytes::from_static(b"spawn")]),
            update: Arc::from(vec![Bytes::from_static(b"update")]),
            head: None,
            moves: None,
            player: None,
            spectator: false,
            shape: None,
        }
    }

    fn moved(entity: i32, chunk: ChunkPos, seq: u64) -> Track {
        Track {
            moves: Some(Arc::from(vec![Bytes::from_static(b"moves")])),
            ..track(entity, chunk, seq)
        }
    }

    fn area(subscribed: &HashSet<ChunkPos>) -> ViewArea<'_> {
        ViewArea {
            center: ChunkPos::new(0, 0),
            view_distance: 2,
            subscribed,
            spectator: false,
            position: [8.0, 64.0, 8.0],
            range_percent: 100,
        }
    }

    fn names(out: &[Bytes]) -> Vec<&[u8]> {
        out.iter()
            .map(|b| {
                if b"sum".contains(&b[0]) {
                    &b[..]
                } else {
                    b"remove"
                }
            })
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn spawns_updates_and_removes_by_distance() {
        let subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(2).into_iter().collect();
        let area = area(&subscribed);
        let mut view = EntityView::default();
        let mut out = Vec::new();
        view.track(track(1, ChunkPos::new(1, 0), 1), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"spawn"[..], b"update"]);
        out.clear();
        view.track(track(1, ChunkPos::new(2, 0), 2), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"update"[..]]);
        out.clear();
        // Sent but outside entity range: the client must not see it.
        view.track(track(1, ChunkPos::new(3, 0), 3), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"remove"[..]]);
        out.clear();
        view.track(track(1, ChunkPos::new(40, 0), 4), &area, 0, false, &mut out);
        assert!(out.is_empty());
        assert!(!view.is_visible(1));
    }

    #[tuff_core::watchdog]
    #[test]
    fn debug_values_wait_for_the_spawn_and_follow_it() {
        let subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(2).into_iter().collect();
        let area = area(&subscribed);
        let mut view = EntityView::default();
        let mut out = Vec::new();
        view.track(track(1, ChunkPos::new(3, 0), 1), &area, 0, false, &mut out);
        assert!(out.is_empty());
        view.debug_value(1, 5, Bytes::from_static(b"sold"), &mut out);
        view.debug_value(1, 5, Bytes::from_static(b"snew"), &mut out);
        assert!(out.is_empty(), "not shown yet");
        view.track(track(1, ChunkPos::new(0, 0), 2), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"spawn"[..], b"update", b"snew"]);
        out.clear();
        view.debug_value(1, 5, Bytes::from_static(b"snext"), &mut out);
        assert_eq!(names(&out), [&b"snext"[..]]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn colliders_remember_the_last_movement() {
        let subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(2).into_iter().collect();
        let area = area(&subscribed);
        let mut view = EntityView::default();
        let mut out = Vec::new();
        view.track(track(1, ChunkPos::new(0, 0), 1), &area, 5, false, &mut out);
        let mut next = track(1, ChunkPos::new(0, 0), 2);
        next.position = [1.0, 0.0, 0.0];
        view.track(next, &area, 9, false, &mut out);
        let mut still = track(1, ChunkPos::new(0, 0), 3);
        still.position = [1.0, 0.0, 0.0];
        view.track(still, &area, 12, false, &mut out);
        let c = view.visible_colliders().next().unwrap();
        assert_eq!((c.previous, c.moved_at), ([8.0, 64.0, 8.0], 9));
    }

    #[tuff_core::watchdog]
    #[test]
    fn colliders_carry_the_reported_shape() {
        let subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(2).into_iter().collect();
        let area = area(&subscribed);
        let mut view = EntityView::default();
        let mut shaped = track(1, ChunkPos::new(0, 0), 1);
        shaped.shape = Some(Shape {
            non_solid: true,
            offset: None,
        });
        view.track(shaped, &area, 5, false, &mut Vec::new());
        let c = view.visible_colliders().next().unwrap();
        assert!(c.shape.unwrap().non_solid);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_driver_never_gets_its_own_vehicles_movement_back() {
        let subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(2).into_iter().collect();
        let area = area(&subscribed);
        let mut view = EntityView::default();
        let mut out = Vec::new();
        // A not-yet-seen vehicle still spawns for its driver.
        view.track(track(1, ChunkPos::new(0, 0), 1), &area, 0, true, &mut out);
        assert_eq!(names(&out), [&b"spawn"[..], b"update"]);
        out.clear();
        // Once spawned, its position echoes are muted for the driver...
        view.track(track(1, ChunkPos::new(1, 0), 2), &area, 0, true, &mut out);
        assert!(out.is_empty());
        assert!(view.is_visible(1));
        // ...but not for anyone else watching the same reports.
        view.track(track(1, ChunkPos::new(1, 0), 3), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"update"[..]]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn only_viewers_that_saw_the_previous_report_get_just_the_changes() {
        let subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(2).into_iter().collect();
        let area = area(&subscribed);
        let mut view = EntityView::default();
        let mut out = Vec::new();
        view.track(moved(1, ChunkPos::new(0, 0), 1), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"spawn"[..], b"update"]);
        out.clear();
        view.track(moved(1, ChunkPos::new(0, 0), 2), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"moves"[..]]);
        out.clear();
        // A report it never got (seq 3) breaks the chain.
        view.track(moved(1, ChunkPos::new(0, 0), 4), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"update"[..]]);
        out.clear();
        view.track(moved(1, ChunkPos::new(0, 0), 5), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"moves"[..]]);
        out.clear();
        // The same report again (from another region) is resent whole.
        view.track(moved(1, ChunkPos::new(0, 0), 5), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"update"[..]]);
        out.clear();
        // Driving skips reports, so the next one after is full.
        view.track(moved(1, ChunkPos::new(0, 0), 6), &area, 0, true, &mut out);
        assert!(out.is_empty());
        view.track(moved(1, ChunkPos::new(0, 0), 7), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"update"[..]]);
        out.clear();
        // Out of range then back: respawned whole, then followed again.
        view.track(moved(1, ChunkPos::new(3, 0), 8), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"remove"[..]]);
        out.clear();
        view.track(moved(1, ChunkPos::new(0, 0), 9), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"spawn"[..], b"update"]);
        out.clear();
        view.track(moved(1, ChunkPos::new(0, 0), 10), &area, 0, false, &mut out);
        assert_eq!(names(&out), [&b"moves"[..]]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_viewer_whose_range_grows_gets_the_full_state_then_changes() {
        let subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(4).into_iter().collect();
        let mut view = EntityView::default();
        let mut out = Vec::new();
        view.track(
            moved(1, ChunkPos::new(3, 0), 1),
            &area(&subscribed),
            0,
            false,
            &mut out,
        );
        assert!(out.is_empty());
        let wider = ViewArea {
            view_distance: 4,
            ..area(&subscribed)
        };
        view.refresh(&wider, 0, &mut out);
        assert_eq!(names(&out), [&b"spawn"[..], b"update"]);
        out.clear();
        view.track(moved(1, ChunkPos::new(3, 0), 2), &wider, 0, false, &mut out);
        assert_eq!(names(&out), [&b"moves"[..]]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn stale_reports_never_resurrect_entities() {
        let subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(2).into_iter().collect();
        let area = area(&subscribed);
        let mut view = EntityView::default();
        let mut out = Vec::new();
        view.track(track(1, ChunkPos::new(0, 0), 5), &area, 0, false, &mut out);
        view.untrack(1, 6, 0, &mut out);
        out.clear();
        view.track(track(1, ChunkPos::new(0, 0), 5), &area, 1, false, &mut out);
        assert!(out.is_empty());
        view.untrack(2, 3, 0, &mut out);
        view.track(track(2, ChunkPos::new(0, 0), 2), &area, 1, false, &mut out);
        assert!(out.is_empty());
        view.track(track(1, ChunkPos::new(0, 0), 7), &area, 1, false, &mut out);
        assert_eq!(names(&out), [&b"spawn"[..], b"update"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn refresh_follows_the_viewer() {
        let mut subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(2).into_iter().collect();
        let mut view = EntityView::default();
        let mut out = Vec::new();
        view.track(
            track(1, ChunkPos::new(3, 0), 1),
            &area(&subscribed),
            0,
            false,
            &mut out,
        );
        assert!(out.is_empty());
        let moved = ViewArea {
            center: ChunkPos::new(1, 0),
            view_distance: 2,
            subscribed: &subscribed,
            spectator: false,
            position: [24.0, 64.0, 8.0],
            range_percent: 100,
        };
        view.refresh(&moved, 0, &mut out);
        assert_eq!(names(&out), [&b"spawn"[..], b"update"]);
        out.clear();
        subscribed.clear();
        view.refresh(&area(&subscribed), 0, &mut out);
        assert_eq!(names(&out), [&b"remove"[..]]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn spectators_are_seen_only_by_spectators() {
        let subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(2).into_iter().collect();
        let mut view = EntityView::default();
        let mut out = Vec::new();
        let spectating = |seq| Track {
            kind: tuff_vanilla_data::registry::EntityType::Player,
            spectator: true,
            ..track(1, ChunkPos::new(0, 0), seq)
        };
        view.track(spectating(1), &area(&subscribed), 0, false, &mut out);
        assert!(out.is_empty());
        assert!(!view.is_visible(1));

        let as_spectator = ViewArea {
            spectator: true,
            ..area(&subscribed)
        };
        view.refresh(&as_spectator, 0, &mut out);
        assert_eq!(names(&out), [&b"spawn"[..], b"update"]);
        out.clear();
        view.refresh(&area(&subscribed), 0, &mut out);
        assert_eq!(names(&out), [&b"remove"[..]]);
        out.clear();

        view.track(
            track(1, ChunkPos::new(0, 0), 2),
            &area(&subscribed),
            0,
            false,
            &mut out,
        );
        assert_eq!(names(&out), [&b"spawn"[..], b"update"]);
        out.clear();
        view.track(spectating(3), &area(&subscribed), 0, false, &mut out);
        assert_eq!(names(&out), [&b"remove"[..]]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn broadcast_percentage_scales_tracking_range() {
        assert_eq!(scaled_range(80, 100), 80);
        assert_eq!(scaled_range(80, 50), 40);
        assert_eq!(scaled_range(80, 10), 8);
        assert_eq!(scaled_range(85, 10), 8);
        assert_eq!(scaled_range(160, 500), 800);
    }

    fn zombie(entity: i32, x: f64, seq: u64) -> Track {
        Track {
            kind: tuff_vanilla_data::registry::EntityType::Zombie,
            position: [x, 64.0, 8.0],
            range: tuff_vanilla_data::registry::EntityType::Zombie
                .tracking()
                .range_chunks as i32
                * 16,
            ..track(entity, ChunkPos::of_position([x, 64.0, 8.0]), seq)
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn entities_beyond_their_tracking_range_send_nothing() {
        let subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(10).into_iter().collect();
        let near = ViewArea {
            view_distance: 10,
            ..area(&subscribed)
        };
        let mut view = EntityView::default();
        let mut out = Vec::new();
        view.track(zombie(1, 8.0 + 140.0, 1), &near, 0, false, &mut out);
        assert!(out.is_empty());
        assert!(!view.is_visible(1));
        let data = Arc::from(vec![Bytes::from_static(b"sdata")]);
        view.entity_packets(1, &data, &mut out);
        view.debug_value(1, 0, Bytes::from_static(b"sdebug"), &mut out);
        assert!(out.is_empty());
        view.track(zombie(1, 8.0 + 120.0, 2), &near, 0, false, &mut out);
        assert_eq!(names(&out), [&b"spawn"[..], b"update", b"sdebug"]);
        out.clear();
        view.track(zombie(1, 8.0 + 150.0, 3), &near, 0, false, &mut out);
        assert_eq!(names(&out), [&b"remove"[..]]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn tracking_range_is_capped_by_view_distance_and_scaled() {
        let subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(10).into_iter().collect();
        let short = ViewArea {
            view_distance: 3,
            ..area(&subscribed)
        };
        let mut view = EntityView::default();
        let mut out = Vec::new();
        view.track(zombie(1, 8.0 + 100.0, 1), &short, 0, false, &mut out);
        assert!(!view.is_visible(1));
        let halved = ViewArea {
            view_distance: 10,
            range_percent: 50,
            ..area(&subscribed)
        };
        view.refresh(&halved, 0, &mut out);
        assert!(!view.is_visible(1));
        let full = ViewArea {
            view_distance: 10,
            ..area(&subscribed)
        };
        view.refresh(&full, 0, &mut out);
        assert!(view.is_visible(1));
    }

    #[tuff_core::watchdog]
    #[test]
    fn range_is_rechecked_only_when_the_entity_changes_section() {
        let subscribed: HashSet<ChunkPos> = ChunkPos::new(0, 0).view(10).into_iter().collect();
        let near = ViewArea {
            view_distance: 10,
            ..area(&subscribed)
        };
        let mut view = EntityView::default();
        let mut out = Vec::new();
        view.track(zombie(1, 137.0, 1), &near, 0, false, &mut out);
        assert!(!view.is_visible(1));
        view.track(zombie(1, 135.0, 2), &near, 0, false, &mut out);
        assert!(!view.is_visible(1));
        view.track(zombie(1, 127.0, 3), &near, 0, false, &mut out);
        assert!(view.is_visible(1));
    }
}
