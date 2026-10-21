//! The icons drawn over a map: where players holding it are and the
//! banners it remembers. They are derived state, never saved.

use tuff_core::protocol::play::MapMarker;
use tuff_item::text::TextComponent;
use tuff_vanilla_data::item::DyeColor;
use tuff_vanilla_data::registry::MapDecorationType;

use crate::data::MapData;

pub fn banner_type(color: DyeColor) -> MapDecorationType {
    MapDecorationType::ALL[(MapDecorationType::BannerWhite.id() + color.id()) as usize]
}

/// Where a marker lies relative to a map, and which kinds may leave it.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    pub center_x: i32,
    pub center_z: i32,
    pub scale: u8,
    pub unlimited: bool,
    pub nether: bool,
}

impl Frame {
    pub fn of(data: &MapData) -> Self {
        Self {
            center_x: data.center_x,
            center_z: data.center_z,
            scale: data.scale,
            unlimited: data.unlimited_tracking,
            nether: data.dimension == "minecraft:the_nether",
        }
    }
}

fn inside(dx: f32, dz: f32) -> bool {
    (-63.0..=63.0).contains(&dx) && (-63.0..=63.0).contains(&dz)
}

fn clamp(delta: f32) -> i8 {
    if delta <= -63.0 {
        -128
    } else if delta >= 63.0 {
        127
    } else {
        ((f64::from(delta * 2.0)) + 0.5) as i32 as i8
    }
}

fn rotation(frame: &Frame, yaw: f64, game_time: Option<u64>) -> i8 {
    if let (true, Some(time)) = (frame.nether, game_time) {
        let s = (time / 10) as i32;
        let mixed = s
            .wrapping_mul(s)
            .wrapping_mul(34_187_121)
            .wrapping_add(s.wrapping_mul(121));
        return ((mixed >> 15) & 15) as i8;
    }
    let adjusted = if yaw < 0.0 { yaw - 8.0 } else { yaw + 8.0 };
    ((adjusted * 16.0) / 360.0) as i32 as i8
}

/// Where in the world a marker is placed and which way it faces.
#[derive(Debug, Clone, Copy)]
pub struct Spot {
    pub x: f64,
    pub z: f64,
    pub yaw: f64,
    /// The game tick, which spins markers on nether maps.
    pub game_time: Option<u64>,
}

impl Spot {
    pub fn new(x: f64, z: f64, yaw: f64, game_time: Option<u64>) -> Self {
        Self {
            x,
            z,
            yaw,
            game_time,
        }
    }
}

/// An ordered set of markers by key.
#[derive(Debug, Default)]
pub struct Markers {
    entries: Vec<(String, MapMarker)>,
    tracked: usize,
}

impl Markers {
    pub fn markers(&self) -> Vec<MapMarker> {
        self.entries
            .iter()
            .map(|(_, marker)| marker.clone())
            .collect()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }

