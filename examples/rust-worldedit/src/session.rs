// SPDX-License-Identifier: MIT
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};

use crate::brush::Bind;
use crate::edit::{Clip, Entry, Job};
use crate::mask::Mask;
use crate::selection::Sel;

pub const DEFAULT_LIMIT: u64 = 100_000;
pub const MAX_LIMIT: u64 = 1_000_000;
pub const OVERWORLD: &str = "minecraft:overworld";

pub struct Player {
    pub dim: String,
    pub sel: Sel,
    pub limit: Option<u64>,
    pub clip: Option<Clip>,
    pub undo: VecDeque<Entry>,
    pub redo: VecDeque<Entry>,
    pub wand: bool,
    pub gmask: Option<Mask>,
    pub binds: BTreeMap<String, Bind>,
    pub range: i32,
    pub last: Option<String>,
    pub name: String,
    pub pending: Option<(bool, u32)>,
    pub timeout: Option<u32>,
    pub fast: bool,
    pub cui: bool,
    pub draw: bool,
    pub cui_sent: Vec<String>,
}

impl Default for Player {
    fn default() -> Self {
        Player {
            dim: String::new(),
            sel: Sel::default(),
            limit: None,
            clip: None,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            wand: true,
            gmask: None,
            binds: BTreeMap::new(),
            range: 300,
            last: None,
            name: String::new(),
            pending: None,
            timeout: None,
            fast: false,
            cui: false,
            draw: true,
            cui_sent: Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct Model {
    pub players: BTreeMap<u128, Player>,
    pub jobs: VecDeque<Job>,
    pub timer: bool,
}

thread_local! {
    static MODEL: RefCell<Model> = RefCell::new(Model::default());
}

pub fn model<R>(f: impl FnOnce(&mut Model) -> R) -> R {
    MODEL.with(|m| f(&mut m.borrow_mut()))
}

pub fn owner() -> Option<u128> {
    let player = tuff_plugin_sdk::world::acting::player()?;
    let id = player.uuid.as_u128();
    let fresh = model(|m| {
        let fresh = !m.players.contains_key(&id);
        m.players.entry(id).or_default().name = player.name;
        fresh
    });
    if fresh {
        crate::history::load(id);
    }
    Some(id)
}

pub fn tell(id: u128, text: &str) {
    let _ = tuff_plugin_sdk::world::PlayerExt::tell(
        &tuff_plugin_sdk::types::Player {
            uuid: tuff_plugin_sdk::types::Uuid::from_u128(id),
            name: String::new(),
        },
        text,
    );
}

pub fn say(text: impl AsRef<str>) -> i32 {
    tuff_plugin_sdk::Replier::acting().reply(text.as_ref()).ok();
    1
}

pub struct Here {
    pub block: [i32; 3],
    pub dim: String,
}

pub fn here() -> Option<Here> {
    let at = tuff_plugin_sdk::world::acting::location()?;
    Some(Here {
        block: [
            at.pos.x.floor() as i32,
            at.pos.y.floor() as i32,
            at.pos.z.floor() as i32,
        ],
        dim: at.dimension,
    })
}

pub fn look() -> Option<(f64, f64, [f64; 3])> {
    let (eye, rotation) = tuff_plugin_sdk::world::acting::eye()?;
    Some((
        rotation.yaw as f64,
        rotation.pitch as f64,
        [eye.x, eye.y, eye.z],
    ))
}
