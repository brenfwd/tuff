//! Display components a stack keeps as stored NBT but sends in their own
//! wire form: `item_model`, `tooltip_style`, `rarity`,
//! `enchantment_glint_override`, `custom_model_data`, `tooltip_display`
//! and `max_stack_size`.

use std::borrow::Cow;

use tuff_vanilla_data::registry::DataComponentType;

use crate::item::hash::Canon;
use tuff_nbt::{Compound, Tag};

/// `rarity` values by wire id.
pub const RARITIES: [&str; 4] = ["common", "uncommon", "rare", "epic"];
/// The most entries one `custom_model_data` list or `tooltip_display`
/// set may hold on the wire.
pub const MAX_DISPLAY_ENTRIES: usize = 256;

/// `namespace:path` with the default namespace filled in, or `None` for
/// something that is not an identifier.
pub fn full_identifier(id: &str) -> Option<Cow<'_, str>> {
    let (namespace, path) = match id.split_once(':') {
        Some((namespace, path)) => (namespace, path),
        None => ("minecraft", id),
    };
    let valid = !path.is_empty()
        && namespace
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
        && path
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/'));
    if !valid {
        None
    } else if id.contains(':') {
        Some(Cow::Borrowed(id))
    } else {
        Some(Cow::Owned(format!("minecraft:{id}")))
    }
}

/// The wire id of a stored `rarity`.
pub fn rarity_id(tag: &Tag) -> Option<u32> {
    let name = tag.as_str()?;
    RARITIES.iter().position(|r| *r == name).map(|i| i as u32)
}

fn number(tag: &Tag) -> Option<f32> {
    Some(match tag {
        Tag::Byte(v) => *v as f32,
        Tag::Short(v) => *v as f32,
        Tag::Int(v) => *v as f32,
        Tag::Long(v) => *v as f32,
        Tag::Float(v) => *v,
        Tag::Double(v) => *v as f32,
        _ => return None,
    })
}

fn channel(value: f32) -> i32 {
    ((value * 255.0).floor() as i32) & 0xFF
}

fn color(tag: &Tag) -> Option<i32> {
    match tag {
        Tag::Int(v) => Some(*v),
        Tag::List(parts) if parts.len() == 3 => {
            let [r, g, b] = [number(&parts[0])?, number(&parts[1])?, number(&parts[2])?];
            Some((0xFF << 24) | (channel(r) << 16) | (channel(g) << 8) | channel(b))
        }
        _ => None,
    }
}

/// `custom_model_data`: the values an item model definition selects on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelData {
    pub floats: Vec<f32>,
    pub flags: Vec<bool>,
    pub strings: Vec<String>,
    pub colors: Vec<i32>,
}

