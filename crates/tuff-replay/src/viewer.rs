//! One viewer's playback: a cursor into the shared script, a playhead and a
//! transformer. Pure; the connection task feeds it time and commands.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use crate::controls::teleport_partial;
use crate::playhead::Playhead;
use crate::reduce::skippable;
use crate::script::{Event, Script, Timed};
use crate::shared::{LeaderState, Shared};
use crate::transform::{Transformer, encode};
use tuff_core::protocol::play::{ContainerSetSlot, GameEvent, PlayerPosition, SystemChat};
use tuff_item::item::ItemStack;
use tuff_item::text::TextComponent;
use tuff_vanilla_data::Item;
use tuff_vanilla_data::packet::play::Clientbound as C;
use tuff_wire::PacketWriter;

const SECOND: f64 = 1_000_000_000.0;
const DRIFT_NS: i64 = 1_000_000_000;
const HOTBAR_FIRST_SLOT: i16 = 36;
const HOTBAR_SKIP_SECONDS: f64 = 10.0;
const HOTBAR_SPEEDS: [f64; 5] = [0.5, 1.0, 2.0, 4.0, 8.0];
const CHANGE_GAME_MODE: u8 = 3;
const CREATIVE: f32 = 1.0;
const SPECTATOR: f32 = 3.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Solo,
    Leader,
    Follower,
}

pub struct Viewer {
    script: Arc<Script>,
    cursor: usize,
    last_ns: Option<i64>,
    pub head: Playhead,
    pub transform: Transformer,
    camera: Option<i32>,
    placed: Option<([f64; 3], f32, f32)>,
    teleports: i32,
    hotbar: bool,
    held: i16,
    shared: Option<Shared>,
    role: Role,
    seeks: u64,
    seen_seeks: u64,
}

impl Viewer {
    pub fn new(script: Arc<Script>, viewer_id: i32) -> Self {
        let head = Playhead::new(script.duration());
        let transform = Transformer::new(viewer_id, script.uuid);
        Self {
            script,
            cursor: 0,
            last_ns: None,
            head,
            transform,
            camera: None,
            placed: None,
            teleports: 0,
            hotbar: false,
            held: 0,
            shared: None,
            role: Role::Solo,
            seeks: 0,
            seen_seeks: 0,
        }
    }

    /// Joins the server's shared playhead, which `/replay lead` and
    /// `/replay follow` use.
    pub fn attach(&mut self, shared: Shared) {
        self.shared = Some(shared);
    }

    pub fn role(&self) -> Role {
        self.role
    }

    pub fn hotbar(&self) -> bool {
        self.hotbar
    }

    /// Index of the next event to play.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Plays every due event. Playing goes a block at a time; a seek loads
    /// the whole gap for the duration of the call so it can skip what is
    /// gone again before the target.
    fn catch_up(&mut self, reduce: bool, out: &mut Vec<Bytes>) {
        let target = self.head.position();
        let mut range: Vec<Timed> = Vec::new();
        while self.cursor < self.script.events.len() {
            let mut block = self.script.events.read_from(self.cursor);
            let due = block.partition_point(|timed| timed.ns <= target);
            let more = due == block.len() && due > 0;
            block.truncate(due);
            self.cursor += due;
            if reduce {
                range.append(&mut block);
            } else {
                self.play(&block, false, out);
            }
            if !more {
                break;
            }
        }
        if reduce {
            self.play(&range, true, out);
        }
        self.transform.finish(out);
        if self.camera.is_some_and(|id| !self.transform.holds(id)) {
            self.release_camera(out);
        }
    }

    /// The entity the viewer is looking through, if any.
    pub fn camera(&self) -> Option<i32> {
        self.camera
    }

    /// Spectates `entity` first person, if the viewer holds it.
    pub fn spectate(&mut self, entity: i32, out: &mut Vec<Bytes>) {
        if self.camera != Some(entity) && self.transform.holds(entity) {
            self.camera = Some(entity);
            out.push(set_camera(entity));
        }
    }

    /// Back to the free camera; what sneaking does while spectating.
    pub fn release_camera(&mut self, out: &mut Vec<Bytes>) {
        if self.camera.take().is_some() {
            out.push(set_camera(self.transform.viewer_id));
        }
    }

