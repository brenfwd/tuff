use std::fmt::Write;

use anyhow::Result;

use super::enums::{IdEnum, by_id};
use crate::input::Input;
use crate::names::{pascal, snake};

pub fn generate(input: &Input) -> Result<String> {
    let mut out = String::new();
    for (state, directions) in &input.packets {
        writeln!(out, "pub mod {} {{", snake(state))?;
        for (direction, packets) in directions {
            let entries = by_id(
                packets
                    .iter()
                    .map(|(name, id)| (name.as_str(), id.protocol_id)),
            )?;
            IdEnum {
                doc: format!("{direction} packets in the {state} state."),
                name: &pascal(direction),
                id_type: "i32",
                entries,
                default: None,
            }
            .emit(&mut out)?;
        }
        writeln!(out, "}}")?;
    }
    Ok(out)
}