impl ModelData {
    pub fn is_empty(&self) -> bool {
        self.floats.is_empty()
            && self.flags.is_empty()
            && self.strings.is_empty()
            && self.colors.is_empty()
    }

    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        let nbt = tag.as_compound()?;
        let mut data = Self::default();
        if let Some(floats) = nbt.get("floats") {
            data.floats = floats
                .as_list()?
                .iter()
                .map(number)
                .collect::<Option<_>>()?;
        }
        if let Some(flags) = nbt.get("flags") {
            data.flags = match flags {
                Tag::ByteArray(bytes) => bytes.iter().map(|b| *b != 0).collect(),
                Tag::List(items) => items.iter().map(Tag::as_bool).collect::<Option<_>>()?,
                _ => return None,
            };
        }
        if let Some(strings) = nbt.get("strings") {
            data.strings = strings
                .as_list()?
                .iter()
                .map(|s| s.as_str().map(str::to_owned))
                .collect::<Option<_>>()?;
        }
        if let Some(colors) = nbt.get("colors") {
            data.colors = match colors {
                Tag::IntArray(values) => values.clone(),
                Tag::List(items) => items.iter().map(color).collect::<Option<_>>()?,
                _ => return None,
            };
        }
        let fits = [
            data.floats.len(),
            data.flags.len(),
            data.strings.len(),
            data.colors.len(),
        ]
        .iter()
        .all(|len| *len <= MAX_DISPLAY_ENTRIES);
        fits.then_some(data)
    }

    /// The stored form, empty lists left out.
    pub fn to_nbt(&self) -> Tag {
        let mut nbt = Compound::new();
        if !self.floats.is_empty() {
            nbt.insert(
                "floats",
                Tag::List(self.floats.iter().map(|f| Tag::Float(*f)).collect()),
            );
        }
        if !self.flags.is_empty() {
            nbt.insert(
                "flags",
                Tag::ByteArray(self.flags.iter().map(|f| *f as i8).collect()),
            );
        }
        if !self.strings.is_empty() {
            nbt.insert(
                "strings",
                Tag::List(
                    self.strings
                        .iter()
                        .map(|s| Tag::String(s.clone()))
                        .collect(),
                ),
            );
        }
        if !self.colors.is_empty() {
            nbt.insert("colors", Tag::IntArray(self.colors.clone()));
        }
        Tag::Compound(nbt)
    }

    pub fn canon(&self) -> Canon {
        let mut fields = Vec::new();
        if !self.floats.is_empty() {
            fields.push((
                "floats".to_owned(),
                Canon::List(self.floats.iter().map(|f| Canon::Float(*f)).collect()),
            ));
        }
        if !self.flags.is_empty() {
            fields.push((
                "flags".to_owned(),
                Canon::List(self.flags.iter().map(|f| Canon::Bool(*f)).collect()),
            ));
        }
        if !self.strings.is_empty() {
            fields.push((
                "strings".to_owned(),
                Canon::List(self.strings.iter().map(Canon::str).collect()),
            ));
        }
        if !self.colors.is_empty() {
            fields.push((
                "colors".to_owned(),
                Canon::List(self.colors.iter().map(|c| Canon::Int(*c)).collect()),
            ));
        }
        Canon::Map(fields)
    }
}

/// `tooltip_display`: hides the whole tooltip or the lines of some
/// components.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TooltipDisplay {
    pub hide_tooltip: bool,
    pub hidden: Vec<DataComponentType>,
}

impl TooltipDisplay {
    pub fn from_nbt(tag: &Tag) -> Option<Self> {
        let nbt = tag.as_compound()?;
        let hide_tooltip = match nbt.get("hide_tooltip") {
            Some(flag) => flag.as_bool()?,
            None => false,
        };
        let mut hidden = Vec::new();
        if let Some(list) = nbt.get("hidden_components") {
            for id in list.as_list()? {
                let kind = DataComponentType::from_name(&full_identifier(id.as_str()?)?)?;
                if !hidden.contains(&kind) {
                    hidden.push(kind);
                }
            }
        }
        (hidden.len() <= MAX_DISPLAY_ENTRIES).then_some(Self {
            hide_tooltip,
            hidden,
        })
    }

    pub fn to_nbt(&self) -> Tag {
        let mut nbt = Compound::new();
        if self.hide_tooltip {
            nbt.insert("hide_tooltip", true);
        }
        if !self.hidden.is_empty() {
            nbt.insert(
                "hidden_components",
                Tag::List(
                    self.hidden
                        .iter()
                        .map(|kind| Tag::String(kind.name().to_owned()))
                        .collect(),
                ),
            );
        }
        Tag::Compound(nbt)
    }

