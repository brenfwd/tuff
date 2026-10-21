//! Typed forms of the decoration and book components crafting reads and
//! writes: dyed colors, banner patterns, armor trims, fireworks and book
//! contents (https://minecraft.wiki/w/Data_component_format).

use tuff_vanilla_data::data_registry::{BANNER_PATTERN, TRIM_MATERIAL, TRIM_PATTERN};
use tuff_vanilla_data::item::{DyeColor, FireworkShape};

use crate::item::hash::Canon;
use crate::text::TextComponent;
use tuff_nbt::{Compound, Tag};

pub const MAX_BANNER_LAYERS: usize = 16;
pub const MAX_EXPLOSIONS: usize = 256;
pub const MAX_BOOK_PAGES: usize = 100;
pub const MAX_PAGE_CHARS: usize = 1024;
pub const MAX_TITLE_CHARS: usize = 32;

/// A value with the version chat filtering produced, if any.
#[derive(Debug, Clone, PartialEq)]
pub struct Filterable<T> {
    pub raw: T,
    pub filtered: Option<T>,
}

impl<T> Filterable<T> {
    pub fn plain(raw: T) -> Self {
        Self {
            raw,
            filtered: None,
        }
    }

    fn from_nbt(tag: &Tag, value: impl Fn(&Tag) -> Option<T>) -> Option<Self> {
        if let Some(map) = tag.as_compound()
            && let Some(raw) = map.get("raw")
        {
            let filtered = match map.get("filtered") {
                Some(filtered) => Some(value(filtered)?),
                None => None,
            };
            return Some(Self {
                raw: value(raw)?,
                filtered,
            });
        }
        Some(Self::plain(value(tag)?))
    }

    fn to_nbt(&self, value: impl Fn(&T) -> Tag) -> Tag {
        let mut map = Compound::new().with("raw", value(&self.raw));
        if let Some(filtered) = &self.filtered {
            map.insert("filtered", value(filtered));
        }
        Tag::Compound(map)
    }

    fn canon(&self, value: impl Fn(&T) -> Canon) -> Canon {
        let mut entries = vec![("raw".to_owned(), value(&self.raw))];
        if let Some(filtered) = &self.filtered {
            entries.push(("filtered".to_owned(), value(filtered)));
        }
        Canon::Map(entries)
    }
}

fn string_tag(tag: &Tag) -> Option<String> {
    tag.as_str().map(str::to_owned)
}

