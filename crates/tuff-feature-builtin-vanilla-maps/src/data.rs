//! A map's saved state: its place in the world, its pixels and the banner
//! and item frame markers it remembers, in the game's saved-data format.

use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_vanilla_data::item::DyeColor;
use tuff_world_model::BlockPos;

pub const SIZE: usize = 128;
pub const PIXELS: usize = SIZE * SIZE;
pub const MAX_SCALE: u8 = 4;

#[derive(Debug, Clone, PartialEq)]
pub struct MapBanner {
    pub pos: BlockPos,
    pub color: DyeColor,
    pub name: Option<TextComponent>,
}

impl MapBanner {
    pub fn key(&self) -> String {
        format!("banner-{},{},{}", self.pos.x, self.pos.y, self.pos.z)
    }

    fn to_nbt(&self) -> Compound {
        let mut nbt = Compound::new().with("pos", pos_tag(self.pos));
        if self.color != DyeColor::White {
            nbt.insert("color", self.color.name());
        }
        if let Some(name) = &self.name {
            nbt.insert("name", name.to_nbt());
        }
        nbt
    }

    fn from_nbt(nbt: &Compound) -> Option<Self> {
        Some(Self {
            pos: pos_of(nbt.get("pos")?)?,
            color: nbt
                .get_str("color")
                .and_then(DyeColor::from_name)
                .unwrap_or(DyeColor::White),
            name: nbt.get("name").and_then(TextComponent::from_nbt),
        })
    }
}

/// An item frame holding the map, as saved.
#[derive(Debug, Clone, PartialEq)]
pub struct MapFrame {
    pub pos: BlockPos,
    pub rotation: i32,
    pub entity_id: i32,
}

impl MapFrame {
    fn to_nbt(&self) -> Compound {
        Compound::new()
            .with("pos", pos_tag(self.pos))
            .with("rotation", self.rotation)
            .with("entity_id", self.entity_id)
    }

    fn from_nbt(nbt: &Compound) -> Option<Self> {
        Some(Self {
            pos: pos_of(nbt.get("pos")?)?,
            rotation: nbt.get_int("rotation")?,
            entity_id: nbt.get_int("entity_id")?,
        })
    }
}

fn pos_tag(pos: BlockPos) -> Tag {
    Tag::IntArray(vec![pos.x, pos.y, pos.z])
}

