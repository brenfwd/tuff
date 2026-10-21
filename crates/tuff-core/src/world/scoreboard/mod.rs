//! The scoreboard: objectives, the scores holders have in them, which
//! objectives show in which display slot, and teams. One actor owns it
//! ([`actor`]); every change goes through it, records the packets clients
//! need and marks the board for saving as `data/minecraft/scoreboard.dat`.
//!
//! Clients only learn the objectives shown in a display slot ("tracked"
//! ones) and their scores; teams are always sent.

pub mod actor;
pub mod teams;

use std::collections::HashMap;

use bytes::Bytes;
use indexmap::{IndexMap, IndexSet};

use crate::protocol::play::{
    ResetScore, SetDisplayObjective, SetObjective, SetPlayerTeam, SetScore, TeamAction,
};
use crate::world::framed;
use crate::world::stats::Stat;
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};

pub use actor::{ScoreboardHandle, start};
pub use teams::{DeathAudience, Member, Teams};

/// Names starting with this are hidden from the sidebar and never resolve
/// to a player.
pub const HIDDEN_PREFIX: &str = "#";
/// Every score holder in an objective, for commands taking `*`.
pub const WILDCARD: &str = "*";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TeamColor {
    Black,
    DarkBlue,
    DarkGreen,
    DarkAqua,
    DarkRed,
    DarkPurple,
    Gold,
    Gray,
    DarkGray,
    Blue,
    Green,
    Aqua,
    Red,
    LightPurple,
    Yellow,
    White,
}

impl TeamColor {
    pub const ALL: [Self; 16] = [
        Self::Black,
        Self::DarkBlue,
        Self::DarkGreen,
        Self::DarkAqua,
        Self::DarkRed,
        Self::DarkPurple,
        Self::Gold,
        Self::Gray,
        Self::DarkGray,
        Self::Blue,
        Self::Green,
        Self::Aqua,
        Self::Red,
        Self::LightPurple,
        Self::Yellow,
        Self::White,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Black => "black",
            Self::DarkBlue => "dark_blue",
            Self::DarkGreen => "dark_green",
            Self::DarkAqua => "dark_aqua",
            Self::DarkRed => "dark_red",
            Self::DarkPurple => "dark_purple",
            Self::Gold => "gold",
            Self::Gray => "gray",
            Self::DarkGray => "dark_gray",
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Aqua => "aqua",
            Self::Red => "red",
            Self::LightPurple => "light_purple",
            Self::Yellow => "yellow",
            Self::White => "white",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|color| color.name() == name)
    }

    pub fn rgb(self) -> [u8; 3] {
        match self {
            Self::Black => [0x00, 0x00, 0x00],
            Self::DarkBlue => [0x00, 0x00, 0xAA],
            Self::DarkGreen => [0x00, 0xAA, 0x00],
            Self::DarkAqua => [0x00, 0xAA, 0xAA],
            Self::DarkRed => [0xAA, 0x00, 0x00],
            Self::DarkPurple => [0xAA, 0x00, 0xAA],
            Self::Gold => [0xFF, 0xAA, 0x00],
            Self::Gray => [0xAA, 0xAA, 0xAA],
            Self::DarkGray => [0x55, 0x55, 0x55],
            Self::Blue => [0x55, 0x55, 0xFF],
            Self::Green => [0x55, 0xFF, 0x55],
            Self::Aqua => [0x55, 0xFF, 0xFF],
            Self::Red => [0xFF, 0x55, 0x55],
            Self::LightPurple => [0xFF, 0x55, 0xFF],
            Self::Yellow => [0xFF, 0xFF, 0x55],
            Self::White => [0xFF, 0xFF, 0xFF],
        }
    }
}

/// Where an objective can be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplaySlot {
    List,
    Sidebar,
    BelowName,
    /// The sidebar seen by members of teams of this color.
    Team(TeamColor),
}

impl DisplaySlot {
    pub const COUNT: usize = 19;

    pub fn all() -> impl Iterator<Item = Self> {
        [Self::List, Self::Sidebar, Self::BelowName]
            .into_iter()
            .chain(TeamColor::ALL.into_iter().map(Self::Team))
    }

    pub fn id(self) -> i32 {
        match self {
            Self::List => 0,
            Self::Sidebar => 1,
            Self::BelowName => 2,
            Self::Team(color) => 3 + color as i32,
        }
    }

