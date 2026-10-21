//! Target selectors (`@p`, `@a[distance=..5]`, a player name, a UUID): how
//! they are read and how they pick from the entities the server knows.

use rand::Rng;
use rand::seq::SliceRandom;
use tuff_storage::admin::{format_uuid, parse_uuid_text};
use tuff_vanilla_data::registry::EntityType;

use crate::protocol::play::GameMode;
use crate::world::dimension::DimensionId;

use super::args::Resources;
use super::error::CommandError;
use super::range::{self, Bounds};
use super::reader::Reader;
use tuff_nbt::{Compound, Tag};

const MAX_NAME_LENGTH: usize = 16;
pub const SELECTOR_START: char = '@';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Arbitrary,
    Nearest,
    Furthest,
    Random,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeTest {
    Kind(EntityType),
    /// An entity type tag by name, with the built-in pack's members until
    /// [`Selector::with_tags`] reads the tags in effect.
    Tag(String, tuff_vanilla_data::Frozen<[EntityType]>),
    /// A type a feature registered, by its saved id.
    Registered(String),
}

impl TypeTest {
    fn matches(&self, candidate: &Candidate) -> bool {
        match self {
            Self::Kind(expected) => *expected == candidate.kind && candidate.registered.is_none(),
            Self::Tag(_, kinds) => {
                kinds.contains(&candidate.kind) && candidate.registered.is_none()
            }
            Self::Registered(id) => candidate.registered.as_deref() == Some(id.as_str()),
        }
    }
}

/// An entity as the selector sees it, gathered from wherever it lives.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: i32,
    pub uuid: u128,
    pub name: String,
    pub kind: EntityType,
    pub dimension: DimensionId,
    pub position: [f64; 3],
    pub rotation: [f32; 2],
    pub bounds: [f64; 6],
    pub alive: bool,
    /// Players only.
    pub level: Option<i32>,
    pub game_mode: Option<GameMode>,
    pub tags: Vec<String>,
    /// Players only: their advancement progress.
    pub advancements: Option<crate::world::advancement::View>,
    /// The saved id of a feature-registered type sent to clients as `kind`.
    pub registered: Option<String>,
    /// The team, when the selector tests teams.
    pub team: Option<String>,
    /// Scores in the objectives the selector tests.
    pub scores: Vec<(String, i32)>,
    /// The saved form, when the selector tests NBT.
    pub nbt: Option<Compound>,
    /// Whether each of the selector's `predicate=` options passed, in
    /// order; filled in only for candidates its other options let through.
    pub predicates: Vec<bool>,
}

impl Candidate {
    pub fn is_player(&self) -> bool {
        self.kind == EntityType::Player
    }

    /// The name scores are kept under: a player's name, else the UUID.
    pub fn score_holder(&self) -> String {
        if self.is_player() {
            self.name.clone()
        } else {
            format_uuid(self.uuid)
        }
    }
}

/// The source a selector is resolved against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SelectorSource {
    pub position: [f64; 3],
    pub dimension: DimensionId,
    /// The entity running the command, for `@s`.
    pub entity: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    pub max_results: usize,
    pub includes_entities: bool,
    pub world_limited: bool,
    pub current_entity: bool,
    pub player_name: Option<String>,
    pub uuid: Option<u128>,
    pub order: Order,
    pub distance: Option<Bounds<f64>>,
    pub level: Option<Bounds<i32>>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
    pub dx: Option<f64>,
    pub dy: Option<f64>,
    pub dz: Option<f64>,
    pub x_rotation: Option<Bounds<f32>>,
    pub y_rotation: Option<Bounds<f32>>,
    /// `(value, inverted)` filters.
    pub names: Vec<(String, bool)>,
    pub game_modes: Vec<(GameMode, bool)>,
    pub types: Vec<(TypeTest, bool)>,
    pub tags: Vec<(String, bool)>,
    pub only_type: Option<EntityType>,
    pub only_alive: bool,
    /// `advancements={...}`: each advancement's expected state.
    pub advancements: Option<Vec<(String, AdvancementCheck)>>,
    /// `team=`: the team name (empty for none) and whether negated.
    pub teams: Vec<(String, bool)>,
    /// `scores={...}`: each objective's accepted range.
    pub scores: Vec<(String, Bounds<i32>)>,
    pub nbt: Vec<(Compound, bool)>,
    /// `predicate=`: stored predicate ids and whether negated.
    pub predicates: Vec<(String, bool)>,
    /// Whether the text used `@`, which needs the selector permission.
    pub uses_selector: bool,
}

impl Selector {
    /// A copy with its type tags' members read from `tags` (data packs may
    /// change them), or `None` when it names no tag.
    pub fn with_tags(&self, tags: &tuff_datapack::tags::TagSet) -> Option<Self> {
        if !self
            .types
            .iter()
            .any(|(test, _)| matches!(test, TypeTest::Tag(..)))
        {
            return None;
        }
        let mut selector = self.clone();
        for (test, _) in &mut selector.types {
            if let TypeTest::Tag(name, kinds) = test {
                *kinds = tags.typed::<EntityType>(name).unwrap_or_default().into();
            }
        }
        Some(selector)
    }

    fn new() -> Self {
        Self {
            max_results: 1,
            includes_entities: false,
            world_limited: false,
            current_entity: false,
            player_name: None,
            uuid: None,
            order: Order::Arbitrary,
            distance: None,
            level: None,
            x: None,
            y: None,
            z: None,
            dx: None,
            dy: None,
            dz: None,
            x_rotation: None,
            y_rotation: None,
            names: Vec::new(),
            game_modes: Vec::new(),
            types: Vec::new(),
            tags: Vec::new(),
            only_type: None,
            only_alive: false,
            advancements: None,
            teams: Vec::new(),
            scores: Vec::new(),
            nbt: Vec::new(),
            predicates: Vec::new(),
            uses_selector: false,
        }
    }

    /// Whether picking needs candidates' teams or scores.
    pub fn needs_scoreboard(&self) -> bool {
        !self.teams.is_empty() || !self.scores.is_empty()
    }

