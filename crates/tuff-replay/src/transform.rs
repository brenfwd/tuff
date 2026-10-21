//! Per-viewer rewriting of the recorded clientbound stream: the viewer gets
//! its own entity id and spectator mode, the recorded player becomes an
//! entity the viewer can watch, and packets about the recorded player's own
//! controls and screens are dropped. The transformer also tracks what the
//! viewer holds (chunks, entities, boss bars, tab list, scoreboard, titles,
//! border, maps, weather) so a seek can reset it.

use std::collections::{BTreeMap, BTreeSet};

use bytes::{BufMut, Bytes, BytesMut};
use tuff_core::protocol::codec::ReadExt;
use tuff_core::protocol::play::{
    AddEntity, BossAction, BossEvent, ClearTitles, EntityPositionSync, ForgetLevelChunk, MapData,
    MapPatch, PlayerInfoRemove, PlayerPosition, RemoveEntities, RotateHead, SetActionBarText,
    SetPlayerTeam, TeamAction,
};
use tuff_core::protocol::{Encode, encode_packet};
use tuff_core::world::border::Border;
use tuff_item::text::TextComponent;
use tuff_vanilla_data::packet::play::Clientbound as C;
use tuff_wire::{PacketReader, PacketWriter};

use crate::script::Event;

const SPECTATOR: u8 = 3;
const START_RAINING: u8 = 1;
const STOP_RAINING: u8 = 2;
const CHANGE_GAME_MODE: u8 = 3;
const WIN_GAME: u8 = 4;
const DEMO_EVENT: u8 = 5;
const RAIN_LEVEL: u8 = 7;
const THUNDER_LEVEL: u8 = 8;
const START_WAITING_FOR_CHUNKS: u8 = 13;
const MAP_SIDE: u8 = 128;

mod info {
    pub const ADD_PLAYER: u8 = 0x01;
    pub const INITIALIZE_CHAT: u8 = 0x02;
    pub const GAME_MODE: u8 = 0x04;
    pub const LISTED: u8 = 0x08;
    pub const LATENCY: u8 = 0x10;
    pub const DISPLAY_NAME: u8 = 0x20;
    pub const LIST_ORDER: u8 = 0x40;
    pub const HAT: u8 = 0x80;
}

pub fn encode<P: Encode>(packet: &P) -> Bytes {
    encode_packet(packet).into_bytes()
}

fn degrees(angle: i8) -> f32 {
    f32::from(angle) * 360.0 / 256.0
}

fn dropped(id: i32) -> bool {
    [
        C::KeepAlive,
        C::Ping,
        C::PlayerRotation,
        C::PlayerAbilities,
        C::SetHealth,
        C::SetExperience,
        C::ContainerSetContent,
        C::ContainerSetSlot,
        C::ContainerSetData,
        C::ContainerClose,
        C::SetCursorItem,
        C::SetPlayerInventory,
        C::OpenScreen,
        C::OpenBook,
        C::OpenSignEditor,
        C::MountScreenOpen,
        C::MerchantOffers,
        C::PlaceGhostRecipe,
        C::RecipeBookAdd,
        C::RecipeBookRemove,
        C::RecipeBookSettings,
        C::SetHeldSlot,
        C::SetCamera,
        C::Commands,
        C::CommandSuggestions,
        C::CustomChatCompletions,
        C::Disconnect,
        C::StartConfiguration,
        C::CookieRequest,
        C::StoreCookie,
        C::Transfer,
        C::PlayerCombatKill,
        C::Cooldown,
        C::AwardStats,
    ]
    .iter()
    .any(|k| k.id() == id)
}

#[derive(Default, Debug, Clone, PartialEq)]
pub struct Transformer {
    pub viewer_id: i32,
    pub recorded_uuid: u128,
    /// The recorded player's entity id, once its Login was seen.
    pub recorded_id: Option<i32>,
    pub position: [f64; 3],
    pub rotation: (f32, f32),
    spawned: bool,
    viewer_placed: bool,
    rejoined: bool,
    pub chunks: BTreeSet<(i32, i32)>,
    pub entities: BTreeSet<i32>,
    pub boss_bars: BTreeSet<u128>,
    pub tab_list: BTreeSet<u128>,
    names: BTreeMap<u128, String>,
    located: BTreeMap<i32, (u128, [f64; 3])>,
    /// Yaw, pitch and head yaw in degrees of the entities in `located`.
    facing: BTreeMap<i32, [f32; 3]>,
    /// Held entities whose moves a seek folded away and that still need one sync.
    collapsed: BTreeSet<i32>,
    pub objectives: BTreeSet<String>,
    pub teams: BTreeSet<String>,
    pub maps: BTreeSet<i32>,
    pub titles: bool,
    pub action_bar: bool,
    pub border: bool,
    pub weather: bool,
    /// The recorded player moved since its entity was last synced.
    moved: bool,
}

