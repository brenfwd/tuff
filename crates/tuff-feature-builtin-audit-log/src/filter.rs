//! Lookup, rollback and purge parameters: `u:` users, `t:` time, `r:`
//! radius, `a:` actions, `i:`/`e:` include and exclude, `#` flags.

use crate::record::{Group, Kind};

/// Where rows must be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Area {
    Global,
    World(String),
    /// Within `radius` blocks of `center` on x and z, any height.
    Radius {
        world: String,
        center: [i32; 3],
        radius: i32,
    },
}

/// Which rows a lookup, rollback or purge touches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filter {
    /// Player names (any case) and `#causes`; empty = everyone.
    pub users: Vec<String>,
    /// Oldest and newest time, unix milliseconds.
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub area: Area,
    /// Empty = every action.
    pub kinds: Vec<Kind>,
    /// Block, item or entity ids; empty = all.
    pub include: Vec<String>,
    /// Ids and users left out.
    pub exclude: Vec<String>,
    pub rolled_back: Option<bool>,
    /// Exactly these rows; empty = no restriction.
    pub row_ids: Vec<i64>,
}

impl Default for Filter {
    fn default() -> Self {
        Filter {
            users: Vec::new(),
            since: None,
            until: None,
            area: Area::Global,
            kinds: Vec::new(),
            include: Vec::new(),
            exclude: Vec::new(),
            rolled_back: None,
            row_ids: Vec::new(),
        }
    }
}

/// Where the command runs from, for `r:` and `#world`.
#[derive(Clone, Debug)]
pub struct Origin {
    pub world: String,
    pub pos: [i32; 3],
}

/// A parsed parameter line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Params {
    pub filter: Filter,
    pub count: bool,
    pub verbose: bool,
    pub silent: bool,
    pub preview: bool,
    /// Rolling back also takes container items from the player who took them.
    pub strict: bool,
    /// 1-based.
    pub page: usize,
    pub has_time: bool,
    pub has_area: bool,
}

/// The `a:` words and the kinds each means.
pub const ACTIONS: &[&str] = &[
    "block",
    "+block",
    "-block",
    "container",
    "+container",
    "-container",
    "item",
    "+item",
    "-item",
    "kill",
    "click",
    "chat",
    "command",
    "session",
    "+session",
    "-session",
    "username",
    "sign",
    "custom",
];

pub const FLAGS: &[&str] = &["#count", "#verbose", "#silent", "#preview", "#strict"];

pub fn action_kinds(word: &str) -> Option<Vec<Kind>> {
    let word = word.to_ascii_lowercase();
    if let Some(kind) = Kind::ALL.into_iter().find(|kind| kind.name() == word) {
        return Some(vec![kind]);
    }
    let group = Group::ALL.into_iter().find(|group| group.name() == word)?;
    Some(group.kinds().collect())
}

/// `2w5d7h2m10s`, `1.5h`, `30` (seconds) as milliseconds.
pub fn parse_duration(text: &str) -> Result<i64, String> {
    let bad = || format!("invalid time {text:?}; use units like 2w5d7h2m10s");
    let mut total = 0f64;
    let mut number = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        let unit = match c.to_ascii_lowercase() {
            'w' => 604_800.0,
            'd' => 86_400.0,
            'h' => 3_600.0,
            'm' => 60.0,
            's' => 1.0,
            _ => return Err(bad()),
        };
        let value: f64 = number.parse().map_err(|_| bad())?;
        total += value * unit;
        number.clear();
    }
    if !number.is_empty() {
        total += number.parse::<f64>().map_err(|_| bad())?;
    }
    if text.is_empty() || !total.is_finite() || total < 0.0 || total > 1e12 {
        return Err(bad());
    }
    Ok((total * 1000.0) as i64)
}

/// `stone` as `minecraft:stone`.
pub fn qualified(id: &str) -> String {
    let id = id.to_ascii_lowercase();
    if id.contains(':') || id.starts_with('#') {
        id
    } else {
        format!("minecraft:{id}")
    }
}

fn world_key(name: &str, origin: &Origin) -> String {
    match name.trim_start_matches('#') {
        "world" => origin.world.clone(),
        "overworld" => "minecraft:overworld".to_owned(),
        "nether" | "the_nether" => "minecraft:the_nether".to_owned(),
        "end" | "the_end" => "minecraft:the_end".to_owned(),
        other => qualified(other),
    }
}

fn list(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').filter(|part| !part.is_empty())
}