    /// Whether picking needs candidates' saved forms.
    pub fn needs_nbt(&self) -> bool {
        !self.nbt.is_empty()
    }

    /// Parses a selector or name; `allow_selectors` is false for sources
    /// without the selector permission.
    pub fn parse(reader: &mut Reader<'_>, allow_selectors: bool) -> Result<Self, CommandError> {
        Self::parse_with(reader, allow_selectors, &Resources::default())
    }

    /// [`Self::parse`], also accepting the entity types in `resources`.
    pub fn parse_with(
        reader: &mut Reader<'_>,
        allow_selectors: bool,
        resources: &Resources,
    ) -> Result<Self, CommandError> {
        let mut parser = Parser {
            reader,
            selector: Self::new(),
            name_state: InvertableState::default(),
            game_mode_state: InvertableState::default(),
            type_state: InvertableState::default(),
            team_state: InvertableState::default(),
            limited: false,
            sorted: false,
            scored: false,
            resources,
        };
        if parser.reader.peek_is(SELECTOR_START) {
            if !allow_selectors {
                return Err(
                    CommandError::simple("argument.entity.selector.not_allowed").at(parser.reader)
                );
            }
            parser.reader.skip();
            parser.parse_selector()?;
        } else {
            parser.parse_name_or_uuid()?;
        }
        Ok(parser.selector)
    }

    /// The box, relative to the selector position, that entities must
    /// touch.
    fn relative_box(&self) -> Option<[f64; 6]> {
        if self.dx.is_some() || self.dy.is_some() || self.dz.is_some() {
            let [dx, dy, dz] = [self.dx, self.dy, self.dz].map(|d| d.unwrap_or(0.0));
            let axis = |d: f64| if d < 0.0 { (d, 1.0) } else { (0.0, d + 1.0) };
            let (x0, x1) = axis(dx);
            let (y0, y1) = axis(dy);
            let (z0, z1) = axis(dz);
            return Some([x0, y0, z0, x1, y1, z1]);
        }
        let max = self.distance.and_then(|d| d.max)?;
        Some([-max, -max, -max, max + 1.0, max + 1.0, max + 1.0])
    }

    fn origin(&self, source: &SelectorSource) -> [f64; 3] {
        [
            self.x.unwrap_or(source.position[0]),
            self.y.unwrap_or(source.position[1]),
            self.z.unwrap_or(source.position[2]),
        ]
    }

    /// Whether `candidate` passes every option but `predicate=`, which
    /// needs the world and is tested only on what this lets through.
    pub fn matches_besides_predicates(
        &self,
        candidate: &Candidate,
        source: &SelectorSource,
    ) -> bool {
        self.test(candidate, source, self.origin(source), false)
    }

    fn matches(&self, candidate: &Candidate, source: &SelectorSource, origin: [f64; 3]) -> bool {
        self.test(candidate, source, origin, true)
    }

    fn test(
        &self,
        candidate: &Candidate,
        source: &SelectorSource,
        origin: [f64; 3],
        predicates: bool,
    ) -> bool {
        if self.only_alive && !candidate.alive {
            return false;
        }
        if !self.includes_entities && !candidate.is_player() {
            return false;
        }
        if self.only_type.is_some_and(|kind| kind != candidate.kind) {
            return false;
        }
        if self.world_limited && candidate.dimension != source.dimension {
            return false;
        }
        if !self
            .names
            .iter()
            .all(|(name, inverted)| (candidate.name == *name) != *inverted)
        {
            return false;
        }
        if !self.game_modes.iter().all(|(mode, inverted)| {
            candidate
                .game_mode
                .is_some_and(|current| (current == *mode) != *inverted)
        }) {
            return false;
        }
        if !self
            .types
            .iter()
            .all(|(test, inverted)| test.matches(candidate) != *inverted)
        {
            return false;
        }
        if !self
            .teams
            .iter()
            .all(|(team, inverted)| (candidate.team.as_deref().unwrap_or("") == team) != *inverted)
        {
            return false;
        }
        if !self.scores.iter().all(|(objective, range)| {
            candidate
                .scores
                .iter()
                .find(|(name, _)| name == objective)
                .is_some_and(|(_, value)| range.contains(*value))
        }) {
            return false;
        }
        if !self.nbt.iter().all(|(expected, inverted)| {
            let empty = Compound::new();
            let actual = candidate.nbt.as_ref().unwrap_or(&empty);
            compound_matches(expected, actual) != *inverted
        }) {
            return false;
        }
        if predicates
            && !(0..self.predicates.len()).all(|i| candidate.predicates.get(i) == Some(&true))
        {
            return false;
        }
        if !self.tags.iter().all(|(tag, inverted)| {
            let has = if tag.is_empty() {
                candidate.tags.is_empty()
            } else {
                candidate.tags.contains(tag)
            };
            has != *inverted
        }) {
            return false;
        }
        if let Some(level) = self.level
            && !candidate.level.is_some_and(|l| level.contains(l))
        {
            return false;
        }
        if let Some(checks) = &self.advancements
            && !checks.is_empty()
        {
            let Some(view) = &candidate.advancements else {
                return false;
            };
            if !checks.iter().all(|(name, check)| check.matches(view, name)) {
                return false;
            }
        }
        if let Some(pitch) = self.x_rotation
            && !rotation_in(pitch, candidate.rotation[1])
        {
            return false;
        }
        if let Some(yaw) = self.y_rotation
            && !rotation_in(yaw, candidate.rotation[0])
        {
            return false;
        }
        if let Some(relative) = self.relative_box() {
            let b = candidate.bounds;
            let absolute = [
                relative[0] + origin[0],
                relative[1] + origin[1],
                relative[2] + origin[2],
                relative[3] + origin[0],
                relative[4] + origin[1],
                relative[5] + origin[2],
            ];
            let intersects = b[0] < absolute[3]
                && b[3] > absolute[0]
                && b[1] < absolute[4]
                && b[4] > absolute[1]
                && b[2] < absolute[5]
                && b[5] > absolute[2];
            if !intersects {
                return false;
            }
        }
        if let Some(distance) = self.distance {
            let d = distance_sq(candidate.position, origin);
            if !distance.contains_sq(d) {
                return false;
            }
        }
        true
    }