impl Transformer {
    pub fn new(viewer_id: i32, recorded_uuid: u128) -> Self {
        Self {
            viewer_id,
            recorded_uuid,
            ..Self::default()
        }
    }

    /// The packets that remove everything the viewer holds, leaving the
    /// transformer as if nothing after the viewer's own Login was played.
    pub fn clear(&mut self, out: &mut Vec<Bytes>) {
        let mut ids: Vec<i32> = self.entities.iter().copied().collect();
        if self.spawned
            && let Some(id) = self.recorded_id
        {
            ids.push(id);
        }
        if !ids.is_empty() {
            out.push(encode(&RemoveEntities(&ids)));
        }
        for &(x, z) in &self.chunks {
            out.push(encode(&ForgetLevelChunk { x, z }));
        }
        for &uuid in &self.boss_bars {
            out.push(encode(&BossEvent {
                uuid,
                action: BossAction::Remove,
            }));
        }
        if !self.tab_list.is_empty() {
            let uuids: Vec<u128> = self.tab_list.iter().copied().collect();
            out.push(encode(&PlayerInfoRemove(&uuids)));
        }
        for name in &self.objectives {
            let mut w = PacketWriter::new();
            w.write_var_int(C::SetObjective.id());
            w.write_string(name);
            w.write_byte(1);
            out.push(w.into_bytes());
        }
        for name in &self.teams {
            out.push(encode(&SetPlayerTeam {
                name,
                action: TeamAction::Remove,
            }));
        }
        if self.titles {
            out.push(encode(&ClearTitles { reset: true }));
        }
        if self.action_bar {
            out.push(encode(&SetActionBarText(
                &TextComponent::new(String::new()),
            )));
        }
        if self.border {
            out.push(encode(&Border::default().initialize()));
        }
        if !self.maps.is_empty() {
            let blank = vec![0u8; usize::from(MAP_SIDE) * usize::from(MAP_SIDE)];
            for &id in &self.maps {
                out.push(encode(&MapData {
                    id: id as u32,
                    scale: 0,
                    locked: false,
                    markers: Some(&[]),
                    patch: Some(MapPatch {
                        x: 0,
                        y: 0,
                        width: MAP_SIDE,
                        height: MAP_SIDE,
                        colors: &blank,
                    }),
                }));
            }
        }
        if self.weather {
            for event in [STOP_RAINING, RAIN_LEVEL, THUNDER_LEVEL] {
                out.push(game_event(event, 0.0));
            }
        }
        *self = Self {
            recorded_id: self.recorded_id,
            position: self.position,
            rotation: self.rotation,
            rejoined: self.recorded_id.is_some(),
            ..Self::new(self.viewer_id, self.recorded_uuid)
        };
    }

    /// Whether the viewer currently holds this entity, so it can be viewed.
    pub fn holds(&self, entity: i32) -> bool {
        self.entities.contains(&entity) || (self.spawned && self.recorded_id == Some(entity))
    }

    /// Names of the players in the recorded tab list, sorted.
    pub fn player_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .names
            .values()
            .filter(|n| !n.is_empty())
            .cloned()
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Where a recorded entity is: a tab list player's name (case
    /// insensitive), a held entity's id or its UUID with or without dashes.
    pub fn locate(&self, target: &str) -> Option<[f64; 3]> {
        if let Some((uuid, _)) = self
            .names
            .iter()
            .find(|(_, name)| name.eq_ignore_ascii_case(target))
            && let Some((_, position)) = self.located.values().find(|(u, _)| u == uuid)
        {
            return Some(*position);
        }
        if let Ok(id) = target.parse::<i32>() {
            return self.located.get(&id).map(|(_, p)| *p);
        }
        let hex: String = target.chars().filter(|c| *c != '-').collect();
        let uuid = u128::from_str_radix(&hex, 16)
            .ok()
            .filter(|_| hex.len() == 32)?;
        self.located
            .values()
            .find(|(u, _)| *u == uuid)
            .map(|(_, p)| *p)
    }

    /// Syncs the recorded player's entity if it moved. Called once after a
    /// batch of events, so a seek sends one sync instead of one per move.
    pub fn finish(&mut self, out: &mut Vec<Bytes>) {
        if self.rejoined && !self.viewer_placed && self.position != [0.0; 3] {
            self.rejoined = false;
            self.viewer_placed = true;
            out.push(encode(&PlayerPosition {
                teleport_id: 1,
                position: self.position,
                yaw: self.rotation.0,
                pitch: self.rotation.1,
            }));
        }
        if std::mem::take(&mut self.moved) {
            self.sync_recorded(out);
        }
    }