fn pos_of(tag: &Tag) -> Option<BlockPos> {
    match tag.as_int_array()? {
        [x, y, z] => Some(BlockPos::new(*x, *y, *z)),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapData {
    pub dimension: String,
    pub center_x: i32,
    pub center_z: i32,
    pub scale: u8,
    pub tracking_position: bool,
    pub unlimited_tracking: bool,
    pub locked: bool,
    pub colors: Vec<u8>,
    pub banners: Vec<MapBanner>,
    pub frames: Vec<MapFrame>,
}

impl MapData {
    /// A blank map of `scale` covering the grid cell around the origin,
    /// the way a new map is centered.
    pub fn fresh(
        origin_x: f64,
        origin_z: f64,
        scale: u8,
        tracking_position: bool,
        unlimited_tracking: bool,
        dimension: &str,
    ) -> Self {
        let size = (SIZE as i32) * (1 << scale);
        let area_x = ((origin_x + 64.0) / f64::from(size)).floor() as i32;
        let area_z = ((origin_z + 64.0) / f64::from(size)).floor() as i32;
        Self {
            dimension: dimension.to_owned(),
            center_x: area_x * size + size / 2 - 64,
            center_z: area_z * size + size / 2 - 64,
            scale,
            tracking_position,
            unlimited_tracking,
            locked: false,
            colors: vec![0; PIXELS],
            banners: Vec::new(),
            frames: Vec::new(),
        }
    }

    /// A locked copy: the same pixels and banners, never redrawn.
    pub fn locked_copy(&self) -> Self {
        Self {
            locked: true,
            frames: Vec::new(),
            ..self.clone()
        }
    }

    /// A blank map one scale larger around the same center.
    pub fn zoomed_out(&self) -> Self {
        Self::fresh(
            f64::from(self.center_x),
            f64::from(self.center_z),
            (self.scale + 1).min(MAX_SCALE),
            self.tracking_position,
            self.unlimited_tracking,
            &self.dimension,
        )
    }

    pub fn to_nbt(&self) -> Compound {
        let mut nbt = Compound::new()
            .with("dimension", self.dimension.as_str())
            .with("xCenter", self.center_x)
            .with("zCenter", self.center_z)
            .with(
                "colors",
                Tag::ByteArray(self.colors.iter().map(|&c| c as i8).collect()),
            );
        if self.scale != 0 {
            nbt.insert("scale", self.scale as i8);
        }
        if !self.tracking_position {
            nbt.insert("trackingPosition", false);
        }
        if self.unlimited_tracking {
            nbt.insert("unlimitedTracking", true);
        }
        if self.locked {
            nbt.insert("locked", true);
        }
        if !self.banners.is_empty() {
            nbt.insert(
                "banners",
                Tag::List(
                    self.banners
                        .iter()
                        .map(|banner| Tag::Compound(banner.to_nbt()))
                        .collect(),
                ),
            );
        }
        if !self.frames.is_empty() {
            nbt.insert(
                "frames",
                Tag::List(
                    self.frames
                        .iter()
                        .map(|frame| Tag::Compound(frame.to_nbt()))
                        .collect(),
                ),
            );
        }
        nbt
    }

    pub fn from_nbt(nbt: &Compound) -> Option<Self> {
        let colors = match nbt.get_byte_array("colors") {
            Some(bytes) if bytes.len() == PIXELS => bytes.iter().map(|&b| b as u8).collect(),
            _ => vec![0; PIXELS],
        };
        let list = |key: &str| nbt.get_list(key).unwrap_or(&[]);
        Some(Self {
            dimension: nbt.get_str("dimension")?.to_owned(),
            center_x: nbt.get_int("xCenter")?,
            center_z: nbt.get_int("zCenter")?,
            scale: nbt
                .get_byte("scale")
                .map_or(0, |scale| scale.clamp(0, MAX_SCALE as i8) as u8),
            tracking_position: nbt.get_bool("trackingPosition").unwrap_or(true),
            unlimited_tracking: nbt.get_bool("unlimitedTracking").unwrap_or(false),
            locked: nbt.get_bool("locked").unwrap_or(false),
            colors,
            banners: list("banners")
                .iter()
                .filter_map(|tag| MapBanner::from_nbt(tag.as_compound()?))
                .collect(),
            frames: list("frames")
                .iter()
                .filter_map(|tag| MapFrame::from_nbt(tag.as_compound()?))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn new_maps_center_on_their_grid_cell() {
        let map = MapData::fresh(0.0, 0.0, 0, true, false, "minecraft:overworld");
        assert_eq!((map.center_x, map.center_z), (0, 0));
        let map = MapData::fresh(-65.0, 64.0, 0, true, false, "minecraft:overworld");
        assert_eq!((map.center_x, map.center_z), (-128, 128));
        let map = MapData::fresh(10.0, 10.0, 2, true, false, "minecraft:overworld");
        assert_eq!((map.center_x, map.center_z), (192, 192));
    }

    #[tuff_core::watchdog]
    #[test]
    fn saved_form_round_trips() {
        let mut map = MapData::fresh(300.0, -40.0, 1, true, true, "minecraft:the_nether");
        map.colors[5] = 0x21;
        map.colors[PIXELS - 1] = 0xFF;
        map.locked = true;
        map.banners.push(MapBanner {
            pos: BlockPos::new(1, 70, -2),
            color: DyeColor::Red,
            name: Some(TextComponent::new("Home")),
        });
        map.frames.push(MapFrame {
            pos: BlockPos::new(3, 64, 4),
            rotation: 90,
            entity_id: 17,
        });
        assert_eq!(MapData::from_nbt(&map.to_nbt()), Some(map));
    }

    #[tuff_core::watchdog]
    #[test]
    fn missing_fields_take_the_games_defaults() {
        let nbt = Compound::new()
            .with("dimension", "minecraft:overworld")
            .with("xCenter", 8)
            .with("zCenter", 9);
        let map = MapData::from_nbt(&nbt).unwrap();
        assert!(map.tracking_position && !map.unlimited_tracking && !map.locked);
        assert_eq!(map.scale, 0);
        assert_eq!(map.colors.len(), PIXELS);
        let nbt = nbt.with("scale", 9i8);
        assert_eq!(MapData::from_nbt(&nbt).unwrap().scale, MAX_SCALE);
        assert!(MapData::from_nbt(&Compound::new()).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn locking_and_zooming_derive_new_maps() {
        let mut map = MapData::fresh(0.0, 0.0, 0, true, false, "minecraft:overworld");
        map.colors[0] = 9;
        let locked = map.locked_copy();
        assert!(locked.locked);
        assert_eq!(locked.colors, map.colors);
        let zoomed = map.zoomed_out();
        assert_eq!(zoomed.scale, 1);
        assert_eq!(zoomed.colors, vec![0; PIXELS]);
        let mut top = map.clone();
        top.scale = MAX_SCALE;
        assert_eq!(top.zoomed_out().scale, MAX_SCALE);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_file_the_game_wrote_reads_and_writes_back_identically() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/vanilla-26.3/map.dat");
        let root = tuff_storage::files::read_nbt_file(&path)
            .unwrap()
            .expect("the fixture exists");
        let data = tuff_storage::document::inner(Some(root));
        let map = MapData::from_nbt(&data).expect("the game's map parses");
        assert_eq!(map.dimension, "minecraft:the_nether");
        assert_eq!((map.center_x, map.center_z, map.scale), (-192, 320, 2));
        assert!(map.locked && map.unlimited_tracking && map.tracking_position);
        assert_eq!(map.colors[1], 7);
        assert_eq!(map.frames.len(), 1);
        assert_eq!(map.banners.len(), 2);
        assert_eq!(map.banners[0].color, DyeColor::Red);
        assert_eq!(
            map.banners[0].name.as_ref().map(TextComponent::plain),
            Some("Home".to_owned())
        );
        assert_eq!(map.banners[1].color, DyeColor::White);
        assert_eq!(map.to_nbt(), data);
    }
}