    /// The candidates the selector picks, in order.
    pub fn select<'c>(
        &self,
        source: &SelectorSource,
        candidates: &'c [Candidate],
        rng: &mut impl Rng,
    ) -> Vec<&'c Candidate> {
        if let Some(name) = &self.player_name {
            return candidates
                .iter()
                .filter(|c| c.is_player() && c.name.eq_ignore_ascii_case(name))
                .take(1)
                .collect();
        }
        if let Some(uuid) = self.uuid {
            return candidates
                .iter()
                .filter(|c| c.uuid == uuid && (self.includes_entities || c.is_player()))
                .take(1)
                .collect();
        }
        let origin = self.origin(source);
        if self.current_entity {
            return candidates
                .iter()
                .filter(|c| Some(c.id) == source.entity && self.matches(c, source, origin))
                .take(1)
                .collect();
        }
        let mut found: Vec<&Candidate> = candidates
            .iter()
            .filter(|c| self.matches(c, source, origin))
            .collect();
        if found.len() > 1 {
            match self.order {
                Order::Arbitrary => {}
                Order::Nearest => found.sort_by(|a, b| {
                    distance_sq(a.position, origin).total_cmp(&distance_sq(b.position, origin))
                }),
                Order::Furthest => found.sort_by(|a, b| {
                    distance_sq(b.position, origin).total_cmp(&distance_sq(a.position, origin))
                }),
                Order::Random => found.shuffle(rng),
            }
        }
        found.truncate(self.max_results);
        found
    }
}

fn distance_sq(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum()
}

fn rotation_in(range: Bounds<f32>, rotation: f32) -> bool {
    let min = super::coords::wrap_degrees(range.min.unwrap_or(0.0));
    let max = super::coords::wrap_degrees(range.max.unwrap_or(359.0));
    let rotation = super::coords::wrap_degrees(rotation);
    if min > max {
        rotation >= min || rotation <= max
    } else {
        rotation >= min && rotation <= max
    }
}

/// Tracks which of an option's forms have been used, so a second positive
/// `name=` is rejected while several negated ones are allowed.
#[derive(Default)]
struct InvertableState {
    positive: bool,
    negative: bool,
    tags: Vec<String>,
}

impl InvertableState {
    fn can_parse_any(&self) -> bool {
        !self.positive
    }

    fn can_parse(&self, inverted: bool) -> bool {
        if inverted {
            !self.positive
        } else {
            !self.positive && !self.negative
        }
    }

    fn can_parse_tag(&self, tag: &str) -> bool {
        !self.positive && !self.tags.iter().any(|t| t == tag)
    }

    fn mark_tag(&mut self, tag: String) {
        self.negative = true;
        self.tags.push(tag);
    }

    fn mark(&mut self, inverted: bool) {
        if inverted {
            self.negative = true;
        } else {
            self.positive = true;
        }
    }
}

struct Parser<'r, 'a> {
    reader: &'r mut Reader<'a>,
    selector: Selector,
    name_state: InvertableState,
    game_mode_state: InvertableState,
    type_state: InvertableState,
    team_state: InvertableState,
    limited: bool,
    sorted: bool,
    scored: bool,
    resources: &'r Resources,
}

pub const OPTION_NAMES: &[&str] = &[
    "name",
    "distance",
    "level",
    "x",
    "y",
    "z",
    "dx",
    "dy",
    "dz",
    "x_rotation",
    "y_rotation",
    "limit",
    "sort",
    "gamemode",
    "type",
    "tag",
    "team",
    "scores",
    "nbt",
    "predicate",
    "advancements",
];

impl Parser<'_, '_> {
    fn parse_selector(&mut self) -> Result<(), CommandError> {
        self.selector.uses_selector = true;
        if !self.reader.can_read() {
            return Err(CommandError::simple("argument.entity.selector.missing").at(self.reader));
        }
        let start = self.reader.cursor();
        let kind = self.reader.read().expect("checked can_read");
        let selector = &mut self.selector;
        let only_alive = match kind {
            'a' => {
                selector.max_results = usize::MAX;
                selector.only_type = Some(EntityType::Player);
                false
            }
            'e' => {
                selector.max_results = usize::MAX;
                selector.includes_entities = true;
                true
            }
            'n' => {
                selector.includes_entities = true;
                selector.order = Order::Nearest;
                true
            }
            'p' => {
                selector.order = Order::Nearest;
                selector.only_type = Some(EntityType::Player);
                false
            }
            'r' => {
                selector.order = Order::Random;
                selector.only_type = Some(EntityType::Player);
                false
            }
            's' => {
                selector.includes_entities = true;
                selector.current_entity = true;
                false
            }
            other => {
                self.reader.set_cursor(start);
                return Err(CommandError::dynamic(
                    "argument.entity.selector.unknown",
                    [format!("@{other}")],
                )
                .at(self.reader));
            }
        };
        selector.only_alive = only_alive;
        if self.reader.peek_is('[') {
            self.reader.skip();
            self.parse_options()?;
        }
        Ok(())
    }

    fn parse_name_or_uuid(&mut self) -> Result<(), CommandError> {
        let start = self.reader.cursor();
        let name = self.reader.read_string()?;
        if let Some(uuid) = parse_uuid_text(&name) {
            self.selector.uuid = Some(uuid);
            self.selector.includes_entities = true;
        } else {
            if name.is_empty() || name.chars().count() > MAX_NAME_LENGTH {
                self.reader.set_cursor(start);
                return Err(CommandError::simple("argument.entity.invalid").at(self.reader));
            }
            self.selector.player_name = Some(name);
        }
        self.selector.max_results = 1;
        Ok(())
    }

