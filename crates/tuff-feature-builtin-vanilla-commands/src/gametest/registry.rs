//! The `test_instance` and `test_environment` registries: the built-in
//! entries plus what data packs define, and the test functions code
//! registers.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use serde_json::Value as Json;
use tuff_core::datapack::Contents;

pub const DEFAULT_ENVIRONMENT: &str = "minecraft:default";
pub const ALWAYS_PASS: &str = "minecraft:always_pass";

/// A quarter-turn rotation, clockwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Rotation {
    #[default]
    None,
    Clockwise90,
    Clockwise180,
    Counterclockwise90,
}

impl Rotation {
    pub const ALL: [Rotation; 4] = [
        Self::None,
        Self::Clockwise90,
        Self::Clockwise180,
        Self::Counterclockwise90,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Clockwise90 => "clockwise_90",
            Self::Clockwise180 => "180",
            Self::Counterclockwise90 => "counterclockwise_90",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.name() == name)
    }

    /// Quarter turns clockwise.
    pub fn turns(self) -> u8 {
        self as u8
    }

    pub fn from_turns(turns: i64) -> Self {
        Self::ALL[turns.rem_euclid(4) as usize]
    }

    /// This rotation followed by `other`.
    pub fn then(self, other: Self) -> Self {
        Self::from_turns(i64::from(self.turns()) + i64::from(other.turns()))
    }

    /// Whether x and z swap.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Self::Clockwise90 | Self::Counterclockwise90)
    }
}

/// A test environment, named or given inline.
#[derive(Debug, Clone, PartialEq)]
pub enum EnvironmentRef {
    Named(String),
    Inline(Box<Environment>),
}

impl EnvironmentRef {
    /// The name a batch reports.
    pub fn display_name(&self) -> String {
        match self {
            Self::Named(name) => name.clone(),
            Self::Inline(_) => "[unregistered]".to_owned(),
        }
    }
}

/// What a batch sets up around its tests and tears down after.
#[derive(Debug, Clone, PartialEq)]
pub enum Environment {
    AllOf(Vec<EnvironmentRef>),
    ClockTime {
        clock: String,
        time: i32,
    },
    Difficulty(String),
    Functions {
        setup: Option<String>,
        teardown: Option<String>,
    },
    GameRules(Vec<(String, String)>),
    Timelines(Vec<String>),
    Weather(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    Function,
    BlockBased,
}

/// One `test_instance` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct TestInstance {
    pub kind: TestKind,
    /// The test function, for a function test.
    pub function: Option<String>,
    pub environment: EnvironmentRef,
    pub dimension: String,
    pub structure: String,
    pub max_ticks: i32,
    pub setup_ticks: i32,
    pub required: bool,
    pub rotation: Rotation,
    pub manual_only: bool,
    pub max_attempts: i32,
    pub required_successes: i32,
    pub sky_access: bool,
    pub padding: i32,
}

/// The tests and environments in effect.
#[derive(Debug, Default)]
pub struct TestRegistry {
    pub instances: BTreeMap<String, Arc<TestInstance>>,
    pub environments: BTreeMap<String, Environment>,
}

