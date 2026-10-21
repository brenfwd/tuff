use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};

/// One request to the in-client hook. Wire form: `<id> <verb> <args>`.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    State,
    Block {
        x: i32,
        y: i32,
        z: i32,
    },
    Light {
        x: i32,
        y: i32,
        z: i32,
    },
    Entities {
        radius: f64,
    },
    Chat(String),
    Command(String),
    Look {
        yaw: f32,
        pitch: f32,
    },
    Hotbar(u8),
    Key {
        name: String,
        down: bool,
    },
    Use,
    Attack,
    Click {
        slot: i32,
        button: i32,
        mode: String,
    },
    Close,
    Respawn,
    Connect(String),
    Disconnect,
    Screenshot(String),
    WaitTicks(u32),
    ReleaseMouse,
    Eval(String),
    Set {
        path: String,
        value: String,
    },
    Hook {
        class: String,
        method: String,
    },
    HookHits {
        class: String,
        method: String,
    },
    HookAlter {
        class: String,
        method: String,
        spec: String,
    },
    Quit,
}

impl Command {
    pub fn verb(&self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Block { .. } => "block",
            Self::Light { .. } => "light",
            Self::Entities { .. } => "entities",
            Self::Chat(_) => "chat",
            Self::Command(_) => "command",
            Self::Look { .. } => "look",
            Self::Hotbar(_) => "hotbar",
            Self::Key { .. } => "key",
            Self::Use => "use",
            Self::Attack => "attack",
            Self::Click { .. } => "click",
            Self::Close => "close",
            Self::Respawn => "respawn",
            Self::Connect(_) => "connect",
            Self::Disconnect => "disconnect",
            Self::Screenshot(_) => "screenshot",
            Self::WaitTicks(_) => "wait_ticks",
            Self::ReleaseMouse => "release_mouse",
            Self::Eval(_) => "eval",
            Self::Set { .. } => "set",
            Self::Hook { .. } => "hook",
            Self::HookHits { .. } => "hook_hits",
            Self::HookAlter { .. } => "hook_alter",
            Self::Quit => "quit",
        }
    }

    fn args(&self) -> String {
        match self {
            Self::Block { x, y, z } | Self::Light { x, y, z } => format!("{x} {y} {z}"),
            Self::Entities { radius } => radius.to_string(),
            Self::Chat(text) | Self::Command(text) => single_line(text),
            Self::Look { yaw, pitch } => format!("{yaw} {pitch}"),
            Self::Hotbar(n) => n.to_string(),
            Self::Key { name, down } => format!("{name} {}", if *down { "down" } else { "up" }),
            Self::Click { slot, button, mode } => format!("{slot} {button} {mode}"),
            Self::Connect(addr) => addr.clone(),
            Self::Screenshot(name) => name.clone(),
            Self::WaitTicks(n) => n.to_string(),
            Self::Eval(path) => single_line(path),
            Self::Set { path, value } => format!("{} = {}", single_line(path), single_line(value)),
            Self::Hook { class, method } | Self::HookHits { class, method } => {
                format!("{class} {method}")
            }
            Self::HookAlter {
                class,
                method,
                spec,
            } => format!("{class} {method} {spec}"),
            _ => String::new(),
        }
    }

    pub fn encode(&self, id: u64) -> String {
        let args = self.args();
        if args.is_empty() {
            format!("{id} {}", self.verb())
        } else {
            format!("{id} {} {args}", self.verb())
        }
    }
}

fn single_line(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub id: u64,
    pub outcome: Result<String, String>,
}

pub fn parse_response(line: &str) -> Result<Response> {
    let mut parts = line.splitn(3, ' ');
    let id = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("bad response id in {line:?}"))?;
    let status = parts
        .next()
        .ok_or_else(|| anyhow!("no status in {line:?}"))?;
    let rest = parts.next().unwrap_or("").to_owned();
    let outcome = match status {
        "ok" => Ok(rest),
        "err" => Err(rest),
        other => bail!("unknown status {other:?} in {line:?}"),
    };
    Ok(Response { id, outcome })
}

/// Parses `key=value` pairs separated by single spaces.
pub fn parse_fields(payload: &str) -> BTreeMap<String, String> {
    payload
        .split(' ')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClientState {
    pub connected: bool,
    pub screen: String,
    pub title: String,
    pub position: Option<[f64; 3]>,
    pub yaw: Option<f32>,
    pub pitch: Option<f32>,
    pub dimension: Option<String>,
    pub game_mode: Option<String>,
    pub health: Option<f32>,
    pub chunks: Option<u32>,
    pub entities: Option<u32>,
    pub tick: Option<i64>,
    pub hardcore: Option<bool>,
}

fn field<T: std::str::FromStr>(fields: &BTreeMap<String, String>, key: &str) -> Option<T> {
    fields.get(key).and_then(|v| v.parse().ok())
}

impl ClientState {
    pub fn parse(payload: &str) -> Result<Self> {
        let f = parse_fields(payload);
        let position = match (field(&f, "x"), field(&f, "y"), field(&f, "z")) {
            (Some(x), Some(y), Some(z)) => Some([x, y, z]),
            _ => None,
        };
        Ok(Self {
            connected: field(&f, "connected")
                .ok_or_else(|| anyhow!("no connected in {payload:?}"))?,
            screen: f.get("screen").cloned().unwrap_or_default(),
            title: f.get("title").cloned().unwrap_or_default(),
            position,
            yaw: field(&f, "yaw"),
            pitch: field(&f, "pitch"),
            dimension: f.get("dim").cloned(),
            game_mode: f.get("mode").cloned(),
            health: field(&f, "health"),
            chunks: field(&f, "chunks"),
            entities: field(&f, "entities"),
            tick: field(&f, "tick"),
            hardcore: field(&f, "hardcore"),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EntityInfo {
    pub id: i32,
    pub kind: String,
    pub position: [f64; 3],
}

pub fn parse_entities(payload: &str) -> Result<Vec<EntityInfo>> {
    let list = parse_fields(payload).remove("list").unwrap_or_default();
    list.split(';')
        .filter(|s| !s.is_empty())
        .map(|item| {
            let p: Vec<&str> = item.split(':').collect();
            // the type is `namespace:path`, so id + 2 type parts + 3 coordinates
            if p.len() != 6 {
                bail!("bad entity {item:?}");
            }
            Ok(EntityInfo {
                id: p[0].parse()?,
                kind: format!("{}:{}", p[1], p[2]),
                position: [p[3].parse()?, p[4].parse()?, p[5].parse()?],
            })
        })
        .collect()
}

pub fn parse_block(payload: &str) -> Result<String> {
    parse_fields(payload)
        .remove("state")
        .ok_or_else(|| anyhow!("no state in {payload:?}"))
}

pub fn parse_light(payload: &str) -> Result<(u8, u8)> {
    let f = parse_fields(payload);
    match (field(&f, "block"), field(&f, "sky")) {
        (Some(b), Some(s)) => Ok((b, s)),
        _ => bail!("bad light payload {payload:?}"),
    }
}

/// `(before, after, thrown)` call counts of a hooked method.
pub fn parse_hook_hits(payload: &str) -> Result<(u64, u64, u64)> {
    let f = parse_fields(payload);
    match (field(&f, "before"), field(&f, "after"), field(&f, "thrown")) {
        (Some(b), Some(a), Some(t)) => Ok((b, a, t)),
        _ => bail!("bad hook_hits payload {payload:?}"),
    }
}