    fn play(&mut self, events: &[Timed], reduce: bool, out: &mut Vec<Bytes>) {
        let (skip, collapse) = if reduce {
            let skips = skippable(events, &self.transform.chunks, &self.transform.entities);
            (skips.skip, skips.collapse)
        } else {
            (vec![false; events.len()], vec![false; events.len()])
        };
        for ((timed, skipped), collapsed) in events.iter().zip(skip).zip(collapse) {
            if skipped {
                continue;
            }
            match &timed.event {
                Event::Packet(payload) if collapsed => self.transform.collapse(payload),
                event => self.transform.apply(event, out),
            }
        }
        self.transform.flush_collapsed(out);
        if let Some(last) = events.last() {
            self.last_ns = Some(last.ns);
        }
    }

    /// Plays everything due after `real` elapsed time.
    pub fn step(&mut self, real: Duration, out: &mut Vec<Bytes>) {
        self.head.advance(real);
        self.catch_up(false, out);
    }

    /// One playback tick: a leader publishes after it, a follower copies the
    /// leader first.
    pub fn tick(&mut self, real: Duration, out: &mut Vec<Bytes>) {
        if self.role == Role::Follower {
            let state = self.shared.as_ref().map(|s| s.borrow().clone());
            match state {
                Some(state) if state.leader.is_some() => {
                    self.follow(&state, real, out);
                    return;
                }
                _ => {
                    self.role = Role::Solo;
                    out.push(chat_line("The leader left; playing on your own"));
                }
            }
        }
        self.step(real, out);
        self.publish();
    }

    fn follow(&mut self, state: &LeaderState, real: Duration, out: &mut Vec<Bytes>) {
        self.head.set_speed(state.speed);
        if state.paused {
            self.head.pause();
        } else {
            self.head.play();
        }
        let seeked = state.seeks != self.seen_seeks;
        self.seen_seeks = state.seeks;
        if seeked || (self.head.position() - state.position).abs() > DRIFT_NS {
            self.seek(state.position, out);
        } else {
            self.step(real, out);
        }
    }

    fn publish(&self) {
        if self.role != Role::Leader {
            return;
        }
        let Some(shared) = &self.shared else { return };
        let id = self.transform.viewer_id;
        let next = LeaderState {
            leader: Some(id),
            position: self.head.position(),
            speed: self.head.speed(),
            paused: self.head.paused(),
            seeks: self.seeks,
        };
        shared.send_if_modified(|state| {
            let changed = state.leader != Some(id) || *state != next;
            if changed {
                *state = next;
            }
            changed
        });
    }

    /// Drops leadership when the viewer leaves.
    pub fn leave(&mut self) {
        if self.role == Role::Leader
            && let Some(shared) = &self.shared
        {
            let id = self.transform.viewer_id;
            shared.send_if_modified(|state| {
                let mine = state.leader == Some(id);
                if mine {
                    state.leader = None;
                }
                mine
            });
        }
        self.role = Role::Solo;
    }

    fn share_command(&mut self, sub: &str) -> Option<String> {
        let shared = self.shared.clone()?;
        let id = self.transform.viewer_id;
        let leader = shared.borrow().leader;
        Some(match sub {
            "lead" => match leader {
                Some(other) if other != id => "Someone is already leading".into(),
                _ => {
                    self.role = Role::Leader;
                    self.publish();
                    "You lead the shared playhead".into()
                }
            },
            "follow" => match leader {
                Some(other) if other != id => {
                    self.role = Role::Follower;
                    self.seen_seeks = shared.borrow().seeks.wrapping_sub(1);
                    "Following the leader".into()
                }
                Some(_) => "You are the leader".into(),
                None => "Nobody is leading; use /replay lead".into(),
            },
            _ => {
                self.leave();
                "Playing on your own".into()
            }
        })
    }

    /// Turns the hotbar controls on or off. They need a mode with a visible
    /// hotbar, so the viewer is in creative while they are shown.
    fn set_hotbar(&mut self, on: bool, out: &mut Vec<Bytes>) {
        self.hotbar = on;
        out.push(encode(&GameEvent {
            event: CHANGE_GAME_MODE,
            value: if on { CREATIVE } else { SPECTATOR },
        }));
        self.send_hotbar(out);
    }

    /// Writes the control items (or empties their slots when off).
    pub fn send_hotbar(&self, out: &mut Vec<Bytes>) {
        let items = self.hotbar_items();
        for (i, item) in items.into_iter().enumerate() {
            out.push(encode(&ContainerSetSlot {
                window_id: 0,
                state_id: 0,
                slot: HOTBAR_FIRST_SLOT + i as i16,
                item: if self.hotbar { Some(item) } else { None },
            }));
        }
    }