    pub fn canon(&self) -> Canon {
        let mut fields = Vec::new();
        if self.hide_tooltip {
            fields.push(("hide_tooltip".to_owned(), Canon::Bool(true)));
        }
        if !self.hidden.is_empty() {
            fields.push((
                "hidden_components".to_owned(),
                Canon::List(self.hidden.iter().map(|k| Canon::str(k.name())).collect()),
            ));
        }
        Canon::Map(fields)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn identifiers_get_the_default_namespace() {
        assert_eq!(full_identifier("ruby").as_deref(), Some("minecraft:ruby"));
        assert_eq!(
            full_identifier("ex:item/ruby").as_deref(),
            Some("ex:item/ruby")
        );
        assert_eq!(full_identifier("Ex:ruby"), None);
        assert_eq!(full_identifier("ex:"), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn model_data_reads_every_stored_form() {
        let tag = Tag::Compound(
            Compound::new()
                .with("floats", Tag::List(vec![Tag::Float(0.5), Tag::Int(2)]))
                .with("flags", Tag::List(vec![Tag::Byte(1), Tag::Byte(0)]))
                .with("strings", Tag::List(vec![Tag::String("red".into())]))
                .with(
                    "colors",
                    Tag::List(vec![
                        Tag::Int(0x123456),
                        Tag::List(vec![Tag::Float(1.0), Tag::Float(0.0), Tag::Float(0.5)]),
                    ]),
                ),
        );
        let data = ModelData::from_nbt(&tag).unwrap();
        assert_eq!(data.floats, [0.5, 2.0]);
        assert_eq!(data.flags, [true, false]);
        assert_eq!(data.strings, ["red"]);
        assert_eq!(data.colors, [0x123456, 0xFFFF_007F_u32 as i32]);
        assert_eq!(ModelData::from_nbt(&data.to_nbt()), Some(data));
        assert!(ModelData::from_nbt(&Tag::Int(1)).is_none());
        let bad = Tag::Compound(Compound::new().with("strings", Tag::List(vec![Tag::Int(1)])));
        assert!(ModelData::from_nbt(&bad).is_none());
    }

    #[tuff_core::watchdog]
    #[test]
    fn model_data_canon_leaves_out_empty_lists() {
        let data = ModelData {
            flags: vec![true],
            ..ModelData::default()
        };
        assert_eq!(
            data.canon(),
            Canon::Map(vec![(
                "flags".to_owned(),
                Canon::List(vec![Canon::Bool(true)])
            )])
        );
        assert_eq!(ModelData::default().canon(), Canon::Map(Vec::new()));
    }

    /// Expected values were produced by the game's own hashing serializer.
    #[tuff_core::watchdog]
    #[test]
    fn hashes_match_the_game() {
        type N<'a> = crate::item::NetworkComponent<'a>;
        let cases = [
            (
                N::Identifier(DataComponentType::ItemModel, Cow::Borrowed("ex:ruby")),
                670908661,
            ),
            (N::Rarity(3), -292715907),
            (N::GlintOverride(true), -1019818302),
            (N::MaxStackSize(16), 1769065625),
            (
                N::CustomModelData(ModelData {
                    floats: vec![0.25],
                    flags: vec![true, false],
                    strings: vec!["lit".into()],
                    colors: vec![0x00FF00],
                }),
                400031734,
            ),
            (
                N::CustomModelData(ModelData {
                    flags: vec![true],
                    ..ModelData::default()
                }),
                1169437062,
            ),
            (
                N::TooltipDisplay(TooltipDisplay {
                    hide_tooltip: true,
                    hidden: vec![DataComponentType::Lore],
                }),
                1370755610,
            ),
        ];
        for (component, expected) in cases {
            assert_eq!(component.hash(), expected, "{component:?}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn tooltip_display_round_trips() {
        let display = TooltipDisplay {
            hide_tooltip: false,
            hidden: vec![DataComponentType::Lore, DataComponentType::Enchantments],
        };
        assert_eq!(TooltipDisplay::from_nbt(&display.to_nbt()), Some(display));
        let short = Tag::Compound(Compound::new().with(
            "hidden_components",
            Tag::List(vec![Tag::String("lore".into()), Tag::String("lore".into())]),
        ));
        assert_eq!(
            TooltipDisplay::from_nbt(&short).unwrap().hidden,
            [DataComponentType::Lore]
        );
        let unknown = Tag::Compound(Compound::new().with(
            "hidden_components",
            Tag::List(vec![Tag::String("ex:nope".into())]),
        ));
        assert!(TooltipDisplay::from_nbt(&unknown).is_none());
    }
}