    pub fn apply(&mut self, event: &Event, out: &mut Vec<Bytes>) {
        match event {
            Event::Moved { position, rotation } => {
                if let Some(p) = position {
                    self.position = *p;
                }
                if let Some(r) = rotation {
                    self.rotation = *r;
                }
                self.moved = true;
            }
            Event::Packet(payload) => self.packet(payload, out),
        }
    }

    fn packet(&mut self, payload: &Bytes, out: &mut Vec<Bytes>) {
        let mut r = PacketReader::new(payload.clone());
        let Ok(id) = r.read_var_int() else {
            return;
        };
        let body = payload.slice(payload.len() - r.remaining()..);
        if dropped(id) {
            return;
        }
        if id == C::Login.id() {
            self.login(&body, out);
        } else if id == C::Respawn.id() {
            if let Some(rewritten) = rewrite_spawn_info(&body, 0) {
                out.push(with_id(C::Respawn.id(), &rewritten));
                out.push(game_event(START_WAITING_FOR_CHUNKS, 0.0));
            }
            self.viewer_placed = false;
            self.rejoined = true;
            self.chunks.clear();
            self.entities.clear();
            self.located.clear();
            self.facing.clear();
            self.collapsed.clear();
            self.spawned = false;
            self.moved = false;
        } else if id == C::PlayerPosition.id() {
            self.teleported(&body, out);
        } else if id == C::GameEvent.id() {
            match body.first() {
                Some(&(CHANGE_GAME_MODE | WIN_GAME | DEMO_EVENT)) | None => {}
                Some(&event) => {
                    if matches!(event, START_RAINING | RAIN_LEVEL | THUNDER_LEVEL) {
                        self.weather = true;
                    }
                    out.push(payload.clone());
                }
            }
        } else {
            self.track(id, &body);
            out.push(payload.clone());
        }
    }

    fn login(&mut self, body: &Bytes, out: &mut Vec<Bytes>) {
        if body.len() < 4 {
            return;
        }
        let recorded = i32::from_be_bytes(body[..4].try_into().unwrap());
        if self.recorded_id.is_some() {
            // A reconnect: its world replaces the old one.
            self.clear(out);
            self.recorded_id = Some(recorded);
            if let Some(spawn) = login_spawn_info(body)
                && let Some(mut respawn) = rewrite_spawn_info(&spawn, 0)
            {
                respawn.push(0);
                out.push(with_id(C::Respawn.id(), &respawn));
                out.push(game_event(START_WAITING_FOR_CHUNKS, 0.0));
            }
            return;
        }
        self.recorded_id = Some(recorded);
        let Some(at) = login_game_mode_offset(body) else {
            return;
        };
        let mut rewritten = BytesMut::from(&body[..]);
        rewritten[..4].copy_from_slice(&self.viewer_id.to_be_bytes());
        rewritten[at] = SPECTATOR;
        out.push(with_id(C::Login.id(), &rewritten));
    }

    fn teleported(&mut self, body: &Bytes, out: &mut Vec<Bytes>) {
        let mut r = PacketReader::new(body.clone());
        let read = (|| {
            r.read_var_int().ok()?;
            let p = [
                r.read_double().ok()?,
                r.read_double().ok()?,
                r.read_double().ok()?,
            ];
            for _ in 0..3 {
                r.read_double().ok()?;
            }
            let rot = (r.read_float().ok()?, r.read_float().ok()?);
            let flags = r.read_int().ok()?;
            Some((p, rot, flags))
        })();
        let Some((p, rot, flags)) = read else {
            return;
        };
        for (axis, value) in p.into_iter().enumerate() {
            if flags & (1 << axis) != 0 {
                self.position[axis] += value;
            } else {
                self.position[axis] = value;
            }
        }
        self.rotation.0 = if flags & 8 != 0 {
            self.rotation.0 + rot.0
        } else {
            rot.0
        };
        self.rotation.1 = if flags & 16 != 0 {
            self.rotation.1 + rot.1
        } else {
            rot.1
        };
        if !self.viewer_placed {
            self.viewer_placed = true;
            out.push(encode(&PlayerPosition {
                teleport_id: 1,
                position: self.position,
                yaw: self.rotation.0,
                pitch: self.rotation.1,
            }));
        }
        self.moved = true;
    }

