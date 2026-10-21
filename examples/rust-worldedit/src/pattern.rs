// SPDX-License-Identifier: MIT
use crate::edit::Clip;

pub const TRAIT_SOLID: i32 = 1;
pub const TRAIT_AIR: i32 = 2;

pub trait Env {
    fn traits(&self, state: &str) -> i32;
    fn properties(&self, state: &str) -> Vec<(String, Vec<String>)>;
    fn normalize(&self, state: &str) -> Option<String>;
    fn tag(&self, tag: &str) -> Option<Vec<String>>;
    fn block_at(&self, pos: [i32; 3]) -> Option<String>;
    fn in_region(&self, pos: [i32; 3]) -> bool;
    fn origin(&self) -> [i32; 3];
    fn seed(&self) -> u64;

    fn biome_at(&self, _pos: [i32; 3]) -> Option<String> {
        None
    }

    fn random(&self, pos: [i32; 3], salt: u64) -> f64 {
        let mut z = self.seed() ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        for c in pos {
            z = z
                .wrapping_add(c as u32 as u64)
                .wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z ^= z >> 31;
        }
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Clone)]
pub enum Pattern {
    State(String),
    Random(Vec<(f64, Pattern)>),
    Tag(Vec<String>),
    Clipboard { clip: Clip, offset: [i32; 3] },
    KeepType(String),
    KeepProps(Vec<(String, String)>),
    AnyState,
}

pub fn split_state(state: &str) -> (&str, Vec<(&str, &str)>) {
    let end = state.find('[').unwrap_or(state.len());
    let props = state[end..]
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter_map(|p| p.split_once('='))
        .collect();
    (&state[..end], props)
}

pub fn join_state(name: &str, props: &[(String, String)]) -> String {
    match props.is_empty() {
        true => name.to_owned(),
        false => {
            let body: Vec<String> = props.iter().map(|(k, v)| format!("{k}={v}")).collect();
            format!("{name}[{}]", body.join(","))
        }
    }
}

pub fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&text[start..]);
    out
}

pub fn tag_members(env: &dyn Env, name: &str) -> Result<Vec<String>, String> {
    let tag = crate::edit::normal(name);
    env.tag(&format!("#{tag}"))
        .ok_or_else(|| format!("Unknown block tag {name}"))
}

pub fn parse_props(text: &str) -> Result<Vec<(String, String)>, String> {
    let body = text
        .strip_prefix('[')
        .and_then(|t| t.strip_suffix(']'))
        .ok_or_else(|| format!("Expected [property=value] but got {text}"))?;
    body.split(',')
        .map(|p| {
            p.split_once('=')
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .ok_or_else(|| format!("Expected property=value but got {p}"))
        })
        .collect()
}

pub fn parse(text: &str, env: &dyn Env, clip: Option<&Clip>) -> Result<Pattern, String> {
    let parts = split_top(text, ',');
    if parts.len() == 1 && !has_weight(parts[0]) {
        return parse_one(parts[0], env, clip);
    }
    let mut choices = Vec::new();
    for part in parts {
        let (weight, rest) = match part.split_once('%') {
            Some((w, rest))
                if !w.is_empty() && w.chars().all(|c| c.is_ascii_digit() || c == '.') =>
            {
                let weight: f64 = w.parse().map_err(|_| format!("Bad weight {w}"))?;
                (weight, rest)
            }
            _ => (1.0, part),
        };
        if weight <= 0.0 {
            return Err(format!("Weight must be positive: {part}"));
        }
        choices.push((weight, parse_one(rest, env, clip)?));
    }
    Ok(Pattern::Random(choices))
}

