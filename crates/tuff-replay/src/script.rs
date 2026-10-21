//! Turns a recording into what replay needs: the configuration packets to
//! greet viewers with and a timeline of play events.

use crate::timeline::{Timeline, TimelineBuilder};
use bytes::Bytes;
use std::path::{Path, PathBuf};
use tuff_wire::PacketReader;

use anyhow::Context;
use tuff_trace::format::{self, Recording};
use tuff_trace::{ConnState, Direction};
use tuff_vanilla_data::packet::{configuration, login, play};

/// The `--player` value that replays every connection's world packets.
pub const ALL_PLAYERS: &str = "*";

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A clientbound play packet (id and body).
    Packet(Bytes),
    /// The recorded player moved or turned (from its serverbound moves).
    Moved {
        position: Option<[f64; 3]>,
        rotation: Option<(f32, f32)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Timed {
    /// Nanoseconds since the first play packet.
    pub ns: i64,
    pub event: Event,
}

#[derive(Debug)]
pub struct Script {
    pub player: String,
    pub uuid: u128,
    /// Every player whose connection the recording holds (global traces).
    pub players: Vec<String>,
    /// Clientbound configuration packets of the first connection, minus
    /// the ones the replay server answers itself.
    pub config: Vec<Bytes>,
    pub events: Timeline,
}

impl Script {
    pub fn duration(&self) -> i64 {
        self.events.last_ns()
    }

    pub fn from_events(
        player: String,
        uuid: u128,
        config: Vec<Bytes>,
        events: Vec<Timed>,
    ) -> anyhow::Result<Self> {
        let mut timeline = TimelineBuilder::default();
        for event in events {
            timeline.push(event)?;
        }
        Ok(Self {
            players: vec![player.clone()],
            player,
            uuid,
            config,
            events: timeline.finish()?,
        })
    }

    pub fn from_recording(recording: &Recording) -> anyhow::Result<Self> {
        Self::from_recording_for(recording, None)
    }

    pub fn from_recording_for(recording: &Recording, player: Option<&str>) -> anyhow::Result<Self> {
        let mut builder = Builder::new(&recording.header, usize::MAX)?.following(player);
        builder.feed(&recording.records, 0)?;
        builder.finish()
    }
}

/// Builds a script from records fed in order, without holding them.
pub struct Builder {
    player: String,
    global: bool,
    union: bool,
    own_entity: Option<i32>,
    wanted: Option<String>,
    chosen: Option<String>,
    players: Vec<String>,
    selected: std::collections::HashMap<u64, bool>,
    current: u64,
    uuid: Option<u128>,
    config: Vec<Bytes>,
    config_done: bool,
    events: TimelineBuilder,
    start: Option<i64>,
    clock: i64,
    budget: usize,
}

impl Builder {
    pub fn new(header: &format::Header, max_raw: usize) -> anyhow::Result<Self> {
        anyhow::ensure!(
            header.protocol_version == tuff_core::protocol::PROTOCOL_VERSION,
            "the recording is for protocol {} ({}), this build speaks {} ({})",
            header.protocol_version,
            header.game_version,
            tuff_core::protocol::PROTOCOL_VERSION,
            tuff_core::protocol::VERSION_NAME,
        );
        Ok(Self {
            player: header.player.clone(),
            global: header.player == tuff_trace::GLOBAL,
            union: false,
            own_entity: None,
            wanted: None,
            chosen: None,
            players: Vec::new(),
            selected: std::collections::HashMap::new(),
            current: 0,
            uuid: None,
            config: Vec::new(),
            config_done: false,
            events: TimelineBuilder::default(),
            start: None,
            clock: i64::MIN,
            budget: max_raw,
        })
    }

    /// For a global trace, follow this player's connections (default: the
    /// first player to log in).
    ///
    /// [`ALL_PLAYERS`] follows the first player to log in and adds every
    /// other connection's world packets (chunks, blocks, entities), so a
    /// free camera sees the union of what all of them saw.
    pub fn following(mut self, player: Option<&str>) -> Self {
        self.union = player == Some(ALL_PLAYERS);
        self.wanted = player.filter(|_| !self.union).map(str::to_lowercase);
        self
    }

    fn is_selected(&self) -> bool {
        !self.global || self.selected.get(&self.current).copied().unwrap_or(false)
    }

    fn note_login(&mut self, body: Bytes) {
        let Ok(name) = PacketReader::new(body).read_string_with_max(64) else {
            return;
        };
        let key = name.to_lowercase();
        if !self.players.contains(&name) {
            self.players.push(name.clone());
        }
        let chosen = self
            .chosen
            .get_or_insert_with(|| self.wanted.clone().unwrap_or_else(|| key.clone()));
        let follow = *chosen == key;
        self.selected.insert(self.current, follow);
        if follow {
            self.player = name;
        }
    }

    pub fn has_config(&self) -> bool {
        self.config_done
    }

    /// Adds `records`, whose timestamps are shifted by `offset_ns`.
    pub fn feed(&mut self, records: &[format::Record], offset_ns: i64) -> anyhow::Result<()> {
        for record in records {
            if record.tag.is_event() {
                if record.payload.first() == Some(&format::EVENT_CONNECTION)
                    && let Some(conn) = format::get_varint(&record.payload[1..])
                {
                    self.current = conn;
                }
                continue;
            }
            if record.tag.is_redacted() {
                continue;
            }
            let Some(id) = record.packet_id() else {
                continue;
            };
            let state = record.tag.state();
            let clientbound = record.tag.direction() == Direction::Clientbound;
            if self.global && state == ConnState::Login && !clientbound {
                if id == login::Serverbound::Hello.id() {
                    self.note_login(record.body());
                }
                continue;
            }
            if !self.is_selected() {
                if self.union
                    && clientbound
                    && state == ConnState::Play
                    && self.is_world_packet(id, &record.body())
                {
                    self.push_play(record, Event::Packet(record.payload.clone()), offset_ns)?;
                }
                continue;
            }
            match (state, clientbound) {
                (ConnState::Login, true) if id == login::Clientbound::LoginFinished.id() => {
                    if self.uuid.is_none() {
                        self.uuid = PacketReader::new(record.body()).read_uuid().ok();
                    }
                }
                (ConnState::Configuration, true) if !self.config_done => {
                    if id == configuration::Clientbound::FinishConfiguration.id() {
                        self.config_done = true;
                    } else if keep_config(id) {
                        self.charge(record.payload.len())?;
                        self.config.push(record.payload.clone());
                    }
                }
                (ConnState::Play, _) => {
                    let event = if clientbound {
                        Event::Packet(record.payload.clone())
                    } else if let Some(moved) = parse_move(id, record.body()) {
                        moved
                    } else {
                        continue;
                    };
                    if clientbound
                        && id == play::Clientbound::Login.id()
                        && self.own_entity.is_none()
                        && let Some(bytes) = record.body().get(..4)
                    {
                        self.own_entity = Some(i32::from_be_bytes(bytes.try_into().unwrap()));
                    }
                    self.push_play(record, event, offset_ns)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn push_play(
        &mut self,
        record: &format::Record,
        event: Event,
        offset_ns: i64,
    ) -> anyhow::Result<()> {
        self.charge(record.payload.len())?;
        let ns = record.ns.saturating_add(offset_ns);
        let start = *self.start.get_or_insert(ns);
        self.clock = self.clock.max(ns - start);
        self.events.push(Timed {
            ns: self.clock,
            event,
        })?;
        Ok(())
    }

    /// True for packets other players' connections add to a union replay:
    /// world and entity state, never anything private to that player or
    /// about the followed player's own entity.
    fn is_world_packet(&self, id: i32, body: &Bytes) -> bool {
        use play::Clientbound as C;
        if [
            C::LevelChunkWithLight,
            C::LightUpdate,
            C::BlockUpdate,
            C::SectionBlocksUpdate,
            C::BlockEntityData,
            C::BlockDestruction,
            C::BlockEvent,
            C::LevelEvent,
            C::RemoveEntities,
        ]
        .iter()
        .any(|k| k.id() == id)
        {
            return true;
        }
        if ![
            C::AddEntity,
            C::MoveEntityPos,
            C::MoveEntityPosRot,
            C::MoveEntityRot,
            C::TeleportEntity,
            C::EntityPositionSync,
            C::RotateHead,
            C::SetEntityData,
            C::SetEquipment,
            C::Animate,
            C::SetEntityMotion,
            C::HurtAnimation,
        ]
        .iter()
        .any(|k| k.id() == id)
        {
            return false;
        }
        let Some(entity) = PacketReader::new(body.clone()).read_var_int().ok() else {
            return false;
        };
        Some(entity) != self.own_entity
    }

    fn charge(&mut self, bytes: usize) -> anyhow::Result<()> {
        self.budget = self
            .budget
            .checked_sub(bytes)
            .ok_or(format::FormatError::TooLarge(self.budget))?;
        Ok(())
    }

    pub fn finish(self) -> anyhow::Result<Script> {
        anyhow::ensure!(
            self.config_done,
            "the recording has no complete configuration phase"
        );
        anyhow::ensure!(!self.events.is_empty(), "the recording has no play packets");
        Ok(Script {
            players: if self.players.is_empty() {
                vec![self.player.clone()]
            } else {
                self.players
            },
            player: self.player,
            uuid: self.uuid.unwrap_or_default(),
            config: self.config,
            events: self.events.finish()?,
        })
    }
}

fn keep_config(id: i32) -> bool {
    use configuration::Clientbound as C;
    ![
        C::CustomPayload,
        C::Disconnect,
        C::KeepAlive,
        C::Ping,
        C::CookieRequest,
        C::StoreCookie,
        C::Transfer,
        C::ResetChat,
    ]
    .iter()
    .any(|k| k.id() == id)
}

fn parse_move(id: i32, body: Bytes) -> Option<Event> {
    use play::Serverbound as S;
    let mut r = PacketReader::new(body);
    let (position, rotation) = if id == S::MovePlayerPos.id() {
        (Some(read_position(&mut r)?), None)
    } else if id == S::MovePlayerPosRot.id() {
        let p = read_position(&mut r)?;
        (Some(p), Some((r.read_float().ok()?, r.read_float().ok()?)))
    } else if id == S::MovePlayerRot.id() {
        (None, Some((r.read_float().ok()?, r.read_float().ok()?)))
    } else {
        return None;
    };
    Some(Event::Moved { position, rotation })
}

fn read_position(r: &mut PacketReader) -> Option<[f64; 3]> {
    Some([
        r.read_double().ok()?,
        r.read_double().ok()?,
        r.read_double().ok()?,
    ])
}

/// The files of one trace: `<stem>.ext`, `<stem>-part2.ext`, ... in order.
/// `path` may name any part; earlier parts supply the configuration phase.
fn trace_parts(path: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return vec![path.to_owned()];
    };
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return vec![path.to_owned()];
    };
    let base = match stem.rsplit_once("-part") {
        Some((base, n)) if n.parse::<u32>().is_ok() => base,
        _ => stem,
    };
    let mut parts = Vec::new();
    for n in 1u32.. {
        let file = if n == 1 {
            format!("{base}.{ext}")
        } else {
            format!("{base}-part{n}.{ext}")
        };
        let candidate = dir.join(file);
        if candidate.is_file() {
            parts.push(candidate);
        } else if n > 1 {
            break;
        } else if candidate != path {
            parts.push(path.to_owned());
            return parts;
        }
        if n > 100_000 {
            break;
        }
    }
    if parts.is_empty() {
        parts.push(path.to_owned());
    }
    parts
}

/// Streams a recording and its rotated parts block by block, holding only
/// the script, bounded per the format's limits and `max_raw`.
pub fn load(path: &Path, max_raw: usize) -> anyhow::Result<Script> {
    load_for(path, max_raw, None)
}

/// Like [`load`], following one player's connections of a global trace.
pub fn load_for(path: &Path, max_raw: usize, player: Option<&str>) -> anyhow::Result<Script> {
    let parts = trace_parts(path);
    let mut builder: Option<Builder> = None;
    let mut first_unix = 0u64;
    for part in &parts {
        let file = std::fs::File::open(part)?;
        let mut reader = format::Reader::new(std::io::BufReader::new(file))?;
        let header = reader.header().clone();
        let builder = match &mut builder {
            Some(b) => b,
            None => {
                first_unix = header.start_unix_ns;
                builder.insert(Builder::new(&header, max_raw)?.following(player))
            }
        };
        let offset =
            (header.start_unix_ns as i128 - first_unix as i128).clamp(0, i64::MAX as i128) as i64;
        while let Some(block) = reader.next_block()? {
            builder.feed(&block, offset)?;
        }
        if reader.truncated() {
            log::warn!(
                "{} ends without an end marker; replaying what is there",
                part.display()
            );
        }
    }
    builder.context("no recording files")?.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_trace::format::{Header, Tag, Writer};

    fn write(path: &Path, start: u64, packets: &[(i64, ConnState, i32)]) {
        let header = Header::current("Tester", start);
        let mut out = Writer::new(std::fs::File::create(path).unwrap(), &header).unwrap();
        for (ns, state, id) in packets {
            let tag = Tag::packet(*state, Direction::Clientbound);
            out.push(*ns, tag, &[*id as u8]).unwrap();
        }
        out.finish().unwrap();
    }

    struct Dir(PathBuf);

    impl Dir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn rotated() -> Dir {
        let path = std::env::temp_dir().join(format!("tuff-replay-{}", rand::random::<u64>()));
        std::fs::create_dir_all(&path).unwrap();
        let dir = Dir(path);
        let finish = configuration::Clientbound::FinishConfiguration.id();
        let play = play::Clientbound::KeepAlive.id();
        let push = configuration::Clientbound::ResourcePackPush.id();
        write(
            &dir.path().join("t.tuffrec"),
            1_000,
            &[
                (0, ConnState::Configuration, push),
                (1, ConnState::Configuration, finish),
                (10, ConnState::Play, play),
            ],
        );
        write(
            &dir.path().join("t-part2.tuffrec"),
            1_000 + 500,
            &[(5, ConnState::Play, play)],
        );
        dir
    }

    #[tuff_core::watchdog]
    #[test]
    fn rotated_parts_replay_in_order_with_resource_packs() {
        let dir = rotated();
        for name in ["t.tuffrec", "t-part2.tuffrec"] {
            let script = load(&dir.path().join(name), usize::MAX).unwrap();
            assert_eq!(script.events.len(), 2);
            assert_eq!(script.events.last_ns(), 495);
            assert_eq!(script.config.len(), 1);
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_byte_budget_bounds_streaming() {
        let dir = rotated();
        assert!(load(&dir.path().join("t.tuffrec"), 2).is_err());
    }

    fn hello(name: &str) -> Vec<u8> {
        let mut out = vec![login::Serverbound::Hello.id() as u8, name.len() as u8];
        out.extend(name.as_bytes());
        out.extend([0u8; 16]);
        out
    }

    fn global_recording() -> Recording {
        use tuff_trace::format::{EVENT_CONNECTION, Header, Tag, Writer};
        let mut out = Writer::new(Vec::new(), &Header::current("*", 0)).unwrap();
        let finish = configuration::Clientbound::FinishConfiguration.id() as u8;
        let keep = play::Clientbound::KeepAlive.id() as u8;
        let chat = play::Clientbound::SystemChat.id() as u8;
        let sb = |s| Tag::packet(s, Direction::Serverbound);
        let cb = |s| Tag::packet(s, Direction::Clientbound);
        for (conn, name) in [(0u64, "Alex"), (1, "Steve")] {
            let mut event = vec![EVENT_CONNECTION];
            format::put_varint(&mut event, conn);
            out.push(conn as i64, Tag::event(), &event).unwrap();
            out.push(conn as i64, sb(ConnState::Login), &hello(name))
                .unwrap();
            out.push(conn as i64, cb(ConnState::Configuration), &[finish])
                .unwrap();
        }
        for tick in 0..3i64 {
            for (conn, id) in [(0u64, keep), (1, chat)] {
                let mut event = vec![EVENT_CONNECTION];
                format::put_varint(&mut event, conn);
                out.push(10 + tick, Tag::event(), &event).unwrap();
                out.push(10 + tick, cb(ConnState::Play), &[id]).unwrap();
            }
        }
        let bytes = out.finish().unwrap();
        format::read_all(&bytes[..], usize::MAX).unwrap()
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_global_trace_replays_each_players_own_stream() {
        let recording = global_recording();
        let ids = |script: &Script| -> Vec<u8> {
            script
                .events
                .iter()
                .filter_map(|t| match t.event {
                    Event::Packet(p) => Some(p[0]),
                    _ => None,
                })
                .collect()
        };
        let keep = play::Clientbound::KeepAlive.id() as u8;
        let chat = play::Clientbound::SystemChat.id() as u8;
        let first = Script::from_recording(&recording).unwrap();
        assert_eq!(first.player, "Alex");
        assert_eq!(first.players, vec!["Alex", "Steve"]);
        assert_eq!(ids(&first), vec![keep; 3]);
        let second = Script::from_recording_for(&recording, Some("steve")).unwrap();
        assert_eq!(second.player, "Steve");
        assert_eq!(ids(&second), vec![chat; 3]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_union_replay_adds_other_connections_world_packets_only() {
        let recording = global_recording_with_world();
        let ids = |script: &Script| -> Vec<u8> {
            script
                .events
                .iter()
                .filter_map(|t| match t.event {
                    Event::Packet(p) => Some(p[0]),
                    _ => None,
                })
                .collect()
        };
        let keep = play::Clientbound::KeepAlive.id() as u8;
        let chat = play::Clientbound::SystemChat.id() as u8;
        let block = play::Clientbound::BlockUpdate.id() as u8;
        let own = Script::from_recording(&recording).unwrap();
        assert_eq!(ids(&own), vec![keep; 3]);
        let all = Script::from_recording_for(&recording, Some(ALL_PLAYERS)).unwrap();
        assert_eq!(all.player, "Alex");
        let got = ids(&all);
        assert_eq!(got.iter().filter(|id| **id == block).count(), 3);
        assert_eq!(got.iter().filter(|id| **id == keep).count(), 3);
        assert!(!got.contains(&chat));
    }

    fn global_recording_with_world() -> Recording {
        use tuff_trace::format::{EVENT_CONNECTION, Header, Tag, Writer};
        let mut out = Writer::new(Vec::new(), &Header::current("*", 0)).unwrap();
        let finish = configuration::Clientbound::FinishConfiguration.id() as u8;
        let keep = play::Clientbound::KeepAlive.id() as u8;
        let chat = play::Clientbound::SystemChat.id() as u8;
        let block = play::Clientbound::BlockUpdate.id() as u8;
        let sb = |s| Tag::packet(s, Direction::Serverbound);
        let cb = |s| Tag::packet(s, Direction::Clientbound);
        for (conn, name) in [(0u64, "Alex"), (1, "Steve")] {
            let mut event = vec![EVENT_CONNECTION];
            format::put_varint(&mut event, conn);
            out.push(conn as i64, Tag::event(), &event).unwrap();
            out.push(conn as i64, sb(ConnState::Login), &hello(name))
                .unwrap();
            out.push(conn as i64, cb(ConnState::Configuration), &[finish])
                .unwrap();
        }
        for tick in 0..3i64 {
            for (conn, id) in [(0u64, keep), (1, chat), (1, block)] {
                let mut event = vec![EVENT_CONNECTION];
                format::put_varint(&mut event, conn);
                out.push(10 + tick, Tag::event(), &event).unwrap();
                out.push(10 + tick, cb(ConnState::Play), &[id]).unwrap();
            }
        }
        let bytes = out.finish().unwrap();
        format::read_all(&bytes[..], usize::MAX).unwrap()
    }
}
