// SPDX-License-Identifier: MIT
use crate::edit::matches;
use crate::pattern::{
    Env, TRAIT_AIR, TRAIT_SOLID, parse_props, split_state, split_top, tag_members,
};

#[derive(Clone)]
pub enum Mask {
    State(String),
    Props(Vec<(String, String)>),
    Tag(Vec<String>),
    Not(Box<Mask>),
    Existing,
    Solid,
    Air,
    Region,
    Random(f64),
    Biome(String),
    Below(Box<Mask>),
    Above(Box<Mask>),
    Or(Vec<Mask>),
    And(Vec<Mask>),
    Expr(std::rc::Rc<crate::expr::Expr>),
}

pub fn parse(terms: &[&str], env: &dyn Env) -> Result<Mask, String> {
    let mut all = Vec::new();
    for (i, term) in terms.iter().enumerate() {
        if let Some(text) = term.strip_prefix('=') {
            let text = [text].into_iter().chain(terms[i + 1..].iter().copied());
            let text: Vec<&str> = text.collect();
            all.push(parse_expr(&text.join(" "))?);
            break;
        }
        all.push(parse_term(term, env)?);
    }
    match all.len() {
        0 => Err("Empty mask".to_owned()),
        1 => Ok(all.remove(0)),
        _ => Ok(Mask::And(all)),
    }
}

fn parse_term(text: &str, env: &dyn Env) -> Result<Mask, String> {
    let parts = split_top(text, ',');
    let mut any = Vec::new();
    for part in parts {
        any.push(parse_atom(part, env)?);
    }
    match any.len() {
        1 => Ok(any.remove(0)),
        _ => Ok(Mask::Or(any)),
    }
}

fn parse_expr(text: &str) -> Result<Mask, String> {
    crate::expr::Expr::parse(text).map(|e| Mask::Expr(std::rc::Rc::new(e)))
}

fn parse_atom(text: &str, env: &dyn Env) -> Result<Mask, String> {
    if let Some(rest) = text.strip_prefix('!') {
        return Ok(Mask::Not(Box::new(parse_atom(rest, env)?)));
    }
    if let Some(rest) = text.strip_prefix('>') {
        return Ok(Mask::Below(Box::new(parse_term(rest, env)?)));
    }
    if let Some(rest) = text.strip_prefix('<') {
        return Ok(Mask::Above(Box::new(parse_term(rest, env)?)));
    }
    if let Some(rest) = text.strip_prefix('%') {
        let pct: f64 = rest.parse().map_err(|_| format!("Bad percentage {rest}"))?;
        return Ok(Mask::Random((pct / 100.0).clamp(0.0, 1.0)));
    }
    if let Some(name) = text.strip_prefix("##") {
        return tag_members(env, name).map(Mask::Tag);
    }
    if let Some(rest) = text.strip_prefix('^') {
        return parse_props(rest).map(Mask::Props);
    }
    match text {
        "#existing" => return Ok(Mask::Existing),
        "#solid" => return Ok(Mask::Solid),
        "#air" => return Ok(Mask::Air),
        "#region" | "#sel" => return Ok(Mask::Region),
        "" => return Err("Empty mask".to_owned()),
        _ => {}
    }
    if let Some(text) = text.strip_prefix('=') {
        return parse_expr(text);
    }
    if let Some(name) = text.strip_prefix('$') {
        return match name {
            "" => Err("Empty biome mask".to_owned()),
            _ if name.contains(':') => Ok(Mask::Biome(name.to_owned())),
            _ => Ok(Mask::Biome(format!("minecraft:{name}"))),
        };
    }
    if text.starts_with('#') {
        return Err(format!("Unknown mask {text}"));
    }
    let state = crate::edit::normal(text);
    let (name, props) = split_state(&state);
    let known = env.normalize(name).is_some();
    let valid = env.properties(name);
    let props_ok = props.iter().all(|(k, v)| {
        valid
            .iter()
            .any(|(n, vals)| n == k && vals.iter().any(|x| x == v))
    });
    match known && props_ok {
        true => Ok(Mask::State(state)),
        false => Err(format!("Unknown block {text}")),
    }
}

impl Mask {
    pub fn has_expr(&self) -> bool {
        match self {
            Mask::Expr(_) => true,
            Mask::Not(m) | Mask::Below(m) | Mask::Above(m) => m.has_expr(),
            Mask::Or(all) | Mask::And(all) => all.iter().any(Mask::has_expr),
            _ => false,
        }
    }

    pub fn test(&self, env: &dyn Env, pos: [i32; 3], old: &str) -> bool {
        match self {
            Mask::State(s) => matches(old, s),
            Mask::Props(want) => {
                let (_, have) = split_state(old);
                want.iter()
                    .all(|(k, v)| have.iter().any(|(hk, hv)| hk == k && hv == v))
            }
            Mask::Tag(names) => names.iter().any(|n| n == split_state(old).0),
            Mask::Not(inner) => !inner.test(env, pos, old),
            Mask::Existing => env.traits(old) & TRAIT_AIR == 0,
            Mask::Solid => env.traits(old) & TRAIT_SOLID != 0,
            Mask::Air => env.traits(old) & TRAIT_AIR != 0,
            Mask::Region => env.in_region(pos),
            Mask::Random(p) => env.random(pos, 100) < *p,
            Mask::Biome(want) => env.biome_at(pos).is_some_and(|b| &b == want),
            Mask::Below(inner) => neighbour(inner, env, [pos[0], pos[1] - 1, pos[2]]),
            Mask::Above(inner) => neighbour(inner, env, [pos[0], pos[1] + 1, pos[2]]),
            Mask::Or(all) => all.iter().any(|m| m.test(env, pos, old)),
            Mask::And(all) => all.iter().all(|m| m.test(env, pos, old)),
            Mask::Expr(e) => e
                .value(
                    [pos[0] as f64, pos[1] as f64, pos[2] as f64],
                    crate::expr::site_seed(env.seed(), pos),
                )
                .is_ok_and(|v| v > 0.0),
        }
    }
}