    fn sync_recorded(&mut self, out: &mut Vec<Bytes>) {
        let Some(entity_id) = self.recorded_id else {
            return;
        };
        let (yaw, pitch) = self.rotation;
        if !self.spawned {
            self.spawned = true;
            out.push(encode(&AddEntity {
                entity_id,
                uuid: self.recorded_uuid,
                kind: player_kind(),
                position: self.position,
                velocity: [0.0; 3],
                pitch,
                yaw,
                head_yaw: yaw,
                data: 0,
            }));
            return;
        }
        out.push(encode(&EntityPositionSync {
            entity_id,
            position: self.position,
            yaw,
            pitch,
            on_ground: false,
        }));
        out.push(encode(&RotateHead {
            entity_id,
            head_yaw: yaw,
        }));
    }

    /// Applies a move a seek folded away: tracked, not sent. `flush_collapsed`
    /// sends the result.
    pub fn collapse(&mut self, payload: &Bytes) {
        let mut r = PacketReader::new(payload.clone());
        let Ok(id) = r.read_var_int() else {
            return;
        };
        let body = payload.slice(payload.len() - r.remaining()..);
        if let Ok(entity) = PacketReader::new(body.clone()).read_var_int() {
            self.collapsed.insert(entity);
        }
        self.track(id, &body);
    }

    /// One position sync per held entity whose moves were folded away.
    pub fn flush_collapsed(&mut self, out: &mut Vec<Bytes>) {
        for entity_id in std::mem::take(&mut self.collapsed) {
            let (Some((_, position)), Some([yaw, pitch, head_yaw])) =
                (self.located.get(&entity_id), self.facing.get(&entity_id))
            else {
                continue;
            };
            out.push(encode(&EntityPositionSync {
                entity_id,
                position: *position,
                yaw: *yaw,
                pitch: *pitch,
                on_ground: false,
            }));
            out.push(encode(&RotateHead {
                entity_id,
                head_yaw: *head_yaw,
            }));
        }
    }

