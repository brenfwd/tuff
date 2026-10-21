//! What one logged action is, as hooks build it on the tick path.

use std::sync::Arc;

use tuff_core::world::dimension::DimensionId;
use tuff_nbt::Compound;
use tuff_vanilla_data::BlockState;

/// An action type. The numbers are stored, so they never change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    BlockBreak = 0,
    BlockPlace = 1,
    ContainerRemove = 2,
    ContainerAdd = 3,
    ItemRemove = 4,
    ItemAdd = 5,
    Kill = 6,
    Click = 7,
    Chat = 8,
    Command = 9,
    Login = 10,
    Logout = 11,
    Username = 12,
    Sign = 13,
    /// An action a feature registered; its name is the row's material.
    Custom = 14,
}

impl Kind {
    pub const ALL: [Kind; 15] = [
        Kind::BlockBreak,
        Kind::BlockPlace,
        Kind::ContainerRemove,
        Kind::ContainerAdd,
        Kind::ItemRemove,
        Kind::ItemAdd,
        Kind::Kill,
        Kind::Click,
        Kind::Chat,
        Kind::Command,
        Kind::Login,
        Kind::Logout,
        Kind::Username,
        Kind::Sign,
        Kind::Custom,
    ];

    pub fn code(self) -> i64 {
        self as i64
    }

    pub fn from_code(code: i64) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.code() == code)
    }

    pub fn bit(self) -> u32 {
        1 << (self as u32)
    }

    /// How lookups show it, the same word `a:` takes.
    pub fn name(self) -> &'static str {
        match self {
            Kind::BlockBreak => "-block",
            Kind::BlockPlace => "+block",
            Kind::ContainerRemove => "-container",
            Kind::ContainerAdd => "+container",
            Kind::ItemRemove => "-item",
            Kind::ItemAdd => "+item",
            Kind::Kill => "kill",
            Kind::Click => "click",
            Kind::Chat => "chat",
            Kind::Command => "command",
            Kind::Login => "+session",
            Kind::Logout => "-session",
            Kind::Username => "username",
            Kind::Sign => "sign",
            Kind::Custom => "custom",
        }
    }

    /// The config group that switches its logging on and off.
    pub fn group(self) -> Group {
        match self {
            Kind::BlockBreak | Kind::BlockPlace => Group::Block,
            Kind::ContainerRemove | Kind::ContainerAdd => Group::Container,
            Kind::ItemRemove | Kind::ItemAdd => Group::Item,
            Kind::Kill => Group::Kill,
            Kind::Click => Group::Click,
            Kind::Chat => Group::Chat,
            Kind::Command => Group::Command,
            Kind::Login | Kind::Logout => Group::Session,
            Kind::Username => Group::Username,
            Kind::Sign => Group::Sign,
            Kind::Custom => Group::Custom,
        }
    }

    pub fn is_block(self) -> bool {
        matches!(self, Kind::BlockBreak | Kind::BlockPlace)
    }

    pub fn is_kill(self) -> bool {
        self == Kind::Kill
    }

    pub fn is_container(self) -> bool {
        matches!(self, Kind::ContainerRemove | Kind::ContainerAdd)
    }
}

/// Action types switched on and off together (`tuff.features.audit-log.log.<name>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Block,
    Container,
    Item,
    Kill,
    Click,
    Chat,
    Command,
    Session,
    Username,
    Sign,
    Custom,
}

impl Group {
    pub const ALL: [Group; 11] = [
        Group::Block,
        Group::Container,
        Group::Item,
        Group::Kill,
        Group::Click,
        Group::Chat,
        Group::Command,
        Group::Session,
        Group::Username,
        Group::Sign,
        Group::Custom,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Group::Block => "block",
            Group::Container => "container",
            Group::Item => "item",
            Group::Kill => "kill",
            Group::Click => "click",
            Group::Chat => "chat",
            Group::Command => "command",
            Group::Session => "session",
            Group::Username => "username",
            Group::Sign => "sign",
            Group::Custom => "custom",
        }
    }

    pub fn kinds(self) -> impl Iterator<Item = Kind> {
        Kind::ALL
            .into_iter()
            .filter(move |kind| kind.group() == self)
    }

    pub fn mask(self) -> u32 {
        self.kinds().fold(0, |mask, kind| mask | kind.bit())
    }
}

/// Who did it: a player, or a natural cause or entity written `#name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Who {
    Player { uuid: u128, name: Arc<str> },
    Cause(String),
}

impl Who {
    /// A cause, `#` added when missing.
    pub fn cause(name: &str) -> Who {
        let name = name.strip_prefix("minecraft:").unwrap_or(name);
        if name.starts_with('#') {
            Who::Cause(name.to_owned())
        } else {
            Who::Cause(format!("#{name}"))
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Who::Player { name, .. } => name,
            Who::Cause(name) => name,
        }
    }
}

/// One action, as cheap to build as a hook can make it: block states are
/// copied ids, the block entity is moved in, and everything is encoded on
/// the writer thread.
#[derive(Clone, Debug)]
pub struct Record {
    /// Unix time in milliseconds.
    pub time: i64,
    pub who: Who,
    pub kind: Kind,
    pub world: DimensionId,
    pub pos: [i32; 3],
    /// What a block row's position held before and after.
    pub old: Option<BlockState>,
    pub new: Option<BlockState>,
    /// The block, item or entity id, when no block state names it.
    pub material: Option<String>,
    pub amount: i32,
    pub text: Option<String>,
    /// The block entity the old block had.
    pub data: Option<Compound>,
}

impl Record {
    pub fn new(time: i64, who: Who, kind: Kind, world: DimensionId, pos: [i32; 3]) -> Record {
        Record {
            time,
            who,
            kind,
            world,
            pos,
            old: None,
            new: None,
            material: None,
            amount: 1,
            text: None,
            data: None,
        }
    }

    /// The id lookups filter on with `i:` and `e:`.
    pub fn material_key(&self) -> Option<String> {
        if let Some(material) = &self.material {
            return Some(material.clone());
        }
        let state = match self.kind {
            Kind::BlockPlace => self.new.or(self.old),
            _ => self.old.or(self.new),
        }?;
        Some(state.block().name().to_owned())
    }
}

/// Now, in unix milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}