fn neighbour(inner: &Mask, env: &dyn Env, pos: [i32; 3]) -> bool {
    env.block_at(pos)
        .is_some_and(|state| inner.test(env, pos, &state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::tests::Mock;

    fn mask(text: &str, env: &Mock) -> Mask {
        let terms: Vec<&str> = text.split(' ').collect();
        parse(&terms, env).unwrap()
    }

    #[test]
    fn biome_masks_match_the_cell_biome() {
        let env = Mock::new();
        let m = mask("$desert", &env);
        assert!(m.test(&env, [0, -4, 0], "minecraft:stone"));
        assert!(!m.test(&env, [0, 4, 0], "minecraft:stone"));
        let m = mask("!$minecraft:desert", &env);
        assert!(m.test(&env, [0, 4, 0], "minecraft:stone"));
        assert!(parse(&["$"], &env).is_err());
    }

    #[test]
    fn block_lists_negation_and_property_masks() {
        let env = Mock::new();
        let m = mask("stone,dirt", &env);
        assert!(m.test(&env, [0; 3], "minecraft:stone"));
        assert!(m.test(&env, [0; 3], "minecraft:dirt"));
        assert!(!m.test(&env, [0; 3], "minecraft:gravel"));
        let m = mask("!stone", &env);
        assert!(m.test(&env, [0; 3], "minecraft:dirt"));
        assert!(!m.test(&env, [0; 3], "minecraft:stone"));
        let m = mask("oak_stairs[half=top]", &env);
        assert!(m.test(&env, [0; 3], "minecraft:oak_stairs[facing=east,half=top]"));
        assert!(!m.test(
            &env,
            [0; 3],
            "minecraft:oak_stairs[facing=east,half=bottom]"
        ));
        let m = mask("^[half=top]", &env);
        assert!(m.test(&env, [0; 3], "minecraft:oak_stairs[facing=east,half=top]"));
        assert!(!m.test(&env, [0; 3], "minecraft:stone"));
    }

    #[test]
    fn trait_and_tag_masks() {
        let env = Mock::new();
        assert!(mask("#existing", &env).test(&env, [0; 3], "minecraft:stone"));
        assert!(!mask("#existing", &env).test(&env, [0; 3], "minecraft:air"));
        assert!(mask("#air", &env).test(&env, [0; 3], "minecraft:air"));
        assert!(mask("#solid", &env).test(&env, [0; 3], "minecraft:stone"));
        assert!(!mask("#solid", &env).test(&env, [0; 3], "minecraft:water"));
        assert!(mask("##logs", &env).test(&env, [0; 3], "minecraft:oak_log"));
        assert!(!mask("##logs", &env).test(&env, [0; 3], "minecraft:stone"));
    }

    #[test]
    fn overlay_masks_look_at_the_neighbour_and_intersect_by_space() {
        let mut env = Mock::new();
        env.blocks.push(([0, 0, 0], "minecraft:stone"));
        let top = mask("#air >stone", &env);
        assert!(top.test(&env, [0, 1, 0], "minecraft:air"));
        assert!(!top.test(&env, [1, 1, 0], "minecraft:air"));
        let under = mask("<#air", &env);
        assert!(under.test(&env, [0, 0, 0], "minecraft:stone"));
        assert!(!mask("<stone", &env).test(&env, [0, 0, 0], "minecraft:stone"));
    }

    #[test]
    fn random_and_region_masks() {
        let mut env = Mock::new();
        env.region = Some(([0; 3], [3, 3, 3]));
        let m = mask("%25", &env);
        let hits = (0..4000)
            .filter(|i| m.test(&env, [*i, 0, i / 7], "minecraft:stone"))
            .count();
        assert!((800..1200).contains(&hits), "{hits}");
        let r = mask("#sel", &env);
        assert!(r.test(&env, [1, 1, 1], "x") && !r.test(&env, [4, 1, 1], "x"));
    }

    #[test]
    fn expression_masks_test_world_coordinates() {
        let env = Mock::new();
        let m = mask("=y>3&&x<2", &env);
        assert!(m.test(&env, [1, 4, 0], "x") && !m.test(&env, [1, 3, 0], "x"));
        assert!(!m.test(&env, [2, 9, 0], "x"));
        let m = mask("#air =y > 3", &env);
        assert!(m.test(&env, [0, 4, 0], "minecraft:air"));
        assert!(!m.test(&env, [0, 4, 0], "minecraft:stone"));
        let m = mask("=while(1)1", &env);
        assert!(!m.test(&env, [0; 3], "x"));
        let m = mask("=rand()<0.5", &env);
        let hits = (0..2000).filter(|i| m.test(&env, [*i, 0, 0], "x")).count();
        assert!((800..1200).contains(&hits), "{hits}");
    }

    #[test]
    fn unsupported_and_unknown_masks_report_errors() {
        let env = Mock::new();
        for bad in ["=x>", "=nope(1)", "$", "nope", "#bogus", ""] {
            assert!(parse(&[bad], &env).is_err(), "{bad}");
        }
    }
}
