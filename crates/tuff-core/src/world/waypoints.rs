use std::collections::HashMap;

use tuff_vanilla_data::packet::play::Clientbound as Id;

use crate::protocol::Encode;
use crate::world::dimension::DimensionId;
use crate::world::messages::PlayerId;
use tuff_wire::PacketWriter;

/// Past this distance a waypoint is shown only by its direction.
const REALLY_FAR: f64 = 332.0;
const AZIMUTH_EPSILON: f32 = 0.008_726_646;
const DEFAULT_STYLE: &str = "minecraft:default";

/// How a waypoint is drawn: a style and a color, each defaulted when unset.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Icon {
    pub style: Option<String>,
    pub color: Option<[u8; 3]>,
}

/// What the locator bar needs to know about one player, as transmitter and
/// as receiver.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: PlayerId,
    pub uuid: u128,
    pub dimension: DimensionId,
    pub position: [f64; 3],
    pub spectator: bool,
    pub transmit_range: f64,
    pub receive_range: f64,
    pub view_distance: i32,
    pub icon: Icon,
    /// Everyone riding it, directly or through other mounts.
    pub indirect_passengers: Vec<PlayerId>,
}

impl Candidate {
    fn block(&self) -> [i32; 3] {
        self.position.map(|axis| axis.floor() as i32)
    }

    fn chunk(&self) -> [i32; 2] {
        let block = self.block();
        [block[0] >> 4, block[2] >> 4]
    }

    fn distance(&self, other: &Candidate) -> f64 {
        let d = [0, 1, 2].map(|i| self.position[i] - other.position[i]);
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    }