fn qualify(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

fn int(
    object: &serde_json::Map<String, Json>,
    key: &str,
    default: Option<i64>,
) -> Result<i64, String> {
    match object.get(key) {
        Some(value) => value
            .as_i64()
            .ok_or_else(|| format!("{key} is not an integer")),
        None => default.ok_or_else(|| format!("missing {key}")),
    }
}

fn flag(object: &serde_json::Map<String, Json>, key: &str, default: bool) -> Result<bool, String> {
    match object.get(key) {
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{key} is not a boolean")),
        None => Ok(default),
    }
}

fn id(object: &serde_json::Map<String, Json>, key: &str) -> Result<Option<String>, String> {
    match object.get(key) {
        Some(Json::String(value)) => Ok(Some(qualify(value))),
        Some(_) => Err(format!("{key} is not an id")),
        None => Ok(None),
    }
}

fn type_of(object: &serde_json::Map<String, Json>) -> Result<String, String> {
    id(object, "type")?.ok_or_else(|| "missing type".to_owned())
}

pub fn parse_environment_ref(value: &Json) -> Result<EnvironmentRef, String> {
    match value {
        Json::String(name) => Ok(EnvironmentRef::Named(qualify(name))),
        Json::Object(_) => Ok(EnvironmentRef::Inline(Box::new(parse_environment(value)?))),
        _ => Err("environment is neither an id nor an object".to_owned()),
    }
}

pub fn parse_environment(value: &Json) -> Result<Environment, String> {
    let object = value.as_object().ok_or("environment is not an object")?;
    Ok(match type_of(object)?.as_str() {
        "minecraft:all_of" => Environment::AllOf(
            object
                .get("definitions")
                .and_then(Json::as_array)
                .ok_or("missing definitions")?
                .iter()
                .map(parse_environment_ref)
                .collect::<Result<_, _>>()?,
        ),
        "minecraft:clock_time" => {
            let time = int(object, "time", None)?;
            if time < 0 {
                return Err("time must be non-negative".to_owned());
            }
            Environment::ClockTime {
                clock: id(object, "clock")?.ok_or("missing clock")?,
                time: time as i32,
            }
        }
        "minecraft:difficulty" => {
            let difficulty = object
                .get("difficulty")
                .and_then(Json::as_str)
                .ok_or("missing difficulty")?;
            if tuff_core::world::player::Difficulty::from_name(difficulty).is_none() {
                return Err(format!("unknown difficulty {difficulty}"));
            }
            Environment::Difficulty(difficulty.to_owned())
        }
        "minecraft:function" => Environment::Functions {
            setup: id(object, "setup")?,
            teardown: id(object, "teardown")?,
        },
        "minecraft:game_rules" => {
            let rules = object
                .get("rules")
                .and_then(Json::as_object)
                .ok_or("missing rules")?;
            let mut set = Vec::with_capacity(rules.len());
            for (name, value) in rules {
                let value = match value {
                    Json::Bool(b) => b.to_string(),
                    Json::Number(n) if n.is_i64() => n.to_string(),
                    _ => return Err(format!("game rule {name} has an invalid value")),
                };
                let name = name.strip_prefix("minecraft:").unwrap_or(name);
                set.push((name.to_owned(), value));
            }
            Environment::GameRules(set)
        }
        "minecraft:timeline_attributes" => Environment::Timelines(
            object
                .get("timelines")
                .and_then(Json::as_array)
                .ok_or("missing timelines")?
                .iter()
                .map(|t| t.as_str().map(qualify).ok_or("timeline is not an id"))
                .collect::<Result<_, _>>()?,
        ),
        "minecraft:weather" => {
            let weather = object
                .get("weather")
                .and_then(Json::as_str)
                .ok_or("missing weather")?;
            if !matches!(weather, "clear" | "rain" | "thunder") {
                return Err(format!("unknown weather {weather}"));
            }
            Environment::Weather(weather.to_owned())
        }
        other => return Err(format!("unknown test environment type {other}")),
    })
}

pub fn parse_instance(value: &Json) -> Result<TestInstance, String> {
    let object = value.as_object().ok_or("test instance is not an object")?;
    let kind = match type_of(object)?.as_str() {
        "minecraft:function" => TestKind::Function,
        "minecraft:block_based" => TestKind::BlockBased,
        other => return Err(format!("unknown test instance type {other}")),
    };
    let function = match kind {
        TestKind::Function => Some(id(object, "function")?.ok_or("missing function")?),
        TestKind::BlockBased => None,
    };
    let positive = |key: &str, default: Option<i64>| -> Result<i32, String> {
        let value = int(object, key, default)?;
        if !(1..=i64::from(i32::MAX)).contains(&value) {
            return Err(format!("{key} must be positive"));
        }
        Ok(value as i32)
    };
    let setup_ticks = int(object, "setup_ticks", Some(0))?;
    if !(0..=i64::from(i32::MAX)).contains(&setup_ticks) {
        return Err("setup_ticks must be non-negative".to_owned());
    }
    let padding = int(object, "padding", Some(0))?;
    if !(0..=128).contains(&padding) {
        return Err("padding must be within 0..=128".to_owned());
    }
    let rotation = match object.get("rotation") {
        None => Rotation::None,
        Some(value) => value
            .as_str()
            .and_then(Rotation::from_name)
            .ok_or("unknown rotation")?,
    };
    Ok(TestInstance {
        kind,
        function,
        environment: parse_environment_ref(
            object.get("environment").ok_or("missing environment")?,
        )?,
        dimension: id(object, "dimension")?.unwrap_or_else(|| "minecraft:overworld".to_owned()),
        structure: id(object, "structure")?.ok_or("missing structure")?,
        max_ticks: positive("max_ticks", None)?,
        setup_ticks: setup_ticks as i32,
        required: flag(object, "required", true)?,
        rotation,
        manual_only: flag(object, "manual_only", false)?,
        max_attempts: positive("max_attempts", Some(1))?,
        required_successes: positive("required_successes", Some(1))?,
        sky_access: flag(object, "sky_access", false)?,
        padding: padding as i32,
    })
}

fn builtin_always_pass() -> TestInstance {
    TestInstance {
        kind: TestKind::Function,
        function: Some(ALWAYS_PASS.to_owned()),
        environment: EnvironmentRef::Named(DEFAULT_ENVIRONMENT.to_owned()),
        dimension: "minecraft:overworld".to_owned(),
        structure: "minecraft:empty".to_owned(),
        max_ticks: 1,
        setup_ticks: 1,
        required: false,
        rotation: Rotation::None,
        manual_only: false,
        max_attempts: 1,
        required_successes: 1,
        sky_access: false,
        padding: 0,
    }
}

impl TestRegistry {
    /// The built-in entries with what `contents` defines over them; a
    /// definition that does not parse is logged and skipped.
    pub fn load(contents: &Contents) -> Self {
        let mut registry = Self::default();
        registry.environments.insert(
            DEFAULT_ENVIRONMENT.to_owned(),
            Environment::AllOf(Vec::new()),
        );
        registry
            .instances
            .insert(ALWAYS_PASS.to_owned(), Arc::new(builtin_always_pass()));
        for (name, bytes) in contents.test_environments.iter() {
            match serde_json::from_slice(bytes)
                .map_err(|e| e.to_string())
                .and_then(|json| parse_environment(&json))
            {
                Ok(environment) => {
                    registry.environments.insert(name.clone(), environment);
                }
                Err(err) => tuff_core::log::error!("Couldn't parse test environment {name}: {err}"),
            }
        }
        for (name, bytes) in contents.test_instances.iter() {
            match serde_json::from_slice(bytes)
                .map_err(|e| e.to_string())
                .and_then(|json| parse_instance(&json))
            {
                Ok(instance) => {
                    registry.instances.insert(name.clone(), Arc::new(instance));
                }
                Err(err) => tuff_core::log::error!("Couldn't parse test instance {name}: {err}"),
            }
        }
        registry
    }

    /// The registry for `contents`, cached per data pack generation.
    pub fn current(contents: &Contents) -> Arc<Self> {
        static CACHE: Mutex<Option<(u64, usize, Arc<TestRegistry>)>> = Mutex::new(None);
        let key = Arc::as_ptr(&contents.test_instances) as usize;
        let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((generation, ptr, registry)) = cache.as_ref()
            && *generation == contents.generation
            && *ptr == key
        {
            return registry.clone();
        }
        let registry = Arc::new(Self::load(contents));
        *cache = Some((contents.generation, key, registry.clone()));
        registry
    }

    /// The ids `patterns` select (resource selector wildcards), sorted.
    pub fn select(&self, patterns: &[String]) -> Vec<String> {
        self.instances
            .keys()
            .filter(|id| {
                patterns
                    .iter()
                    .any(|p| tuff_core::command::args::wildcard_matches(p, id))
            })
            .cloned()
            .collect()
    }

    pub fn environment(&self, reference: &EnvironmentRef) -> Option<Environment> {
        match reference {
            EnvironmentRef::Named(name) => self.environments.get(name).cloned(),
            EnvironmentRef::Inline(environment) => Some((**environment).clone()),
        }
    }
}

/// What a test function sees and does while it runs.
pub struct TestHelper {
    /// Ticks since the test started.
    pub tick: i32,
    /// The test structure's start corner, which `relative` positions are
    /// measured from.
    pub origin: [i32; 3],
    pub(super) outcome: Option<Result<(), String>>,
    pub(super) commands: Vec<String>,
    pub(super) each_tick: Vec<TickCallback>,
    pub(super) at_tick: Vec<(i32, TickCallback)>,
}

pub type TickCallback = Box<dyn FnMut(&mut TestHelper) + Send + Sync>;

impl TestHelper {
    pub(super) fn new(tick: i32, origin: [i32; 3]) -> Self {
        Self {
            tick,
            origin,
            outcome: None,
            commands: Vec::new(),
            each_tick: Vec::new(),
            at_tick: Vec::new(),
        }
    }

    pub fn succeed(&mut self) {
        if self.outcome.is_none() {
            self.outcome = Some(Ok(()));
        }
    }

    pub fn fail(&mut self, message: impl Into<String>) {
        self.outcome = Some(Err(message.into()));
    }

    /// Runs `line` as the server (gamemaster, silent) at the start
    /// corner, after this tick's callbacks.
    pub fn run_command(&mut self, line: impl Into<String>) {
        self.commands.push(line.into());
    }

    /// The absolute position of `relative` to the start corner.
    pub fn absolute(&self, relative: [i32; 3]) -> [i32; 3] {
        [
            self.origin[0] + relative[0],
            self.origin[1] + relative[1],
            self.origin[2] + relative[2],
        ]
    }

    pub fn on_each_tick(&mut self, callback: impl FnMut(&mut TestHelper) + Send + Sync + 'static) {
        self.each_tick.push(Box::new(callback));
    }

    /// Runs `callback` once the test has run `tick` ticks.
    pub fn run_at_tick(
        &mut self,
        tick: i32,
        callback: impl FnMut(&mut TestHelper) + Send + Sync + 'static,
    ) {
        self.at_tick.push((tick, Box::new(callback)));
    }
}

pub type TestFunction = Arc<dyn Fn(&mut TestHelper) + Send + Sync>;

fn functions() -> &'static Mutex<BTreeMap<String, TestFunction>> {
    static FUNCTIONS: OnceLock<Mutex<BTreeMap<String, TestFunction>>> = OnceLock::new();
    FUNCTIONS.get_or_init(|| {
        let mut map: BTreeMap<String, TestFunction> = BTreeMap::new();
        map.insert(ALWAYS_PASS.to_owned(), Arc::new(|helper| helper.succeed()));
        Mutex::new(map)
    })
}

