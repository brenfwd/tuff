// SPDX-License-Identifier: MIT
//! Arena maps: teams with islands, beds and forges, diamond and emerald
//! generators, the waiting platform and the box players may build in.
//! Owners add maps as JSON in the `maps` setting; `classic` and `doubles`
//! are built in.

use std::collections::BTreeMap;

use serde::Deserialize;

pub type Pos = [i32; 3];

/// Dye colors a team may use; the bed, the island and chat follow it.
pub const COLORS: [&str; 16] = [
    "white",
    "orange",
    "magenta",
    "light_blue",
    "yellow",
    "lime",
    "pink",
    "gray",
    "light_gray",
    "cyan",
    "purple",
    "blue",
    "brown",
    "green",
    "red",
    "black",
];

const MAX_TEAMS: usize = 8;
const MAX_TEAM_SIZE: usize = 4;
const MAX_FILL_BLOCKS: i64 = 262_144;
const MAX_SIDE: i32 = 512;
const PLATFORM_RADIUS: i32 = 4;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Facing {
    North,
    South,
    East,
    West,
}

impl Facing {
    pub fn name(self) -> &'static str {
        match self {
            Facing::North => "north",
            Facing::South => "south",
            Facing::East => "east",
            Facing::West => "west",
        }
    }

    pub fn offset(self) -> [i32; 2] {
        match self {
            Facing::North => [0, -1],
            Facing::South => [0, 1],
            Facing::East => [1, 0],
            Facing::West => [-1, 0],
        }
    }

    fn of([dx, dz]: [i32; 2]) -> Facing {
        match (dx.signum(), dz.signum()) {
            (0, -1) => Facing::North,
            (0, 1) => Facing::South,
            (1, _) => Facing::East,
            _ => Facing::West,
        }
    }

    /// The yaw of a body looking this way.
    pub fn yaw(self) -> f32 {
        match self {
            Facing::South => 0.0,
            Facing::West => 90.0,
            Facing::North => 180.0,
            Facing::East => -90.0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Team {
    pub name: String,
    /// A dye color from [`COLORS`].
    pub color: String,
    /// Where members stand when the game starts and when they respawn.
    pub spawn: Pos,
    #[serde(default)]
    pub yaw: f32,
    /// The foot of the bed; its head is one block towards `bed_facing`.
    pub bed: Pos,
    pub bed_facing: Facing,
    /// Where the team's iron and gold appear.
    pub forge: Pos,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Fill {
    pub from: Pos,
    pub to: Pos,
    pub block: String,
}

impl Fill {
    fn volume(&self) -> i64 {
        (0..3)
            .map(|i| i64::from((self.from[i] - self.to[i]).abs() + 1))
            .product()
    }
}

fn one() -> usize {
    1
}

fn two() -> i32 {
    2
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Map {
    pub name: String,
    #[serde(default = "one")]
    pub team_size: usize,
    pub teams: Vec<Team>,
    #[serde(default)]
    pub diamonds: Vec<Pos>,
    #[serde(default)]
    pub emeralds: Vec<Pos>,
    /// Where queued players wait; a glass platform is built under it and
    /// removed when the game starts.
    pub waiting: Pos,
    /// The arena's box, corners inclusive: nothing is placed outside it.
    pub bounds: [Pos; 2],
    /// The highest y players may build at.
    pub build_limit: i32,
    /// Players may not build within this many blocks (sideways) of a
    /// spawn or a generator.
    #[serde(default = "two")]
    pub protect: i32,
    /// The map's blocks, built in order.
    #[serde(default)]
    pub fills: Vec<Fill>,
}

impl Map {
    /// Where shopkeepers stand: one between each team's spawn and bed.
    pub fn shop_spots(&self) -> Vec<[f64; 3]> {
        self.teams
            .iter()
            .map(|t| {
                [
                    f64::from(t.spawn[0] + t.bed[0]) / 2.0 + 0.5,
                    f64::from(t.spawn[1]),
                    f64::from(t.spawn[2] + t.bed[2]) / 2.0 + 0.5,
                ]
            })
            .collect()
    }
}

/// Maps from the setting's JSON: a list of maps, or `{"maps": [...]}`.
pub fn parse(json: &str) -> Result<Vec<Map>, String> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Doc {
        List(Vec<Map>),
        Wrapped { maps: Vec<Map> },
    }
    if json.trim().is_empty() {
        return Ok(Vec::new());
    }
    let maps = match serde_json::from_str::<Doc>(json).map_err(|e| e.to_string())? {
        Doc::List(maps) | Doc::Wrapped { maps } => maps,
    };
    for map in &maps {
        map.validate()
            .map_err(|e| format!("map {}: {e}", map.name))?;
    }
    Ok(maps)
}

/// The built-in maps followed by the configured ones; a configured map
/// replaces a built-in one of the same name.
pub fn all(json: &str) -> Result<Vec<Map>, String> {
    let mut maps: BTreeMap<String, Map> =
        builtin().into_iter().map(|m| (m.name.clone(), m)).collect();
    for map in parse(json)? {
        maps.insert(map.name.clone(), map);
    }
    Ok(maps.into_values().collect())
}

pub fn builtin() -> Vec<Map> {
    vec![classic("classic", 1), classic("doubles", 2)]
}

fn dye_chat(color: &str) -> tuff_plugin_sdk::text::Color {
    use tuff_plugin_sdk::text::Color;
    match color {
        "white" | "light_gray" => Color::White,
        "orange" => Color::Gold,
        "magenta" | "purple" => Color::DarkPurple,
        "pink" => Color::LightPurple,
        "light_blue" => Color::Aqua,
        "cyan" => Color::DarkAqua,
        "yellow" => Color::Yellow,
        "lime" => Color::Green,
        "green" => Color::DarkGreen,
        "gray" | "black" => Color::DarkGray,
        "blue" => Color::Blue,
        "brown" | "red" => Color::Red,
        _ => Color::Gray,
    }
}

impl Team {
    pub fn chat_color(&self) -> tuff_plugin_sdk::text::Color {
        dye_chat(&self.color)
    }

    pub fn bed_head(&self) -> Pos {
        let [dx, dz] = self.bed_facing.offset();
        [self.bed[0] + dx, self.bed[1], self.bed[2] + dz]
    }

    /// Both halves of the bed with their states.
    pub fn bed_blocks(&self) -> [(Pos, String); 2] {
        let state = |part: &str| {
            format!(
                "minecraft:{}_bed[facing={},part={part},occupied=false]",
                self.color,
                self.bed_facing.name()
            )
        };
        [(self.bed, state("foot")), (self.bed_head(), state("head"))]
    }

    /// Where the team chest and the ender chest stand: either side of
    /// the bed.
    pub fn chest_spots(&self) -> [Pos; 2] {
        let [dx, dz] = self.bed_facing.offset();
        let [sx, sz] = [dz.abs() * 2, dx.abs() * 2];
        let [x, y, z] = self.bed;
        [[x + sx, y, z + sz], [x - sx, y, z - sz]]
    }

    /// The chest blocks of both spots.
    pub fn chest_blocks(&self) -> [(Pos, String); 2] {
        let [team, ender] = self.chest_spots();
        let facing = self.bed_facing.name();
        [
            (
                team,
                format!("minecraft:chest[facing={facing},type=single]"),
            ),
            (ender, format!("minecraft:ender_chest[facing={facing}]")),
        ]
    }

    pub fn bed_item(&self) -> String {
        format!("minecraft:{}_bed", self.color)
    }
}

impl Map {
    pub fn capacity(&self) -> usize {
        self.teams.len() * self.team_size
    }

    pub fn inside(&self, [x, y, z]: Pos) -> bool {
        let [lo, hi] = self.bounds;
        (lo[0]..=hi[0]).contains(&x) && (lo[1]..=hi[1]).contains(&y) && (lo[2]..=hi[2]).contains(&z)
    }

    /// Whether players may place a block at `at`.
    pub fn buildable(&self, at: Pos) -> bool {
        self.inside(at) && at[1] <= self.build_limit && !self.protected(at)
    }

    fn protected(&self, at: Pos) -> bool {
        let r = self.protect;
        self.teams
            .iter()
            .flat_map(|t| [t.spawn, t.forge])
            .chain(self.diamonds.iter().copied())
            .chain(self.emeralds.iter().copied())
            .any(|p| {
                (at[0] - p[0]).abs() <= r
                    && (at[2] - p[2]).abs() <= r
                    && (-1..=3).contains(&(at[1] - p[1]))
            })
    }

    /// The team whose bed has a half at `at`.
    pub fn bed_owner(&self, at: Pos) -> Option<usize> {
        self.teams
            .iter()
            .position(|t| t.bed == at || t.bed_head() == at)
    }

    pub fn platform(&self) -> Vec<Pos> {
        let [x, y, z] = self.waiting;
        let r = PLATFORM_RADIUS;
        let mut out = Vec::new();
        for dx in -r..=r {
            for dz in -r..=r {
                out.push([x + dx, y - 1, z + dz]);
            }
        }
        out
    }

    /// Every block the map consists of, in build order: fills, beds and
    /// the waiting platform.
    pub fn blocks(&self) -> Vec<(Pos, String)> {
        let mut out = Vec::new();
        for fill in &self.fills {
            let lo = |i: usize| fill.from[i].min(fill.to[i]);
            let hi = |i: usize| fill.from[i].max(fill.to[i]);
            for y in lo(1)..=hi(1) {
                for z in lo(2)..=hi(2) {
                    for x in lo(0)..=hi(0) {
                        out.push(([x, y, z], fill.block.clone()));
                    }
                }
            }
        }
        for team in &self.teams {
            out.extend(team.bed_blocks());
            out.extend(team.chest_blocks());
        }
        for at in self.platform() {
            out.push((at, "minecraft:glass".to_owned()));
        }
        out
    }

    pub fn validate(&self) -> Result<(), String> {
        let name_ok = !self.name.is_empty()
            && self.name.len() <= 32
            && self
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if !name_ok {
            return Err("names are 1 to 32 of a-z 0-9 _ -".into());
        }
        if !(2..=MAX_TEAMS).contains(&self.teams.len()) {
            return Err(format!("needs 2 to {MAX_TEAMS} teams"));
        }
        if !(1..=MAX_TEAM_SIZE).contains(&self.team_size) {
            return Err(format!("team_size must be 1 to {MAX_TEAM_SIZE}"));
        }
        let [lo, hi] = self.bounds;
        if (0..3).any(|i| lo[i] > hi[i] || hi[i] - lo[i] >= MAX_SIDE) {
            return Err(format!(
                "bounds must be a min and a max corner at most {MAX_SIDE} apart"
            ));
        }
        let mut names = Vec::new();
        for team in &self.teams {
            if names.contains(&team.name.to_lowercase()) {
                return Err(format!("team {} appears twice", team.name));
            }
            names.push(team.name.to_lowercase());
            if !COLORS.contains(&team.color.as_str()) {
                return Err(format!("team {}: unknown color {}", team.name, team.color));
            }
            for (what, at) in [
                ("spawn", team.spawn),
                ("bed", team.bed),
                ("bed head", team.bed_head()),
                ("forge", team.forge),
                ("team chest", team.chest_spots()[0]),
                ("ender chest", team.chest_spots()[1]),
            ] {
                if !self.inside(at) {
                    return Err(format!("team {}: {what} is outside the bounds", team.name));
                }
            }
        }
        let points = self.diamonds.iter().chain(&self.emeralds);
        if points
            .chain(std::iter::once(&self.waiting))
            .any(|&p| !self.inside(p))
        {
            return Err("a generator or the waiting spot is outside the bounds".into());
        }
        if self.platform().iter().any(|&p| !self.inside(p)) {
            return Err("the waiting platform is outside the bounds".into());
        }
        let mut volume = 0;
        for fill in &self.fills {
            if !self.inside(fill.from) || !self.inside(fill.to) {
                return Err(format!("a fill of {} is outside the bounds", fill.block));
            }
            if fill.block.trim().is_empty() {
                return Err("a fill has no block".into());
            }
            volume += fill.volume();
        }
        if volume > MAX_FILL_BLOCKS {
            return Err(format!("fills cover more than {MAX_FILL_BLOCKS} blocks"));
        }
        Ok(())
    }
}

fn add(a: Pos, [dx, dz]: [i32; 2], by: i32) -> Pos {
    [a[0] + dx * by, a[1], a[2] + dz * by]
}

fn square(center: Pos, r: i32, from_y: i32, to_y: i32, block: &str) -> Fill {
    Fill {
        from: [center[0] - r, from_y, center[2] - r],
        to: [center[0] + r, to_y, center[2] + r],
        block: block.to_owned(),
    }
}

/// Four teams on islands 40 blocks out, four diamond islands on the
/// diagonals and two emerald generators in the middle.
pub fn classic(name: &str, team_size: usize) -> Map {
    const TEAMS: [(&str, &str, [i32; 2]); 4] = [
        ("Red", "red", [0, -1]),
        ("Blue", "blue", [1, 0]),
        ("Green", "lime", [0, 1]),
        ("Yellow", "yellow", [-1, 0]),
    ];
    const SURFACE: i32 = 64;
    let mut fills = Vec::new();
    let mut teams = Vec::new();
    for (team, color, out) in TEAMS {
        let center = [out[0] * 40, SURFACE, out[1] * 40];
        let inward = [-out[0], -out[1]];
        fills.push(square(
            center,
            5,
            SURFACE - 3,
            SURFACE - 1,
            "minecraft:stone_bricks",
        ));
        fills.push(square(
            center,
            5,
            SURFACE,
            SURFACE,
            &format!("minecraft:{color}_concrete"),
        ));
        let up = |p: Pos| [p[0], SURFACE + 1, p[2]];
        teams.push(Team {
            name: team.to_owned(),
            color: color.to_owned(),
            spawn: up(add(center, out, 2)),
            yaw: Facing::of(inward).yaw(),
            bed: up(add(center, inward, 3)),
            bed_facing: Facing::of(inward),
            forge: up(add(center, out, 5)),
        });
    }
    let mut diamonds = Vec::new();
    for (dx, dz) in [(1, 1), (1, -1), (-1, 1), (-1, -1)] {
        let center = [dx * 24, SURFACE, dz * 24];
        fills.push(square(
            center,
            2,
            SURFACE - 2,
            SURFACE,
            "minecraft:end_stone",
        ));
        fills.push(square(
            center,
            0,
            SURFACE,
            SURFACE,
            "minecraft:diamond_block",
        ));
        diamonds.push([center[0], SURFACE + 1, center[2]]);
    }
    fills.push(square(
        [0, 0, 0],
        6,
        SURFACE - 3,
        SURFACE,
        "minecraft:stone",
    ));
    let mut emeralds = Vec::new();
    for dx in [-2, 2] {
        fills.push(square(
            [dx, 0, 0],
            0,
            SURFACE,
            SURFACE,
            "minecraft:emerald_block",
        ));
        emeralds.push([dx, SURFACE + 1, 0]);
    }
    Map {
        name: name.to_owned(),
        team_size,
        teams,
        diamonds,
        emeralds,
        waiting: [0, 110, 0],
        bounds: [[-64, 0, -64], [64, 127, 64]],
        build_limit: 100,
        protect: 2,
        fills,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_maps_are_valid_and_beds_face_the_middle() {
        for map in builtin() {
            map.validate().unwrap();
        }
        let map = classic("classic", 1);
        assert_eq!(map.capacity(), 4);
        let red = &map.teams[0];
        assert_eq!(red.bed, [0, 65, -37]);
        assert_eq!(red.bed_head(), [0, 65, -36]);
        assert_eq!(red.spawn, [0, 65, -42]);
        assert_eq!(red.forge, [0, 65, -45]);
        assert_eq!(
            red.bed_blocks()[1].1,
            "minecraft:red_bed[facing=south,part=head,occupied=false]"
        );
        assert_eq!(map.bed_owner([0, 65, -36]), Some(0));
        assert_eq!(map.bed_owner([40, 65, 0]), None);
        let blue = &map.teams[1];
        assert_eq!(blue.bed_facing, Facing::West);
        assert_eq!(blue.bed, [37, 65, 0]);
    }

    #[test]
    fn building_is_limited_to_the_box_away_from_spawns_and_generators() {
        let map = classic("classic", 1);
        assert!(map.buildable([0, 65, -30]));
        assert!(!map.buildable([0, 66, -42]), "at the red spawn");
        assert!(!map.buildable([1, 65, -44]), "next to the red forge");
        assert!(!map.buildable([24, 66, 24]), "on a diamond generator");
        assert!(map.buildable([24, 70, 24]), "high above it");
        assert!(!map.buildable([0, 101, 0]), "over the build limit");
        assert!(!map.buildable([65, 65, 0]), "outside the box");
    }

    #[test]
    fn blocks_list_fills_then_beds_then_the_platform() {
        let map = classic("classic", 1);
        let blocks = map.blocks();
        let fills: i64 = map.fills.iter().map(Fill::volume).sum();
        assert_eq!(blocks.len() as i64, fills + 16 + 81);
        assert_eq!(
            blocks.last().unwrap(),
            &([4, 109, 4], "minecraft:glass".to_owned())
        );
        assert!(blocks.contains(&([0, 64, -40], "minecraft:red_concrete".to_owned())));
    }

    #[test]
    fn configured_maps_parse_validate_and_replace_builtins() {
        let json = r#"[{
            "name": "classic",
            "teams": [
                {"name": "Red", "color": "red", "spawn": [0, 65, -10], "bed": [0, 65, -8],
                 "bed_facing": "south", "forge": [0, 65, -12]},
                {"name": "Blue", "color": "blue", "spawn": [0, 65, 10], "bed": [0, 65, 8],
                 "bed_facing": "north", "forge": [0, 65, 12]}
            ],
            "waiting": [0, 90, 0],
            "bounds": [[-20, 0, -20], [20, 100, 20]],
            "build_limit": 80,
            "fills": [{"from": [-3, 64, -14], "to": [3, 64, 14], "block": "minecraft:oak_planks"}]
        }]"#;
        let maps = all(json).unwrap();
        assert_eq!(maps.len(), 2);
        let mine = maps.iter().find(|m| m.name == "classic").unwrap();
        assert_eq!(mine.teams.len(), 2);
        assert_eq!(mine.protect, 2);
        assert_eq!(parse(&format!("{{\"maps\": {json}}}")).unwrap().len(), 1);
        assert!(all("").unwrap().len() == 2);

        let bad_color = json.replace("\"blue\"", "\"teal\"");
        assert!(
            parse(&bad_color)
                .unwrap_err()
                .contains("unknown color teal")
        );
        let outside = json.replace("[0, 65, 12]", "[0, 65, 40]");
        assert!(parse(&outside).unwrap_err().contains("forge is outside"));
        let one_team = r#"[{"name": "x", "teams": [], "waiting": [0,0,0],
            "bounds": [[0,0,0],[1,1,1]], "build_limit": 1}]"#;
        assert!(parse(one_team).unwrap_err().contains("2 to 8 teams"));
        assert!(parse("{").is_err());
        assert!(parse(r#"[{"name": "x", "bogus": 1}]"#).is_err());
    }
}
