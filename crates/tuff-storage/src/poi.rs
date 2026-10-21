//! Point of interest chunks (`poi/r.<x>.<z>.mca`): `{DataVersion, Sections:
//! {"<section y>": {Valid, Records: [{pos: [x, y, z], type, free_tickets}]}}}`.

use std::collections::BTreeMap;

use tuff_vanilla_data::registry::PointOfInterestType;
use tuff_vanilla_data::version::DATA_VERSION;

use crate::error::ChunkNbtError;
use tuff_nbt::{Compound, Tag};
use tuff_world_model::BlockPos;
use tuff_world_model::poi::Record;
use tuff_world_model::pos::ChunkPos;

/// Older records are left for the chunk's blocks to rebuild.
const OLDEST_VERSION: i32 = 2860;

/// The saved records of the chunk at `expected`, or `None` when they are
/// too old to trust. Records of unknown types or other chunks are dropped.
pub fn decode(root: &Compound, expected: ChunkPos) -> Result<Option<Vec<Record>>, ChunkNbtError> {
    let version = root.get_int("DataVersion").unwrap_or(DATA_VERSION);
    if version > DATA_VERSION {
        return Err(ChunkNbtError::NewerVersion(version));
    }
    if version < OLDEST_VERSION {
        return Ok(None);
    }
    let mut records = Vec::new();
    let Some(sections) = root.get_compound("Sections") else {
        return Ok(Some(records));
    };
    for (_, section) in sections.iter() {
        let Some(list) = section.as_compound().and_then(|s| s.get_list("Records")) else {
            continue;
        };
        for record in list.iter().filter_map(Tag::as_compound) {
            let Some(&[x, y, z]) = record.get_int_array("pos") else {
                continue;
            };
            let pos = BlockPos::new(x, y, z);
            let Some(kind) = record
                .get_str("type")
                .and_then(PointOfInterestType::from_name)
            else {
                continue;
            };
            if ChunkPos::of_block(pos) != expected || records.iter().any(|r: &Record| r.pos == pos)
            {
                continue;
            }
            let free = record.get_int("free_tickets").unwrap_or(0).max(0) as u32;
            records.push(Record {
                pos,
                kind,
                free: free.min(kind.info().max_tickets),
            });
        }
    }
    Ok(Some(records))
}

/// `None` when the chunk has no points, which clears its record.
pub fn encode(records: &[Record]) -> Option<Compound> {
    if records.is_empty() {
        return None;
    }
    let mut by_section: BTreeMap<i32, Vec<&Record>> = BTreeMap::new();
    for record in records {
        by_section
            .entry(record.pos.y >> 4)
            .or_default()
            .push(record);
    }
    let mut sections = Compound::new();
    for (y, mut list) in by_section {
        list.sort_by_key(|r| (r.pos.y, r.pos.z, r.pos.x));
        let list: Vec<Compound> = list
            .into_iter()
            .map(|r| {
                Compound::new()
                    .with("pos", vec![r.pos.x, r.pos.y, r.pos.z])
                    .with("type", r.kind.name())
                    .with("free_tickets", r.free as i32)
            })
            .collect();
        sections.insert(
            y.to_string(),
            Compound::new().with("Valid", true).with("Records", list),
        );
    }
    Some(
        Compound::new()
            .with("Sections", sections)
            .with("DataVersion", DATA_VERSION),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn records_round_trip_in_the_vanilla_layout() {
        let records = vec![
            Record {
                free: 0,
                ..Record::new(BlockPos::new(17, 70, 33), PointOfInterestType::Home)
            },
            Record::new(BlockPos::new(18, -3, 34), PointOfInterestType::Meeting),
        ];
        let root = encode(&records).unwrap();
        let section = root
            .get_compound("Sections")
            .and_then(|s| s.get_compound("4"))
            .unwrap();
        assert_eq!(section.get_bool("Valid"), Some(true));
        let record = section.get_list("Records").unwrap()[0]
            .as_compound()
            .unwrap();
        assert_eq!(record.get_int_array("pos"), Some(&[17, 70, 33][..]));
        assert_eq!(record.get_str("type"), Some("minecraft:home"));
        assert_eq!(record.get_int("free_tickets"), Some(0));
        assert!(root.get_compound("Sections").unwrap().contains_key("-1"));
        let mut decoded = decode(&root, ChunkPos::new(1, 2)).unwrap().unwrap();
        decoded.sort_by_key(|r| r.pos.y);
        let mut expected = records.clone();
        expected.sort_by_key(|r| r.pos.y);
        assert_eq!(decoded, expected);
        assert_eq!(encode(&[]), None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn unknown_types_strays_and_old_or_newer_data_are_handled() {
        let record = |pos: Vec<i32>, kind: &str, free: i32| {
            Tag::Compound(
                Compound::new()
                    .with("pos", pos)
                    .with("type", kind)
                    .with("free_tickets", free),
            )
        };
        let root = |version: i32| {
            Compound::new().with("DataVersion", version).with(
                "Sections",
                Compound::new().with(
                    "4",
                    Compound::new().with(
                        "Records",
                        vec![
                            record(vec![1, 70, 1], "minecraft:unemployed", 1),
                            record(vec![99, 70, 1], "minecraft:home", 1),
                            record(vec![2, 70, 1], "minecraft:home", 7),
                            record(vec![3, 70], "minecraft:home", 1),
                        ],
                    ),
                ),
            )
        };
        let decoded = decode(&root(DATA_VERSION), ChunkPos::new(0, 0))
            .unwrap()
            .unwrap();
        assert_eq!(
            decoded,
            vec![Record::new(
                BlockPos::new(2, 70, 1),
                PointOfInterestType::Home
            )]
        );
        assert_eq!(decode(&root(2000), ChunkPos::new(0, 0)).unwrap(), None);
        assert!(decode(&root(DATA_VERSION + 1), ChunkPos::new(0, 0)).is_err());
    }
}