    pub fn name(self) -> String {
        match self {
            Self::List => "list".into(),
            Self::Sidebar => "sidebar".into(),
            Self::BelowName => "below_name".into(),
            Self::Team(color) => format!("sidebar.team.{}", color.name()),
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "list" => Some(Self::List),
            "sidebar" => Some(Self::Sidebar),
            "below_name" => Some(Self::BelowName),
            _ => TeamColor::from_name(name.strip_prefix("sidebar.team.")?).map(Self::Team),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderType {
    #[default]
    Integer,
    Hearts,
}

impl RenderType {
    pub fn name(self) -> &'static str {
        match self {
            Self::Integer => "integer",
            Self::Hearts => "hearts",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "integer" => Some(Self::Integer),
            "hearts" => Some(Self::Hearts),
            _ => None,
        }
    }
}

/// How a score's number is drawn.
#[derive(Debug, Clone, PartialEq)]
pub enum NumberFormat {
    Blank,
    /// The number in this style (kept as its NBT form).
    Styled(Compound),
    /// This text instead of the number.
    Fixed(TextComponent),
}

impl NumberFormat {
    fn to_nbt(&self) -> Compound {
        match self {
            Self::Blank => Compound::new().with("type", "minecraft:blank"),
            Self::Styled(style) => {
                let mut out = Compound::new().with("type", "minecraft:styled");
                for (key, value) in style.iter() {
                    out.insert(key, value.clone());
                }
                out
            }
            Self::Fixed(text) => Compound::new()
                .with("type", "minecraft:fixed")
                .with("value", text.to_nbt()),
        }
    }

    fn from_nbt(tag: &Tag) -> Option<Self> {
        let nbt = tag.as_compound()?;
        let kind = nbt.get_str("type")?;
        match kind.strip_prefix("minecraft:").unwrap_or(kind) {
            "blank" => Some(Self::Blank),
            "styled" => {
                let mut style = nbt.clone();
                style.remove("type");
                Some(Self::Styled(style))
            }
            "fixed" => Some(Self::Fixed(TextComponent::from_nbt(nbt.get("value")?)?)),
            _ => None,
        }
    }
}

/// What an objective counts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Criterion {
    Dummy,
    Trigger,
    DeathCount,
    PlayerKillCount,
    TotalKillCount,
    Health,
    Food,
    Air,
    Armor,
    Xp,
    Level,
    TeamKill(TeamColor),
    KilledByTeam(TeamColor),
    Stat(Stat),
}

impl Criterion {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "dummy" => Self::Dummy,
            "trigger" => Self::Trigger,
            "deathCount" => Self::DeathCount,
            "playerKillCount" => Self::PlayerKillCount,
            "totalKillCount" => Self::TotalKillCount,
            "health" => Self::Health,
            "food" => Self::Food,
            "air" => Self::Air,
            "armor" => Self::Armor,
            "xp" => Self::Xp,
            "level" => Self::Level,
            _ => {
                if let Some(color) = name.strip_prefix("teamkill.") {
                    return TeamColor::from_name(color).map(Self::TeamKill);
                }
                if let Some(color) = name.strip_prefix("killedByTeam.") {
                    return TeamColor::from_name(color).map(Self::KilledByTeam);
                }
                return Stat::from_criterion(name).map(Self::Stat);
            }
        })
    }

    pub fn name(&self) -> String {
        match self {
            Self::Dummy => "dummy".into(),
            Self::Trigger => "trigger".into(),
            Self::DeathCount => "deathCount".into(),
            Self::PlayerKillCount => "playerKillCount".into(),
            Self::TotalKillCount => "totalKillCount".into(),
            Self::Health => "health".into(),
            Self::Food => "food".into(),
            Self::Air => "air".into(),
            Self::Armor => "armor".into(),
            Self::Xp => "xp".into(),
            Self::Level => "level".into(),
            Self::TeamKill(color) => format!("teamkill.{}", color.name()),
            Self::KilledByTeam(color) => format!("killedByTeam.{}", color.name()),
            Self::Stat(stat) => stat.criterion().unwrap_or_default(),
        }
    }

    /// Whether only the game, not commands, may change these scores.
    pub fn read_only(&self) -> bool {
        matches!(
            self,
            Self::Health | Self::Food | Self::Air | Self::Armor | Self::Xp | Self::Level
        )
    }

    pub fn default_render_type(&self) -> RenderType {
        if *self == Self::Health {
            RenderType::Hearts
        } else {
            RenderType::Integer
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Objective {
    pub name: String,
    pub criterion: Criterion,
    pub display_name: TextComponent,
    pub render_type: RenderType,
    pub display_auto_update: bool,
    pub number_format: Option<NumberFormat>,
}

impl Objective {
    /// The display name in brackets, showing the objective's name on hover.
    pub fn formatted_display_name(&self) -> TextComponent {
        TextComponent::translate(
            "chat.square_brackets",
            [self
                .display_name
                .clone()
                .hover_text(TextComponent::new(self.name.clone()))],
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub value: i32,
    /// Trigger objectives only accept `/trigger` while unlocked.
    pub locked: bool,
    pub display: Option<TextComponent>,
    pub number_format: Option<NumberFormat>,
}

impl Default for Score {
    fn default() -> Self {
        Self {
            value: 0,
            locked: true,
            display: None,
            number_format: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Always,
    Never,
    HideForOtherTeams,
    HideForOwnTeam,
}

impl Visibility {
    pub fn name(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Never => "never",
            Self::HideForOtherTeams => "hideForOtherTeams",
            Self::HideForOwnTeam => "hideForOwnTeam",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::Always,
            Self::Never,
            Self::HideForOtherTeams,
            Self::HideForOwnTeam,
        ]
        .into_iter()
        .find(|v| v.name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CollisionRule {
    #[default]
    Always,
    Never,
    PushOtherTeams,
    PushOwnTeam,
}

impl CollisionRule {
    pub fn name(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Never => "never",
            Self::PushOtherTeams => "pushOtherTeams",
            Self::PushOwnTeam => "pushOwnTeam",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::Always,
            Self::Never,
            Self::PushOtherTeams,
            Self::PushOwnTeam,
        ]
        .into_iter()
        .find(|v| v.name() == name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Team {
    pub name: String,
    pub display_name: TextComponent,
    pub color: Option<TeamColor>,
    pub friendly_fire: bool,
    pub see_friendly_invisibles: bool,
    pub prefix: TextComponent,
    pub suffix: TextComponent,
    pub name_tag_visibility: Visibility,
    pub death_message_visibility: Visibility,
    pub collision_rule: CollisionRule,
    pub members: IndexSet<String>,
}

impl Team {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            display_name: TextComponent::new(name),
            color: None,
            friendly_fire: true,
            see_friendly_invisibles: true,
            prefix: TextComponent::default(),
            suffix: TextComponent::default(),
            name_tag_visibility: Visibility::Always,
            death_message_visibility: Visibility::Always,
            collision_rule: CollisionRule::Always,
            members: IndexSet::new(),
        }
    }

    /// The option bits of Update Teams.
    pub fn options(&self) -> u8 {
        u8::from(self.friendly_fire) | u8::from(self.see_friendly_invisibles) << 1
    }

    /// The display name in brackets, colored like the team, inserting and
    /// showing the team's name.
    pub fn formatted_display_name(&self) -> TextComponent {
        let inner = self
            .display_name
            .clone()
            .insertion(self.name.clone())
            .hover_text(TextComponent::new(self.name.clone()));
        let out = TextComponent::translate("chat.square_brackets", [inner]);
        match self.color {
            Some(color) => out.color(color.name()),
            None => out,
        }
    }
}

/// How a role decorates a player's nametag: shown through a synthetic
/// team the scoreboard keeps only while the player is in no vanilla team.
#[derive(Debug, Clone, PartialEq)]
pub struct Nametag {
    pub prefix: TextComponent,
    pub suffix: TextComponent,
    pub color: Option<TeamColor>,
}

fn nametag_team_name(holder: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in holder.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    format!("tr{:014x}", hash & 0x00ff_ffff_ffff_ffff)
}

/// Who a score belongs to: a player's name, an entity's UUID string, or
/// any name; `display` is what auto-updating objectives show for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Holder {
    pub name: String,
    pub display: Option<TextComponent>,
}

impl Holder {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            display: None,
        }
    }

    /// How command feedback names the holder.
    pub fn feedback_name(&self) -> TextComponent {
        self.display
            .clone()
            .unwrap_or_else(|| TextComponent::new(self.name.clone()))
    }
}

#[derive(Debug, Default)]
pub struct Scoreboard {
    objectives: IndexMap<String, Objective>,
    scores: IndexMap<String, IndexMap<String, Score>>,
    display: [Option<String>; DisplaySlot::COUNT],
    teams: IndexMap<String, Team>,
    team_of: HashMap<String, String>,
    /// Objectives clients know, because a display slot shows them.
    tracked: IndexSet<String>,
    /// Packets for every player, in order, since the last drain.
    outbox: Vec<Bytes>,
    dirty: bool,
    /// Objectives came or went since the criteria were last read.
    objectives_changed: bool,
    /// Teams or their members changed since the teams were last read.
    teams_changed: bool,
    /// Role nametag decoration by player name; clients see it as a synthetic
    /// team, never stored, saved or read by gameplay.
    nametags: HashMap<String, Nametag>,
}

impl Scoreboard {
    pub fn objectives(&self) -> impl Iterator<Item = &Objective> {
        self.objectives.values()
    }

    pub fn objective(&self, name: &str) -> Option<&Objective> {
        self.objectives.get(name)
    }

    pub fn teams(&self) -> impl Iterator<Item = &Team> {
        self.teams.values()
    }

    pub fn team(&self, name: &str) -> Option<&Team> {
        self.teams.get(name)
    }

    /// The team `holder` belongs to.
    pub fn team_of(&self, holder: &str) -> Option<&Team> {
        self.teams.get(self.team_of.get(holder)?)
    }

    pub fn display_objective(&self, slot: DisplaySlot) -> Option<&Objective> {
        self.objectives
            .get(self.display[slot.id() as usize].as_deref()?)
    }

    pub fn score(&self, holder: &str, objective: &str) -> Option<&Score> {
        self.scores.get(holder)?.get(objective)
    }

    /// Every holder with a score in any objective.
    pub fn holders(&self) -> impl Iterator<Item = &str> {
        self.scores.keys().map(String::as_str)
    }

    /// Every holder with a score in `objective`.
    pub fn holders_in(&self, objective: &str) -> Vec<String> {
        self.scores
            .iter()
            .filter(|(_, scores)| scores.contains_key(objective))
            .map(|(holder, _)| holder.clone())
            .collect()
    }

    /// `holder`'s scores, by objective.
    pub fn scores_of(&self, holder: &str) -> Vec<(&Objective, i32)> {
        let Some(scores) = self.scores.get(holder) else {
            return Vec::new();
        };
        scores
            .iter()
            .filter_map(|(name, score)| Some((self.objectives.get(name)?, score.value)))
            .collect()
    }

    /// Objectives with this criterion.
    pub fn objectives_with(&self, criterion: &Criterion) -> Vec<String> {
        self.objectives
            .values()
            .filter(|o| o.criterion == *criterion)
            .map(|o| o.name.clone())
            .collect()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// The packets to send everyone since the last call.
    pub fn take_outbox(&mut self) -> Vec<Bytes> {
        std::mem::take(&mut self.outbox)
    }

    fn broadcast<P: crate::protocol::Encode>(&mut self, packet: &P) {
        self.outbox.push(framed(packet));
    }

    fn objective_packet(objective: &Objective, method: u8) -> Bytes {
        framed(&SetObjective {
            name: &objective.name,
            method,
            display_name: &objective.display_name,
            render_type: objective.render_type,
            number_format: objective.number_format.as_ref(),
        })
    }

    /// The synthetic team showing `holder`'s role nametag: only while it
    /// has one and is in no vanilla team, so vanilla teams win.
    fn nametag_team(&self, holder: &str) -> Option<Team> {
        if self.team_of.contains_key(holder) {
            return None;
        }
        let tag = self.nametags.get(holder)?;
        let mut team = Team::new(&nametag_team_name(holder));
        team.prefix = tag.prefix.clone();
        team.suffix = tag.suffix.clone();
        team.color = tag.color;
        team.members.insert(holder.to_owned());
        Some(team)
    }

    fn add_nametag_team(&mut self, holder: &str) {
        if let Some(team) = self.nametag_team(holder) {
            self.broadcast(&SetPlayerTeam {
                name: &team.name,
                action: TeamAction::Add(&team),
            });
        }
    }

    fn remove_nametag_team(&mut self, holder: &str) {
        if self.nametags.contains_key(holder) && !self.team_of.contains_key(holder) {
            self.broadcast(&SetPlayerTeam {
                name: &nametag_team_name(holder),
                action: TeamAction::Remove,
            });
        }
    }

    /// Sets or clears the role nametag of the player named `holder`,
    /// telling clients unless a vanilla team already decorates the name.
    pub fn set_nametag(&mut self, holder: &str, tag: Option<Nametag>) {
        if self.nametags.get(holder) == tag.as_ref() {
            return;
        }
        let was_shown = self.nametag_team(holder).is_some();
        match tag {
            Some(tag) => {
                self.nametags.insert(holder.to_owned(), tag);
            }
            None => {
                self.nametags.remove(holder);
            }
        }
        match self.nametag_team(holder) {
            Some(team) if was_shown => self.broadcast(&SetPlayerTeam {
                name: &team.name,
                action: TeamAction::Change(&team),
            }),
            Some(_) => self.add_nametag_team(holder),
            None if was_shown => self.broadcast(&SetPlayerTeam {
                name: &nametag_team_name(holder),
                action: TeamAction::Remove,
            }),
            None => {}
        }
    }

    /// Everything a player joining needs: every team, then each displayed
    /// objective with its slots and scores.
    pub fn join_packets(&self) -> Vec<Bytes> {
        let mut out: Vec<Bytes> = self
            .teams
            .values()
            .map(|team| {
                framed(&SetPlayerTeam {
                    name: &team.name,
                    action: TeamAction::Add(team),
                })
            })
            .collect();
        for holder in self.nametags.keys() {
            if let Some(team) = self.nametag_team(holder) {
                out.push(framed(&SetPlayerTeam {
                    name: &team.name,
                    action: TeamAction::Add(&team),
                }));
            }
        }
        let mut sent = IndexSet::new();
        for slot in DisplaySlot::all() {
            if let Some(name) = &self.display[slot.id() as usize]
                && sent.insert(name.clone())
            {
                out.extend(self.start_tracking_packets(name));
            }
        }
        out
    }

    fn start_tracking_packets(&self, name: &str) -> Vec<Bytes> {
        let Some(objective) = self.objectives.get(name) else {
            return Vec::new();
        };
        let mut out = vec![Self::objective_packet(objective, 0)];
        for slot in DisplaySlot::all() {
            if self.display[slot.id() as usize].as_deref() == Some(name) {
                out.push(framed(&SetDisplayObjective {
                    slot: slot.id(),
                    name,
                }));
            }
        }
        for (holder, scores) in &self.scores {
            if let Some(score) = scores.get(name) {
                out.push(framed(&SetScore {
                    holder,
                    objective: name,
                    value: score.value,
                    display: score.display.as_ref(),
                    number_format: score.number_format.as_ref(),
                }));
            }
        }
        out
    }

    fn start_tracking(&mut self, name: &str) {
        let packets = self.start_tracking_packets(name);
        self.outbox.extend(packets);
        self.tracked.insert(name.to_owned());
    }

    fn stop_tracking(&mut self, name: &str) {
        if let Some(objective) = self.objectives.get(name) {
            let packet = Self::objective_packet(objective, 1);
            self.outbox.push(packet);
        }
        for slot in DisplaySlot::all() {
            if self.display[slot.id() as usize].as_deref() == Some(name) {
                self.broadcast(&SetDisplayObjective {
                    slot: slot.id(),
                    name,
                });
            }
        }
        self.tracked.shift_remove(name);
    }

    fn display_count(&self, name: &str) -> usize {
        self.display
            .iter()
            .filter(|shown| shown.as_deref() == Some(name))
            .count()
    }

    /// Adds an objective; false if the name is taken.
    pub fn add_objective(&mut self, objective: Objective) -> bool {
        if self.objectives.contains_key(&objective.name) {
            return false;
        }
        self.objectives.insert(objective.name.clone(), objective);
        self.dirty = true;
        self.objectives_changed = true;
        true
    }

    pub fn remove_objective(&mut self, name: &str) {
        if !self.objectives.contains_key(name) {
            return;
        }
        for slot in DisplaySlot::all() {
            if self.display[slot.id() as usize].as_deref() == Some(name) {
                self.set_display(slot, None);
            }
        }
        for scores in self.scores.values_mut() {
            scores.shift_remove(name);
        }
        if self.tracked.contains(name) {
            self.stop_tracking(name);
        }
        self.objectives.shift_remove(name);
        self.dirty = true;
        self.objectives_changed = true;
    }

    /// Changes an objective, telling clients if they know it.
    pub fn modify_objective(&mut self, name: &str, change: impl FnOnce(&mut Objective)) {
        let Some(objective) = self.objectives.get_mut(name) else {
            return;
        };
        change(objective);
        if self.tracked.contains(name) {
            let packet = Self::objective_packet(&self.objectives[name], 2);
            self.outbox.push(packet);
        }
        self.dirty = true;
    }

    pub fn set_display(&mut self, slot: DisplaySlot, objective: Option<&str>) {
        let index = slot.id() as usize;
        let old = self.display[index].take();
        self.display[index] = objective.map(str::to_owned);
        if let Some(old) = old.as_deref()
            && Some(old) != objective
        {
            if self.display_count(old) > 0 {
                self.broadcast(&SetDisplayObjective {
                    slot: slot.id(),
                    name: objective.unwrap_or(""),
                });
            } else {
                self.stop_tracking(old);
            }
        }
        if let Some(name) = objective {
            if self.tracked.contains(name) {
                self.broadcast(&SetDisplayObjective {
                    slot: slot.id(),
                    name,
                });
            } else {
                self.start_tracking(name);
            }
        }
        self.dirty = true;
    }

    fn send_score(&mut self, holder: &str, objective: &str) {
        if !self.tracked.contains(objective) {
            return;
        }
        let Some(score) = self.scores.get(holder).and_then(|s| s.get(objective)) else {
            return;
        };
        let packet = framed(&SetScore {
            holder,
            objective,
            value: score.value,
            display: score.display.as_ref(),
            number_format: score.number_format.as_ref(),
        });
        self.outbox.push(packet);
    }

    /// The score of `holder`, made (locked, 0) if missing; true if made.
    fn entry(&mut self, holder: &str, objective: &str) -> (&mut Score, bool) {
        let scores = self.scores.entry(holder.to_owned()).or_default();
        let created = !scores.contains_key(objective);
        (scores.entry(objective.to_owned()).or_default(), created)
    }

    /// Makes the score if missing (unsent until it changes) and returns
    /// its value.
    pub fn get_or_create(&mut self, holder: &str, objective: &str) -> i32 {
        if !self.objectives.contains_key(objective) {
            return 0;
        }
        let (score, created) = self.entry(holder, objective);
        let value = score.value;
        if created {
            self.dirty = true;
        }
        value
    }

    /// Sets a score as the game's score access does: an auto-updating
    /// objective also refreshes the shown name, and clients hear of any
    /// change (a new score counts as one).
    pub fn set(&mut self, holder: &Holder, objective: &str, value: i32) {
        let Some(auto) = self
            .objectives
            .get(objective)
            .map(|o| o.display_auto_update)
        else {
            return;
        };
        let (score, created) = self.entry(&holder.name, objective);
        let mut changed = created;
        if auto
            && let Some(display) = &holder.display
            && score.display.as_ref() != Some(display)
        {
            score.display = Some(display.clone());
            changed = true;
        }
        if score.value != value {
            score.value = value;
            changed = true;
        }
        if changed {
            self.send_score(&holder.name, objective);
        }
        self.dirty = true;
    }

    pub fn add(&mut self, holder: &Holder, objective: &str, amount: i32) -> i32 {
        let value = self
            .get_or_create(&holder.name, objective)
            .wrapping_add(amount);
        self.set(holder, objective, value);
        value
    }

    pub fn set_score_display(
        &mut self,
        holder: &str,
        objective: &str,
        display: Option<TextComponent>,
    ) {
        if !self.objectives.contains_key(objective) {
            return;
        }
        let (score, created) = self.entry(holder, objective);
        if created || score.display != display {
            score.display = display;
            self.send_score(holder, objective);
        }
        self.dirty = true;
    }

    pub fn set_score_format(
        &mut self,
        holder: &str,
        objective: &str,
        format: Option<NumberFormat>,
    ) {
        if !self.objectives.contains_key(objective) {
            return;
        }
        let (score, _) = self.entry(holder, objective);
        score.number_format = format;
        self.send_score(holder, objective);
        self.dirty = true;
    }

    /// Locks or unlocks a score, making it if missing.
    pub fn set_locked(&mut self, holder: &str, objective: &str, locked: bool) {
        if !self.objectives.contains_key(objective) {
            return;
        }
        let (score, created) = self.entry(holder, objective);
        score.locked = locked;
        if created {
            self.send_score(holder, objective);
        }
        self.dirty = true;
    }

    pub fn reset_all(&mut self, holder: &str) {
        if self.scores.shift_remove(holder).is_some() {
            if !self.tracked.is_empty() {
                self.broadcast(&ResetScore {
                    holder,
                    objective: None,
                });
            }
            self.dirty = true;
        }
    }

    pub fn reset(&mut self, holder: &str, objective: &str) {
        let Some(scores) = self.scores.get_mut(holder) else {
            return;
        };
        let removed = scores.shift_remove(objective).is_some();
        if scores.is_empty() {
            self.reset_all(holder);
            return;
        }
        if removed {
            if self.tracked.contains(objective) {
                self.broadcast(&ResetScore {
                    holder,
                    objective: Some(objective),
                });
            }
            self.dirty = true;
        }
    }

    /// Adds a team; false if the name is taken.
    pub fn add_team(&mut self, team: Team) -> bool {
        if self.teams.contains_key(&team.name) {
            return false;
        }
        self.broadcast(&SetPlayerTeam {
            name: &team.name,
            action: TeamAction::Add(&team),
        });
        for member in &team.members {
            self.remove_nametag_team(member);
            self.team_of.insert(member.clone(), team.name.clone());
        }
        self.teams.insert(team.name.clone(), team);
        self.dirty = true;
        self.teams_changed = true;
        true
    }

    pub fn remove_team(&mut self, name: &str) {
        let Some(team) = self.teams.shift_remove(name) else {
            return;
        };
        for member in &team.members {
            self.team_of.remove(member);
        }
        self.broadcast(&SetPlayerTeam {
            name,
            action: TeamAction::Remove,
        });
        for member in &team.members {
            self.add_nametag_team(member);
        }
        self.dirty = true;
        self.teams_changed = true;
    }

    /// Changes a team's settings, telling clients.
    pub fn modify_team(&mut self, name: &str, change: impl FnOnce(&mut Team)) {
        let Some(team) = self.teams.get_mut(name) else {
            return;
        };
        change(team);
        let packet = framed(&SetPlayerTeam {
            name,
            action: TeamAction::Change(&self.teams[name]),
        });
        self.outbox.push(packet);
        self.dirty = true;
        self.teams_changed = true;
    }

    /// Puts `holder` in `team`, leaving any other; false if it was already
    /// a member.
    pub fn join_team(&mut self, holder: &str, team: &str) -> bool {
        if !self.teams.contains_key(team) {
            return false;
        }
        if self.team_of.contains_key(holder) {
            self.leave_team_keeping_tag_hidden(holder);
        } else {
            self.remove_nametag_team(holder);
        }
        self.team_of.insert(holder.to_owned(), team.to_owned());
        let added = self.teams[team].members.insert(holder.to_owned());
        if added {
            self.broadcast(&SetPlayerTeam {
                name: team,
                action: TeamAction::Join(&[holder.to_owned()]),
            });
            self.dirty = true;
            self.teams_changed = true;
        }
        added
    }

    /// Takes `holder` out of its team; false if it had none.
    pub fn leave_team(&mut self, holder: &str) -> bool {
        let left = self.leave_team_keeping_tag_hidden(holder);
        if left {
            self.add_nametag_team(holder);
        }
        left
    }

    fn leave_team_keeping_tag_hidden(&mut self, holder: &str) -> bool {
        let Some(team) = self.team_of.remove(holder) else {
            return false;
        };
        if let Some(entry) = self.teams.get_mut(&team) {
            entry.members.shift_remove(holder);
        }
        self.broadcast(&SetPlayerTeam {
            name: &team,
            action: TeamAction::Leave(&[holder.to_owned()]),
        });
        self.dirty = true;
        self.teams_changed = true;
        true
    }

    /// The teams as gameplay reads them, if they changed since last asked.
    pub fn take_teams(&mut self) -> Option<Teams> {
        std::mem::take(&mut self.teams_changed).then(|| Teams::new(self.teams.values()))
    }

    /// Whether objectives came or went since last asked.
    pub fn take_objectives_changed(&mut self) -> bool {
        std::mem::take(&mut self.objectives_changed)
    }

    /// Every criterion some objective counts.
    pub fn criteria(&self) -> std::collections::HashSet<Criterion> {
        self.objectives
            .values()
            .map(|o| o.criterion.clone())
            .collect()
    }

    /// Adds `amount` to `holder`'s score in every objective counting
    /// `criterion`, read-only ones included.
    pub fn add_for(&mut self, criterion: &Criterion, holder: &Holder, amount: i32) {
        for objective in self.objectives_with(criterion) {
            self.add(holder, &objective, amount);
        }
    }

    /// Sets `holder`'s score in every objective counting `criterion`.
    pub fn set_for(&mut self, criterion: &Criterion, holder: &Holder, value: i32) {
        for objective in self.objectives_with(criterion) {
            self.set(holder, &objective, value);
        }
    }

    /// A player killed `victim`: kill counts for the killer, and the team
    /// color criteria for both sides.
    pub fn award_kill(&mut self, killer: &Holder, victim: &Holder, victim_is_player: bool) {
        if killer.name == victim.name {
            return;
        }
        self.add_for(&Criterion::TotalKillCount, killer, 1);
        if victim_is_player {
            self.add_for(&Criterion::PlayerKillCount, killer, 1);
        }
        if let Some(color) = self.team_of(&victim.name).and_then(|t| t.color) {
            self.add_for(&Criterion::TeamKill(color), killer, 1);
        }
        if let Some(color) = self.team_of(&killer.name).and_then(|t| t.color) {
            self.add_for(&Criterion::KilledByTeam(color), victim, 1);
        }
    }

    /// The saved form, `{Objectives, PlayerScores, DisplaySlots, Teams}`.
    pub fn save(&mut self) -> Compound {
        self.dirty = false;
        let objectives: Vec<Tag> = self
            .objectives
            .values()
            .map(|o| {
                let mut out = Compound::new().with("Name", o.name.clone());
                if o.criterion != Criterion::Dummy {
                    out.insert("CriteriaName", o.criterion.name());
                }
                out.insert("DisplayName", o.display_name.to_nbt());
                if o.render_type != RenderType::Integer {
                    out.insert("RenderType", o.render_type.name());
                }
                if o.display_auto_update {
                    out.insert("display_auto_update", true);
                }
                if let Some(format) = &o.number_format {
                    out.insert("format", format.to_nbt());
                }
                Tag::Compound(out)
            })
            .collect();
        let mut scores = Vec::new();
        for (holder, entries) in &self.scores {
            for (objective, score) in entries {
                let mut out = Compound::new()
                    .with("Name", holder.clone())
                    .with("Objective", objective.clone());
                if score.value != 0 {
                    out.insert("Score", score.value);
                }
                if score.locked {
                    out.insert("Locked", true);
                }
                if let Some(display) = &score.display {
                    out.insert("display", display.to_nbt());
                }
                if let Some(format) = &score.number_format {
                    out.insert("format", format.to_nbt());
                }
                scores.push(Tag::Compound(out));
            }
        }
        let mut slots = Compound::new();
        for slot in DisplaySlot::all() {
            if let Some(name) = &self.display[slot.id() as usize] {
                slots.insert(slot.name(), name.clone());
            }
        }
        let teams: Vec<Tag> = self
            .teams
            .values()
            .map(|t| {
                let mut out = Compound::new()
                    .with("Name", t.name.clone())
                    .with("DisplayName", t.display_name.to_nbt());
                if let Some(color) = t.color {
                    out.insert("TeamColor", color.name());
                }
                if !t.friendly_fire {
                    out.insert("AllowFriendlyFire", false);
                }
                if !t.see_friendly_invisibles {
                    out.insert("SeeFriendlyInvisibles", false);
                }
                if t.prefix != TextComponent::default() {
                    out.insert("MemberNamePrefix", t.prefix.to_nbt());
                }
                if t.suffix != TextComponent::default() {
                    out.insert("MemberNameSuffix", t.suffix.to_nbt());
                }
                if t.name_tag_visibility != Visibility::Always {
                    out.insert("NameTagVisibility", t.name_tag_visibility.name());
                }
                if t.death_message_visibility != Visibility::Always {
                    out.insert("DeathMessageVisibility", t.death_message_visibility.name());
                }
                if t.collision_rule != CollisionRule::Always {
                    out.insert("CollisionRule", t.collision_rule.name());
                }
                if !t.members.is_empty() {
                    out.insert(
                        "Players",
                        Tag::List(t.members.iter().map(|m| Tag::String(m.clone())).collect()),
                    );
                }
                Tag::Compound(out)
            })
            .collect();
        let mut out = Compound::new();
        if !objectives.is_empty() {
            out.insert("Objectives", Tag::List(objectives));
        }
        if !scores.is_empty() {
            out.insert("PlayerScores", Tag::List(scores));
        }
        if !slots.is_empty() {
            out.insert("DisplaySlots", slots);
        }
        if !teams.is_empty() {
            out.insert("Teams", Tag::List(teams));
        }
        out
    }

    /// Reads the saved form; entries that do not parse are skipped, as the
    /// game skips scores of unknown objectives.
    pub fn load(data: &Compound) -> Self {
        let mut board = Self::default();
        let text = |tag: Option<&Tag>| tag.and_then(TextComponent::from_nbt);
        for tag in data.get_list("Objectives").unwrap_or_default() {
            let Some(nbt) = tag.as_compound() else {
                continue;
            };
            let Some(name) = nbt.get_str("Name") else {
                continue;
            };
            let criterion = match nbt.get_str("CriteriaName") {
                Some(criteria) => match Criterion::parse(criteria) {
                    Some(criterion) => criterion,
                    None => continue,
                },
                None => Criterion::Dummy,
            };
            board.objectives.insert(
                name.to_owned(),
                Objective {
                    name: name.to_owned(),
                    display_name: text(nbt.get("DisplayName"))
                        .unwrap_or_else(|| TextComponent::new(name)),
                    render_type: nbt
                        .get_str("RenderType")
                        .and_then(RenderType::from_name)
                        .unwrap_or_default(),
                    display_auto_update: nbt.get_bool("display_auto_update").unwrap_or(false),
                    number_format: nbt.get("format").and_then(NumberFormat::from_nbt),
                    criterion,
                },
            );
        }
        for tag in data.get_list("PlayerScores").unwrap_or_default() {
            let Some(nbt) = tag.as_compound() else {
                continue;
            };
            let (Some(holder), Some(objective)) = (nbt.get_str("Name"), nbt.get_str("Objective"))
            else {
                continue;
            };
            if !board.objectives.contains_key(objective) {
                continue;
            }
            board.scores.entry(holder.to_owned()).or_default().insert(
                objective.to_owned(),
                Score {
                    value: nbt.get_int("Score").unwrap_or(0),
                    locked: nbt.get_bool("Locked").unwrap_or(false),
                    display: text(nbt.get("display")),
                    number_format: nbt.get("format").and_then(NumberFormat::from_nbt),
                },
            );
        }
        if let Some(slots) = data.get_compound("DisplaySlots") {
            for (slot, name) in slots.iter() {
                if let (Some(slot), Some(name)) = (DisplaySlot::from_name(slot), name.as_str())
                    && board.objectives.contains_key(name)
                {
                    board.display[slot.id() as usize] = Some(name.to_owned());
                    board.tracked.insert(name.to_owned());
                }
            }
        }
        for tag in data.get_list("Teams").unwrap_or_default() {
            let Some(nbt) = tag.as_compound() else {
                continue;
            };
            let Some(name) = nbt.get_str("Name") else {
                continue;
            };
            let mut team = Team::new(name);
            if let Some(display) = text(nbt.get("DisplayName")) {
                team.display_name = display;
            }
            team.color = nbt.get_str("TeamColor").and_then(TeamColor::from_name);
            team.friendly_fire = nbt.get_bool("AllowFriendlyFire").unwrap_or(true);
            team.see_friendly_invisibles = nbt.get_bool("SeeFriendlyInvisibles").unwrap_or(true);
            team.prefix = text(nbt.get("MemberNamePrefix")).unwrap_or_default();
            team.suffix = text(nbt.get("MemberNameSuffix")).unwrap_or_default();
            team.name_tag_visibility = nbt
                .get_str("NameTagVisibility")
                .and_then(Visibility::from_name)
                .unwrap_or_default();
            team.death_message_visibility = nbt
                .get_str("DeathMessageVisibility")
                .and_then(Visibility::from_name)
                .unwrap_or_default();
            team.collision_rule = nbt
                .get_str("CollisionRule")
                .and_then(CollisionRule::from_name)
                .unwrap_or_default();
            for member in nbt.get_list("Players").unwrap_or_default() {
                if let Some(member) = member.as_str() {
                    if let Some(old) = board.team_of.insert(member.to_owned(), name.to_owned())
                        && let Some(old) = board.teams.get_mut(&old)
                    {
                        old.members.shift_remove(member);
                    }
                    team.members.insert(member.to_owned());
                }
            }
            board.teams.insert(name.to_owned(), team);
        }
        board
    }
}

#[cfg(test)]
mod tests;