    fn hotbar_items(&self) -> [ItemStack; 5] {
        let named = |item: Item, name: &str| {
            let mut stack = ItemStack::new(item, 1);
            stack.components.custom_name = Some(Box::new(TextComponent::new(name).color("aqua")));
            stack
        };
        [
            if self.head.paused() {
                named(Item::LimeDye, "Play")
            } else {
                named(Item::RedDye, "Pause")
            },
            named(Item::Feather, "Back 10s"),
            named(Item::Arrow, "Forward 10s"),
            named(
                Item::Clock,
                &format!("Speed {}x (click to change)", self.head.speed()),
            ),
            named(Item::Barrier, "Restart"),
        ]
    }

    /// Handles selecting hotbar slot `slot`; returns feedback when it was a
    /// control item.
    pub fn hotbar_select(&mut self, slot: i16, out: &mut Vec<Bytes>) -> Option<String> {
        self.held = slot;
        self.hotbar_use(out)
    }

    /// Handles using the held item.
    pub fn hotbar_use(&mut self, out: &mut Vec<Bytes>) -> Option<String> {
        if !self.hotbar || !(0..5).contains(&self.held) {
            return None;
        }
        let command = match self.held {
            0 if self.head.paused() => "replay play".to_string(),
            0 => "replay pause".to_string(),
            1 => format!("replay skip -{HOTBAR_SKIP_SECONDS}"),
            2 => format!("replay skip {HOTBAR_SKIP_SECONDS}"),
            3 => {
                let next = HOTBAR_SPEEDS
                    .iter()
                    .copied()
                    .find(|s| *s > self.head.speed() + 1e-9)
                    .unwrap_or(HOTBAR_SPEEDS[0]);
                format!("replay speed {next}")
            }
            _ => "replay restart".to_string(),
        };
        let reply = self.command(&command, out);
        self.send_hotbar(out);
        Some(reply)
    }

    /// Jumps to `ns`. Going back resets the viewer's world and plays again
    /// from the start at once; going forward plays the gap at once. Either
    /// way only what is still there at `ns` is sent.
    pub fn seek(&mut self, ns: i64, out: &mut Vec<Bytes>) {
        let target = self.head.seek(ns);
        self.seeks += 1;
        if self.last_ns.is_some_and(|last| target < last) {
            self.release_camera(out);
            self.transform.clear(out);
            self.cursor = 0;
            self.last_ns = None;
        }
        self.catch_up(true, out);
    }