    fn track(&mut self, id: i32, body: &Bytes) {
        let mut r = PacketReader::new(body.clone());
        if id == C::LevelChunkWithLight.id() {
            if let (Ok(x), Ok(z)) = (r.read_int(), r.read_int()) {
                self.chunks.insert((x, z));
            }
        } else if id == C::ForgetLevelChunk.id() {
            if let (Ok(z), Ok(x)) = (r.read_int(), r.read_int()) {
                self.chunks.remove(&(x, z));
            }
        } else if id == C::AddEntity.id() {
            if let Ok(entity) = r.read_var_int() {
                self.entities.insert(entity);
                if let (Ok(uuid), Ok(_), Ok(x), Ok(y), Ok(z)) = (
                    r.read_uuid(),
                    r.read_var_int(),
                    r.read_double(),
                    r.read_double(),
                    r.read_double(),
                ) {
                    self.located.insert(entity, (uuid, [x, y, z]));
                    let [pitch, yaw, head] = r
                        .read_lp_vec3()
                        .ok()
                        .and_then(|_| {
                            Some([
                                r.read_byte().ok()?,
                                r.read_byte().ok()?,
                                r.read_byte().ok()?,
                            ])
                        })
                        .map_or([0.0; 3], |angles| angles.map(degrees));
                    self.facing.insert(entity, [yaw, pitch, head]);
                }
            }
        } else if id == C::RemoveEntities.id() {
            let count = r.read_var_int().unwrap_or(0).max(0);
            for _ in 0..count {
                let Ok(entity) = r.read_var_int() else {
                    break;
                };
                self.entities.remove(&entity);
                self.located.remove(&entity);
                self.facing.remove(&entity);
                self.collapsed.remove(&entity);
            }
        } else if id == C::MoveEntityPos.id() || id == C::MoveEntityPosRot.id() {
            if let (Ok(entity), Ok(_), Ok(dx), Ok(dy), Ok(dz)) = (
                r.read_var_int(),
                r.read_var_int(),
                r.read_short(),
                r.read_short(),
                r.read_short(),
            ) && let Some((_, position)) = self.located.get_mut(&entity)
            {
                for (axis, d) in [dx, dy, dz].into_iter().enumerate() {
                    position[axis] += f64::from(d) / 4096.0;
                }
                if id == C::MoveEntityPosRot.id()
                    && let (Ok(yaw), Ok(pitch)) = (r.read_byte(), r.read_byte())
                    && let Some(f) = self.facing.get_mut(&entity)
                {
                    f[0] = degrees(yaw);
                    f[1] = degrees(pitch);
                }
            }
        } else if id == C::MoveEntityRot.id() {
            if let (Ok(entity), Ok(yaw), Ok(pitch)) =
                (r.read_var_int(), r.read_byte(), r.read_byte())
                && let Some(f) = self.facing.get_mut(&entity)
            {
                f[0] = degrees(yaw);
                f[1] = degrees(pitch);
            }
        } else if id == C::RotateHead.id() {
            if let (Ok(entity), Ok(head)) = (r.read_var_int(), r.read_byte())
                && let Some(f) = self.facing.get_mut(&entity)
            {
                f[2] = degrees(head);
            }
        } else if id == C::EntityPositionSync.id() {
            let read = |r: &mut PacketReader| -> Option<(i32, [f64; 3], f32, f32)> {
                let entity = r.read_var_int().ok()?;
                let mut at = [0.0; 3];
                match r.read_var_int().ok()? {
                    0 => {
                        for c in &mut at {
                            *c = r.read_double().ok()?;
                        }
                    }
                    1 => {
                        let steps = r.read_var_int().ok()?;
                        if steps <= 0 {
                            return None;
                        }
                        for _ in 0..steps {
                            for c in &mut at {
                                *c = r.read_double().ok()?;
                            }
                            r.read_var_int().ok()?;
                        }
                    }
                    _ => return None,
                }
                Some((entity, at, r.read_float().ok()?, r.read_float().ok()?))
            };
            if let Some((entity, [x, y, z], yaw, pitch)) = read(&mut r)
                && let Some((_, position)) = self.located.get_mut(&entity)
            {
                *position = [x, y, z];
                if let Some(f) = self.facing.get_mut(&entity) {
                    f[0] = yaw;
                    f[1] = pitch;
                }
            }
        } else if id == C::TeleportEntity.id() {
            let read = |r: &mut PacketReader| -> Option<(i32, [f64; 3], [f32; 2], i32)> {
                let entity = r.read_var_int().ok()?;
                let at = [
                    r.read_double().ok()?,
                    r.read_double().ok()?,
                    r.read_double().ok()?,
                ];
                for _ in 0..3 {
                    r.read_double().ok()?;
                }
                let look = [r.read_float().ok()?, r.read_float().ok()?];
                Some((entity, at, look, r.read_int().ok()?))
            };
            if let Some((entity, at, look, relative)) = read(&mut r)
                && let Some((_, position)) = self.located.get_mut(&entity)
            {
                if let Some(f) = self.facing.get_mut(&entity) {
                    for (axis, value) in look.into_iter().enumerate() {
                        if relative & (1 << (3 + axis)) == 0 {
                            f[axis] = value;
                        } else {
                            f[axis] += value;
                        }
                    }
                }
                for axis in 0..3 {
                    if relative & (1 << axis) == 0 {
                        position[axis] = at[axis];
                    } else {
                        position[axis] += at[axis];
                    }
                }
            }
        } else if id == C::BossEvent.id()
            && let (Ok(uuid), Ok(action)) = (r.read_uuid(), r.read_var_int())
        {
            match action {
                0 => {
                    self.boss_bars.insert(uuid);
                }
                1 => {
                    self.boss_bars.remove(&uuid);
                }
                _ => {}
            }
        } else if id == C::PlayerInfoUpdate.id() {
            for (uuid, name) in added_players(&mut r) {
                self.tab_list.insert(uuid);
                self.names.insert(uuid, name);
            }
        } else if id == C::PlayerInfoRemove.id() {
            let count = r.read_var_int().unwrap_or(0).max(0);
            for _ in 0..count {
                let Ok(uuid) = r.read_uuid() else {
                    break;
                };
                self.tab_list.remove(&uuid);
                self.names.remove(&uuid);
            }
        } else if id == C::SetObjective.id() || id == C::SetPlayerTeam.id() {
            let names = if id == C::SetObjective.id() {
                &mut self.objectives
            } else {
                &mut self.teams
            };
            if let (Ok(name), Ok(method)) = (r.read_string(), r.read_byte()) {
                match method {
                    0 => {
                        names.insert(name);
                    }
                    1 => {
                        names.remove(&name);
                    }
                    _ => {}
                }
            }
        } else if id == C::SetTitleText.id()
            || id == C::SetSubtitleText.id()
            || id == C::SetTitlesAnimation.id()
        {
            self.titles = true;
        } else if id == C::ClearTitles.id() {
            if r.read_bool().unwrap_or(false) {
                self.titles = false;
            }
        } else if id == C::SetActionBarText.id() {
            self.action_bar = true;
        } else if [
            C::InitializeBorder,
            C::SetBorderCenter,
            C::SetBorderLerpSize,
            C::SetBorderSize,
            C::SetBorderWarningDelay,
            C::SetBorderWarningDistance,
        ]
        .iter()
        .any(|k| k.id() == id)
        {
            self.border = true;
        } else if id == C::MapItemData.id()
            && let Ok(map) = r.read_var_int()
        {
            self.maps.insert(map);
        }
    }
}