    fn parse_options(&mut self) -> Result<(), CommandError> {
        self.reader.skip_whitespace();
        while self.reader.can_read() && !self.reader.peek_is(']') {
            self.reader.skip_whitespace();
            let start = self.reader.cursor();
            let key = self.reader.read_string()?;
            self.reader.skip_whitespace();
            if !self.reader.peek_is('=') {
                self.reader.set_cursor(start);
                return Err(
                    CommandError::dynamic("argument.entity.options.valueless", [key])
                        .at(self.reader),
                );
            }
            self.reader.skip();
            self.reader.skip_whitespace();
            self.parse_option(&key, start)?;
            self.reader.skip_whitespace();
            if self.reader.can_read() {
                if self.reader.peek_is(',') {
                    self.reader.skip();
                } else if self.reader.peek_is(']') {
                    break;
                } else {
                    return Err(CommandError::simple("argument.entity.options.unterminated")
                        .at(self.reader));
                }
            }
        }
        if self.reader.can_read() {
            self.reader.skip();
            return Ok(());
        }
        Err(CommandError::simple("argument.entity.options.unterminated").at(self.reader))
    }

    fn inapplicable(&mut self, start: usize, key: &str) -> CommandError {
        self.reader.set_cursor(start);
        CommandError::dynamic("argument.entity.options.inapplicable", [key]).at(self.reader)
    }

    fn invert(&mut self) -> bool {
        self.reader.skip_whitespace();
        if self.reader.peek_is('!') {
            self.reader.skip();
            self.reader.skip_whitespace();
            return true;
        }
        false
    }

    fn parse_option(&mut self, key: &str, start: usize) -> Result<(), CommandError> {
        match key {
            "name" => {
                if !self.name_state.can_parse_any() {
                    return Err(self.inapplicable(start, key));
                }
                let value_start = self.reader.cursor();
                let inverted = self.invert();
                let name = self.reader.read_string()?;
                if !self.name_state.can_parse(inverted) {
                    return Err(self.inapplicable(value_start, key));
                }
                self.name_state.mark(inverted);
                self.selector.names.push((name, inverted));
            }
            "distance" => {
                if self.selector.distance.is_some() {
                    return Err(self.inapplicable(start, key));
                }
                let value_start = self.reader.cursor();
                let bounds = range::parse_doubles(self.reader)?;
                if bounds.min.is_some_and(|m| m < 0.0) || bounds.max.is_some_and(|m| m < 0.0) {
                    self.reader.set_cursor(value_start);
                    return Err(
                        CommandError::simple("argument.entity.options.distance.negative")
                            .at(self.reader),
                    );
                }
                self.selector.distance = Some(bounds);
                self.selector.world_limited = true;
            }
            "level" => {
                if self.selector.level.is_some() {
                    return Err(self.inapplicable(start, key));
                }
                let value_start = self.reader.cursor();
                let bounds = range::parse_ints(self.reader)?;
                if bounds.min.is_some_and(|m| m < 0) || bounds.max.is_some_and(|m| m < 0) {
                    self.reader.set_cursor(value_start);
                    return Err(
                        CommandError::simple("argument.entity.options.level.negative")
                            .at(self.reader),
                    );
                }
                self.selector.level = Some(bounds);
                self.selector.includes_entities = false;
            }
            "x" | "y" | "z" | "dx" | "dy" | "dz" => {
                let slot = match key {
                    "x" => &mut self.selector.x,
                    "y" => &mut self.selector.y,
                    "z" => &mut self.selector.z,
                    "dx" => &mut self.selector.dx,
                    "dy" => &mut self.selector.dy,
                    _ => &mut self.selector.dz,
                };
                if slot.is_some() {
                    return Err(self.inapplicable(start, key));
                }
                self.selector.world_limited = true;
                let value = self.reader.read_double()?;
                match key {
                    "x" => self.selector.x = Some(value),
                    "y" => self.selector.y = Some(value),
                    "z" => self.selector.z = Some(value),
                    "dx" => self.selector.dx = Some(value),
                    "dy" => self.selector.dy = Some(value),
                    _ => self.selector.dz = Some(value),
                }
            }
            "x_rotation" | "y_rotation" => {
                let slot = if key == "x_rotation" {
                    &self.selector.x_rotation
                } else {
                    &self.selector.y_rotation
                };
                if slot.is_some() {
                    return Err(self.inapplicable(start, key));
                }
                let bounds = range::parse_floats(self.reader)?;
                if key == "x_rotation" {
                    self.selector.x_rotation = Some(bounds);
                } else {
                    self.selector.y_rotation = Some(bounds);
                }
            }
            "limit" => {
                if self.selector.current_entity || self.limited {
                    return Err(self.inapplicable(start, key));
                }
                let value_start = self.reader.cursor();
                let count = self.reader.read_int()?;
                if count < 1 {
                    self.reader.set_cursor(value_start);
                    return Err(
                        CommandError::simple("argument.entity.options.limit.toosmall")
                            .at(self.reader),
                    );
                }
                self.selector.max_results = count as usize;
                self.limited = true;
            }
            "sort" => {
                if self.selector.current_entity || self.sorted {
                    return Err(self.inapplicable(start, key));
                }
                let value_start = self.reader.cursor();
                let name = self.reader.read_unquoted_string();
                self.selector.order = match name {
                    "nearest" => Order::Nearest,
                    "furthest" => Order::Furthest,
                    "random" => Order::Random,
                    "arbitrary" => Order::Arbitrary,
                    other => {
                        let other = other.to_owned();
                        self.reader.set_cursor(value_start);
                        return Err(CommandError::dynamic(
                            "argument.entity.options.sort.irreversible",
                            [other],
                        )
                        .at(self.reader));
                    }
                };
                self.sorted = true;
            }
            "gamemode" => {
                if !self.game_mode_state.can_parse_any() {
                    return Err(self.inapplicable(start, key));
                }
                let value_start = self.reader.cursor();
                let inverted = self.invert();
                if !self.game_mode_state.can_parse(inverted) {
                    return Err(self.inapplicable(value_start, key));
                }
                let name = self.reader.read_unquoted_string().to_owned();
                let Some(mode) = GameMode::from_name(&name) else {
                    self.reader.set_cursor(value_start);
                    return Err(CommandError::dynamic(
                        "argument.entity.options.mode.invalid",
                        [name],
                    )
                    .at(self.reader));
                };
                self.selector.includes_entities = false;
                self.selector.game_modes.push((mode, inverted));
                self.game_mode_state.mark(inverted);
            }
            "type" => {
                if !self.type_state.can_parse_any() {
                    return Err(self.inapplicable(start, key));
                }
                let value_start = self.reader.cursor();
                let inverted = self.invert();
                self.reader.skip_whitespace();
                let test = if self.reader.peek_is('#') {
                    self.reader.skip();
                    self.reader.skip_whitespace();
                    let id = super::args::read_identifier(self.reader)?;
                    let (name, kinds) = tag_lookup(&id);
                    if !self.type_state.can_parse_tag(&name) {
                        return Err(self.inapplicable(value_start, key));
                    }
                    self.selector
                        .types
                        .push((TypeTest::Tag(name.clone(), kinds), inverted));
                    self.type_state.mark_tag(name);
                    return Ok(());
                } else {
                    if !self.type_state.can_parse(inverted) {
                        return Err(self.inapplicable(value_start, key));
                    }
                    let id = super::args::read_identifier(self.reader)?;
                    let full = if id.contains(':') {
                        id.clone()
                    } else {
                        format!("minecraft:{id}")
                    };
                    if EntityType::from_name(&id).is_none()
                        && self.resources.contains("minecraft:entity_type", &full)
                    {
                        if !inverted {
                            self.selector.includes_entities = true;
                        }
                        self.selector
                            .types
                            .push((TypeTest::Registered(full), inverted));
                        self.type_state.mark(inverted);
                        return Ok(());
                    }
                    let Some(kind) = EntityType::from_name(&id) else {
                        self.reader.set_cursor(value_start);
                        return Err(CommandError::dynamic(
                            "argument.entity.options.type.invalid",
                            [id],
                        )
                        .at(self.reader));
                    };
                    if kind == EntityType::Player && !inverted {
                        self.selector.includes_entities = false;
                    }
                    if !inverted {
                        self.selector.only_type = Some(kind);
                    }
                    TypeTest::Kind(kind)
                };
                self.selector.types.push((test, inverted));
                self.type_state.mark(inverted);
            }
            "tag" => {
                let inverted = self.invert();
                let tag = self.reader.read_unquoted_string().to_owned();
                self.selector.tags.push((tag, inverted));
            }
            "team" => {
                if !self.team_state.can_parse_any() {
                    return Err(self.inapplicable(start, key));
                }
                let value_start = self.reader.cursor();
                let inverted = self.invert();
                let team = self.reader.read_unquoted_string().to_owned();
                if !self.team_state.can_parse(inverted) {
                    return Err(self.inapplicable(value_start, key));
                }
                self.selector.teams.push((team, inverted));
                self.team_state.mark(inverted);
            }
            "scores" => {
                if self.scored {
                    return Err(self.inapplicable(start, key));
                }
                self.selector.scores = parse_scores(self.reader)?;
                self.scored = true;
            }
            "nbt" => {
                let inverted = self.invert();
                let nbt = super::args::read_nbt_compound(self.reader)?;
                self.selector.nbt.push((nbt, inverted));
            }
            "predicate" => {
                let inverted = self.invert();
                let id = super::args::read_identifier(self.reader)?;
                self.selector.predicates.push((id, inverted));
            }
            "advancements" => {
                if self.selector.advancements.is_some() {
                    return Err(self.inapplicable(start, key));
                }
                let checks = parse_advancements(self.reader)?;
                if !checks.is_empty() {
                    self.selector.includes_entities = false;
                }
                self.selector.advancements = Some(checks);
            }
            _ => {
                self.reader.set_cursor(start);
                return Err(
                    CommandError::dynamic("argument.entity.options.unknown", [key]).at(self.reader),
                );
            }
        }
        Ok(())
    }
}