/// Registers the test function `id` (features and Rust code add theirs;
/// `minecraft:always_pass` is built in). Replaces an earlier one.
pub fn register_test_function(id: &str, function: TestFunction) {
    functions()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(qualify(id), function);
}

pub fn test_function(id: &str) -> Option<TestFunction> {
    functions()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(id)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn parses_a_function_instance_with_defaults() {
        let json = serde_json::json!({
            "type": "minecraft:function",
            "function": "always_pass",
            "environment": "minecraft:default",
            "structure": "test:box",
            "max_ticks": 20
        });
        let instance = parse_instance(&json).unwrap();
        assert_eq!(instance.function.as_deref(), Some(ALWAYS_PASS));
        assert_eq!(instance.dimension, "minecraft:overworld");
        assert!(instance.required);
        assert_eq!((instance.max_attempts, instance.required_successes), (1, 1));
        assert_eq!(instance.padding, 0);
    }

    #[tuff_core::watchdog]
    #[test]
    fn rejects_out_of_range_fields() {
        let base = |key: &str, value: Json| {
            let mut json = serde_json::json!({
                "type": "minecraft:block_based",
                "environment": {"type": "minecraft:all_of", "definitions": []},
                "structure": "test:box",
                "max_ticks": 20
            });
            json[key] = value;
            parse_instance(&json)
        };
        assert!(base("max_ticks", 0.into()).is_err());
        assert!(base("padding", 129.into()).is_err());
        assert!(base("setup_ticks", (-1).into()).is_err());
        assert!(base("rotation", "sideways".into()).is_err());
        assert!(base("padding", 128.into()).is_ok());
    }

    #[tuff_core::watchdog]
    #[test]
    fn parses_every_environment_type() {
        let json = serde_json::json!({
            "type": "all_of",
            "definitions": [
                "minecraft:default",
                {"type": "clock_time", "clock": "overworld", "time": 6000},
                {"type": "difficulty", "difficulty": "hard"},
                {"type": "function", "setup": "test:setup"},
                {"type": "game_rules", "rules": {"minecraft:advance_time": false, "random_tick_speed": 0}},
                {"type": "timeline_attributes", "timelines": ["day"]},
                {"type": "weather", "weather": "thunder"}
            ]
        });
        let Environment::AllOf(definitions) = parse_environment(&json).unwrap() else {
            panic!("not all_of");
        };
        assert_eq!(definitions.len(), 7);
        assert_eq!(
            definitions[4],
            EnvironmentRef::Inline(Box::new(Environment::GameRules(vec![
                ("advance_time".into(), "false".into()),
                ("random_tick_speed".into(), "0".into()),
            ])))
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn rotations_compose_clockwise() {
        assert_eq!(
            Rotation::Clockwise90.then(Rotation::Clockwise180),
            Rotation::Counterclockwise90
        );
        assert_eq!(
            Rotation::Counterclockwise90.then(Rotation::Clockwise90),
            Rotation::None
        );
    }
}