/// The players a Player Info Update adds to the tab list, as far as its
/// entries parse.
fn added_players(r: &mut PacketReader) -> Vec<(u128, String)> {
    let mut added = Vec::new();
    let Ok(actions) = r.read_unsigned_byte() else {
        return added;
    };
    if actions & info::ADD_PLAYER == 0 {
        return added;
    }
    let count = r.read_var_int().unwrap_or(0).max(0);
    for _ in 0..count {
        let Ok(uuid) = r.read_uuid() else {
            break;
        };
        match skip_info_entry(r, actions) {
            Some(name) => added.push((uuid, name.unwrap_or_default())),
            None => {
                added.push((uuid, String::new()));
                break;
            }
        }
    }
    added
}

fn skip_info_entry(r: &mut PacketReader, actions: u8) -> Option<Option<String>> {
    const MAX_KEY: usize = 512;
    const MAX_SIGNATURE: usize = 4096;
    let mut name = None;
    if actions & info::ADD_PLAYER != 0 {
        name = Some(r.read_string().ok()?);
        let properties = r.read_var_int().ok()?;
        for _ in 0..properties.max(0) {
            r.read_string().ok()?;
            r.read_string().ok()?;
            if r.read_bool().ok()? {
                r.read_string().ok()?;
            }
        }
    }
    if actions & info::INITIALIZE_CHAT != 0 && r.read_bool().ok()? {
        r.read_uuid().ok()?;
        r.read_long().ok()?;
        r.read_prefixed_bytes(MAX_KEY).ok()?;
        r.read_prefixed_bytes(MAX_SIGNATURE).ok()?;
    }
    if actions & info::GAME_MODE != 0 {
        r.read_var_int().ok()?;
    }
    if actions & info::LISTED != 0 {
        r.read_bool().ok()?;
    }
    if actions & info::LATENCY != 0 {
        r.read_var_int().ok()?;
    }
    if actions & info::DISPLAY_NAME != 0 && r.read_bool().ok()? {
        r.read_nbt().ok()?;
    }
    if actions & info::LIST_ORDER != 0 {
        r.read_var_int().ok()?;
    }
    if actions & info::HAT != 0 {
        r.read_bool().ok()?;
    }
    Some(name)
}

fn game_event(event: u8, value: f32) -> Bytes {
    let mut w = PacketWriter::new();
    w.write_var_int(C::GameEvent.id());
    w.write_unsigned_byte(event);
    w.write_float(value);
    w.into_bytes()
}

fn player_kind() -> i32 {
    use tuff_vanilla_data::registry::EntityType;
    EntityType::Player as i32
}

fn with_id(id: i32, body: &[u8]) -> Bytes {
    let mut w = PacketWriter::new();
    w.write_var_int(id);
    w.write_bytes(body);
    w.into_bytes()
}

/// Where the Login body's spawn info starts and where its game mode byte is.
fn login_layout(body: &Bytes) -> Option<(usize, usize)> {
    let mut r = PacketReader::new(body.clone());
    r.read_int().ok()?;
    r.read_bool().ok()?;
    let dimensions = r.read_var_int().ok()?;
    if !(0..=4096).contains(&dimensions) {
        return None;
    }
    for _ in 0..dimensions {
        r.read_string().ok()?;
    }
    r.read_var_int().ok()?;
    r.read_var_int().ok()?;
    r.read_var_int().ok()?;
    r.read_bool().ok()?;
    r.read_bool().ok()?;
    r.read_bool().ok()?;
    let spawn = body.len() - r.remaining();
    Some((spawn, spawn + spawn_game_mode_offset(&body.slice(spawn..))?))
}

fn login_game_mode_offset(body: &Bytes) -> Option<usize> {
    login_layout(body).map(|(_, at)| at)
}

/// The spawn info part of a Login body (without the trailing secure chat
/// flags).
fn login_spawn_info(body: &Bytes) -> Option<Bytes> {
    let (spawn, _) = login_layout(body)?;
    let rest = body.slice(spawn..);
    let len = spawn_info_len(&rest)?;
    Some(rest.slice(..len))
}

fn spawn_game_mode_offset(info: &Bytes) -> Option<usize> {
    let mut r = PacketReader::new(info.clone());
    r.read_var_int().ok()?;
    r.read_string().ok()?;
    r.read_long().ok()?;
    let at = info.len() - r.remaining();
    (at < info.len()).then_some(at)
}