/// What an `advancements` selector option expects of one advancement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvancementCheck {
    Done(bool),
    /// Each criterion met or not.
    Criteria(Vec<(String, bool)>),
}

impl AdvancementCheck {
    fn matches(&self, view: &crate::world::advancement::View, name: &str) -> bool {
        match self {
            Self::Done(expected) => view.done(name) == Some(*expected),
            Self::Criteria(criteria) => {
                view.done(name).is_some()
                    && criteria.iter().all(|(criterion, expected)| {
                        view.criterion(name, criterion) == Some(*expected)
                    })
            }
        }
    }
}

/// `{name=bool, name={criterion=bool, ...}, ...}`.
fn parse_advancements(
    reader: &mut Reader<'_>,
) -> Result<Vec<(String, AdvancementCheck)>, CommandError> {
    let mut checks = Vec::new();
    reader.expect('{')?;
    reader.skip_whitespace();
    while reader.can_read() && !reader.peek_is('}') {
        reader.skip_whitespace();
        let name = super::args::read_identifier(reader)?;
        reader.skip_whitespace();
        reader.expect('=')?;
        reader.skip_whitespace();
        let check = if reader.peek_is('{') {
            reader.expect('{')?;
            reader.skip_whitespace();
            let mut criteria = Vec::new();
            while reader.can_read() && !reader.peek_is('}') {
                reader.skip_whitespace();
                let criterion = reader.read_unquoted_string().to_owned();
                reader.skip_whitespace();
                reader.expect('=')?;
                reader.skip_whitespace();
                criteria.push((criterion, reader.read_bool()?));
                reader.skip_whitespace();
                if reader.peek_is(',') {
                    reader.skip();
                }
            }
            reader.skip_whitespace();
            reader.expect('}')?;
            reader.skip_whitespace();
            AdvancementCheck::Criteria(criteria)
        } else {
            AdvancementCheck::Done(reader.read_bool()?)
        };
        checks.push((name, check));
        reader.skip_whitespace();
        if reader.peek_is(',') {
            reader.skip();
        }
    }
    reader.expect('}')?;
    Ok(checks)
}

