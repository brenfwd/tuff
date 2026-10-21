//! Light engine block properties from `build/LightProperties.java`.
//!
//! Face occlusion shapes are reduced to ids, and the game's own face
//! occlusion test is evaluated for every pair of ids, so the runtime never
//! models block shapes. The output is validated strictly: after a
//! version bump, a changed dump format or meaning fails here instead of
//! producing wrong light.

use std::collections::HashMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail, ensure};

use super::array_body;
use crate::input::Input;

/// The game's direction order, which the runtime `Direction` enum mirrors.
const DIRECTIONS: [&str; 6] = ["down", "up", "north", "south", "west", "east"];
const EMPTY_FACE: usize = 0;
const BLOCK_FACE: usize = 1;

struct State {
    emission: u8,
    dampening: u8,
    empty_shape: bool,
    faces: [usize; 6],
}

pub fn generate(input: &Input) -> Result<String> {
    let state_count: usize = input.blocks.values().map(|b| b.states.len()).sum();
    let mut lines = input.light.lines();
    let mut next = |what: &str| {
        lines
            .next()
            .with_context(|| format!("light properties: missing {what}"))
    };

    let directions: Vec<&str> = field(next("directions")?, "directions")?
        .split(' ')
        .collect();
    ensure!(
        directions == DIRECTIONS,
        "light properties: Direction order is {directions:?}, expected {DIRECTIONS:?}"
    );

    let face_count: usize = field(next("faces")?, "faces")?.parse()?;
    ensure!(face_count > BLOCK_FACE, "light properties: too few faces");
    let mut occludes = vec![0u64; (face_count * face_count).div_ceil(64)];
    for a in 0..face_count {
        let row = field(next("occludes row")?, "occludes")?;
        ensure!(
            row.len() == face_count,
            "light properties: occludes row {a} has {} entries, expected {face_count}",
            row.len()
        );
        for (b, bit) in row.bytes().enumerate() {
            match bit {
                b'1' => {
                    let index = a * face_count + b;
                    occludes[index / 64] |= 1 << (index % 64);
                }
                b'0' => {}
                _ => bail!("light properties: bad occludes entry {:?}", bit as char),
            }
        }
    }
    let occludes_at = |a: usize, b: usize| {
        let index = a * face_count + b;
        occludes[index / 64] >> (index % 64) & 1 == 1
    };
    ensure!(
        !occludes_at(EMPTY_FACE, EMPTY_FACE),
        "light properties: face {EMPTY_FACE} is not the empty shape"
    );
    ensure!(
        (0..face_count).all(|b| occludes_at(BLOCK_FACE, b) && occludes_at(b, BLOCK_FACE)),
        "light properties: face {BLOCK_FACE} is not the full block"
    );

    let mut states = Vec::with_capacity(state_count);
    for line in lines {
        let values = field(line, "state")?
            .split(' ')
            .map(str::parse::<usize>)
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("light properties: bad state line {line:?}"))?;
        let [id, emission, dampening, empty_shape, faces @ ..] = values.as_slice() else {
            bail!("light properties: short state line {line:?}");
        };
        let faces: [usize; 6] = faces
            .try_into()
            .with_context(|| format!("light properties: bad face count in {line:?}"))?;
        ensure!(
            *id == states.len(),
            "light properties: state {id} out of order"
        );
        ensure!(
            *emission <= 15 && *dampening <= 15 && *empty_shape <= 1,
            "light properties: out of range values in {line:?}"
        );
        ensure!(
            faces.iter().all(|f| *f < face_count),
            "light properties: face id out of range in {line:?}"
        );
        ensure!(
            *empty_shape == 0 || faces == [EMPTY_FACE; 6],
            "light properties: empty-shape state {id} has faces"
        );
        states.push(State {
            emission: *emission as u8,
            dampening: *dampening as u8,
            empty_shape: *empty_shape == 1,
            faces,
        });
    }
    ensure!(
        states.len() == state_count,
        "light properties: {} states, but blocks.json has {state_count}",
        states.len()
    );

    let mut face_sets: Vec<[usize; 6]> = Vec::new();
    let mut face_set_ids: HashMap<[usize; 6], usize> = HashMap::new();
    let state_face_set: Vec<usize> = states
        .iter()
        .map(|state| {
            *face_set_ids.entry(state.faces).or_insert_with(|| {
                face_sets.push(state.faces);
                face_sets.len() - 1
            })
        })
        .collect();
    let face_type = if face_count <= 1 << 8 { "u8" } else { "u16" };
    let set_type = if face_sets.len() <= 1 << 16 {
        "u16"
    } else {
        "u32"
    };

    let mut out = String::new();
    writeln!(out, "const FACE_COUNT: usize = {face_count};")?;
    writeln!(out, "type FaceRaw = {face_type};")?;
    write!(
        out,
        "\n/// Emission in the low nibble, dampening in the high one.\n\
         static STATE_LIGHT: [u8; crate::block::BlockState::COUNT] = ["
    )?;
    array_body(
        &mut out,
        states.iter().map(|s| s.emission | s.dampening << 4),
    );
    writeln!(out, "];")?;
    let mut empty = vec![0u64; state_count.div_ceil(64)];
    for (id, state) in states.iter().enumerate() {
        if state.empty_shape {
            empty[id / 64] |= 1 << (id % 64);
        }
    }
    write!(
        out,
        "\nstatic STATE_EMPTY_SHAPE: [u64; {}] = [",
        empty.len()
    )?;
    array_body(&mut out, empty.iter().map(|w| format!("{w:#x}")));
    writeln!(out, "];")?;
    write!(
        out,
        "\nstatic STATE_FACE_SET: [{set_type}; crate::block::BlockState::COUNT] = ["
    )?;
    array_body(&mut out, state_face_set);
    writeln!(out, "];")?;
    write!(
        out,
        "\nstatic FACE_SETS: [[FaceRaw; 6]; {}] = [",
        face_sets.len()
    )?;
    array_body(
        &mut out,
        face_sets.iter().map(|set| {
            let ids: Vec<String> = set.iter().map(usize::to_string).collect();
            format!("[{}]", ids.join(", "))
        }),
    );
    writeln!(out, "];")?;
    write!(out, "\nstatic FACE_OCCLUDES: [u64; {}] = [", occludes.len())?;
    array_body(&mut out, occludes.iter().map(|w| format!("{w:#x}")));
    writeln!(out, "];")?;
    Ok(out)
}

fn field<'a>(line: &'a str, name: &str) -> Result<&'a str> {
    line.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix(' '))
        .with_context(|| format!("light properties: expected {name:?}, got {line:?}"))
}