fn text_canon(tag: &Tag) -> Canon {
    TextComponent::from_nbt(tag).map_or_else(|| Canon::from_nbt(tag), |text| text.canon())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerLayer {
    /// A `minecraft:banner_pattern` entry name, or the asset id of the
    /// inline definition.
    pub pattern: String,
    /// The translation key of an inline pattern definition.
    pub inline: Option<String>,
    pub color: DyeColor,
}

impl BannerLayer {
    pub fn network_id(&self) -> Option<u32> {
        registry_index(BANNER_PATTERN.entries, &self.pattern)
    }

    pub fn is_sendable(&self) -> bool {
        self.inline.is_some() || self.network_id().is_some()
    }
}

fn registry_index(entries: &[&str], name: &str) -> Option<u32> {
    entries.binary_search(&name).ok().map(|index| index as u32)
}

pub fn banner_patterns_from_nbt(tag: &Tag) -> Option<Vec<BannerLayer>> {
    let layers = tag.as_list()?;
    if layers.len() > MAX_BANNER_LAYERS {
        return None;
    }
    layers
        .iter()
        .map(|layer| {
            let layer = layer.as_compound()?;
            let color = DyeColor::from_name(layer.get_str("color")?)?;
            match layer.get("pattern")? {
                Tag::Compound(definition) => Some(BannerLayer {
                    pattern: definition.get_str("asset_id")?.to_owned(),
                    inline: Some(definition.get_str("translation_key")?.to_owned()),
                    color,
                }),
                tag => Some(BannerLayer {
                    pattern: tag.as_str()?.to_owned(),
                    inline: None,
                    color,
                }),
            }
        })
        .collect()
}

pub fn banner_patterns_to_nbt(layers: &[BannerLayer]) -> Tag {
    Tag::List(
        layers
            .iter()
            .map(|layer| {
                let pattern = match &layer.inline {
                    Some(key) => Tag::Compound(
                        Compound::new()
                            .with("asset_id", layer.pattern.as_str())
                            .with("translation_key", key.as_str()),
                    ),
                    None => Tag::String(layer.pattern.clone()),
                };
                Tag::Compound(
                    Compound::new()
                        .with("pattern", pattern)
                        .with("color", layer.color.name()),
                )
            })
            .collect(),
    )
}

pub fn banner_patterns_canon(layers: &[BannerLayer]) -> Canon {
    Canon::List(
        layers
            .iter()
            .map(|layer| {
                let pattern = match &layer.inline {
                    Some(key) => Canon::Map(vec![
                        ("asset_id".to_owned(), Canon::str(layer.pattern.clone())),
                        ("translation_key".to_owned(), Canon::str(key.clone())),
                    ]),
                    None => Canon::str(layer.pattern.clone()),
                };
                Canon::Map(vec![
                    ("pattern".to_owned(), pattern),
                    ("color".to_owned(), Canon::str(layer.color.name())),
                ])
            })
            .collect(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineTrimMaterial {
    pub palette_id: String,
    pub description: TextComponent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineTrimPattern {
    pub asset_id: String,
    pub description: TextComponent,
    pub decal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmorTrim {
    /// A `minecraft:trim_material` entry name, empty for an inline one.
    pub material: String,
    /// A `minecraft:trim_pattern` entry name, empty for an inline one.
    pub pattern: String,
    pub inline_material: Option<InlineTrimMaterial>,
    pub inline_pattern: Option<InlineTrimPattern>,
}

impl ArmorTrim {
    pub fn named(material: impl Into<String>, pattern: impl Into<String>) -> Self {
        Self {
            material: material.into(),
            pattern: pattern.into(),
            inline_material: None,
            inline_pattern: None,
        }
    }

    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        let map = tag.as_compound()?;
        let mut trim = Self::named("", "");
        match map.get("material")? {
            Tag::Compound(definition) => {
                trim.inline_material = Some(InlineTrimMaterial {
                    palette_id: definition.get_str("palette_id")?.to_owned(),
                    description: TextComponent::from_nbt(definition.get("description")?)?,
                });
            }
            tag => trim.material = tag.as_str()?.to_owned(),
        }
        match map.get("pattern")? {
            Tag::Compound(definition) => {
                trim.inline_pattern = Some(InlineTrimPattern {
                    asset_id: definition.get_str("asset_id")?.to_owned(),
                    description: TextComponent::from_nbt(definition.get("description")?)?,
                    decal: definition.get_bool("decal").unwrap_or(false),
                });
            }
            tag => trim.pattern = tag.as_str()?.to_owned(),
        }
        Some(trim)
    }

    pub fn to_nbt(&self) -> Tag {
        let material = match &self.inline_material {
            Some(inline) => Tag::Compound(
                Compound::new()
                    .with("palette_id", inline.palette_id.as_str())
                    .with("description", inline.description.to_nbt()),
            ),
            None => Tag::String(self.material.clone()),
        };
        let pattern = match &self.inline_pattern {
            Some(inline) => Tag::Compound(
                Compound::new()
                    .with("asset_id", inline.asset_id.as_str())
                    .with("description", inline.description.to_nbt())
                    .with("decal", inline.decal),
            ),
            None => Tag::String(self.pattern.clone()),
        };
        Tag::Compound(
            Compound::new()
                .with("material", material)
                .with("pattern", pattern),
        )
    }

    pub fn canon(&self) -> Canon {
        let Tag::Compound(map) = self.to_nbt() else {
            unreachable!()
        };
        Canon::from_nbt(&Tag::Compound(map))
    }

    /// `(material, pattern)` registry indices, `None` for an inline half.
    pub fn network_ids(&self) -> (Option<u32>, Option<u32>) {
        (
            registry_index(TRIM_MATERIAL.entries, &self.material),
            registry_index(TRIM_PATTERN.entries, &self.pattern),
        )
    }

    pub fn is_sendable(&self) -> bool {
        let (material, pattern) = self.network_ids();
        (self.inline_material.is_some() || material.is_some())
            && (self.inline_pattern.is_some() || pattern.is_some())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FireworkExplosion {
    pub shape: FireworkShape,
    pub colors: Vec<i32>,
    pub fade_colors: Vec<i32>,
    pub has_trail: bool,
    pub has_twinkle: bool,
}

impl Default for FireworkExplosion {
    fn default() -> Self {
        Self {
            shape: FireworkShape::SmallBall,
            colors: Vec::new(),
            fade_colors: Vec::new(),
            has_trail: false,
            has_twinkle: false,
        }
    }
}

fn ints_from_nbt(tag: Option<&Tag>) -> Option<Vec<i32>> {
    match tag {
        None => Some(Vec::new()),
        Some(Tag::IntArray(values)) => Some(values.clone()),
        Some(Tag::List(values)) => values.iter().map(Tag::as_int).collect(),
        Some(_) => None,
    }
}

impl FireworkExplosion {
    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        let map = tag.as_compound()?;
        Some(Self {
            shape: FireworkShape::from_name(map.get_str("shape")?)?,
            colors: ints_from_nbt(map.get("colors"))?,
            fade_colors: ints_from_nbt(map.get("fade_colors"))?,
            has_trail: map.get_bool("has_trail").unwrap_or(false),
            has_twinkle: map.get_bool("has_twinkle").unwrap_or(false),
        })
    }

    pub fn to_nbt(&self) -> Tag {
        let mut map = Compound::new().with("shape", self.shape.name());
        if !self.colors.is_empty() {
            map.insert("colors", Tag::IntArray(self.colors.clone()));
        }
        if !self.fade_colors.is_empty() {
            map.insert("fade_colors", Tag::IntArray(self.fade_colors.clone()));
        }
        if self.has_trail {
            map.insert("has_trail", true);
        }
        if self.has_twinkle {
            map.insert("has_twinkle", true);
        }
        Tag::Compound(map)
    }

    pub fn canon(&self) -> Canon {
        let ints = |values: &[i32]| Canon::List(values.iter().map(|v| Canon::Int(*v)).collect());
        let mut entries = vec![("shape".to_owned(), Canon::str(self.shape.name()))];
        if !self.colors.is_empty() {
            entries.push(("colors".to_owned(), ints(&self.colors)));
        }
        if !self.fade_colors.is_empty() {
            entries.push(("fade_colors".to_owned(), ints(&self.fade_colors)));
        }
        if self.has_trail {
            entries.push(("has_trail".to_owned(), Canon::Bool(true)));
        }
        if self.has_twinkle {
            entries.push(("has_twinkle".to_owned(), Canon::Bool(true)));
        }
        Canon::Map(entries)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fireworks {
    pub flight_duration: u8,
    pub explosions: Vec<FireworkExplosion>,
}

impl Fireworks {
    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        let map = tag.as_compound()?;
        let flight_duration = match map.get("flight_duration") {
            None => 0,
            Some(Tag::Byte(v)) => *v as u8,
            Some(Tag::Int(v)) => u8::try_from(*v).ok()?,
            Some(Tag::Short(v)) => u8::try_from(*v).ok()?,
            Some(_) => return None,
        };
        let explosions = match map.get("explosions") {
            None => Vec::new(),
            Some(list) => {
                let list = list.as_list()?;
                if list.len() > MAX_EXPLOSIONS {
                    return None;
                }
                list.iter()
                    .map(FireworkExplosion::from_nbt)
                    .collect::<Option<Vec<_>>>()?
            }
        };
        Some(Self {
            flight_duration,
            explosions,
        })
    }

    pub fn to_nbt(&self) -> Tag {
        let mut map = Compound::new();
        if self.flight_duration != 0 {
            map.insert("flight_duration", self.flight_duration as i8);
        }
        if !self.explosions.is_empty() {
            map.insert(
                "explosions",
                Tag::List(
                    self.explosions
                        .iter()
                        .map(FireworkExplosion::to_nbt)
                        .collect(),
                ),
            );
        }
        Tag::Compound(map)
    }

    pub fn canon(&self) -> Canon {
        let mut entries = Vec::new();
        if self.flight_duration != 0 {
            entries.push((
                "flight_duration".to_owned(),
                Canon::Byte(self.flight_duration as i8),
            ));
        }
        if !self.explosions.is_empty() {
            entries.push((
                "explosions".to_owned(),
                Canon::List(
                    self.explosions
                        .iter()
                        .map(FireworkExplosion::canon)
                        .collect(),
                ),
            ));
        }
        Canon::Map(entries)
    }
}

pub fn writable_book_from_nbt(tag: &Tag) -> Option<Vec<Filterable<String>>> {
    let map = tag.as_compound()?;
    let Some(pages) = map.get("pages") else {
        return Some(Vec::new());
    };
    let pages = pages.as_list()?;
    if pages.len() > MAX_BOOK_PAGES {
        return None;
    }
    pages
        .iter()
        .map(|page| Filterable::from_nbt(page, string_tag))
        .collect()
}

pub fn writable_book_to_nbt(pages: &[Filterable<String>]) -> Tag {
    let mut map = Compound::new();
    if !pages.is_empty() {
        map.insert(
            "pages",
            Tag::List(
                pages
                    .iter()
                    .map(|page| page.to_nbt(|s| Tag::String(s.clone())))
                    .collect(),
            ),
        );
    }
    Tag::Compound(map)
}

pub fn writable_book_canon(pages: &[Filterable<String>]) -> Canon {
    let mut entries = Vec::new();
    if !pages.is_empty() {
        entries.push((
            "pages".to_owned(),
            Canon::List(
                pages
                    .iter()
                    .map(|page| page.canon(|s| Canon::str(s.clone())))
                    .collect(),
            ),
        ));
    }
    Canon::Map(entries)
}

/// A signed book's contents; pages are text components kept as NBT.
#[derive(Debug, Clone, PartialEq)]
pub struct WrittenBook {
    pub title: Filterable<String>,
    pub author: String,
    pub generation: i32,
    pub pages: Vec<Filterable<Tag>>,
    pub resolved: bool,
}

impl WrittenBook {
    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        let map = tag.as_compound()?;
        let pages = match map.get("pages") {
            None => Vec::new(),
            Some(pages) => {
                let pages = pages.as_list()?;
                if pages.len() > MAX_BOOK_PAGES {
                    return None;
                }
                pages
                    .iter()
                    .map(|page| Filterable::from_nbt(page, |t| Some(t.clone())))
                    .collect::<Option<Vec<_>>>()?
            }
        };
        Some(Self {
            title: Filterable::from_nbt(map.get("title")?, string_tag)?,
            author: map.get_str("author")?.to_owned(),
            generation: map.get_int("generation").unwrap_or(0),
            pages,
            resolved: map.get_bool("resolved").unwrap_or(false),
        })
    }

    pub fn to_nbt(&self) -> Tag {
        let mut map = Compound::new()
            .with("title", self.title.to_nbt(|s| Tag::String(s.clone())))
            .with("author", self.author.as_str());
        if self.generation != 0 {
            map.insert("generation", self.generation);
        }
        if !self.pages.is_empty() {
            map.insert(
                "pages",
                Tag::List(
                    self.pages
                        .iter()
                        .map(|page| page.to_nbt(Clone::clone))
                        .collect(),
                ),
            );
        }
        if self.resolved {
            map.insert("resolved", true);
        }
        Tag::Compound(map)
    }

    pub fn canon(&self) -> Canon {
        let mut entries = vec![
            (
                "title".to_owned(),
                self.title.canon(|s| Canon::str(s.clone())),
            ),
            ("author".to_owned(), Canon::str(self.author.clone())),
        ];
        if self.generation != 0 {
            entries.push(("generation".to_owned(), Canon::Int(self.generation)));
        }
        if !self.pages.is_empty() {
            entries.push((
                "pages".to_owned(),
                Canon::List(
                    self.pages
                        .iter()
                        .map(|page| page.canon(text_canon))
                        .collect(),
                ),
            ));
        }
        if self.resolved {
            entries.push(("resolved".to_owned(), Canon::Bool(true)));
        }
        Canon::Map(entries)
    }

    /// The copy crafting makes: one generation older.
    pub fn craft_copy(&self) -> Self {
        Self {
            generation: self.generation + 1,
            ..self.clone()
        }
    }
}

/// Reads a stored `dyed_color`: an RGB int, or the older `{rgb}` compound.
pub fn dyed_color_from_nbt(tag: &Tag) -> Option<i32> {
    match tag {
        Tag::Int(rgb) => Some(*rgb),
        Tag::Compound(map) => map.get_int("rgb"),
        _ => None,
    }
}

/// Mixes dyes into an item's current color the way the game does: channel
/// averages rescaled so the brightest channel keeps the average intensity.
pub fn apply_dyes(current: Option<i32>, dyes: &[DyeColor]) -> i32 {
    let mut red_total = 0;
    let mut green_total = 0;
    let mut blue_total = 0;
    let mut intensity_total = 0;
    let mut count = 0;
    let mut add = |rgb: i32| {
        let red = (rgb >> 16) & 0xFF;
        let green = (rgb >> 8) & 0xFF;
        let blue = rgb & 0xFF;
        intensity_total += red.max(green.max(blue));
        red_total += red;
        green_total += green;
        blue_total += blue;
        count += 1;
    };
    if let Some(current) = current {
        add(current);
    }
    for dye in dyes {
        add(dye.diffuse_color());
    }
    let red = red_total / count;
    let green = green_total / count;
    let blue = blue_total / count;
    let average_intensity = (intensity_total / count) as f32;
    let result_intensity = red.max(green.max(blue)) as f32;
    let scale = |channel: i32| ((channel as f32 * average_intensity) / result_intensity) as i32;
    (scale(red) << 16) | (scale(green) << 8) | scale(blue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn dye_mixing_matches_the_game_formula() {
        assert_eq!(apply_dyes(None, &[DyeColor::Red]), 0xB02E26);
        assert_eq!(apply_dyes(None, &[DyeColor::Red, DyeColor::Blue]), 0xAD5398);
        assert_eq!(apply_dyes(Some(0xB02E26), &[DyeColor::White]), 0xD79894);
        assert_eq!(
            apply_dyes(None, &[DyeColor::Yellow, DyeColor::Blue, DyeColor::Red]),
            0xC8866D
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn decoration_components_round_trip_through_nbt() {
        let layers = vec![BannerLayer {
            pattern: "minecraft:stripe_bottom".into(),
            inline: None,
            color: DyeColor::Lime,
        }];
        assert_eq!(
            banner_patterns_from_nbt(&banner_patterns_to_nbt(&layers)),
            Some(layers.clone())
        );
        assert!(layers[0].network_id().is_some());
        let trim = ArmorTrim::named("minecraft:netherite", "minecraft:coast");
        assert_eq!(ArmorTrim::from_nbt(&trim.to_nbt()), Some(trim.clone()));
        assert!(trim.is_sendable());
        let fireworks = Fireworks {
            flight_duration: 2,
            explosions: vec![FireworkExplosion {
                shape: FireworkShape::Star,
                colors: vec![1, 2],
                fade_colors: vec![],
                has_trail: true,
                has_twinkle: false,
            }],
        };
        assert_eq!(Fireworks::from_nbt(&fireworks.to_nbt()), Some(fireworks));
        let book = WrittenBook {
            title: Filterable::plain("Tale".into()),
            author: "Steve".into(),
            generation: 1,
            pages: vec![Filterable {
                raw: Tag::String("once".into()),
                filtered: Some(Tag::String("****".into())),
            }],
            resolved: false,
        };
        assert_eq!(WrittenBook::from_nbt(&book.to_nbt()), Some(book.clone()));
        assert_eq!(book.craft_copy().generation, 2);
        let pages = vec![Filterable::plain("draft".to_owned())];
        assert_eq!(
            writable_book_from_nbt(&writable_book_to_nbt(&pages)),
            Some(pages)
        );
        assert_eq!(dyed_color_from_nbt(&Tag::Int(7)), Some(7));
        assert_eq!(
            dyed_color_from_nbt(&Tag::Compound(Compound::new().with("rgb", 9))),
            Some(9)
        );
    }
}