/// `{objective=range, ...}`.
fn parse_scores(reader: &mut Reader<'_>) -> Result<Vec<(String, Bounds<i32>)>, CommandError> {
    let mut scores = Vec::new();
    reader.expect('{')?;
    reader.skip_whitespace();
    while reader.can_read() && !reader.peek_is('}') {
        reader.skip_whitespace();
        let name = reader.read_unquoted_string().to_owned();
        reader.skip_whitespace();
        reader.expect('=')?;
        reader.skip_whitespace();
        let range = range::parse_ints(reader)?;
        scores.retain(|(existing, _)| *existing != name);
        scores.push((name, range));
        reader.skip_whitespace();
        if reader.peek_is(',') {
            reader.skip();
        }
    }
    reader.expect('}')?;
    Ok(scores)
}

/// Whether `actual` has everything `expected` has: compounds may hold more
/// keys, and every expected list element must match some actual element.
pub fn tag_matches(expected: &Tag, actual: &Tag) -> bool {
    match (expected, actual) {
        (Tag::Compound(expected), Tag::Compound(actual)) => compound_matches(expected, actual),
        (Tag::List(expected), Tag::List(actual)) => {
            if expected.is_empty() {
                return actual.is_empty();
            }
            actual.len() >= expected.len()
                && expected
                    .iter()
                    .all(|e| actual.iter().any(|a| tag_matches(e, a)))
        }
        _ => expected == actual,
    }
}

pub fn compound_matches(expected: &Compound, actual: &Compound) -> bool {
    actual.len() >= expected.len()
        && expected.iter().all(|(key, value)| {
            actual
                .get(key)
                .is_some_and(|actual| tag_matches(value, actual))
        })
}

/// The full tag id and its built-in members (none for a tag only a data
/// pack may define; [`Selector::with_tags`] fills those in).
fn tag_lookup(id: &str) -> (String, tuff_vanilla_data::Frozen<[EntityType]>) {
    let (namespace, path) = id.split_once(':').unwrap_or(("minecraft", id));
    let wanted = format!("{namespace}:{path}");
    let kinds = tuff_vanilla_data::tag::entity_type::ALL
        .iter()
        .find(|(name, _)| *name == wanted)
        .map_or_else(Default::default, |(_, kinds)| {
            tuff_vanilla_data::Frozen::Static(*kinds)
        });
    (wanted, kinds)
}

