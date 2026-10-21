//! Compact SNBT form of structure templates: the block palette becomes
//! strings like `minecraft:oak_stairs{facing:north,half:bottom}` and block
//! entries are sorted and refer to those strings.

use std::cmp::Ordering;

use crate::{Compound, NbtError, Result, Tag};

const PROPERTIES_RENAME_VERSION: i32 = 5006;

fn error(reason: &'static str) -> NbtError {
    NbtError::Snbt {
        position: 0,
        reason,
    }
}

fn state_keys(version: i32) -> (&'static str, &'static str) {
    if version >= PROPERTIES_RENAME_VERSION {
        ("id", "properties")
    } else {
        ("Name", "Properties")
    }
}

fn pack_state(state: &Tag, version: i32) -> Result<String> {
    let (id_key, properties_key) = state_keys(version);
    let state = state
        .as_compound()
        .ok_or(error("palette entry is not a compound"))?;
    let mut packed = state
        .get_str(id_key)
        .ok_or(error("palette entry has no block id"))?
        .to_owned();
    if let Some(properties) = state.get_compound(properties_key) {
        let mut pairs = Vec::with_capacity(properties.len());
        for (key, value) in properties.iter() {
            let value = value
                .as_str()
                .ok_or(error("block property is not a string"))?;
            pairs.push((key, value));
        }
        pairs.sort_unstable_by_key(|(key, _)| *key);
        let joined: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}:{v}")).collect();
        packed.push('{');
        packed.push_str(&joined.join(","));
        packed.push('}');
    }
    Ok(packed)
}

fn unpack_state(packed: &str, version: i32) -> Compound {
    let (id_key, properties_key) = state_keys(version);
    let mut properties = Compound::new();
    let name = match packed.find('{') {
        Some(open) => {
            if open + 2 <= packed.len()
                && let Some(close) = packed[open..].find('}')
            {
                for pair in packed[open + 1..open + close].split(',') {
                    if let Some((key, value)) = pair.split_once(':') {
                        properties.insert(key, value);
                    }
                }
            }
            &packed[..open]
        }
        None => packed,
    };
    let mut state = Compound::new().with(id_key, name);
    if !properties.is_empty() {
        state.insert(properties_key, properties);
    }
    state
}

fn number_at(list: Option<&[Tag]>, index: usize) -> f64 {
    list.and_then(|l| l.get(index))
        .and_then(Tag::as_double)
        .unwrap_or(0.0)
}

fn yxz_order(a: Option<&[Tag]>, b: Option<&[Tag]>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(_), Some(_)) => [1, 0, 2]
            .into_iter()
            .map(|i| number_at(a, i).total_cmp(&number_at(b, i)))
            .find(|o| o.is_ne())
            .unwrap_or(Ordering::Equal),
    }
}

fn sorted_by_pos(tags: &[Tag]) -> Vec<Compound> {
    let mut compounds: Vec<Compound> = tags
        .iter()
        .filter_map(|t| t.as_compound().cloned())
        .collect();
    compounds.sort_by(|a, b| yxz_order(a.get_list("pos"), b.get_list("pos")));
    compounds
}

/// Packs a structure template into its compact SNBT text.
pub fn structure_to_snbt(structure: &Compound) -> Result<String> {
    Ok(format!(
        "{:#}",
        Tag::Compound(pack_structure(structure.clone())?)
    ))
}

/// Parses compact SNBT text back into a structure template.
pub fn snbt_to_structure(snbt: &str) -> Result<Compound> {
    match Tag::from_snbt(snbt)? {
        Tag::Compound(template) => unpack_structure(template),
        _ => Err(error("expected a compound")),
    }
}

pub fn pack_structure(mut structure: Compound) -> Result<Compound> {
    let version = structure.get_int("DataVersion").unwrap_or(-1);
    let palettes = structure.get_list("palettes").map(<[Tag]>::to_vec);
    let palette: Vec<Tag> = match &palettes {
        Some(palettes) => palettes
            .first()
            .and_then(Tag::as_list)
            .map(<[Tag]>::to_vec)
            .unwrap_or_default(),
        None => structure
            .get_list("palette")
            .map(<[Tag]>::to_vec)
            .unwrap_or_default(),
    };
    let deflated: Vec<String> = palette
        .iter()
        .map(|state| pack_state(state, version))
        .collect::<Result<_>>()?;
    structure.insert(
        "palette",
        deflated
            .iter()
            .map(|s| Tag::from(s.as_str()))
            .collect::<Vec<_>>(),
    );
    if let Some(palettes) = palettes {
        let mut packed = Vec::with_capacity(palettes.len());
        for old in &palettes {
            let old = old.as_list().ok_or(error("palette is not a list"))?;
            let mut entry = Compound::new();
            for (i, key) in deflated.iter().enumerate() {
                let state = old.get(i).ok_or(error("palette shorter than the first"))?;
                entry.insert(key.as_str(), pack_state(state, version)?.as_str());
            }
            packed.push(entry);
        }
        structure.insert("palettes", packed);
    }
    if let Some(entities) = structure.get_list("entities") {
        let sorted = sorted_by_pos(entities);
        structure.insert("entities", sorted);
    }
    let mut blocks = sorted_by_pos(structure.get_list("blocks").unwrap_or_default());
    for block in &mut blocks {
        let index = block.get_int("state").unwrap_or(0);
        let name = usize::try_from(index)
            .ok()
            .and_then(|i| deflated.get(i))
            .ok_or(error("block state index outside the palette"))?;
        block.insert("state", name.as_str());
    }
    structure.insert("data", blocks);
    structure.remove("blocks");
    Ok(structure)
}