fn spawn_info_len(info: &Bytes) -> Option<usize> {
    let mut r = PacketReader::new(info.clone());
    r.read_var_int().ok()?;
    r.read_string().ok()?;
    r.read_long().ok()?;
    r.read_byte().ok()?;
    r.read_byte().ok()?;
    r.read_bool().ok()?;
    r.read_bool().ok()?;
    if r.read_bool().ok()? {
        r.read_string().ok()?;
        r.read_long().ok()?;
    }
    r.read_var_int().ok()?;
    r.read_var_int().ok()?;
    Some(info.len() - r.remaining())
}

/// A Respawn (or Login spawn info) body with the game mode set to
/// spectator. `start` is where the spawn info begins.
fn rewrite_spawn_info(body: &Bytes, start: usize) -> Option<Vec<u8>> {
    let at = start + spawn_game_mode_offset(&body.slice(start..))?;
    let mut out = body.to_vec();
    out[at] = SPECTATOR;
    Some(out)
}

/// Frames a payload (id and body) for a connection.
pub fn frame(payload: &[u8], compression: Option<usize>) -> Bytes {
    let mut w = PacketWriter::new();
    w.write_bytes(payload);
    match compression {
        Some(threshold) => w
            .finish_compressed(threshold)
            .expect("in-memory zlib compression is infallible"),
        None => {
            let mut b = BytesMut::new();
            b.put(w.finish());
            b.freeze()
        }
    }
}

#[cfg(test)]
mod tests {
    use tuff_core::protocol::codec::WriteExt;
    use tuff_core::protocol::play::SetTitleText;

    use super::*;

    fn raw(kind: C, body: impl FnOnce(&mut PacketWriter)) -> Event {
        let mut w = PacketWriter::new();
        w.write_var_int(kind.id());
        body(&mut w);
        Event::Packet(w.into_bytes())
    }

    fn named(kind: C, name: &str, method: i8) -> Event {
        raw(kind, |w| {
            w.write_string(name);
            w.write_byte(method);
        })
    }

    fn ids(out: &[Bytes]) -> Vec<i32> {
        out.iter()
            .map(|p| PacketReader::new(p.clone()).read_var_int().unwrap())
            .collect()
    }