    /// Runs a `/replay` command (without the leading slash); returns the
    /// feedback line.
    pub fn command(&mut self, line: &str, out: &mut Vec<Bytes>) -> String {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("replay") => {}
            Some("tp" | "teleport") => return self.teleport(&words.collect::<Vec<_>>(), out),
            _ => return "Unknown command. Try /replay help".into(),
        }
        let sub = words.next().unwrap_or("status");
        let arg = words.next();
        if matches!(sub, "lead" | "follow" | "unfollow") {
            return self
                .share_command(sub)
                .unwrap_or_else(|| "No shared playhead here".into());
        }
        if sub == "hotbar" {
            let on = match arg {
                Some("on") => true,
                Some("off") => false,
                _ => !self.hotbar,
            };
            self.set_hotbar(on, out);
            return format!("Hotbar controls {}", if on { "on" } else { "off" });
        }
        if self.role == Role::Follower && !matches!(sub, "status") {
            return "You are following the leader; /replay unfollow first".into();
        }
        let seconds = arg
            .and_then(|a| a.parse::<f64>().ok())
            .filter(|s| s.is_finite());
        match (sub, seconds) {
            ("play", _) => {
                self.head.play();
                self.status()
            }
            ("pause", _) => {
                self.head.pause();
                self.status()
            }
            ("speed", Some(speed)) => {
                self.head.set_speed(speed);
                self.status()
            }
            ("seek", Some(at)) => {
                self.seek((at * SECOND) as i64, out);
                self.status()
            }
            ("skip", Some(by)) => {
                self.seek(self.head.position().saturating_add((by * SECOND) as i64), out);
                self.status()
            }
            ("restart", _) => {
                self.seek(0, out);
                self.status()
            }
            ("status", _) => self.status(),
            _ => "Usage: /replay play | pause | speed <x> | seek <seconds> | skip <+-seconds> | restart | status | hotbar | lead | follow | unfollow".into(),
        }
    }

    /// Moves only the viewer: `<x> <y> <z> [yaw pitch]` with `~` relative
    /// coordinates, or the recorded player's name. Frees a spectated entity.
    fn teleport(&mut self, args: &[&str], out: &mut Vec<Bytes>) -> String {
        let (base, base_yaw, base_pitch) = self.placed.unwrap_or((
            self.transform.position,
            self.transform.rotation.0,
            self.transform.rotation.1,
        ));
        let (position, yaw, pitch) = match args {
            [name] if name.eq_ignore_ascii_case(&self.script.player) => (
                self.transform.position,
                self.transform.rotation.0,
                self.transform.rotation.1,
            ),
            [target] if self.transform.locate(target).is_some() => (
                self.transform.locate(target).unwrap_or_default(),
                base_yaw,
                base_pitch,
            ),
            [x, y, z] | [x, y, z, _, _] => {
                let coord = |arg: &str, base: f64| relative(arg, base);
                let rot = |i: usize, base: f32| match args.get(i) {
                    Some(a) => relative(a, f64::from(base)).map(|v| v as f32),
                    None => Some(base),
                };
                match (
                    coord(x, base[0]),
                    coord(y, base[1]),
                    coord(z, base[2]),
                    rot(3, base_yaw),
                    rot(4, base_pitch),
                ) {
                    (Some(x), Some(y), Some(z), Some(yaw), Some(pitch)) => ([x, y, z], yaw, pitch),
                    _ => return "Expected a coordinate".into(),
                }
            }
            [] => return "Usage: /tp <x> <y> <z> [yaw pitch] | /tp <player>".into(),
            _ => return "No entity was found".into(),
        };
        if position.iter().any(|c| !c.is_finite() || c.abs() > 3.0e7) {
            return "Coordinates are out of bounds".into();
        }
        self.release_camera(out);
        self.teleports = self.teleports.wrapping_add(1);
        out.push(encode(&PlayerPosition {
            teleport_id: 1000 + self.teleports,
            position,
            yaw,
            pitch,
        }));
        self.placed = Some((position, yaw, pitch));
        format!(
            "Teleported to {:.2}, {:.2}, {:.2}",
            position[0], position[1], position[2]
        )
    }

    /// Tab completion for the `/tp` target: recorded player names.
    pub fn suggest(&self, text: &str) -> Option<(usize, usize, Vec<String>)> {
        let (start, partial) = teleport_partial(text)?;
        let lower = partial.to_ascii_lowercase();
        let mut names = self.transform.player_names();
        if !names
            .iter()
            .any(|n| n.eq_ignore_ascii_case(&self.script.player))
        {
            names.push(self.script.player.clone());
        }
        names.retain(|n| n.to_ascii_lowercase().starts_with(&lower));
        names.sort();
        Some((start, partial.len(), names))
    }

    pub fn status(&self) -> String {
        format!(
            "{} {} / {} at {}x",
            if self.head.paused() {
                "Paused"
            } else {
                "Playing"
            },
            clock(self.head.position()),
            clock(self.head.duration()),
            self.head.speed()
        )
    }
}

fn chat_line(text: &str) -> Bytes {
    encode(&SystemChat {
        content: &TextComponent::new(text),
        overlay: false,
    })
}

fn relative(arg: &str, base: f64) -> Option<f64> {
    match arg.strip_prefix('~') {
        Some("") => Some(base),
        Some(offset) => offset.parse::<f64>().ok().map(|o| base + o),
        None => arg.parse::<f64>().ok(),
    }
}

fn set_camera(entity: i32) -> Bytes {
    let mut w = PacketWriter::new();
    w.write_var_int(C::SetCamera.id());
    w.write_var_int(entity);
    w.into_bytes()
}

