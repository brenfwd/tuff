//! Text, block and item displays: entities whose only job is to show
//! their saved data to clients. Everything they hold stays in their
//! saved form; the synced values clients render are read from it.

use tuff_vanilla_data::registry::EntityType;
use tuff_vanilla_data::{Block, BlockState};

use tuff_core::protocol::play::MetadataValue;
use tuff_core::world::entity::{EntityBase, RegionEntity, Tick, TickContext, number};
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};

const START_DELAY: u8 = 8;
const INTERPOLATION_DURATION: u8 = 9;
const TELEPORT_DURATION: u8 = 10;
const TRANSLATION: u8 = 11;
const SCALE: u8 = 12;
const LEFT_ROTATION: u8 = 13;
const RIGHT_ROTATION: u8 = 14;
const BILLBOARD: u8 = 15;
const BRIGHTNESS: u8 = 16;
const VIEW_RANGE: u8 = 17;
const SHADOW_RADIUS: u8 = 18;
const SHADOW_STRENGTH: u8 = 19;
const WIDTH: u8 = 20;
const HEIGHT: u8 = 21;
const GLOW_COLOR: u8 = 22;
const SUBTYPE: u8 = 23;

const INITIAL_BACKGROUND: i32 = 0x4000_0000;
const IDENTITY: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

#[derive(Debug)]
pub struct Display {
    base: EntityBase,
    values: Vec<(u8, MetadataValue)>,
}

fn float(nbt: &Compound, key: &str, default: f32) -> f32 {
    nbt.get(key).and_then(number).map_or(default, |v| v as f32)
}

fn int(nbt: &Compound, key: &str, default: i32) -> i32 {
    nbt.get(key).and_then(number).map_or(default, |v| v as i32)
}

fn floats<const N: usize>(tag: &Tag) -> Option<[f32; N]> {
    let Tag::List(list) = tag else { return None };
    if list.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (slot, tag) in out.iter_mut().zip(list) {
        *slot = number(tag)? as f32;
    }
    Some(out)
}

fn normalized(q: [f32; 4]) -> [f32; 4] {
    let length = q.iter().map(|v| v * v).sum::<f32>().sqrt();
    if length == 0.0 || !length.is_finite() {
        return IDENTITY;
    }
    q.map(|v| v / length)
}

/// A rotation as `[x, y, z, w]`: four components, or an axis and angle.
fn rotation(tag: &Tag) -> Option<[f32; 4]> {
    if let Some(q) = floats::<4>(tag) {
        return Some(normalized(q));
    }
    let nbt = tag.as_compound()?;
    let axis = floats::<3>(nbt.get("axis")?)?;
    let angle = number(nbt.get("angle")?)? as f32;
    let length = axis.iter().map(|v| v * v).sum::<f32>().sqrt();
    if length == 0.0 {
        return Some(IDENTITY);
    }
    let (sin, cos) = (angle / 2.0).sin_cos();
    let k = sin / length;
    Some([axis[0] * k, axis[1] * k, axis[2] * k, cos])
}

fn quaternion_of(m: [[f32; 3]; 3]) -> [f32; 4] {
    let trace = m[0][0] + m[1][1] + m[2][2];
    let q = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [
            (m[2][1] - m[1][2]) / s,
            (m[0][2] - m[2][0]) / s,
            (m[1][0] - m[0][1]) / s,
            s / 4.0,
        ]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let s = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt() * 2.0;
        [
            s / 4.0,
            (m[0][1] + m[1][0]) / s,
            (m[0][2] + m[2][0]) / s,
            (m[2][1] - m[1][2]) / s,
        ]
    } else if m[1][1] > m[2][2] {
        let s = (1.0 + m[1][1] - m[0][0] - m[2][2]).sqrt() * 2.0;
        [
            (m[0][1] + m[1][0]) / s,
            s / 4.0,
            (m[1][2] + m[2][1]) / s,
            (m[0][2] - m[2][0]) / s,
        ]
    } else {
        let s = (1.0 + m[2][2] - m[0][0] - m[1][1]).sqrt() * 2.0;
        [
            (m[0][2] + m[2][0]) / s,
            (m[1][2] + m[2][1]) / s,
            s / 4.0,
            (m[1][0] - m[0][1]) / s,
        ]
    };
    normalized(q)
}

