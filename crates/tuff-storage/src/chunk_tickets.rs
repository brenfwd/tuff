//! A dimension's persistent chunk tickets (`/forceload`), kept as the
//! dimension's `minecraft:chunk_tickets` saved data:
//! `{tickets: [{chunk_pos: [I; x, z], type: "minecraft:forced", level: 31}]}`.
//! Tickets of other types are kept verbatim so they survive a save.

use std::io;

use crate::backend::Documents;
use crate::document::Document;
use crate::document::{inner, saved};
use tuff_nbt::{Compound, Tag};
use tuff_world_model::pos::ChunkPos;

pub const CHUNK_TICKETS: &str = "minecraft:chunk_tickets";
const FORCED: &str = "minecraft:forced";
const FORCED_LEVEL: i32 = 31;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChunkTickets {
    pub forced: Vec<ChunkPos>,
    raw: Compound,
    others: Vec<Tag>,
}

fn is_forced(ticket: &Compound) -> bool {
    matches!(ticket.get_str("type"), Some(FORCED | "forced"))
}

impl ChunkTickets {
    pub fn load(docs: &dyn Documents, dimension: &str) -> io::Result<Self> {
        let mut raw = inner(docs.read(Document::DimensionData {
            dimension,
            id: CHUNK_TICKETS,
        })?);
        let mut forced = Vec::new();
        let mut others = Vec::new();
        if let Some(Tag::List(tickets)) = raw.remove("tickets") {
            for tag in tickets {
                let pos = match &tag {
                    Tag::Compound(ticket) if is_forced(ticket) => {
                        match ticket.get_int_array("chunk_pos") {
                            Some(&[x, z]) => Some(ChunkPos { x, z }),
                            _ => None,
                        }
                    }
                    _ => None,
                };
                match pos {
                    Some(pos) if !forced.contains(&pos) => forced.push(pos),
                    Some(_) => {}
                    None => others.push(tag),
                }
            }
        }
        Ok(Self {
            forced,
            raw,
            others,
        })
    }

    pub fn save(&self, docs: &dyn Documents, dimension: &str) -> io::Result<()> {
        let mut data = self.raw.clone();
        let mut tickets = self.others.clone();
        tickets.extend(self.forced.iter().map(|pos| {
            Tag::Compound(
                Compound::new()
                    .with("chunk_pos", Tag::IntArray(vec![pos.x, pos.z]))
                    .with("type", FORCED)
                    .with("level", FORCED_LEVEL),
            )
        }));
        data.insert("tickets", Tag::List(tickets));
        docs.write(
            Document::DimensionData {
                dimension,
                id: CHUNK_TICKETS,
            },
            saved(data),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::WorldBackend;
    use crate::memory::MemoryBackend;

    const DIM: &str = "minecraft:overworld";

    fn round_trip(backend: &dyn WorldBackend) {
        let docs = backend.documents();
        let empty = ChunkTickets::load(docs, DIM).unwrap();
        assert!(empty.forced.is_empty());
        let mut tickets = empty;
        tickets.forced = vec![ChunkPos { x: 3, z: -7 }, ChunkPos { x: -1, z: 0 }];
        tickets.save(docs, DIM).unwrap();
        let loaded = ChunkTickets::load(docs, DIM).unwrap();
        assert_eq!(loaded.forced, tickets.forced);
        assert!(
            ChunkTickets::load(docs, "minecraft:the_nether")
                .unwrap()
                .forced
                .is_empty()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn memory_backend_round_trips_forced_chunks() {
        round_trip(&MemoryBackend::default());
    }

    #[tuff_core::watchdog]
    #[test]
    fn vanilla_backend_round_trips_forced_chunks() {
        let dir = crate::test_dir::TestDir::new();
        let backend = crate::vanilla::VanillaBackend::new(dir.path().join("world"));
        round_trip(&backend);
        assert!(
            dir.path()
                .join("world/dimensions/minecraft/overworld/data/minecraft/chunk_tickets.dat")
                .exists()
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn sqlite_backend_round_trips_forced_chunks() {
        let dir = crate::test_dir::TestDir::new();
        let backend = crate::sqlite::SqliteBackend::open(dir.path().join("w.mcserver")).unwrap();
        round_trip(&backend);
    }

    #[tuff_core::watchdog]
    #[test]
    fn other_ticket_types_survive_a_save() {
        let backend = MemoryBackend::default();
        let docs = backend.documents();
        let portal = Compound::new()
            .with("chunk_pos", Tag::IntArray(vec![1, 2]))
            .with("type", "minecraft:portal")
            .with("level", 30)
            .with("ticks_left", 100i64);
        let forced = Compound::new()
            .with("chunk_pos", Tag::IntArray(vec![5, 6]))
            .with("type", FORCED)
            .with("level", FORCED_LEVEL);
        docs.write(
            Document::DimensionData {
                dimension: DIM,
                id: CHUNK_TICKETS,
            },
            saved(Compound::new().with(
                "tickets",
                Tag::List(vec![Tag::Compound(portal.clone()), Tag::Compound(forced)]),
            )),
        )
        .unwrap();
        let mut tickets = ChunkTickets::load(docs, DIM).unwrap();
        assert_eq!(tickets.forced, vec![ChunkPos { x: 5, z: 6 }]);
        tickets.forced.clear();
        tickets.save(docs, DIM).unwrap();
        let root = inner(
            docs.read(Document::DimensionData {
                dimension: DIM,
                id: CHUNK_TICKETS,
            })
            .unwrap(),
        );
        assert_eq!(
            root.get("tickets"),
            Some(&Tag::List(vec![Tag::Compound(portal)]))
        );
    }
}