    fn tab_add(uuids: &[u128]) -> Event {
        raw(C::PlayerInfoUpdate, |w| {
            w.write_unsigned_byte(
                info::ADD_PLAYER | info::INITIALIZE_CHAT | info::LISTED | info::DISPLAY_NAME,
            );
            w.write_var_int(uuids.len() as i32);
            for &uuid in uuids {
                w.write_uuid(uuid);
                w.write_string("Someone");
                w.write_var_int(1);
                w.write_string("textures");
                w.write_string("abc");
                w.write_bool(true);
                w.write_string("sig");
                w.write_bool(true);
                w.write_uuid(9);
                w.write_long(0);
                w.write_prefixed_bytes(&[1, 2, 3]);
                w.write_prefixed_bytes(&[4, 5]);
                w.write_bool(true);
                w.write_bool(true);
                w.write_text(&TextComponent::new("Shown"));
            }
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn tracks_and_resets_the_whole_hud() {
        let mut t = Transformer::new(7, 1);
        let mut out = Vec::new();
        let events = [
            tab_add(&[10, 11, 12]),
            raw(C::PlayerInfoRemove, |w| {
                w.write_var_int(1);
                w.write_uuid(11);
            }),
            raw(C::PlayerInfoUpdate, |w| {
                w.write_unsigned_byte(info::LATENCY);
                w.write_var_int(1);
                w.write_uuid(13);
                w.write_var_int(5);
            }),
            named(C::SetObjective, "kills", 0),
            named(C::SetObjective, "deaths", 0),
            named(C::SetObjective, "deaths", 1),
            named(C::SetPlayerTeam, "red", 0),
            named(C::SetPlayerTeam, "red", 3),
            Event::Packet(encode(&SetTitleText(&TextComponent::new("Hi")))),
            Event::Packet(encode(&Border::default().initialize())),
            raw(C::MapItemData, |w| w.write_var_int(4)),
            raw(C::GameEvent, |w| {
                w.write_unsigned_byte(START_RAINING);
                w.write_float(0.0);
            }),
        ];
        for event in &events {
            t.apply(event, &mut out);
        }
        assert_eq!(out.len(), events.len());
        assert_eq!(t.tab_list, BTreeSet::from([10, 12]));
        assert_eq!(t.objectives, BTreeSet::from(["kills".to_string()]));
        assert_eq!(t.teams, BTreeSet::from(["red".to_string()]));
        assert_eq!(t.maps, BTreeSet::from([4]));
        assert!(t.titles && t.border && t.weather && !t.action_bar);

        out.clear();
        t.clear(&mut out);
        assert_eq!(
            ids(&out),
            [
                C::PlayerInfoRemove,
                C::SetObjective,
                C::SetPlayerTeam,
                C::ClearTitles,
                C::InitializeBorder,
                C::MapItemData,
                C::GameEvent,
                C::GameEvent,
                C::GameEvent,
            ]
            .map(|k| k.id())
        );
        let mut removed = PacketReader::new(out[0].clone());
        removed.read_var_int().unwrap();
        assert_eq!(removed.read_var_int().unwrap(), 2);
        assert_eq!(t, Transformer::new(7, 1));

        out.clear();
        t.clear(&mut out);
        assert!(out.is_empty());
    }

    #[tuff_core::watchdog]
    #[test]
    fn moves_are_synced_once_per_batch() {
        let mut t = Transformer::new(7, 1);
        t.recorded_id = Some(3);
        let mut out = Vec::new();
        for x in 0..10 {
            t.apply(
                &Event::Moved {
                    position: Some([f64::from(x), 64.0, 0.0]),
                    rotation: None,
                },
                &mut out,
            );
        }
        assert!(out.is_empty());
        t.finish(&mut out);
        assert_eq!(ids(&out), [C::AddEntity.id()]);
        assert_eq!(t.position, [9.0, 64.0, 0.0]);
        out.clear();
        t.finish(&mut out);
        assert!(out.is_empty());
    }

    fn login_event(id: i32) -> Event {
        raw(C::Login, |w| {
            w.write_int(id);
            w.write_bool(false);
            w.write_var_int(1);
            w.write_string("minecraft:overworld");
            w.write_var_int(20);
            w.write_var_int(8);
            w.write_var_int(8);
            w.write_bool(false);
            w.write_bool(true);
            w.write_bool(false);
            w.write_var_int(0);
            w.write_string("minecraft:overworld");
            w.write_long(0);
            w.write_byte(0);
            w.write_byte(-1);
            w.write_bool(false);
            w.write_bool(false);
            w.write_bool(false);
            w.write_var_int(0);
            w.write_var_int(63);
            w.write_bool(false);
        })
    }

    #[tuff_core::watchdog]
    #[test]
    fn rejoin_asks_for_terrain_and_replaces_the_viewer_position() {
        let mut t = Transformer::new(1, 5);
        let mut out = Vec::new();
        t.apply(&login_event(7), &mut out);
        t.apply(
            &Event::Moved {
                position: Some([100.0, 64.0, 100.0]),
                rotation: Some((0.0, 0.0)),
            },
            &mut out,
        );
        t.finish(&mut out);
        out.clear();
        t.apply(&login_event(7), &mut out);
        t.apply(
            &Event::Moved {
                position: Some([100.0, 64.0, 100.0]),
                rotation: Some((0.0, 0.0)),
            },
            &mut out,
        );
        t.finish(&mut out);
        let got = ids(&out);
        let at = |kind: C| got.iter().position(|&i| i == kind.id()).unwrap();
        assert!(at(C::Respawn) < at(C::GameEvent));
        assert!(at(C::GameEvent) < at(C::PlayerPosition));
    }

    #[tuff_core::watchdog]
    #[test]
    fn position_syncs_move_and_turn_held_entities() {
        use tuff_core::protocol::play::{EntityPositionSyncStepped, PositionStep};
        let mut t = Transformer::new(7, 1);
        let mut out = Vec::new();
        t.apply(
            &Event::Packet(encode(&AddEntity {
                entity_id: 5,
                uuid: 9,
                kind: player_kind(),
                position: [1.0, 2.0, 3.0],
                velocity: [0.0; 3],
                pitch: 0.0,
                yaw: 0.0,
                head_yaw: 0.0,
                data: 0,
            })),
            &mut out,
        );
        t.apply(
            &Event::Packet(encode(&EntityPositionSync {
                entity_id: 5,
                position: [4.0, 5.0, 6.0],
                yaw: 45.0,
                pitch: -10.0,
                on_ground: true,
            })),
            &mut out,
        );
        assert_eq!(t.located[&5].1, [4.0, 5.0, 6.0]);
        assert_eq!(t.facing[&5][..2], [45.0, -10.0]);
        let steps = [
            PositionStep {
                position: [7.0, 8.0, 9.0],
                tick_offset: 1,
            },
            PositionStep {
                position: [10.0, 11.0, 12.0],
                tick_offset: 2,
            },
        ];
        t.apply(
            &Event::Packet(encode(&EntityPositionSyncStepped {
                entity_id: 5,
                steps: &steps,
                yaw: 90.0,
                pitch: 20.0,
                on_ground: false,
            })),
            &mut out,
        );
        assert_eq!(t.located[&5].1, [10.0, 11.0, 12.0]);
        assert_eq!(t.facing[&5][..2], [90.0, 20.0]);
    }
}