/// Translation, left rotation, scale and right rotation of a row-major
/// affine matrix, assuming it is a rotation times a scale (no shear).
fn decompose(rows: [f32; 16]) -> ([f32; 3], [f32; 4], [f32; 3]) {
    let at = |row: usize, column: usize| rows[row * 4 + column];
    let translation = [at(0, 3), at(1, 3), at(2, 3)];
    let mut scale = [0.0f32; 3];
    for (column, slot) in scale.iter_mut().enumerate() {
        *slot = (0..3)
            .map(|row| at(row, column).powi(2))
            .sum::<f32>()
            .sqrt();
    }
    let determinant = at(0, 0) * (at(1, 1) * at(2, 2) - at(1, 2) * at(2, 1))
        - at(0, 1) * (at(1, 0) * at(2, 2) - at(1, 2) * at(2, 0))
        + at(0, 2) * (at(1, 0) * at(2, 1) - at(1, 1) * at(2, 0));
    if determinant < 0.0 {
        scale[0] = -scale[0];
    }
    let rotation: [[f32; 3]; 3] = std::array::from_fn(|row| {
        std::array::from_fn(|column| {
            if scale[column] == 0.0 {
                f32::from(u8::from(row == column))
            } else {
                at(row, column) / scale[column]
            }
        })
    });
    (translation, quaternion_of(rotation), scale)
}

/// The four transformation values, from either saved form.
fn transformation(tag: Option<&Tag>) -> ([f32; 3], [f32; 4], [f32; 3], [f32; 4]) {
    let identity = ([0.0; 3], IDENTITY, [1.0; 3], IDENTITY);
    let Some(tag) = tag else { return identity };
    if let Some(rows) = floats::<16>(tag) {
        let (translation, left, scale) = decompose(rows);
        return (translation, left, scale, IDENTITY);
    }
    let Some(nbt) = tag.as_compound() else {
        return identity;
    };
    let parts = (|| {
        Some((
            floats::<3>(nbt.get("translation")?)?,
            rotation(nbt.get("left_rotation")?)?,
            floats::<3>(nbt.get("scale")?)?,
            rotation(nbt.get("right_rotation")?)?,
        ))
    })();
    parts.unwrap_or(identity)
}

fn billboard(nbt: &Compound) -> i8 {
    match nbt.get_str("billboard") {
        Some("vertical") => 1,
        Some("horizontal") => 2,
        Some("center") => 3,
        _ => 0,
    }
}

fn brightness(nbt: &Compound) -> i32 {
    let Some(light) = nbt.get_compound("brightness") else {
        return -1;
    };
    let (Some(block), Some(sky)) = (
        light.get("block").and_then(number),
        light.get("sky").and_then(number),
    ) else {
        return -1;
    };
    if !(0.0..=15.0).contains(&block) || !(0.0..=15.0).contains(&sky) {
        return -1;
    }
    ((block as i32) << 4) | ((sky as i32) << 20)
}

fn block_state(tag: Option<&Tag>) -> BlockState {
    let (name, properties) = match tag {
        Some(Tag::Compound(entry)) => (
            entry.get_str("Name").unwrap_or(""),
            entry.get_compound("Properties"),
        ),
        _ => ("", None),
    };
    let Some(block) = Block::from_name(name) else {
        return Block::Air.default_state();
    };
    let mut state = block.default_state();
    for (key, value) in properties.into_iter().flat_map(Compound::iter) {
        if let Some(value) = value.as_str()
            && let Some(next) = state.with(key, value)
        {
            state = next;
        }
    }
    state
}

fn item_context(name: Option<&str>) -> i8 {
    match name {
        Some("thirdperson_lefthand") => 1,
        Some("thirdperson_righthand") => 2,
        Some("firstperson_lefthand") => 3,
        Some("firstperson_righthand") => 4,
        Some("head") => 5,
        Some("gui") => 6,
        Some("ground") => 7,
        Some("fixed") => 8,
        Some("on_shelf") => 9,
        _ => 0,
    }
}

fn flag(nbt: &Compound, key: &str, mask: i8) -> i8 {
    if nbt.get(key).and_then(number).is_some_and(|v| v != 0.0) {
        mask
    } else {
        0
    }
}

fn text_flags(nbt: &Compound) -> i8 {
    let align = match nbt.get_str("alignment") {
        Some("left") => 8,
        Some("right") => 16,
        _ => 0,
    };
    flag(nbt, "shadow", 1)
        | flag(nbt, "see_through", 2)
        | flag(nbt, "default_background", 4)
        | align
}