/// `m:ss` for a time in nanoseconds.
pub fn clock(ns: i64) -> String {
    let seconds = ns.max(0) / 1_000_000_000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script::Event;
    use tuff_core::protocol::play::{AddEntity, RemoveEntities};

    const TICK: i64 = 50_000_000;

    fn viewer() -> Viewer {
        let packet = |id: i32, add: bool| {
            if add {
                encode(&AddEntity {
                    entity_id: id,
                    uuid: id as u128,
                    kind: 1,
                    position: [0.0; 3],
                    velocity: [0.0; 3],
                    pitch: 0.0,
                    yaw: 0.0,
                    head_yaw: 0.0,
                    data: 0,
                })
            } else {
                encode(&RemoveEntities(&[id]))
            }
        };
        let events = vec![
            Timed {
                ns: 0,
                event: Event::Packet(packet(7, true)),
            },
            Timed {
                ns: 10 * TICK,
                event: Event::Packet(packet(7, false)),
            },
            Timed {
                ns: 20 * TICK,
                event: Event::Packet(packet(7, true)),
            },
        ];
        let script = Script::from_events("P".into(), 1, Vec::new(), events).unwrap();
        let mut viewer = Viewer::new(Arc::new(script), 999);
        viewer.seek(0, &mut Vec::new());
        viewer
    }

    fn camera_packets(out: &[Bytes]) -> Vec<Bytes> {
        out.iter()
            .filter(|p| p.first().copied().map(i32::from) == Some(C::SetCamera.id()))
            .cloned()
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn spectating_a_held_entity_sets_the_camera_and_sneaking_frees_it() {
        let mut v = viewer();
        let mut out = Vec::new();
        v.spectate(8, &mut out);
        assert!(out.is_empty());
        v.spectate(7, &mut out);
        assert_eq!(v.camera(), Some(7));
        assert_eq!(camera_packets(&out), vec![set_camera(7)]);
        out.clear();
        v.release_camera(&mut out);
        assert_eq!(camera_packets(&out), vec![set_camera(999)]);
        v.release_camera(&mut out);
        assert_eq!(camera_packets(&out).len(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn camera_returns_when_the_entity_despawns_or_the_viewer_seeks_back() {
        let mut v = viewer();
        let mut out = Vec::new();
        v.spectate(7, &mut out);
        out.clear();
        v.step(Duration::from_millis(600), &mut out);
        assert_eq!(v.camera(), None);
        assert_eq!(camera_packets(&out), vec![set_camera(999)]);

        v.seek(25 * TICK, &mut Vec::new());
        v.spectate(7, &mut out);
        out.clear();
        v.seek(0, &mut out);
        assert_eq!(v.camera(), None);
        assert_eq!(camera_packets(&out), vec![set_camera(999)]);
    }

    fn positions(out: &[Bytes]) -> Vec<Bytes> {
        out.iter()
            .filter(|p| p.first().copied().map(i32::from) == Some(C::PlayerPosition.id()))
            .cloned()
            .collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn tp_moves_only_the_viewer_and_frees_the_camera() {
        let mut v = viewer();
        let mut out = Vec::new();
        v.spectate(7, &mut out);
        out.clear();
        let reply = v.command("tp 10 64 -5", &mut out);
        assert!(reply.starts_with("Teleported"));
        assert_eq!(camera_packets(&out), vec![set_camera(999)]);
        assert_eq!(positions(&out).len(), 1);
        assert_eq!(v.transform.position, [0.0; 3]);
        out.clear();
        v.command("tp ~1 ~ ~", &mut out);
        let expect = encode(&PlayerPosition {
            teleport_id: 1002,
            position: [11.0, 64.0, -5.0],
            yaw: 0.0,
            pitch: 0.0,
        });
        assert_eq!(positions(&out), vec![expect]);
        out.clear();
        assert_eq!(v.command("tp nope", &mut out), "No entity was found");
        assert!(v.command("tp a b c", &mut out).contains("coordinate"));
        assert!(out.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn tp_to_the_recorded_player_by_name() {
        let mut v = viewer();
        let mut out = Vec::new();
        v.command("tp p", &mut out);
        assert_eq!(positions(&out).len(), 1);
    }

    fn watched_player() -> Viewer {
        let mut info = PacketWriter::new();
        info.write_var_int(C::PlayerInfoUpdate.id());
        info.write_unsigned_byte(0x01);
        info.write_var_int(1);
        info.write_uuid(7);
        info.write_string("Seven");
        info.write_var_int(0);
        let add = encode(&AddEntity {
            entity_id: 7,
            uuid: 7,
            kind: 1,
            position: [1.0, 2.0, 3.0],
            velocity: [0.0; 3],
            pitch: 0.0,
            yaw: 0.0,
            head_yaw: 0.0,
            data: 0,
        });
        let mut step = PacketWriter::new();
        step.write_var_int(C::MoveEntityPos.id());
        step.write_var_int(7);
        step.write_var_int(0);
        step.write_short(4096);
        step.write_short(0);
        step.write_short(-2048);
        let events = [info.into_bytes(), add, step.into_bytes()]
            .into_iter()
            .map(|p| Timed {
                ns: 0,
                event: Event::Packet(p),
            })
            .collect();
        let script = Script::from_events("P".into(), 1, Vec::new(), events).unwrap();
        let mut viewer = Viewer::new(Arc::new(script), 999);
        viewer.seek(0, &mut Vec::new());
        viewer
    }

    #[tuff_core::watchdog]
    #[test]
    fn tp_to_a_recorded_entity_by_name_id_or_uuid() {
        let mut v = watched_player();
        let expect = encode(&PlayerPosition {
            teleport_id: 1001,
            position: [2.0, 2.0, 2.5],
            yaw: 0.0,
            pitch: 0.0,
        });
        let mut out = Vec::new();
        assert!(v.command("tp seven", &mut out).starts_with("Teleported"));
        assert_eq!(positions(&out), vec![expect]);
        out.clear();
        assert!(v.command("tp 7", &mut out).starts_with("Teleported"));
        assert!(
            v.command("tp 00000000-0000-0000-0000-000000000007", &mut out)
                .starts_with("Teleported")
        );
        assert_eq!(v.command("tp 8", &mut out), "No entity was found");
    }

    #[tuff_core::watchdog]
    #[test]
    fn tab_completion_offers_recorded_names() {
        let v = watched_player();
        assert_eq!(
            v.suggest("/tp "),
            Some((4, 0, vec!["P".to_string(), "Seven".to_string()]))
        );
        assert_eq!(
            v.suggest("/teleport se"),
            Some((10, 2, vec!["Seven".to_string()]))
        );
        assert_eq!(v.suggest("/tp Seven 1"), None);
    }

    fn pair() -> (Viewer, Viewer) {
        let mut a = viewer();
        let mut b = viewer();
        b.transform.viewer_id = 998;
        let shared = crate::shared::new();
        a.attach(shared.clone());
        b.attach(shared);
        (a, b)
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_follower_copies_the_leader_and_seeks_with_it() {
        let (mut a, mut b) = pair();
        let mut out = Vec::new();
        assert!(b.command("replay follow", &mut out).contains("lead"));
        assert!(a.command("replay lead", &mut out).contains("lead"));
        assert!(b.command("replay lead", &mut out).contains("already"));
        assert!(b.command("replay follow", &mut out).contains("Following"));
        a.command("replay seek 0.6", &mut out);
        a.tick(Duration::ZERO, &mut out);
        b.tick(Duration::ZERO, &mut out);
        assert_eq!(b.head.position(), a.head.position());
        assert_eq!(b.role(), Role::Follower);
        a.command("replay pause", &mut out);
        a.tick(Duration::ZERO, &mut out);
        b.tick(Duration::from_secs(1), &mut out);
        assert!(b.head.paused());
        assert_eq!(b.head.position(), a.head.position());
        assert!(b.command("replay seek 0", &mut out).contains("unfollow"));
    }

    #[tuff_core::watchdog]
    #[test]
    fn a_follower_goes_solo_when_the_leader_leaves() {
        let (mut a, mut b) = pair();
        let mut out = Vec::new();
        a.command("replay lead", &mut out);
        b.command("replay follow", &mut out);
        a.leave();
        out.clear();
        b.tick(Duration::ZERO, &mut out);
        assert_eq!(b.role(), Role::Solo);
        assert_eq!(out.len(), 1);
    }

    #[tuff_core::watchdog]
    #[test]
    fn hotbar_items_control_playback() {
        let mut v = viewer();
        let mut out = Vec::new();
        assert!(v.hotbar_select(0, &mut out).is_none());
        v.command("replay hotbar on", &mut out);
        assert!(v.hotbar());
        out.clear();
        assert!(v.hotbar_select(0, &mut out).unwrap().starts_with("Paused"));
        assert_eq!(out.len(), 5);
        assert!(v.head.paused());
        v.hotbar_select(0, &mut out);
        assert!(!v.head.paused());
        v.hotbar_select(2, &mut out);
        assert_eq!(v.head.position(), 10_000_000_000.min(v.head.duration()));
        v.hotbar_select(3, &mut out);
        assert_eq!(v.head.speed(), 2.0);
        v.hotbar_select(4, &mut out);
        assert_eq!(v.head.position(), 0);
        assert!(v.hotbar_select(6, &mut out).is_none());
        v.command("replay hotbar off", &mut out);
        assert!(v.hotbar_select(0, &mut out).is_none());
    }
}