pub fn unpack_structure(mut template: Compound) -> Result<Compound> {
    let version = template.get_int("DataVersion").unwrap_or(-1);
    let packed: Vec<String> = template
        .get_list("palette")
        .unwrap_or_default()
        .iter()
        .filter_map(|t| t.as_str().map(str::to_owned))
        .collect();
    if let Some(palettes) = template.get_list("palettes").map(<[Tag]>::to_vec) {
        let mut unpacked = Vec::with_capacity(palettes.len());
        for palette in &palettes {
            let palette = palette
                .as_compound()
                .ok_or(error("palette is not a compound"))?;
            let mut states = Vec::with_capacity(packed.len());
            for key in &packed {
                let state = palette.get_str(key).ok_or(error("palette entry missing"))?;
                states.push(Tag::Compound(unpack_state(state, version)));
            }
            unpacked.push(Tag::List(states));
        }
        template.insert("palettes", unpacked);
        template.remove("palette");
    } else {
        let states: Vec<Tag> = packed
            .iter()
            .map(|s| Tag::Compound(unpack_state(s, version)))
            .collect();
        template.insert("palette", states);
    }
    if let Some(Tag::List(mut blocks)) = template.remove("data") {
        for block in &mut blocks {
            let block = block
                .as_compound_mut()
                .ok_or(error("block is not a compound"))?;
            let name = block
                .get_str("state")
                .ok_or(error("block has no state"))?
                .to_owned();
            let index = packed
                .iter()
                .position(|p| *p == name)
                .ok_or(error("block state missing from the palette"))?;
            block.insert("state", index as i32);
        }
        template.insert("blocks", Tag::List(blocks));
    }
    Ok(template)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str, facing: Option<&str>) -> Compound {
        let mut state = Compound::new().with("id", name);
        if let Some(facing) = facing {
            state.insert(
                "properties",
                Compound::new().with("facing", facing).with("half", "top"),
            );
        }
        state
    }

    fn block(x: i32, y: i32, z: i32, state: i32) -> Compound {
        Compound::new()
            .with(
                "pos",
                vec![x, y, z].into_iter().map(Tag::Int).collect::<Vec<_>>(),
            )
            .with("state", state)
    }

    #[tuff_core::watchdog]
    #[test]
    fn template_packs_into_sorted_named_blocks_and_unpacks_back() {
        let template = Compound::new()
            .with("DataVersion", 5006)
            .with(
                "palette",
                vec![
                    state("minecraft:stone", None),
                    state("minecraft:oak_stairs", Some("north")),
                ],
            )
            .with(
                "blocks",
                vec![block(1, 1, 0, 1), block(0, 0, 0, 0), block(1, 0, 0, 1)],
            );
        let packed = pack_structure(template.clone()).unwrap();
        assert_eq!(
            packed.get_list("palette").unwrap()[1].as_str(),
            Some("minecraft:oak_stairs{facing:north,half:top}")
        );
        let data = packed.get_list("data").unwrap();
        let order: Vec<_> = data
            .iter()
            .map(|b| {
                b.as_compound()
                    .unwrap()
                    .get_str("state")
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(
            order,
            [
                "minecraft:stone",
                "minecraft:oak_stairs{facing:north,half:top}",
                "minecraft:oak_stairs{facing:north,half:top}"
            ]
        );
        assert!(packed.get("blocks").is_none());
        let text = structure_to_snbt(&template).unwrap();
        let back = snbt_to_structure(&text).unwrap();
        let positions: Vec<_> = back
            .get_list("blocks")
            .unwrap()
            .iter()
            .map(|b| b.as_compound().unwrap().get_int("state").unwrap())
            .collect();
        assert_eq!(positions, [0, 1, 1]);
        assert_eq!(back.get_list("palette"), template.get_list("palette"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn legacy_versions_use_name_and_properties_keys() {
        let unpacked = unpack_state("minecraft:lever{powered:true}", 4000);
        assert_eq!(unpacked.get_str("Name"), Some("minecraft:lever"));
        assert_eq!(
            unpacked
                .get_compound("Properties")
                .unwrap()
                .get_str("powered"),
            Some("true")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn block_state_outside_palette_is_an_error() {
        let template = Compound::new()
            .with("DataVersion", 5006)
            .with("palette", vec![state("minecraft:stone", None)])
            .with("blocks", vec![block(0, 0, 0, 3)]);
        assert!(pack_structure(template).is_err());
    }
}