/// A hyphenated UUID as `UUID.fromString` accepts it.
#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn parse(input: &str) -> Result<Selector, String> {
        Selector::parse(&mut Reader::new(input), true).map_err(|e| e.key().unwrap().to_owned())
    }

    fn player(id: i32, name: &str, position: [f64; 3]) -> Candidate {
        Candidate {
            id,
            uuid: id as u128,
            name: name.into(),
            kind: EntityType::Player,
            dimension: DimensionId::default(),
            position,
            rotation: [0.0, 0.0],
            bounds: [
                position[0] - 0.3,
                position[1],
                position[2] - 0.3,
                position[0] + 0.3,
                position[1] + 1.8,
                position[2] + 0.3,
            ],
            alive: true,
            level: Some(id),
            game_mode: Some(GameMode::Survival),
            tags: Vec::new(),
            advancements: None,
            registered: None,
            team: None,
            scores: Vec::new(),
            nbt: None,
            predicates: Vec::new(),
        }
    }

    fn item(id: i32, position: [f64; 3]) -> Candidate {
        Candidate {
            id,
            uuid: 1000 + id as u128,
            name: "stone".into(),
            kind: EntityType::Item,
            dimension: DimensionId::default(),
            position,
            rotation: [0.0, 0.0],
            bounds: [
                position[0] - 0.125,
                position[1],
                position[2] - 0.125,
                position[0] + 0.125,
                position[1] + 0.25,
                position[2] + 0.125,
            ],
            alive: true,
            level: None,
            game_mode: None,
            tags: Vec::new(),
            advancements: None,
            registered: None,
            team: None,
            scores: Vec::new(),
            nbt: None,
            predicates: Vec::new(),
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn type_tags_mark_the_option_multiple() {
        assert!(parse("@e[type=!pig,type=#minecraft:skeletons]").is_ok());
        assert!(parse("@e[type=#minecraft:skeletons,type=!pig]").is_ok());
        assert!(parse("@e[type=#minecraft:skeletons,type=#minecraft:zombies]").is_ok());
        assert!(parse("@e[type=#minecraft:skeletons,type=#minecraft:skeletons]").is_err());
        assert!(parse("@e[type=#minecraft:skeletons,type=pig]").is_err());
        assert!(parse("@e[type=pig,type=#minecraft:skeletons]").is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_advancements_option_tests_done_and_criteria() {
        use crate::world::advancement::{ProgressView, View};
        let selector = parse(
            "@a[advancements={story/root=true,minecraft:husbandry/tactical_fishing={cod_bucket=true, salmon_bucket=false}}]",
        )
        .unwrap();
        let checks = selector.advancements.as_ref().unwrap();
        assert_eq!(checks.len(), 2);
        assert_eq!(
            checks[0],
            (
                "minecraft:story/root".to_owned(),
                AdvancementCheck::Done(true)
            )
        );
        assert_eq!(
            parse("@a[advancements={},advancements={}]"),
            Err("argument.entity.options.inapplicable".into())
        );
        assert_eq!(
            parse("@a[advancements={story/root=maybe}]"),
            Err("parsing.bool.invalid".into())
        );

        struct Stub(bool);
        impl ProgressView for Stub {
            fn done(&self, name: &str) -> Option<bool> {
                match name {
                    "minecraft:story/root" => Some(self.0),
                    "minecraft:husbandry/tactical_fishing" => Some(false),
                    _ => None,
                }
            }

            fn criterion(&self, name: &str, criterion: &str) -> Option<bool> {
                match (name, criterion) {
                    ("minecraft:husbandry/tactical_fishing", "cod_bucket") => Some(self.0),
                    ("minecraft:husbandry/tactical_fishing", "salmon_bucket") => Some(false),
                    _ => None,
                }
            }
        }
        let mut done = player(1, "Done", [0.0, 64.0, 0.0]);
        done.advancements = Some(View(std::sync::Arc::new(Stub(true))));
        let mut fresh = player(2, "Fresh", [0.0, 64.0, 0.0]);
        fresh.advancements = Some(View(std::sync::Arc::new(Stub(false))));
        let candidates = [done, fresh, item(3, [0.0, 64.0, 0.0])];
        let picked: Vec<&str> = selector
            .select(&source(), &candidates, &mut StdRng::seed_from_u64(1))
            .into_iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(picked, ["Done"]);
        let none = parse("@a[advancements={minecraft:no/such=false}]").unwrap();
        assert!(
            none.select(&source(), &candidates, &mut StdRng::seed_from_u64(1))
                .is_empty()
        );
    }

    fn source() -> SelectorSource {
        SelectorSource {
            position: [0.0, 64.0, 0.0],
            dimension: DimensionId::default(),
            entity: Some(1),
        }
    }

    fn names(found: Vec<&Candidate>) -> Vec<String> {
        found.iter().map(|c| c.name.clone()).collect()
    }

    #[tuff_core::watchdog]
    #[test]
    fn parses_selector_forms_and_options() {
        let all = parse("@a").unwrap();
        assert_eq!(all.max_results, usize::MAX);
        assert!(!all.includes_entities);
        let entities = parse("@e[type=item,distance=..5,limit=2,sort=nearest]").unwrap();
        assert_eq!(entities.max_results, 2);
        assert_eq!(entities.order, Order::Nearest);
        assert!(entities.world_limited && entities.includes_entities);
        assert_eq!(entities.only_type, Some(EntityType::Item));
        let named = parse("Steve").unwrap();
        assert_eq!(named.player_name.as_deref(), Some("Steve"));
        let uuid = parse("069a79f4-44e9-4726-a5be-fca90e38aaf5").unwrap();
        assert_eq!(uuid.uuid, Some(0x069a79f444e94726a5befca90e38aaf5));
        assert_eq!(
            format_uuid(0x069a79f444e94726a5befca90e38aaf5),
            "069a79f4-44e9-4726-a5be-fca90e38aaf5"
        );
        let negated =
            parse("@a[name=!a,name=!b,gamemode=!creative,tag=x,x_rotation=-10..10]").unwrap();
        assert_eq!(negated.names.len(), 2);
        assert_eq!(negated.game_modes, vec![(GameMode::Creative, true)]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn rejects_bad_selectors_with_the_games_keys() {
        assert_eq!(parse("@x"), Err("argument.entity.selector.unknown".into()));
        assert_eq!(parse("@"), Err("argument.entity.selector.missing".into()));
        assert_eq!(
            parse("@a[foo=1]"),
            Err("argument.entity.options.unknown".into())
        );
        assert_eq!(
            parse("@a[name]"),
            Err("argument.entity.options.valueless".into())
        );
        assert_eq!(
            parse("@a[name=a"),
            Err("argument.entity.options.unterminated".into())
        );
        assert_eq!(
            parse("@a[name=a name=b]"),
            Err("argument.entity.options.unterminated".into())
        );
        assert_eq!(
            parse("@a[name=a,name=b]"),
            Err("argument.entity.options.inapplicable".into())
        );
        assert_eq!(
            parse("@s[limit=1]"),
            Err("argument.entity.options.inapplicable".into())
        );
        assert_eq!(
            parse("@a[limit=0]"),
            Err("argument.entity.options.limit.toosmall".into())
        );
        assert_eq!(
            parse("@a[distance=-1..]"),
            Err("argument.entity.options.distance.negative".into())
        );
        assert_eq!(
            parse("@a[level=..-1]"),
            Err("argument.entity.options.level.negative".into())
        );
        assert_eq!(
            parse("@a[sort=up]"),
            Err("argument.entity.options.sort.irreversible".into())
        );
        assert_eq!(
            parse("@a[gamemode=hard]"),
            Err("argument.entity.options.mode.invalid".into())
        );
        assert_eq!(
            parse("@e[type=rock]"),
            Err("argument.entity.options.type.invalid".into())
        );
        assert_eq!(
            parse("abcdefghijklmnopq"),
            Err("argument.entity.invalid".into())
        );
        assert_eq!(
            Selector::parse(&mut Reader::new("@a"), false)
                .unwrap_err()
                .key(),
            Some("argument.entity.selector.not_allowed")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn selects_players_and_entities() {
        let candidates = vec![
            player(1, "Alice", [0.0, 64.0, 0.0]),
            player(2, "Bob", [3.0, 64.0, 0.0]),
            player(3, "Carol", [10.0, 64.0, 0.0]),
            item(4, [1.0, 64.0, 0.0]),
        ];
        let mut rng = StdRng::seed_from_u64(1);
        let select = |input: &str, rng: &mut StdRng| {
            names(parse(input).unwrap().select(&source(), &candidates, rng))
        };
        assert_eq!(select("@a", &mut rng), ["Alice", "Bob", "Carol"]);
        assert_eq!(select("@s", &mut rng), ["Alice"]);
        assert_eq!(select("@p", &mut rng), ["Alice"]);
        assert_eq!(select("@p[x=9]", &mut rng), ["Carol"]);
        assert_eq!(select("@a[distance=..5]", &mut rng), ["Alice", "Bob"]);
        assert_eq!(select("@a[distance=1..]", &mut rng), ["Bob", "Carol"]);
        assert_eq!(select("@a[sort=furthest,limit=1]", &mut rng), ["Carol"]);
        assert_eq!(select("@e[type=item]", &mut rng), ["stone"]);
        assert_eq!(select("@n", &mut rng), ["Alice"]);
        assert_eq!(select("@n[type=!player]", &mut rng), ["stone"]);
        assert_eq!(
            select("@e[dx=2,dy=0,dz=0]", &mut rng),
            ["Alice", "Bob", "stone"]
        );
        assert_eq!(select("@e[dx=1,dy=0,dz=0]", &mut rng), ["Alice", "stone"]);
        assert_eq!(select("@a[level=2..]", &mut rng), ["Bob", "Carol"]);
        assert_eq!(select("@a[name=!Alice]", &mut rng), ["Bob", "Carol"]);
        assert_eq!(select("bob", &mut rng), ["Bob"]);
        assert!(select("dave", &mut rng).is_empty());
        assert_eq!(
            select("00000000-0000-0000-0000-0000000003ec", &mut rng),
            ["stone"]
        );
        assert_eq!(select("@a[gamemode=survival]", &mut rng).len(), 3);
        assert!(select("@a[gamemode=creative]", &mut rng).is_empty());
        let random: Vec<_> = (0..20).map(|_| select("@r", &mut rng)[0].clone()).collect();
        assert!(random.iter().any(|n| n != &random[0]));
    }

    #[tuff_core::watchdog]
    #[test]
    fn team_scores_nbt_and_predicate_options_filter_candidates() {
        let mut alice = player(1, "Alice", [0.0, 64.0, 0.0]);
        alice.team = Some("red".into());
        alice.scores = vec![("kills".into(), 5)];
        alice.nbt = Some(Compound::new().with("Health", Tag::Float(20.0)).with(
            "Inventory",
            Tag::List(vec![Tag::Compound(
                        Compound::new().with("id", "minecraft:stone").with("count", 3),
                    )]),
        ));
        alice.predicates = vec![true, false];
        let mut bob = player(2, "Bob", [1.0, 64.0, 0.0]);
        bob.scores = vec![("kills".into(), 1)];
        bob.predicates = vec![false];
        let candidates = vec![alice, bob];
        let mut rng = StdRng::seed_from_u64(1);
        let mut select = |input: &str| {
            names(
                parse(input)
                    .unwrap()
                    .select(&source(), &candidates, &mut rng),
            )
        };
        assert_eq!(select("@a[team=red]"), ["Alice"]);
        assert_eq!(select("@a[team=!red]"), ["Bob"]);
        assert_eq!(select("@a[team=]"), ["Bob"]);
        assert_eq!(select("@a[team=!]"), ["Alice"]);
        assert_eq!(select("@a[scores={kills=2..}]"), ["Alice"]);
        assert_eq!(select("@a[scores={kills=..1}]"), ["Bob"]);
        assert_eq!(select("@a[scores={deaths=0..}]").len(), 0);
        assert_eq!(select("@a[scores={}]").len(), 2);
        assert_eq!(select("@a[nbt={Health:20.0f}]"), ["Alice"]);
        assert_eq!(select("@a[nbt=!{Health:20.0f}]"), ["Bob"]);
        assert_eq!(
            select("@a[nbt={Inventory:[{id:\"minecraft:stone\"}]}]"),
            ["Alice"]
        );
        assert_eq!(select("@a[nbt={Inventory:[]}]").len(), 0);
        assert_eq!(select("@a[predicate=a:b]"), ["Alice"]);
        assert_eq!(
            select("@a[predicate=a:b,predicate=a:c]").len(),
            0,
            "every predicate must pass"
        );
        let untested = parse("@a[predicate=!a:b]").unwrap();
        let mut fresh = player(3, "Carol", [0.0, 64.0, 0.0]);
        fresh.predicates.clear();
        assert!(
            untested
                .select(&source(), &[fresh.clone()], &mut StdRng::seed_from_u64(1))
                .is_empty(),
            "a candidate whose predicates were not tested never matches"
        );
        assert!(untested.matches_besides_predicates(&fresh, &source()));
    }

    #[tuff_core::watchdog]
    #[test]
    fn new_options_reject_repeats_like_the_game() {
        assert!(parse("@a[team=!a,team=!b]").is_ok());
        assert_eq!(
            parse("@a[team=a,team=b]"),
            Err("argument.entity.options.inapplicable".into())
        );
        assert_eq!(
            parse("@a[team=a,team=!b]"),
            Err("argument.entity.options.inapplicable".into())
        );
        assert_eq!(
            parse("@a[scores={a=1},scores={b=1}]"),
            Err("argument.entity.options.inapplicable".into())
        );
        assert!(parse("@a[nbt={a:1b},nbt=!{b:1b}]").is_ok());
        assert_eq!(
            parse("@a[nbt=foo]"),
            Err("argument.nbt.expected.compound".into())
        );
        assert_eq!(
            parse("@a[scores={a=x}]"),
            Err("argument.range.empty".into())
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn registered_feature_types_parse_and_match_only_their_own_entities() {
        let mut resources = Resources::default();
        resources.add("minecraft:entity_type", "example:counter");
        let selector = Selector::parse_with(
            &mut Reader::new("@e[type=example:counter]"),
            true,
            &resources,
        )
        .unwrap();
        assert_eq!(
            parse("@e[type=example:counter]"),
            Err("argument.entity.options.type.invalid".into()),
            "unregistered ids stay invalid"
        );
        let mut counter = item(3, [0.0, 64.0, 0.0]);
        counter.kind = EntityType::Interaction;
        counter.registered = Some("example:counter".into());
        counter.name = "counter".into();
        let mut plain = item(4, [0.0, 64.0, 0.0]);
        plain.kind = EntityType::Interaction;
        plain.name = "interaction".into();
        let candidates = vec![counter, plain];
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(
            names(selector.select(&source(), &candidates, &mut rng)),
            ["counter"]
        );
        let vanilla = parse("@e[type=interaction]").unwrap();
        assert_eq!(
            names(vanilla.select(&source(), &candidates, &mut rng)),
            ["interaction"]
        );
        let negated = Selector::parse_with(
            &mut Reader::new("@e[type=!example:counter]"),
            true,
            &resources,
        )
        .unwrap();
        assert_eq!(
            names(negated.select(&source(), &candidates, &mut rng)),
            ["interaction"]
        );
    }
}