    fn azimuth_to(&self, receiver: &Candidate) -> f32 {
        let dx = receiver.position[0] - self.position[0];
        let dz = receiver.position[2] - self.position[2];
        (dx).atan2(-dz) as f32
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Operation {
    Track,
    Untrack,
    Update,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Contents {
    Empty,
    Position([i32; 3]),
    Chunk([i32; 2]),
    Azimuth(f32),
}

/// A locator bar entry added, moved or removed on one client.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackedWaypoint {
    pub operation: Operation,
    pub uuid: u128,
    pub icon: Icon,
    pub contents: Contents,
}

impl Encode for TrackedWaypoint {
    const ID: i32 = Id::Waypoint.id();

    fn encode(&self, w: &mut PacketWriter) {
        w.write_var_int(match self.operation {
            Operation::Track => 0,
            Operation::Untrack => 1,
            Operation::Update => 2,
        });
        w.write_bool(true);
        w.write_uuid(self.uuid);
        w.write_string(self.icon.style.as_deref().unwrap_or(DEFAULT_STYLE));
        match self.icon.color {
            Some(rgb) => {
                w.write_bool(true);
                w.write_bytes(&rgb);
            }
            None => w.write_bool(false),
        }
        match self.contents {
            Contents::Empty => w.write_var_int(0),
            Contents::Position(pos) => {
                w.write_var_int(1);
                pos.iter().for_each(|axis| w.write_var_int(*axis));
            }
            Contents::Chunk(chunk) => {
                w.write_var_int(2);
                chunk.iter().for_each(|axis| w.write_var_int(*axis));
            }
            Contents::Azimuth(angle) => {
                w.write_var_int(3);
                w.write_float(angle);
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Link {
    Block([i32; 3]),
    Chunk([i32; 2]),
    Azimuth(f32),
}

fn receiver_sees_chunk(receiver: &Candidate, chunk: [i32; 2]) -> bool {
    let at = receiver.chunk();
    (at[0] - chunk[0]).abs().max((at[1] - chunk[1]).abs()) <= receiver.view_distance
}

fn source_ignores(source: &Candidate, receiver: &Candidate) -> bool {
    if receiver.spectator {
        return false;
    }
    if source.spectator || source.indirect_passengers.contains(&receiver.id) {
        return true;
    }
    source.distance(receiver) >= source.transmit_range.min(receiver.receive_range)
}

fn manhattan(a: [i32; 3], b: [i32; 3]) -> i32 {
    (0..3).map(|i| (a[i] - b[i]).abs()).sum()
}

fn chessboard(a: [i32; 2], b: [i32; 2]) -> i32 {
    (a[0] - b[0]).abs().max((a[1] - b[1]).abs())
}

fn make(source: &Candidate, receiver: &Candidate) -> Option<Link> {
    if source_ignores(source, receiver) {
        return None;
    }
    if source.distance(receiver) > REALLY_FAR {
        return Some(Link::Azimuth(source.azimuth_to(receiver)));
    }
    if !receiver_sees_chunk(receiver, source.chunk()) {
        return Some(Link::Chunk(source.chunk()));
    }
    Some(Link::Block(source.block()))
}

impl Link {
    fn contents(&self) -> Contents {
        match *self {
            Link::Block(pos) => Contents::Position(pos),
            Link::Chunk(chunk) => Contents::Chunk(chunk),
            Link::Azimuth(angle) => Contents::Azimuth(angle),
        }
    }

    fn connect(&self, source: &Candidate, operation: Operation) -> TrackedWaypoint {
        TrackedWaypoint {
            operation,
            uuid: source.uuid,
            icon: source.icon.clone(),
            contents: self.contents(),
        }
    }

    fn is_broken(&self, source: &Candidate, receiver: &Candidate) -> bool {
        match *self {
            Link::Block(last) => {
                manhattan(last, source.block()) > 1 || source_ignores(source, receiver)
            }
            Link::Chunk(last) => {
                chessboard(last, source.chunk()) > 1
                    || source_ignores(source, receiver)
                    || receiver_sees_chunk(receiver, last)
            }
            Link::Azimuth(_) => {
                source_ignores(source, receiver)
                    || receiver_sees_chunk(receiver, source.chunk())
                    || source.distance(receiver) <= REALLY_FAR
            }
        }
    }

    fn update(&mut self, source: &Candidate, receiver: &Candidate) -> Option<Contents> {
        match self {
            Link::Block(last) => {
                let now = source.block();
                (manhattan(*last, now) > 0).then(|| {
                    *last = now;
                    Contents::Position(now)
                })
            }
            Link::Chunk(last) => {
                let now = source.chunk();
                (chessboard(*last, now) > 0).then(|| {
                    *last = now;
                    Contents::Chunk(now)
                })
            }
            Link::Azimuth(last) => {
                let now = source.azimuth_to(receiver);
                ((now - *last).abs() > AZIMUTH_EPSILON).then(|| {
                    *last = now;
                    Contents::Azimuth(now)
                })
            }
        }
    }
}

/// Who sees whom on the locator bar, as (receiver, transmitter) links.
#[derive(Default)]
pub struct WaypointManager {
    links: HashMap<(PlayerId, PlayerId), (u128, Link, Icon)>,
}

impl WaypointManager {
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// Brings every link up to date with `players` and returns the packets
    /// to send, each with the player it goes to. With `enabled` false every
    /// link is broken.
    pub fn step(
        &mut self,
        players: &[Candidate],
        enabled: bool,
    ) -> Vec<(PlayerId, TrackedWaypoint)> {
        let by_id: HashMap<PlayerId, &Candidate> = players.iter().map(|p| (p.id, p)).collect();
        let mut out = Vec::new();
        let keys: Vec<(PlayerId, PlayerId)> = self.links.keys().copied().collect();
        for key in keys {
            let (receiver_id, source_id) = key;
            let pair = by_id.get(&receiver_id).zip(by_id.get(&source_id));
            let usable = pair.filter(|(receiver, source)| {
                enabled
                    && receiver.dimension == source.dimension
                    && source.transmit_range > 0.0
                    && receiver.receive_range > 0.0
            });
            let Some((receiver, source)) = usable else {
                if let Some((uuid, ..)) = self.links.remove(&key)
                    && by_id.contains_key(&receiver_id)
                {
                    out.push(untrack(receiver_id, uuid));
                }
                continue;
            };
            let (_, link, icon) = self.links.get_mut(&key).expect("key came from the map");
            if !link.is_broken(source, receiver) {
                let moved = link.update(source, receiver);
                if moved.is_some() || *icon != source.icon {
                    *icon = source.icon.clone();
                    out.push((
                        receiver_id,
                        TrackedWaypoint {
                            operation: Operation::Update,
                            uuid: source.uuid,
                            icon: source.icon.clone(),
                            contents: link.contents(),
                        },
                    ));
                }
            } else if let Some(fresh) = make(source, receiver) {
                out.push((receiver_id, fresh.connect(source, Operation::Track)));
                *icon = source.icon.clone();
                *link = fresh;
            } else {
                self.links.remove(&key);
                out.push(untrack(receiver_id, source.uuid));
            }
        }
        if enabled {
            for receiver in players.iter().filter(|p| p.receive_range > 0.0) {
                for source in players.iter().filter(|p| p.transmit_range > 0.0) {
                    let key = (receiver.id, source.id);
                    if receiver.id == source.id
                        || receiver.dimension != source.dimension
                        || self.links.contains_key(&key)
                    {
                        continue;
                    }
                    if let Some(fresh) = make(source, receiver) {
                        out.push((receiver.id, fresh.connect(source, Operation::Track)));
                        self.links
                            .insert(key, (source.uuid, fresh, source.icon.clone()));
                    }
                }
            }
        }
        out
    }
}

fn untrack(receiver: PlayerId, uuid: u128) -> (PlayerId, TrackedWaypoint) {
    (
        receiver,
        TrackedWaypoint {
            operation: Operation::Untrack,
            uuid,
            icon: Icon::default(),
            contents: Contents::Empty,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(id: PlayerId, position: [f64; 3]) -> Candidate {
        Candidate {
            id,
            uuid: id as u128,
            dimension: DimensionId::default(),
            position,
            spectator: false,
            transmit_range: 60.0,
            receive_range: 60.0,
            view_distance: 10,
            icon: Icon::default(),
            indirect_passengers: Vec::new(),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_mount_is_not_shown_to_its_indirect_passengers() {
        let mut rider = player(1, [5.5, 65.0, 0.5]);
        rider.transmit_range = 0.0;
        let mut mount = player(500, [5.5, 64.0, 0.5]);
        mount.receive_range = 0.0;
        let shown = |mount: &Candidate| {
            WaypointManager::default()
                .step(&[rider.clone(), mount.clone()], true)
                .len()
        };
        assert_eq!(shown(&mount), 1);
        mount.indirect_passengers = vec![1];
        assert_eq!(shown(&mount), 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_transmit_only_entity_shows_to_players_and_sees_none() {
        let mut manager = WaypointManager::default();
        let mut mob = player(500, [5.5, 64.0, 0.5]);
        mob.receive_range = 0.0;
        let players = vec![player(1, [0.5, 64.0, 0.5]), mob];
        let packets = manager.step(&players, true);
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].0, 1);
        assert_eq!(packets[0].1.uuid, 500);
        assert_eq!(packets[0].1.operation, Operation::Track);
    }

    #[tuff_core::watchdog]
    #[test]
    fn nearby_players_track_each_other_and_updates_follow_block_moves() {
        let mut manager = WaypointManager::default();
        let mut players = vec![player(1, [0.5, 64.0, 0.5]), player(2, [10.5, 64.0, 0.5])];
        let first = manager.step(&players, true);
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|(_, p)| p.operation == Operation::Track));
        assert!(manager.step(&players, true).is_empty());
        players[1].position[0] = 11.5;
        let moved = manager.step(&players, true);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].0, 1);
        assert_eq!(moved[0].1.operation, Operation::Update);
        assert_eq!(moved[0].1.contents, Contents::Position([11, 64, 0]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn out_of_range_players_are_not_tracked_and_leaving_untracks() {
        let mut manager = WaypointManager::default();
        let mut players = vec![player(1, [0.5, 64.0, 0.5]), player(2, [100.5, 64.0, 0.5])];
        assert!(manager.step(&players, true).is_empty());
        players[1].position[0] = 20.5;
        assert_eq!(manager.step(&players, true).len(), 2);
        players.pop();
        let gone = manager.step(&players, true);
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].0, 1);
        assert_eq!(gone[0].1.operation, Operation::Untrack);
        assert_eq!(gone[0].1.uuid, 2);
        assert!(manager.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn disabling_the_rule_breaks_every_connection() {
        let mut manager = WaypointManager::default();
        let players = vec![player(1, [0.5, 64.0, 0.5]), player(2, [10.5, 64.0, 0.5])];
        manager.step(&players, true);
        let off = manager.step(&players, false);
        assert_eq!(off.len(), 2);
        assert!(off.iter().all(|(_, p)| p.operation == Operation::Untrack));
        assert!(manager.step(&players, false).is_empty());
        assert_eq!(manager.step(&players, true).len(), 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn spectators_are_hidden_but_still_see_others_and_zero_range_hides() {
        let mut manager = WaypointManager::default();
        let mut players = vec![player(1, [0.5, 64.0, 0.5]), player(2, [10.5, 64.0, 0.5])];
        players[1].spectator = true;
        let out = manager.step(&players, true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 2);
        players[0].transmit_range = 0.0;
        let out = manager.step(&players, true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.operation, Operation::Untrack);
    }

    #[tuff_core::watchdog]
    #[test]
    fn far_players_use_chunks_then_azimuth() {
        let mut manager = WaypointManager::default();
        let mut players = vec![player(1, [0.5, 64.0, 0.5]), player(2, [400.5, 64.0, 0.5])];
        players[0].transmit_range = 1000.0;
        players[0].receive_range = 1000.0;
        players[1].transmit_range = 1000.0;
        players[1].receive_range = 1000.0;
        let far = manager.step(&players, true);
        assert!(matches!(far[0].1.contents, Contents::Azimuth(_)));
        players[1].position[0] = 250.5;
        let mid = manager.step(&players, true);
        assert!(mid.iter().all(|(_, p)| p.operation == Operation::Track));
        assert!(matches!(mid[0].1.contents, Contents::Chunk(_)));
    }

    #[tuff_core::watchdog]
    #[test]
    fn an_icon_change_sends_an_update_with_the_current_position() {
        let mut manager = WaypointManager::default();
        let mut players = vec![player(1, [0.5, 64.0, 0.5]), player(2, [10.5, 64.0, 0.5])];
        manager.step(&players, true);
        players[1].icon.color = Some([1, 2, 3]);
        let out = manager.step(&players, true);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.operation, Operation::Update);
        assert_eq!(out[0].1.icon.color, Some([1, 2, 3]));
        assert_eq!(out[0].1.contents, Contents::Position([10, 64, 0]));
        assert!(manager.step(&players, true).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_crowd_links_every_pair_once_and_a_quiet_step_is_empty() {
        let mut manager = WaypointManager::default();
        let players: Vec<Candidate> = (0..200)
            .map(|i| player(i, [f64::from(i % 20) + 0.5, 64.0, f64::from(i / 20) + 0.5]))
            .collect();
        assert_eq!(manager.step(&players, true).len(), 200 * 199);
        assert!(manager.step(&players, true).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn encodes_a_position_track_packet() {
        let packet = TrackedWaypoint {
            operation: Operation::Track,
            uuid: 7,
            icon: Icon::default(),
            contents: Contents::Position([1, 2, 3]),
        };
        assert!(!crate::world::framed(&packet).is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_locator_bar_rule_defaults_on_and_players_transmit() {
        assert!(crate::world::game_rules::GameRules::default().bool("locator_bar"));
        let player = tuff_vanilla_data::registry::EntityType::Player;
        let base = |a| player.base_attribute(a);
        assert!(
            base(tuff_vanilla_data::registry::Attribute::WaypointTransmitRange)
                .is_some_and(|v| v > 0.0)
        );
        assert!(
            base(tuff_vanilla_data::registry::Attribute::WaypointReceiveRange)
                .is_some_and(|v| v > 0.0)
        );
    }
}