/// The synced values a display of `kind` shows for its saved data.
pub fn synced_values(kind: EntityType, nbt: &Compound) -> Vec<(u8, MetadataValue)> {
    let (translation, left, scale, right) = transformation(nbt.get("transformation"));
    let mut values = vec![
        (
            START_DELAY,
            MetadataValue::VarInt(int(nbt, "start_interpolation", 0)),
        ),
        (
            INTERPOLATION_DURATION,
            MetadataValue::VarInt(int(nbt, "interpolation_duration", 0)),
        ),
        (
            TELEPORT_DURATION,
            MetadataValue::VarInt(int(nbt, "teleport_duration", 0).clamp(0, 59)),
        ),
        (TRANSLATION, MetadataValue::Vector3(translation)),
        (SCALE, MetadataValue::Vector3(scale)),
        (LEFT_ROTATION, MetadataValue::Quaternion(left)),
        (RIGHT_ROTATION, MetadataValue::Quaternion(right)),
        (BILLBOARD, MetadataValue::Byte(billboard(nbt))),
        (BRIGHTNESS, MetadataValue::VarInt(brightness(nbt))),
        (
            VIEW_RANGE,
            MetadataValue::Float(float(nbt, "view_range", 1.0)),
        ),
        (
            SHADOW_RADIUS,
            MetadataValue::Float(float(nbt, "shadow_radius", 0.0)),
        ),
        (
            SHADOW_STRENGTH,
            MetadataValue::Float(float(nbt, "shadow_strength", 1.0)),
        ),
        (WIDTH, MetadataValue::Float(float(nbt, "width", 0.0))),
        (HEIGHT, MetadataValue::Float(float(nbt, "height", 0.0))),
        (
            GLOW_COLOR,
            MetadataValue::VarInt(int(nbt, "glow_color_override", -1)),
        ),
    ];
    match kind {
        EntityType::BlockDisplay => values.push((
            SUBTYPE,
            MetadataValue::BlockState(block_state(nbt.get("block_state")).id()),
        )),
        EntityType::ItemDisplay => {
            let stack = nbt.get_compound("item").and_then(ItemStack::from_nbt);
            values.push((SUBTYPE, MetadataValue::Slot(stack)));
            values.push((
                SUBTYPE + 1,
                MetadataValue::Byte(item_context(nbt.get_str("item_display"))),
            ));
        }
        EntityType::TextDisplay => {
            let text = nbt
                .get("text")
                .and_then(TextComponent::from_nbt)
                .unwrap_or_else(|| TextComponent::new(""));
            values.push((SUBTYPE, MetadataValue::Text(text)));
            values.push((
                SUBTYPE + 1,
                MetadataValue::VarInt(int(nbt, "line_width", 200)),
            ));
            values.push((
                SUBTYPE + 2,
                MetadataValue::VarInt(int(nbt, "background", INITIAL_BACKGROUND)),
            ));
            values.push((
                SUBTYPE + 3,
                MetadataValue::Byte(
                    nbt.get("text_opacity")
                        .and_then(number)
                        .map_or(-1, |v| v as i8),
                ),
            ));
            values.push((SUBTYPE + 4, MetadataValue::Byte(text_flags(nbt))));
        }
        _ => {}
    }
    values
}

pub fn load(mut base: EntityBase, nbt: &mut Compound) -> Option<Box<dyn RegionEntity>> {
    base.no_gravity = true;
    let values = synced_values(base.kind, nbt);
    Some(Box::new(Display { base, values }))
}

pub fn register(registry: &mut tuff_core::world::entity::Registry) {
    for kind in [
        EntityType::TextDisplay,
        EntityType::BlockDisplay,
        EntityType::ItemDisplay,
    ] {
        registry.register(kind, load);
    }
}

impl RegionEntity for Display {
    fn base(&self) -> &EntityBase {
        &self.base
    }

    fn base_mut(&mut self) -> &mut EntityBase {
        &mut self.base
    }

    fn save(&self, _out: &mut Compound) {}

    fn metadata(&self) -> Vec<(u8, MetadataValue)> {
        self.values.clone()
    }