fn has_weight(part: &str) -> bool {
    part.split_once('%')
        .is_some_and(|(w, _)| !w.is_empty() && w.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

fn parse_one(text: &str, env: &dyn Env, clip: Option<&Clip>) -> Result<Pattern, String> {
    if text.is_empty() {
        return Err("Empty pattern".to_owned());
    }
    if text == "*" {
        return Ok(Pattern::AnyState);
    }
    if let Some(name) = text.strip_prefix("##") {
        return tag_members(env, name).map(Pattern::Tag);
    }
    if let Some(rest) = text.strip_prefix("#clipboard") {
        let clip = clip.ok_or("Your clipboard is empty (//copy)")?.clone();
        let offset = match rest.strip_prefix('@') {
            None if rest.is_empty() => [0; 3],
            Some(o) => parse_offset(o)?,
            None => return Err(format!("Unknown pattern {text}")),
        };
        return Ok(Pattern::Clipboard { clip, offset });
    }
    if let Some(rest) = text.strip_prefix('^') {
        return match rest.starts_with('[') {
            true => parse_props(rest).map(Pattern::KeepProps),
            false => env
                .normalize(&crate::edit::normal(rest))
                .map(|s| Pattern::KeepType(split_state(&s).0.to_owned()))
                .ok_or_else(|| format!("Unknown block {rest}")),
        };
    }
    if text.starts_with(['#', '$', '=']) {
        return Err(format!("Unsupported pattern {text}"));
    }
    env.normalize(&crate::edit::normal(text))
        .map(Pattern::State)
        .ok_or_else(|| format!("Unknown block {text}"))
}

fn parse_offset(text: &str) -> Result<[i32; 3], String> {
    let nums: Vec<i32> = text
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter_map(|n| n.trim().parse().ok())
        .collect();
    <[i32; 3]>::try_from(nums).map_err(|_| format!("Expected @[x,y,z] but got @{text}"))
}

impl Pattern {
    pub fn apply(&self, env: &dyn Env, pos: [i32; 3], old: &str) -> Option<String> {
        match self {
            Pattern::State(s) => Some(s.clone()),
            Pattern::Random(choices) => {
                let total: f64 = choices.iter().map(|(w, _)| w).sum();
                let mut roll = env.random(pos, 0) * total;
                for (weight, pattern) in choices {
                    if roll < *weight {
                        return pattern.apply(env, pos, old);
                    }
                    roll -= weight;
                }
                choices.last()?.1.apply(env, pos, old)
            }
            Pattern::Tag(names) => {
                let at = (env.random(pos, 1) * names.len() as f64) as usize;
                names.get(at.min(names.len().saturating_sub(1))).cloned()
            }
            Pattern::Clipboard { clip, offset } => {
                let o = env.origin();
                clip.tile([
                    pos[0] - o[0] - offset[0],
                    pos[1] - o[1] - offset[1],
                    pos[2] - o[2] - offset[2],
                ])
                .map(str::to_owned)
            }
            Pattern::KeepType(name) => {
                let (_, old_props) = split_state(old);
                let valid = env.properties(name);
                let kept: Vec<(String, String)> = old_props
                    .into_iter()
                    .filter(|(k, v)| {
                        valid
                            .iter()
                            .any(|(n, vals)| n == k && vals.iter().any(|x| x == v))
                    })
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect();
                env.normalize(&join_state(name, &kept))
            }
            Pattern::KeepProps(set) => {
                let (name, old_props) = split_state(old);
                let mut props: Vec<(String, String)> = old_props
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect();
                for (k, v) in set {
                    match props.iter_mut().find(|(pk, _)| pk == k) {
                        Some(slot) => slot.1 = v.clone(),
                        None => props.push((k.clone(), v.clone())),
                    }
                }
                env.normalize(&join_state(name, &props))
            }
            Pattern::AnyState => {
                let (name, _) = split_state(old);
                let props: Vec<(String, String)> = env
                    .properties(name)
                    .into_iter()
                    .enumerate()
                    .filter(|(_, (_, vals))| !vals.is_empty())
                    .map(|(i, (k, vals))| {
                        let at = (env.random(pos, 2 + i as u64) * vals.len() as f64) as usize;
                        let v = vals[at.min(vals.len() - 1)].clone();
                        (k, v)
                    })
                    .collect();
                env.normalize(&join_state(name, &props))
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub struct Mock {
        pub blocks: Vec<([i32; 3], &'static str)>,
        pub region: Option<([i32; 3], [i32; 3])>,
    }

    impl Mock {
        pub fn new() -> Mock {
            Mock {
                blocks: Vec::new(),
                region: None,
            }
        }
    }

    impl Env for Mock {
        fn traits(&self, state: &str) -> i32 {
            match split_state(state).0 {
                "minecraft:air" => TRAIT_AIR,
                "minecraft:water" => 4,
                _ => TRAIT_SOLID,
            }
        }
        fn properties(&self, state: &str) -> Vec<(String, Vec<String>)> {
            let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect();
            match split_state(state).0 {
                "minecraft:oak_stairs" | "minecraft:stone_stairs" => vec![
                    ("facing".to_owned(), v(&["north", "south", "east", "west"])),
                    ("half".to_owned(), v(&["top", "bottom"])),
                ],
                _ => Vec::new(),
            }
        }
        fn normalize(&self, state: &str) -> Option<String> {
            let (name, props) = split_state(state);
            let known = [
                "minecraft:stone",
                "minecraft:dirt",
                "minecraft:gravel",
                "minecraft:air",
                "minecraft:water",
                "minecraft:oak_stairs",
                "minecraft:stone_stairs",
                "minecraft:oak_log",
            ];
            if !known.contains(&name) {
                return None;
            }
            let valid = self.properties(name);
            for (k, v) in &props {
                if !valid
                    .iter()
                    .any(|(n, vals)| n == k && vals.iter().any(|x| x == v))
                {
                    return None;
                }
            }
            Some(state.to_owned())
        }
        fn tag(&self, tag: &str) -> Option<Vec<String>> {
            (tag == "#minecraft:logs").then(|| vec!["minecraft:oak_log".to_owned()])
        }
        fn block_at(&self, pos: [i32; 3]) -> Option<String> {
            Some(
                self.blocks
                    .iter()
                    .find(|(p, _)| *p == pos)
                    .map_or("minecraft:air", |(_, s)| s)
                    .to_owned(),
            )
        }
        fn in_region(&self, pos: [i32; 3]) -> bool {
            self.region
                .is_none_or(|(a, b)| (0..3).all(|i| pos[i] >= a[i] && pos[i] <= b[i]))
        }
        fn origin(&self) -> [i32; 3] {
            [0; 3]
        }
        fn seed(&self) -> u64 {
            42
        }
        fn biome_at(&self, pos: [i32; 3]) -> Option<String> {
            Some(match pos[1] < 0 {
                true => "minecraft:desert".to_owned(),
                false => "minecraft:plains".to_owned(),
            })
        }
    }

    #[test]
    fn single_states_normalize_and_unknown_blocks_fail() {
        let env = Mock::new();
        let p = parse("stone", &env, None).unwrap();
        assert_eq!(
            p.apply(&env, [0; 3], "minecraft:air").unwrap(),
            "minecraft:stone"
        );
        let p = parse("oak_stairs[facing=north]", &env, None).unwrap();
        assert_eq!(
            p.apply(&env, [0; 3], "x").unwrap(),
            "minecraft:oak_stairs[facing=north]"
        );
        assert!(parse("nope", &env, None).is_err());
        assert!(parse("oak_stairs[facing=up]", &env, None).is_err());
    }

    #[test]
    fn weighted_patterns_follow_their_weights_deterministically() {
        let env = Mock::new();
        let p = parse("50%stone,30%dirt,gravel", &env, None).unwrap();
        let mut counts = [0u32; 3];
        for x in 0..100 {
            for z in 0..100 {
                match p.apply(&env, [x, 0, z], "minecraft:air").unwrap().as_str() {
                    "minecraft:stone" => counts[0] += 1,
                    "minecraft:dirt" => counts[1] += 1,
                    _ => counts[2] += 1,
                }
            }
        }
        let total = 10_000.0;
        let near = |n: u32, f: f64| (n as f64 / total - f).abs() < 0.03;
        assert!(near(counts[0], 50.0 / 81.0), "{counts:?}");
        assert!(near(counts[1], 30.0 / 81.0), "{counts:?}");
        assert!(near(counts[2], 1.0 / 81.0), "{counts:?}");
        let again = p.apply(&env, [7, 3, 9], "x");
        assert_eq!(again, p.apply(&env, [7, 3, 9], "x"));
    }

    #[test]
    fn tags_pick_members_and_unknown_tags_fail() {
        let env = Mock::new();
        let p = parse("##logs", &env, None).unwrap();
        assert_eq!(p.apply(&env, [1, 2, 3], "x").unwrap(), "minecraft:oak_log");
        assert!(parse("##nothing", &env, None).is_err());
    }

    #[test]
    fn keep_patterns_preserve_properties_across_types() {
        let env = Mock::new();
        let to_stone = parse("^stone_stairs", &env, None).unwrap();
        assert_eq!(
            to_stone
                .apply(&env, [0; 3], "minecraft:oak_stairs[facing=east,half=top]")
                .unwrap(),
            "minecraft:stone_stairs[facing=east,half=top]"
        );
        let flip = parse("^[half=bottom]", &env, None).unwrap();
        assert_eq!(
            flip.apply(&env, [0; 3], "minecraft:oak_stairs[facing=east,half=top]")
                .unwrap(),
            "minecraft:oak_stairs[facing=east,half=bottom]"
        );
        assert!(flip.apply(&env, [0; 3], "minecraft:stone").is_none());
    }

    #[test]
    fn any_state_picks_valid_states_per_block() {
        let env = Mock::new();
        let p = parse("*", &env, None).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for x in 0..64 {
            let s = p
                .apply(
                    &env,
                    [x, 0, 0],
                    "minecraft:oak_stairs[facing=north,half=top]",
                )
                .unwrap();
            assert!(env.normalize(&s).is_some(), "{s}");
            seen.insert(s);
        }
        assert!(seen.len() > 4);
    }

    #[test]
    fn clipboard_pattern_tiles_with_an_offset() {
        let env = Mock::new();
        let clip = Clip {
            palette: vec!["minecraft:stone".to_owned(), "minecraft:dirt".to_owned()],
            size: [2, 1, 1],
            idx: vec![0, 1],
            offset: [0; 3],
            origin: [0; 3],
            biomes: None,
        };
        let p = parse("#clipboard", &env, Some(&clip)).unwrap();
        let at = |x| p.apply(&env, [x, 0, 0], "x").unwrap();
        assert_eq!(
            [at(0), at(1), at(2), at(-1)],
            [
                "minecraft:stone",
                "minecraft:dirt",
                "minecraft:stone",
                "minecraft:dirt"
            ]
        );
        let p = parse("#clipboard@[1,0,0]", &env, Some(&clip)).unwrap();
        assert_eq!(p.apply(&env, [1, 0, 0], "x").unwrap(), "minecraft:stone");
        assert!(parse("#clipboard", &env, None).is_err());
    }
}