    pub fn get(&self, key: &str) -> Option<&MapMarker> {
        self.entries
            .iter()
            .find_map(|(k, marker)| (k == key).then_some(marker))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether more than `limit` tracked markers are held.
    pub fn over_limit(&self, limit: usize) -> bool {
        self.tracked > limit
    }

    /// Whether `key` can be placed: it is held already or fewer than
    /// `limit` tracked markers are.
    pub fn has_room(&self, key: &str, limit: usize) -> bool {
        self.contains(key) || !self.over_limit(limit)
    }

    /// Removes a marker; whether there was one.
    pub fn remove(&mut self, key: &str) -> bool {
        let Some(at) = self.entries.iter().position(|(k, _)| k == key) else {
            return false;
        };
        let (_, marker) = self.entries.remove(at);
        if tracks(&marker) {
            self.tracked -= 1;
        }
        true
    }

    /// Places or moves a marker at a world position; whether anything
    /// visible changed. A position off the map removes it unless the kind
    /// may leave.
    pub fn place(
        &mut self,
        frame: &Frame,
        kind: MapDecorationType,
        key: &str,
        spot: Spot,
        name: Option<TextComponent>,
    ) -> bool {
        let Spot {
            x,
            z,
            yaw,
            game_time,
        } = spot;
        let scale = f64::from(1 << frame.scale);
        let dx = ((x - f64::from(frame.center_x)) as f32) / scale as f32;
        let dz = ((z - f64::from(frame.center_z)) as f32) / scale as f32;
        let (cx, cz) = (clamp(dx), clamp(dz));
        let located = if kind == MapDecorationType::Player {
            if inside(dx, dz) {
                Some(kind)
            } else if dx.abs() < 320.0 && dz.abs() < 320.0 {
                Some(MapDecorationType::PlayerOffMap)
            } else if frame.unlimited {
                Some(MapDecorationType::PlayerOffLimits)
            } else {
                None
            }
        } else if inside(dx, dz) || frame.unlimited {
            Some(kind)
        } else {
            None
        };
        let Some(kind) = located else {
            return self.remove(key);
        };
        let marker = MapMarker {
            kind: kind.id(),
            x: cx,
            y: cz,
            rotation: rotation(frame, yaw, game_time),
            name,
        };
        match self.entries.iter_mut().find(|(k, _)| k == key) {
            Some((_, existing)) if *existing == marker => false,
            Some((_, existing)) => {
                if tracks(existing) {
                    self.tracked -= 1;
                }
                if tracks(&marker) {
                    self.tracked += 1;
                }
                *existing = marker;
                true
            }
            None => {
                if tracks(&marker) {
                    self.tracked += 1;
                }
                self.entries.push((key.to_owned(), marker));
                true
            }
        }
    }
}

fn tracks(marker: &MapMarker) -> bool {
    MapDecorationType::from_id(marker.kind).is_some_and(|kind| kind.info().track_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> Frame {
        Frame {
            center_x: 0,
            center_z: 0,
            scale: 0,
            unlimited: false,
            nether: false,
        }
    }

    fn at(x: f64, z: f64, yaw: f64) -> Spot {
        Spot::new(x, z, yaw, None)
    }

    #[tuff_core::watchdog]
    #[test]
    fn players_inside_the_map_show_at_half_pixel_steps() {
        let mut markers = Markers::default();
        let player = MapDecorationType::Player;
        assert!(markers.place(&frame(), player, "A", at(10.0, -20.0, 90.0), None));
        let marker = markers.get("A").unwrap();
        assert_eq!(marker.kind, player.id());
        assert_eq!((marker.x, marker.y), (20, -39));
        assert_eq!(marker.rotation, 4);
        assert!(!markers.place(&frame(), player, "A", at(10.0, -20.0, 90.0), None));
    }

    #[tuff_core::watchdog]
    #[test]
    fn players_leaving_the_map_turn_into_edge_markers() {
        let mut markers = Markers::default();
        let player = MapDecorationType::Player;
        markers.place(&frame(), player, "A", at(100.0, 0.0, 0.0), None);
        let marker = markers.get("A").unwrap();
        assert_eq!(marker.kind, MapDecorationType::PlayerOffMap.id());
        assert_eq!(marker.x, 127);
        markers.place(&frame(), player, "A", at(1000.0, 0.0, 0.0), None);
        assert!(
            !markers.contains("A"),
            "past the limit without unlimited tracking"
        );
        let unlimited = Frame {
            unlimited: true,
            ..frame()
        };
        markers.place(&unlimited, player, "A", at(1000.0, 0.0, 0.0), None);
        assert_eq!(
            markers.get("A").unwrap().kind,
            MapDecorationType::PlayerOffLimits.id()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn banners_outside_the_map_are_dropped() {
        let mut markers = Markers::default();
        let kind = banner_type(DyeColor::Red);
        assert_eq!(kind, MapDecorationType::BannerRed);
        assert!(!markers.place(&frame(), kind, "b", at(200.0, 0.0, 180.0), None));
        assert!(markers.place(&frame(), kind, "b", at(5.5, 5.5, 180.0), None));
        assert_eq!(markers.get("b").unwrap().rotation, 8);
        assert!(markers.remove("b"));
        assert!(markers.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn nether_rotations_spin_with_the_clock() {
        let nether = Frame {
            nether: true,
            ..frame()
        };
        let first = rotation(&nether, 0.0, Some(0));
        let later = rotation(&nether, 0.0, Some(5000));
        assert!((0..16).contains(&first) && (0..16).contains(&later));
        assert_eq!(rotation(&nether, 0.0, None), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn tracked_markers_are_counted() {
        let mut markers = Markers::default();
        let white = banner_type(DyeColor::White);
        markers.place(&frame(), white, "a", at(1.0, 1.0, 0.0), None);
        markers.place(
            &frame(),
            MapDecorationType::Player,
            "p",
            at(1.0, 1.0, 0.0),
            None,
        );
        assert!(markers.over_limit(1));
        assert!(!markers.over_limit(2));
        markers.remove("a");
        assert!(!markers.over_limit(1));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_full_set_takes_no_new_keys_but_keeps_its_own() {
        let mut markers = Markers::default();
        let white = banner_type(DyeColor::White);
        for n in 0..3 {
            markers.place(&frame(), white, &format!("k{n}"), at(1.0, 1.0, 0.0), None);
        }
        assert!(markers.has_room("k1", 2));
        assert!(!markers.has_room("new", 2));
        assert!(markers.has_room("new", 3));
    }
}