/// Parses `text` (space separated parameters) at `now`.
pub fn parse(text: &str, origin: &Origin, now: i64) -> Result<Params, String> {
    let mut params = Params {
        page: 1,
        ..Params::default()
    };
    for word in text.split_whitespace() {
        let lower = word.to_ascii_lowercase();
        if let Some(flag) = FLAGS.iter().find(|flag| **flag == lower) {
            match *flag {
                "#count" => params.count = true,
                "#verbose" => params.verbose = true,
                "#silent" => params.silent = true,
                "#preview" => params.preview = true,
                _ => params.strict = true,
            }
            continue;
        }
        if let Ok(page) = word.parse::<usize>() {
            params.page = page.max(1);
            continue;
        }
        let Some((key, value)) = word.split_once(':') else {
            return Err(format!("unknown parameter {word:?}"));
        };
        match key.to_ascii_lowercase().as_str() {
            "u" | "user" | "users" => {
                params
                    .filter
                    .users
                    .extend(list(value).map(|user| user.to_owned()));
            }
            "t" | "time" => {
                let (newest, oldest) = match value.split_once('-') {
                    Some((newest, oldest)) => (parse_duration(newest)?, parse_duration(oldest)?),
                    None => (0, parse_duration(value)?),
                };
                let (newest, oldest) = (newest.min(oldest), newest.max(oldest));
                params.filter.since = Some(now - oldest);
                params.filter.until = (newest > 0).then_some(now - newest);
                params.has_time = true;
            }
            "r" | "radius" => {
                params.filter.area = match value.to_ascii_lowercase().as_str() {
                    "#global" | "global" => Area::Global,
                    name if name.starts_with('#') => Area::World(world_key(name, origin)),
                    number => {
                        let radius: i32 = number
                            .parse()
                            .ok()
                            .filter(|radius| *radius >= 0)
                            .ok_or_else(|| format!("invalid radius {value:?}"))?;
                        Area::Radius {
                            world: origin.world.clone(),
                            center: origin.pos,
                            radius,
                        }
                    }
                };
                params.has_area = true;
            }
            "a" | "action" => {
                for action in list(value) {
                    let kinds =
                        action_kinds(action).ok_or_else(|| format!("unknown action {action:?}"))?;
                    for kind in kinds {
                        if !params.filter.kinds.contains(&kind) {
                            params.filter.kinds.push(kind);
                        }
                    }
                }
            }
            "i" | "include" => params.filter.include.extend(list(value).map(qualified)),
            "e" | "exclude" => params
                .filter
                .exclude
                .extend(list(value).map(|e| e.to_owned())),
            "page" => {
                params.page = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid page {value:?}"))?
                    .max(1);
            }
            _ => return Err(format!("unknown parameter {word:?}")),
        }
    }
    Ok(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Origin {
        Origin {
            world: "minecraft:overworld".to_owned(),
            pos: [10, 64, -5],
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn durations_add_their_units() {
        assert_eq!(
            parse_duration("2w5d7h2m10s").unwrap(),
            ((2 * 7 + 5) * 86_400 + 7 * 3600 + 2 * 60 + 10) * 1000
        );
        assert_eq!(parse_duration("1.5h").unwrap(), 5_400_000);
        assert_eq!(parse_duration("30").unwrap(), 30_000);
        for bad in ["", "1x", "h", "-1d", "1..2h"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[tuff_core::watchdog]
    #[test]
    fn every_parameter_parses() {
        let now = 1_000_000_000;
        let params = parse(
            "u:Alex,#tnt t:1h r:5 a:-block,chat i:stone e:dirt,Steve #count #silent 3",
            &origin(),
            now,
        )
        .unwrap();
        let filter = &params.filter;
        assert_eq!(filter.users, ["Alex", "#tnt"]);
        assert_eq!(filter.since, Some(now - 3_600_000));
        assert_eq!(filter.until, None);
        assert_eq!(
            filter.area,
            Area::Radius {
                world: "minecraft:overworld".to_owned(),
                center: [10, 64, -5],
                radius: 5
            }
        );
        assert_eq!(filter.kinds, [Kind::BlockBreak, Kind::Chat]);
        assert_eq!(filter.include, ["minecraft:stone"]);
        assert_eq!(filter.exclude, ["dirt", "Steve"]);
        assert!(params.count && params.silent && !params.verbose && !params.preview);
        assert_eq!(params.page, 3);
        assert!(params.has_time && params.has_area);
    }

    #[tuff_core::watchdog]
    #[test]
    fn time_ranges_and_world_areas() {
        let now = 10_000_000;
        let params = parse("t:2h-1h r:#nether", &origin(), now).unwrap();
        assert_eq!(params.filter.since, Some(now - 7_200_000));
        assert_eq!(params.filter.until, Some(now - 3_600_000));
        assert_eq!(
            params.filter.area,
            Area::World("minecraft:the_nether".to_owned())
        );
        let params = parse("r:#world a:block", &origin(), now).unwrap();
        assert_eq!(
            params.filter.area,
            Area::World("minecraft:overworld".to_owned())
        );
        assert_eq!(params.filter.kinds, [Kind::BlockBreak, Kind::BlockPlace]);
        assert_eq!(
            parse("r:#global", &origin(), now).unwrap().filter.area,
            Area::Global
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn bad_parameters_are_refused() {
        for bad in ["x:1", "r:-3", "a:nothing", "t:soon", "loose", "page:x"] {
            assert!(parse(bad, &origin(), 0).is_err(), "{bad}");
        }
    }
}