    fn tick(&mut self, _cx: &mut TickContext) -> Tick {
        Tick::Alive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos() -> Tag {
        Tag::List([0.0f64; 3].map(Tag::Double).to_vec())
    }

    fn list(values: &[f32]) -> Tag {
        Tag::List(values.iter().map(|v| Tag::Float(*v)).collect())
    }

    #[tuff_core::watchdog]
    #[test]
    fn layouts_match_the_dumped_data() {
        let nbt = Compound::new();
        for kind in [
            EntityType::TextDisplay,
            EntityType::BlockDisplay,
            EntityType::ItemDisplay,
        ] {
            let layout = kind.data_layout();
            let values = synced_values(kind, &nbt);
            assert_eq!(values.len() + 8, layout.len(), "{kind:?}");
            for (index, value) in values {
                assert_eq!(
                    layout[usize::from(index)],
                    value.type_id() as u8,
                    "{kind:?} {index}"
                );
            }
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn saved_fields_become_synced_values() {
        let nbt = Compound::new()
            .with("billboard", "center")
            .with("view_range", 2.5f32)
            .with("glow_color_override", 255)
            .with("line_width", 90)
            .with("alignment", "right")
            .with("shadow", true)
            .with(
                "brightness",
                Compound::new().with("block", 3).with("sky", 15),
            )
            .with(
                "transformation",
                Compound::new()
                    .with("translation", list(&[1.0, 2.0, 3.0]))
                    .with("left_rotation", list(&[0.0, 0.0, 0.0, 2.0]))
                    .with("scale", list(&[2.0, 2.0, 2.0]))
                    .with(
                        "right_rotation",
                        Compound::new()
                            .with("axis", list(&[0.0, 1.0, 0.0]))
                            .with("angle", std::f32::consts::PI),
                    ),
            );
        let values = synced_values(EntityType::TextDisplay, &nbt);
        let get = |index: u8| values.iter().find(|(i, _)| *i == index).unwrap().1.clone();
        assert_eq!(get(BILLBOARD), MetadataValue::Byte(3));
        assert_eq!(get(VIEW_RANGE), MetadataValue::Float(2.5));
        assert_eq!(get(GLOW_COLOR), MetadataValue::VarInt(255));
        assert_eq!(
            get(BRIGHTNESS),
            MetadataValue::VarInt((3 << 4) | (15 << 20))
        );
        assert_eq!(get(TRANSLATION), MetadataValue::Vector3([1.0, 2.0, 3.0]));
        assert_eq!(get(SCALE), MetadataValue::Vector3([2.0; 3]));
        assert_eq!(get(LEFT_ROTATION), MetadataValue::Quaternion(IDENTITY));
        let MetadataValue::Quaternion(right) = get(RIGHT_ROTATION) else {
            panic!()
        };
        assert!((right[1] - 1.0).abs() < 1e-6 && right[3].abs() < 1e-6);
        assert_eq!(get(SUBTYPE + 1), MetadataValue::VarInt(90));
        assert_eq!(get(SUBTYPE + 3), MetadataValue::Byte(-1));
        assert_eq!(get(SUBTYPE + 4), MetadataValue::Byte(1 | 16));
        assert_eq!(get(SUBTYPE + 2), MetadataValue::VarInt(INITIAL_BACKGROUND));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_matrix_decomposes_into_translation_scale_and_rotation() {
        #[rustfmt::skip]
        let rows = [
            0.0, -3.0, 0.0, 5.0,
            3.0, 0.0, 0.0, 6.0,
            0.0, 0.0, 3.0, 7.0,
            0.0, 0.0, 0.0, 1.0,
        ];
        let nbt = Compound::new().with("transformation", list(&rows));
        let values = synced_values(EntityType::BlockDisplay, &nbt);
        let get = |index: u8| values.iter().find(|(i, _)| *i == index).unwrap().1.clone();
        assert_eq!(get(TRANSLATION), MetadataValue::Vector3([5.0, 6.0, 7.0]));
        let MetadataValue::Vector3(scale) = get(SCALE) else {
            panic!()
        };
        assert!(scale.iter().all(|v| (v - 3.0).abs() < 1e-6));
        let MetadataValue::Quaternion(q) = get(LEFT_ROTATION) else {
            panic!()
        };
        let half = std::f32::consts::FRAC_1_SQRT_2;
        assert!((q[2] - half).abs() < 1e-5 && (q[3] - half).abs() < 1e-5);
    }

    #[tuff_core::watchdog]
    #[test]
    fn block_and_item_subtypes_and_unknown_keys_survive_a_save() {
        let mut registry = tuff_core::world::entity::Registry::default();
        register(&mut registry);
        let nbt = Compound::new()
            .with("id", "minecraft:block_display")
            .with("Pos", pos())
            .with("billboard", "vertical")
            .with(
                "block_state",
                Compound::new().with("Name", "minecraft:stone"),
            );
        let loaded = registry.load(&nbt, 1).expect("loads");
        let values = loaded.metadata();
        let state = values.iter().find(|(i, _)| *i == SUBTYPE).unwrap();
        assert_eq!(
            state.1,
            MetadataValue::BlockState(Block::Stone.default_state().id())
        );
        let saved = tuff_core::world::entity::save(loaded.as_ref());
        assert_eq!(saved.get_str("billboard"), Some("vertical"));
        assert!(saved.get_compound("block_state").is_some());
    }
}
